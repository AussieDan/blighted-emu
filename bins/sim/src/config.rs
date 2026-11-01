// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/sim.toml` with `NW_SIM_` overrides (M4 spec sections 6.3 and 7, M5 spec section 7.1,
//! M6-A spec section 9).

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use nw_bus::BusId;
use nw_common::config::{ConfigError, SECRET_ENV_PREFIX, load_config};
use nw_db::DatabaseConfig;
use std::sync::Arc;

use nw_common::time::CivilDate;
use nw_content::combat::{CombatData, CombatRows};
use nw_content::instances::InstanceCatalog;
use nw_content::leaderboards::RotationCalendar;
use nw_content::loot_limits::{HOURS_PER_DAY, ResetClock};
use nw_messages::LevelSpec;
use nw_sim_core::{CellBounds, MoveLimits, MoveLimitsError, TICKS_PER_SECOND};
use serde::Deserialize;
use uuid::Uuid;

use crate::combat::CombatSettings;
use crate::mob_ai::{AiRates, MobAi};
use crate::mobs::VitalsSpawn;
use crate::trading_post::TradingPostConfig;
use crate::world::WorldSettings;

/// Most players and ghosts one player may see (our choice). Retail's 45-vs-45 war showed one
/// client 180 other players (replication note); the per-tick budget (24 remote groups, about
/// 1 KB) keeps the bytes of any number bounded, and each seen player costs a slot and a card
/// refresh every 30 bundles: at 500, about 17 cards per tick.
pub const MAX_VISIBLE_PLAYERS: usize = 500;

/// Environment prefix of `sim` overrides.
pub const SIM_ENV_PREFIX: &str = "NW_SIM_";
/// `tick_threads` value for every core of the PC.
pub const ALL_CORES: usize = 0;
/// One tick, rounded up to whole milliseconds: the shortest ghost lifetime.
pub const TICK_MS: u64 = 1000_u64.div_ceil(TICKS_PER_SECOND);

/// Default share of a player's damage that mob armor takes off (nwdb, M6-A spec F7).
pub const DEFAULT_ARMOR_SHARE: f64 = 0.35;
/// Default share of a mob's damage that a player's armor takes off: retail 3601 hits on a new
/// character in the tutorial's starter armor keep 0.6006 of the damage (live capture
/// 2026-09-12, all 2 ship hits and the first island hits).
pub const DEFAULT_PLAYER_ARMOR_SHARE: f64 = 0.3994;
/// Default quiet time before an awake mob without a target sleeps (our engineering choice: a
/// sleeping mob costs nothing).
pub const DEFAULT_SLEEP_AFTER_MS: u64 = 30_000;
/// Default melee reach of a player's attack request without a swing.
pub const DEFAULT_ATTACK_REACH_M: f32 = 3.0;
/// Default reach margin for the lag between client and server.
pub const DEFAULT_REACH_MARGIN_M: f32 = 1.0;
/// Default shortest time between two attacks of a player: the retail sword's hit spacing, the
/// fastest of the tutorial weapons (swing note: sword 400, axe 700, hammer 830 ms).
pub const DEFAULT_MIN_ATTACK_INTERVAL_MS: u64 = 400;
/// Default mob respawn: the retail time (Inferred from the one clean retail respawn, mobs note
/// 2026-10-31: `corrupted_sailor_starterbeach_named_00` came back 62.0 s after its death with the
/// player within 30 m all the time; its spawner slice says 360 s, and the rule that shortens it
/// is Unknown).
pub const DEFAULT_MOB_RESPAWN_MS: u64 = 62_000;
/// Default shortest time from a death to a respawn request: below the lowest captured gap
/// (M6-B spec F7).
pub const DEFAULT_MIN_RESPAWN_MS: u64 = 2_000;
/// Default seed of the combat dice.
pub const DEFAULT_COMBAT_SEED: u64 = 1;
/// Default `u32` of a spawn card's `AssetId` (always 2 per the Azoth catalog field note).
pub const DEFAULT_ASSET_SUB_ID: u32 = 2;
/// Default loot drop multiplier: the game data's drop chance, as retail.
pub const DEFAULT_LOOT_DROP_MULTIPLIER: f64 = 1.0;

