// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/status-reporter.toml` with `NW_REPORTER_` overrides.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nw_common::config::{ConfigError, SECRET_ENV_PREFIX, load_config};
use nw_content::leaderboards::{RotationSettings, RotationSettingsError};
use nw_db::{DatabaseConfig, GlobalDatabase};
use serde::Deserialize;
use uuid::Uuid;

/// Environment prefix of `status-reporter` overrides.
pub const REPORTER_ENV_PREFIX: &str = "NW_REPORTER_";
/// The website reads a report every 15 s and shows "No signal" after 45 s without one.
pub const MAX_STATUS_INTERVAL_S: u64 = 15;
/// The website takes at most 4 leaderboard reports a minute.
pub const MIN_LEADERBOARDS_INTERVAL_S: u64 = 15;
const HTTP_SCHEME: &str = "http://";
const HTTPS_SCHEME: &str = "https://";
const PATH_SEPARATOR: char = '/';

/// The whole `status-reporter` configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReporterConfig {
    /// The world, the same `[[world]] id` as in `config/edge.toml` and the website's `WORLDS`.
    pub world_id: Uuid,
    /// The website, without a trailing `/`, such as `https://example.org`.
    pub site_url: String,
    /// The environment variable with the world's report secret (the same secret as in the
    /// website's `WORLDS`); it starts with `NW_SECRET_`.
    pub secret_env: String,
    /// Seconds between two status reports.
    pub status_interval_s: u64,
    /// Seconds between two leaderboard reports.
    pub leaderboards_interval_s: u64,
    /// Longest wait for one metrics page or one website answer, in milliseconds.
    pub request_timeout_ms: u64,
    /// `tracing` filter, for example `info`.
    pub log_filter: String,
    /// The metrics pages of each part of the world.
    pub parts: PartPages,
    /// The pinned client build file: it names the leaderboard table.
    pub client_build_config: PathBuf,
    /// Folder of the leaderboard table (`leaderboards-<build>.json` of
    /// `content-tool leaderboards`).
    pub content_dir: PathBuf,
    /// The rotations: the same values as `[leaderboards]` of `config/sim.toml`.
    pub leaderboards: RotationSettings,
    /// The global database with the leaderboard values.
    pub global_database: GlobalDatabase,
    /// The world database with the character and company names.
    pub world_database: DatabaseConfig,
    /// The environment variable with the world database password; it starts with `NW_SECRET_`.
    pub world_database_password_env: String,
}

/// The metrics pages of each part. A part is up when every page of it answers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartPages {
    /// `edge-http` (`metrics_listen` of `config/edge.toml`).
    pub login: Vec<String>,
    /// `rep-gateway` (`config/rep-gateway.toml`).
    pub gateway: Vec<String>,
    /// `world-director` (`config/world-director.toml`).
    pub director: Vec<String>,
    /// Every sim of the world (`metrics_listen` of each sim).
    pub sims: Vec<String>,
    /// `social` (`config/social.toml`).
    pub social: Vec<String>,
}

