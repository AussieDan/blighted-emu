// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/social.toml` with `NW_SOCIAL_` overrides.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use nw_common::config::{ConfigError, SECRET_ENV_PREFIX, load_config};
use nw_db::{DatabaseConfig, GlobalDatabase};
use serde::Deserialize;
use uuid::Uuid;

/// Environment prefix of `social` overrides.
pub const SOCIAL_ENV_PREFIX: &str = "NW_SOCIAL_";
/// The default time between two reads of the changed privacy settings.
pub const DEFAULT_PRIVACY_POLL_MS: u64 = 5000;

const fn default_privacy_poll_ms() -> u64 {
    DEFAULT_PRIVACY_POLL_MS
}

/// The whole `social` configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialConfig {
    /// The world, the same `[[world]] id` as in `config/edge.toml`.
    pub world_id: Uuid,
    /// NATS, internal only.
    pub nats_url: String,
    /// The world database.
    pub world_database: DatabaseConfig,
    /// The environment variable with the world database password; it starts with `NW_SECRET_`.
    pub world_database_password_env: String,
    /// The game install whose paks hold the profanity lists (`dictionary/*/profanity.csv`).
    pub game_dir: PathBuf,
    /// Time between checks of the sim heartbeats; sims beat at this interval.
    pub heartbeat_interval_ms: u64,
    /// Missed sim heartbeats before its players count as offline.
    pub heartbeat_misses: u32,
    /// Longest wait for a sim to move a player's coins.
    pub coin_timeout_ms: u64,
    /// The company rules.
    pub companies: CompanyConfig,
    /// The voice service; leave it out when the community runs none.
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    /// The global database with the players' privacy settings; leave it out and every setting
    /// keeps its default (everyone may write, talk, ask for friendship and see the online
    /// status).
    #[serde(default)]
    pub global_database: Option<GlobalDatabase>,
    /// Time between two reads of the privacy settings that changed (Inferred default
    /// [`DEFAULT_PRIVACY_POLL_MS`]: a change reaches the world within seconds, at one small
    /// read each time).
    #[serde(default = "default_privacy_poll_ms")]
    pub privacy_poll_ms: u64,
    /// Internal Prometheus listener.
    pub metrics_listen: SocketAddr,
    /// `tracing` filter, for example `info`.
    pub log_filter: String,
}

/// The company rules: the client's `javelin.social.*` keys of the same names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyConfig {
    /// Seconds after leaving a company before a player joins one again
    /// (`guild-rejoining-cooldown-seconds` and `guild-switching-cooldown-seconds`).
    pub join_cooldown_s: u64,
    /// The least zero-based level that joins a company (`guild-join-required-level`).
    pub join_required_level: u32,
    /// The most a treasury holds, hundredths of a coin.
    pub treasury_max: u64,
}

/// A Vivox-compatible voice service the community runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {
    /// Its API address, sent to the client.
    pub api_url: String,
    /// The token issuer.
    pub issuer: String,
    /// The SIP domain host.
    pub domain: String,
    /// The environment variable with the token signing key; it starts with `NW_SECRET_`.
    pub key_env: String,
    /// The suffix of every channel name of this world.
    pub channel_suffix: String,
    /// Edge length of the place of a positional channel, in metres.
    pub cell_size_m: f32,
}

/// A value of the configuration that is not allowed.
#[derive(Debug, thiserror::Error)]
pub enum SocialConfigError {
    /// The file cannot be read or has a wrong key or type.
    #[error(transparent)]
    Load(#[from] ConfigError),
    /// A value is not allowed.
    #[error("config value `{key}` is not valid: {reason}")]
    Invalid {
        /// Key path.
        key: &'static str,
        /// Why the value is refused.
        reason: &'static str,
    },
}

/// Reads and checks the configuration.
///
/// # Errors
///
/// Returns [`SocialConfigError`] when the file is missing, has a wrong key, or a value is not
/// allowed.
pub fn load_social_config(path: &Path) -> Result<SocialConfig, SocialConfigError> {
    let config: SocialConfig = load_config(path, SOCIAL_ENV_PREFIX)?;
    config.validate()?;
    Ok(config)
}

impl SocialConfig {
    fn validate(&self) -> Result<(), SocialConfigError> {
        let invalid = |key, reason| Err(SocialConfigError::Invalid { key, reason });
        if !self
            .world_database_password_env
            .starts_with(SECRET_ENV_PREFIX)
        {
            return invalid("world_database_password_env", "must start with NW_SECRET_");
        }
        if self
            .global_database
            .as_ref()
            .is_some_and(|global| !global.password_env.starts_with(SECRET_ENV_PREFIX))
        {
            return invalid("global_database.password_env", "must start with NW_SECRET_");
        }
        if self.heartbeat_interval_ms == 0 || self.coin_timeout_ms == 0 || self.privacy_poll_ms == 0
        {
            return invalid("timeouts", "must be above 0");
        }
        if self.heartbeat_misses == 0 {
            return invalid("heartbeat_misses", "must be above 0");
        }
        if self.companies.treasury_max == 0 {
            return invalid("companies.treasury_max", "must be above 0");
        }
        if let Some(voice) = &self.voice {
            if !voice.key_env.starts_with(SECRET_ENV_PREFIX) {
                return invalid("voice.key_env", "must start with NW_SECRET_");
            }
            if !(voice.cell_size_m.is_finite() && voice.cell_size_m > 0.0) {
                return invalid("voice.cell_size_m", "must be a finite number above 0");
            }
            if voice.issuer.is_empty() || voice.domain.is_empty() {
                return invalid("voice", "issuer and domain must not be empty");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMITTED_CONFIG: &str = "../../config/social.toml";

    fn committed() -> SocialConfig {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        load_social_config(&path).expect("committed config")
    }

    #[test]
    fn the_committed_config_is_valid() {
        let config = committed();
        assert!(config.voice.is_none(), "no voice service by default");
        assert!(config.heartbeat_misses > 0);
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = committed();
        config.world_database_password_env = "PASSWORD".to_owned();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.coin_timeout_ms = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.companies.treasury_max = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.privacy_poll_ms = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.global_database = Some(GlobalDatabase {
            database: config.world_database.clone(),
            password_env: "PASSWORD".to_owned(),
        });
        assert!(
            config.validate().is_err(),
            "the password must be a secret variable"
        );
        let mut config = committed();
        config.voice = Some(VoiceConfig {
            api_url: "https://voice.example/api2/".to_owned(),
            issuer: "issuer".to_owned(),
            domain: "voice.example".to_owned(),
            key_env: "KEY".to_owned(),
            channel_suffix: String::new(),
            cell_size_m: 512.0,
        });
        assert!(
            config.validate().is_err(),
            "the key must be a secret variable"
        );
    }
}
