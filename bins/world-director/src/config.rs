// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/world-director.toml` with `NW_DIRECTOR_` overrides (M5 spec section 6).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use nw_common::config::{ConfigError, SECRET_ENV_PREFIX, load_config};
use nw_db::DatabaseConfig;
use nw_sim_core::{Ground, Position};
use serde::Deserialize;
use uuid::Uuid;

/// Environment prefix of `world-director` overrides.
pub const DIRECTOR_ENV_PREFIX: &str = "NW_DIRECTOR_";

/// The whole `world-director` configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorConfig {
    /// The world, the same `[[world]] id` as in `config/edge.toml`.
    pub world_id: Uuid,
    /// NATS, internal only.
    pub nats_url: String,
    /// Valkey of `edge-http`, for the free player slots.
    pub valkey_url: String,
    /// The world database.
    pub world_database: DatabaseConfig,
    /// The environment variable with the world database password; it starts with `NW_SECRET_`.
    pub world_database_password_env: String,
    /// The global database, for the admin ranks and the account ages: an account with the game
    /// master rank gets the client GM flag, and a young account without a rank is in probation.
    /// Without it no account gets GM rights and none is in probation.
    #[serde(default)]
    pub global_database: Option<GlobalDatabase>,
    /// The pinned client build file.
    pub client_build_config: PathBuf,
    /// Folder of the world files (`world-<build>-<level>.bin`).
    pub content_dir: PathBuf,
    /// The open map level.
    pub level: String,
    /// Where characters that were never in the world appear, in turn.
    pub start_positions: Vec<StartPosition>,
    /// Edge length of a cell, in metres.
    pub cell_size_m: f32,
    /// Longest wait for a sim to take a player.
    pub admit_timeout_ms: u64,
    /// Time between table publications and free-slot reports; sims beat at the same interval.
    pub heartbeat_interval_ms: u64,
    /// Missed sim heartbeats before its cells go to the other sims.
    pub heartbeat_misses: u32,
    /// Internal Prometheus listener.
    pub metrics_listen: SocketAddr,
    /// `tracing` filter, for example `info`.
    pub log_filter: String,
    /// The instances (expeditions, raids, trials); without this section the world opens none.
    #[serde(default)]
    pub instances: Option<InstancesConfig>,
}

/// The `[instances]` section: how the director runs the instance sims. These are operator
/// settings (resource use), not game values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstancesConfig {
    /// An instance with no player for this long ends and its sim is free again.
    pub empty_timeout_ms: u64,
    /// Longest wait for an instance sim to open an instance.
    pub open_timeout_ms: u64,
    /// Longest wait for the social service's answer about a group.
    pub group_timeout_ms: u64,
    /// How the director starts instance sims itself; without it the operator starts them
    /// (each with `role = "instance"` and the level of its expeditions).
    #[serde(default)]
    pub launch: Option<LaunchConfig>,
}

/// The `[instances.launch]` section: the command that starts one instance sim. The director
/// adds the sim's id, level and role as `NW_SIM_SIM_ID`, `NW_SIM_LEVEL` and `NW_SIM_ROLE`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    /// The `sim` program.
    pub program: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Free instance sims each level keeps ready while players ask for it.
    pub spare_per_level: u32,
    /// Most instance sims the director runs at once.
    pub max_sims: u32,
    /// Prefix of the ids of the sims it starts.
    pub id_prefix: String,
    /// A level nobody asked for in this long keeps no spare sim.
    pub forget_after_ms: u64,
}

/// The global database and the environment variable with its password (it starts with
/// `NW_SECRET_`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalDatabase {
    /// Where it is.
    pub database: DatabaseConfig,
    /// The environment variable with its password.
    pub password_env: String,
}

/// A start position, in metres. The height comes from the ground of the level.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartPosition {
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
}

