// SPDX-License-Identifier: AGPL-3.0-or-later
//! `voice-server`: the community's voice service for the game's voice SDK (Vivox 5.23.1).
//!
//! After the prelogin (`nw_login::voice`, served by `edge-http`), the SDK opens the XMPP server
//! the prelogin names (`XMPPServer`: host, port, TLS from the start) to sign the player in with
//! the social service's login token and to join its channels. Retail's XMPP traffic is not in
//! the captures (only the HTTPS prelogin is): [`xmpp`] checks the sign-in against the social
//! service's tokens, follows RFC 6120 for the steps after it, and every byte the SDK sends is
//! logged, so the next stages can be built from what it does.

pub mod media;
pub mod rooms;
pub mod vani;
pub mod xmpp;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nw_common::config::{ConfigError, SECRET_ENV_PREFIX, load_config};
use nw_login::certificate::{CertificateError, read_chain, read_private_key};
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_rustls::TlsAcceptor;

use crate::media::{MediaConfig, Relay, answer_sdp};
use crate::rooms::{Occupant, Rooms};
use crate::xmpp::{
    CHAN_ADD_DONE, CHAN_ADD_NO_CALL, CallRequest, NO_CALL_MESSAGE, RoomChange, Session, SignIn,
    chan_add_result, invite_result, not_in_room,
};

/// Environment overrides of the configuration start with this.
pub const ENV_PREFIX: &str = "NW_VOICE_";
/// Largest read from one connection at a time.
const READ_BUFFER_BYTES: usize = 16 * 1024;
/// Longest text of one read that goes in the log.
const MAX_LOGGED_CHARS: usize = 4_096;

/// `config/voice-server.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceServerConfig {
    /// Log filter (`tracing` syntax).
    pub log: String,
    /// The XMPP listener: the `xmpp_host` and `xmpp_port` of `edge-http`'s `[voice]`.
    pub xmpp_listen: SocketAddr,
    /// The SIP realm the streams come from: `edge-http`'s `[voice] realm`.
    pub realm: String,
    /// The token issuer: the social service's `[voice] issuer`.
    pub issuer: String,
    /// The variable that holds the token key: the social service's `[voice] key_env`.
    pub key_env: String,
    /// The certificate of the XMPP host (PEM chain; the `edge-http` certificate fits).
    pub certificate_pem_path: PathBuf,
    /// Its private key (PEM).
    pub private_key_pem_path: PathBuf,
    /// The audio relay.
    pub media: MediaConfig,
}