/// Section `[combat]` (M6-A spec section 9).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CombatConfig {
    /// The player row of `VitalsBaseData`.
    pub player_vitals_id: String,
    /// The player weapon row of `WeaponItemDefinitions`.
    pub weapon_id: String,
    /// The unique id of the player's damage table, where its attack rows are.
    pub damage_table: String,
    /// Share of a player's damage that mob armor takes off, 0 to below 1.
    pub armor_share: f64,
    /// Share of a mob's damage that player armor takes off, 0 to below 1.
    pub player_armor_share: f64,
    /// A player this near wakes a mob; absent means `interest_radius_m`.
    pub wake_radius_m: Option<f32>,
    /// Quiet time before an awake mob without a target sleeps.
    pub sleep_after_ms: u64,
    /// Melee reach of a player's attack request without a swing.
    pub attack_reach_m: f32,
    /// Extra reach allowed for lag.
    pub reach_margin_m: f32,
    /// Shortest time between two attacks of a player.
    pub min_attack_interval_ms: u64,
    /// Time from a mob's death to its respawn.
    pub mob_respawn_ms: u64,
    /// Shortest time from a player's death to a respawn on its request.
    pub min_respawn_ms: u64,
    /// Seed of the combat dice.
    pub seed: u64,
    /// The `u32` of a mob's spawn card `AssetId`.
    pub asset_sub_id: u32,
    /// Times a mob type's `LootDropChance` (at most a sure drop): 1.0 is the game data's chance,
    /// 2.0 twice as many kills roll loot, 0.5 half as many.
    pub loot_drop_multiplier: f64,
}

impl Default for CombatConfig {
    fn default() -> Self {
        let rows = CombatRows::default();
        Self {
            player_vitals_id: rows.player_vitals_id,
            weapon_id: rows.weapon_id,
            damage_table: rows.damage_table,
            armor_share: DEFAULT_ARMOR_SHARE,
            player_armor_share: DEFAULT_PLAYER_ARMOR_SHARE,
            wake_radius_m: None,
            sleep_after_ms: DEFAULT_SLEEP_AFTER_MS,
            attack_reach_m: DEFAULT_ATTACK_REACH_M,
            reach_margin_m: DEFAULT_REACH_MARGIN_M,
            min_attack_interval_ms: DEFAULT_MIN_ATTACK_INTERVAL_MS,
            mob_respawn_ms: DEFAULT_MOB_RESPAWN_MS,
            min_respawn_ms: DEFAULT_MIN_RESPAWN_MS,
            seed: DEFAULT_COMBAT_SEED,
            asset_sub_id: DEFAULT_ASSET_SUB_ID,
            loot_drop_multiplier: DEFAULT_LOOT_DROP_MULTIPLIER,
        }
    }
}

impl CombatConfig {
    /// The datasheet rows of this config.
    pub fn rows(&self) -> CombatRows {
        CombatRows {
            player_vitals_id: self.player_vitals_id.clone(),
            weapon_id: self.weapon_id.clone(),
            damage_table: self.damage_table.clone(),
        }
    }

    fn validate(&self, interest_radius_m: f32) -> Result<(), SimConfigError> {
        let invalid = |key, reason| Err(SimConfigError::Invalid { key, reason });
        let positive = |value: f32| value.is_finite() && value > 0.0;
        if !positive(self.attack_reach_m) {
            return invalid("combat.attack_reach_m", "must be a finite number above 0");
        }
        if !(self.reach_margin_m.is_finite() && self.reach_margin_m >= 0.0) {
            return invalid(
                "combat.reach_margin_m",
                "must be a finite number of at least 0",
            );
        }
        let times = [
            self.sleep_after_ms,
            self.min_attack_interval_ms,
            self.mob_respawn_ms,
            self.min_respawn_ms,
        ];
        if times.contains(&0) {
            return invalid("combat.times", "must be above 0");
        }
        let share = |value: f64| (0.0..1.0).contains(&value);
        if !share(self.armor_share) {
            return invalid("combat.armor_share", "must be at least 0 and below 1");
        }
        if !share(self.player_armor_share) {
            return invalid(
                "combat.player_armor_share",
                "must be at least 0 and below 1",
            );
        }
        if !positive(self.wake_radius_m.unwrap_or(interest_radius_m)) {
            return invalid("combat.wake_radius_m", "must be a finite number above 0");
        }
        Ok(())
    }
}

