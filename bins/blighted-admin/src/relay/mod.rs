// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admin relay (admin relay spec, `2026-11-04-admin-relay-design.md`, kept locally).
//! The relay (`blighted-admin relay`) runs on another machine and takes the browser connections.
//! The agent (`blighted-admin serve` with `[access.relay]`) runs on the game machine, opens a
//! mutual-TLS connection to the relay and serves the admin router over it with HTTP/2. No port
//! opens into the game machine.

pub mod agent;
pub mod config;
pub mod front;
pub mod keys;

use std::time::Duration;

use nw_login::certificate::CertificateError;

/// The only ALPN protocol of the agent connection.
const H2_ALPN: &[u8] = b"h2";
/// Time between two HTTP/2 pings on the agent connection.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);
/// Longest wait for a ping answer before the agent connection counts as dead.
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// The relay or the agent connection failed.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// A certificate or key cannot be read or used.
    #[error(transparent)]
    Certificate(#[from] CertificateError),
    /// `server_name` is not a DNS name.
    #[error("relay server name {0} is not a DNS name")]
    ServerName(String),
    /// A socket or the TLS handshake failed.
    #[error("relay connection: {0}")]
    Io(#[from] std::io::Error),
    /// The HTTP/2 connection failed.
    #[error("relay HTTP/2: {0}")]
    Http(#[from] hyper::Error),
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode, Uri, header};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tokio::net::TcpListener;
    use tokio::sync::watch;
    use tokio_rustls::TlsAcceptor;
    use tower::ServiceExt;

    use super::agent::serve_agent;
    use super::config::AgentTlsConfig;
    use super::front::{AgentLink, NOT_CONNECTED, accept_agents, relay_agent_tls, relay_router};
    use super::keys::{
        AGENT_CERTIFICATE_FILE, AGENT_KEY_FILE, RELAY_CERTIFICATE_FILE, RELAY_KEY_FILE,
        write_relay_keys,
    };
    use crate::config::RelayLinkConfig;

    const RELAY_NAME: &str = "relay.example.test";
    const ADMIN_HOST: &str = "admin.example.test";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    /// Tries while the agent connects; with 50 ms steps, 5 s in all.
    const CONNECT_TRIES: u32 = 100;
    const TRY_STEP: Duration = Duration::from_millis(50);

    fn relay_files(dir: &Path) -> AgentTlsConfig {
        AgentTlsConfig {
            certificate_pem_path: dir.join(RELAY_CERTIFICATE_FILE),
            private_key_pem_path: dir.join(RELAY_KEY_FILE),
            agent_certificate_pem_path: dir.join(AGENT_CERTIFICATE_FILE),
        }
    }

    /// The agent side: it trusts the relay certificate in `relay_dir` and shows the agent
    /// certificate in `agent_dir`.
    fn agent_files(relay_dir: &Path, agent_dir: &Path, address: SocketAddr) -> RelayLinkConfig {
        RelayLinkConfig {
            address: address.to_string(),
            server_name: RELAY_NAME.to_owned(),
            relay_certificate_pem_path: relay_dir.join(RELAY_CERTIFICATE_FILE),
            certificate_pem_path: agent_dir.join(AGENT_CERTIFICATE_FILE),
            private_key_pem_path: agent_dir.join(AGENT_KEY_FILE),
        }
    }

    fn keys() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        write_relay_keys(RELAY_NAME, dir.path()).expect("keys");
        dir
    }

    /// Starts a relay with the files in `dir`; returns its agent address and browser router.
    async fn start_relay(dir: &Path) -> (SocketAddr, Router) {
        let listener = TcpListener::bind(LOOPBACK_ANY_PORT).await.expect("bind");
        let address = listener.local_addr().expect("address");
        let acceptor = TlsAcceptor::from(Arc::new(
            relay_agent_tls(&relay_files(dir)).expect("relay tls"),
        ));
        let link = AgentLink::default();
        tokio::spawn(accept_agents(listener, acceptor, link.clone()));
        (address, relay_router(link))
    }

    /// The agent's app: answers with the authority, path and query it got, and whether a
    /// connection header reached it.
    fn echo_app() -> Router {
        Router::new().route(
            "/echo",
            get(|uri: Uri, headers: HeaderMap| async move {
                let authority = uri.authority().map(ToString::to_string).unwrap_or_default();
                let keep_alive = headers.contains_key("keep-alive");
                format!(
                    "{authority} {uri_path} {keep_alive}",
                    uri_path = uri
                        .path_and_query()
                        .map(ToString::to_string)
                        .unwrap_or_default()
                )
            }),
        )
    }

    fn start_agent(link: RelayLinkConfig) -> watch::Sender<bool> {
        let (close, closing) = watch::channel(false);
        tokio::spawn(async move { serve_agent(&link, echo_app(), closing).await });
        close
    }

    async fn ask(relay: &Router) -> (StatusCode, String) {
        let request = Request::builder()
            .uri("/echo?x=1")
            .header(header::HOST, ADMIN_HOST)
            .header("keep-alive", "timeout=5")
            .body(Body::empty())
            .expect("request");
        let response = relay.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8(body.to_vec()).expect("utf-8"))
    }

    /// Asks until the answer is not "not connected", or the tries run out.
    async fn ask_until_connected(relay: &Router) -> (StatusCode, String) {
        for _ in 0..CONNECT_TRIES {
            let answer = ask(relay).await;
            if answer.0 != StatusCode::SERVICE_UNAVAILABLE {
                return answer;
            }
            tokio::time::sleep(TRY_STEP).await;
        }
        ask(relay).await
    }

    #[tokio::test]
    async fn a_request_goes_to_the_agent_and_back() {
        let dir = keys();
        let (address, relay) = start_relay(dir.path()).await;
        assert_eq!(
            ask(&relay).await,
            (StatusCode::SERVICE_UNAVAILABLE, NOT_CONNECTED.to_owned())
        );
        let close = start_agent(agent_files(dir.path(), dir.path(), address));
        assert_eq!(
            ask_until_connected(&relay).await,
            (StatusCode::OK, format!("{ADMIN_HOST} /echo?x=1 false"))
        );
        close.send(true).expect("close");
        let mut gone = false;
        for _ in 0..CONNECT_TRIES {
            if ask(&relay).await.0 == StatusCode::SERVICE_UNAVAILABLE {
                gone = true;
                break;
            }
            tokio::time::sleep(TRY_STEP).await;
        }
        assert!(gone, "the relay still answers after the agent closed");
    }

    #[tokio::test]
    async fn an_agent_with_another_certificate_is_refused() {
        let relay_dir = keys();
        let other_dir = keys();
        let (address, relay) = start_relay(relay_dir.path()).await;
        let _close = start_agent(agent_files(relay_dir.path(), other_dir.path(), address));
        assert_eq!(
            ask_until_connected(&relay).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn an_agent_never_serves_a_false_relay() {
        let relay_dir = keys();
        let other_dir = keys();
        let (address, relay) = start_relay(relay_dir.path()).await;
        let _close = start_agent(agent_files(other_dir.path(), relay_dir.path(), address));
        assert_eq!(
            ask_until_connected(&relay).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
