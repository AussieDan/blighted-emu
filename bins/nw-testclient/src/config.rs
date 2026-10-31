// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/nw-testclient.toml` with `NW_TESTCLIENT_` overrides (M3 spec section 8, M4 spec
//! section 9, M5 spec section 11).

use std::path::{Path, PathBuf};
use std::time::Duration;

use nw_common::config::{ConfigError, load_config};
use nw_login::names::CHARACTER_NAME_MAX_LEN;
use serde::Deserialize;

use crate::fleet::{FleetSettings, MAX_BOTS, NAME_SUFFIX_LETTERS};
use crate::hunt::{HuntSettings, MAX_BOTS_PER_START};
use crate::route::{Route, RouteError};
use crate::swarm::SwarmSettings;

/// Environment prefix of overrides.
pub const TESTCLIENT_ENV_PREFIX: &str = "NW_TESTCLIENT_";
/// Environment variable with the account password.
pub const PASSWORD_ENV: &str = "NW_SECRET_TESTCLIENT_PASSWORD";
/// Environment variable with the password of the second account (`pair`).
pub const SECOND_PASSWORD_ENV: &str = "NW_SECRET_TESTCLIENT_PASSWORD_B";
/// Environment variable with the password of every bot account (`swarm`).
pub const SWARM_PASSWORD_ENV: &str = "NW_SECRET_TESTCLIENT_SWARM_PASSWORD";

/// The whole test client configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestClientConfig {
    /// `https://login.<domain>`.
    pub login_url: String,
    /// `https://game.<domain>`.
    pub game_url: String,
    /// Account name; registered when it is new.
    pub account: String,
    /// Character created when the account has none.
    pub character_name: String,
    /// The second account of `pair`.
    pub second_account: String,
    /// The character of the second account.
    pub second_character_name: String,
    /// The gateway's certificate; the client trusts only this file.
    pub certificate_file: PathBuf,
    /// DTLS MTU.
    pub mtu: u32,
    /// Seconds to stay connected after registration.
    pub hold_seconds: u64,
    /// Seconds to wait for the registration response.
    pub registration_timeout_seconds: u64,
    /// Largest distance between the saved position and the next login, in metres (`pair`).
    pub position_tolerance_m: f32,
    /// The walk of `pair`: points east and north of where the first client stands, in metres.
    pub route_m: Vec<[f32; 2]>,
    /// Walking speed of `pair`, in metres per second.
    pub route_speed_mps: f32,
    /// The far move of `pair` after the walk, east, in metres; the world must refuse it.
    pub jump_m: f32,
    /// Largest distance of a seen position from the route, in metres (`pair` and `swarm`).
    pub path_tolerance_m: f32,
    /// `[swarm]`: the bots of the M5 exit test.
    pub swarm: SwarmConfig,
    /// `[hunt]`: the bots of the M6-B exit test.
    pub hunt: HuntConfig,
}

/// `[hunt]` (M6-B spec section 11). The sockets, speed and ground are those of `[swarm]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HuntConfig {
    /// Bots to run.
    pub bots: usize,
    /// Account names are this and five digits; not the swarm's, whose bots stand elsewhere.
    pub account_prefix: String,
    /// Character names are this and three letters; letters only.
    pub character_prefix: String,
    /// New bots per second.
    pub bots_per_second: u32,
    /// The hunt starts, east and north, in metres.
    pub starts_m: Vec<[f32; 2]>,
    /// A bot attacks a mob within this distance, in metres.
    pub reach_m: f32,
    /// Time between two attacks of a bot, in milliseconds.
    pub attack_interval_ms: u64,
    /// Time from a 4299 to the bot's 1938, in milliseconds.
    pub respawn_wait_ms: u64,
    /// Seconds in the world per bot.
    pub duration_seconds: u64,
    /// Longest gap between two own bundles, in milliseconds.
    pub max_bundle_gap_ms: u64,
    /// The metrics URL of every sim.
    pub metrics_urls: Vec<String>,
}