/// The whole `sim` configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimConfig {
    /// This sim's id in bus subjects; unique in the world.
    pub sim_id: BusId,
    /// The world, the same `[[world]] id` as in `config/edge.toml`.
    pub world_id: Uuid,
    /// NATS, internal only.
    pub nats_url: String,
    /// The world database.
    pub world_database: DatabaseConfig,
    /// The environment variable with the world database password; it starts with `NW_SECRET_`.
    pub world_database_password_env: String,
    /// The global database: the sim adds the leaderboard stats there.
    pub global_database: DatabaseConfig,
    /// The environment variable with the global database password; it starts with `NW_SECRET_`.
    pub global_database_password_env: String,
    /// The leaderboard rotations.
    pub leaderboards: LeaderboardSection,
    /// The pinned client build file.
    pub client_build_config: PathBuf,
    /// The `edge-http` config whose `[remote_config]` sets the sim reads its gameplay values
    /// from: the same layers the client gets (`crate::gameplay`).
    pub edge_config: PathBuf,
    /// The server's daily and weekly resets of the loot limits.
    pub loot_limits: ResetClock,
    /// Folder of the world files (`world-<build>-<level>.bin`).
    pub content_dir: PathBuf,
    /// The open map level; the same as the director's.
    pub level: String,
    /// Most players in this sim.
    pub max_players: u32,
    /// Players see each other inside this radius, in metres.
    pub interest_radius_m: f32,
    /// Most other players and ghosts a player sees: those fighting it, then the nearest. Not a
    /// bundle limit: bundles split by bytes, and the per-tick budget picks who goes in each tick.
    pub max_visible_players: usize,
    /// How far a position may be below the ground, in metres.
    pub ground_tolerance_m: f32,
    /// How far a position may be above the ground, in metres.
    pub max_height_above_ground_m: f32,
    /// The run speed of the datasheets (`ServerValidatedCanRun`), in metres per second.
    pub run_speed_mps: f32,
    /// How much faster than `run_speed_mps` a move may be, as a share (1.2 is 120 % more).
    pub speed_margin: f32,
    /// Longest burst of movement that the distance budget holds, in seconds.
    pub burst_seconds: f32,
    /// How far into another sim's cells a player goes before its handoff starts, in metres.
    pub handoff_margin_m: f32,
    /// Longest wait for the answer to a handoff.
    pub handoff_timeout_ms: u64,
    /// Wait after a refused or late handoff, and after a take, before the next handoff.
    pub handoff_retry_ms: u64,
    /// How long inputs of a handed-off connection go on to the new sim.
    pub forward_ttl_ms: u64,
    /// Ghosts of another sim not renewed for this long are dropped.
    pub ghost_ttl_ms: u64,
    /// Threads of the bundle step; 0 means every core of the PC.
    pub tick_threads: usize,
    /// Time between position saves.
    pub save_interval_ms: u64,
    /// Time between heartbeats.
    pub heartbeat_interval_ms: u64,
    /// Missed gateway heartbeats before its players leave.
    pub heartbeat_misses: u32,
    /// Internal Prometheus listener.
    pub metrics_listen: SocketAddr,
    /// `tracing` filter, for example `info`.
    pub log_filter: String,
    /// Mobs and combat.
    #[serde(default)]
    pub combat: CombatConfig,
    /// How often the mobs' trees run.
    #[serde(default)]
    pub mob_ai: AiRates,
    /// The admin tool.
    #[serde(default)]
    pub admin: AdminSection,
    /// The trading post's durations, limits, fees and taxes.
    #[serde(default)]
    pub trading_post: TradingPostConfig,
    /// What this sim runs: cells of the open map, or one instance at a time.
    #[serde(default)]
    pub role: SimRole,
    /// Longest wait for the director's answer to an expedition request (it asks the social
    /// service and opens the instance first).
    #[serde(default = "default_instance_request_timeout_ms")]
    pub instance_request_timeout_ms: u64,
    /// Test worlds only: the tools a new character wears at its first spawn (retail gives a new
    /// character no tool). Empty in every world players use.
    #[serde(default)]
    pub start_tools: Vec<StartTool>,
}

