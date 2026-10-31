// SPDX-License-Identifier: AGPL-3.0-or-later
//! The encounters of the instance an instance sim runs (dungeon2 study, run 15): the stages of
//! each encounter of its game mode's level script ([`nw_world::dungeon_scripts`]), their spawn
//! timelines, objectives and actions, the encounter's reset at a wipe, and its end: the loot
//! of the loot containers it opens, and the instance's completion after the last one.
//!
//! The level script's own layout is data (encounter slices, `EncounterManagerComponent`,
//! `EncounterComponent`); how the server runs it is not in the client. The rules here follow
//! the class names, the fields and their values (best guesses, dungeon2 REPORT):
//!
//! - an encounter with `m_autoActivate` starts with the instance (after its delay); another
//!   starts when a living player comes within its volume's radius;
//! - its top stages run in order; a stage runs its spawn timeline and its start actions, then
//!   its child stages in order; it ends when its own objectives are done and (unless its
//!   children are optional) its children ended; a stage without objectives of its own ends with
//!   its children; a child whose objectives all wait for the parent runs beside the others and
//!   ends with the parent;
//! - an objective's time limit ends it (`m_succeedOnTimeout`) or fails its stage; a failed
//!   stage or a wipe (every player in the instance dead for the encounter's grace period)
//!   resets the encounter: its mobs go and it starts again.

use nw_bus::Point;
use nw_sim_core::Position;
use nw_world::dungeon_scripts::{
    Encounter, LevelScript, ObjectiveSpec, Spawn, StageAction, TargetMob,
};
use uuid::Uuid;

use super::loot_bag::LimitsSent;
use super::{Body, Player, SimWorld, Vitals};
use crate::objectives::Event;

/// Milliseconds in a second.
const MS_PER_S: u64 = 1_000;
/// `EventType` values of an entity event (`slice_triggers`).
const EVENT_TRIGGER: u32 = 0;
const EVENT_ACTIVATE: u32 = 2;
const EVENT_DEACTIVATE: u32 = 3;
/// The volume radius of an encounter whose slice has none: the most common sphere radius of the
/// encounter slices' detection volumes (best guess).
const DEFAULT_VOLUME_RADIUS_M: f32 = 30.0;
/// How long a dead encounter mob's body stays before it goes (best guess: the retail bodies of
/// the tutorial stayed 5.1 to 7.1 s after their death, encounter note).
const CORPSE_MS: u64 = 7_000;
/// The blackboard key of a boss's phase (behavior-tree study, engine key table).
const BOSS_PHASE_KEY: &str = "Boss_Phase";
/// Spawns of one GM spawn call.
const ONE: u32 = 1;

/// What the script asks of the sim.
pub(super) trait ScriptHost {
    /// Spawns mob `mob` at `at`; its entity, or `None` when it cannot spawn.
    fn spawn(&mut self, mob: &TargetMob, at: [f32; 3]) -> Option<u64>;
    /// Removes encounter mob `entity`.
    fn remove(&mut self, entity: u64);
    /// The health of mob `entity`; `None` when it is gone.
    fn health(&self, entity: u64) -> Option<u32>;
    /// The boss phase of mob `entity`.
    fn boss_phase(&self, entity: u64) -> Option<u32>;
    /// A random number below `below`.
    fn pick(&mut self, below: u32) -> u32;
}

/// What the players of the instance are doing in one tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ScriptView {
    /// The places of the living players.
    pub(super) living: Vec<Position>,
    /// Some player is in the instance (living or dead).
    pub(super) present: bool,
    /// The encounter events the mobs' trees sent: (mob, event id).
    pub(super) events: Vec<(u64, String)>,
}

/// Where a stage is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StageStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
}

/// The run of one stage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StageRun {
    status: StageStatus,
    started_ms: u64,
    /// Spawns of its timeline not made yet: (due time, index in the stage's spawns).
    due: Vec<(u64, usize)>,
    /// The next child stage to run.
    child: usize,
    /// Which of its objectives are done.
    done: Vec<bool>,
}

/// Where an encounter is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EncounterStatus {
    #[default]
    Idle,
    /// Its first stage starts at this time.
    Due(u64),
    Running,
    Done,
}

/// One mob an encounter spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EncounterMob {
    entity: u64,
    /// Its target and the slice of it.
    target: usize,
    slice: usize,
    /// The stage that spawned it.
    stage: usize,
    /// When it died.
    died_ms: Option<u64>,
    /// Its body went.
    gone: bool,
}

/// The run of one encounter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EncounterRun {
    status: EncounterStatus,
    /// The top stage running.
    top: usize,
    stages: Vec<StageRun>,
    mobs: Vec<EncounterMob>,
    /// Entity events its actions sent: (entity, event, time).
    entity_events: Vec<(u64, u32, u64)>,
    /// Tree events of its mobs: (event id, time).
    events: Vec<(String, u64)>,
    /// A living player is within its volume.
    engaged: bool,
    /// Since when every player is dead.
    wiped_since_ms: Option<u64>,
    /// Its end went out as an outcome.
    reported: bool,
}

/// What a tick of the script did that the sim acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScriptOutcome {
    /// The encounter of this index ended.
    EncounterDone(usize),
    /// Every encounter ended.
    Completed,
}