/// `[swarm]` (M5 spec section 11).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmConfig {
    /// Bots to run.
    pub bots: usize,
    /// world-director's `start_positions`, east and north, in metres; their x must be sim
    /// borders.
    pub starts_m: Vec<[f32; 2]>,
    /// Receive and send buffer of each bot socket, in bytes.
    pub udp_buffer_bytes: usize,
    /// Account names are this and five digits.
    pub account_prefix: String,
    /// Character names are this and three letters; letters only.
    pub character_prefix: String,
    /// New bots per second.
    pub bots_per_second: u32,
    /// Lanes are spread this far north and south of the start, in metres.
    pub lane_spread_m: f32,
    /// Bots walk this far east and west of the start line, in metres.
    pub swing_m: f32,
    /// Walking speed, in metres per second.
    pub speed_mps: f32,
    /// Seconds in the world per bot.
    pub duration_seconds: u64,
    /// Fewest crossings of the start line per bot.
    pub min_crossings: u32,
    /// Longest gap between two own bundles, in milliseconds.
    pub max_bundle_gap_ms: u64,
    /// Each bot changes its block this often, in milliseconds, and the run measures how fast the
    /// others see it; absent: no block changes.
    #[serde(default)]
    pub block_toggle_ms: Option<u64>,
    /// The longest p99 action-to-visible delay, in milliseconds.
    pub max_sync_latency_ms: u64,
    /// The metrics URL of every sim.
    pub metrics_urls: Vec<String>,
    /// The pinned client build file.
    pub client_build_config: PathBuf,
    /// Folder of the world files; the bots walk on the ground of the level.
    pub content_dir: PathBuf,
    /// The open map level.
    pub level: String,
}

/// A `[swarm]` or `[hunt]` value that is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("config value `{key}` is not valid: {reason}")]
pub struct ScenarioConfigError {
    /// Key.
    pub key: &'static str,
    /// Why.
    pub reason: &'static str,
}

impl TestClientConfig {
    /// The walk of `pair`.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError`] when the points or the speed cannot be walked.
    pub fn route(&self) -> Result<Route, RouteError> {
        Route::new(self.route_m.clone(), self.route_speed_mps)
    }

    /// The swarm settings with the bot password `password`.
    ///
    /// # Errors
    ///
    /// Returns [`ScenarioConfigError`] for a `[swarm]` value that is not allowed.
    pub fn swarm_settings(&self, password: String) -> Result<SwarmSettings, ScenarioConfigError> {
        let swarm = &self.swarm;
        let fleet = self.fleet_settings(
            Fleet {
                bots: swarm.bots,
                account_prefix: &swarm.account_prefix,
                character_prefix: &swarm.character_prefix,
                bots_per_second: swarm.bots_per_second,
                duration_seconds: swarm.duration_seconds,
                metrics_urls: &swarm.metrics_urls,
            },
            password,
        )?;
        if swarm.min_crossings == 0 {
            return invalid("min_crossings", "must be at least 1");
        }
        valid_starts(&swarm.starts_m)?;
        if !(positive(swarm.swing_m) && positive(swarm.speed_mps)) {
            return invalid("swing_m", "swing and speed must be finite numbers above 0");
        }
        if !(swarm.lane_spread_m.is_finite() && swarm.lane_spread_m >= 0.0) {
            return invalid("lane_spread_m", "must be a finite number of at least 0");
        }
        Ok(SwarmSettings {
            fleet,
            lane_spread_m: swarm.lane_spread_m,
            swing_m: swarm.swing_m,
            speed_mps: swarm.speed_mps,
            min_crossings: swarm.min_crossings,
            max_bundle_gap: Duration::from_millis(swarm.max_bundle_gap_ms),
            path_tolerance_m: self.path_tolerance_m,
            starts: swarm.starts_m.clone(),
            block_toggle: swarm.block_toggle_ms.map(Duration::from_millis),
            max_sync_latency: Duration::from_millis(swarm.max_sync_latency_ms),
        })
    }

