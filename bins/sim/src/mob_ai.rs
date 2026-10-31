// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mob AI from the game's own data (behavior-tree study 2026-09-18, sections 3 and 5): each mob
//! of a creature slice whose tree Server.pak has runs that tree (`nw_ai::Brain`), with the slice's
//! config values, vision, navigation configs and CAGE grid from the AI data file
//! (`ai-data-<build>.bin`, `content-tool ai-data`). This module holds the shared data, the design
//! files' overrides of a spawn, and one mob's mind: what it perceives, what the engine keys of
//! its tree read, and how its tree's commands move and animate it.
//!
//! Marks: **Observed** (read in the data or the capture), **Inferred** (follows from them, not
//! seen run), **Unknown** (not found).
//!
//! What the engine keys are (the writers in the client are Unknown, study section 3.1):
//! - `Has_Target`, `Target_Pos`: the mob's target (see [`TreeMind::perceive`]).
//! - `Target_Dist`: horizontal metres (Inferred: the boar's 3 m and 7 m bands fit the retail
//!   attack starts at 1.69 to 5.30 m; `content-tool` `ai_replay`).
//! - `Target_Angle`: signed degrees from the facing to the target, positive on the mob's left
//!   (the trace rule; Inferred from the Turn sub-tree's `< -30 Or > 30`).
//! - `Target_Visible`: the target is within the combat vision distance (Inferred: the retail
//!   boar turned to a player at 77 to 107 degrees, so its 180 degree combat cone does not hide
//!   the target in a fight; trace finding a).
//! - `Target_Reachable`, `Target_Directly_Reachable`: the straight line to the target stands on
//!   floor within the slice's `m_maxSlope` (Inferred: the server has no navigation mesh, see
//!   [`floor_line`]).
//! - `Time_Since_Target_Reachable`: seconds since the target was last reachable (Inferred from
//!   the name and `TargetUnreachableResetTime` 5 s).
//! - `Target_Vitals_State`, `Vitals_State`: 4 while alive (Inferred: the trees name the test
//!   `$Target_Vitals_State Equal 4` "HasAliveTarget", 154 times; a dead player is no target).
//! - `Health_Percent`: 0 to 100 (Observed: the trees compare it with 5 to 100, and 33.33 and
//!   66.67 for thirds).
//! - `Spawn_Pos`, `Last_Damage_Pos`, `Full_Body_Reaction`, `Waypoint_Pos`, `Waypoint_Duration`.
//! - `Passive`: the slice's `Is_Passive` (Inferred: the boar's `IsPassive?` branch reads
//!   `Passive`, its slice sets `Is_Passive`).
//! - Every other engine key is not set: its value is Unknown, and the trees then read their
//!   fallback or the slice config (for example `Leash_Time_Limit`: the cvar values are Unknown,
//!   so the leash takes the branch without a time limit).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use nw_ai::brain::{Brain, Command, Destination, MotionOutcome, Runtime, Slot, World};
use nw_ai::data::{AiData, Archetype, CageAction, FacingMode, NavConfig, ProcWindow};
use nw_ai::keys::EngineKey;
use nw_ai::node::{
    EntityQueryParams, NodeId, NodeKind, PositionType, SelectPositionParams, TrackerParams,
};
use nw_ai::rng::Rng48;
use nw_ai::value::{Blackboard, KeyId, NameId, Param, Value};
use nw_content::container::ContainerError;
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_sim_core::character::STEP_UP_M;
use nw_sim_core::{Ground, Mover, Position, Seen, Steering, Threat, yaw_gap, yaw_toward};
use nw_world::WorldFile;
use nw_world::records::{AiBehaviour, AiProfile, PathMode, PatrolPath};
use serde::Deserialize;

use crate::mobs::MobReach;

/// A moving mob keeps its own height when the level's ground lies more than this far below it
/// (our engineering limit): it stands on a floor the height map does not hold, such as the
/// tutorial ship's deck high above the sea bed (live 2026-09-12: the sailor dropped under the
/// ship and vanished).
pub const MAX_STEP_DOWN_M: f32 = 3.0;
/// Longest time one AI step may move a mob, so a stalled tick never makes it jump (our
/// engineering limit).
const MAX_STEP_MS: u64 = 1_000;
/// The step of a floor check along a line (our engineering limit: half a body).
const FLOOR_STEP_M: f32 = 0.5;
/// A body this near the floor under it stands on that floor (our engineering limit: a floor
/// check's step).
const ON_FLOOR_M: f32 = 0.5;
/// The most a mob's feet rise in one floor check step (Observed: the highest rise of a retail
/// mob between two of its places was 0.58 m over the beach captures; ours climbed 1.36 m in
/// 0.21 m onto a solid, live 2026-10-29 18:29:39.797, mob ground height note).
const MAX_STEP_RISE_M: f32 = 0.6;
/// The turns, nearest first, a mob tries when its straight way meets a wall, a climb or an edge:
/// it walks around the solid (Inferred: retail mobs follow the level's navigation mesh around
/// rocks and crates; ours has none, live 2026-10-29: Janszoon stood blocked by a rock 2 m from
/// his spawn and never reached the player).
const DETOUR_TURNS_DEG: [f64; 6] = [30.0, -30.0, 60.0, -60.0, 90.0, -90.0];
/// The longest first leg of a way around a solid (our engineering limit: about a rock's size).
const DETOUR_LEG_M: f32 = 4.0;
/// The slice config param that gives the engine key `Difficulty_Tier` (Inferred: Janszoon's
/// slice, `corrupted_sailor_starterbeach_named_00`, sets `DifficultyTier` 1 and no
/// `Difficulty_Tier`; his great axeman trees attack only at a tier, and tier 1 picks the
/// `Damned_GreatAxeman_Medium_AttackTree`, which holds the two attacks he used in the retail
/// newchar run, `Attack_Close_2H_Axe_04` and `Attack_Far_2H_Axe_01`. Without it he only strafed,
/// live 2026-10-29).
const DIFFICULTY_TIER_PARAM: &str = "DifficultyTier";
/// The tree fact that turns on `DistanceLeash` (with `Leash_Opt_Out` 0). Neither the client's
/// engine key table nor a slice sets it; the server engine does (Inferred: every retail reset of
/// the beach and Steal the Boat captures plays `DistanceLeash`'s `Combat_Fidget` then
/// `Reset_Move`, the dynasty mobs 61 to 73 m from where they started, past their
/// `CombatLeashDistance`). A mob starts with it true unless its slice config sets it. Without it
/// no tree mob ever walked home (live 2026-10-29: Janszoon stood 51 m from his spawn, past his
/// 45 m leash).
const DOES_LEASH_KEY: &str = "DoesLeash";
/// The slice config param that gives the engine key `Leash_Opt_Out` (a dungeon mob's opt-out of
/// the leash); 0 without one.
const LEASH_OPT_OUT_PARAM: &str = "Leash_Opt_Out";
/// `Target_Vitals_State` and `Vitals_State` of a living entity (see the module notes).
const ALIVE_VITALS_STATE: i64 = 4;
/// `Health_Percent` is 0 to 100 (module notes).
const PERCENT: f32 = 100.0;
const MS_PER_S: f32 = 1_000.0;
const MS_PER_S_F64: f64 = 1_000.0;
const HALF: f32 = 0.5;
const FULL_TURN_DEG: f64 = 360.0;
/// `SelectPositionParams.Direction` (study section 1.4: `AnchorToOrigin` 1, `OriginOrientation`
/// 2 Observed; `OriginToAnchor` 0 Inferred).
const ANCHOR_TO_ORIGIN: i64 = 1;
const ORIGIN_ORIENTATION: i64 = 2;
/// The grid aliases of the actions the engine starts (Observed: `<AliasMap>` of the creature
/// grids, `Reaction` and `Death`).
const REACTION_ALIAS: &str = "Reaction";
const DEATH_ALIAS: &str = "Death";
/// The procedural clips of a fragment and their parameters (Observed in `npc_boar_anims.adb`).
const DAMAGE_CLIP: &str = "CAGE-Damage";
const HOMING_CLIP: &str = "Homing";
const DAMAGE_ROW_PARAM: &str = "DamageTableRow";
const SHAPE_RADIUS_PARAM: &str = "MeleeAttackShapeRadius";
const SHAPE_HALF_HEIGHT_PARAM: &str = "MeleeAttackCapsuleHalfHeight";
const HOMING_RADIUS_PARAM: &str = "Radius";
const HOMING_ANGLE_PARAM: &str = "MaxAngle";
/// A ranged attack clip and its parameters (Observed: Janszoon's `Officer_AnchorToss` in
/// `npc_damned_anims.adb`).
const RANGED_CLIP: &str = "CAGE-RangedAttack";
const RANGED_ATTACK_PARAM: &str = "RangedAttackName";
const SLICE_OVERRIDE_PARAM: &str = "SliceOverride";
const LEAD_TARGET_PARAM: &str = "AILeadTarget";
const LEAD_MAX_ANGLE_PARAM: &str = "AILeadTargetMaxAngle";
/// The value of a true clip parameter.
const PARAM_TRUE: &str = "true";
/// The navigation config of a run (Observed: the boar's `Run` nodes name `RunToTarget`; retail
/// boar runs reached 6.96 m/s, its maximum 7.0).
const RUN_CONFIG: &str = "RunToTarget";
/// The status effects of a leash reset (Observed: `ResetToHome` and `DistanceLeash` apply
/// `ResetAI` or `ResetAINoHeal` while the mob walks home).
const RESET_EFFECT: &str = "ResetAI";
const RESET_NO_HEAL_EFFECT: &str = "ResetAINoHeal";
/// Slice config keys the design overrides set (Observed names in the slices and the trees).
const IS_PASSIVE_KEY: &str = "Is_Passive";
const ENABLE_WANDER_KEY: &str = "Enable_Wander";
const WANDER_MAX_KEY: &str = "WanderDistanceMax";
const AMBIENT_LEASH_KEY: &str = "AmbientLeashDistance";
/// The combat leash (Observed: DistanceLeash reads it).
pub(crate) const COMBAT_LEASH_KEY: &str = "CombatLeashDistance";
/// The attack cooldown of the base trees (Observed: `AttackCooldown` in boar_base and the
/// legion and damned trees).
const ATTACK_COOLDOWN: &str = "AttackCooldown";

/// `[mob_ai]`: how often a mob's tree runs, by what it does and how near the nearest player is
/// (trace finding c, `local-data/scratch-work/btree/trace/findings.txt`: the retail ALC state
/// rate, Inferred equal to the AI tick rate; the cvars `javelin-server.ai-update-rate-*` hold the
/// retail values, which are Unknown).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiRates {
    /// A mob with a target, at every distance (retail 33 to 34 ms).
    pub combat_interval_ms: u64,
    /// A moving mob without a target: near, middle and far (retail 98, 218 and 333 ms).
    pub moving_interval_ms: [u64; 3],
    /// Where the middle and far bands of a moving mob start (retail 20 m and 40 m).
    pub moving_range_m: [f32; 2],
    /// A mob that stands without a target, at every distance (retail 333 ms).
    pub idle_interval_ms: u64,
}

/// The retail rates of trace finding c.
pub const RETAIL_AI_RATES: AiRates = AiRates {
    combat_interval_ms: 33,
    moving_interval_ms: [98, 218, 333],
    moving_range_m: [20.0, 40.0],
    idle_interval_ms: 333,
};

impl Default for AiRates {
    fn default() -> Self {
        RETAIL_AI_RATES
    }
}

/// What a mob does, for its tick rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// It has a target.
    Combat,
    /// It moves without a target.
    Moving,
    /// It stands without a target.
    Idle,
}

impl AiRates {
    /// Whether every interval is above 0 and the bands rise.
    pub fn is_valid(&self) -> bool {
        self.combat_interval_ms > 0
            && self.idle_interval_ms > 0
            && self.moving_interval_ms.iter().all(|ms| *ms > 0)
            && self.moving_range_m[0].is_finite()
            && self.moving_range_m[0] > 0.0
            && self.moving_range_m[1].is_finite()
            && self.moving_range_m[1] > self.moving_range_m[0]
    }

    /// The time to the next tick of a mob doing `activity` with the nearest player
    /// `nearest_m` away.
    pub fn interval_ms(&self, activity: Activity, nearest_m: f32) -> u64 {
        match activity {
            Activity::Combat => self.combat_interval_ms,
            Activity::Idle => self.idle_interval_ms,
            Activity::Moving => {
                let band = self
                    .moving_range_m
                    .iter()
                    .take_while(|start| nearest_m >= **start)
                    .count();
                self.moving_interval_ms[band]
            }
        }
    }
}

/// The names the sim looks for in the data, resolved once.
#[derive(Debug, Clone, Copy, Default)]
struct Names {
    reaction: Option<NameId>,
    death: Option<NameId>,
    damage_clip: Option<NameId>,
    homing_clip: Option<NameId>,
    damage_row: Option<NameId>,
    shape_radius: Option<NameId>,
    shape_half_height: Option<NameId>,
    homing_radius: Option<NameId>,
    homing_angle: Option<NameId>,
    ranged_clip: Option<NameId>,
    ranged_attack: Option<NameId>,
    slice_override: Option<NameId>,
    lead_target: Option<NameId>,
    lead_max_angle: Option<NameId>,
    run_config: Option<NameId>,
    reset: Option<NameId>,
    reset_no_heal: Option<NameId>,
    attack_cooldown: Option<NameId>,
    is_passive: Option<KeyId>,
    enable_wander: Option<KeyId>,
    wander_max: Option<KeyId>,
    ambient_leash: Option<KeyId>,
    combat_leash: Option<KeyId>,
}

