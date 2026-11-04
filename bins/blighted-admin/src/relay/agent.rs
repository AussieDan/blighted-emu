// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent side (admin relay spec section 5.2): `blighted-admin serve` with `[access.relay]`
//! connects to the relay, proves itself with its certificate, checks the relay certificate, and
//! serves the admin router over that connection until it ends. Then it connects again.

use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper::server::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use nw_login::certificate::{CertificateError, read_chain, read_private_key};
use nw_login::install_crypto_provider;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

use crate::config::RelayLinkConfig;
use crate::relay::{H2_ALPN, KEEP_ALIVE_INTERVAL, KEEP_ALIVE_TIMEOUT, RelayError};
use crate::run::SHUTDOWN_GRACE;

/// Wait before the next connection after one fails or ends.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// The TLS client settings of `link`: the relay certificate is the only root, and this site
/// shows its own certificate. Also the relay's server name.
///
/// # Errors
///
/// Returns [`RelayError`] when a file cannot be read or used, or the name is not a DNS name.
pub fn agent_tls(
    link: &RelayLinkConfig,
) -> Result<(ClientConfig, ServerName<'static>), RelayError> {
    install_crypto_provider();
    let mut roots = RootCertStore::empty();
    for certificate in read_chain(&link.relay_certificate_pem_path)? {
        roots.add(certificate).map_err(CertificateError::from)?;
    }
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            read_chain(&link.certificate_pem_path)?,
            read_private_key(&link.private_key_pem_path)?,
        )
        .map_err(CertificateError::from)?;
    config.alpn_protocols = vec![H2_ALPN.to_vec()];
    let name = ServerName::try_from(link.server_name.clone())
        .map_err(|_| RelayError::ServerName(link.server_name.clone()))?;
    Ok((config, name))
}

/// Serves `app` to the relay of `link` until `closing` turns true, and connects again
/// [`RECONNECT_DELAY`] after each failed or ended connection.
///
/// # Errors
///
/// Returns [`RelayError`] when the certificates cannot be used; connection failures are logged
/// and retried.
pub async fn serve_agent(
    link: &RelayLinkConfig,
    app: Router,
    mut closing: watch::Receiver<bool>,
) -> Result<(), RelayError> {
    let (tls, name) = agent_tls(link)?;
    let connector = TlsConnector::from(Arc::new(tls));
    loop {
        match serve_once(
            &connector,
            &link.address,
            name.clone(),
            app.clone(),
            closing.clone(),
        )
        .await
        {
            Ok(()) => tracing::info!(relay = %link.address, "relay connection ended"),
            Err(error) => tracing::warn!(relay = %link.address, %error, "relay connection failed"),
        }
        tokio::select! {
            () = tokio::time::sleep(RECONNECT_DELAY) => {}
            _ = closing.wait_for(|closed| *closed) => return Ok(()),
        }
    }
}

/// One connection: connect, serve `app` until the connection ends or `closing` turns true.
async fn serve_once(
    connector: &TlsConnector,
    address: &str,
    name: ServerName<'static>,
    app: Router,
    mut closing: watch::Receiver<bool>,
) -> Result<(), RelayError> {
    let stream = tokio::select! {
        stream = TcpStream::connect(address) => stream?,
        _ = closing.wait_for(|closed| *closed) => return Ok(()),
    };
    let stream = connector.connect(name, stream).await?;
    tracing::info!(relay = %address, "connected to the relay");
    let mut connection = pin!(
        http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .keep_alive_interval(KEEP_ALIVE_INTERVAL)
            .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
    );
    tokio::select! {
        result = connection.as_mut() => return Ok(result?),
        _ = closing.wait_for(|closed| *closed) => {}
    }
    connection.as_mut().graceful_shutdown();
    // Requests still open after the grace time end with the connection.
    match tokio::time::timeout(SHUTDOWN_GRACE, connection).await {
        Ok(result) => Ok(result?),
        Err(_) => Ok(()),
    }
}