    /// The hunt settings with the bot password `password`. The sockets, the walking speed and
    /// the ground come from `[swarm]`.
    ///
    /// # Errors
    ///
    /// Returns [`ScenarioConfigError`] for a `[hunt]` or `[swarm]` value that is not allowed.
    pub fn hunt_settings(&self, password: String) -> Result<HuntSettings, ScenarioConfigError> {
        let hunt = &self.hunt;
        if hunt.account_prefix == self.swarm.account_prefix {
            return invalid(
                "account_prefix",
                "the hunt needs its own accounts: swarm runs move the swarm's bots",
            );
        }
        let fleet = self.fleet_settings(
            Fleet {
                bots: hunt.bots,
                account_prefix: &hunt.account_prefix,
                character_prefix: &hunt.character_prefix,
                bots_per_second: hunt.bots_per_second,
                duration_seconds: hunt.duration_seconds,
                metrics_urls: &hunt.metrics_urls,
            },
            password,
        )?;
        valid_starts(&hunt.starts_m)?;
        if hunt.bots > hunt.starts_m.len() * MAX_BOTS_PER_START {
            return invalid("bots", "at most 30 bots per start: more see only bots");
        }
        if !(positive(hunt.reach_m) && positive(self.swarm.speed_mps)) {
            return invalid("reach_m", "reach and speed must be finite numbers above 0");
        }
        if hunt.attack_interval_ms == 0 || hunt.respawn_wait_ms == 0 {
            return invalid(
                "attack_interval_ms",
                "the attack and respawn waits must be above 0",
            );
        }
        Ok(HuntSettings {
            fleet,
            starts: hunt.starts_m.clone(),
            speed_mps: self.swarm.speed_mps,
            reach_m: hunt.reach_m,
            attack_interval: Duration::from_millis(hunt.attack_interval_ms),
            respawn_wait: Duration::from_millis(hunt.respawn_wait_ms),
            max_bundle_gap: Duration::from_millis(hunt.max_bundle_gap_ms),
        })
    }

    /// The bots of a scenario, with the sockets of `[swarm]`.
    fn fleet_settings(
        &self,
        fleet: Fleet<'_>,
        password: String,
    ) -> Result<FleetSettings, ScenarioConfigError> {
        let swarm = &self.swarm;
        if !(1..=MAX_BOTS).contains(&fleet.bots) {
            return invalid("bots", "must be 1 to 17576");
        }
        let name_length = fleet.character_prefix.len() + NAME_SUFFIX_LETTERS as usize;
        let letters = fleet
            .character_prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic());
        if !letters || name_length > CHARACTER_NAME_MAX_LEN {
            return invalid(
                "character_prefix",
                "must be letters only, at most 13 of them",
            );
        }
        if swarm.udp_buffer_bytes == 0 {
            return invalid("udp_buffer_bytes", "must be above 0");
        }
        if fleet.bots_per_second == 0 {
            return invalid("bots_per_second", "must be above 0");
        }
        Ok(FleetSettings {
            bots: fleet.bots,
            account_prefix: fleet.account_prefix.to_owned(),
            character_prefix: fleet.character_prefix.to_owned(),
            password,
            login_url: self.login_url.clone(),
            game_url: self.game_url.clone(),
            bots_per_second: fleet.bots_per_second,
            // 0 is "until stopped".
            duration: (fleet.duration_seconds != 0)
                .then(|| Duration::from_secs(fleet.duration_seconds)),
            registration_timeout: Duration::from_secs(self.registration_timeout_seconds),
            metrics_urls: fleet.metrics_urls.to_vec(),
            udp_buffer_bytes: swarm.udp_buffer_bytes,
        })
    }
}

/// The values of a scenario section that make its fleet.
struct Fleet<'a> {
    bots: usize,
    account_prefix: &'a str,
    character_prefix: &'a str,
    bots_per_second: u32,
    duration_seconds: u64,
    metrics_urls: &'a [String],
}

fn invalid<T>(key: &'static str, reason: &'static str) -> Result<T, ScenarioConfigError> {
    Err(ScenarioConfigError { key, reason })
}