/// The script of the running instance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DungeonRun {
    instance: Uuid,
    script: LevelScript,
    encounters: Vec<EncounterRun>,
    completed: bool,
}

impl DungeonRun {
    /// The run of `script` for instance `instance`: every encounter idle.
    pub(super) fn new(instance: Uuid, script: LevelScript) -> Self {
        let encounters = script
            .encounters
            .iter()
            .map(|encounter| EncounterRun {
                stages: vec![StageRun::default(); encounter.stages.len()],
                ..EncounterRun::default()
            })
            .collect();
        Self {
            instance,
            script,
            encounters,
            completed: false,
        }
    }

    /// The live mobs of every encounter.
    pub(super) fn mobs(&self) -> impl Iterator<Item = u64> + '_ {
        self.encounters
            .iter()
            .flat_map(|run| &run.mobs)
            .filter(|mob| !mob.gone)
            .map(|mob| mob.entity)
    }

    /// One tick at `now_ms`.
    pub(super) fn tick(
        &mut self,
        host: &mut impl ScriptHost,
        view: &ScriptView,
        now_ms: u64,
    ) -> Vec<ScriptOutcome> {
        let mut outcomes = Vec::new();
        for (index, encounter) in self.script.encounters.iter().enumerate() {
            let run = &mut self.encounters[index];
            tick_encounter(encounter, run, host, view, now_ms);
            if run.status == EncounterStatus::Done && !run.reported {
                run.reported = true;
                outcomes.push(ScriptOutcome::EncounterDone(index));
            }
        }
        let all_done = !self.encounters.is_empty()
            && self
                .encounters
                .iter()
                .all(|run| run.status == EncounterStatus::Done);
        if all_done && !self.completed {
            self.completed = true;
            outcomes.push(ScriptOutcome::Completed);
        }
        outcomes
    }

    /// The loot containers whose nearest encounter is encounter `index` (best guess: an
    /// encounter's end opens the containers beside it; the level script's own key and gate
    /// logic is server script that the client does not have).
    pub(super) fn chests_of(&self, index: usize) -> impl Iterator<Item = (&str, [f32; 3])> + '_ {
        self.script.chests.iter().filter_map(move |chest| {
            let nearest = self
                .script
                .encounters
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    distance(a.position, chest.position)
                        .total_cmp(&distance(b.position, chest.position))
                })
                .map(|(nearest, _)| nearest)?;
            (nearest == index).then_some((chest.loot_table.as_str(), chest.position))
        })
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f32>()
        .sqrt()
}

fn position([x, y, z]: [f32; 3]) -> Position {
    Position { x, y, z }
}

fn tick_encounter(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    view: &ScriptView,
    now_ms: u64,
) {
    for (mob, event) in &view.events {
        if run.mobs.iter().any(|known| known.entity == *mob) {
            run.events.push((event.clone(), now_ms));
        }
    }
    bury(run, host, now_ms);
    let radius = encounter
        .detection_radius_m
        .unwrap_or(DEFAULT_VOLUME_RADIUS_M);
    let place = position(encounter.position);
    run.engaged = view
        .living
        .iter()
        .any(|player| player.horizontal_distance(place) <= radius);
    let reset_asked = !encounter.reset_event.is_empty()
        && run
            .events
            .iter()
            .any(|(event, _)| event.eq_ignore_ascii_case(&encounter.reset_event));
    if run.status == EncounterStatus::Idle {
        if encounter.auto_activate {
            run.status = EncounterStatus::Due(
                now_ms.saturating_add(u64::from(encounter.auto_activate_delay_ms)),
            );
        } else if run.engaged {
            run.status = EncounterStatus::Due(now_ms);
        }
    }
    if let EncounterStatus::Due(at_ms) = run.status
        && now_ms >= at_ms
    {
        run.status = EncounterStatus::Running;
        run.top = 0;
    }
    if run.status != EncounterStatus::Running {
        return;
    }
    let wiped = view.present && view.living.is_empty();
    run.wiped_since_ms = wiped.then(|| run.wiped_since_ms.unwrap_or(now_ms));
    let grace_ms = u64::from(encounter.grace_period_s).saturating_mul(MS_PER_S);
    let wipe_over = run
        .wiped_since_ms
        .is_some_and(|since| now_ms.saturating_sub(since) >= grace_ms);
    if wipe_over || reset_asked {
        reset(run, host);
        return;
    }
    run_tops(encounter, run, host, now_ms);
}

/// Runs the top stages in order; a failed one resets the encounter.
fn run_tops(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    now_ms: u64,
) {
    while let Some(&stage) = encounter.top_stages.get(run.top) {
        if run.stages[stage].status == StageStatus::Pending {
            start_stage(encounter, run, host, stage, now_ms);
        }
        match step_stage(encounter, run, host, stage, now_ms) {
            StageStatus::Done => run.top += 1,
            StageStatus::Failed => {
                reset(run, host);
                return;
            }
            StageStatus::Pending | StageStatus::Running => return,
        }
    }
    run.status = EncounterStatus::Done;
}

