// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/blighted-admin-relay.toml` with `BLIGHTED_ADMIN_RELAY_` overrides (admin relay spec
//! section 5.1).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use nw_common::config::{ConfigError, load_config};
use nw_login::config::TlsConfig;
use serde::Deserialize;

/// Prefix of the environment overrides.
pub const RELAY_ENV_PREFIX: &str = "BLIGHTED_ADMIN_RELAY_";

/// The relay configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// The browser address.
    pub listen: SocketAddr,
    /// `tracing` filter.
    pub log_filter: String,
    /// HTTPS for the browsers. Without it the relay serves plain HTTP for a TLS proxy in front;
    /// the browsers must still use HTTPS (the session cookie is `Secure`).
    pub tls: Option<TlsConfig>,
    /// The address the agent connects to.
    pub agent_listen: SocketAddr,
    /// The certificates of the agent connection.
    pub agent_tls: AgentTlsConfig,
}

/// `[agent_tls]`: the files of `blighted-admin relay-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTlsConfig {
    /// The relay certificate (`relay.pem`).
    pub certificate_pem_path: PathBuf,
    /// Its private key (`relay-key.pem`).
    pub private_key_pem_path: PathBuf,
    /// The agent certificate (`agent.pem`); the agent must show exactly it.
    pub agent_certificate_pem_path: PathBuf,
}

/// Reads `path` with `BLIGHTED_ADMIN_RELAY_` overrides.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file or an override cannot be read.
pub fn load_relay_config(path: &Path) -> Result<RelayConfig, ConfigError> {
    load_config(path, RELAY_ENV_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_relay_config_loads() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/blighted-admin-relay.toml");
        let config = load_relay_config(&path).expect("load");
        assert!(config.tls.is_none());
        assert_ne!(config.listen.port(), config.agent_listen.port());
        assert_eq!(
            config.agent_tls.agent_certificate_pem_path,
            Path::new("local-data/admin-relay/agent.pem")
        );
    }
}