/// A tool a new character of a test world wears ([`SimConfig::start_tools`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartTool {
    /// The item id (`MasterItemDefinitions` `ItemID`): `LoggingAxeT1`.
    pub item: String,
    /// Its paperdoll tool slot: 33 logging, 34 harvesting, 35 skinning, 36 mining, 37 fishing.
    pub slot: u32,
}

/// The paperdoll tool slots (`CHOPPING_SLOT` 33 to `FISHING_SLOT` 37).
const TOOL_SLOTS: std::ops::RangeInclusive<u32> = 33..=37;

/// The role of a sim (architecture spec section 4.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SimRole {
    /// Cells of the open map, or the tutorial.
    #[default]
    OpenMap,
    /// One expedition, raid or trial at a time, on the level of its game mode (its `level` is
    /// the lower-case map name, `nw_dungeon_windsward_00`).
    Instance,
}

/// The wait for an instance answer when the file names none: the director's admit timeout of
/// `config/world-director.toml`, above its group and opening waits.
const DEFAULT_INSTANCE_REQUEST_TIMEOUT_MS: u64 = 3000;

const fn default_instance_request_timeout_ms() -> u64 {
    DEFAULT_INSTANCE_REQUEST_TIMEOUT_MS
}

/// `[admin]` (admin tools spec section 5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    /// Time between two admin snapshots on the bus; 0 is off, and then the sim does no snapshot
    /// work.
    pub snapshot_interval_ms: u64,
}

/// `[leaderboards]`: the rotation keys the config layers give the client, so the sim names the
/// same boards (study note `docs/re/2026-09-18-leaderboards-and-analytics.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaderboardSection {
    /// `javelin.leaderboard-rotation-startDate`, `year-month-day`.
    pub rotation_start_date: String,
    /// `javelin.weekly-leaderboard-reset-hour`, UTC.
    pub weekly_reset_hour: u32,
    /// `javelin.leaderboard-season-rotation`.
    pub season: u32,
}

/// Shortest snapshot interval that is not off.
pub const MIN_SNAPSHOT_INTERVAL_MS: u64 = 100;