/// The encounter starts again: its mobs go, its stages wait, it is idle.
fn reset(run: &mut EncounterRun, host: &mut impl ScriptHost) {
    for mob in run.mobs.iter_mut().filter(|mob| !mob.gone) {
        host.remove(mob.entity);
        mob.gone = true;
    }
    let stages = run.stages.len();
    *run = EncounterRun {
        stages: vec![StageRun::default(); stages],
        ..EncounterRun::default()
    };
}

/// Notes the deaths of the encounter's mobs; a body goes [`CORPSE_MS`] after its death.
fn bury(run: &mut EncounterRun, host: &mut impl ScriptHost, now_ms: u64) {
    for mob in run.mobs.iter_mut().filter(|mob| !mob.gone) {
        match host.health(mob.entity) {
            None => {
                mob.gone = true;
                mob.died_ms.get_or_insert(now_ms);
            }
            Some(0) => {
                let died = *mob.died_ms.get_or_insert(now_ms);
                if now_ms.saturating_sub(died) >= CORPSE_MS {
                    host.remove(mob.entity);
                    mob.gone = true;
                }
            }
            Some(_) => {}
        }
    }
}

fn start_stage(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    stage: usize,
    now_ms: u64,
) {
    let Some(def) = encounter.stages.get(stage) else {
        return;
    };
    run.stages[stage] = StageRun {
        status: StageStatus::Running,
        started_ms: now_ms,
        due: def
            .spawns
            .iter()
            .enumerate()
            .map(|(index, spawn)| {
                (
                    now_ms.saturating_add(u64::from(spawn.delay_s).saturating_mul(MS_PER_S)),
                    index,
                )
            })
            .collect(),
        child: 0,
        done: vec![false; def.objectives.len()],
    };
    run_actions(encounter, run, host, &def.start_actions, stage, now_ms);
}

/// Whether every objective of `stage` waits for its parent.
fn waits_for_parent(encounter: &Encounter, stage: usize) -> bool {
    encounter.stages.get(stage).is_some_and(|def| {
        !def.objectives.is_empty()
            && def
                .objectives
                .iter()
                .all(|objective| objective.kind == ObjectiveSpec::WaitForParent)
    })
}

/// One step of running stage `stage`; its status after it.
fn step_stage(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    stage: usize,
    now_ms: u64,
) -> StageStatus {
    let Some(def) = encounter.stages.get(stage) else {
        return StageStatus::Done;
    };
    if run.stages[stage].status != StageStatus::Running {
        return run.stages[stage].status;
    }
    spawn_due(encounter, run, host, stage, now_ms);
    // Its children, in order; a child that waits for its parent runs beside the next ones.
    while let Some(&child) = def.children.get(run.stages[stage].child) {
        if run.stages[child].status == StageStatus::Pending {
            start_stage(encounter, run, host, child, now_ms);
        }
        if waits_for_parent(encounter, child) {
            spawn_due(encounter, run, host, child, now_ms);
            run.stages[stage].child += 1;
            continue;
        }
        match step_stage(encounter, run, host, child, now_ms) {
            StageStatus::Done => run.stages[stage].child += 1,
            StageStatus::Failed => {
                run.stages[stage].status = StageStatus::Failed;
                return StageStatus::Failed;
            }
            StageStatus::Pending | StageStatus::Running => break,
        }
    }
    let children_ended = run.stages[stage].child >= def.children.len();
    let mut own = false;
    for (index, objective) in def.objectives.iter().enumerate() {
        if objective.kind == ObjectiveSpec::WaitForParent {
            continue;
        }
        own = true;
        if run.stages[stage].done[index] {
            continue;
        }
        let started = run.stages[stage].started_ms;
        let limit_ms = u64::from(objective.time_limit_s).saturating_mul(MS_PER_S);
        let timed_out = objective.time_limit_s > 0 && now_ms.saturating_sub(started) >= limit_ms;
        let met = objective_met(encounter, run, &*host, stage, &objective.kind, started);
        if (met && objective.failure) || (timed_out && !objective.succeed_on_timeout && !met) {
            run.stages[stage].status = StageStatus::Failed;
            return StageStatus::Failed;
        }
        if met || timed_out {
            run.stages[stage].done[index] = true;
        }
    }
    let own_done = def
        .objectives
        .iter()
        .zip(&run.stages[stage].done)
        .all(|(objective, done)| {
            *done || objective.failure || objective.kind == ObjectiveSpec::WaitForParent
        });
    let ended = if own {
        own_done && (children_ended || def.child_stages_optional)
    } else {
        children_ended
    };
    if ended {
        end_stage(encounter, run, host, stage, now_ms);
        return StageStatus::Done;
    }
    StageStatus::Running
}

/// Ends stage `stage` and every stage under it that still runs, with their end actions.
fn end_stage(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    stage: usize,
    now_ms: u64,
) {
    let Some(def) = encounter.stages.get(stage) else {
        return;
    };
    for child in &def.children {
        if run.stages[*child].status == StageStatus::Running {
            end_stage(encounter, run, host, *child, now_ms);
        }
    }
    run.stages[stage].status = StageStatus::Done;
    run.stages[stage].due.clear();
    run_actions(encounter, run, host, &def.end_actions, stage, now_ms);
}