/// The slice asset of each vitals id's most seen look (the mob prefab table's).
pub type LookAssets = HashMap<u32, [u8; ASSET_ID_LEN]>;

/// The shared AI of a sim: the data, its runtime, the tick rates, and the slice of each vitals
/// id's look.
#[derive(Debug)]
pub struct MobAi {
    runtime: Runtime,
    names: Names,
    rates: AiRates,
    looks: LookAssets,
}

impl MobAi {
    /// The AI of `data` with tick rates `rates`; a mob of a vitals id shows the slice `looks`
    /// gives, unless its spawn names its own.
    pub fn new(data: impl Into<Arc<AiData>>, rates: AiRates, looks: LookAssets) -> Self {
        let runtime = Runtime::new(data);
        let name = |text: &str| runtime.name_id(text);
        let key = |text: &str| runtime.data().key(text);
        let names = Names {
            reaction: name(REACTION_ALIAS),
            death: name(DEATH_ALIAS),
            damage_clip: name(DAMAGE_CLIP),
            homing_clip: name(HOMING_CLIP),
            damage_row: name(DAMAGE_ROW_PARAM),
            shape_radius: name(SHAPE_RADIUS_PARAM),
            shape_half_height: name(SHAPE_HALF_HEIGHT_PARAM),
            homing_radius: name(HOMING_RADIUS_PARAM),
            homing_angle: name(HOMING_ANGLE_PARAM),
            ranged_clip: name(RANGED_CLIP),
            ranged_attack: name(RANGED_ATTACK_PARAM),
            slice_override: name(SLICE_OVERRIDE_PARAM),
            lead_target: name(LEAD_TARGET_PARAM),
            lead_max_angle: name(LEAD_MAX_ANGLE_PARAM),
            run_config: name(RUN_CONFIG),
            reset: name(RESET_EFFECT),
            reset_no_heal: name(RESET_NO_HEAL_EFFECT),
            attack_cooldown: name(ATTACK_COOLDOWN),
            is_passive: key(IS_PASSIVE_KEY),
            enable_wander: key(ENABLE_WANDER_KEY),
            wander_max: key(WANDER_MAX_KEY),
            ambient_leash: key(AMBIENT_LEASH_KEY),
            combat_leash: key(COMBAT_LEASH_KEY),
        };
        Self {
            runtime,
            names,
            rates,
            looks,
        }
    }

    /// The AI data file of `client_build` in `dir`, with tick rates `rates` and the looks
    /// `looks`.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError`] when the file is missing, from another build or bad.
    pub fn load(
        dir: &Path,
        client_build: &str,
        rates: AiRates,
        looks: LookAssets,
    ) -> Result<Self, ContainerError> {
        Ok(Self::new(AiData::load(dir, client_build)?, rates, looks))
    }

    /// No data: every mob is without a tree.
    pub fn empty() -> Self {
        Self::new(AiData::default(), AiRates::default(), LookAssets::new())
    }

    /// The archetype of a mob of `vitals_id` shown with slice asset `asset`, else with its
    /// vitals id's look.
    pub fn archetype_for(&self, vitals_id: u32, asset: Option<[u8; ASSET_ID_LEN]>) -> Option<u32> {
        self.archetype_of(&asset.or_else(|| self.looks.get(&vitals_id).copied())?)
    }

    /// The data.
    pub fn data(&self) -> &AiData {
        self.runtime.data()
    }

    /// The tick rates.
    pub const fn rates(&self) -> &AiRates {
        &self.rates
    }

    /// The archetype index of the creature slice asset `asset`.
    pub fn archetype_of(&self, asset: &[u8; ASSET_ID_LEN]) -> Option<u32> {
        let data = self.data();
        let index = data
            .creatures
            .binary_search_by(|creature| creature.asset.cmp(asset))
            .ok()?;
        Some(data.creatures[index].archetype)
    }

    /// Whether archetype `archetype` has a tree Server.pak holds.
    pub fn has_tree(&self, archetype: u32) -> bool {
        self.arch(archetype)
            .is_some_and(|arch| arch.trees.iter().any(|tree| tree.tree.is_some()))
    }

    /// Archetype `archetype`.
    pub fn arch(&self, archetype: u32) -> Option<&Archetype> {
        self.data().archetypes.get(archetype as usize)
    }

    /// The config value of `key` of archetype `archetype` as a number.
    pub fn config_number(&self, archetype: u32, key: &str) -> Option<f64> {
        let key = self.data().key(key)?;
        self.arch(archetype)?.config_value(key)?.as_number()
    }

    /// The action of archetype `archetype`'s grid whose alias is `alias`.
    fn aliased(&self, archetype: u32, alias: Option<NameId>) -> Option<u32> {
        let data = self.data();
        let alias = alias?;
        let grid = data.grids.get(self.arch(archetype)?.grid? as usize)?;
        grid.actions
            .iter()
            .find(|(name, _)| grid.alias(*name) == Some(alias))
            .map(|(_, action)| *action)
    }

    /// The CAGE action `action`.
    pub fn cage_action(&self, action: u32) -> Option<&CageAction> {
        self.data().cage_actions.get(action as usize)
    }

    /// The text of name `id`.
    pub fn name(&self, id: NameId) -> &str {
        self.data().name(id)
    }
}

/// How a patrol goes on after its last point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RouteMode {
    /// Back to the first point.
    #[default]
    Loop,
    /// Back along the points, then forward again.
    BackAndForth,
    /// It stays at the last point.
    Once,
}

/// The waypoints a patrolling mob walks (admin studio spec section 6.4): the `Waypoint_Pos` and
/// `Waypoint_Duration` its tree's Patrol sub-tree reads, advanced by its `SelectWaypointAction`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Route {
    /// The points, in order.
    pub points: Vec<Position>,
    /// The wait at each point (missing: no wait).
    pub waits_ms: Vec<u64>,
    /// What comes after the last point.
    pub mode: RouteMode,
    /// The task at each point (missing: none).
    pub tasks: Vec<Option<RouteTask>>,
}

/// What a mob does at a point of its route: a spawner waypoint's `AIWaypointComponent`
/// ([`nw_world::records::WaypointTask`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteTask {
    /// `Waypoint_Task`: the fragment it plays there.
    pub task: NameId,
    /// `Waypoint_Duration_Min` and `Waypoint_Duration_Max`, seconds (negative: none).
    pub duration_s: [f32; 2],
    /// `Waypoint_Align`.
    pub align: bool,
}

impl RouteTask {
    /// The `Waypoint_Duration` of one visit for the random draw `unit` in `[0, 1)`: uniform
    /// between the minimum and the maximum (Inferred: the engine's rule is Unknown; retail boars
    /// grazed for times between them), the minimum as it is when that is not above 0 (the
    /// Patrol sub-tree then plays the task once, `POI_Waypoint`).
    fn duration_s(&self, unit: f32) -> f32 {
        let [min, max] = self.duration_s;
        if min <= 0.0 {
            min
        } else {
            min + unit * (max - min).max(0.0)
        }
    }
}

/// Where a mob is on its route.
#[derive(Debug, Clone, PartialEq)]
struct RouteWalk {
    route: Arc<Route>,
    next: usize,
    backwards: bool,
    /// The `Waypoint_Duration` of this visit of a point with a task.
    task_s: f32,
}

impl RouteWalk {
    fn point(&self) -> Option<Position> {
        self.route.points.get(self.next).copied()
    }

    fn task(&self) -> Option<&RouteTask> {
        self.route.tasks.get(self.next)?.as_ref()
    }

    fn wait_s(&self) -> f32 {
        if self.task().is_some() {
            return self.task_s;
        }
        // A wait of minutes at most.
        #[allow(clippy::cast_precision_loss, reason = "a wait of minutes")]
        let ms = self.route.waits_ms.get(self.next).copied().unwrap_or(0) as f32;
        ms / MS_PER_S
    }

    /// Draws the duration of the visit of its point from `unit`.
    fn visit(&mut self, unit: f32) {
        self.task_s = self.task().map_or(0.0, |task| task.duration_s(unit));
    }

    /// Goes on to the point after `next`, as the mode says.
    fn advance(&mut self) {
        let last = self.route.points.len().saturating_sub(1);
        match self.route.mode {
            RouteMode::Loop => self.next = if self.next >= last { 0 } else { self.next + 1 },
            RouteMode::Once => self.next = (self.next + 1).min(last),
            RouteMode::BackAndForth => {
                if last == 0 {
                    return;
                }
                if (self.backwards && self.next == 0) || (!self.backwards && self.next >= last) {
                    self.backwards = !self.backwards;
                }
                self.next = if self.backwards {
                    self.next - 1
                } else {
                    self.next + 1
                };
            }
        }
    }
}

/// What a design file changes of a spawn (admin studio spec section 6.5): the slice config
/// values it overrides (they become the mob's own blackboard values), its attack cooldown, its
/// run speed, its sight, its route and its respawn time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpawnAi {
    /// Blackboard values over the slice's.
    pub facts: Vec<(KeyId, Value)>,
    /// The attack cooldown, seconds.
    pub attack_cooldown_s: Option<f64>,
    /// The top speed of its run (`RunToTarget`).
    pub run_speed_mps: Option<f32>,
    /// The distance of its ambient vision.
    pub sight_m: Option<f32>,
    /// The route it patrols.
    pub route: Option<Arc<Route>>,
    /// Its respawn time.
    pub respawn_ms: Option<u64>,
}

/// The overrides of each vitals spawn of `world`, in spawn order; `None` keeps the slice's AI.
///
/// The design fields map onto the trees' own values:
/// - `behaviour`: `guard` sets `Enable_Wander` false (the Wander sub-tree runs only while it is
///   true, Observed); `idle` also sets `Is_Passive` true (a passive mob takes only its attackers
///   as targets, see [`TreeMind::perceive`]); `patrol` walks the spawn's path.
/// - `leash_radius_m`: `AmbientLeashDistance` and `CombatLeashDistance` (DistanceLeash reads
///   them, Observed).
/// - `wander_radius_m`: `WanderDistanceMax` (the Wander sub-tree picks points up to it).
/// - `attack_interval_ms`: the times of the `AttackCooldown` cooldown.
/// - `speed_mps`: the top speed of the `RunToTarget` navigation config.
/// - `sight_m`: the distance of the ambient vision profile.
/// - `respawn_ms`: the respawn time.
///
/// `wake_radius_m`, `wander_pause_ms` and `attack_reach_m` have no value in the trees: the wake
/// radius is the sim's, the wander pause and the attack bands are node literals (the boar's
/// `LimitFidgetAndWander` 2-7 s, its 3 m and 7 m bands). They are left out with a warning.
pub fn resolve_ai(world: &WorldFile, ai: &MobAi) -> Arc<[Option<SpawnAi>]> {
    let names = &ai.names;
    world
        .vitals_ai
        .iter()
        .map(|spawn| {
            if spawn.profile.is_none() && spawn.path.is_none() {
                return None;
            }
            let id = spawn.profile.as_deref().unwrap_or_default();
            let profile = spawn
                .profile
                .as_ref()
                .and_then(|id| world.ai_profiles.get(id))
                .cloned()
                .unwrap_or_default();
            warn_unmapped(id, &profile);
            let route = spawn
                .path
                .as_ref()
                .and_then(|id| world.paths.get(id))
                .map(|path| Arc::new(route_of(path, ai)));
            let mut facts = Vec::new();
            let mut set = |key: Option<KeyId>, value: Value| {
                if let Some(key) = key {
                    facts.push((key, value));
                }
            };
            match profile.behaviour {
                AiBehaviour::Wander | AiBehaviour::Patrol => {}
                AiBehaviour::Guard => set(names.enable_wander, Value::Bool(false)),
                AiBehaviour::Idle => {
                    set(names.enable_wander, Value::Bool(false));
                    set(names.is_passive, Value::Bool(true));
                }
            }
            if let Some(leash) = profile.leash_radius_m {
                set(names.ambient_leash, Value::Float(leash));
                set(names.combat_leash, Value::Float(leash));
            }
            if let Some(wander) = profile.wander_radius_m {
                set(names.wander_max, Value::Float(wander));
            }
            Some(SpawnAi {
                facts,
                // A cooldown of seconds.
                #[allow(clippy::cast_precision_loss, reason = "a cooldown of seconds")]
                attack_cooldown_s: profile
                    .attack_interval_ms
                    .map(|ms| ms as f64 / MS_PER_S_F64),
                run_speed_mps: profile.speed_mps,
                sight_m: profile.sight_m,
                route: route.filter(|_| {
                    matches!(profile.behaviour, AiBehaviour::Patrol | AiBehaviour::Wander)
                }),
                respawn_ms: profile.respawn_ms,
            })
        })
        .collect()
}

fn warn_unmapped(id: &str, profile: &AiProfile) {
    let unmapped = [
        ("wake_radius_m", profile.wake_radius_m.is_some()),
        ("wander_pause_ms", profile.wander_pause_ms.is_some()),
        ("attack_reach_m", profile.attack_reach_m.is_some()),
    ];
    for (field, set) in unmapped {
        if set {
            tracing::warn!(
                profile = id,
                field,
                "AI profile field has no value in the behavior trees: left out"
            );
        }
    }
}

fn route_of(path: &PatrolPath, ai: &MobAi) -> Route {
    Route {
        points: path
            .points
            .iter()
            .map(|&[x, y, z]| Position { x, y, z })
            .collect(),
        waits_ms: path.waits_ms.clone(),
        mode: match path.mode {
            PathMode::Loop => RouteMode::Loop,
            PathMode::BackAndForth => RouteMode::BackAndForth,
            PathMode::Once => RouteMode::Once,
        },
        tasks: path
            .tasks
            .iter()
            .map(|task| {
                let task = task.as_ref()?;
                Some(RouteTask {
                    task: ai.data().name_id(&task.task)?,
                    duration_s: task.duration_s,
                    align: task.align,
                })
            })
            .collect(),
    }
}

