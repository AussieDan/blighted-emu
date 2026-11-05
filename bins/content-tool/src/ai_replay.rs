// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replays retail mob fights through the behavior-tree runtime (`nw_ai`) and compares the tasks
//! it starts with the ones the retail mob played (capture `newchar-retail-2026-09-18`, traced by
//! [`crate::ai_trace`]).
//!
//! The replay is open-loop: the mob's position and facing and the player's position come from the
//! capture, so the test checks the tree's decisions and timing, not movement. What the replay
//! world gives the tree (the sim's job, written down here):
//! - `Target_Dist`: the horizontal distance mob to player (Inferred: the trees' 3 m and 7 m bands
//!   fit the horizontal distances of the retail attack starts, 1.69 to 5.30 m).
//! - `Target_Angle`: the signed angle from the mob's facing to the player, in degrees.
//! - `Has_Target`, `Target_Visible`, `Target_Reachable`, `Target_Directly_Reachable`: true from the
//!   fight's first entry (the player hit the mob in melee range).
//! - `Target_Vitals_State`: 4, alive (Inferred: the trees name the test `$Target_Vitals_State
//!   Equal 4` "HasAliveTarget", 154 times; the player did not die in these fights).
//! - `Full_Body_Reaction`: true while the retail mob played an action of grid alias `Reaction`;
//!   such an action and a `Death` reach the brain as engine actions (they cancel the plan and set
//!   their Mannequin tags, `Alerted` for the boar).
//! - A move ends where the retail move ended (the next retail entry): the navmesh and arrival
//!   are the sim's.
//! - Every other engine key is not set.
//!
//! A start counts as matched when the tree started the same fragment within
//! [`START_TOLERANCE_MS`] of the retail start. When the tree made a weighted random pick that
//! differs from retail's, the replay draws again with other seeds from the state before that
//! tick; a seed that gives retail's pick shows that the tree can make it, and the replay goes on
//! from there.

use nw_ai::brain::{Brain, Command, MotionOutcome, Runtime, Slot, World};
use nw_ai::data::{AiData, Grid};
use nw_ai::keys::EngineKey;
use nw_ai::node::{EntityQueryParams, NodeKind, PositionType, SelectPositionParams, TrackerParams};
use nw_ai::value::{Blackboard, NameId, Value};

use crate::ai_trace::{AiTrace, MobTrace};

/// The server tick (retail combat states every 33 to 34 ms, trace finding c).
pub const TICK_MS: u64 = 33;
/// How far a tree start may lie from the retail start: the measured lag between a computed
/// completion and the next retail entry was 32 to 94 ms (boar `Attack_Gore` 1,465 and 1,498 ms
/// against 1,404 ms; `Combat_Turn` 1,632 and 1,683 ms against 1,600 ms), plus one tick of
/// sampling.
pub const START_TOLERANCE_MS: u64 = 4 * TICK_MS;
/// Seeds tried when a weighted pick differs from retail's.
const SEEDS_TRIED: u64 = 64;
/// The grid aliases of the actions the engine starts and that cancel the plan.
const REACTION_ALIAS: &str = "Reaction";
const DEATH_ALIAS: &str = "Death";
/// `Target_Vitals_State` of a living target (see the module notes).
const ALIVE_VITALS_STATE: i64 = 4;
const DEGREES_PER_HALF_TURN: f32 = 180.0;
const FULL_TURN_DEG: f32 = 360.0;

/// One retail FullBody entry of the fight.
#[derive(Debug, Clone, PartialEq)]
pub struct RetailEntry {
    /// Its start.
    pub time_ms: u64,
    /// Its fragment.
    pub fragment: String,
    /// The CAGE action of that fragment in the mob's grid.
    pub action: Option<u32>,
    /// Its grid alias.
    pub alias: Option<String>,
}

/// One start of the tree in the replay.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeStart {
    /// When.
    pub time_ms: u64,
    /// The fragment of the task.
    pub fragment: String,
    /// The horizontal distance to the player then.
    pub distance_m: f32,
    /// The angle from the mob's facing to the player then (degrees).
    pub angle_deg: f32,
    /// Whether a redraw with another seed was needed to pick retail's choice.
    pub redrawn: bool,
}

/// The comparison of one fight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FightReport {
    /// The retail entries the tree drives (alias `BehaviorTreeTask`), in order.
    pub retail: Vec<RetailEntry>,
    /// The tree's starts, in order.
    pub tree: Vec<TreeStart>,
    /// Retail entries with no tree start of the same fragment within the tolerance: (retail
    /// entry, the tree's start nearest in time).
    pub unmatched: Vec<(RetailEntry, Option<TreeStart>)>,
}

