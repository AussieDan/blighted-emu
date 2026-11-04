// SPDX-License-Identifier: AGPL-3.0-or-later
//! The relay side (admin relay spec section 5.1): `blighted-admin relay` takes the agent
//! connection on `agent_listen` and the browser requests on `listen`, and sends each browser
//! request to the agent over HTTP/2.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::uri::{PathAndQuery, Scheme};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, Version, header};
use axum::response::{IntoResponse, Response};
use hyper::client::conn::http2::{self, SendRequest};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use nw_login::certificate::{CertificateError, read_chain, read_private_key};
use nw_login::install_crypto_provider;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::relay::config::AgentTlsConfig;
use crate::relay::{H2_ALPN, KEEP_ALIVE_INTERVAL, KEEP_ALIVE_TIMEOUT, RelayError};

/// The answer while no agent is connected.
pub const NOT_CONNECTED: &str = "the game server is not connected";
/// Headers of one HTTP/1 connection, which HTTP/2 does not allow; `host` becomes `:authority`.
const CONNECTION_HEADERS: [HeaderName; 6] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    header::TE,
    HeaderName::from_static("proxy-connection"),
];

/// The id and HTTP/2 client of the current agent connection.
type CurrentAgent = Option<(u64, SendRequest<Body>)>;

/// The agent connection of the moment, if any. A new connection replaces the old one.
#[derive(Debug, Clone, Default)]
pub struct AgentLink {
    current: Arc<Mutex<CurrentAgent>>,
    next_id: Arc<AtomicU64>,
}

impl AgentLink {
    fn replace(&self, sender: SendRequest<Body>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some((id, sender));
        id
    }

    /// Forgets connection `id` unless a newer one replaced it.
    fn clear(&self, id: u64) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current
            .as_ref()
            .is_some_and(|(current_id, _)| *current_id == id)
        {
            *current = None;
        }
    }

    fn sender(&self) -> Option<SendRequest<Body>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, sender)| sender.clone())
    }
}

/// The TLS server settings of the agent port: the relay certificate, and a client certificate
/// that must be exactly the agent certificate.
///
/// # Errors
///
/// Returns [`CertificateError`] when a file cannot be read or used.
pub fn relay_agent_tls(tls: &AgentTlsConfig) -> Result<ServerConfig, CertificateError> {
    install_crypto_provider();
    let mut roots = RootCertStore::empty();
    for certificate in read_chain(&tls.agent_certificate_pem_path)? {
        roots.add(certificate)?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|error| rustls::Error::General(error.to_string()))?;
    let mut config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            read_chain(&tls.certificate_pem_path)?,
            read_private_key(&tls.private_key_pem_path)?,
        )?;
    config.alpn_protocols = vec![H2_ALPN.to_vec()];
    Ok(config)
}

/// Accepts agent connections on `listener` until the task ends; each one replaces the last.
pub async fn accept_agents(listener: TcpListener, acceptor: TlsAcceptor, link: AgentLink) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "agent accept failed");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let link = link.clone();
        tokio::spawn(async move {
            let result = match acceptor.accept(stream).await {
                Ok(stream) => attach(stream, peer, &link).await,
                Err(error) => Err(RelayError::Io(error)),
            };
            if let Err(error) = result {
                tracing::warn!(%peer, %error, "agent connection failed");
            }
        });
    }
}

/// Makes the HTTP/2 client of one agent connection the current one until the connection ends.
async fn attach(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    peer: SocketAddr,
    link: &AgentLink,
) -> Result<(), RelayError> {
    let (sender, connection) = http2::Builder::new(TokioExecutor::new())
        .timer(TokioTimer::new())
        .keep_alive_interval(KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
        .handshake(TokioIo::new(stream))
        .await?;
    let id = link.replace(sender);
    tracing::info!(%peer, "agent connected");
    let result = connection.await;
    link.clear(id);
    tracing::info!(%peer, "agent disconnected");
    Ok(result?)
}

/// Every browser request goes to the agent of `link`.
pub fn relay_router(link: AgentLink) -> Router {
    Router::new().fallback(forward).with_state(link)
}

async fn forward(State(link): State<AgentLink>, request: Request) -> Response {
    let Some(mut sender) = link.sender() else {
        return (StatusCode::SERVICE_UNAVAILABLE, NOT_CONNECTED).into_response();
    };
    let Some(request) = agent_request(request) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if sender.ready().await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, NOT_CONNECTED).into_response();
    }
    match sender.send_request(request).await {
        Ok(response) => {
            let (mut parts, body) = response.into_parts();
            remove_connection_headers(&mut parts.headers);
            Response::from_parts(parts, Body::new(body))
        }
        Err(error) => {
            tracing::warn!(%error, "agent request failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// `request` as an HTTP/2 request to the agent: `https://<browser Host><path and query>`, without
/// the connection headers. `None` without a host.
fn agent_request(request: Request) -> Option<Request> {
    let (mut parts, body) = request.into_parts();
    let host = match parts.headers.get(header::HOST) {
        Some(host) => host.to_str().ok()?.to_owned(),
        None => parts.uri.authority()?.as_str().to_owned(),
    };
    let path = parts
        .uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    parts.uri = Uri::builder()
        .scheme(Scheme::HTTPS)
        .authority(host)
        .path_and_query(path)
        .build()
        .ok()?;
    parts.version = Version::HTTP_2;
    remove_connection_headers(&mut parts.headers);
    parts.headers.remove(header::HOST);
    Some(Request::from_parts(parts, body))
}

/// Removes the HTTP/1 connection headers and the headers that `Connection` names.
fn remove_connection_headers(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::try_from(name.trim()).ok())
        .collect();
    for name in CONNECTION_HEADERS.iter().chain(&named) {
        headers.remove(name);
    }
}