fn positive(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

/// At least one start, every one finite.
fn valid_starts(starts: &[[f32; 2]]) -> Result<(), ScenarioConfigError> {
    let finite = |start: &[f32; 2]| start.iter().all(|value| value.is_finite());
    if starts.is_empty() || !starts.iter().all(finite) {
        return invalid("starts_m", "must hold at least one finite position");
    }
    Ok(())
}

/// Reads the configuration.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file is missing or has a wrong key or type.
pub fn load_testclient_config(path: &Path) -> Result<TestClientConfig, ConfigError> {
    load_config(path, TESTCLIENT_ENV_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMITTED_CONFIG: &str = "../../config/nw-testclient.toml";
    const HOLD_SECONDS: u64 = 60;
    /// M4 spec section 1.2 criterion 3.5.
    const SPEC_TOLERANCE_M: f32 = 0.1;
    /// M4 spec section 1.2 criterion 3.3 (`path_tolerance_m`, default 1.0).
    const SPEC_PATH_TOLERANCE_M: f32 = 1.0;
    /// M5 spec section 1.2 criterion 2.2.
    const M5_BOTS: usize = 1000;
    const M5_BUNDLE_GAP_MS: u64 = 250;
    /// One start (M6-B spec section 14, amendment 5).
    const M6B_BOTS: usize = 30;
    /// M6-B spec section 9.2 (F7).
    const M6B_RESPAWN_WAIT_MS: u64 = 2500;

    #[test]
    fn committed_example_config_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        let config = load_testclient_config(&path).expect("config");
        assert_eq!(config.hold_seconds, HOLD_SECONDS);
        assert_ne!(config.second_account, config.account);
        assert!((config.position_tolerance_m - SPEC_TOLERANCE_M).abs() < f32::EPSILON);
        assert!((config.path_tolerance_m - SPEC_PATH_TOLERANCE_M).abs() < f32::EPSILON);
        let route = config.route().expect("route");
        assert!(route.length() > 0.0);
        assert!(config.jump_m > 0.0);
        let swarm = config.swarm_settings(String::new()).expect("swarm");
        assert_eq!(swarm.fleet.bots, M5_BOTS);
        let hunt = config.hunt_settings(String::new()).expect("hunt");
        assert_eq!(hunt.fleet.bots, M6B_BOTS);
        assert_eq!(
            hunt.respawn_wait,
            Duration::from_millis(M6B_RESPAWN_WAIT_MS)
        );
        assert_eq!(
            swarm.max_bundle_gap,
            Duration::from_millis(M5_BUNDLE_GAP_MS)
        );
    }

    #[test]
    fn bad_swarm_values_are_refused() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        let config = load_testclient_config(&path).expect("config");
        let refused = |change: fn(&mut SwarmConfig)| {
            let mut bad = config.clone();
            change(&mut bad.swarm);
            bad.swarm_settings(String::new()).is_err()
        };
        assert!(refused(|swarm| swarm.bots = 0));
        assert!(refused(|swarm| swarm.bots = MAX_BOTS + 1));
        assert!(refused(|swarm| swarm.character_prefix = "Bot1".to_owned()));
        assert!(refused(|swarm| swarm.swing_m = 0.0));
        assert!(refused(|swarm| swarm.speed_mps = f32::NAN));
        assert!(refused(|swarm| swarm.lane_spread_m = -1.0));
        assert!(refused(|swarm| swarm.bots_per_second = 0));
        assert!(refused(|swarm| swarm.min_crossings = 0));
        assert!(refused(
            |swarm| swarm.character_prefix = "Abcdefghijklmn".to_owned()
        ));
        assert!(refused(|swarm| swarm.starts_m = vec![[f32::NAN, 0.0]]));
        assert!(refused(|swarm| swarm.starts_m.clear()));
        assert!(refused(|swarm| swarm.udp_buffer_bytes = 0));
    }

    #[test]
    fn a_zero_duration_means_until_stopped() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        let mut config = load_testclient_config(&path).expect("config");
        config.swarm.duration_seconds = 0;
        config.hunt.duration_seconds = 0;
        let swarm = config.swarm_settings(String::new()).expect("swarm");
        assert_eq!(swarm.fleet.duration, None);
        let hunt = config.hunt_settings(String::new()).expect("hunt");
        assert_eq!(hunt.fleet.duration, None);
    }

    #[test]
    fn swarm_settings_keep_the_other_checks() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        let mut config = load_testclient_config(&path).expect("config");
        config.swarm.duration_seconds = 0;
        config.swarm.bots_per_second = 0;
        assert!(config.swarm_settings(String::new()).is_err());
    }

    #[test]
    fn bad_hunt_values_are_refused() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        let config = load_testclient_config(&path).expect("config");
        let refused = |change: fn(&mut HuntConfig)| {
            let mut bad = config.clone();
            change(&mut bad.hunt);
            bad.hunt_settings(String::new()).is_err()
        };
        assert!(refused(|hunt| hunt.bots = 0));
        assert!(refused(|hunt| hunt.bots_per_second = 0));
        assert!(refused(|hunt| hunt.starts_m.clear()));
        assert!(refused(|hunt| hunt.reach_m = 0.0));
        assert!(refused(|hunt| hunt.attack_interval_ms = 0));
        assert!(refused(|hunt| hunt.respawn_wait_ms = 0));
        assert!(refused(|hunt| hunt.character_prefix = "Hunt1".to_owned()));
        assert!(refused(|hunt| hunt.account_prefix = "bot".to_owned()));
        assert!(
            refused(|hunt| hunt.bots = MAX_BOTS_PER_START + 1),
            "a crowd at one start sees only bots"
        );
    }
}