/// The height of a mob that moved to `to` on `ground`: the floor's under it, unless that lies
/// more than [`MAX_STEP_DOWN_M`] below the mob.
pub fn height_after_move(ground: &dyn Ground, to: Position) -> f32 {
    height_over(ground.floor_at(to.x, to.y, to.z), to.z)
}

/// [`height_after_move`] of a mob at height `z` over `floor`.
fn height_over(floor: Option<f32>, z: f32) -> f32 {
    match floor {
        Some(height) if z - height <= MAX_STEP_DOWN_M => height,
        _ => z,
    }
}

/// The end of the straight line from `from` to `to` on floor: at each step of [`FLOOR_STEP_M`]
/// the body stands as [`height_after_move`] puts it (on the floor near its height; where the
/// level has no height or its ground lies far below, it keeps its height), and the rise between
/// two steps must stay within
/// `max_slope_deg` (the slice's `m_maxSlope`; no limit without one) and below
/// [`MAX_STEP_RISE_M`], and no wall may stand at knee height between two steps. `None` when the
/// line is too steep, climbs too high, meets a wall, or when a body standing on a floor would
/// step off an edge more than
/// [`MAX_STEP_DOWN_M`] high: kept at its height it walked on in the air (live
/// login-world-entry-01: boars off the wreck's deck 3.7 to 12.1 m above the floor for up to 84
/// s; retail newchar: no body more than 3 m over the floor for longer than a jump). The server has no navigation mesh (the level `.bai` files are not read, their format
/// is Unknown): the level's floors (the terrain and the level collision's structures) stand in
/// for it (Inferred).
pub fn floor_line(
    ground: &dyn Ground,
    from: Position,
    to: Position,
    max_slope_deg: Option<f32>,
) -> Option<Position> {
    let length = from.horizontal_distance(to);
    // A line of tens of metres.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a line of tens of metres"
    )]
    let steps = (length / FLOOR_STEP_M).ceil().max(1.0) as u32;
    let max_rise = max_slope_deg.map(|slope| slope.to_radians().tan());
    let mut last = Position {
        z: height_after_move(ground, from),
        ..from
    };
    for step in 1..=steps {
        // A few hundred steps.
        #[allow(clippy::cast_precision_loss, reason = "a few hundred steps")]
        let share = step as f32 / steps as f32;
        let x = from.x + (to.x - from.x) * share;
        let y = from.y + (to.y - from.y) * share;
        let on_floor = ground
            .floor_at(last.x, last.y, last.z)
            .is_some_and(|floor| (last.z - floor).abs() <= ON_FLOOR_M);
        let below = ground.floor_at(x, y, last.z);
        if on_floor && below.is_some_and(|floor| last.z - floor > MAX_STEP_DOWN_M) {
            return None;
        }
        let z = height_over(below, last.z);
        let run = last.horizontal_distance(Position { x, y, z });
        if max_rise.is_some_and(|rise| run > 0.0 && (z - last.z).abs() / run > rise) {
            return None;
        }
        if z - last.z > MAX_STEP_RISE_M {
            return None;
        }
        // A wall at knee height stops the line, as it stops a pushed body (mob ground height
        // note: 13 of 14,548 live mob moves went into a solid, one 4 times next to a crate).
        let knee = |at: Position| Position {
            z: at.z + STEP_UP_M,
            ..at
        };
        let stand = Position { x, y, z };
        if ground.blocked(knee(last), knee(stand)) {
            return None;
        }
        last = stand;
    }
    Some(last)
}

/// The end of a move from `from` toward `to` over the floors ([`floor_line`]); when the straight
/// line is blocked, the move of the same length along the nearest turn of [`DETOUR_TURNS_DEG`]
/// that is free.
fn floor_move(
    ground: &dyn Ground,
    from: Position,
    to: Position,
    max_slope_deg: Option<f32>,
) -> Option<Position> {
    floor_line(ground, from, to, max_slope_deg).or_else(|| {
        let length = from.horizontal_distance(to);
        let toward = yaw_toward(from, to);
        DETOUR_TURNS_DEG.iter().find_map(|degrees| {
            let side = ahead(from, turned(toward, *degrees), length);
            floor_line(ground, from, side, max_slope_deg)
        })
    })
}

/// Whether a mob at `from` reaches `to` over the floors: straight, or around one solid through a
/// side point along a turn of [`DETOUR_TURNS_DEG`] at most [`DETOUR_LEG_M`] away.
fn reaches(ground: &dyn Ground, from: Position, to: Position, max_slope_deg: Option<f32>) -> bool {
    floor_line(ground, from, to, max_slope_deg).is_some() || {
        let leg = from.horizontal_distance(to).min(DETOUR_LEG_M);
        let toward = yaw_toward(from, to);
        DETOUR_TURNS_DEG.iter().any(|degrees| {
            let side = ahead(from, turned(toward, *degrees), leg);
            floor_line(ground, from, side, max_slope_deg)
                .and_then(|side| floor_line(ground, side, to, max_slope_deg))
                .is_some()
        })
    }
}

/// The yaw of `facing` turned by `degrees` (a positive angle turns west, as the yaw does).
fn turned(facing: f32, degrees: f64) -> f32 {
    // An angle of a turn.
    #[allow(clippy::cast_possible_truncation, reason = "an angle of a turn")]
    let radians = degrees.to_radians() as f32;
    facing + radians
}

/// The point `distance` from `from` along yaw `yaw`.
fn ahead(from: Position, yaw: f32, distance: f32) -> Position {
    Position {
        x: from.x - yaw.sin() * distance,
        y: from.y + yaw.cos() * distance,
        z: from.z,
    }
}

/// How a mob moves now: its tree's navigation or strafe.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Motion {
    slot: Slot,
    kind: MotionKind,
    steering: Steering,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MotionKind {
    /// To a point.
    To(Position),
    /// To the target: to a point the archetype's navigation config `config` picks around it,
    /// picked again when the target moved more than the config's repath distance from where it
    /// was.
    Follow {
        goal: Option<(Position, Position)>,
        config: Option<usize>,
    },
    /// Around the target until `until_ms`.
    Strafe {
        ideal_m: (f32, f32),
        left: bool,
        until_ms: u64,
    },
}

/// The CAGE action a mob plays, as its replicated animation shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// The CAGE action (index in the data's `cage_actions`).
    pub action: u32,
    /// When it started (sim ms).
    pub started_ms: u64,
    /// Counts the starts, so that a start of the same action again is a new entry.
    pub serial: u32,
}

/// A damage window of the action a mob plays (`CAGE-Damage` of its fragment entry), or the
/// moment its `CAGE-RangedAttack` clip fires.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Window {
    at_ms: u64,
    row: NameId,
    reach: MobReach,
    ranged: Option<RangedWindow>,
}

/// The parameters of a `CAGE-RangedAttack` clip.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RangedWindow {
    attack: NameId,
    slice: Option<NameId>,
    lead: bool,
    lead_max_rad: f32,
}

/// The projectile of a `CAGE-RangedAttack` clip, which the mob fires at its target.
#[derive(Debug, Clone, PartialEq)]
pub struct RangedClip {
    /// `RangedAttackName`: the attack of the main-hand weapon's ranged profile.
    pub attack: String,
    /// `SliceOverride`: the projectile slice (lower-case pak path); `None` when the clip names
    /// none.
    pub slice: Option<String>,
    /// `AILeadTarget`: it aims where the target will be when it gets there.
    pub lead: bool,
    /// `AILeadTargetMaxAngle`: how far from the target the lead may turn the aim (radians).
    pub lead_max_rad: f32,
}

/// The hit of a damage window: its damage table row and how far it reaches.
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    /// The unique id of the creature's damage table (`#DamageData` header).
    pub table: String,
    /// The `DamageTableRow` of the window.
    pub row: String,
    /// The item of the creature's main hand, whose weapon row a block's stamina comes from.
    pub weapon: Option<String>,
    /// How the swing reaches its target (a ranged strike lands by its projectile instead).
    pub reach: MobReach,
    /// The projectile of a ranged clip; `None` for a damage window.
    pub ranged: Option<RangedClip>,
}

/// A hit of a damage window on the mob's target.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowHit {
    /// The target.
    pub target: u64,
    /// Where the target was.
    pub target_at: Position,
    /// The hit.
    pub strike: Strike,
}

/// What a mob's think step did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thought {
    /// The hits whose damage windows came.
    pub hits: Vec<WindowHit>,
    /// The target it took this step, when it had none.
    pub acquired: Option<u64>,
    /// A reset that heals ended: refill its health (Inferred: the `ResetAI` row of
    /// `javelindata_statuseffects_ai` has health columns that `ResetAINoHeal` lacks).
    pub healed: bool,
    /// Its tree ran this step (its tick was due).
    pub ticked: bool,
    /// The encounter events its tree sent (`EncounterEventDecorator`), for the encounter that
    /// spawned it (dungeon scripts).
    pub encounter_events: Vec<NameId>,
}

/// The surroundings of a mob in one step.
pub struct Surroundings<'a> {
    /// The sim time.
    pub now_ms: u64,
    /// The players it may take as targets.
    pub seen: &'a [Seen],
    /// The level's ground: the terrain and the level collision's structures.
    pub ground: &'a (dyn Ground + Sync),
    /// Its health, 0 to 1.
    pub health_share: f32,
}

/// One mob's mind: its tree's brain and what the engine keeps for it.
#[derive(Debug, Clone)]
pub struct TreeMind {
    brain: Brain,
    state: MindState,
}

/// What the engine keeps for a mob's tree: the parts its world view reads, apart from the brain
/// so that the brain can think while the view reads them.
#[derive(Debug, Clone)]
struct MindState {
    archetype: u32,
    spawn: Position,
    yaw: f32,
    /// Its facing is its own: it turned or moved since its spawn.
    turned: bool,
    speed_mps: f32,
    motion: Option<Motion>,
    shown: Option<Shown>,
    serial: u32,
    task_slot: Option<Slot>,
    /// The turn of a facing task: its start, its end and its start yaw.
    turn: Option<(u64, u64, f32)>,
    windows: Vec<Window>,
    target: Option<u64>,
    target_at: Option<Position>,
    threat: Threat,
    locked: bool,
    manual: Option<Position>,
    last_damage: Option<Position>,
    reachable_ms: Option<u64>,
    statuses: Vec<NameId>,
    /// While a hit reaction plays: until when.
    reaction_until_ms: Option<u64>,
    route: Option<RouteWalk>,
    next_tick_ms: u64,
    last_step_ms: Option<u64>,
    picks: Rng48,
    run_speed_mps: Option<f32>,
    sight_m: Option<f32>,
}

/// What a mob perceives in one step.
struct Perceived {
    target: Option<TargetView>,
    nearest_m: f32,
    candidates: u32,
}

#[derive(Debug, Clone, Copy)]
struct TargetView {
    position: Position,
    distance_m: f32,
    angle_deg: f32,
    visible: bool,
    reachable: bool,
}

impl MindState {
    /// The vision of the archetype: ambient without a target, combat with one (the slice's
    /// `m_ambientVisionProfile` and `m_combatVisionProfile`; `-1` is none). A design override of
    /// the sight replaces the ambient distance.
    fn vision(&self, ai: &MobAi, combat: bool) -> Option<(f32, f32)> {
        let arch = ai.arch(self.archetype)?;
        let profile = if combat {
            arch.combat_vision.or(arch.ambient_vision)
        } else {
            arch.ambient_vision
        }?;
        let distance = if combat {
            profile.distance_m
        } else {
            self.sight_m.unwrap_or(profile.distance_m)
        };
        (profile.angle_deg >= 0.0 && distance >= 0.0).then_some((profile.angle_deg, distance))
    }

    fn stop_moving(&mut self) {
        self.motion = None;
        self.speed_mps = 0.0;
    }

    fn motion_goal(&self) -> Option<Position> {
        match self.motion?.kind {
            MotionKind::To(goal) => Some(goal),
            MotionKind::Follow { goal, .. } => goal.map(|(point, _)| point),
            MotionKind::Strafe { .. } => None,
        }
    }

    /// Whether the mob is in a leash reset (it walks home and sees no player).
    fn resetting(&self, ai: &MobAi) -> bool {
        self.statuses.iter().any(|status| {
            Some(*status) == ai.names.reset || Some(*status) == ai.names.reset_no_heal
        })
    }