/// Makes the spawns of `stage` that are due.
fn spawn_due(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    stage: usize,
    now_ms: u64,
) {
    let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut run.stages[stage].due)
        .into_iter()
        .partition(|(at_ms, _)| *at_ms <= now_ms);
    run.stages[stage].due = later;
    for (_, index) in due {
        if let Some(spawn) = encounter.stages[stage].spawns.get(index) {
            spawn_one(encounter, run, host, stage, spawn);
        }
    }
}

/// Spawns `spawn.count` mobs of its target: each picks a slice of the target by its weight
/// (an alias) and takes the next location (the encounter's place without one).
fn spawn_one(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    stage: usize,
    spawn: &Spawn,
) {
    let Some(target) = encounter.targets.get(spawn.target) else {
        return;
    };
    let mobs: Vec<(usize, &TargetMob, u32)> = target
        .slices
        .iter()
        .enumerate()
        .filter_map(|(index, slice)| Some((index, slice.mob.as_ref()?, slice.percent.max(ONE))))
        .collect();
    let total: u32 = mobs.iter().map(|(_, _, weight)| weight).sum();
    if mobs.is_empty() {
        tracing::debug!(target = %target.name, "encounter spawn of no mob");
        return;
    }
    for number in 0..spawn.count {
        let mut roll = host.pick(total);
        let Some(&(slice, mob, _)) = mobs.iter().find(|(_, _, weight)| {
            let hit = roll < *weight;
            roll = roll.saturating_sub(*weight);
            hit
        }) else {
            continue;
        };
        let at = usize::try_from(number)
            .ok()
            .and_then(|number| spawn.locations.get(number % spawn.locations.len().max(1)))
            .copied()
            .unwrap_or(encounter.position);
        match host.spawn(mob, at) {
            Some(entity) => run.mobs.push(EncounterMob {
                entity,
                target: spawn.target,
                slice,
                stage,
                died_ms: None,
                gone: false,
            }),
            None => tracing::warn!(
                vitals = %mob.vitals_id,
                level = mob.level,
                "an encounter mob cannot spawn"
            ),
        }
    }
}

/// Whether encounter mob `mob` is one of target `target`: its slice or its vitals row is one of
/// the target's.
fn is_of(encounter: &Encounter, mob: &EncounterMob, target: usize) -> bool {
    let Some(own) = encounter
        .targets
        .get(mob.target)
        .and_then(|own| own.slices.get(mob.slice))
    else {
        return false;
    };
    encounter.targets.get(target).is_some_and(|wanted| {
        wanted.slices.iter().any(|slice| {
            slice.path == own.path
                || slice
                    .mob
                    .as_ref()
                    .zip(own.mob.as_ref())
                    .is_some_and(|(a, b)| a.vitals_id.eq_ignore_ascii_case(&b.vitals_id))
        })
    })
}

/// Whether stage `stage` is `ancestor` or under it.
fn is_under(encounter: &Encounter, stage: usize, ancestor: usize) -> bool {
    stage == ancestor
        || encounter.stages.get(ancestor).is_some_and(|def| {
            def.children
                .iter()
                .any(|child| is_under(encounter, stage, *child))
        })
}

/// The mobs an objective of stage `stage` tracks: those of `target` (best guess: when no mob
/// of the encounter is of it, the target is a creature the stage's spawner slices pick by
/// their own alias, so the stage's own mobs), or the stage's own mobs without a target.
fn tracked<'a>(
    encounter: &'a Encounter,
    run: &'a EncounterRun,
    stage: usize,
    target: Option<usize>,
) -> Vec<&'a EncounterMob> {
    let own: Vec<&EncounterMob> = run
        .mobs
        .iter()
        .filter(|mob| is_under(encounter, mob.stage, stage))
        .collect();
    match target {
        Some(target) => {
            let of_target: Vec<&EncounterMob> = run
                .mobs
                .iter()
                .filter(|mob| is_of(encounter, mob, target))
                .collect();
            if of_target.is_empty() { own } else { of_target }
        }
        None => own,
    }
}

fn objective_met(
    encounter: &Encounter,
    run: &EncounterRun,
    host: &impl ScriptHost,
    stage: usize,
    kind: &ObjectiveSpec,
    started_ms: u64,
) -> bool {
    match kind {
        ObjectiveSpec::SliceDestroyed { target, required } => {
            let deaths = tracked(encounter, run, stage, *target)
                .into_iter()
                .filter(|mob| mob.died_ms.is_some_and(|died| died >= started_ms))
                .count();
            u32::try_from(deaths).unwrap_or(u32::MAX) >= (*required).max(ONE)
        }
        ObjectiveSpec::BossPhase { target, phase } => {
            let candidates: Vec<&EncounterMob> = match target {
                Some(target) => tracked(encounter, run, stage, Some(*target)),
                None => run.mobs.iter().collect(),
            };
            // Best guess: the boss's death also ends its phases.
            candidates.into_iter().any(|mob| {
                mob.died_ms.is_some()
                    || host
                        .boss_phase(mob.entity)
                        .is_some_and(|reached| reached >= *phase)
            })
        }
        ObjectiveSpec::EncounterEvent { event } => run
            .events
            .iter()
            .any(|(sent, at)| *at >= started_ms && sent.eq_ignore_ascii_case(event)),
        ObjectiveSpec::EntityActiveState {
            entity,
            on_activated,
            on_deactivated,
        } => {
            let sent = |wanted: u32| {
                run.entity_events
                    .iter()
                    .any(|(to, event, at)| to == entity && *event == wanted && *at >= started_ms)
            };
            if *on_activated || *on_deactivated {
                (*on_activated && sent(EVENT_ACTIVATE))
                    || (*on_deactivated && sent(EVENT_DEACTIVATE))
            } else {
                // Best guess: the entity is a timeline that the encounter's trigger area starts
                // when a player comes in.
                sent(EVENT_TRIGGER) || run.engaged
            }
        }
        ObjectiveSpec::WaitForParent => false,
        ObjectiveSpec::InvasionWave => {
            let spawned_all = run.stages[stage].due.is_empty();
            let own: Vec<&EncounterMob> =
                run.mobs.iter().filter(|mob| mob.stage == stage).collect();
            spawned_all && !own.is_empty() && own.iter().all(|mob| mob.died_ms.is_some())
        }
    }
}