/// A value of the configuration that is not allowed.
#[derive(Debug, thiserror::Error)]
pub enum DirectorConfigError {
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
/// Returns [`DirectorConfigError`] when the file is missing, has a wrong key, or a value is not
/// allowed.
pub fn load_director_config(path: &Path) -> Result<DirectorConfig, DirectorConfigError> {
    let config: DirectorConfig = load_config(path, DIRECTOR_ENV_PREFIX)?;
    config.validate()?;
    Ok(config)
}

impl DirectorConfig {
    fn validate(&self) -> Result<(), DirectorConfigError> {
        let invalid = |key, reason| Err(DirectorConfigError::Invalid { key, reason });
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
        if self.level.is_empty() {
            return invalid("level", "must not be empty");
        }
        let finite = |start: &StartPosition| start.x.is_finite() && start.y.is_finite();
        if self.start_positions.is_empty() || !self.start_positions.iter().all(finite) {
            return invalid("start_positions", "must hold at least one finite position");
        }
        if !(self.cell_size_m.is_finite() && self.cell_size_m > 0.0) {
            return invalid("cell_size_m", "must be a finite number above 0");
        }
        if self.admit_timeout_ms == 0 || self.heartbeat_interval_ms == 0 {
            return invalid("timeouts", "must be above 0");
        }
        if self.heartbeat_misses == 0 {
            return invalid("heartbeat_misses", "must be above 0");
        }
        if let Some(instances) = &self.instances {
            if instances.empty_timeout_ms == 0
                || instances.open_timeout_ms == 0
                || instances.group_timeout_ms == 0
            {
                return invalid("instances", "timeouts must be above 0");
            }
            if instances.open_timeout_ms >= self.admit_timeout_ms {
                return invalid(
                    "instances.open_timeout_ms",
                    "must be below admit_timeout_ms",
                );
            }
            if let Some(launch) = &instances.launch
                && (launch.program.is_empty()
                    || launch.id_prefix.is_empty()
                    || launch.max_sims == 0
                    || launch.forget_after_ms == 0)
            {
                return invalid(
                    "instances.launch",
                    "needs a program, an id prefix, max_sims and forget_after_ms above 0",
                );
            }
        }
        Ok(())
    }

    /// The start positions on the ground of `ground`.
    ///
    /// # Errors
    ///
    /// Returns [`DirectorConfigError::Invalid`] when the level has no height at one of them.
    pub fn starts(&self, ground: &impl Ground) -> Result<Vec<Position>, DirectorConfigError> {
        self.start_positions
            .iter()
            .map(|&StartPosition { x, y }| {
                let z = ground.height_at(x, y).ok_or(DirectorConfigError::Invalid {
                    key: "start_positions",
                    reason: "the level has no ground height at one of them",
                })?;
                Ok(Position { x, y, z })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use nw_sim_core::Ground;

    use super::*;

    const COMMITTED_CONFIG: &str = "../../config/world-director.toml";
    const GROUND_HEIGHT: f32 = 168.0;

    struct Flat;

    impl Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_HEIGHT)
        }
    }

    struct Nothing;

    impl Ground for Nothing {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            None
        }
    }

    fn committed() -> DirectorConfig {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        load_director_config(&path).expect("committed config")
    }

    #[test]
    fn the_committed_config_is_valid_and_starts_on_the_ground() {
        let config = committed();
        assert_eq!(config.level, "newworld_vitaeeterna");
        let starts = config.starts(&Flat).expect("starts");
        assert_eq!(starts[0].z, GROUND_HEIGHT);
        assert_eq!(starts[0].x, config.start_positions[0].x);
        assert!(config.starts(&Nothing).is_err());
        assert!(config.admit_timeout_ms > 0);
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = committed();
        config.world_database_password_env = "PASSWORD".to_owned();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.start_positions[0].y = f32::NAN;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.start_positions.clear();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.cell_size_m = 0.0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.heartbeat_misses = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.admit_timeout_ms = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.level = String::new();
        assert!(config.validate().is_err());
        let mut config = committed();
        let global = config.global_database.as_mut().expect("global database");
        global.password_env = "PASSWORD".to_owned();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.global_database = None;
        assert!(config.validate().is_ok(), "the ranks are optional");
    }

    #[test]
    fn the_instance_section_is_checked() {
        let config = committed();
        let instances = config.instances.clone().expect("instances");
        assert!(instances.open_timeout_ms < config.admit_timeout_ms);
        let mut bad = config.clone();
        if let Some(instances) = bad.instances.as_mut() {
            instances.empty_timeout_ms = 0;
        }
        assert!(bad.validate().is_err());
        let mut bad = config.clone();
        if let Some(instances) = bad.instances.as_mut() {
            instances.open_timeout_ms = config.admit_timeout_ms;
        }
        assert!(bad.validate().is_err());
        let mut bad = config.clone();
        if let Some(instances) = bad.instances.as_mut() {
            instances.launch = Some(LaunchConfig {
                program: String::new(),
                args: Vec::new(),
                spare_per_level: 1,
                max_sims: 1,
                id_prefix: "inst".to_owned(),
                forget_after_ms: 1,
            });
        }
        assert!(bad.validate().is_err());
        let mut none = config;
        none.instances = None;
        assert!(none.validate().is_ok(), "a world without instances");
    }
}