    /// Picks the target (the targeting profile's curve formula is Unknown, study section 3.2;
    /// this rule is Inferred): a mob in a leash reset has none (it walks home); a target is kept
    /// while it is within the combat vision distance, and while locked
    /// (`LockTargetSelection`); else the top threat that is near; else, for a mob that is not
    /// `passive`, the nearest player in its ambient vision cone (the slice profile: the cone's
    /// full angle and its distance).
    fn perceive(
        &mut self,
        ai: &MobAi,
        at: Position,
        passive: bool,
        around: &Surroundings<'_>,
    ) -> Perceived {
        let combat = self.vision(ai, true);
        let seen: &[Seen] = if self.resetting(ai) { &[] } else { around.seen };
        let in_combat_range = |player: &Seen| {
            combat.is_none_or(|(_, distance)| player.position.horizontal_distance(at) <= distance)
        };
        let near = |entity: u64| {
            seen.iter()
                .find(|player| player.entity == entity)
                .filter(|player| in_combat_range(player))
                .copied()
        };
        let had = self.target;
        if self.target.is_some_and(|target| near(target).is_none()) {
            self.target = None;
        }
        if !(self.locked && self.target.is_some()) {
            while let Some(top) = self.threat.top() {
                if near(top).is_some() {
                    self.target = Some(top);
                    break;
                }
                self.threat.forget(top);
            }
        }
        if self.target.is_none()
            && !passive
            && let Some((angle_deg, distance_m)) = self.vision(ai, false)
        {
            self.target = seen
                .iter()
                .filter(|player| {
                    let distance = player.position.horizontal_distance(at);
                    let bearing = yaw_gap(self.yaw, yaw_toward(at, player.position)).abs();
                    distance <= distance_m && bearing.to_degrees() <= angle_deg * HALF
                })
                .min_by(|a, b| {
                    let da = a.position.horizontal_distance(at);
                    let db = b.position.horizontal_distance(at);
                    da.total_cmp(&db).then(a.entity.cmp(&b.entity))
                })
                .map(|player| player.entity);
        }
        if self.target.is_some() && had.is_none() {
            // The fight takes over from the damage reaction (Inferred).
            self.last_damage = None;
        }
        let max_slope = ai.arch(self.archetype).and_then(|arch| arch.max_slope_deg);
        let target = self.target.and_then(near).map(|player| {
            let position = self.manual.unwrap_or(player.position);
            let distance_m = position.horizontal_distance(at);
            let gap = yaw_gap(self.yaw, yaw_toward(at, position));
            let reachable = reaches(around.ground, at, position, max_slope);
            if reachable {
                self.reachable_ms = Some(around.now_ms);
            }
            TargetView {
                position,
                distance_m,
                angle_deg: gap.to_degrees(),
                visible: combat.is_none_or(|(_, distance)| distance_m <= distance),
                reachable,
            }
        });
        self.target_at = target.map(|target| target.position);
        if target.is_none() {
            self.reachable_ms = None;
        }
        let nearest_m = around
            .seen
            .iter()
            .map(|player| player.position.horizontal_distance(at))
            .fold(f32::INFINITY, f32::min);
        let candidates = seen.iter().filter(|player| in_combat_range(player)).count();
        Perceived {
            target,
            nearest_m,
            candidates: u32::try_from(candidates).unwrap_or(u32::MAX),
        }
    }

    /// The world view of this state.
    fn view<'a>(&'a self, ai: &'a MobAi, at: Position, now_ms: u64, passive: bool) -> View<'a> {
        View {
            ai,
            state: self,
            at,
            now_ms,
            passive,
            target: None,
            candidates: 0,
            health_share: 1.0,
            ground: None,
            picks: Cell::new(self.picks),
        }
    }

    fn fire_windows(&mut self, ai: &MobAi, now_ms: u64, thought: &mut Thought) {
        let (due, later): (Vec<Window>, Vec<Window>) = self
            .windows
            .iter()
            .partition(|window| now_ms >= window.at_ms);
        self.windows = later;
        let (Some(target), Some(target_at)) = (self.target, self.target_at) else {
            return;
        };
        let Some(arch) = ai.arch(self.archetype) else {
            return;
        };
        let Some(table) = arch.damage_table_id else {
            return;
        };
        for window in due {
            thought.hits.push(WindowHit {
                target,
                target_at,
                strike: Strike {
                    table: ai.name(table).to_owned(),
                    row: ai.name(window.row).to_owned(),
                    weapon: arch.main_hand_item.map(|item| ai.name(item).to_owned()),
                    reach: window.reach,
                    ranged: window.ranged.map(|ranged| RangedClip {
                        attack: ai.name(ranged.attack).to_owned(),
                        slice: ranged.slice.map(|slice| ai.name(slice).to_lowercase()),
                        lead: ranged.lead,
                        lead_max_rad: ranged.lead_max_rad,
                    }),
                },
            });
        }
    }

    /// The task `task` of `slot` starts playing.
    fn show(&mut self, ai: &MobAi, slot: Slot, task: NameId, now_ms: u64) {
        let data = ai.data();
        let Some(action) = ai
            .arch(self.archetype)
            .and_then(|arch| data.grids.get(arch.grid? as usize))
            .and_then(|grid| grid.action(task))
        else {
            return;
        };
        self.serial = self.serial.wrapping_add(1);
        self.shown = Some(Shown {
            action,
            started_ms: now_ms,
            serial: self.serial,
        });
        self.task_slot = Some(slot);
    }

    /// A CAGE task starts: what it shows, its turn, and the damage windows of the fragment entries
    /// the mob plays, its body's and its weapon's (their `CAGE-Damage` clips: start and `DamageTableRow`; the reach comes from
    /// the clip's shape and the entry's `Homing`, see [`window_reach`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "the fields of a PlayTask command"
    )]
    fn play(
        &mut self,
        ai: &MobAi,
        tags: &[NameId],
        slot: Slot,
        task: NameId,
        action: u32,
        done_at_ms: Option<u64>,
        now_ms: u64,
    ) {
        self.show(ai, slot, task, now_ms);
        self.windows.clear();
        self.turn = None;
        let Some(cage) = ai.cage_action(action) else {
            return;
        };
        if cage.facing.is_some() {
            // Inferred: the animation turns the body over the action (retail `Combat_Turn` 1632
            // and 1683 ms for 77 to 107 degrees, trace finding a).
            self.turn = done_at_ms.map(|end| (now_ms, end, self.yaw));
        }
        let has_tag = |tag| tags.contains(&tag);
        // The weapon's `WeaponContext` scope plays the fragment from the weapon's database
        // beside the body. Its windows count only when the body's entry has no `CAGE-Damage`
        // clip (Inferred: 449 tasks hold damage clips in both, mostly the same count, as one hit
        // written twice; Janszoon's axe hits are only in the weapon's database).
        let body = cage
            .variant(&has_tag)
            .map_or(&[][..], |variant| &variant.windows[..]);
        let weapon = cage
            .weapon_variant(&has_tag)
            .map_or(&[][..], |variant| &variant.windows[..]);
        let is_damage = |window: &&ProcWindow| Some(window.kind) == ai.names.damage_clip;
        let windows = if body.iter().any(|window| is_damage(&window)) {
            body
        } else {
            weapon
        };
        let homing = body
            .iter()
            .chain(weapon)
            .find(|window| Some(window.kind) == ai.names.homing_clip);
        for window in windows.iter().filter(is_damage) {
            let Some(row) = param_of(window, ai.names.damage_row) else {
                continue;
            };
            // Seconds of a fragment.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "seconds of a fragment"
            )]
            let at_ms = now_ms + (window.start_s * MS_PER_S).round() as u64;
            self.windows.push(Window {
                at_ms,
                row,
                reach: window_reach(ai, window, homing),
                ranged: None,
            });
        }
        // A ranged clip written in both databases is one shot, as a damage clip.
        let is_ranged = |window: &&ProcWindow| Some(window.kind) == ai.names.ranged_clip;
        let ranged_windows = if body.iter().any(|window| is_ranged(&window)) {
            body
        } else {
            weapon
        };
        for window in ranged_windows.iter().filter(is_ranged) {
            let (Some(row), Some(attack)) = (
                param_of(window, ai.names.damage_row),
                param_of(window, ai.names.ranged_attack),
            ) else {
                continue;
            };
            let text = |name| param_of(window, name).map(|value| ai.name(value).trim());
            // Seconds of a fragment.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "seconds of a fragment"
            )]
            let at_ms = now_ms + (window.start_s * MS_PER_S).round() as u64;
            self.windows.push(Window {
                at_ms,
                row,
                reach: window_reach(ai, window, homing),
                ranged: Some(RangedWindow {
                    attack,
                    slice: param_of(window, ai.names.slice_override)
                        .filter(|slice| !ai.name(*slice).trim().is_empty()),
                    lead: text(ai.names.lead_target) == Some(PARAM_TRUE),
                    lead_max_rad: text(ai.names.lead_max_angle)
                        .and_then(|degrees| degrees.parse::<f32>().ok())
                        .unwrap_or(0.0)
                        .to_radians(),
                }),
            });
        }
    }

    /// The moves of the motion and the turn of a facing task over `dt_s`; the end of a motion
    /// goes to `brain`.
    fn move_and_turn(
        &mut self,
        ai: &MobAi,
        brain: &mut Brain,
        at: &mut Position,
        target: Option<TargetView>,
        around: &Surroundings<'_>,
        dt_s: f32,
    ) {
        let now_ms = around.now_ms;
        let ground = around.ground;
        if let Some((start_ms, end_ms, from_yaw)) = self.turn {
            let facing = self
                .shown
                .and_then(|shown| ai.cage_action(shown.action))
                .and_then(|action| action.facing);
            let toward = match (facing, target) {
                (Some(FacingMode::AngleToTarget), Some(target)) => {
                    Some(yaw_toward(*at, target.position))
                }
                _ => self.motion_goal().map(|goal| yaw_toward(*at, goal)),
            };
            if let Some(toward) = toward {
                // A share of a turn of a second or two.
                #[allow(clippy::cast_precision_loss, reason = "a turn of a second or two")]
                let share = (now_ms.saturating_sub(start_ms) as f32
                    / end_ms.saturating_sub(start_ms).max(1) as f32)
                    .min(1.0);
                self.yaw = from_yaw + yaw_gap(from_yaw, toward) * share;
                self.turned = true;
            }
        }
        let Some(mut motion) = self.motion else {
            return;
        };
        let mut mover = Mover {
            position: *at,
            yaw: self.yaw,
            speed_mps: self.speed_mps,
        };
        let max_slope = ai.arch(self.archetype).and_then(|arch| arch.max_slope_deg);
        let outcome = match &mut motion.kind {
            MotionKind::To(goal) => mover
                .steer_to(*goal, &motion.steering, dt_s)
                .then_some(MotionOutcome::Arrived),
            MotionKind::Follow { goal, config } => match target {
                None => Some(MotionOutcome::Failed),
                Some(target) => {
                    let config =
                        config.and_then(|index| ai.arch(self.archetype)?.nav_configs.get(index));
                    let repath_m = config.map_or(0.0, |config| config.target_repath_distance);
                    let stale = goal.is_none_or(|(_, picked_at)| {
                        picked_at.horizontal_distance(target.position) > repath_m
                    });
                    if stale {
                        let picked = config
                            .and_then(|config| config.select_position.as_ref())
                            .and_then(|params| {
                                self.pick_around(ai, *at, target.position, params, around.ground)
                            })
                            .unwrap_or(target.position);
                        *goal = Some((picked, target.position));
                    }
                    goal.and_then(|(point, _)| {
                        mover
                            .steer_to(point, &motion.steering, dt_s)
                            .then_some(MotionOutcome::Arrived)
                    })
                }
            },
            MotionKind::Strafe {
                ideal_m,
                left,
                until_ms,
            } => match target {
                None => Some(MotionOutcome::Failed),
                Some(_) if now_ms >= *until_ms => Some(MotionOutcome::Arrived),
                Some(target) => {
                    mover.strafe_around(target.position, *ideal_m, *left, &motion.steering, dt_s);
                    None
                }
            },
        };
        let outcome = match floor_move(ground, *at, mover.position, max_slope) {
            Some(on_floor) => {
                *at = Position {
                    z: height_after_move(ground, on_floor),
                    ..on_floor
                };
                outcome
            }
            // It cannot go on: no floor ahead within its slope.
            None => Some(MotionOutcome::Failed),
        };
        self.yaw = mover.yaw;
        self.turned = true;
        self.speed_mps = mover.speed_mps;
        match outcome {
            Some(outcome) => {
                brain.report_motion(motion.slot, outcome);
                self.stop_moving();
                // The tree learns of it at once.
                self.next_tick_ms = 0;
            }
            None => self.motion = Some(motion),
        }
    }

    /// A point around `anchor` that `params` pick (a navigation config's
    /// `m_selectPositionParams`: the boar's `RunToTarget` picks 1.5 to 2 m from its target), with
    /// the mob at `at`; the pick reads only the params' own values.
    fn pick_around(
        &mut self,
        ai: &MobAi,
        at: Position,
        anchor: Position,
        params: &SelectPositionParams,
        ground: &(dyn Ground + Sync),
    ) -> Option<Position> {
        let (picked, picks) = {
            let mut view = self.view(ai, at, 0, false);
            view.ground = Some(ground);
            view.target = Some(TargetView {
                position: anchor,
                distance_m: anchor.horizontal_distance(at),
                angle_deg: 0.0,
                visible: true,
                reachable: true,
            });
            let picked = view.select_position(params, &NoBoard).map(position_of);
            (picked, view.picks.get())
        };
        self.picks = picks;
        picked
    }
}