/// A value of the configuration that is not allowed.
#[derive(Debug, thiserror::Error)]
pub enum SimConfigError {
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
/// Returns [`SimConfigError`] when the file is missing, has a wrong key, or a value is not
/// allowed.
pub fn load_sim_config(path: &Path) -> Result<SimConfig, SimConfigError> {
    let config: SimConfig = load_config(path, SIM_ENV_PREFIX)?;
    config.validate()?;
    Ok(config)
}

impl SimConfig {
    fn validate(&self) -> Result<(), SimConfigError> {
        let invalid = |key, reason| Err(SimConfigError::Invalid { key, reason });
        if !self
            .world_database_password_env
            .starts_with(SECRET_ENV_PREFIX)
        {
            return invalid("world_database_password_env", "must start with NW_SECRET_");
        }
        if !self
            .global_database_password_env
            .starts_with(SECRET_ENV_PREFIX)
        {
            return invalid("global_database_password_env", "must start with NW_SECRET_");
        }
        self.leaderboard_calendar()?;
        if self.loot_limits.hour_utc >= HOURS_PER_DAY {
            return invalid("loot_limits.hour_utc", "must be below 24");
        }
        if self.level.is_empty() {
            return invalid("level", "must not be empty");
        }
        // Ghosts go in as many bus messages as they need (`nw_bus::ghost_parts`).
        if self.max_players == 0 {
            return invalid("max_players", "must be above 0");
        }
        if !(self.handoff_margin_m.is_finite() && self.handoff_margin_m >= 0.0) {
            return invalid("handoff_margin_m", "must be a finite number of at least 0");
        }
        let times = [
            self.handoff_timeout_ms,
            self.handoff_retry_ms,
            self.forward_ttl_ms,
            self.ghost_ttl_ms,
        ];
        if times.contains(&0) {
            return invalid("handoff", "times must be above 0");
        }
        if !(1..=MAX_VISIBLE_PLAYERS).contains(&self.max_visible_players) {
            return invalid(
                "max_visible_players",
                "must be at least 1 and at most 500 (the slot and card-refresh budget)",
            );
        }
        let snapshot = self.admin.snapshot_interval_ms;
        if snapshot != 0 && snapshot < MIN_SNAPSHOT_INTERVAL_MS {
            return invalid(
                "admin.snapshot_interval_ms",
                "must be 0 (off) or at least 100",
            );
        }
        if self.ghost_ttl_ms < TICK_MS {
            return invalid("ghost_ttl_ms", "must be at least one tick (34 ms)");
        }
        if self.forward_ttl_ms <= self.handoff_timeout_ms {
            return invalid("forward_ttl_ms", "must be longer than handoff_timeout_ms");
        }
        self.combat.validate(self.interest_radius_m)?;
        self.trading_post.validate()?;
        if !self.mob_ai.is_valid() {
            return Err(SimConfigError::Invalid {
                key: "mob_ai",
                reason: "intervals must be above 0 and the bands must rise",
            });
        }
        if !(self.interest_radius_m.is_finite() && self.interest_radius_m > 0.0) {
            return invalid("interest_radius_m", "must be a finite number above 0");
        }
        let band = [self.ground_tolerance_m, self.max_height_above_ground_m];
        if band
            .iter()
            .any(|value| !(value.is_finite() && *value >= 0.0))
        {
            return invalid("ground", "tolerances must be finite numbers of at least 0");
        }
        if self.save_interval_ms == 0 || self.heartbeat_interval_ms == 0 {
            return invalid("intervals", "must be above 0");
        }
        if self.heartbeat_misses == 0 {
            return invalid("heartbeat_misses", "must be above 0");
        }
        self.move_limits()
            .map_err(|error| SimConfigError::Invalid {
                key: error.name,
                reason: error.rule,
            })?;
        Ok(())
    }

    /// The leaderboard rotation calendar of `[leaderboards]`.
    ///
    /// # Errors
    ///
    /// Returns [`SimConfigError::Invalid`] for a start date the calendar does not have or a
    /// reset hour past the day.
    pub fn leaderboard_calendar(&self) -> Result<RotationCalendar, SimConfigError> {
        let section = &self.leaderboards;
        let start =
            CivilDate::parse(&section.rotation_start_date).ok_or(SimConfigError::Invalid {
                key: "leaderboards.rotation_start_date",
                reason: "must be a year-month-day date",
            })?;
        if section.weekly_reset_hour >= HOURS_PER_DAY {
            return Err(SimConfigError::Invalid {
                key: "leaderboards.weekly_reset_hour",
                reason: "must be below 24",
            });
        }
        Ok(RotationCalendar {
            start,
            reset_hour: section.weekly_reset_hour,
            season: section.season,
        })
    }