/// The world of the replay at one time.
struct ReplayWorld {
    now: u64,
    own: [f32; 3],
    yaw: f32,
    target: [f32; 3],
    reacting: bool,
    spawn: [f32; 3],
}

impl ReplayWorld {
    fn distance(&self) -> f32 {
        (self.target[0] - self.own[0]).hypot(self.target[1] - self.own[1])
    }

    /// Yaw 0 faces +y and a positive yaw turns west (trace rule); the angle is positive when the
    /// player is on the mob's left.
    fn angle_deg(&self) -> f32 {
        let bearing = (-(self.target[0] - self.own[0])).atan2(self.target[1] - self.own[1]);
        let mut angle = (bearing - self.yaw).to_degrees();
        while angle > DEGREES_PER_HALF_TURN {
            angle -= FULL_TURN_DEG;
        }
        while angle <= -DEGREES_PER_HALF_TURN {
            angle += FULL_TURN_DEG;
        }
        angle
    }
}

impl World for ReplayWorld {
    fn now_ms(&self) -> u64 {
        self.now
    }

    fn engine_value(&self, key: EngineKey) -> Option<Value> {
        Some(match key {
            EngineKey::TargetDist => Value::Float(self.distance()),
            EngineKey::TargetAngle => Value::Float(self.angle_deg()),
            EngineKey::HasTarget
            | EngineKey::TargetVisible
            | EngineKey::TargetReachable
            | EngineKey::TargetDirectlyReachable => Value::Bool(true),
            EngineKey::FullBodyReaction => Value::Bool(self.reacting),
            EngineKey::TargetVitalsState => Value::Int(ALIVE_VITALS_STATE),
            EngineKey::SpawnPos => Value::Vec3(self.spawn),
            EngineKey::TargetPos => Value::Vec3(self.target),
            _ => return None,
        })
    }

    fn has_status_effect(&self, _effect: &str) -> bool {
        false
    }

    fn has_status_category(&self, _category: &str) -> bool {
        false
    }

    fn position(&self, of: PositionType) -> Option<[f32; 3]> {
        match of {
            PositionType::Own => Some(self.own),
            PositionType::Target | PositionType::ActionTarget | PositionType::LastKnownTarget => {
                Some(self.target)
            }
            PositionType::SpawnPosition => Some(self.spawn),
            _ => None,
        }
    }

    fn action_targets(&self, _params: &TrackerParams) -> u32 {
        1
    }

    fn entity_query(&self, _params: &EntityQueryParams, _board: &dyn Blackboard) -> bool {
        false
    }

    fn entities_with_tag(&self, _tag: &str) -> u32 {
        0
    }

    fn can_act(&self, _kind: &NodeKind, _target_index: Option<u32>) -> bool {
        // The replay compares the tree's own choices with a retail capture, where the retail
        // server ran each action leaf.
        true
    }

    fn has_patrol(&self) -> bool {
        false
    }

    fn line_of_sight(&self, _from: [f32; 3], _to: [f32; 3]) -> bool {
        true
    }

    fn nav_clearance(&self, _direction_deg: f64, _distance: f64, _from_wall: f64) -> bool {
        true
    }

    fn select_position(
        &self,
        _params: &SelectPositionParams,
        _board: &dyn Blackboard,
    ) -> Option<[f32; 3]> {
        Some(self.target)
    }
}

/// The value of `samples` at `time_ms`: the last one sent at or before it.
fn last_at<T: Copy>(samples: &[(u64, T)], time_ms: u64) -> Option<T> {
    let index = samples.partition_point(|(time, _)| *time <= time_ms);
    index.checked_sub(1).map(|index| samples[index].1)
}

fn name_of(data: &AiData, id: NameId) -> String {
    data.name(id).to_owned()
}