impl TreeMind {
    /// The mind of a mob of archetype `archetype` at `spawn` facing `yaw`, with random numbers
    /// from `seed` and the design overrides `spawn_ai`; `None` without a tree.
    pub fn new(
        ai: &MobAi,
        archetype: u32,
        spawn: Position,
        yaw: f32,
        seed: u64,
        spawn_ai: Option<&SpawnAi>,
    ) -> Option<Self> {
        if !ai.has_tree(archetype) {
            return None;
        }
        let mut brain = Brain::new(&ai.runtime, archetype, seed).ok()?;
        if let Some(key) = ai.data().key(DOES_LEASH_KEY)
            && ai
                .arch(archetype)
                .is_none_or(|arch| arch.config_value(key).is_none())
        {
            brain.set_fact(key, Value::Bool(true));
        }
        if let Some(overrides) = spawn_ai {
            for (key, value) in &overrides.facts {
                brain.set_fact(*key, *value);
            }
            if let (Some(seconds), Some(name)) =
                (overrides.attack_cooldown_s, ai.names.attack_cooldown)
            {
                brain.set_cooldown_times(name, seconds, seconds);
            }
        }
        Some(Self {
            brain,
            state: MindState {
                archetype,
                spawn,
                yaw,
                turned: false,
                speed_mps: 0.0,
                motion: None,
                shown: None,
                serial: 0,
                task_slot: None,
                turn: None,
                windows: Vec::new(),
                target: None,
                target_at: None,
                threat: Threat::default(),
                locked: false,
                manual: None,
                last_damage: None,
                reachable_ms: None,
                statuses: Vec::new(),
                reaction_until_ms: None,
                route: spawn_ai
                    .and_then(|overrides| overrides.route.clone())
                    .filter(|route| !route.points.is_empty())
                    .map(|route| {
                        let mut walk = RouteWalk {
                            route,
                            next: 0,
                            backwards: false,
                            task_s: 0.0,
                        };
                        walk.visit(Rng48::new(seed).unit());
                        walk
                    }),
                next_tick_ms: 0,
                last_step_ms: None,
                picks: Rng48::new(seed.rotate_left(1)),
                run_speed_mps: spawn_ai.and_then(|overrides| overrides.run_speed_mps),
                sight_m: spawn_ai.and_then(|overrides| overrides.sight_m),
            },
        })
    }

    /// Its archetype.
    pub const fn archetype(&self) -> u32 {
        self.state.archetype
    }

    /// Where it faces.
    pub const fn yaw(&self) -> f32 {
        self.state.yaw
    }

    /// Whether its facing is its own: it turned or moved since its spawn (the spawn's facing is
    /// the prefab's, which the world file does not keep).
    pub const fn turned(&self) -> bool {
        self.state.turned
    }

    /// Its target.
    pub const fn target(&self) -> Option<u64> {
        self.state.target
    }

    /// The action it plays.
    pub const fn shown(&self) -> Option<Shown> {
        self.state.shown
    }

    /// The task of the route point it is at, when the point has one.
    pub fn waypoint_task(&self) -> Option<NameId> {
        Some(self.state.route.as_ref()?.task()?.task)
    }

    /// Its Mannequin tags.
    pub fn tags(&self) -> &[NameId] {
        self.brain.tags()
    }

    /// The brain.
    pub const fn brain(&self) -> &Brain {
        &self.brain
    }

    /// Whether it walks home in a leash reset: the tree applied `ResetAI` or `ResetAINoHeal`
    /// (it takes no damage then; Inferred, as the old return state).
    pub fn returning(&self, ai: &MobAi) -> bool {
        self.state.statuses.iter().any(|status| {
            Some(*status) == ai.names.reset || Some(*status) == ai.names.reset_no_heal
        })
    }

    /// Whether it plays an action with a damage window still to come.
    pub fn attacking(&self) -> bool {
        !self.state.windows.is_empty()
    }

    /// Whether its slice (or its spawn's override) makes it passive.
    fn passive(&self, ai: &MobAi) -> bool {
        ai.names
            .is_passive
            .and_then(|key| {
                self.brain
                    .fact(key)
                    .or_else(|| ai.arch(self.state.archetype)?.config_value(key))
            })
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `attacker` at `from` hit the mob, adding `threat`: whether the hit counts (a mob in a
    /// leash reset takes none).
    pub fn hurt(&mut self, ai: &MobAi, attacker: u64, from: Position, threat: u64) -> bool {
        if self.returning(ai) {
            return false;
        }
        self.state.threat.add(attacker, threat);
        self.state.last_damage = Some(from);
        // A hit wakes a waiting tree at once.
        self.state.next_tick_ms = 0;
        true
    }

    /// The mob takes `target` as its target when it has none (group aggro).
    pub fn share_target(&mut self, target: u64) {
        if self.state.target.is_none() {
            self.state.threat.add(target, 0);
            self.state.next_tick_ms = 0;
        }
    }

    /// The mob forgets `entity`.
    pub fn forget(&mut self, entity: u64) {
        self.state.threat.forget(entity);
        if self.state.target == Some(entity) {
            self.state.target = None;
        }
    }

    /// A hit reaction of `length_ms` starts at `now_ms` on the mob at `at`: the engine's
    /// `Reaction` action reaches the tree (it cancels the plan and sets the action's tags, for
    /// example the boar's `Alerted`; Observed in `boar_reaction.actionlist`), and
    /// `Full_Body_Reaction` is true while it plays. A swing in progress is lost (retail dropped a
    /// creature's swing when a hit made it reel; swing note).
    pub fn react(&mut self, ai: &MobAi, at: Position, now_ms: u64, length_ms: u64) {
        self.state.reaction_until_ms = Some(now_ms + length_ms);
        self.reaction(ai, at, now_ms, true);
        self.state.stop_moving();
        self.state.windows.clear();
    }

    /// The engine's `Reaction` action starts (`playing`) or ends.
    fn reaction(&mut self, ai: &MobAi, at: Position, now_ms: u64, playing: bool) {
        self.state.next_tick_ms = 0;
        let Some(action) = ai.aliased(self.state.archetype, ai.names.reaction) else {
            return;
        };
        let passive = self.passive(ai);
        let mut out = Vec::new();
        {
            let view = self.state.view(ai, at, now_ms, passive);
            if playing {
                self.brain
                    .engine_action(&ai.runtime, &view, action, &mut out);
            } else {
                self.brain
                    .engine_action_end(&ai.runtime, &view, action, &mut out);
            }
        }
        self.apply(ai, at, now_ms, out, &mut Thought::default());
    }

    /// The mob died at `at`: the engine's `Death` action cancels the plan.
    pub fn died(&mut self, ai: &MobAi, at: Position, now_ms: u64) {
        if let Some(action) = ai.aliased(self.state.archetype, ai.names.death) {
            let passive = self.passive(ai);
            let mut out = Vec::new();
            let view = self.state.view(ai, at, now_ms, passive);
            self.brain
                .engine_action(&ai.runtime, &view, action, &mut out);
        }
        let state = &mut self.state;
        state.stop_moving();
        state.windows.clear();
        state.shown = None;
        state.target = None;
        state.threat.clear();
    }

    /// Whether the mind has work at `now_ms`: its tick, a damage window or the end of a
    /// reaction is due (between its ticks a mob neither perceives nor moves: the retail state
    /// rate, trace finding c).
    pub fn due(&self, now_ms: u64) -> bool {
        let state = &self.state;
        now_ms >= state.next_tick_ms
            || state.windows.iter().any(|window| now_ms >= window.at_ms)
            || state.reaction_until_ms.is_some_and(|until| now_ms >= until)
    }

    /// One step at `around.now_ms` of the mob at `at`: it perceives, its damage windows hit,
    /// and when its tick is due it moves, turns and its tree thinks.
    pub fn step(&mut self, ai: &MobAi, at: &mut Position, around: &Surroundings<'_>) -> Thought {
        let now_ms = around.now_ms;
        let mut thought = Thought::default();
        if self
            .state
            .reaction_until_ms
            .is_some_and(|until| now_ms >= until)
        {
            self.state.reaction_until_ms = None;
            self.reaction(ai, *at, now_ms, false);
        }
        let had = self.state.target;
        let passive = self.passive(ai);
        let perceived = self.state.perceive(ai, *at, passive, around);
        self.log_target_change(ai, had, *at);
        let perceived_target = self.state.target;
        if had.is_none() {
            thought.acquired = self.state.target;
        }
        self.state.fire_windows(ai, now_ms, &mut thought);
        if now_ms < self.state.next_tick_ms {
            return thought;
        }
        let dt_ms = self
            .state
            .last_step_ms
            .map_or(0, |last| now_ms.saturating_sub(last).min(MAX_STEP_MS));
        self.state.last_step_ms = Some(now_ms);
        // A step below a second.
        #[allow(clippy::cast_precision_loss, reason = "a step below a second")]
        let dt_s = dt_ms as f32 / MS_PER_S;
        self.state
            .move_and_turn(ai, &mut self.brain, at, perceived.target, around, dt_s);
        let mut out = Vec::new();
        let picks = {
            let mut view = self.state.view(ai, *at, now_ms, passive);
            view.target = perceived.target;
            view.candidates = perceived.candidates;
            view.health_share = around.health_share;
            view.ground = Some(around.ground);
            self.brain.tick(&ai.runtime, &view, &mut out);
            view.picks.get()
        };
        self.state.picks = picks;
        thought.ticked = true;
        self.apply(ai, *at, now_ms, out, &mut thought);
        self.log_target_change(ai, perceived_target, *at);
        let activity = if self.state.target.is_some() {
            Activity::Combat
        } else if self.state.motion.is_some() {
            Activity::Moving
        } else {
            Activity::Idle
        };
        self.state.next_tick_ms = now_ms + ai.rates.interval_ms(activity, perceived.nearest_m);
        thought
    }

    /// Logs a change of the target from `from` (the live check of aggro and leash).
    fn log_target_change(&self, ai: &MobAi, from: Option<u64>, at: Position) {
        if self.state.target != from {
            tracing::debug!(
                archetype = self.state.archetype,
                from = ?from,
                to = ?self.state.target,
                resetting = self.state.resetting(ai),
                x = at.x,
                y = at.y,
                z = at.z,
                "mob target changed"
            );
        }
    }

    /// The steering of a navigation config of the archetype, or of the node's own fields.
    fn steering(
        &self,
        ai: &MobAi,
        node: NodeId,
        config: Option<&NavConfig>,
        view: &View<'_>,
    ) -> Steering {
        if let Some(config) = config {
            let run = Some(config.kind) == ai.names.run_config;
            return Steering {
                max_speed_mps: self
                    .state
                    .run_speed_mps
                    .filter(|_| run)
                    .unwrap_or(config.max_speed),
                min_speed_mps: config.min_speed,
                acceleration_mps2: config.linear_acceleration,
                deceleration_mps2: config.linear_deceleration,
                turn_rate_deg_s: config.turn_rate_deg,
                arrival_m: config.arrival_threshold,
            };
        }
        let number = |param: &Param| {
            self.brain
                .resolve(&ai.runtime, view, param)
                .and_then(Value::as_number)
                // Node fields of metres and seconds.
                .map_or(0.0, |value| value as f32)
        };
        let (motion, turn_rate) = match &ai.data().node(node).kind {
            NodeKind::Navigation(nav) => (&nav.motion, &nav.turn_rate),
            // Inferred: a strafe's facing follows its target at this rate.
            NodeKind::Strafe(strafe) => (&strafe.motion, &strafe.facing_convergence_rate),
            _ => return STANDING,
        };
        Steering {
            max_speed_mps: number(&motion.max_speed),
            min_speed_mps: number(&motion.min_speed),
            acceleration_mps2: number(&motion.linear_acceleration),
            deceleration_mps2: number(&motion.linear_deceleration),
            turn_rate_deg_s: number(turn_rate),
            arrival_m: number(&motion.arrival_threshold),
        }
    }

    /// Applies the tree's commands at `now_ms` to the mob at `at`.
    fn apply(
        &mut self,
        ai: &MobAi,
        at: Position,
        now_ms: u64,
        out: Vec<Command>,
        thought: &mut Thought,
    ) {
        let passive = self.passive(ai);
        for command in out {
            match command {
                Command::PlayTask {
                    slot,
                    task,
                    action,
                    done_at_ms,
                    ..
                } => {
                    let tags = self.brain.tags();
                    self.state
                        .play(ai, tags, slot, task, action, done_at_ms, now_ms);
                }
                Command::EndTask { slot } => {
                    if self.state.task_slot == Some(slot) {
                        self.state.windows.clear();
                        self.state.turn = None;
                    }
                }
                Command::Navigate {
                    slot,
                    node,
                    task,
                    destination,
                    config,
                } => {
                    let config_index = config;
                    let config = config
                        .and_then(|index| ai.arch(self.state.archetype)?.nav_configs.get(index));
                    let steering = {
                        let view = self.state.view(ai, at, now_ms, passive);
                        self.steering(ai, node, config, &view)
                    };
                    let kind = match destination {
                        Destination::Point(point) => MotionKind::To(position_of(point)),
                        Destination::Target => MotionKind::Follow {
                            goal: None,
                            config: config_index,
                        },
                    };
                    self.state.motion = Some(Motion {
                        slot,
                        kind,
                        steering,
                    });
                    self.state.show(ai, slot, task, now_ms);
                }
                Command::Strafe {
                    slot,
                    node,
                    task,
                    config,
                } => {
                    let NodeKind::Strafe(strafe) = &ai.data().node(node).kind else {
                        continue;
                    };
                    let config = config
                        .and_then(|index| ai.arch(self.state.archetype)?.nav_configs.get(index));
                    let (steering, ideal, lateral) = {
                        let view = self.state.view(ai, at, now_ms, passive);
                        let number = |param: &Param| {
                            self.brain
                                .resolve(&ai.runtime, &view, param)
                                .and_then(Value::as_number)
                        };
                        (
                            self.steering(ai, node, config, &view),
                            (
                                number(&strafe.ideal_distance_min).unwrap_or(0.0),
                                number(&strafe.ideal_distance_max).unwrap_or(0.0),
                            ),
                            number(&strafe.lateral_frequency_min)
                                .zip(number(&strafe.lateral_frequency_max)),
                        )
                    };
                    // Inferred: one sideways move lasts a random time of its lateral move
                    // frequency (seconds); retail strafes of 31 to 1303 ms were cut earlier by the
                    // next plan (trace finding b).
                    let lateral_s =
                        lateral.map_or(0.0, |(min, max)| self.state.picks.range(min, max));
                    let left = self.state.picks.unit() < HALF;
                    // Metres and seconds of the node.
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "metres and seconds of the node"
                    )]
                    let (ideal_m, until_ms) = (
                        (ideal.0 as f32, ideal.1 as f32),
                        now_ms + (lateral_s * MS_PER_S_F64) as u64,
                    );
                    self.state.motion = Some(Motion {
                        slot,
                        kind: MotionKind::Strafe {
                            ideal_m,
                            left,
                            until_ms,
                        },
                        steering,
                    });
                    self.state.show(ai, slot, task, now_ms);
                }
                Command::StopMotion { slot } => {
                    if self.state.motion.is_some_and(|motion| motion.slot == slot) {
                        self.state.stop_moving();
                    }
                }
                Command::StatusEffect {
                    effect,
                    apply,
                    node,
                } => {
                    self.status_effect(ai, effect, apply, thought);
                    self.log_reset(ai, effect, apply, node, at, now_ms);
                }
                Command::LockTarget(locked) => self.state.locked = locked,
                Command::ManualTarget(point) => self.state.manual = point.map(position_of),
                Command::Action { node, .. } => match ai.data().node(node).kind {
                    NodeKind::SelectWaypoint => {
                        if let Some(route) = &mut self.state.route {
                            route.advance();
                            route.visit(self.state.picks.unit());
                        }
                    }
                    ref other => tracing::debug!(
                        archetype = self.state.archetype,
                        node = ?std::mem::discriminant(other),
                        "tree action the sim does not run"
                    ),
                },
                Command::EncounterEvent(event) => {
                    tracing::debug!(
                        archetype = self.state.archetype,
                        event = ai.name(event),
                        "tree encounter event"
                    );
                    thought.encounter_events.push(event);
                }
                // The tags stay in the brain (the replicated tag state reads them); action
                // targets are the mob's candidates (see `World::action_targets`).
                Command::Tag { .. } | Command::TrackActionTargets(_) => {}
            }
        }
    }