    /// The settings of the world state on a level with the edges `bounds` and its sea at
    /// `ocean_level_m`.
    ///
    /// # Errors
    ///
    /// Returns [`SimConfigError::Invalid`] for a movement limit that is not usable.
    pub fn world_settings(
        &self,
        bounds: CellBounds,
        ocean_level_m: f32,
        instances: &InstanceCatalog,
    ) -> Result<WorldSettings, SimConfigError> {
        let limits = self
            .move_limits()
            .map_err(|error| SimConfigError::Invalid {
                key: error.name,
                reason: error.rule,
            })?;
        Ok(WorldSettings {
            interest_radius_m: self.interest_radius_m,
            max_visible_players: self.max_visible_players,
            max_players: self.max_players,
            limits,
            bounds,
            handoff_margin_m: self.handoff_margin_m,
            handoff_retry_ms: self.handoff_retry_ms,
            ghost_ttl_ms: self.ghost_ttl_ms,
            tick_threads: self.tick_threads(),
            local_player_card: crate::world::local_player_card(),
            level: self.level_spec(instances)?,
            ocean_level_m,
            instance_role: self.role == SimRole::Instance,
            instance_request_timeout_ms: self.instance_request_timeout_ms,
        })
    }

    /// The start tools, each in a tool slot.
    ///
    /// # Errors
    ///
    /// Returns [`SimConfigError::Invalid`] for a tool in a slot that is no tool slot.
    pub fn start_tools(&self) -> Result<Vec<StartTool>, SimConfigError> {
        match self
            .start_tools
            .iter()
            .find(|tool| !TOOL_SLOTS.contains(&tool.slot))
        {
            Some(_) => Err(SimConfigError::Invalid {
                key: "start_tools",
                reason: "every slot must be a tool slot, 33 to 37",
            }),
            None => Ok(self.start_tools.clone()),
        }
    }

    /// The level this sim names to the client: the open map or the tutorial, or for an instance
    /// sim the level of a game mode of `instances` (each instance names its own frame).
    ///
    /// # Errors
    ///
    /// Returns [`SimConfigError::Invalid`] for a level this server cannot name.
    pub fn level_spec(&self, instances: &InstanceCatalog) -> Result<LevelSpec, SimConfigError> {
        match self.role {
            SimRole::OpenMap => LevelSpec::of_folder(&self.level).ok_or(SimConfigError::Invalid {
                key: "level",
                reason: "not a level this server can name to the client",
            }),
            SimRole::Instance => instances
                .of_level(&self.level)
                .next()
                .map(|mode| {
                    LevelSpec::instance(
                        &mode.map.folder,
                        &mode.map.map_name,
                        &mode.map.coatlicue_path,
                        mode.map.frame,
                    )
                })
                .ok_or(SimConfigError::Invalid {
                    key: "level",
                    reason: "no expedition, raid or trial has this level",
                }),
        }
    }

    /// The combat settings on the rows of `data` with the vitals `spawns` of the level and the
    /// mob AI `ai`.
    pub fn combat_settings(
        &self,
        data: Arc<CombatData>,
        spawns: Arc<[VitalsSpawn]>,
        ai: Arc<MobAi>,
    ) -> CombatSettings {
        let combat = &self.combat;
        CombatSettings {
            data,
            spawns,
            ai,
            wake_radius_m: combat.wake_radius_m.unwrap_or(self.interest_radius_m),
            sleep_after_ms: combat.sleep_after_ms,
            attack_reach_m: combat.attack_reach_m,
            mob_respawn_ms: combat.mob_respawn_ms,
            armor_share: combat.armor_share,
            player_armor_share: combat.player_armor_share,
            reach_margin_m: combat.reach_margin_m,
            min_attack_interval_ms: combat.min_attack_interval_ms,
            min_respawn_ms: combat.min_respawn_ms,
            seed: combat.seed,
            asset_sub_id: combat.asset_sub_id,
            loot_drop_multiplier: combat.loot_drop_multiplier,
        }
    }

    /// Threads of the bundle step: `tick_threads`, or every core when it is 0.
    pub fn tick_threads(&self) -> usize {
        if self.tick_threads == ALL_CORES {
            std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
        } else {
            self.tick_threads
        }
    }

