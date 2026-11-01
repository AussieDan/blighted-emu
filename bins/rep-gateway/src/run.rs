// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve` and `generate-certificate`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusBuilder;
use nw_bus::{Bus, BusError};
use nw_common::logging::{LoggingError, init_logging};
use nw_dtls::{COOKIE_SECRET_LEN, DtlsError, DtlsServerConfig, self_signed_identity};
use nw_login::secrets::{SIGNING_KEY_ENV, SecretError, SigningKey};
use nw_login::store::StoreError;
use nw_login::valkey::Valkey;

use crate::config::{GatewayConfig, GatewayConfigError, load_gateway_config};
use crate::server::{Gateway, serve as serve_udp, with_round_trip_buckets};
use crate::workers::{ConnectionCount, bind_workers};
use crate::world::BusWorld;

/// Common name of a generated gateway certificate.
pub const CERTIFICATE_NAME: &str = "blighted-rep-gateway";
/// Owner read and write only, for the private key file.
#[cfg(unix)]
const PRIVATE_KEY_MODE: u32 = 0o600;

/// Failure while starting or running `rep-gateway`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration file is missing or not valid.
    #[error(transparent)]
    Config(#[from] GatewayConfigError),
    /// The signing key is missing or not valid.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// Logging could not start.
    #[error(transparent)]
    Logging(#[from] LoggingError),
    /// The async runtime could not start.
    #[error("runtime failed")]
    Runtime(#[source] std::io::Error),
    /// A certificate or key file cannot be read or written.
    #[error("certificate file {path} failed")]
    CertificateFile {
        /// File.
        path: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// The certificate, key, or DTLS setup failed.
    #[error(transparent)]
    Dtls(#[from] DtlsError),
    /// Valkey could not be reached.
    #[error("Valkey connection failed")]
    Valkey(#[source] StoreError),
    /// NATS could not be reached, or a subscription failed.
    #[error(transparent)]
    Bus(#[from] BusError),
    /// The UDP socket failed.
    #[error("UDP socket failed")]
    Socket(#[source] std::io::Error),
    /// The Prometheus listener could not start.
    #[error("metrics listener failed: {0}")]
    Metrics(String),
    /// No random bytes for the cookie secret.
    #[error("no random bytes: {0}")]
    Random(getrandom::Error),
}

fn read_file(path: &Path) -> Result<Vec<u8>, RunError> {
    std::fs::read(path).map_err(|source| RunError::CertificateFile {
        path: path.display().to_string(),
        source,
    })
}

/// Serves UDP until Ctrl-C.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, the key, the certificate, Valkey, or the socket
/// fails.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_gateway_config(config_path)?;
    let key = SigningKey::from_hex(
        &std::env::var(SIGNING_KEY_ENV)
            .map_err(|_| SecretError::Missing(SIGNING_KEY_ENV.to_owned()))?,
    )?;
    init_logging(&config.log_filter)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Runtime)?
        .block_on(serve_async(config, key))
}

async fn serve_async(config: GatewayConfig, key: SigningKey) -> Result<(), RunError> {
    let mut cookie_secret = [0_u8; COOKIE_SECRET_LEN];
    getrandom::fill(&mut cookie_secret).map_err(RunError::Random)?;
    let dtls = DtlsServerConfig::new(
        &read_file(&config.certificate_file)?,
        &read_file(&config.private_key_file)?,
        cookie_secret,
    )?;
    let store = Valkey::connect(&config.valkey_url)
        .await
        .map_err(RunError::Valkey)?;
    let bus = Bus::connect(&config.nats_url).await?;
    let (world, events) = BusWorld::start(bus, config.world_link(), config.workers).await?;
    with_round_trip_buckets(PrometheusBuilder::new().with_http_listener(config.metrics_listen))
        .and_then(PrometheusBuilder::install)
        .map_err(|error| RunError::Metrics(error.to_string()))?;
    tracing::info!(address = %config.metrics_listen, "serving metrics");
    let sockets = bind_workers(config.udp_listen, config.workers, config.udp_buffer_bytes)
        .map_err(RunError::Socket)?;
    tracing::info!(
        address = %config.udp_listen,
        world = %config.world_id,
        workers = config.workers,
        "serving UDP"
    );
    let gateway = Gateway {
        dtls: Arc::new(dtls),
        store,
        signing_key: Arc::new(key),
        world_id: config.world_id,
        server_version: config.server_version.clone(),
        peer_settings: config.peer_settings(),
        limits: config.worker_limits(),
        connections: ConnectionCount::new(config.limits.max_connections),
        world,
    };
    let mut workers = tokio::task::JoinSet::new();
    for (worker, (socket, events)) in (0_u16..).zip(sockets.into_iter().zip(events)) {
        let shutdown = async {
            // A failed signal handler means the gateway runs until killed.
            let _ = tokio::signal::ctrl_c().await;
        };
        workers.spawn(serve_udp(gateway.clone(), socket, events, worker, shutdown));
    }
    while let Some(done) = workers.join_next().await {
        done.map_err(|error| RunError::Socket(std::io::Error::other(error)))?
            .map_err(RunError::Socket)?;
    }
    Ok(())
}

/// Writes a new self-signed certificate and key to the configured files. Existing files are
/// never replaced.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration fails, a file exists, or a file cannot be written.
pub fn generate_certificate(config_path: &Path, valid_days: u32) -> Result<(), RunError> {
    let config = load_gateway_config(config_path)?;
    let identity = self_signed_identity(CERTIFICATE_NAME, valid_days)?;
    write_new(&config.certificate_file, &identity.certificate_pem, false)?;
    write_new(&config.private_key_file, &identity.private_key_pem, true)?;
    println!(
        "wrote {} and {}",
        config.certificate_file.display(),
        config.private_key_file.display()
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), RunError> {
    let failed = |source| RunError::CertificateFile {
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_KEY_MODE);
    }
    #[cfg(not(unix))]
    let _ = private;
    options
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAYS: u32 = 1;

    #[test]
    fn generated_files_load_and_are_never_replaced() {
        let dir = tempfile::tempdir().expect("dir");
        let certificate = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        let identity = self_signed_identity(CERTIFICATE_NAME, DAYS).expect("identity");
        write_new(&certificate, &identity.certificate_pem, false).expect("certificate");
        write_new(&key, &identity.private_key_pem, true).expect("key");
        let secret = [0_u8; COOKIE_SECRET_LEN];
        DtlsServerConfig::new(
            &read_file(&certificate).expect("read"),
            &read_file(&key).expect("read"),
            secret,
        )
        .expect("loads");
        assert!(matches!(
            write_new(&certificate, b"other", false),
            Err(RunError::CertificateFile { .. })
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, PRIVATE_KEY_MODE);
        }
    }
}