    /// Logs the start of a leash reset by tree node `node` (the live check of the leash): what
    /// the tree's leash conditions read then.
    fn log_reset(
        &self,
        ai: &MobAi,
        effect: NameId,
        apply: bool,
        node: NodeId,
        at: Position,
        now_ms: u64,
    ) {
        if !apply || (Some(effect) != ai.names.reset && Some(effect) != ai.names.reset_no_heal) {
            return;
        }
        let state = &self.state;
        tracing::debug!(
            archetype = state.archetype,
            node,
            since_reachable_ms = ?state.reachable_ms.map(|at_ms| now_ms.saturating_sub(at_ms)),
            from_spawn_m = at.horizontal_distance(state.spawn),
            from_waypoint_m = ?state.route.as_ref().and_then(RouteWalk::point).map(|point| at.horizontal_distance(point)),
            target_at = ?state.target_at,
            x = at.x,
            y = at.y,
            z = at.z,
            "mob leash reset"
        );
    }

    /// The tree applies (`apply`) or removes status effect `effect`. The sim keeps the effects
    /// for the `SE_` keys; a leash reset (`ResetAI`, `ResetAINoHeal`) ends the fight, and the
    /// end of `ResetAI` heals (see [`Thought::healed`]).
    fn status_effect(&mut self, ai: &MobAi, effect: NameId, apply: bool, thought: &mut Thought) {
        let state = &mut self.state;
        if apply {
            if !state.statuses.contains(&effect) {
                state.statuses.push(effect);
            }
            if Some(effect) == ai.names.reset || Some(effect) == ai.names.reset_no_heal {
                // Inferred: the reset walks home and forgets the fight.
                state.target = None;
                state.threat.clear();
                state.last_damage = None;
            }
        } else {
            state.statuses.retain(|known| *known != effect);
            thought.healed |= Some(effect) == ai.names.reset;
        }
    }
}

/// The steering of a node that does not move.
const STANDING: Steering = Steering {
    max_speed_mps: 0.0,
    min_speed_mps: 0.0,
    acceleration_mps2: 0.0,
    deceleration_mps2: 0.0,
    turn_rate_deg_s: 0.0,
    arrival_m: 0.0,
};

/// The text value of parameter `name` of `window`.
fn param_of(window: &ProcWindow, name: Option<NameId>) -> Option<NameId> {
    let name = name?;
    window
        .params
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, value)| *value)
}

/// How the swing of a damage window reaches (Inferred: the joint the shape hangs on is Unknown
/// without the creature's skeleton): the shape reaches its radius plus its length (twice the
/// capsule's half height) in front of the body; the swing lunges toward its target as far as
/// the entry's `Homing` `Radius` and turns to a target within its `MaxAngle` (Observed values:
/// the boar's gore homing radius 5, angle 180). Without a homing clip it does not lunge.
fn window_reach(ai: &MobAi, window: &ProcWindow, homing: Option<&ProcWindow>) -> MobReach {
    let number = |clip: &ProcWindow, name: Option<NameId>| {
        param_of(clip, name).and_then(|value| ai.name(value).trim().parse::<f32>().ok())
    };
    let radius = number(window, ai.names.shape_radius).unwrap_or(0.0);
    let half_height = number(window, ai.names.shape_half_height).unwrap_or(0.0);
    let (lunge_max_m, lunge_turn_rad) = homing.map_or((0.0, 0.0), |homing| {
        (
            number(homing, ai.names.homing_radius).unwrap_or(0.0),
            number(homing, ai.names.homing_angle)
                .unwrap_or(0.0)
                .to_radians(),
        )
    });
    MobReach {
        lunge_max_m,
        lunge_turn_rad,
        reach_m: radius + CAPSULE_ENDS * half_height,
    }
}

/// A capsule's length is twice its half height.
const CAPSULE_ENDS: f32 = 2.0;

fn position_of(point: [f32; 3]) -> Position {
    Position {
        x: point[0],
        y: point[1],
        z: point[2],
    }
}

fn vec3(at: Position) -> Value {
    Value::Vec3([at.x, at.y, at.z])
}

/// A blackboard with nothing set (a navigation config's own pick reads only its literals).
struct NoBoard;

impl Blackboard for NoBoard {
    fn value(&self, _key: KeyId) -> Option<Value> {
        None
    }

    fn dynamic_scale(&self) -> Option<f64> {
        None
    }
}

/// The world as one mob's tree reads it.
struct View<'a> {
    ai: &'a MobAi,
    state: &'a MindState,
    at: Position,
    now_ms: u64,
    passive: bool,
    target: Option<TargetView>,
    candidates: u32,
    health_share: f32,
    ground: Option<&'a (dyn Ground + Sync)>,
    /// The random numbers of position picks (the tree's own draws are in its brain).
    picks: Cell<Rng48>,
}

impl View<'_> {
    fn draw(&self) -> f32 {
        let mut rng = self.picks.get();
        let unit = rng.unit();
        self.picks.set(rng);
        unit
    }

    fn max_slope(&self) -> Option<f32> {
        self.ai.arch(self.state.archetype)?.max_slope_deg
    }
}

impl World for View<'_> {
    fn now_ms(&self) -> u64 {
        self.now_ms
    }

    fn engine_value(&self, key: EngineKey) -> Option<Value> {
        let state = self.state;
        let target = self.target;
        Some(match key {
            EngineKey::HasTarget => Value::Bool(target.is_some()),
            EngineKey::TargetDist => Value::Float(target?.distance_m),
            EngineKey::TargetAngle => Value::Float(target?.angle_deg),
            EngineKey::TargetPos | EngineKey::TargetLastKnownPos => vec3(target?.position),
            EngineKey::TargetVisible => Value::Bool(target.is_some_and(|target| target.visible)),
            EngineKey::TargetReachable | EngineKey::TargetDirectlyReachable => {
                Value::Bool(target.is_some_and(|target| target.reachable))
            }
            EngineKey::TargetVitalsState => {
                target?;
                Value::Int(ALIVE_VITALS_STATE)
            }
            EngineKey::VitalsState => Value::Int(ALIVE_VITALS_STATE),
            EngineKey::TimeSinceTargetReachable => {
                target?;
                let since = state
                    .reachable_ms
                    .map_or(self.now_ms, |at| self.now_ms.saturating_sub(at));
                // Seconds of a fight.
                #[allow(clippy::cast_precision_loss, reason = "seconds of a fight")]
                Value::Float(since as f32 / MS_PER_S)
            }
            EngineKey::HealthPercent => Value::Float(self.health_share * PERCENT),
            EngineKey::SpawnPos => vec3(state.spawn),
            EngineKey::LastDamagePos => vec3(state.last_damage?),
            EngineKey::FullBodyReaction => Value::Bool(state.reaction_until_ms.is_some()),
            EngineKey::Passive => Value::Int(i64::from(self.passive)),
            EngineKey::WaypointPos => vec3(state.route.as_ref()?.point()?),
            EngineKey::WaypointDuration => Value::Float(state.route.as_ref()?.wait_s()),
            EngineKey::WaypointTask => Value::Text(state.route.as_ref()?.task()?.task),
            EngineKey::WaypointDurationMin => {
                Value::Float(state.route.as_ref()?.task()?.duration_s[0])
            }
            EngineKey::WaypointDurationMax => {
                Value::Float(state.route.as_ref()?.task()?.duration_s[1])
            }
            EngineKey::WaypointAlign => Value::Bool(state.route.as_ref()?.task()?.align),
            EngineKey::LeashOptOut => {
                let opt_out = self
                    .ai
                    .config_number(state.archetype, LEASH_OPT_OUT_PARAM)
                    .unwrap_or(0.0);
                // An opt-out of 0 or 1.
                #[allow(clippy::cast_possible_truncation, reason = "an opt-out of 0 or 1")]
                Value::Int(opt_out.round() as i64)
            }
            EngineKey::DifficultyTier => {
                let tier = self
                    .ai
                    .config_number(state.archetype, DIFFICULTY_TIER_PARAM)?;
                // A tier of 0 to 3.
                #[allow(clippy::cast_possible_truncation, reason = "a tier of 0 to 3")]
                Value::Int(tier.round() as i64)
            }
            _ => return None,
        })
    }

    fn has_status_effect(&self, effect: &str) -> bool {
        self.state
            .statuses
            .iter()
            .any(|status| self.ai.name(*status) == effect)
    }

    fn has_status_category(&self, _category: &str) -> bool {
        // The sim reads no `EffectCategories` of the status effect rows: no category is
        // active.
        false
    }

    fn position(&self, of: PositionType) -> Option<[f32; 3]> {
        let state = self.state;
        let at = match of {
            PositionType::Own => self.at,
            PositionType::Target | PositionType::LastKnownTarget | PositionType::ActionTarget => {
                self.target?.position
            }
            PositionType::SpawnPosition => state.spawn,
            PositionType::LastDamagePosition => state.last_damage?,
            PositionType::WaypointPosition => state.route.as_ref()?.point()?,
            // Group, objective, nav link and above-start positions: the sim has no such data
            // (a selected position is a tree fact, read through the blackboard).
            PositionType::SelectedPosition
            | PositionType::GroupCenter
            | PositionType::AttackObjectivePosition
            | PositionType::GuardObjectivePosition
            | PositionType::AboveStartPosition
            | PositionType::NavLinkEndPosition => return None,
        };
        Some([at.x, at.y, at.z])
    }

    fn action_targets(&self, _params: &TrackerParams) -> u32 {
        // Inferred: the players the mob could take as targets within its combat vision (the
        // targeting profile's curves are Unknown).
        self.candidates
    }

    fn entity_query(&self, _params: &EntityQueryParams, _board: &dyn Blackboard) -> bool {
        // The rule of `EntityQueryCondition` is Unknown (study section 2.2): it never passes.
        false
    }

    fn entities_with_tag(&self, _tag: &str) -> u32 {
        // The sim gives its entities no `NWTags`: no entity has one.
        0
    }

    fn has_patrol(&self) -> bool {
        self.state.route.is_some()
    }

    fn line_of_sight(&self, _from: [f32; 3], _to: [f32; 3]) -> bool {
        // The server has no level geometry to block a line (Inferred: nothing blocks).
        true
    }

    fn nav_clearance(&self, direction_deg: f64, distance: f64, _from_wall: f64) -> bool {
        let Some(ground) = self.ground else {
            return true;
        };
        // Metres of a node.
        #[allow(clippy::cast_possible_truncation, reason = "metres of a node")]
        let end = ahead(
            self.at,
            turned(self.state.yaw, direction_deg),
            distance as f32,
        );
        floor_line(ground, self.at, end, self.max_slope()).is_some()
    }

    fn select_position(
        &self,
        params: &SelectPositionParams,
        board: &dyn Blackboard,
    ) -> Option<[f32; 3]> {
        let (origin, anchor) = (
            position_of(self.position(params.origin)?),
            position_of(self.position(params.anchor)?),
        );
        let number = |plain: &Param, keyed: &Param| {
            if keyed.is_unset() {
                plain.number(&board)
            } else {
                keyed.number(&board)
            }
        };
        let direction = params.direction.number(&board).unwrap_or(0.0);
        // A small whole number.
        #[allow(clippy::cast_possible_truncation, reason = "a small whole number")]
        let base = match direction as i64 {
            ANCHOR_TO_ORIGIN => yaw_toward(anchor, origin),
            ORIGIN_ORIENTATION => self.state.yaw,
            _ => yaw_toward(origin, anchor),
        };
        let start = number(&params.start_angle, &params.start_angle_param).unwrap_or(0.0);
        let stop = number(&params.stop_angle, &params.stop_angle_param).unwrap_or(0.0);
        let span = (stop - start).rem_euclid(FULL_TURN_DEG);
        let range_min = number(&params.range_min, &params.range_min_param).unwrap_or(0.0);
        let range_max = number(&params.range_max, &params.range_max_param).unwrap_or(0.0);
        let attempts = number(&params.max_attempts, &params.max_attempts_param)
            .unwrap_or(1.0)
            .max(1.0);
        // A few attempts.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a few attempts"
        )]
        let attempts = attempts as u32;
        for _ in 0..attempts {
            let angle = start + f64::from(self.draw()) * span;
            let range = range_min + f64::from(self.draw()) * (range_max - range_min);
            // Metres of a pick.
            #[allow(clippy::cast_possible_truncation, reason = "metres of a pick")]
            let point = ahead(origin, turned(base, angle), range as f32);
            let Some(ground) = self.ground else {
                return Some([point.x, point.y, point.z]);
            };
            if let Some(reached) = floor_line(ground, self.at, point, self.max_slope())
                .filter(|reached| reached.horizontal_distance(point) < FLOOR_STEP_M)
            {
                return Some([reached.x, reached.y, reached.z]);
            }
        }
        None
    }
}