/// The replay of one mob of `trace` from `start_ms` to `end_ms` with archetype `archetype`.
///
/// # Panics
///
/// Panics when the archetype has no grid.
pub fn replay(
    data: &AiData,
    archetype: u32,
    trace: &AiTrace,
    mob: &MobTrace,
    start_ms: u64,
    end_ms: u64,
    seed: u64,
) -> FightReport {
    let runtime = Runtime::new(data.clone());
    let arch = &data.archetypes[archetype as usize];
    let grid: &Grid = &data.grids[arch.grid.expect("grid") as usize];
    let positions: Vec<(u64, [f32; 3])> = mob
        .samples
        .iter()
        .filter_map(|sample| Some((sample.time_ms, sample.position?)))
        .collect();
    let yaws: Vec<(u64, f32)> = mob
        .samples
        .iter()
        .filter_map(|sample| Some((sample.time_ms, sample.yaw?)))
        .collect();
    let player: Vec<(u64, [f32; 3])> = trace
        .player
        .iter()
        .map(|sample| (sample.time_ms, sample.position))
        .collect();
    let entries = retail_entries(data, grid, mob, start_ms, end_ms);
    let spawn = positions.first().map_or([0.0; 3], |(_, at)| *at);
    let mut brain = Brain::new(&runtime, archetype, seed).expect("brain");
    let mut report = FightReport {
        retail: tree_entries(grid, &entries),
        ..FightReport::default()
    };
    // The tree's move: its slot, its fragment, and whether the retail move of that fragment
    // started (the move ends with the retail entry after it).
    let mut moving: Option<(Slot, String, bool)> = None;
    let mut last_retail: Option<String> = None;
    let mut reaction: Option<u32> = None;
    let mut next_entry = 0;
    let mut now = start_ms;
    while now <= end_ms {
        let world = ReplayWorld {
            now,
            own: last_at(&positions, now).unwrap_or(spawn),
            yaw: last_at(&yaws, now).unwrap_or(0.0),
            target: last_at(&player, now).unwrap_or(spawn),
            reacting: reaction.is_some(),
            spawn,
        };
        let mut out = Vec::new();
        while let Some(entry) = entries.get(next_entry).filter(|entry| entry.time_ms <= now) {
            if let Some(action) = reaction.take() {
                brain.engine_action_end(&runtime, &world, action, &mut out);
            }
            let engine = matches!(entry.alias.as_deref(), Some(REACTION_ALIAS | DEATH_ALIAS));
            if let (true, Some(action)) = (engine, entry.action) {
                brain.engine_action(&runtime, &world, action, &mut out);
                reaction = (entry.alias.as_deref() == Some(REACTION_ALIAS)).then_some(action);
                moving = None;
            }
            if let Some((slot, fragment, retail_started)) = &mut moving {
                if *retail_started {
                    brain.report_motion(*slot, MotionOutcome::Arrived);
                    moving = None;
                } else if *fragment == entry.fragment {
                    *retail_started = true;
                }
            }
            last_retail = Some(entry.fragment.clone());
            next_entry += 1;
        }
        let world = ReplayWorld {
            reacting: reaction.is_some(),
            ..world
        };
        let before = brain.clone();
        out.clear();
        brain.tick(&runtime, &world, &mut out);
        let mut started = starts(data, grid, &out);
        if let Some((_, fragment, _)) = started.first()
            && let Some(wanted) = retail_near(&report.retail, now)
            && wanted.fragment != *fragment
        {
            for redraw in 1..=SEEDS_TRIED {
                let mut trial = before.clone();
                trial.reseed(seed.wrapping_add(redraw));
                let mut trial_out = Vec::new();
                trial.tick(&runtime, &world, &mut trial_out);
                let trial_started = starts(data, grid, &trial_out);
                if trial_started
                    .first()
                    .is_some_and(|(_, fragment, _)| *fragment == wanted.fragment)
                {
                    brain = trial;
                    out = trial_out;
                    started = trial_started;
                    started[0].2 = true;
                    break;
                }
            }
        }
        for (slot, fragment, redrawn) in started {
            if let Some(slot) = slot {
                let retail_started = last_retail.as_deref() == Some(fragment.as_str());
                moving = Some((slot, fragment.clone(), retail_started));
            }
            report.tree.push(TreeStart {
                time_ms: now,
                fragment,
                distance_m: world.distance(),
                angle_deg: world.angle_deg(),
                redrawn,
            });
        }
        now += TICK_MS;
    }
    for entry in &report.retail {
        let nearest = report
            .tree
            .iter()
            .filter(|start| start.fragment == entry.fragment)
            .min_by_key(|start| start.time_ms.abs_diff(entry.time_ms));
        if nearest.is_none_or(|start| start.time_ms.abs_diff(entry.time_ms) > START_TOLERANCE_MS) {
            let any = report
                .tree
                .iter()
                .min_by_key(|start| start.time_ms.abs_diff(entry.time_ms))
                .cloned();
            report.unmatched.push((entry.clone(), any));
        }
    }
    report
}