/// Runs `actions` of stage `stage`.
fn run_actions(
    encounter: &Encounter,
    run: &mut EncounterRun,
    host: &mut impl ScriptHost,
    actions: &[StageAction],
    stage: usize,
    now_ms: u64,
) {
    for action in actions {
        match action {
            StageAction::EntityEvent { entity, event, .. } => {
                run.entity_events.push((*entity, *event, now_ms));
            }
            StageAction::DestroySlice { target } => {
                for index in 0..run.mobs.len() {
                    let matches =
                        target.is_none_or(|target| is_of(encounter, &run.mobs[index], target));
                    if matches && !run.mobs[index].gone {
                        host.remove(run.mobs[index].entity);
                        run.mobs[index].gone = true;
                    }
                }
            }
            StageAction::KillSpawns => {
                for index in 0..run.mobs.len() {
                    let mob = &run.mobs[index];
                    if !mob.gone && is_under(encounter, mob.stage, stage) {
                        host.remove(mob.entity);
                        run.mobs[index].gone = true;
                    }
                }
            }
        }
    }
}

/// The sim as the host of its instance's script.
struct SimHost<'a> {
    world: &'a mut SimWorld,
}

impl ScriptHost for SimHost<'_> {
    fn spawn(&mut self, mob: &TargetMob, at: [f32; 3]) -> Option<u64> {
        let [x, y, z] = at;
        match self
            .world
            .spawn_gm(&mob.vitals_id, mob.level, Point { x, y, z }, ONE)
        {
            Ok(entities) => {
                let entity = entities.first().copied()?;
                self.world.mutate_mob(entity);
                Some(entity)
            }
            Err(reason) => {
                tracing::warn!(vitals = %mob.vitals_id, %reason, "encounter spawn");
                None
            }
        }
    }

    fn remove(&mut self, entity: u64) {
        self.world.remove_gm(&[entity]);
    }

    fn health(&self, entity: u64) -> Option<u32> {
        self.world.mobs.health(entity)
    }

    fn boss_phase(&self, entity: u64) -> Option<u32> {
        if let Some(phase) = self.world.boss_phases.get(&entity) {
            return Some(*phase);
        }
        let key = self.world.mobs.ai().data().key(BOSS_PHASE_KEY)?;
        let value = self
            .world
            .mobs
            .tree(entity)?
            .brain()
            .fact(key)?
            .as_number()?;
        // A phase is a small whole number.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a phase"
        )]
        Some(value.max(0.0) as u32)
    }

    fn pick(&mut self, below: u32) -> u32 {
        self.world.dice.below(below.max(ONE))
    }
}

impl SimWorld {
    /// The encounters part of the tick at `now_ms`: the script of the running instance starts
    /// with it and ends with it; its encounters run; an ended one opens its loot containers,
    /// and the last one completes the instance.
    pub(super) fn tick_dungeon_script(&mut self, now_ms: u64) {
        let events = self.mobs.take_encounter_events();
        let running = self
            .instance_run
            .as_ref()
            .map(|run| (run.id(), run.game_mode().to_owned()));
        let current = self.dungeon_run.as_ref().map(|run| run.instance);
        if current != running.as_ref().map(|(instance, _)| *instance) {
            if let Some(old) = self.dungeon_run.take() {
                let mobs: Vec<u64> = old.mobs().collect();
                self.remove_gm(&mobs);
            }
            self.dungeon_run = running.and_then(|(instance, game_mode)| {
                let script = self.content.dungeon_scripts.game_mode(&game_mode)?.clone();
                tracing::info!(
                    %instance,
                    game_mode = %game_mode,
                    encounters = script.encounters.len(),
                    chests = script.chests.len(),
                    "dungeon script"
                );
                Some(DungeonRun::new(instance, script))
            });
        }
        let Some(mut run) = self.dungeon_run.take() else {
            return;
        };
        let mut view = ScriptView {
            events,
            ..ScriptView::default()
        };
        let mut query = self.world.query::<(&Player, &Body, &Vitals)>();
        for (_, body, vitals) in query.iter(&self.world) {
            view.present = true;
            if !vitals.health.is_dead() {
                view.living.push(body.motion.position());
            }
        }
        let outcomes = run.tick(&mut SimHost { world: self }, &view, now_ms);
        for outcome in outcomes {
            match outcome {
                ScriptOutcome::EncounterDone(index) => {
                    let chests: Vec<(String, [f32; 3])> = run
                        .chests_of(index)
                        .map(|(table, at)| (table.to_owned(), at))
                        .collect();
                    tracing::info!(
                        encounter = %run.script.encounters[index].name,
                        chests = chests.len(),
                        "encounter done"
                    );
                    for (table, _) in chests {
                        self.open_dungeon_chest(&table, now_ms);
                    }
                }
                ScriptOutcome::Completed => {
                    tracing::info!(instance = %run.instance, "every encounter done");
                    self.complete_mutated_instance(now_ms);
                    self.game_mode_completed(now_ms);
                }
            }
        }
        self.dungeon_run = Some(run);
    }