    /// The movement limits (M4 spec section 7.2 with sections 15 and 16).
    ///
    /// # Errors
    ///
    /// Returns [`MoveLimitsError`] for a value that is not usable.
    pub fn move_limits(&self) -> Result<MoveLimits, MoveLimitsError> {
        MoveLimits::new(
            self.run_speed_mps,
            self.speed_margin,
            self.burst_seconds,
            self.ground_tolerance_m,
            self.max_height_above_ground_m,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const COMMITTED_CONFIG: &str = "../../config/sim.toml";
    /// Sprint (5.5 m/s) with the 15 % road boost (movement numbers note, Check section).
    const BOOSTED_SPRINT_MPS: f32 = 6.325;
    /// The longest gap between two captured movement messages (movement numbers note, section 3).
    const LONGEST_HONEST_GAP_S: f32 = 2.5;
    /// The largest captured fall (movement numbers note, Check section).
    const LARGEST_CAPTURED_FALL_M: f32 = 128.6;
    /// The committed view: more players than retail's 45-vs-45 war showed one client (180).
    const COMMITTED_VISIBLE_PLAYERS: usize = 200;

    /// The committed example configuration.
    pub(crate) fn committed() -> SimConfig {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_CONFIG);
        load_sim_config(&path).expect("committed config")
    }

    #[test]
    fn start_tools_are_off_and_only_go_in_tool_slots() {
        let mut config = committed();
        assert!(
            config.start_tools().expect("tools").is_empty(),
            "no tools in a players' world"
        );
        config.start_tools = vec![StartTool {
            item: "LoggingAxeT1".to_owned(),
            slot: 33,
        }];
        assert_eq!(config.start_tools().expect("tools").len(), 1);
        config.start_tools[0].slot = 24;
        assert!(
            config.start_tools().is_err(),
            "a main-hand slot is no tool slot"
        );
    }

    #[test]
    fn the_committed_config_is_valid() {
        let config = committed();
        assert_eq!(config.level, "newworld_vitaeeterna");
        assert_eq!(config.max_visible_players, COMMITTED_VISIBLE_PLAYERS);
        assert!(config.max_height_above_ground_m > LARGEST_CAPTURED_FALL_M);
    }

    #[test]
    fn the_loot_limits_reset_at_a_valid_hour() {
        use nw_content::loot_limits::Weekday;

        let config = committed();
        assert_eq!(
            config.loot_limits,
            ResetClock {
                hour_utc: 9,
                weekly_day: Weekday::Tuesday,
            },
            "the captured resets of the US East server"
        );
        for (hour, ok) in [(0, true), (23, true), (HOURS_PER_DAY, false)] {
            let mut config = committed();
            config.loot_limits.hour_utc = hour;
            assert_eq!(config.validate().is_ok(), ok, "hour {hour}");
        }
    }

    #[test]
    fn the_committed_sim_runs_the_open_map_and_waits_for_instance_answers() {
        let config = committed();
        assert_eq!(config.role, SimRole::OpenMap);
        assert!(config.instance_request_timeout_ms > 0);
        let level = config
            .level_spec(&InstanceCatalog::default())
            .expect("open map");
        assert!(!level.private);
        let instance = SimConfig {
            role: SimRole::Instance,
            level: "nw_dungeon_windsward_00".to_owned(),
            ..config
        };
        assert!(
            instance.level_spec(&InstanceCatalog::default()).is_err(),
            "an instance level needs its game mode"
        );
    }

    #[test]
    fn the_committed_config_has_no_snapshot() {
        assert_eq!(committed().admin.snapshot_interval_ms, 0);
    }

    #[test]
    fn a_snapshot_interval_below_100_ms_is_refused() {
        for (interval, ok) in [(0, true), (1, false), (99, false), (100, true)] {
            let mut config = committed();
            config.admin.snapshot_interval_ms = interval;
            assert_eq!(config.validate().is_ok(), ok, "{interval} ms");
        }
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = committed();
        config.world_database_password_env = "WORLD_PASSWORD".to_owned();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.interest_radius_m = f32::NAN;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.heartbeat_misses = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.speed_margin = -1.0;
        assert!(matches!(
            config.validate(),
            Err(SimConfigError::Invalid {
                key: "speed_margin",
                ..
            })
        ));
        let mut config = committed();
        config.max_players = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.max_players = u32::MAX;
        assert!(config.validate().is_ok(), "ghosts of any count go in parts");
        let mut config = committed();
        config.handoff_margin_m = -1.0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.handoff_timeout_ms = 0;
        assert!(config.validate().is_err());
        let mut config = committed();
        config.max_visible_players = MAX_VISIBLE_PLAYERS + 1;
        assert!(config.validate().is_err(), "past the slot budget");
        let mut config = committed();
        config.max_visible_players = 0;
        assert!(config.validate().is_err(), "a player sees someone");
        let mut config = committed();
        config.ghost_ttl_ms = 1;
        assert!(config.validate().is_err(), "ghosts live at least one tick");
        let mut config = committed();
        config.forward_ttl_ms = config.handoff_timeout_ms;
        assert!(config.validate().is_err(), "forwards outlive a handoff");
        let mut config = committed();
        config.global_database_password_env = "GLOBAL_PASSWORD".to_owned();
        assert!(config.validate().is_err());
        let mut config = committed();
        config.leaderboards.rotation_start_date = "2023-02-30".to_owned();
        assert!(config.validate().is_err(), "no such day");
        let mut config = committed();
        config.leaderboards.weekly_reset_hour = 24;
        assert!(config.validate().is_err(), "past the day");
    }

    #[test]
    fn the_committed_rotations_are_the_retail_keys() {
        assert_eq!(
            committed().leaderboard_calendar().ok(),
            Some(RotationCalendar {
                start: CivilDate {
                    year: 2023,
                    month: 2,
                    day: 7
                },
                reset_hour: 2,
                season: 11
            })
        );
    }

    #[test]
    fn the_combat_section_has_its_defaults() {
        let config = committed();
        assert_eq!(config.combat, CombatConfig::default());
        assert_eq!(
            config.combat.rows(),
            nw_content::combat::CombatRows::default()
        );
        let ai = || Arc::new(MobAi::empty());
        let data = || Arc::new(crate::combat::test_support::test_data());
        let combat = config.combat_settings(data(), Arc::from([]), ai());
        assert_eq!(
            combat.wake_radius_m, config.interest_radius_m,
            "wake defaults to interest"
        );
        let mut set = committed();
        set.combat.wake_radius_m = Some(config.interest_radius_m * 2.0);
        assert_eq!(
            set.combat_settings(data(), Arc::from([]), ai())
                .wake_radius_m,
            config.interest_radius_m * 2.0
        );
        assert_eq!(
            config.mob_ai,
            crate::mob_ai::RETAIL_AI_RATES,
            "the retail rates"
        );
    }

    /// A config key and the fault that must make the check refuse it.
    type CombatFault = (&'static str, fn(&mut CombatConfig));

    #[test]
    fn bad_combat_values_are_refused() {
        let cases: [CombatFault; 5] = [
            ("combat.attack_reach_m", |c| c.attack_reach_m = 0.0),
            ("combat.times", |c| c.mob_respawn_ms = 0),
            ("combat.armor_share", |c| c.armor_share = 1.5),
            ("combat.player_armor_share", |c| c.player_armor_share = -0.1),
            ("combat.wake_radius_m", |c| c.wake_radius_m = Some(f32::NAN)),
        ];
        for (key, fault) in cases {
            let mut config = committed();
            fault(&mut config.combat);
            assert!(
                matches!(config.validate(), Err(SimConfigError::Invalid { key: found, .. }) if found == key),
                "{key}: {:?}",
                config.validate()
            );
        }
    }

    #[test]
    fn zero_tick_threads_means_every_core() {
        let mut config = committed();
        config.tick_threads = ALL_CORES;
        assert!(config.tick_threads() >= 1);
        config.tick_threads = 3;
        assert_eq!(config.tick_threads(), 3);
    }

    #[test]
    fn the_speed_limit_is_above_boosted_sprint() {
        let limits = committed().move_limits().expect("limits");
        assert!(limits.max_speed_mps > BOOSTED_SPRINT_MPS);
        assert!(committed().burst_seconds > LONGEST_HONEST_GAP_S);
    }
}