/// A value of the configuration that is not allowed.
#[derive(Debug, thiserror::Error)]
pub enum ReporterConfigError {
    /// The file cannot be read or has a wrong key or type.
    #[error(transparent)]
    Load(#[from] ConfigError),
    /// The rotations are not valid.
    #[error(transparent)]
    Rotations(#[from] RotationSettingsError),
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
/// Returns [`ReporterConfigError`] when the file is missing, has a wrong key, or a value is not
/// allowed.
pub fn load_reporter_config(path: &Path) -> Result<ReporterConfig, ReporterConfigError> {
    let config: ReporterConfig = load_config(path, REPORTER_ENV_PREFIX)?;
    config.validate()?;
    Ok(config)
}

impl ReporterConfig {
    fn validate(&self) -> Result<(), ReporterConfigError> {
        let invalid = |key, reason| Err(ReporterConfigError::Invalid { key, reason });
        let url = self.site_url.as_str();
        if !(url.starts_with(HTTP_SCHEME) || url.starts_with(HTTPS_SCHEME))
            || url.ends_with(PATH_SEPARATOR)
        {
            return invalid(
                "site_url",
                "must start with http:// or https:// and have no trailing /",
            );
        }
        let secret_envs = [
            ("secret_env", self.secret_env.as_str()),
            (
                "global_database.password_env",
                self.global_database.password_env.as_str(),
            ),
            (
                "world_database_password_env",
                self.world_database_password_env.as_str(),
            ),
        ];
        for (key, env) in secret_envs {
            if !env.starts_with(SECRET_ENV_PREFIX) {
                return invalid(key, "must start with NW_SECRET_");
            }
        }
        if !(1..=MAX_STATUS_INTERVAL_S).contains(&self.status_interval_s) {
            return invalid("status_interval_s", "must be 1 to 15");
        }
        if self.leaderboards_interval_s < MIN_LEADERBOARDS_INTERVAL_S {
            return invalid("leaderboards_interval_s", "must be at least 15");
        }
        if self.request_timeout_ms == 0
            || self.request_timeout() >= Duration::from_secs(self.status_interval_s)
        {
            return invalid(
                "request_timeout_ms",
                "must be above 0 and below the status interval",
            );
        }
        let pages = &self.parts;
        if [
            &pages.login,
            &pages.gateway,
            &pages.director,
            &pages.sims,
            &pages.social,
        ]
        .iter()
        .any(|urls| urls.is_empty())
        {
            return invalid("parts", "every part needs at least one metrics page");
        }
        self.leaderboards.calendar()?;
        Ok(())
    }

    /// [`ReporterConfig::request_timeout_ms`].
    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMITTED_CONFIG: &str = "../../config/status-reporter.toml";
    const SIM_CONFIG: &str = "../../config/sim.toml";
    const SOCIAL_CONFIG: &str = "../../config/social.toml";

    fn committed() -> ReporterConfig {
        load_reporter_config(Path::new(COMMITTED_CONFIG)).expect("committed config")
    }

    /// The value of the top-level key `key` of the TOML file `path`.
    fn value_of(path: &str, key: &str) -> toml::Value {
        let text = std::fs::read_to_string(path).expect("config");
        let table: toml::Table = text.parse().expect("toml");
        table.get(key).cloned().expect("key")
    }

    #[test]
    fn the_committed_config_loads() {
        let config = committed();
        assert_eq!(config.status_interval_s, MAX_STATUS_INTERVAL_S);
        assert!(config.leaderboards_interval_s >= MIN_LEADERBOARDS_INTERVAL_S);
    }

    #[test]
    fn the_committed_config_matches_the_sim_and_social_configs() {
        let config = committed();
        let sim_rotations: RotationSettings = value_of(SIM_CONFIG, "leaderboards")
            .try_into()
            .expect("sim rotations");
        assert_eq!(config.leaderboards, sim_rotations);
        assert_eq!(
            toml::Value::String(config.world_id.to_string()),
            value_of(SOCIAL_CONFIG, "world_id")
        );
        assert_eq!(
            toml::Value::String(config.world_database_password_env.clone()),
            value_of(SOCIAL_CONFIG, "world_database_password_env")
        );
    }

    #[test]
    fn bad_values_are_refused() {
        let refused = |change: fn(&mut ReporterConfig)| {
            let mut config = committed();
            change(&mut config);
            config.validate().is_err()
        };
        assert!(refused(|config| config.site_url = "example.org".to_owned()));
        assert!(refused(
            |config| config.site_url = "https://example.org/".to_owned()
        ));
        assert!(refused(
            |config| config.secret_env = "REPORT_SECRET".to_owned()
        ));
        assert!(refused(|config| {
            config.global_database.password_env = "PASSWORD".to_owned();
        }));
        assert!(refused(|config| config.status_interval_s = 0));
        assert!(refused(|config| config.status_interval_s = 16));
        assert!(refused(|config| config.leaderboards_interval_s = 14));
        assert!(refused(|config| config.request_timeout_ms = 0));
        assert!(refused(|config| config.request_timeout_ms = 15_000));
        assert!(refused(|config| config.parts.sims.clear()));
        assert!(refused(|config| config.leaderboards.weekly_reset_hour = 24));
        assert!(!refused(|config| {
            config.site_url = "http://localhost:3000".to_owned();
        }));
    }
}