    /// Every player of the instance finished its game mode (`TaskCompleteDungeon`).
    fn game_mode_completed(&mut self, now_ms: u64) {
        let Some(mode) = self
            .instance_run
            .as_ref()
            .map(|run| run.game_mode().to_owned())
        else {
            return;
        };
        let numbers: Vec<u64> = {
            let mut query = self.world.query::<&Player>();
            query
                .iter(&self.world)
                .map(|player| player.number)
                .collect()
        };
        for number in numbers {
            self.quest_event(number, Event::GameModeCompleted(&mode), now_ms);
            self.update_quest_tracker(number);
        }
    }

    /// Every player of the instance gets the loot of loot table `table` (each its own roll, as
    /// an instanced loot container; needs live check: retail players open the container).
    fn open_dungeon_chest(&mut self, table: &str, now_ms: u64) {
        let content = std::sync::Arc::clone(&self.content);
        let numbers: Vec<u64> = {
            let mut query = self.world.query::<&Player>();
            query
                .iter(&self.world)
                .map(|player| player.number)
                .collect()
        };
        for number in numbers {
            let reached = self.reached_loot_limits(number);
            let context = self.loot_context(number, None, &reached);
            let dice = &mut self.dice;
            let rolled = content
                .loot
                .roll(table, context, &mut |below| dice.below(below));
            let hints = rolled.gear_scores.clone();
            let loot = self.limit_loot(number, rolled);
            self.send_loot_limits(number, LimitsSent::Changed);
            let (items, rolls) = self.roll_drops(number, None, loot, &hints);
            self.give_drops(number, items.into_iter().zip(rolls).collect(), now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use nw_world::dungeon_scripts::{Stage, StageObjective, Target, TargetSlice};

    use super::*;

    const BOSS: usize = 0;
    const ADDS: usize = 1;
    const PLACE: [f32; 3] = [886.0, 906.0, 141.0];
    const RADIUS_M: f32 = 30.0;
    const INTRO_S: u32 = 20;
    const GRACE_S: u32 = 20;
    const BOSS_VITALS: &str = "Withered_Brute_Named_08";
    const ADD_VITALS: &str = "Risen_DG_Windsward_00";
    const LEVEL: u32 = 25;
    const T0: u64 = 1_000;

    /// A test host: spawned mobs by entity with their health and phase.
    #[derive(Default)]
    struct Host {
        next: u64,
        health: HashMap<u64, u32>,
        phase: HashMap<u64, u32>,
        spawned: Vec<(String, [f32; 3])>,
        removed: Vec<u64>,
    }

    impl ScriptHost for Host {
        fn spawn(&mut self, mob: &TargetMob, at: [f32; 3]) -> Option<u64> {
            self.next += 1;
            self.health.insert(self.next, 100);
            self.spawned.push((mob.vitals_id.clone(), at));
            Some(self.next)
        }
        fn remove(&mut self, entity: u64) {
            self.health.remove(&entity);
            self.removed.push(entity);
        }
        fn health(&self, entity: u64) -> Option<u32> {
            self.health.get(&entity).copied()
        }
        fn boss_phase(&self, entity: u64) -> Option<u32> {
            self.phase.get(&entity).copied()
        }
        fn pick(&mut self, _below: u32) -> u32 {
            0
        }
    }

    impl Host {
        fn kill(&mut self, vitals: &str) {
            let entities: Vec<u64> = self
                .spawned
                .iter()
                .enumerate()
                .filter(|(_, (id, _))| id == vitals)
                .map(|(index, _)| u64::try_from(index).expect("index") + 1)
                .collect();
            for entity in entities {
                if let Some(health) = self.health.get_mut(&entity) {
                    *health = 0;
                }
            }
        }
    }

    fn target(vitals: &str) -> Target {
        Target {
            name: vitals.to_owned(),
            slices: vec![TargetSlice {
                path: format!("slices/{vitals}.dynamicslice"),
                mob: Some(TargetMob {
                    vitals_id: vitals.to_owned(),
                    category_id: String::new(),
                    level: LEVEL,
                }),
                ..TargetSlice::default()
            }],
        }
    }

    fn objective(kind: ObjectiveSpec, time_limit_s: u32, succeed: bool) -> StageObjective {
        StageObjective {
            time_limit_s,
            succeed_on_timeout: succeed,
            failure: false,
            kind,
        }
    }

    /// The Amrine boss encounter's shape: a wave that spawns the boss and ends with its death,
    /// with optional children: wait for a player, a 20 s intro, the fight.
    fn boss_encounter() -> Encounter {
        let wave = Stage {
            name: "Wave_01".to_owned(),
            child_stages_optional: true,
            children: vec![1, 2, 3],
            spawns: vec![Spawn {
                target: BOSS,
                count: 1,
                ..Spawn::default()
            }],
            objectives: vec![objective(
                ObjectiveSpec::SliceDestroyed {
                    target: Some(BOSS),
                    required: 1,
                },
                0,
                false,
            )],
            ..Stage::default()
        };
        let wait = Stage {
            name: "Phase 0 Wait".to_owned(),
            objectives: vec![objective(
                ObjectiveSpec::EntityActiveState {
                    entity: 0xa8,
                    on_activated: false,
                    on_deactivated: false,
                },
                0,
                false,
            )],
            ..Stage::default()
        };
        let intro = Stage {
            name: "Phase 1 Intro".to_owned(),
            objectives: vec![objective(
                ObjectiveSpec::EncounterEvent {
                    event: "Intro2_Complete".to_owned(),
                },
                INTRO_S,
                true,
            )],
            ..Stage::default()
        };
        let fight = Stage {
            name: "Phase 2 Fight".to_owned(),
            objectives: vec![objective(
                ObjectiveSpec::BossPhase {
                    target: None,
                    phase: 3,
                },
                0,
                false,
            )],
            ..Stage::default()
        };
        Encounter {
            name: "EncounterBoss".to_owned(),
            position: PLACE,
            auto_activate: true,
            grace_period_s: GRACE_S,
            detection_radius_m: Some(RADIUS_M),
            top_stages: vec![0],
            stages: vec![wave, wait, intro, fight],
            targets: vec![target(BOSS_VITALS), target(ADD_VITALS)],
            ..Encounter::default()
        }
    }

    fn script(encounters: Vec<Encounter>) -> LevelScript {
        LevelScript {
            level: "nw_dungeon_windsward_00".to_owned(),
            encounters,
            chests: vec![nw_world::dungeon_scripts::Chest {
                name: "LockedChest_Simon".to_owned(),
                position: [PLACE[0] - 16.0, PLACE[1] - 21.0, PLACE[2]],
                loot_table: "ChestLootElite".to_owned(),
                ..Default::default()
            }],
            ..LevelScript::default()
        }
    }

    fn view(players: &[[f32; 3]]) -> ScriptView {
        ScriptView {
            living: players.iter().map(|at| position(*at)).collect(),
            present: true,
            events: Vec::new(),
        }
    }

    fn stage_status(run: &DungeonRun, stage: usize) -> StageStatus {
        run.encounters[0].stages[stage].status
    }

    #[test]
    fn the_boss_spawns_waits_for_a_player_and_its_death_ends_the_encounter() {
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![boss_encounter()]));
        let mut host = Host::default();
        let far = [0.0, 0.0, 0.0];
        run.tick(&mut host, &view(&[far]), T0);
        run.tick(&mut host, &view(&[far]), T0 + 1);
        assert_eq!(host.spawned, [(BOSS_VITALS.to_owned(), PLACE)]);
        assert_eq!(
            stage_status(&run, 1),
            StageStatus::Running,
            "waits for a player"
        );
        run.tick(&mut host, &view(&[PLACE]), T0 + 2);
        assert_eq!(stage_status(&run, 1), StageStatus::Done);
        assert_eq!(stage_status(&run, 2), StageStatus::Running, "the intro");
        let mut events = view(&[PLACE]);
        events.events.push((1, "Intro2_Complete".to_owned()));
        run.tick(&mut host, &events, T0 + 3);
        assert_eq!(stage_status(&run, 2), StageStatus::Done);
        assert_eq!(stage_status(&run, 3), StageStatus::Running, "the fight");
        host.kill(BOSS_VITALS);
        let outcomes = run.tick(&mut host, &view(&[PLACE]), T0 + 4);
        assert_eq!(
            outcomes,
            [ScriptOutcome::EncounterDone(0), ScriptOutcome::Completed]
        );
        assert_eq!(run.chests_of(0).count(), 1);
        run.tick(&mut host, &view(&[PLACE]), T0 + 4 + CORPSE_MS);
        assert_eq!(host.removed, [1], "the body goes after its corpse time");
    }

    #[test]
    fn the_intro_ends_at_its_time_limit_without_its_event() {
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![boss_encounter()]));
        let mut host = Host::default();
        run.tick(&mut host, &view(&[PLACE]), T0);
        run.tick(&mut host, &view(&[PLACE]), T0 + 1);
        run.tick(&mut host, &view(&[PLACE]), T0 + 2);
        assert_eq!(stage_status(&run, 2), StageStatus::Running);
        let limit = u64::from(INTRO_S) * MS_PER_S;
        run.tick(&mut host, &view(&[PLACE]), T0 + 2 + limit);
        assert_eq!(stage_status(&run, 2), StageStatus::Done);
        host.phase.insert(1, 3);
        run.tick(&mut host, &view(&[PLACE]), T0 + 3 + limit);
        assert_eq!(
            stage_status(&run, 3),
            StageStatus::Done,
            "phase 3 ends the fight"
        );
    }

    #[test]
    fn a_wipe_past_the_grace_period_resets_the_encounter() {
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![boss_encounter()]));
        let mut host = Host::default();
        run.tick(&mut host, &view(&[PLACE]), T0);
        run.tick(&mut host, &view(&[PLACE]), T0 + 1);
        assert_eq!(host.spawned.len(), 1);
        run.tick(&mut host, &view(&[]), T0 + 2);
        let grace = u64::from(GRACE_S) * MS_PER_S;
        run.tick(&mut host, &view(&[]), T0 + 2 + grace);
        assert_eq!(host.removed, [1], "the boss goes");
        assert_eq!(run.encounters[0].status, EncounterStatus::Idle);
        run.tick(&mut host, &view(&[PLACE]), T0 + 3 + grace);
        run.tick(&mut host, &view(&[PLACE]), T0 + 4 + grace);
        assert_eq!(host.spawned.len(), 2, "it starts again");
    }

    #[test]
    fn a_kill_wave_counts_its_own_mobs_when_its_target_names_other_slices() {
        let wave = Stage {
            name: "Wave_01 - Objective: Kill Wave".to_owned(),
            spawns: vec![Spawn {
                target: ADDS,
                count: 2,
                delay_s: 1,
                locations: vec![[1.0, 2.0, 3.0]],
            }],
            objectives: vec![objective(
                ObjectiveSpec::SliceDestroyed {
                    target: Some(BOSS),
                    required: 2,
                },
                0,
                false,
            )],
            ..Stage::default()
        };
        let cleanup = Stage {
            name: "Wave_03 - Cleanup".to_owned(),
            objectives: vec![objective(ObjectiveSpec::InvasionWave, 3, true)],
            ..Stage::default()
        };
        let encounter = Encounter {
            auto_activate: false,
            top_stages: vec![0, 1],
            stages: vec![wave, cleanup],
            ..boss_encounter()
        };
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![encounter]));
        let mut host = Host::default();
        run.tick(&mut host, &view(&[[0.0; 3]]), T0);
        assert_eq!(
            run.encounters[0].status,
            EncounterStatus::Idle,
            "no player near"
        );
        run.tick(&mut host, &view(&[PLACE]), T0 + 1);
        run.tick(&mut host, &view(&[PLACE]), T0 + 2);
        assert!(host.spawned.is_empty(), "the spawn waits for its delay");
        run.tick(&mut host, &view(&[PLACE]), T0 + 2 + MS_PER_S);
        assert_eq!(host.spawned.len(), 2);
        assert!(host.spawned.iter().all(|(_, at)| *at == [1.0, 2.0, 3.0]));
        host.kill(ADD_VITALS);
        run.tick(&mut host, &view(&[PLACE]), T0 + 3 + MS_PER_S);
        assert_eq!(stage_status(&run, 0), StageStatus::Done);
        assert_eq!(stage_status(&run, 1), StageStatus::Running);
        let outcomes = run.tick(&mut host, &view(&[PLACE]), T0 + 4 + 4 * MS_PER_S);
        assert!(outcomes.contains(&ScriptOutcome::Completed));
    }

    #[test]
    fn a_failure_objective_fails_its_stage_and_resets_the_encounter() {
        let mut encounter = boss_encounter();
        encounter.stages[0].objectives.push(StageObjective {
            time_limit_s: 0,
            succeed_on_timeout: false,
            failure: true,
            kind: ObjectiveSpec::EncounterEvent {
                event: "Defeat".to_owned(),
            },
        });
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![encounter]));
        let mut host = Host::default();
        run.tick(&mut host, &view(&[PLACE]), T0);
        run.tick(&mut host, &view(&[PLACE]), T0 + 1);
        let mut events = view(&[PLACE]);
        events.events.push((1, "Defeat".to_owned()));
        run.tick(&mut host, &events, T0 + 2);
        assert_eq!(run.encounters[0].status, EncounterStatus::Idle);
        assert_eq!(host.removed, [1]);
    }

    #[test]
    fn end_actions_remove_the_stages_mobs_and_note_entity_events() {
        let mut encounter = boss_encounter();
        encounter.stages[0].end_actions = vec![
            StageAction::KillSpawns,
            StageAction::EntityEvent {
                entity: 7,
                event: EVENT_ACTIVATE,
                recursive: false,
                local_player_only: false,
            },
        ];
        encounter.stages[0].objectives = vec![objective(
            ObjectiveSpec::EncounterEvent {
                event: "Fight_Complete".to_owned(),
            },
            0,
            false,
        )];
        encounter.stages[0].child_stages_optional = true;
        let mut run = DungeonRun::new(Uuid::nil(), script(vec![encounter]));
        let mut host = Host::default();
        run.tick(&mut host, &view(&[PLACE]), T0);
        run.tick(&mut host, &view(&[PLACE]), T0 + 1);
        let mut events = view(&[PLACE]);
        events.events.push((1, "Fight_Complete".to_owned()));
        run.tick(&mut host, &events, T0 + 2);
        assert_eq!(host.removed, [1], "the living boss goes with its stage");
        assert!(
            run.encounters[0]
                .entity_events
                .contains(&(7, EVENT_ACTIVATE, T0 + 2))
        );
    }
}