/// The retail FullBody entries of `mob` from `start_ms` to `end_ms`, with the CAGE action of
/// each fragment in `grid` and its grid alias.
fn retail_entries(
    data: &AiData,
    grid: &Grid,
    mob: &MobTrace,
    start_ms: u64,
    end_ms: u64,
) -> Vec<RetailEntry> {
    mob.samples
        .iter()
        .filter(|sample| (start_ms..=end_ms).contains(&sample.time_ms))
        .filter_map(|sample| {
            let fragment = sample.full_body.as_ref()?.fragment_name.clone()?;
            let fragment_id = data.name_id(&fragment);
            let action = grid
                .actions
                .iter()
                .map(|(_, action)| *action)
                .find(|action| Some(data.cage_actions[*action as usize].fragment) == fragment_id);
            let alias = action
                .map(|action| data.cage_actions[action as usize].action)
                .and_then(|name| grid.alias(name))
                .map(|alias| name_of(data, alias));
            Some(RetailEntry {
                time_ms: sample.time_ms,
                fragment,
                action,
                alias,
            })
        })
        .collect()
}

/// The entries of `entries` that a tree drives: not the engine's reactions and deaths, and of an
/// action that a task of `grid` plays.
fn tree_entries(grid: &Grid, entries: &[RetailEntry]) -> Vec<RetailEntry> {
    entries
        .iter()
        .filter(|entry| {
            entry.alias.as_deref() != Some(REACTION_ALIAS)
                && entry.alias.as_deref() != Some(DEATH_ALIAS)
                && entry
                    .action
                    .is_some_and(|action| grid.tasks.iter().any(|(_, known)| *known == action))
        })
        .cloned()
        .collect()
}

/// One start of the sim's mob in the in-sim replay.
#[derive(Debug, Clone, PartialEq)]
pub struct SimStart {
    /// When (capture ms).
    pub time_ms: u64,
    /// The fragment of the action it started.
    pub fragment: String,
    /// The horizontal distance to the player then.
    pub distance_m: f32,
}

/// A retail fight replayed in the sim: the retail tree entries and what the sim's mob did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimFight {
    /// The retail entries the tree drives, in order.
    pub retail: Vec<RetailEntry>,
    /// The sim mob's starts, in order.
    pub starts: Vec<SimStart>,
    /// Its fastest move between two ticks.
    pub top_speed_mps: f32,
}

/// The ground of the replay: the beach's heights are not read here (the mob keeps its height).
struct Unmapped;

impl nw_sim_core::Ground for Unmapped {
    fn height_at(&self, _x: f32, _y: f32) -> Option<f32> {
        None
    }
}