/// Why the voice server stopped.
#[derive(Debug, Error)]
pub enum VoiceServerError {
    /// The configuration could not be read.
    #[error("configuration")]
    Config(#[from] ConfigError),
    /// The log filter is not valid.
    #[error("logging")]
    Logging(#[from] nw_common::logging::LoggingError),
    /// The certificate or key could not be read.
    #[error("certificate")]
    Certificate(#[from] CertificateError),
    /// The TLS settings are not valid.
    #[error("TLS settings")]
    Tls(#[from] rustls::Error),
    /// A socket failed.
    #[error("socket")]
    Io(#[from] std::io::Error),
    /// The key variable is not a secret's name, or it is empty.
    #[error("the token key variable {0} must start with NW_SECRET_ and be set")]
    Key(String),
}

/// The TLS acceptor of `config`'s certificate.
///
/// # Errors
///
/// Returns [`VoiceServerError`] when the certificate or key cannot be read or used.
pub fn acceptor(config: &VoiceServerConfig) -> Result<TlsAcceptor, VoiceServerError> {
    let chain = read_chain(&config.certificate_pem_path)?;
    let key = read_private_key(&config.private_key_pem_path)?;
    let server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    Ok(TlsAcceptor::from(Arc::new(server)))
}

/// Serves the voice service of the configuration file `path` until the process stops.
///
/// # Errors
///
/// Returns [`VoiceServerError`] when the configuration, the certificate or the listener fails.
pub fn serve(path: &Path) -> Result<(), VoiceServerError> {
    let config: VoiceServerConfig = load_config(path, ENV_PREFIX)?;
    nw_common::logging::init_logging(&config.log)?;
    nw_login::install_crypto_provider();
    let acceptor = acceptor(&config)?;
    let key = std::env::var(&config.key_env)
        .ok()
        .filter(|key| !key.is_empty() && config.key_env.starts_with(SECRET_ENV_PREFIX))
        .ok_or_else(|| VoiceServerError::Key(config.key_env.clone()))?;
    let sign_in = SignIn {
        realm: config.realm.clone(),
        issuer: config.issuer.clone(),
        key: key.into_bytes(),
    };
    let media = config.media.clone();
    tokio::runtime::Runtime::new()?.block_on(async move {
        let relay = Relay::new(media, &sign_in.realm);
        tokio::spawn(relay.clone().run_rosters());
        listen(config.xmpp_listen, sign_in, acceptor, relay).await
    })
}

async fn listen(
    listen: SocketAddr,
    sign_in: SignIn,
    acceptor: TlsAcceptor,
    relay: Relay,
) -> Result<(), VoiceServerError> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(%listen, "voice XMPP listener up");
    let rooms = Rooms::default();
    loop {
        let (socket, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let session = Session::new(sign_in.clone());
        let rooms = rooms.clone();
        let relay = relay.clone();
        tokio::spawn(async move {
            let (outbox, inbox) = unbounded_channel();
            let mut joined = Joined {
                rooms,
                relay,
                jid: None,
                in_rooms: HashSet::new(),
            };
            let ended = connection(
                socket,
                peer,
                acceptor,
                session,
                &mut joined,
                (outbox, inbox),
            )
            .await;
            if let Err(error) = ended {
                tracing::info!(%peer, %error, "voice connection ended");
            }
        });
    }
}

/// The rooms a connection may be in: it leaves them all when the connection ends.
/// The rooms and the call of a connection: it leaves them all when the connection ends.
struct Joined {
    rooms: Rooms,
    relay: Relay,
    jid: Option<String>,
    /// The rooms this connection joined: a call may only be in these.
    in_rooms: HashSet<String>,
}

impl Joined {
    /// The answer to a call setup of this connection, bound as `jid`.
    async fn call(&self, jid: &str, request: CallRequest) -> Option<String> {
        match request {
            CallRequest::Invite { id, room, offer } => {
                if !self.in_rooms.contains(&room) {
                    return Some(not_in_room(&id, jid, &room));
                }
                match self.relay.open_call(jid, &room).await {
                    Ok(port) => {
                        tracing::info!(%jid, %room, port, "voice call opened");
                        let sdp = answer_sdp(self.relay.address(), port, &offer, &room);
                        Some(invite_result(&id, jid, &room, &sdp))
                    }
                    Err(error) => {
                        tracing::warn!(%jid, %room, %error, "voice call refused: no port");
                        None
                    }
                }
            }
            CallRequest::ChanAdd { id, room } => {
                let added = self.in_rooms.contains(&room) && self.relay.add_room(jid, &room).await;
                let (code, msg) = if added {
                    (CHAN_ADD_DONE, "OK")
                } else {
                    (CHAN_ADD_NO_CALL, NO_CALL_MESSAGE)
                };
                tracing::info!(%jid, %room, code, "voice room added to the call");
                Some(chan_add_result(&id, jid, &room, code, msg))
            }
        }
    }
}

impl Drop for Joined {
    fn drop(&mut self) {
        if let Some(jid) = &self.jid {
            self.rooms.leave_everywhere(jid);
            self.relay.close_call(jid);
        }
    }
}

/// One SDK connection: the TLS handshake, then every read logged and answered by its
/// [`Session`], room changes applied to the shared rooms, and what other connections send it
/// (the presences of its rooms) written as it comes.
async fn connection(
    socket: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    mut session: Session,
    joined: &mut Joined,
    (outbox, mut inbox): (
        tokio::sync::mpsc::UnboundedSender<String>,
        UnboundedReceiver<String>,
    ),
) -> Result<(), std::io::Error> {
    tracing::info!(%peer, "voice connection");
    let mut stream = acceptor.accept(socket).await?;
    {
        let (_, session) = stream.get_ref();
        tracing::info!(
            %peer,
            server_name = ?session.server_name(),
            alpn = ?session.alpn_protocol().map(String::from_utf8_lossy),
            "voice TLS up"
        );
    }
    let mut buffer = vec![0; READ_BUFFER_BYTES];
    loop {
        let read = tokio::select! {
            read = stream.read(&mut buffer) => read?,
            Some(pushed) = inbox.recv() => {
                tracing::info!(%peer, text = %pushed, "voice server sent");
                stream.write_all(pushed.as_bytes()).await?;
                continue;
            }
        };
        if read == 0 {
            tracing::info!(%peer, "voice connection closed by the client");
            return Ok(());
        }
        let text = String::from_utf8_lossy(&buffer[..read]);
        let shown: String = text.chars().take(MAX_LOGGED_CHARS).collect();
        tracing::info!(%peer, bytes = read, text = %shown, "voice client sent");
        let answer = session.on_text(&text, unix_seconds());
        if let Some(user) = &answer.signed_in {
            tracing::info!(%peer, %user, "voice sign-in accepted");
        }
        let mut replies = answer.replies;
        match answer.room {
            Some(RoomChange::Join {
                room,
                nick,
                jid,
                status,
                id,
            }) => {
                tracing::info!(%peer, %room, %nick, "voice room joined");
                joined.jid = Some(jid.clone());
                joined.in_rooms.insert(room.clone());
                let occupant = Occupant {
                    jid,
                    outbox: outbox.clone(),
                    status,
                };
                replies.extend(joined.rooms.join(&room, &nick, occupant, id.as_deref()));
            }
            Some(RoomChange::Update {
                room,
                nick,
                status,
                id,
            }) => {
                tracing::info!(%peer, %room, %nick, %status, "voice room status");
                replies.extend(joined.rooms.update(&room, &nick, &status, id.as_deref()));
            }
            Some(RoomChange::Leave { room, nick, id }) => {
                tracing::info!(%peer, %room, %nick, "voice room left");
                replies.extend(joined.rooms.leave(&room, &nick, id.as_deref()));
                if let Some(jid) = &joined.jid {
                    joined.relay.leave_room(jid, &room).await;
                }
                joined.in_rooms.remove(&room);
            }
            None => {}
        }
        if let Some(request) = answer.call
            && let Some(jid) = session.jid().map(str::to_owned)
        {
            replies.extend(joined.call(&jid, request).await);
        }
        for reply in &replies {
            tracing::info!(%peer, text = %reply, "voice server sent");
            stream.write_all(reply.as_bytes()).await?;
        }
        if answer.close {
            stream.shutdown().await?;
            tracing::info!(%peer, "voice connection closed by the server");
            return Ok(());
        }
    }
}

/// Seconds since the Unix epoch now.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_configuration_reads() {
        let config: VoiceServerConfig = load_config(
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../config/voice-server.toml"
            )),
            ENV_PREFIX,
        )
        .expect("config");
        assert_eq!(config.xmpp_listen.port(), 5223);
        assert_eq!(config.realm, "voice.example.org");
        assert!(config.key_env.starts_with(SECRET_ENV_PREFIX));
    }
}