/// A small test AI (feature `test-util`): one archetype whose tree walks home in a leash reset
/// when it is farther than its leash from its spawn, fights a target in reach, runs to a target
/// out of reach, and else stands.
#[cfg(any(test, feature = "test-util"))]
pub mod test_support {
    use nw_ai::build::{Build, run_config};
    use nw_ai::data::{
        AiData, Creature, FragmentLength, ProcWindow, ProjectileSlice, RangedAttack, Vision,
    };
    use nw_ai::node::PositionType;
    use nw_ai::value::{CompareOp, Value};
    use nw_content::player_attacks::Flight;
    use nw_formats::objectstream::lowercase_crc32;

    use super::{
        AiRates, COMBAT_LEASH_KEY, DAMAGE_CLIP, DAMAGE_ROW_PARAM, DEATH_ALIAS, HOMING_ANGLE_PARAM,
        HOMING_CLIP, HOMING_RADIUS_PARAM, LookAssets, MobAi, REACTION_ALIAS, RUN_CONFIG,
        SHAPE_HALF_HEIGHT_PARAM, SHAPE_RADIUS_PARAM,
    };

    /// The slice asset of the test archetype.
    pub const TEST_ASSET: [u8; 16] = [0x7e; 16];
    /// The test tree attacks a target this near.
    pub const TEST_REACH_M: f32 = 3.0;
    /// It sees a player this far, all around.
    pub const TEST_SIGHT_M: f32 = 20.0;
    const FULL_CIRCLE_DEG: f32 = 360.0;
    /// Its combat leash.
    pub const TEST_LEASH_M: f32 = 50.0;
    /// It shares a target it takes with mobs this near, at most this many.
    pub const TEST_GROUP_AGGRO_M: f32 = 10.0;
    /// The most mobs it shares a target with.
    pub const TEST_GROUP_AGGRO_CAP: u32 = 2;
    /// Its attack: the task, the length, the cooldown after it and its damage window.
    pub const TEST_ATTACK_TASK: &str = "Attack_Task";
    /// The length of its attack.
    pub const TEST_ATTACK_S: f32 = 1.5;
    /// The cooldown after its attack.
    pub const TEST_COOLDOWN_S: f32 = 2.0;
    /// When its damage window comes after the attack starts.
    pub const TEST_HIT_S: f32 = 0.5;
    /// The damage table and row of its hits.
    pub const TEST_MOB_TABLE: &str = "TestMobTable";
    /// The row of its hits.
    pub const TEST_MOB_ROW: &str = "Test_Mob_Attack";
    /// Its homing: radius and angle.
    pub const TEST_HOMING_RADIUS_M: &str = "2";
    const TEST_HOMING_ANGLE_DEG: &str = "180";
    const TEST_SHAPE_RADIUS_M: &str = "0.4";
    const TEST_SHAPE_HALF_HEIGHT_M: &str = "0.2";
    const TEST_HIT_LENGTH_S: f32 = 0.2;
    /// Its run, its combat idle and its idle tasks.
    pub const TEST_RUN_TASK: &str = "Move_Combat_Task";
    /// Its combat idle task.
    pub const TEST_COMBAT_IDLE_TASK: &str = "Combat_Idle_Task";
    /// Its idle task.
    pub const TEST_IDLE_TASK: &str = "Idle_Task";
    /// Its walk home in a leash reset.
    pub const TEST_RESET_TASK: &str = "Reset_Move_Task";
    const TEST_REACTION_S: f32 = 0.8;
    const TEST_DEATH_S: f32 = 2.0;
    const TRUE: f32 = 1.0;
    /// A ranged attack of the data: its profile, its name and its projectile slice.
    pub const TEST_RANGED_PROFILE: &str = "AI_Test";
    /// The name of the test ranged attack.
    pub const TEST_RANGED_ATTACK: &str = "Test_Shot";
    /// The projectile slice of the test ranged attack.
    pub const TEST_SHOT_SLICE: &str = "slices/characters/spellslices/test_shot.dynamicslice";
    /// The test ranged attack's range and radius.
    pub const TEST_SHOT_RANGE_M: f32 = 10.0;
    /// The radius of the test projectile.
    pub const TEST_SHOT_RADIUS_M: f32 = 0.5;
    /// The test projectile flies straight at this speed.
    pub const TEST_SHOT_SPEED_MPS: f32 = 20.0;
    const TEST_SHOT_LIFETIME_S: f32 = 5.0;

    /// The data of the test archetype.
    pub fn test_ai_data() -> AiData {
        let mut build = Build::default();
        let damage = build.name(DAMAGE_CLIP);
        let homing = build.name(HOMING_CLIP);
        let param =
            |build: &mut Build, name: &str, value: &str| (build.name(name), build.name(value));
        let windows = vec![
            ProcWindow {
                kind: homing,
                start_s: 0.0,
                end_s: Some(TEST_HIT_S),
                params: vec![
                    param(&mut build, HOMING_RADIUS_PARAM, TEST_HOMING_RADIUS_M),
                    param(&mut build, HOMING_ANGLE_PARAM, TEST_HOMING_ANGLE_DEG),
                ],
            },
            ProcWindow {
                kind: damage,
                start_s: TEST_HIT_S,
                end_s: Some(TEST_HIT_S + TEST_HIT_LENGTH_S),
                params: vec![
                    param(&mut build, DAMAGE_ROW_PARAM, TEST_MOB_ROW),
                    param(&mut build, SHAPE_RADIUS_PARAM, TEST_SHAPE_RADIUS_M),
                    param(
                        &mut build,
                        SHAPE_HALF_HEIGHT_PARAM,
                        TEST_SHAPE_HALF_HEIGHT_M,
                    ),
                ],
            },
        ];
        build.action(
            TEST_ATTACK_TASK,
            FragmentLength::Seconds(TEST_ATTACK_S + nw_ai::build::COMPLETION_OFFSET_S),
            Vec::new(),
            windows,
        );
        let attack_params = build.cage_params(TEST_ATTACK_TASK);
        let attack = build.node(nw_ai::node::NodeKind::Cage(Box::new(attack_params)));
        let cooled = build.cooldown(
            super::ATTACK_COOLDOWN,
            TEST_COOLDOWN_S,
            TEST_COOLDOWN_S,
            attack,
        );
        let near = build.gate(
            "Target_Dist",
            CompareOp::LessThanOrEqual,
            TEST_REACH_M,
            cooled,
        );
        let run = build.navigation(TEST_RUN_TASK, Some(RUN_CONFIG));
        let far = build.gate("Target_Dist", CompareOp::GreaterThan, TEST_REACH_M, run);
        let combat_idle = build.idle(TEST_COMBAT_IDLE_TASK);
        let fight = build.selector(&[near, far, combat_idle]);
        let combat = build.gate("Has_Target", CompareOp::Equal, TRUE, fight);
        let leash_key = build.key(COMBAT_LEASH_KEY);
        let home = build.navigation_to(
            TEST_RESET_TASK,
            Some(RUN_CONFIG),
            PositionType::SpawnPosition,
        );
        // The walk home finishes before the tree plans again (as DistanceLeash's).
        build.data.nodes[home as usize].interruptable = false;
        let reset = build.status_effect(super::RESET_EFFECT, home);
        let leash_param = nw_ai::value::Param {
            key: Some(leash_key),
            value: Some(Value::Float(TEST_LEASH_M)),
        };
        let leash = build.farther_than("Spawn_Pos", leash_param, reset);
        let idle = build.idle(TEST_IDLE_TASK);
        let root = build.selector(&[leash, combat, idle]);
        build.action(
            REACTION_ALIAS,
            FragmentLength::Seconds(TEST_REACTION_S),
            Vec::new(),
            Vec::new(),
        );
        build.action(
            DEATH_ALIAS,
            FragmentLength::Seconds(TEST_DEATH_S),
            Vec::new(),
            Vec::new(),
        );
        let run_kind = build.name(RUN_CONFIG);
        // The keys the design overrides set, as the retail trees read them.
        for key in [
            super::IS_PASSIVE_KEY,
            super::ENABLE_WANDER_KEY,
            super::WANDER_MAX_KEY,
            super::AMBIENT_LEASH_KEY,
        ] {
            build.key(key);
        }

        let table = build.name(TEST_MOB_TABLE);
        let ranged_attacks = vec![RangedAttack {
            profile: build.name(TEST_RANGED_PROFILE),
            attack: build.name(TEST_RANGED_ATTACK),
            range_m: TEST_SHOT_RANGE_M,
            radius_m: TEST_SHOT_RADIUS_M,
            hit_scan: false,
        }];
        let projectile_slices = vec![ProjectileSlice {
            slice: build.name(TEST_SHOT_SLICE),
            flight: Flight {
                base_speed_mps: TEST_SHOT_SPEED_MPS,
                extra_speed_mps: 0.0,
                time_for_max_speed_s: 0.0,
                gravity_mps2: 0.0,
                lifetime_s: TEST_SHOT_LIFETIME_S,
            },
        }];
        let mut data = build.finish(root);
        data.ranged_attacks = ranged_attacks;
        data.projectile_slices = projectile_slices;
        let reaction = data.name_id(REACTION_ALIAS).expect("reaction");
        let death = data.name_id(DEATH_ALIAS).expect("death");
        let mut aliases = vec![(reaction, reaction), (death, death)];
        aliases.sort_unstable();
        data.grids[0].aliases = aliases;
        let arch = &mut data.archetypes[0];
        let vision = Vision {
            angle_deg: FULL_CIRCLE_DEG,
            distance_m: TEST_SIGHT_M,
        };
        arch.ambient_vision = Some(vision);
        arch.nav_configs.push(run_config(run_kind));
        arch.config.push((leash_key, Value::Float(TEST_LEASH_M)));
        arch.damage_table_id = Some(table);
        arch.group_aggro_distance = Some(TEST_GROUP_AGGRO_M);
        arch.group_aggro_cap = Some(TEST_GROUP_AGGRO_CAP);
        data.creatures.push(Creature {
            asset: TEST_ASSET,
            archetype: 0,
        });
        data
    }

    /// Picks that give their origin exactly (every field a test does not set).
    pub fn exact_pick() -> nw_ai::node::SelectPositionParams {
        let zero = nw_ai::value::Param::literal(Value::Float(0.0));
        let unset = nw_ai::value::Param::UNSET;
        nw_ai::node::SelectPositionParams {
            origin: PositionType::Own,
            anchor: PositionType::Own,
            direction: unset,
            mode: unset,
            check_path_to_origin: false,
            check_path_to_anchor: false,
            start_angle: zero,
            stop_angle: zero,
            favor_middle_angle: false,
            range_min: zero,
            range_max: zero,
            distance_from_wall: unset,
            query_radius: unset,
            max_attempts: unset,
            start_angle_param: unset,
            stop_angle_param: unset,
            range_min_param: unset,
            range_max_param: unset,
            distance_from_wall_param: unset,
            query_radius_param: unset,
            max_attempts_param: unset,
            group_detector: unset,
            group_range: unset,
        }
    }

    /// The test AI with the retail tick rates; the mobs of `vitals_ids` show the test slice.
    pub fn test_ai(vitals_ids: &[&str]) -> MobAi {
        let looks: LookAssets = vitals_ids
            .iter()
            .map(|id| (lowercase_crc32(id), TEST_ASSET))
            .collect();
        MobAi::new(test_ai_data(), AiRates::default(), looks)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nw_ai::data::ProcWindow;
    use nw_sim_core::test_support::FlatGround;
    use nw_world::records::SpawnAi as DesignSpawn;

    use super::test_support::{TEST_ASSET, test_ai, test_ai_data};
    use super::*;

    const GROUND_Z: f32 = 10.0;
    const START: Position = Position {
        x: 100.0,
        y: 100.0,
        z: GROUND_Z,
    };
    const TEST_MOB: &str = "Test_Mob";

    /// A ground that rises `rise` metres per metre east of `START`.
    struct Ramp {
        rise: f32,
    }

    impl Ground for Ramp {
        fn height_at(&self, x: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z + (x - START.x).max(0.0) * self.rise)
        }
    }

    fn east(metres: f32) -> Position {
        Position {
            x: START.x + metres,
            ..START
        }
    }

    #[test]
    fn the_retail_rates_pick_the_band_of_the_nearest_player() {
        let rates = AiRates::default();
        assert!(rates.is_valid());
        assert_eq!(rates.interval_ms(Activity::Combat, 100.0), 33);
        assert_eq!(rates.interval_ms(Activity::Idle, 1.0), 333);
        assert_eq!(rates.interval_ms(Activity::Moving, 19.9), 98);
        assert_eq!(rates.interval_ms(Activity::Moving, 20.0), 218);
        assert_eq!(rates.interval_ms(Activity::Moving, f32::INFINITY), 333);
        let falling = AiRates {
            moving_range_m: [40.0, 20.0],
            ..rates
        };
        assert!(!falling.is_valid());
        assert!(
            !AiRates {
                combat_interval_ms: 0,
                ..rates
            }
            .is_valid()
        );
    }