/// The in-sim replay of mob `mob` of `trace` from `start_ms` to `end_ms`: a sim mob of vitals id
/// `vitals_name` (its CRC must be the mob's), shown with the mob's slice, spawned where the
/// retail mob stood at the start, runs its tree in `sim::mobs::Mobs` (closed loop: it moves by
/// the sim's motion) against the retail player's positions; each retail reaction hurts it and
/// plays for its retail length.
///
/// # Panics
///
/// Panics when the sim config, the slice asset or the mob's grid is missing.
pub fn sim_replay(
    data: &std::sync::Arc<AiData>,
    trace: &AiTrace,
    mob: &MobTrace,
    vitals_name: &str,
    span_ms: (u64, u64),
) -> SimFight {
    use nw_content::combat::{CombatData, CombatRows, MobType};
    use nw_formats::objectstream::lowercase_crc32;
    use nw_sim_core::{Position, Seen};
    use sim::combat::test_support::{TEST_LEVEL, test_source};
    use sim::mob_ai::{AiRates, LookAssets, MobAi};
    use sim::mobs::{Mobs, VitalsSpawn, mob_entity};

    const PLAYER: u64 = 1;
    const SIM_CONFIG: &str = "config/sim.toml";
    const MS_PER_S: f32 = 1_000.0;
    let (start_ms, end_ms) = span_ms;
    assert_eq!(
        lowercase_crc32(vitals_name),
        mob.vitals_id,
        "the mob's vitals id"
    );
    let asset: [u8; 16] = hex::decode(&mob.asset)
        .expect("hex")
        .try_into()
        .expect("an asset id");
    let looks: LookAssets = [(mob.vitals_id, asset)].into_iter().collect();
    let ai = std::sync::Arc::new(MobAi::new(
        std::sync::Arc::clone(data),
        AiRates::default(),
        looks,
    ));
    let archetype = ai.archetype_for(mob.vitals_id, None).expect("archetype");
    let grid = &data.grids[data.archetypes[archetype as usize].grid.expect("grid") as usize];
    let retail = tree_entries(grid, &retail_entries(data, grid, mob, start_ms, end_ms));
    let reactions: Vec<(u64, u64)> = {
        let entries = retail_entries(data, grid, mob, start_ms, end_ms);
        entries
            .iter()
            .zip(entries.iter().skip(1))
            .filter(|(entry, _)| entry.alias.as_deref() == Some(REACTION_ALIAS))
            .map(|(entry, next)| (entry.time_ms, next.time_ms - entry.time_ms))
            .collect()
    };
    let position = |time_ms: u64| {
        mob.samples
            .iter()
            .filter(|sample| sample.time_ms >= time_ms)
            .find_map(|sample| sample.position)
            .expect("a retail position")
    };
    let [x, y, z] = position(start_ms);
    let spawn = VitalsSpawn {
        vitals_id: mob.vitals_id,
        level: TEST_LEVEL,
        position: [x, y, z],
        ..VitalsSpawn::default()
    };
    let mut source = test_source();
    source
        .base_types
        .push((vitals_name.to_owned(), MobType::PLAIN));
    let combat_data =
        std::sync::Arc::new(CombatData::build(source, &CombatRows::default()).expect("rows"));
    let config = sim::config::load_sim_config(&repo_root().join(SIM_CONFIG)).expect("sim config");
    let combat = config.combat_settings(combat_data, vec![spawn].into(), ai);
    let mut mobs = Mobs::new(combat.spawns.clone(), &combat);
    mobs.set_cells(|_| true);
    let entity = mob_entity(0);
    let player: Vec<(u64, [f32; 3])> = trace
        .player
        .iter()
        .map(|sample| (sample.time_ms, sample.position))
        .collect();
    let mut fight = SimFight {
        retail,
        ..SimFight::default()
    };
    let mut serial = None;
    let mut last = mobs.get(entity).expect("mob").position;
    let mut now = start_ms;
    while now <= end_ms {
        let [px, py, pz] = last_at(&player, now).expect("a player position");
        let seen = Seen {
            entity: PLAYER,
            position: Position {
                x: px,
                y: py,
                z: pz,
            },
        };
        for (at, length_ms) in reactions
            .iter()
            .filter(|(at, _)| (now.saturating_sub(TICK_MS) + 1..=now).contains(at))
        {
            mobs.hurt(entity, PLAYER, seen.position, 1, *at);
            mobs.react(entity, now, *length_ms);
        }
        mobs.tick(&[seen], now, &Unmapped, 1);
        let here = mobs.get(entity).expect("mob").position;
        // A tick of 33 ms.
        #[allow(clippy::cast_precision_loss, reason = "a tick of 33 ms")]
        let speed = here.horizontal_distance(last) * MS_PER_S / TICK_MS as f32;
        fight.top_speed_mps = fight.top_speed_mps.max(speed);
        last = here;
        if let Some((shown, _, fragment)) = mobs.shown(entity)
            && serial != Some(shown.serial)
        {
            serial = Some(shown.serial);
            fight.starts.push(SimStart {
                time_ms: now,
                fragment: fragment.to_owned(),
                distance_m: here.horizontal_distance(seen.position),
            });
        }
        now += TICK_MS;
    }
    fight
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The retail tree entry that starts within the tolerance of `now`, if one does.
fn retail_near(retail: &[RetailEntry], now: u64) -> Option<&RetailEntry> {
    retail
        .iter()
        .filter(|entry| entry.time_ms.abs_diff(now) <= START_TOLERANCE_MS)
        .min_by_key(|entry| entry.time_ms.abs_diff(now))
}

/// The task starts of a tick's commands: the move slot (for a move), the fragment of the task in
/// `grid`, and whether a redraw picked it.
fn starts(data: &AiData, grid: &Grid, out: &[Command]) -> Vec<(Option<Slot>, String, bool)> {
    out.iter()
        .filter_map(|command| match command {
            Command::PlayTask { task, .. } => Some((None, *task)),
            Command::Navigate { slot, task, .. } | Command::Strafe { slot, task, .. } => {
                Some((Some(*slot), *task))
            }
            _ => None,
        })
        .map(|(slot, task)| {
            let fragment = grid.action(task).map_or_else(
                || name_of(data, task),
                |action| name_of(data, data.cage_actions[action as usize].fragment),
            );
            (slot, fragment, false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use nw_content::client_build::load_client_build;

    use super::*;
    use crate::ai_trace::{read_creatures, trace_ledger};
    use crate::ftue_entities::read_ledger;

    /// The retail capture of a new character's tutorial and beach (2026-09-18).
    const CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
    const LEDGER: &str = "local-data/captures/own/newchar-retail-2026-09-18/ledger.bin";
    const BOAR_SLICE: &str = "slices/characters/boar.dynamicslice";
    /// Fragments that open a fight: the boars were hit first (`r_R2_F`).
    const BOAR_FIGHT_STARTS: [&str; 1] = ["r_R2_F"];
    const DEATH_PREFIX: &str = "r_Death";
    const SEED: u64 = 1;
    /// The retail boar fights (trace finding a: mob 23 at 625.3 s, mob 113 at 1,349.3 s).
    const BOAR_FIGHTS: usize = 2;
    const SAILOR_SLICE: &str =
        "slices/characters/ftue_corrupted/corrupted_sailor_zone3_club.dynamicslice";
    /// The FTUE club sailors open their fight with their alert (trace finding b).
    const ALERT: &str = "Alert";
    const SAILOR_FIGHT_STARTS: [&str; 1] = [ALERT];
    /// The FTUE club sailors fought in the capture (trace finding b: mobs 0, 1 and 2); the first
    /// fight (77.6 to 87.6 s) began before the trace has the player's position (82.9 s), so two
    /// can be replayed.
    const SAILOR_FIGHTS: usize = 2;
    /// The boar's close band: `Target_Dist <= 3` (boar_base `CloseRangeCombatBehavior`).
    const CLOSE_BAND_M: f32 = 3.0;

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixtures() -> (AiData, AiTrace) {
        let content = repo().join("local-data/content");
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let data = AiData::load(&content, &build).expect("run `content-tool ai-data` first");
        let creatures = read_creatures(&nw_content::creature_animations::creature_animations_path(
            &content, &build,
        ))
        .expect("creature table");
        let ledger = read_ledger(&repo().join(LEDGER)).expect("ledger");
        (data, trace_ledger(&ledger, &creatures))
    }

    /// The fights of mobs of `slice`: from the first entry of `starts` to the death, when the
    /// trace has the player's position from the start on.
    fn fights<'a>(
        trace: &'a AiTrace,
        slice: &str,
        starts: &[&str],
    ) -> Vec<(&'a MobTrace, u64, u64)> {
        let first_player = trace
            .player
            .first()
            .map_or(u64::MAX, |sample| sample.time_ms);
        trace
            .mobs
            .iter()
            .filter(|mob| {
                mob.slice
                    .as_deref()
                    .is_some_and(|known| known.eq_ignore_ascii_case(slice))
            })
            .filter_map(|mob| {
                let fragment_at = |sample: &crate::ai_trace::MobSample| {
                    sample
                        .full_body
                        .as_ref()
                        .and_then(|entry| entry.fragment_name.clone())
                };
                let start = mob.samples.iter().find(|sample| {
                    fragment_at(sample).is_some_and(|fragment| starts.contains(&fragment.as_str()))
                })?;
                let end = mob
                    .samples
                    .iter()
                    .find(|sample| {
                        sample.time_ms > start.time_ms
                            && fragment_at(sample)
                                .is_some_and(|fragment| fragment.starts_with(DEATH_PREFIX))
                    })
                    .map_or(mob.last_ms, |sample| sample.time_ms);
                (start.time_ms >= first_player).then_some((mob, start.time_ms, end))
            })
            .collect()
    }

    #[test]
    fn the_boar_tree_replays_the_retail_boar_fights() {
        let (data, trace) = fixtures();
        let archetype = data
            .archetypes
            .iter()
            .position(|arch| data.name(arch.slice) == BOAR_SLICE)
            .expect("boar archetype") as u32;
        let fights = fights(&trace, BOAR_SLICE, &BOAR_FIGHT_STARTS);
        assert_eq!(fights.len(), BOAR_FIGHTS);
        for (mob, start, end) in fights {
            let report = replay(&data, archetype, &trace, mob, start, end, SEED);
            assert!(report.retail.len() >= 4, "{report:#?}");
            assert!(
                report.unmatched.is_empty(),
                "boar {}: {:#?}",
                mob.interest,
                report.unmatched
            );
            for (index, start) in report.tree.iter().enumerate() {
                let after_move = index > 0 && report.tree[index - 1].fragment == "Move_Combat";
                if start.fragment == "Attack_Gore" && !after_move {
                    assert!(start.distance_m <= CLOSE_BAND_M, "{start:?}");
                }
            }
        }
    }

    /// The vitals id of the beach boars (study section 2.4: `Boar_StarterBeach` 3861127905).
    const BOAR_VITALS: &str = "Boar_StarterBeach";
    /// The boar's run: `RunToTarget` at most 7 m/s (slice `boar`; retail 6.96 m/s).
    const RUN_MAX_MPS: f32 = 7.0;
    const SPEED_SLACK_MPS: f32 = 0.05;
    /// The gore is done 1,404 ms after its start (46 frames at 30 fps over speed 0.85, less its
    /// 0.4 s completion offset); retail's next entries came 1,465 and 1,498 ms after it.
    const GORE_DONE_MS: u64 = 1_404;
    /// The retail turns lasted 1,632 and 1,683 ms.
    const RETAIL_TURN_MS: [u64; 2] = [1_632, 1_683];
    const TURN: &str = "Combat_Turn";
    const GORE: &str = "Attack_Gore";
    const RUN: &str = "Move_Combat";

    /// The boar fights in the sim (closed loop: the sim's motion places the boar, so its starts
    /// drift from retail's after the first move): the first action is retail's turn at retail's
    /// time; every action the retail tree drove comes; a gore starts in the close band (3 m) or
    /// after a run; the gore and the turn last their retail lengths; the run keeps to its config
    /// speed.
    #[test]
    fn the_sim_replays_the_retail_boar_fights() {
        let (data, trace) = fixtures();
        let data = std::sync::Arc::new(data);
        let fights = fights(&trace, BOAR_SLICE, &BOAR_FIGHT_STARTS);
        assert_eq!(fights.len(), BOAR_FIGHTS);
        for (mob, start, end) in fights {
            let fight = sim_replay(&data, &trace, mob, BOAR_VITALS, (start, end));
            let first_retail = &fight.retail[0];
            let first_sim = &fight.starts[0];
            assert_eq!(first_sim.fragment, first_retail.fragment, "{fight:#?}");
            assert!(
                first_sim.time_ms.abs_diff(first_retail.time_ms) <= START_TOLERANCE_MS,
                "{fight:#?}"
            );
            for entry in &fight.retail {
                assert!(
                    fight
                        .starts
                        .iter()
                        .any(|start| start.fragment == entry.fragment),
                    "{} never started: {fight:#?}",
                    entry.fragment
                );
            }
            for (index, start) in fight.starts.iter().enumerate() {
                let next = fight.starts.get(index + 1);
                if start.fragment == GORE {
                    let after_run = index > 0 && fight.starts[index - 1].fragment == RUN;
                    assert!(after_run || start.distance_m <= CLOSE_BAND_M, "{start:?}");
                    if let Some(next) = next {
                        let lasted = next.time_ms - start.time_ms;
                        assert!(lasted.abs_diff(GORE_DONE_MS) <= 2 * TICK_MS, "{lasted}");
                    }
                }
                if start.fragment == TURN
                    && let Some(next) = next
                {
                    let lasted = next.time_ms - start.time_ms;
                    let near = RETAIL_TURN_MS
                        .iter()
                        .any(|retail| lasted.abs_diff(*retail) <= START_TOLERANCE_MS);
                    assert!(near, "turn {lasted} ms");
                }
            }
            assert!(
                fight.top_speed_mps <= RUN_MAX_MPS + SPEED_SLACK_MPS,
                "{fight:#?}"
            );
        }
    }

    /// The candidate stand-in for the FTUE club sailor's missing
    /// `drowned/ftue/drowned_qte_basebehavior` (study section 2.4: same folder and grid).
    const SAILOR_CANDIDATE: &str =
        "sharedassets/ai/behaviortrees/drowned/ftue/drowned_ftue_basebehavior.behaviortree";

    /// Evidence that the candidate tree is not the club sailor's (so the data gives the sailor
    /// no tree): replayed with it, the sailor does not do what retail did. Retail opens each
    /// replayable fight with two `Alert` entries (1,731 + 1,400 ms and 1,967 + 1,266 ms, trace
    /// finding b); the candidate plays one `Alert` and moves on, and later starts drift from
    /// retail.
    #[test]
    fn the_candidate_tree_does_not_replay_the_retail_club_sailor_fights() {
        let (mut data, trace) = fixtures();
        let archetype = data
            .archetypes
            .iter()
            .position(|arch| data.name(arch.slice) == SAILOR_SLICE)
            .expect("sailor archetype");
        assert!(
            data.archetypes[archetype]
                .trees
                .iter()
                .all(|tree| tree.tree.is_none()),
            "Server.pak has no tree of the sailor"
        );
        let candidate = data
            .tree(SAILOR_CANDIDATE)
            .map(|tree| tree.path)
            .expect("candidate");
        let index = data.trees.iter().position(|tree| tree.path == candidate);
        data.archetypes[archetype].trees[0].tree = index.map(|index| index as u32);
        let fights = fights(&trace, SAILOR_SLICE, &SAILOR_FIGHT_STARTS);
        assert_eq!(fights.len(), SAILOR_FIGHTS);
        for (mob, start, end) in fights {
            let report = replay(&data, archetype as u32, &trace, mob, start, end, SEED);
            let alerts = |fragments: Vec<&str>| {
                fragments
                    .iter()
                    .take_while(|fragment| **fragment == ALERT)
                    .count()
            };
            let retail = alerts(
                report
                    .retail
                    .iter()
                    .map(|entry| entry.fragment.as_str())
                    .collect(),
            );
            let tree = alerts(
                report
                    .tree
                    .iter()
                    .map(|start| start.fragment.as_str())
                    .collect(),
            );
            assert_eq!((retail, tree), (2, 1), "sailor {}", mob.interest);
            assert!(!report.unmatched.is_empty(), "sailor {}", mob.interest);
        }
    }

    /// The beach boar's slice values and its Gore (study section 2.4 and 3.2: leash 50 / 70 m,
    /// run 2-7 m/s; `boar_gore` 46 frames at speed 0.85, done 0.4 s before its end; damage from
    /// 0.70 s for 0.20 s, row `Attack_Gore`).
    #[test]
    fn the_boar_archetype_holds_its_slice_and_cage_values() {
        const AMBIENT_LEASH_M: f32 = 50.0;
        const COMBAT_LEASH_M: f32 = 70.0;
        const RUN_MAX_SPEED: f32 = 7.0;
        const GORE_DONE_S: f32 = 46.0 / 30.0 / 0.85 - 0.4;
        const DAMAGE_START_S: f32 = 0.7;
        const DAMAGE_END_S: f32 = 0.9;
        const CLOSE_S: f32 = 1e-4;
        let (data, _) = fixtures();
        let boar = data.archetype(BOAR_SLICE).expect("boar");
        let value = |key: &str| data.key(key).and_then(|key| boar.config_value(key));
        assert_eq!(
            value("AmbientLeashDistance"),
            Some(Value::Float(AMBIENT_LEASH_M))
        );
        assert_eq!(
            value("CombatLeashDistance"),
            Some(Value::Float(COMBAT_LEASH_M))
        );
        let run = data
            .name_id("RunToTarget")
            .and_then(|kind| boar.nav_config(kind))
            .expect("run");
        assert_eq!(boar.nav_configs[run].max_speed, RUN_MAX_SPEED);
        let task = data.name_id("Attack_Gore_CAGE_Task").expect("task");
        let gore = data.cage_action(boar, task).expect("gore");
        let done = gore.done_after_s(&|_| false).expect("length");
        assert!((done - GORE_DONE_S).abs() < CLOSE_S, "{done}");
        let damage = data.name_id("CAGE-Damage").expect("kind");
        let row = data.name_id("DamageTableRow").expect("param");
        let window = gore
            .variant(&|_| false)
            .and_then(|variant| variant.windows.iter().find(|window| window.kind == damage))
            .expect("damage window");
        assert!((window.start_s - DAMAGE_START_S).abs() < CLOSE_S);
        assert!(
            window
                .end_s
                .is_some_and(|end| (end - DAMAGE_END_S).abs() < CLOSE_S)
        );
        let row_value = window
            .params
            .iter()
            .find(|(name, _)| *name == row)
            .map(|(_, value)| data.name(*value));
        assert_eq!(row_value, Some("Attack_Gore"));
        let pounce = data.name_id("Pounce_CAGE_Task").expect("task");
        assert!(
            data.cage_action(boar, pounce).is_none(),
            "in no grid (study section 0.4)"
        );
    }
}