    #[test]
    fn a_mob_shows_its_spawns_slice_or_its_vitals_look() {
        let ai = test_ai(&[TEST_MOB]);
        let vitals = nw_formats::objectstream::lowercase_crc32(TEST_MOB);
        assert_eq!(ai.archetype_for(vitals, None), Some(0));
        assert_eq!(
            ai.archetype_for(vitals, Some([1; 16])),
            None,
            "an unknown slice"
        );
        assert_eq!(ai.archetype_for(vitals + 1, Some(TEST_ASSET)), Some(0));
        assert_eq!(ai.archetype_for(vitals + 1, None), None);
        assert!(ai.has_tree(0));
        assert!(!MobAi::empty().has_tree(0));
    }

    #[test]
    fn a_line_stands_on_the_floor_within_the_slope() {
        let gentle = Ramp { rise: 0.5 };
        let end = floor_line(&gentle, START, east(10.0), Some(45.0)).expect("a 27 degree ramp");
        assert!((end.z - (GROUND_Z + 5.0)).abs() < 1e-3, "{end:?}");
        assert_eq!(
            floor_line(&Ramp { rise: 2.0 }, START, east(10.0), Some(45.0)),
            None,
            "a 63 degree ramp is too steep"
        );
        assert!(
            floor_line(&Ramp { rise: 1.0 }, START, east(10.0), None).is_some(),
            "without a slope limit a 45 degree ramp rises 0.5 m a step"
        );
        assert_eq!(
            floor_line(&Ramp { rise: 2.0 }, START, east(10.0), None),
            None,
            "1 m a step is over the retail climb"
        );
        let deck = Position {
            z: GROUND_Z + MAX_STEP_DOWN_M + 1.0,
            ..START
        };
        let on_deck =
            floor_line(&FlatGround(GROUND_Z), deck, east(5.0), Some(45.0)).map(|end| end.z);
        assert_eq!(
            on_deck,
            Some(deck.z),
            "a deck above the height map keeps its height"
        );
    }

    /// A deck the floor holds east of `START` up to [`DECK_END_M`], [`DECK_HEIGHT_M`] above the
    /// ground; the ground lies under it.
    struct Deck;

    const DECK_END_M: f32 = 3.0;
    const DECK_HEIGHT_M: f32 = MAX_STEP_DOWN_M + 4.0;

    impl Ground for Deck {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z)
        }

        fn floor_at(&self, x: f32, _: f32, z: f32) -> Option<f32> {
            let deck = GROUND_Z + DECK_HEIGHT_M;
            Some(if x - START.x <= DECK_END_M && z >= deck - ON_FLOOR_M {
                deck
            } else {
                GROUND_Z
            })
        }
    }

    /// Flat ground with a wall across the line from 2 m to 3 m east of [`START`].
    struct Wall;

    impl Ground for Wall {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z)
        }

        fn blocked(&self, from: Position, to: Position) -> bool {
            let near = from.x.min(to.x) - START.x;
            let far = from.x.max(to.x) - START.x;
            far >= 2.0 && near <= 3.0
        }
    }

    #[test]
    fn a_line_stops_at_a_wall() {
        // Live 2026-10-29: mobs walked into rock and a crate; the line tested the floor only.
        assert!(
            floor_line(&Wall, START, east(1.5), None).is_some(),
            "short of it"
        );
        assert_eq!(
            floor_line(&Wall, START, east(5.0), None),
            None,
            "through it"
        );
    }

    /// Flat ground with a rock from 2 m to 3 m east of [`START`], [`ROCK_HALF_WIDTH_M`] to each
    /// side of the line.
    struct Rock;

    const ROCK_HALF_WIDTH_M: f32 = 1.5;

    impl Ground for Rock {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z)
        }

        fn blocked(&self, from: Position, to: Position) -> bool {
            let near = from.x.min(to.x) - START.x;
            let far = from.x.max(to.x) - START.x;
            let beside = |at: Position| (at.y - START.y).abs() <= ROCK_HALF_WIDTH_M;
            far >= 2.0 && near <= 3.0 && beside(from) && beside(to)
        }
    }

    #[test]
    fn a_target_behind_a_rock_is_reached_around_it() {
        // Live 2026-10-29: Janszoon stood blocked by a rock 2 m from his spawn.
        assert_eq!(floor_line(&Rock, START, east(5.0), None), None, "straight");
        assert!(reaches(&Rock, START, east(5.0), None), "around it");
        assert!(
            !reaches(&Wall, START, east(5.0), None),
            "a long wall stays a wall"
        );
    }

    #[test]
    fn a_move_into_a_rock_turns_along_it() {
        const STEP_M: f32 = 0.5;
        let before = east(1.75);
        let into = east(1.75 + STEP_M);
        let moved = floor_move(&Rock, before, into, None).expect("a free turn");
        assert!(moved.x - START.x < 2.0, "not into the rock");
        assert!(
            (moved.horizontal_distance(before) - STEP_M).abs() < 0.01,
            "the same step length"
        );
        assert_eq!(
            floor_move(&Rock, START, east(1.0), None),
            Some(east(1.0)),
            "straight"
        );
    }

    #[test]
    fn a_mob_on_a_floor_does_not_walk_off_a_high_edge() {
        let on_deck = Position {
            z: GROUND_Z + DECK_HEIGHT_M,
            ..START
        };
        assert!(
            floor_line(&Deck, on_deck, east(DECK_END_M - 1.0), None).is_some(),
            "along the deck"
        );
        assert_eq!(
            floor_line(&Deck, on_deck, east(DECK_END_M + 2.0), None),
            None,
            "off its edge, 7 m above the ground"
        );
    }

    #[test]
    fn a_design_profile_becomes_overrides_of_the_slice_values() {
        const LEASH_M: f32 = 30.0;
        const WANDER_M: f32 = 4.0;
        const INTERVAL_MS: u64 = 2_500;
        const RESPAWN_MS: u64 = 9_000;
        let ai = test_ai(&[]);
        let key = |name: &str| ai.data().key(name).expect("key");
        let world = WorldFile {
            ai_profiles: BTreeMap::from([(
                "guards".to_owned(),
                AiProfile {
                    behaviour: AiBehaviour::Guard,
                    leash_radius_m: Some(LEASH_M),
                    wander_radius_m: Some(WANDER_M),
                    attack_interval_ms: Some(INTERVAL_MS),
                    respawn_ms: Some(RESPAWN_MS),
                    ..AiProfile::default()
                },
            )]),
            paths: BTreeMap::from([(
                "walk".to_owned(),
                PatrolPath {
                    points: vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
                    waits_ms: vec![1_500],
                    mode: PathMode::BackAndForth,
                    tasks: Vec::new(),
                },
            )]),
            vitals_ai: vec![
                DesignSpawn {
                    profile: Some("guards".to_owned()),
                    path: None,
                },
                DesignSpawn::default(),
                DesignSpawn {
                    profile: None,
                    path: Some("walk".to_owned()),
                },
            ],
            ..WorldFile::default()
        };
        let resolved = resolve_ai(&world, &ai);
        let guard = resolved[0].as_ref().expect("a profile");
        assert!(
            guard
                .facts
                .contains(&(key(ENABLE_WANDER_KEY), Value::Bool(false)))
        );
        assert!(
            guard
                .facts
                .contains(&(key(AMBIENT_LEASH_KEY), Value::Float(LEASH_M)))
        );
        assert!(
            guard
                .facts
                .contains(&(key(COMBAT_LEASH_KEY), Value::Float(LEASH_M)))
        );
        assert!(
            guard
                .facts
                .contains(&(key(WANDER_MAX_KEY), Value::Float(WANDER_M)))
        );
        assert_eq!(guard.attack_cooldown_s, Some(2.5));
        assert_eq!(guard.respawn_ms, Some(RESPAWN_MS));
        assert_eq!(resolved[1], None, "no profile, no path");
        let route = resolved[2]
            .as_ref()
            .and_then(|spawn| spawn.route.clone())
            .expect("a patrol");
        assert_eq!(route.points.len(), 2);
        assert_eq!(route.mode, RouteMode::BackAndForth);
    }

    #[test]
    fn a_route_goes_on_as_its_mode_says() {
        let walk = |mode| RouteWalk {
            route: Arc::new(Route {
                points: vec![START, east(1.0), east(2.0)],
                waits_ms: vec![2_000],
                mode,
                tasks: Vec::new(),
            }),
            next: 0,
            backwards: false,
            task_s: 0.0,
        };
        let visits = |mut walk: RouteWalk| {
            (0..5)
                .map(|_| {
                    walk.advance();
                    walk.next
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(visits(walk(RouteMode::Loop)), [1, 2, 0, 1, 2]);
        assert_eq!(visits(walk(RouteMode::BackAndForth)), [1, 2, 1, 0, 1]);
        assert_eq!(visits(walk(RouteMode::Once)), [1, 2, 2, 2, 2]);
        assert!((walk(RouteMode::Loop).wait_s() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_waypoint_task_stays_its_drawn_time_or_plays_once_without_one() {
        // The retail beach patrol spawner's graze waypoint (15 to 45 s) and the graze
        // spawner's (-1: no duration).
        const GRAZE: NameId = 7;
        let task = |duration_s| RouteTask {
            task: GRAZE,
            duration_s,
            align: false,
        };
        let mut walk = RouteWalk {
            route: Arc::new(Route {
                points: vec![START, east(1.0)],
                waits_ms: vec![2_000, 2_000],
                mode: RouteMode::Loop,
                tasks: vec![Some(task([15.0, 45.0])), Some(task([-1.0, -1.0]))],
            }),
            next: 0,
            backwards: false,
            task_s: 0.0,
        };
        walk.visit(0.5);
        assert!(
            (walk.wait_s() - 30.0).abs() < 1e-4,
            "the task's time, not the wait"
        );
        assert_eq!(walk.task().map(|task| task.task), Some(GRAZE));
        walk.advance();
        walk.visit(0.5);
        assert!(walk.wait_s() < 0.0, "no duration: the single task");
    }

    #[test]
    fn a_damage_window_reaches_by_its_shape_and_lunges_by_its_homing() {
        let mut data = test_ai_data();
        let mut name = |text: &str| {
            data.name_id(text).unwrap_or_else(|| {
                data.names.push(text.to_owned());
                (data.names.len() - 1) as NameId
            })
        };
        let damage = ProcWindow {
            kind: name(DAMAGE_CLIP),
            start_s: 0.7,
            end_s: Some(0.9),
            params: vec![
                (name(SHAPE_RADIUS_PARAM), name("0.40000001")),
                (name(SHAPE_HALF_HEIGHT_PARAM), name("0.2")),
            ],
        };
        let homing = ProcWindow {
            kind: name(HOMING_CLIP),
            start_s: 0.0,
            end_s: None,
            params: vec![
                (name(HOMING_RADIUS_PARAM), name("5")),
                (name(HOMING_ANGLE_PARAM), name("180")),
            ],
        };
        let ai = MobAi::new(data, AiRates::default(), LookAssets::new());
        // The boar gore's clips (`npc_boar_anims.adb`): radius 0.4, half height 0.2, homing
        // radius 5 and angle 180.
        let reach = window_reach(&ai, &damage, Some(&homing));
        assert!((reach.reach_m - 0.8).abs() < 1e-6);
        assert!((reach.lunge_max_m - 5.0).abs() < 1e-6);
        assert!((reach.lunge_turn_rad - std::f32::consts::PI).abs() < 1e-6);
        let still = window_reach(&ai, &damage, None);
        assert_eq!((still.lunge_max_m, still.lunge_turn_rad), (0.0, 0.0));
    }

    #[test]
    fn a_ring_pick_lands_within_its_range_and_angles() {
        const RANGE: (f32, f32) = (3.0, 6.0);
        const PICKS: u64 = 200;
        let ai = test_ai(&[]);
        let mind = TreeMind::new(&ai, 0, START, 0.0, 1, None).expect("a tree");
        let ground = FlatGround(GROUND_Z);
        let value = |metres: f32| Param::literal(Value::Float(metres));
        // The wander's straight pick: from the spawn, away from the mob, 310 to 50 degrees.
        let params = SelectPositionParams {
            origin: PositionType::SpawnPosition,
            anchor: PositionType::Own,
            direction: Param::literal(Value::Int(ANCHOR_TO_ORIGIN)),
            start_angle_param: value(310.0),
            stop_angle_param: value(50.0),
            range_min_param: value(RANGE.0),
            range_max_param: value(RANGE.1),
            max_attempts_param: Param::literal(Value::Int(5)),
            ..test_support::exact_pick()
        };
        let at = Position {
            y: START.y - 1.0,
            ..START
        };
        for seed in 0..PICKS {
            let mut view = mind.state.view(&ai, at, 0, false);
            view.ground = Some(&ground);
            view.picks = Cell::new(Rng48::new(seed));
            let point = view
                .select_position(&params, &NoBoard)
                .map(position_of)
                .expect("a point");
            let distance = point.horizontal_distance(START);
            assert!((RANGE.0..=RANGE.1 + 1e-3).contains(&distance), "{distance}");
            // Away from the mob (south of the spawn): north of it, within 50 degrees.
            let bearing = yaw_toward(START, point).to_degrees();
            assert!(bearing.abs() <= 50.0 + 1e-3, "{bearing}");
            assert_eq!(point.z, GROUND_Z);
        }
    }
}
