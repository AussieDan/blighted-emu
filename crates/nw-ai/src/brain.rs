// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-mob runtime: plans the archetype's tree each tick and executes the plan (behavior-tree
//! study, section 5.3).
//!
//! Planning (Observed in the client, `FUN_1426355b0` and the composite handlers around
//! `FUN_142635140`..`FUN_142635c00`): a plan is made from the root by entry checks; a Selector
//! takes the first child that plans; a Sequencer plans every child; a UtilitySelector plans every
//! child, collects (child, utility) of those that plan, and picks the highest (`Top1`) or draws by
//! utility weight among the top `WeightedRandomN` (`WeightedRandom`); it keeps its executing
//! choice when `AlwaysPreserveExecutingPlan` is set, and its last choice when
//! `PreserveExecutingPlanIfNoChange` is set, it is in the executing plan and the candidates and
//! utilities did not change. A selector that is not in the executing plan draws again (the client
//! checks the executing plan for the selector before both keeps). A composite's utility is the
//! highest of its own and its planned children's.
//!
//! Re-planning (Inferred from the retail capture `newchar-retail-2026-09-18`): the plan is made
//! again every tick; when it differs from the executing plan and every executing node is
//! `Interruptable`, the executing plan is cancelled and the new one starts. Evidence: the beach
//! boar's interruptable `Run` (mid band, 3 to 7 m) was cut after 470 ms when the target came
//! within the close band and the boar started `Attack_Gore` at 3.22 m; `Combat_Idle` entries of
//! the club sailors ended after 134 to 999 ms, when their attack cooldown ran out, although the
//! idle fragment loops. A finished plan is followed by a new plan in the same tick (retail: the
//! boar's second `Move_Combat` started 0 ms after its `Attack_Gore` ended).
//!
//! The runtime does not move, animate or damage anything: it emits [`Command`]s and reads the
//! world through [`World`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::data::{AiData, Archetype, CageAction, TagEffect};
use crate::keys::{
    CAGE_TASK_PREFIX, EngineKey, KeySource, STATUS_CATEGORY_PREFIX, STATUS_EFFECT_PREFIX,
};
use crate::node::{
    EntityQueryParams, FactOp, NodeId, NodeKind, PositionType, SelectPositionParams,
    SelectionMethod, TrackerParams,
};
use crate::rng::Rng48;
use crate::value::{Blackboard, ConditionGroup, KeyId, NameId, Param, Value};

/// Plans made at most in one tick (our engineering limit: a tree whose plan finishes at once,
/// for example a lone `SetCooldownAction`, would otherwise plan forever).
pub const MAX_PLANS_PER_TICK: usize = 4;
/// The slot of the root node.
const ROOT_SLOT: Slot = 0x9E37_79B9_7F4A_7C15;
/// Children a `Parallel` can track (the trees have 2).
const MAX_PARALLEL_CHILDREN: u32 = u64::BITS;
const MS_PER_S: f64 = 1000.0;

/// Whole milliseconds of `seconds` (rounded: `1.8 - 0.4` in `f32` is `1.39999`).
fn ms_of(seconds: f64) -> u64 {
    (seconds * MS_PER_S).round() as u64
}
/// `Parallel.CompletionMode` (names Observed: AllCompleted, FirstCompleted, AnyCompleted; the
/// numbers 0, 1, 2 are Inferred from the name order; the trees use 0).
const COMPLETION_ALL: i64 = 0;
const COMPLETION_FIRST: i64 = 1;
const COMPLETION_ANY: i64 = 2;
/// `-1` in a target limit: not set.
const UNSET_LIMIT: f64 = -1.0;
/// splitmix64 constants for the slot of a child.
const MIX_A: u64 = 0xBF58_476D_1CE4_E5B9;
const MIX_B: u64 = 0x94D0_49BB_1331_11EB;
const MIX_SHIFT_A: u32 = 30;
const MIX_SHIFT_B: u32 = 27;
const MIX_SHIFT_C: u32 = 31;

/// The place of a node in the expanded tree: the same node can sit in several places (shared
/// sub-trees), each with its own state.
pub type Slot = u64;

fn slot_of(parent: Slot, index: usize) -> Slot {
    let mut value = parent ^ (index as u64 + 1).wrapping_mul(ROOT_SLOT);
    value = (value ^ (value >> MIX_SHIFT_A)).wrapping_mul(MIX_A);
    value = (value ^ (value >> MIX_SHIFT_B)).wrapping_mul(MIX_B);
    value ^ (value >> MIX_SHIFT_C)
}

/// Where a navigation action goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Destination {
    /// A point.
    Point([f32; 3]),
    /// The current target (or action target); the sim follows it.
    Target,
}

/// What the sim must do for a mob.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Play a CAGE task. `action` is the resolved grid action; `done_at_ms` is when the tree
    /// moves on (`None`: when the plan changes).
    PlayTask {
        /// The node's slot.
        slot: Slot,
        /// The node.
        node: NodeId,
        /// The CAGE task.
        task: NameId,
        /// Index in [`AiData::cage_actions`].
        action: u32,
        /// When the tree moves on.
        done_at_ms: Option<u64>,
    },
    /// The task of `slot` was cancelled before its end.
    EndTask {
        /// The node's slot.
        slot: Slot,
    },
    /// Move on the navmesh while playing `task`; report the result with
    /// [`Brain::report_motion`].
    Navigate {
        /// The node's slot.
        slot: Slot,
        /// The `NavigationAction` node (its fields hold the steering values).
        node: NodeId,
        /// The move task.
        task: NameId,
        /// Where to.
        destination: Destination,
        /// The slice navigation config the node names, by index in the archetype's
        /// `nav_configs`, when the slice has it (retail boar runs reached 6.96 m/s: the slice
        /// `RunToTarget` maximum 7.0, not the node's 5.0; Observed).
        config: Option<usize>,
    },
    /// Strafe around the target while playing `task`.
    Strafe {
        /// The node's slot.
        slot: Slot,
        /// The `StrafeAction` node.
        node: NodeId,
        /// The strafe task.
        task: NameId,
        /// The slice navigation config the node names, by index in the archetype's
        /// `nav_configs`.
        config: Option<usize>,
    },
    /// Stop the move of `slot`.
    StopMotion {
        /// The node's slot.
        slot: Slot,
    },
    /// A Mannequin tag of the mob changed.
    Tag {
        /// The tag.
        tag: NameId,
        /// Set or cleared.
        set: bool,
    },
    /// Apply or remove a status effect on the mob.
    StatusEffect {
        /// The status effect id.
        effect: NameId,
        /// Apply or remove.
        apply: bool,
        /// The `StatusEffectDecorator` node.
        node: NodeId,
    },
    /// Keep the current target (`LockTargetSelection`).
    LockTarget(bool),
    /// Target a position from the blackboard, or stop (`ManualTargeting`).
    ManualTarget(Option<[f32; 3]>),
    /// Track action targets with the node's targeting fields, or stop.
    TrackActionTargets(Option<NodeId>),
    /// Send an encounter event.
    EncounterEvent(NameId),
    /// A leaf that acts on other entities or spawns: the node holds its fields. `target_index`
    /// is the target of an enclosing `ForEachTargetDecorator`.
    Action {
        /// The node.
        node: NodeId,
        /// The target index, inside a `ForEachTargetDecorator`.
        target_index: Option<u32>,
    },
}

/// The result of a move the sim reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionOutcome {
    /// It arrived (or the strafe ended).
    Arrived,
    /// It could not go on.
    Failed,
}

/// What the runtime reads of the world.
pub trait World {
    /// Milliseconds on the sim clock.
    fn now_ms(&self) -> u64;
    /// An engine blackboard key ([`crate::keys`]).
    fn engine_value(&self, key: EngineKey) -> Option<Value>;
    /// Whether a status effect with this id is active on the mob.
    fn has_status_effect(&self, effect: &str) -> bool;
    /// Whether a status effect of this category is active on the mob.
    fn has_status_category(&self, category: &str) -> bool;
    /// A position the trees name.
    fn position(&self, of: PositionType) -> Option<[f32; 3]>;
    /// How many action targets the tracker's targeting fields find.
    fn action_targets(&self, params: &TrackerParams) -> u32;
    /// Whether an `EntityQueryCondition` passes (its rule is **Unknown**: the sim owns it).
    fn entity_query(&self, params: &EntityQueryParams, board: &dyn Blackboard) -> bool;
    /// How many entities have the tag.
    fn entities_with_tag(&self, tag: &str) -> u32;
    /// Whether the mob follows a patrol path (`SelectWaypointAction` advances it; Inferred: the
    /// action cannot plan without one, or every mob's Patrol sub-tree would take it).
    fn has_patrol(&self) -> bool;
    /// Whether nothing blocks the line from `from` to `to`.
    fn line_of_sight(&self, from: [f32; 3], to: [f32; 3]) -> bool;
    /// Whether the navmesh has room for an action that moves `distance` metres at `direction`
    /// degrees from the facing, `from_wall` metres from walls.
    fn nav_clearance(&self, direction_deg: f64, distance: f64, from_wall: f64) -> bool;
    /// A navmesh point picked by the fields of `params` (resolve them with `board`).
    fn select_position(
        &self,
        params: &SelectPositionParams,
        board: &dyn Blackboard,
    ) -> Option<[f32; 3]>;
}

/// The shared part: the data and where each key's value comes from.
#[derive(Debug)]
pub struct Runtime {
    data: Arc<AiData>,
    /// Every name of the data, by text (the data's own lookup scans its whole name list).
    name_ids: HashMap<String, NameId>,
    sources: Vec<KeySource>,
    selected_pos: Option<KeyId>,
    /// `CAGE_<task>` keys by task, sorted by task.
    task_keys: Vec<(NameId, KeyId)>,
}

impl Runtime {
    /// The runtime of `data`.
    pub fn new(data: impl Into<Arc<AiData>>) -> Self {
        let data: Arc<AiData> = data.into();
        let name_ids: HashMap<String, NameId> = data
            .names
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, name)| Some((name.clone(), NameId::try_from(index).ok()?)))
            .collect();
        let name_id = |text: &str| name_ids.get(text).copied();
        let sources = (0..data.keys.len())
            .map(|key| {
                let name = data.key_name(KeyId::try_from(key).unwrap_or(KeyId::MAX));
                if let Some(engine) = EngineKey::of(name) {
                    KeySource::Engine(engine)
                } else if let Some(category) = name.strip_prefix(STATUS_CATEGORY_PREFIX) {
                    name_id(category).map_or(KeySource::Tree, KeySource::StatusCategory)
                } else if let Some(effect) = name.strip_prefix(STATUS_EFFECT_PREFIX) {
                    name_id(effect).map_or(KeySource::Tree, KeySource::StatusEffect)
                } else {
                    KeySource::Tree
                }
            })
            .collect();
        let mut task_keys: Vec<(NameId, KeyId)> = (0..data.keys.len())
            .filter_map(|key| {
                let key = KeyId::try_from(key).ok()?;
                let task = data.key_name(key).strip_prefix(CAGE_TASK_PREFIX)?;
                Some((name_id(task)?, key))
            })
            .collect();
        task_keys.sort_unstable();
        let selected_pos = data.key(EngineKey::SelectedPos.name());
        Self {
            data,
            name_ids,
            sources,
            selected_pos,
            task_keys,
        }
    }

    /// The id of name `text`, when the data has it (a hash lookup).
    pub fn name_id(&self, text: &str) -> Option<NameId> {
        self.name_ids.get(text).copied()
    }

    /// The `CAGE_<task>` key of `task`, when a tree reads one.
    fn task_key(&self, task: NameId) -> Option<KeyId> {
        self.task_keys
            .binary_search_by_key(&task, |(known, _)| *known)
            .ok()
            .map(|index| self.task_keys[index].1)
    }

    /// The data.
    pub fn data(&self) -> &AiData {
        &self.data
    }

    /// Where the value of `key` comes from.
    pub fn source(&self, key: KeyId) -> KeySource {
        self.sources
            .get(usize::from(key))
            .copied()
            .unwrap_or(KeySource::Tree)
    }
}

/// The node status of the client (`FUN_14263c780` success, `FUN_142638f20` failure,
/// `FUN_142634ef0` waiting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    Success,
    Failure,
}

/// The state of an executing node.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    /// Entered; nothing else to keep.
    Entered,
    /// Composite: the child it runs.
    Index(u32),
    /// Parallel: children finished and children failed (bits).
    Parallel { finished: u64, failed: u64 },
    /// Timer or Delay: the end.
    Until(u64),
    /// Repeater: runs left and the end of the time limit.
    Repeat { left: i32, until: Deadline },
    /// VariableMonitor: the value when it started.
    Monitor(Option<Value>),
    /// A leaf that waits: when it is done and when it fails.
    Leaf {
        done_at: Deadline,
        fail_at: Deadline,
    },
}

/// A time in milliseconds, or none, in 8 bytes (the per-mob state budget, study section 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Deadline(u64);

impl Deadline {
    const NONE: u64 = u64::MAX;

    fn of(time_ms: Option<u64>) -> Self {
        Self(time_ms.unwrap_or(Self::NONE))
    }

    fn get(self) -> Option<u64> {
        (self.0 != Self::NONE).then_some(self.0)
    }

    /// Whether the time has come at `now`.
    fn reached(self, now: u64) -> bool {
        self.get().is_some_and(|at| now >= at)
    }
}

/// One executing node.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Active {
    slot: Slot,
    node: NodeId,
    state: State,
}

/// A tag change that waits for its `DelayTimer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DelayedTag {
    at_ms: u64,
    tag: NameId,
    set: bool,
}

/// A child of a UtilitySelector that planned: its index, utility and own plan choices.
type Candidate = (u16, f32, Vec<(Slot, u16)>);

/// What a UtilitySelector chose last, for `PreserveExecutingPlanIfNoChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Choice {
    slot: Slot,
    signature: u64,
    child: u16,
}

/// One mob's AI state.
#[derive(Debug, Clone)]
pub struct Brain {
    archetype: u32,
    root: Option<NodeId>,
    rng: Rng48,
    facts: Vec<(KeyId, Value)>,
    tags: Vec<NameId>,
    cooldowns: Vec<(NameId, u64)>,
    /// FrequencyLimiter: the time it opens again.
    limiters: Vec<(Slot, u64)>,
    /// CountLimiter runs and EncounterEvent sends.
    counts: Vec<(Slot, u32)>,
    choices: Vec<Choice>,
    plan: Vec<(Slot, u16)>,
    active: Vec<Active>,
    delayed_tags: Vec<DelayedTag>,
    motion: Option<(Slot, MotionOutcome)>,
    /// Cooldown times that replace the tree's (seconds): an override of the mob's spawn.
    cooldown_times: Vec<(NameId, f64, f64)>,
}

/// A mob that cannot get a brain.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrainError {
    /// The archetype index is out of range.
    #[error("archetype {0} is not in the data")]
    NoArchetype(u32),
}

impl Brain {
    /// The brain of a mob of archetype `archetype`, with random numbers from `seed`. Its tree is
    /// the archetype's first tree that Server.pak has; `None` leaves the mob without a plan.
    ///
    /// # Errors
    ///
    /// Returns [`BrainError::NoArchetype`] when `archetype` is out of range.
    pub fn new(runtime: &Runtime, archetype: u32, seed: u64) -> Result<Self, BrainError> {
        let data = runtime.data();
        let arch = data
            .archetypes
            .get(archetype as usize)
            .ok_or(BrainError::NoArchetype(archetype))?;
        let root = arch
            .trees
            .iter()
            .find_map(|tree| tree.tree)
            .and_then(|tree| data.trees.get(tree as usize))
            .and_then(|tree| tree.root);
        Ok(Self {
            archetype,
            root,
            rng: Rng48::new(seed),
            facts: Vec::new(),
            tags: arch.default_tags.clone(),
            cooldowns: Vec::new(),
            limiters: Vec::new(),
            counts: Vec::new(),
            choices: Vec::new(),
            plan: Vec::new(),
            active: Vec::new(),
            delayed_tags: Vec::new(),
            motion: None,
            cooldown_times: Vec::new(),
        })
    }

    /// The mob's archetype index.
    pub fn archetype(&self) -> u32 {
        self.archetype
    }

    /// Whether the mob has Mannequin tag `tag`.
    pub fn has_tag(&self, tag: NameId) -> bool {
        self.tags.contains(&tag)
    }

    /// The mob's Mannequin tags.
    pub fn tags(&self) -> &[NameId] {
        &self.tags
    }

    /// The tree's own value of `key` (facts the tree wrote).
    pub fn fact(&self, key: KeyId) -> Option<Value> {
        self.facts
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, value)| *value)
    }

    /// When cooldown `name` ends, while it runs at `now_ms`.
    pub fn cooldown_until(&self, name: NameId, now_ms: u64) -> Option<u64> {
        self.cooldowns
            .iter()
            .find(|(known, until)| *known == name && *until > now_ms)
            .map(|(_, until)| *until)
    }

    /// The leaves executing now.
    pub fn running_leaves<'a>(&'a self, data: &'a AiData) -> impl Iterator<Item = NodeId> + 'a {
        self.active.iter().map(|active| active.node).filter(|node| {
            let kind = &data.node(*node).kind;
            kind.children().is_none()
                && !kind.is_decorator()
                && !matches!(kind, NodeKind::SubTree { .. })
        })
    }

    /// Sets the mob's own value of `key`, as a tree fact (a spawn's override of a slice config
    /// value: `m_configParams` are the blackboard's start values, study section 3.1).
    pub fn set_fact(&mut self, key: KeyId, value: Value) {
        match self.facts.iter_mut().find(|(known, _)| *known == key) {
            Some(entry) => entry.1 = value,
            None => self.facts.push((key, value)),
        }
    }

    /// Every cooldown `name` the tree starts lasts a random time from `min_s` to `max_s` from now
    /// on, in place of the node's times (a spawn's override).
    pub fn set_cooldown_times(&mut self, name: NameId, min_s: f64, max_s: f64) {
        self.cooldown_times.retain(|(known, _, _)| *known != name);
        self.cooldown_times.push((name, min_s, max_s));
    }

    /// Restarts the random numbers from `seed`.
    pub fn reseed(&mut self, seed: u64) {
        self.rng = Rng48::new(seed);
    }

    /// The sim started a CAGE action the engine plays itself (a grid action without a task: a
    /// hit `Reaction`, a `Death`; index in [`AiData::cage_actions`]): the executing plan is
    /// cancelled and the action's `SetMannequinTag` enter effects apply (Observed: the boar's
    /// `Reaction` sets `Alerted` on enter, `boar_reaction.actionlist`).
    pub fn engine_action(
        &mut self,
        runtime: &Runtime,
        world: &impl World,
        action: u32,
        out: &mut Vec<Command>,
    ) {
        self.engine_tags(runtime, world, action, true, out);
    }

    /// The engine action `action` ended: its `SetMannequinTag` exit effects apply.
    pub fn engine_action_end(
        &mut self,
        runtime: &Runtime,
        world: &impl World,
        action: u32,
        out: &mut Vec<Command>,
    ) {
        self.engine_tags(runtime, world, action, false, out);
    }

    fn engine_tags(
        &mut self,
        runtime: &Runtime,
        world: &impl World,
        action: u32,
        enter: bool,
        out: &mut Vec<Command>,
    ) {
        let data = runtime.data();
        let (Some(arch), Some(cage)) = (
            data.archetypes.get(self.archetype as usize),
            data.cage_actions.get(action as usize),
        ) else {
            return;
        };
        let mut ctx = Ctx {
            rt: runtime,
            arch,
            world,
            now: world.now_ms(),
            brain: self,
            out,
        };
        if enter {
            ctx.cancel_all();
        }
        for activity in &cage.tags {
            let effect = if enter {
                activity.on_enter
            } else {
                activity.on_exit
            };
            ctx.tag_effect(
                activity.tag,
                effect,
                if enter { activity.delay_s } else { 0.0 },
            );
        }
    }

    /// The sim reports the end of the move of `slot`, or of a task of `slot` that has no end of
    /// its own (a looping fragment the engine ends, for example a waypoint task after its
    /// `Waypoint_Duration`; Inferred from the Patrol sub-tree, whose `POI_Waypoint_Loop` task
    /// loops and must end for the patrol to go on).
    pub fn report_motion(&mut self, slot: Slot, outcome: MotionOutcome) {
        self.motion = Some((slot, outcome));
    }

    /// Cancels the executing plan (the sim interrupted the mob, for example a hit reaction); the
    /// next tick plans again.
    pub fn interrupt(&mut self, runtime: &Runtime, world: &impl World, out: &mut Vec<Command>) {
        let Some(arch) = runtime.data().archetypes.get(self.archetype as usize) else {
            return;
        };
        let mut ctx = Ctx {
            rt: runtime,
            arch,
            world,
            now: world.now_ms(),
            brain: self,
            out,
        };
        ctx.cancel_all();
    }

    /// The value of a node parameter for this mob (the sim reads the fields of the nodes in
    /// [`Command`]s with it, for example a navigation node's `MaxSpeed`).
    pub fn resolve(&self, runtime: &Runtime, world: &impl World, param: &Param) -> Option<Value> {
        let arch = runtime.data().archetypes.get(self.archetype as usize)?;
        param.resolve(&board(runtime, arch, world, &self.facts))
    }

    /// Bytes of the brain's state (inline and on the heap), for the memory budget.
    pub fn state_bytes(&self) -> usize {
        size_of::<Self>()
            + self.facts.capacity() * size_of::<(KeyId, Value)>()
            + self.tags.capacity() * size_of::<NameId>()
            + self.cooldowns.capacity() * size_of::<(NameId, u64)>()
            + self.limiters.capacity() * size_of::<(Slot, u64)>()
            + self.counts.capacity() * size_of::<(Slot, u32)>()
            + self.choices.capacity() * size_of::<Choice>()
            + self.plan.capacity() * size_of::<(Slot, u16)>()
            + self.active.capacity() * size_of::<Active>()
            + self.delayed_tags.capacity() * size_of::<DelayedTag>()
            + self.cooldown_times.capacity() * size_of::<(NameId, f64, f64)>()
    }

    /// One AI tick: plan, then execute; commands go to `out`.
    pub fn tick(&mut self, runtime: &Runtime, world: &impl World, out: &mut Vec<Command>) {
        let Some(arch) = runtime.data().archetypes.get(self.archetype as usize) else {
            return;
        };
        let Some(root) = self.root else {
            return;
        };
        let now = world.now_ms();
        let mut ctx = Ctx {
            rt: runtime,
            arch,
            world,
            now,
            brain: self,
            out,
        };
        ctx.apply_delayed_tags();
        for _ in 0..MAX_PLANS_PER_TICK {
            // A plan that cannot be cut is not planned against: no draw, no remembered choice
            // (Inferred; otherwise a UtilitySelector would fix its weighted pick while, for
            // example, the boar's Turn still runs).
            let idle = ctx.brain.active.is_empty();
            if idle || ctx.interruptable() {
                let mut fresh = Vec::new();
                let planned = ctx.plan(ROOT_SLOT, root, &mut fresh).is_some();
                if idle {
                    if !planned {
                        return;
                    }
                    ctx.brain.plan = fresh;
                } else if planned && fresh != ctx.brain.plan {
                    ctx.cancel_all();
                    ctx.brain.plan = fresh;
                }
            }
            if ctx.exec(ROOT_SLOT, root, None) == Status::Running {
                return;
            }
            ctx.brain.plan.clear();
        }
    }
}

/// The blackboard of one mob as the conditions read it: the tree's facts, then the engine and
/// status effect keys, then the slice config.
struct Board<'a, W: World> {
    rt: &'a Runtime,
    arch: &'a Archetype,
    world: &'a W,
    facts: &'a [(KeyId, Value)],
}

impl<W: World> Blackboard for Board<'_, W> {
    fn value(&self, key: KeyId) -> Option<Value> {
        if let Some((_, value)) = self.facts.iter().find(|(known, _)| *known == key) {
            return Some(*value);
        }
        let from_world = match self.rt.source(key) {
            KeySource::Tree => None,
            KeySource::Engine(engine) => self.world.engine_value(engine),
            KeySource::StatusEffect(effect) => Some(Value::Bool(
                self.world.has_status_effect(self.rt.data().name(effect)),
            )),
            KeySource::StatusCategory(category) => Some(Value::Bool(
                self.world
                    .has_status_category(self.rt.data().name(category)),
            )),
        };
        from_world.or_else(|| self.arch.config_value(key))
    }

    fn dynamic_scale(&self) -> Option<f64> {
        self.world
            .engine_value(EngineKey::DynamicEntityScalar)
            .and_then(Value::as_number)
    }
}

/// One tick's working set.
struct Ctx<'a, 'b, W: World> {
    rt: &'a Runtime,
    arch: &'a Archetype,
    world: &'a W,
    now: u64,
    brain: &'b mut Brain,
    out: &'b mut Vec<Command>,
}

fn board<'a, W: World>(
    rt: &'a Runtime,
    arch: &'a Archetype,
    world: &'a W,
    facts: &'a [(KeyId, Value)],
) -> Board<'a, W> {
    Board {
        rt,
        arch,
        world,
        facts,
    }
}

impl<'a, W: World> Ctx<'a, '_, W> {
    fn data(&self) -> &'a AiData {
        self.rt.data()
    }

    fn holds(&self, group: &ConditionGroup) -> bool {
        group.holds(&board(self.rt, self.arch, self.world, &self.brain.facts))
    }

    fn number(&self, param: &Param) -> Option<f64> {
        param.number(&board(self.rt, self.arch, self.world, &self.brain.facts))
    }

    fn resolve(&self, param: &Param) -> Option<Value> {
        param.resolve(&board(self.rt, self.arch, self.world, &self.brain.facts))
    }

    /// Milliseconds from now after a random time between two parameters (seconds).
    fn random_end(&mut self, min: &Param, max: &Param) -> Option<u64> {
        let (min, max) = (self.number(min)?, self.number(max)?);
        Some(self.random_until(min, max))
    }

    /// Milliseconds from now after a random time between `min` and `max` seconds.
    fn random_until(&mut self, min: f64, max: f64) -> u64 {
        let seconds = self.brain.rng.range(min, max).max(0.0);
        // A whole number of milliseconds; the trees hold times below an hour.
        self.now + ms_of(seconds)
    }

    fn cooldown_active(&self, name: NameId) -> bool {
        self.brain.cooldown_until(name, self.now).is_some()
    }

    fn set_cooldown(&mut self, spec: &crate::node::CooldownSpec) {
        let overridden = self
            .brain
            .cooldown_times
            .iter()
            .find(|(name, _, _)| *name == spec.name)
            .map(|(_, min, max)| (*min, *max));
        let until = match overridden {
            Some((min, max)) => Some(self.random_until(min, max)),
            None => self.random_end(&spec.min, &spec.max),
        };
        let Some(until) = until else {
            return;
        };
        let running = self.brain.cooldown_until(spec.name, self.now);
        let until = match (spec.override_rule, running) {
            (crate::node::CooldownOverride::Never, Some(running)) => running,
            (crate::node::CooldownOverride::KeepLongest, Some(running)) => until.max(running),
            (crate::node::CooldownOverride::KeepShortest, Some(running)) => until.min(running),
            _ => until,
        };
        match self
            .brain
            .cooldowns
            .iter_mut()
            .find(|(known, _)| *known == spec.name)
        {
            Some(entry) => entry.1 = until,
            None => self.brain.cooldowns.push((spec.name, until)),
        }
    }

    fn count(&self, slot: Slot) -> u32 {
        self.brain
            .counts
            .iter()
            .find(|(known, _)| *known == slot)
            .map_or(0, |(_, count)| *count)
    }

    fn add_count(&mut self, slot: Slot) {
        match self
            .brain
            .counts
            .iter_mut()
            .find(|(known, _)| *known == slot)
        {
            Some(entry) => entry.1 += 1,
            None => self.brain.counts.push((slot, 1)),
        }
    }

    fn set_tag(&mut self, tag: NameId, set: bool) {
        let has = self.brain.tags.contains(&tag);
        if set && !has {
            self.brain.tags.push(tag);
        } else if !set && has {
            self.brain.tags.retain(|known| *known != tag);
        } else {
            return;
        }
        self.out.push(Command::Tag { tag, set });
    }

    fn apply_delayed_tags(&mut self) {
        let now = self.now;
        let due: Vec<DelayedTag> = self
            .brain
            .delayed_tags
            .iter()
            .copied()
            .filter(|delayed| delayed.at_ms <= now)
            .collect();
        self.brain
            .delayed_tags
            .retain(|delayed| delayed.at_ms > now);
        for delayed in due {
            self.set_tag(delayed.tag, delayed.set);
        }
    }

    fn tag_effect(&mut self, tag: NameId, effect: TagEffect, delay_s: f32) {
        let set = match effect {
            TagEffect::Set => true,
            TagEffect::Clear => false,
            TagEffect::NoEffect => return,
        };
        if delay_s > 0.0 {
            self.brain.delayed_tags.push(DelayedTag {
                at_ms: self.now + ms_of(f64::from(delay_s)),
                tag,
                set,
            });
        } else {
            self.set_tag(tag, set);
        }
    }

    fn cage_action(&self, task: NameId) -> Option<(u32, &'a CageAction)> {
        let data = self.data();
        let grid = data.grids.get(self.arch.grid? as usize)?;
        let index = grid.action(task)?;
        Some((index, data.cage_actions.get(index as usize)?))
    }

    /// The CAGE task a node's `ActionName` names for this mob.
    fn task(&self, task: &Param) -> Option<NameId> {
        match self.resolve(task)? {
            Value::Text(task) => Some(task),
            _ => None,
        }
    }

    fn nav_config(&self, kind: Option<NameId>) -> Option<usize> {
        self.arch.nav_config(kind?)
    }

    fn clearance(&self, ray: &crate::node::NavRayCast) -> bool {
        let distance = self.number(&ray.distance).unwrap_or(0.0);
        if distance <= 0.0 {
            return true;
        }
        let direction = self.number(&ray.direction).unwrap_or(0.0);
        let from_wall = self.number(&ray.distance_from_wall).unwrap_or(0.0);
        self.world.nav_clearance(direction, distance, from_wall)
    }

    // ---- planning ----

    /// Plans node `id` at `slot`; its choices go to `choices`. `None`: it does not plan.
    fn plan(&mut self, slot: Slot, id: NodeId, choices: &mut Vec<(Slot, u16)>) -> Option<f32> {
        let data = self.rt.data();
        let node = data.node(id);
        let utility = match &node.kind {
            NodeKind::Sequencer(_) | NodeKind::ForEachTarget { .. } | NodeKind::Parallel { .. } => {
                self.plan_all(slot, id, choices, false)
            }
            NodeKind::PermissiveSequencer(_) => self.plan_all(slot, id, choices, true),
            NodeKind::Selector(_) => self.plan_first(slot, id, choices),
            NodeKind::UtilitySelector { .. } => self.plan_utility(slot, id, choices),
            NodeKind::SubTree { root, .. } => {
                root.and_then(|root| self.plan(slot_of(slot, 0), root, choices))
            }
            kind if kind.is_decorator() => match kind.child() {
                Some(child) if self.gate_passes(slot, kind) => {
                    self.plan_child(slot, child, choices, kind)
                }
                _ => None,
            },
            leaf => self.gate_passes(slot, leaf).then_some(node.utility),
        };
        utility.map(|value| value.max(node.utility))
    }

    fn plan_child(
        &mut self,
        slot: Slot,
        child: NodeId,
        choices: &mut Vec<(Slot, u16)>,
        kind: &NodeKind,
    ) -> Option<f32> {
        let mark = choices.len();
        let planned = self.plan(slot_of(slot, 0), child, choices);
        if planned.is_none() {
            choices.truncate(mark);
            if let NodeKind::AlwaysSucceed {
                during_evaluation: true,
                ..
            } = kind
            {
                return Some(0.0);
            }
        }
        planned
    }

    fn plan_all(
        &mut self,
        slot: Slot,
        id: NodeId,
        choices: &mut Vec<(Slot, u16)>,
        permissive: bool,
    ) -> Option<f32> {
        let children = self.rt.data().children(id);
        let mut best: f32 = 0.0;
        for (index, child) in children.iter().enumerate() {
            let mark = choices.len();
            match self.plan(slot_of(slot, index), *child, choices) {
                Some(utility) => best = best.max(utility),
                None if permissive => choices.truncate(mark),
                None => return None,
            }
        }
        Some(best)
    }

    fn plan_first(
        &mut self,
        slot: Slot,
        id: NodeId,
        choices: &mut Vec<(Slot, u16)>,
    ) -> Option<f32> {
        let children = self.rt.data().children(id);
        for (index, child) in children.iter().enumerate() {
            let mark = choices.len();
            if let Some(utility) = self.plan(slot_of(slot, index), *child, choices) {
                choices.insert(mark, (slot, u16::try_from(index).unwrap_or(u16::MAX)));
                return Some(utility);
            }
            choices.truncate(mark);
        }
        None
    }

    fn plan_utility(
        &mut self,
        slot: Slot,
        id: NodeId,
        choices: &mut Vec<(Slot, u16)>,
    ) -> Option<f32> {
        let data = self.rt.data();
        let NodeKind::UtilitySelector {
            method,
            top_n,
            preserve_if_no_change,
            always_preserve,
            ..
        } = data.node(id).kind
        else {
            return None;
        };
        let children = data.children(id);
        let mut candidates: Vec<Candidate> = Vec::new();
        for (index, child) in children.iter().enumerate() {
            let mut own = Vec::new();
            if let Some(utility) = self.plan(slot_of(slot, index), *child, &mut own) {
                candidates.push((u16::try_from(index).unwrap_or(u16::MAX), utility, own));
            }
        }
        if candidates.is_empty() {
            return None;
        }
        let signature = candidates
            .iter()
            .fold(ROOT_SLOT, |hash, (index, utility, _)| {
                slot_of(
                    slot_of(hash, usize::from(*index)),
                    utility.to_bits() as usize,
                )
            });
        let has = |child: u16| candidates.iter().any(|(index, _, _)| *index == child);
        let executing = self.brain.active.iter().any(|active| active.slot == slot);
        let current = self
            .brain
            .plan
            .iter()
            .find(|(known, _)| *known == slot)
            .map(|(_, child)| *child);
        let remembered = self
            .brain
            .choices
            .iter()
            .find(|choice| choice.slot == slot)
            .copied();
        let chosen = if let Some(child) =
            current.filter(|child| always_preserve && executing && has(*child))
        {
            child
        } else if let Some(choice) = remembered.filter(|choice| {
            preserve_if_no_change && executing && choice.signature == signature && has(choice.child)
        }) {
            choice.child
        } else {
            self.draw(&candidates, method, top_n)
        };
        match self
            .brain
            .choices
            .iter_mut()
            .find(|choice| choice.slot == slot)
        {
            Some(choice) => {
                choice.signature = signature;
                choice.child = chosen;
            }
            None => self.brain.choices.push(Choice {
                slot,
                signature,
                child: chosen,
            }),
        }
        let (_, utility, own) = candidates
            .into_iter()
            .find(|(index, _, _)| *index == chosen)?;
        choices.push((slot, chosen));
        choices.extend(own);
        Some(utility)
    }

    /// The UtilitySelector pick (Observed, `FUN_1426355b0`): candidates sorted by utility;
    /// `Top1` takes the highest; `WeightedRandom` draws `r` in `[0, 1)` and walks down from the
    /// highest of the top `n`, adding utilities, until the sum passes `r` times their total.
    /// Equal utilities keep child order (the client's sort order for ties is **Unknown**).
    fn draw(&mut self, candidates: &[Candidate], method: SelectionMethod, top_n: u64) -> u16 {
        let mut sorted: Vec<(u16, f32)> = candidates
            .iter()
            .map(|(index, utility, _)| (*index, *utility))
            .collect();
        sorted.sort_by(|left, right| left.1.total_cmp(&right.1));
        match method {
            SelectionMethod::Top => sorted.last().map_or(0, |(index, _)| *index),
            SelectionMethod::WeightedRandom => {
                let take = usize::try_from(top_n)
                    .unwrap_or(usize::MAX)
                    .min(sorted.len());
                let top = &sorted[sorted.len() - take..];
                let total: f32 = top.iter().map(|(_, utility)| utility).sum();
                let goal = self.brain.rng.unit() * total;
                let mut sum = 0.0;
                for (index, utility) in top.iter().rev() {
                    sum += utility;
                    if sum > goal {
                        return *index;
                    }
                }
                top.first().map_or(0, |(index, _)| *index)
            }
        }
    }

    /// The plan-time check of a decorator or leaf class at `slot`.
    fn gate_passes(&mut self, slot: Slot, kind: &NodeKind) -> bool {
        match kind {
            NodeKind::BlackboardCondition { gates, .. } => self.holds(&gates.entry),
            NodeKind::DistanceCondition {
                gates,
                vertical_offset,
                vertical_threshold,
                allow_dynamic_scaling,
                ..
            } => self.distance_holds(
                &gates.entry,
                vertical_offset,
                vertical_threshold,
                *allow_dynamic_scaling,
            ),
            NodeKind::EntityQuery { params, .. } => {
                let board = board(self.rt, self.arch, self.world, &self.brain.facts);
                self.world.entity_query(params, &board)
            }
            NodeKind::Cooldown { cooldown, .. } => !self.cooldown_active(cooldown.name),
            NodeKind::IsCooldownActive { name, invert, .. } => {
                self.cooldown_active(*name) != *invert
            }
            NodeKind::MannequinTag { tag, invert, .. } => self.brain.has_tag(*tag) != *invert,
            NodeKind::ActionTargetTracker { params, .. } => {
                let min = self
                    .number(&params.min_targets)
                    .filter(|min| *min != UNSET_LIMIT)
                    .unwrap_or(1.0);
                f64::from(self.world.action_targets(params)) >= min
            }
            NodeKind::FrequencyLimiter { .. } => self
                .brain
                .limiters
                .iter()
                .find(|(known, _)| *known == slot)
                .is_none_or(|(_, until)| self.now >= *until),
            NodeKind::CountLimiter { max, .. } => self
                .number(max)
                .is_some_and(|max| f64::from(self.count(slot)) < max),
            NodeKind::VariableExists { key, invert, .. } => {
                let board = board(self.rt, self.arch, self.world, &self.brain.facts);
                board.value(*key).is_some() != *invert
            }
            NodeKind::RandomChance { rate, .. } => {
                let Some(rate) = self.number(rate) else {
                    return false;
                };
                f64::from(self.brain.rng.unit()) < rate
            }
            NodeKind::VisibilityRaycast {
                start, end, offset, ..
            } => {
                let (Some(from), Some(mut to)) =
                    (self.world.position(*start), self.world.position(*end))
                else {
                    return false;
                };
                to[2] += self.number(offset).unwrap_or(0.0) as f32;
                self.world.line_of_sight(from, to)
            }
            NodeKind::ManualTargeting { key, .. } => self
                .resolve(&Param::of_key(*key))
                .and_then(Value::as_vec3)
                .is_some(),
            NodeKind::Cage(cage) => {
                cage.nav_ray_cast.check_at_execution || self.clearance(&cage.nav_ray_cast)
            }
            NodeKind::Navigation(nav) => {
                (nav.cage.nav_ray_cast.check_at_execution || self.clearance(&nav.cage.nav_ray_cast))
                    && nav.nav_key.is_none_or(|key| {
                        self.resolve(&Param::of_key(key))
                            .and_then(Value::as_vec3)
                            .is_some()
                    })
            }
            NodeKind::Strafe(strafe) => {
                strafe.cage.nav_ray_cast.check_at_execution
                    || self.clearance(&strafe.cage.nav_ray_cast)
            }
            NodeKind::SelectWaypoint => self.world.has_patrol(),
            _ => true,
        }
    }

    /// `DistanceCondition`: the left operand is a position, the right a distance; the distance
    /// is the horizontal one from the mob (the client logs "Left operand cannot resolve to
    /// Vector3" and "Right operand cannot resolve to float"). For a point whose height
    /// difference minus `VerticalOffset` is beyond `VerticalThreshold`, the distance counts the
    /// height too (Inferred). Evidence: no retail mob reset in 53 fights of three captures
    /// (newchar-retail, newchar-linux, general-other-01; the tutorial club sailor fought 3.7 m
    /// off its home's height 29 m away), and retail resets came 61 to 73 m from where the mob
    /// started, so a height gap alone does not pass a leash (live 2026-10-31: the earlier rule,
    /// "farther" always holds beyond the threshold, reset Janszoon 25 m from his waypoint and
    /// 2.1 m below it, his leash 45 m); and `ResetToHome`'s `Spawn_Pos GreaterThan 1.0` with a
    /// 2 m threshold must hold for Janszoon 37 m from his spawn and 2.3 m below it, so that he
    /// walks home (live 2026-10-29).
    fn distance_holds(
        &self,
        group: &ConditionGroup,
        vertical_offset: &Param,
        vertical_threshold: &Param,
        allow_dynamic_scaling: bool,
    ) -> bool {
        let Some(own) = self.world.position(PositionType::Own) else {
            return false;
        };
        let board = board(self.rt, self.arch, self.world, &self.brain.facts);
        let offset = vertical_offset.number(&board).unwrap_or(0.0);
        let threshold = vertical_threshold.number(&board);
        let check = |condition: &crate::value::Condition| {
            let Some(point) = condition.left.resolve(&board).and_then(Value::as_vec3) else {
                return false;
            };
            let Some(right) = condition.right.resolve(&board) else {
                return false;
            };
            let right = if allow_dynamic_scaling {
                condition.scaled(right, &board)
            } else {
                right
            };
            let dx = f64::from(point[0] - own[0]);
            let dy = f64::from(point[1] - own[1]);
            let rise = f64::from(point[2] - own[2]) - offset;
            let within = threshold.is_none_or(|limit| rise.abs() <= limit);
            let distance = if within {
                dx.hypot(dy)
            } else {
                dx.hypot(dy).hypot(rise)
            };
            // Distances of a few hundred metres.
            condition.op.compare(Value::Float(distance as f32), right)
        };
        if group.any {
            group.conditions.is_empty() || group.conditions.iter().any(check)
        } else {
            group.conditions.iter().all(check)
        }
    }

    // ---- execution ----

    fn state(&self, slot: Slot) -> Option<State> {
        self.brain
            .active
            .iter()
            .find(|active| active.slot == slot)
            .map(|active| active.state)
    }

    fn set_state(&mut self, slot: Slot, node: NodeId, state: State) {
        match self
            .brain
            .active
            .iter_mut()
            .find(|active| active.slot == slot)
        {
            Some(active) => active.state = state,
            None => self.brain.active.push(Active { slot, node, state }),
        }
    }

    fn remove(&mut self, slot: Slot) {
        self.brain.active.retain(|active| active.slot != slot);
    }

    fn interruptable(&self) -> bool {
        let data = self.rt.data();
        self.brain
            .active
            .iter()
            .all(|active| data.node(active.node).interruptable)
    }

    /// The child a selector chose when the plan was made.
    fn planned_choice(&self, slot: Slot) -> Option<u16> {
        self.brain
            .plan
            .iter()
            .find(|(known, _)| *known == slot)
            .map(|(_, child)| *child)
    }

    fn exec(&mut self, slot: Slot, id: NodeId, target: Option<u32>) -> Status {
        let data = self.rt.data();
        let kind = &data.node(id).kind;
        match kind {
            NodeKind::Sequencer(_)
            | NodeKind::PermissiveSequencer(_)
            | NodeKind::ForEachTarget { .. } => {
                let permissive = matches!(kind, NodeKind::PermissiveSequencer(_));
                let each = matches!(kind, NodeKind::ForEachTarget { .. });
                let children = data.children(id);
                let mut index = match self.state(slot) {
                    Some(State::Index(index)) => index as usize,
                    _ => 0,
                };
                loop {
                    let Some(child) = children.get(index) else {
                        self.remove(slot);
                        return Status::Success;
                    };
                    self.set_state(slot, id, State::Index(index as u32));
                    let target = if each {
                        u32::try_from(index).ok()
                    } else {
                        target
                    };
                    match self.exec(slot_of(slot, index), *child, target) {
                        Status::Running => return Status::Running,
                        Status::Failure if !permissive => {
                            self.remove(slot);
                            return Status::Failure;
                        }
                        _ => index += 1,
                    }
                }
            }
            NodeKind::Selector(_) | NodeKind::UtilitySelector { .. } => {
                let choice = match self.state(slot) {
                    Some(State::Index(index)) => Some(index),
                    _ => self.planned_choice(slot).map(u32::from),
                };
                let Some(child) =
                    choice.and_then(|index| data.children(id).get(index as usize).copied())
                else {
                    self.remove(slot);
                    return Status::Failure;
                };
                let index = choice.unwrap_or_default();
                self.set_state(slot, id, State::Index(index));
                let status = self.exec(slot_of(slot, index as usize), child, target);
                if status != Status::Running {
                    self.remove(slot);
                }
                status
            }
            NodeKind::Parallel {
                completion_mode, ..
            } => self.exec_parallel(slot, id, completion_mode, target),
            NodeKind::SubTree { root, .. } => {
                let recorded = !data.node(id).interruptable;
                if recorded {
                    self.set_state(slot, id, State::Entered);
                }
                let status = match root {
                    Some(root) => self.exec(slot_of(slot, 0), *root, target),
                    None => Status::Failure,
                };
                if status != Status::Running {
                    self.remove(slot);
                }
                status
            }
            kind if kind.is_decorator() => self.exec_decorator(slot, id, target),
            _ => self.exec_leaf(slot, id, target),
        }
    }

    fn exec_parallel(
        &mut self,
        slot: Slot,
        id: NodeId,
        mode: &Param,
        target: Option<u32>,
    ) -> Status {
        let data = self.rt.data();
        let children = data.children(id);
        let mode = self.number(mode).map_or(COMPLETION_ALL, |mode| mode as i64);
        if children.len() as u32 > MAX_PARALLEL_CHILDREN {
            return Status::Failure;
        }
        let (mut finished, mut failed) = match self.state(slot) {
            Some(State::Parallel { finished, failed }) => (finished, failed),
            _ => (0, 0),
        };
        self.set_state(slot, id, State::Parallel { finished, failed });
        for (index, child) in children.iter().enumerate() {
            let bit = 1 << index;
            if finished & bit != 0 {
                continue;
            }
            match self.exec(slot_of(slot, index), *child, target) {
                Status::Running => {}
                Status::Success => finished |= bit,
                Status::Failure => {
                    finished |= bit;
                    failed |= bit;
                }
            }
        }
        let all = if children.len() == u64::BITS as usize {
            u64::MAX
        } else {
            (1u64 << children.len()) - 1
        };
        let done = match mode {
            COMPLETION_FIRST if finished != 0 => Some(if failed != 0 {
                Status::Failure
            } else {
                Status::Success
            }),
            COMPLETION_ANY if finished & !failed != 0 => Some(Status::Success),
            COMPLETION_ALL | COMPLETION_FIRST | COMPLETION_ANY if finished == all => Some(
                if failed == 0 || (mode == COMPLETION_ANY && failed != all) {
                    Status::Success
                } else {
                    Status::Failure
                },
            ),
            _ => None,
        };
        match done {
            Some(status) => {
                for (index, child) in children.iter().enumerate() {
                    if finished & (1 << index) == 0 {
                        self.cancel_tree(slot_of(slot, index), *child);
                    }
                }
                self.remove(slot);
                status
            }
            None => {
                self.set_state(slot, id, State::Parallel { finished, failed });
                Status::Running
            }
        }
    }

    fn exec_decorator(&mut self, slot: Slot, id: NodeId, target: Option<u32>) -> Status {
        let data = self.rt.data();
        let node = data.node(id);
        let kind = &node.kind;
        let recorded = !node.interruptable || has_execution_state(kind) || has_continuation(kind);
        let fresh = self.state(slot).is_none();
        if fresh {
            let state = self.enter(id, kind);
            if recorded {
                self.set_state(slot, id, state);
            }
        }
        if let Some(status) = self.stop_check(slot, kind, fresh) {
            if let Some(child) = kind.child() {
                self.cancel_tree(slot_of(slot, 0), child);
            }
            return self.leave(slot, id, status, true);
        }
        let Some(child) = kind.child() else {
            return self.leave(slot, id, Status::Failure, false);
        };
        let mut status = self.exec(slot_of(slot, 0), child, target);
        if status == Status::Success
            && let Some(State::Repeat { left, until }) = self.state(slot)
        {
            let more_time = until.get().is_some_and(|until| self.now < until);
            if left > 1 || more_time {
                self.set_state(
                    slot,
                    id,
                    State::Repeat {
                        left: left - 1,
                        until,
                    },
                );
                let mut fresh = Vec::new();
                if self.plan(slot_of(slot, 0), child, &mut fresh).is_none() {
                    return self.leave(slot, id, Status::Failure, false);
                }
                self.brain
                    .plan
                    .retain(|(known, _)| !fresh.iter().any(|(slot, _)| slot == known));
                self.brain.plan.extend(fresh);
                status = Status::Running;
            }
        }
        if status == Status::Running {
            return Status::Running;
        }
        self.leave(slot, id, status, false)
    }

    /// The state a decorator starts with, and its start effects.
    fn enter(&mut self, id: NodeId, kind: &NodeKind) -> State {
        match kind {
            NodeKind::ActionTargetTracker { .. } => {
                self.out.push(Command::TrackActionTargets(Some(id)));
                State::Entered
            }
            NodeKind::Timer { min, max, .. } => {
                let until = self.random_end(min, max).unwrap_or(self.now);
                State::Until(until)
            }
            NodeKind::Repeater {
                count_min,
                count_max,
                seconds_min,
                seconds_max,
                ..
            } => {
                let count = match (self.number(count_min), self.number(count_max)) {
                    (Some(min), Some(max)) => {
                        let count = self.brain.rng.whole(min as i64, max as i64);
                        i32::try_from(count).unwrap_or(i32::MAX)
                    }
                    _ => 1,
                };
                let seconds = match (self.number(seconds_min), self.number(seconds_max)) {
                    (Some(min), Some(max)) if max > 0.0 => Some(self.brain.rng.range(min, max)),
                    _ => None,
                };
                State::Repeat {
                    left: count,
                    until: Deadline::of(seconds.map(|seconds| self.now + ms_of(seconds))),
                }
            }
            NodeKind::VariableMonitor { key, .. } => {
                State::Monitor(self.resolve(&Param::of_key(*key)))
            }
            NodeKind::SetMannequinTag { tag, .. } => {
                self.set_tag(*tag, true);
                State::Entered
            }
            NodeKind::StatusEffect { effect, .. } => {
                self.out.push(Command::StatusEffect {
                    effect: *effect,
                    apply: true,
                    node: id,
                });
                State::Entered
            }
            NodeKind::LockTargetSelection { .. } => {
                self.out.push(Command::LockTarget(true));
                State::Entered
            }
            NodeKind::ManualTargeting {
                key, height_offset, ..
            } => {
                let point = self
                    .resolve(&Param::of_key(*key))
                    .and_then(Value::as_vec3)
                    .map(|mut point| {
                        point[2] += self.number(height_offset).unwrap_or(0.0) as f32;
                        point
                    });
                self.out.push(Command::ManualTarget(point));
                State::Entered
            }
            _ => State::Entered,
        }
    }

    /// The executing checks of a decorator: `Some(status)` stops its child with that status. On
    /// the tick it starts (`fresh`), its continuation conditions are not checked: they hold a
    /// running child, the entry conditions gate the start (Inferred from the risen's
    /// `AlertUntilClose`: entry `True Equal True`, continuation `Target_Dist > 4`; a check at
    /// the start would never let a mob whose target is already near play its alert, whose
    /// `Alerted` tag its fight needs).
    fn stop_check(&mut self, slot: Slot, kind: &NodeKind, fresh: bool) -> Option<Status> {
        let gates = match kind {
            NodeKind::BlackboardCondition { gates, .. } => Some(gates.as_ref()),
            NodeKind::EntityQuery { params, .. } => Some(&params.gates),
            _ => None,
        };
        if let Some(gates) = gates {
            let continues =
                fresh || gates.continuation.is_empty() || self.holds(&gates.continuation);
            let interrupted = !gates.interrupt.is_empty() && self.holds(&gates.interrupt);
            return (!continues || interrupted).then_some(Status::Failure);
        }
        match kind {
            NodeKind::DistanceCondition {
                gates,
                vertical_offset,
                vertical_threshold,
                allow_dynamic_scaling,
                ..
            } => {
                let continues = fresh
                    || gates.continuation.is_empty()
                    || self.distance_holds(
                        &gates.continuation,
                        vertical_offset,
                        vertical_threshold,
                        *allow_dynamic_scaling,
                    );
                let interrupted = !gates.interrupt.is_empty()
                    && self.distance_holds(
                        &gates.interrupt,
                        vertical_offset,
                        vertical_threshold,
                        *allow_dynamic_scaling,
                    );
                (!continues || interrupted).then_some(Status::Failure)
            }
            NodeKind::Timer {
                succeed_on_timeout, ..
            } => match self.state(slot) {
                Some(State::Until(until)) if self.now >= until => Some(if *succeed_on_timeout {
                    Status::Success
                } else {
                    Status::Failure
                }),
                _ => None,
            },
            NodeKind::VariableMonitor { key, .. } => {
                let now = self.resolve(&Param::of_key(*key));
                match self.state(slot) {
                    Some(State::Monitor(started)) if started != now => Some(Status::Failure),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Ends a decorator: its end effects, its status mapping, and its state.
    fn leave(&mut self, slot: Slot, id: NodeId, status: Status, cancelled: bool) -> Status {
        let data = self.rt.data();
        let kind = &data.node(id).kind;
        let success = status == Status::Success;
        let status = match kind {
            NodeKind::AlwaysSucceed {
                during_execution: true,
                ..
            } => Status::Success,
            _ => status,
        };
        match kind {
            NodeKind::Cooldown {
                cooldown,
                omit_on_failure,
                ..
            } if success || !omit_on_failure => {
                self.set_cooldown(cooldown);
            }
            NodeKind::FrequencyLimiter {
                min,
                max,
                stamp_on_success,
                stamp_on_failure,
                ..
            } if (success && *stamp_on_success) || (!success && *stamp_on_failure) => {
                if let Some(until) = self.random_end(min, max) {
                    match self
                        .brain
                        .limiters
                        .iter_mut()
                        .find(|(known, _)| *known == slot)
                    {
                        Some(entry) => entry.1 = until,
                        None => self.brain.limiters.push((slot, until)),
                    }
                }
            }
            NodeKind::CountLimiter {
                count_on_success,
                count_on_failure,
                ..
            } if (success && *count_on_success) || (!success && *count_on_failure) => {
                self.add_count(slot);
            }
            NodeKind::SetMannequinTag { tag, .. } => self.set_tag(*tag, false),
            NodeKind::StatusEffect {
                effect,
                remove_on_end: true,
                ..
            } => self.out.push(Command::StatusEffect {
                effect: *effect,
                apply: false,
                node: id,
            }),
            NodeKind::LockTargetSelection { .. } => self.out.push(Command::LockTarget(false)),
            NodeKind::ManualTargeting { .. } => self.out.push(Command::ManualTarget(None)),
            NodeKind::ActionTargetTracker { .. } => {
                self.out.push(Command::TrackActionTargets(None))
            }
            NodeKind::EncounterEvent {
                event,
                max_count,
                on_success,
                on_failure,
                on_cancelled,
                ..
            } => {
                let send = if cancelled {
                    *on_cancelled
                } else if success {
                    *on_success
                } else {
                    *on_failure
                };
                // MaxCount 0: no limit (Inferred: 5 of the 140 uses hold 0).
                let limit = self.number(max_count).unwrap_or(0.0);
                if send && (limit <= 0.0 || f64::from(self.count(slot)) < limit) {
                    self.add_count(slot);
                    self.out.push(Command::EncounterEvent(*event));
                }
            }
            _ => {}
        }
        self.remove(slot);
        status
    }

    fn exec_leaf(&mut self, slot: Slot, id: NodeId, target: Option<u32>) -> Status {
        if self.state(slot).is_none() {
            match self.start_leaf(slot, id, target) {
                LeafStart::Wait(state) => self.set_state(slot, id, state),
                LeafStart::Done(status) => return status,
            }
        }
        self.poll_leaf(slot, id)
    }

    /// Starts a leaf: its commands, and the state it waits in, or its result when it is done at
    /// once.
    fn start_leaf(&mut self, slot: Slot, id: NodeId, target: Option<u32>) -> LeafStart {
        let data = self.rt.data();
        match &data.node(id).kind {
            NodeKind::Cage(cage) => {
                if cage.nav_ray_cast.check_at_execution && !self.clearance(&cage.nav_ray_cast) {
                    return LeafStart::Done(Status::Failure);
                }
                let tags = self.brain.tags.clone();
                let has_tag = |tag: NameId| tags.contains(&tag);
                let Some(task) = self.task(&cage.task) else {
                    return LeafStart::Done(Status::Failure);
                };
                let Some((action, cage_action)) = self.cage_action(task) else {
                    return LeafStart::Done(Status::Failure);
                };
                let done_at = cage_action
                    .done_after_s(&has_tag)
                    .map(|seconds| self.now + ms_of(f64::from(seconds)));
                for activity in &cage_action.tags {
                    self.tag_effect(activity.tag, activity.on_enter, activity.delay_s);
                }
                self.out.push(Command::PlayTask {
                    slot,
                    node: id,
                    task,
                    action,
                    done_at_ms: done_at,
                });
                self.task_running(task, cage.wait_for_completion);
                if !cage.wait_for_completion {
                    return LeafStart::Done(Status::Success);
                }
                LeafStart::Wait(State::Leaf {
                    done_at: Deadline::of(done_at),
                    fail_at: Deadline::of(self.fail_at(&cage.fail_timeout)),
                })
            }
            NodeKind::Navigation(nav) => {
                if nav.cage.nav_ray_cast.check_at_execution
                    && !self.clearance(&nav.cage.nav_ray_cast)
                {
                    return LeafStart::Done(Status::Failure);
                }
                let destination = if let Some(key) = nav.nav_key {
                    match self.resolve(&Param::of_key(key)).and_then(Value::as_vec3) {
                        Some(point) => Destination::Point(point),
                        None => return LeafStart::Done(Status::Failure),
                    }
                } else if let Some(params) = &nav.motion.select_position {
                    let board = board(self.rt, self.arch, self.world, &self.brain.facts);
                    match self.world.select_position(params, &board) {
                        Some(point) => Destination::Point(point),
                        None => return LeafStart::Done(Status::Failure),
                    }
                } else {
                    Destination::Target
                };
                let Some(task) = self.task(&nav.cage.task) else {
                    return LeafStart::Done(Status::Failure);
                };
                self.out.push(Command::Navigate {
                    slot,
                    node: id,
                    task,
                    destination,
                    config: self.nav_config(nav.motion.nav_config),
                });
                self.task_running(task, true);
                LeafStart::Wait(State::Leaf {
                    done_at: Deadline::of(None),
                    fail_at: Deadline::of(self.fail_at(&nav.cage.fail_timeout)),
                })
            }
            NodeKind::Strafe(strafe) => {
                let Some(task) = self.task(&strafe.cage.task) else {
                    return LeafStart::Done(Status::Failure);
                };
                self.out.push(Command::Strafe {
                    slot,
                    node: id,
                    task,
                    config: self.nav_config(strafe.motion.nav_config),
                });
                self.task_running(task, true);
                LeafStart::Wait(State::Leaf {
                    done_at: Deadline::of(None),
                    fail_at: Deadline::of(self.fail_at(&strafe.cage.fail_timeout)),
                })
            }
            NodeKind::SetCooldown(spec) => {
                self.set_cooldown(spec);
                LeafStart::Done(Status::Success)
            }
            NodeKind::SetFact { key, op, value } => {
                let Some(value) = self.resolve(value) else {
                    return LeafStart::Done(Status::Failure);
                };
                let value = match op {
                    FactOp::Assign => Some(value),
                    FactOp::Add | FactOp::Subtract => {
                        // A key not set yet counts as 0 (Inferred).
                        let current = self
                            .resolve(&Param::of_key(*key))
                            .and_then(Value::as_number)
                            .unwrap_or(0.0);
                        let step = value.as_number();
                        step.map(|step| {
                            let sum = if *op == FactOp::Add {
                                current + step
                            } else {
                                current - step
                            };
                            match value {
                                Value::Int(_) | Value::Bool(_) => Value::Int(sum as i64),
                                _ => Value::Float(sum as f32),
                            }
                        })
                    }
                };
                match value {
                    Some(value) => {
                        self.set_fact(*key, value);
                        LeafStart::Done(Status::Success)
                    }
                    None => LeafStart::Done(Status::Failure),
                }
            }
            NodeKind::SelectPosition {
                params, storage, ..
            } => {
                let board = board(self.rt, self.arch, self.world, &self.brain.facts);
                let point = self.world.select_position(params, &board);
                match (point, storage.or(self.rt.selected_pos)) {
                    (Some(point), Some(key)) => {
                        self.set_fact(key, Value::Vec3(point));
                        LeafStart::Done(Status::Success)
                    }
                    _ => LeafStart::Done(Status::Failure),
                }
            }
            NodeKind::CountEntitiesWithTag { tag, key } => {
                let count = self.world.entities_with_tag(data.name(*tag));
                self.set_fact(*key, Value::Int(i64::from(count)));
                LeafStart::Done(Status::Success)
            }
            NodeKind::Delay { min, max } => match self.random_end(min, max) {
                Some(until) => LeafStart::Wait(State::Until(until)),
                None => LeafStart::Done(Status::Failure),
            },
            NodeKind::Fake { indefinitely: true } => LeafStart::Wait(State::Leaf {
                done_at: Deadline::of(None),
                fail_at: Deadline::of(None),
            }),
            NodeKind::Fake {
                indefinitely: false,
            } => LeafStart::Done(Status::Success),
            NodeKind::SetRemoteFact { .. }
            | NodeKind::TriggerEntity { .. }
            | NodeKind::TriggerRemoteEntity { .. }
            | NodeKind::SetBeam { .. }
            | NodeKind::ExecuteSpawn { .. }
            | NodeKind::CompleteOrder
            | NodeKind::AssignOrder { .. }
            | NodeKind::SelectWaypoint
            | NodeKind::SpawnProjectile(_)
            | NodeKind::UpdateNwTags { .. } => {
                self.out.push(Command::Action {
                    node: id,
                    target_index: target,
                });
                LeafStart::Done(Status::Success)
            }
            _ => LeafStart::Done(Status::Failure),
        }
    }

    fn fail_at(&self, timeout: &Param) -> Option<u64> {
        self.number(timeout)
            .filter(|seconds| *seconds > 0.0)
            .map(|seconds| self.now + ms_of(seconds))
    }

    /// Marks `task` as running (`CAGE_<task>` exists while it runs; Inferred from the trees:
    /// Turn.behaviortree does not turn while `CAGE_Move_Combat_CAGE_Task` exists).
    fn task_running(&mut self, task: NameId, running: bool) {
        let Some(key) = self.rt.task_key(task) else {
            return;
        };
        if running {
            self.set_fact(key, Value::Bool(true));
        } else {
            self.brain.facts.retain(|(known, _)| *known != key);
        }
    }

    fn set_fact(&mut self, key: KeyId, value: Value) {
        self.brain.set_fact(key, value);
    }

    fn poll_leaf(&mut self, slot: Slot, id: NodeId) -> Status {
        let data = self.rt.data();
        let status = match self.state(slot) {
            Some(State::Leaf { done_at, fail_at }) => {
                let reportable = matches!(
                    data.node(id).kind,
                    NodeKind::Navigation(_) | NodeKind::Strafe(_) | NodeKind::Cage(_)
                );
                let reported = self
                    .brain
                    .motion
                    .filter(|(known, _)| reportable && *known == slot)
                    .map(|(_, outcome)| outcome);
                if let Some(outcome) = reported {
                    self.brain.motion = None;
                    match outcome {
                        MotionOutcome::Arrived => Status::Success,
                        MotionOutcome::Failed => Status::Failure,
                    }
                } else if done_at.reached(self.now) {
                    Status::Success
                } else if fail_at.reached(self.now) {
                    Status::Failure
                } else {
                    Status::Running
                }
            }
            Some(State::Until(until)) if self.now >= until => Status::Success,
            Some(State::Until(_)) => Status::Running,
            _ => Status::Failure,
        };
        if status != Status::Running {
            self.end_leaf(slot, id, false);
        }
        status
    }

    /// Ends a leaf: the CAGE tag exits, and the stop of a cancelled task or move.
    fn end_leaf(&mut self, slot: Slot, id: NodeId, cancelled: bool) {
        let data = self.rt.data();
        if let Some(task) = data.node(id).kind.task().and_then(|task| self.task(&task)) {
            self.task_running(task, false);
        }
        match &data.node(id).kind {
            NodeKind::Cage(cage) => {
                if let Some((_, action)) = self
                    .task(&cage.task)
                    .and_then(|task| self.cage_action(task))
                {
                    let exits: Vec<_> = action
                        .tags
                        .iter()
                        .map(|activity| (activity.tag, activity.on_exit))
                        .collect();
                    for (tag, effect) in exits {
                        self.tag_effect(tag, effect, 0.0);
                    }
                }
                if cancelled {
                    self.out.push(Command::EndTask { slot });
                }
            }
            NodeKind::Navigation(_) | NodeKind::Strafe(_) if cancelled => {
                self.out.push(Command::StopMotion { slot });
            }
            _ => {}
        }
        self.remove(slot);
    }

    /// Cancels node `id` at `slot` and every executing node below it, deepest first: the
    /// executing path is followed by the composites' states.
    fn cancel_tree(&mut self, slot: Slot, id: NodeId) {
        let data = self.rt.data();
        let kind = &data.node(id).kind;
        if kind.children().is_some() {
            let children = data.children(id);
            match self.state(slot) {
                Some(State::Index(index)) => {
                    if let Some(child) = children.get(index as usize) {
                        self.cancel_tree(slot_of(slot, index as usize), *child);
                    }
                }
                Some(State::Parallel { finished, .. }) => {
                    for (index, child) in children.iter().enumerate() {
                        if finished & (1 << index) == 0 {
                            self.cancel_tree(slot_of(slot, index), *child);
                        }
                    }
                }
                _ => {}
            }
            self.remove(slot);
        } else if let NodeKind::SubTree { root, .. } = kind {
            if let Some(root) = root {
                self.cancel_tree(slot_of(slot, 0), *root);
            }
            self.remove(slot);
        } else if kind.is_decorator() {
            if let Some(child) = kind.child() {
                self.cancel_tree(slot_of(slot, 0), child);
            }
            if self.state(slot).is_some() {
                self.leave(slot, id, Status::Failure, true);
            }
        } else if self.state(slot).is_some() {
            self.end_leaf(slot, id, true);
        }
    }

    /// Cancels the whole executing plan.
    fn cancel_all(&mut self) {
        if let Some(root) = self.brain.root {
            self.cancel_tree(ROOT_SLOT, root);
        }
        self.brain.active.clear();
        self.brain.plan.clear();
        self.brain.motion = None;
    }
}

/// Whether a decorator keeps a state while its child runs: a time, a count, a value, or an
/// effect at its end. The others are recorded only when not `Interruptable` (the per-mob state
/// budget, study section 5.2).
fn has_execution_state(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Cooldown { .. }
            | NodeKind::FrequencyLimiter { .. }
            | NodeKind::CountLimiter { .. }
            | NodeKind::Timer { .. }
            | NodeKind::SetMannequinTag { .. }
            | NodeKind::StatusEffect { .. }
            | NodeKind::LockTargetSelection { .. }
            | NodeKind::EncounterEvent { .. }
            | NodeKind::VariableMonitor { .. }
            | NodeKind::Repeater { .. }
            | NodeKind::ManualTargeting { .. }
            | NodeKind::ActionTargetTracker { .. }
    )
}

/// Whether a condition decorator has continuation conditions: it keeps a state while its child
/// runs, so that they are checked from its second tick on.
fn has_continuation(kind: &NodeKind) -> bool {
    match kind {
        NodeKind::BlackboardCondition { gates, .. } | NodeKind::DistanceCondition { gates, .. } => {
            !gates.continuation.is_empty()
        }
        NodeKind::EntityQuery { params, .. } => !params.gates.continuation.is_empty(),
        _ => false,
    }
}

/// How a leaf starts.
enum LeafStart {
    /// It waits in this state.
    Wait(State),
    /// It is done.
    Done(Status),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{Build, COMPLETION_OFFSET_S as GORE_OFFSET_S, EPSILON_UTILITY, GORE_S};
    use crate::data::{FragmentLength, TagActivity};
    use crate::node::{Children, CooldownOverride, CooldownSpec, Gates};
    use crate::value::CompareOp;

    const SEED: u64 = 7;
    const TICK_MS: u64 = 33;
    const NEAR_M: f32 = 3.0;

    #[derive(Default)]
    struct TestWorld {
        now: u64,
        engine: Vec<(EngineKey, Value)>,
        effects: Vec<String>,
    }

    impl TestWorld {
        fn set(&mut self, key: EngineKey, value: Value) {
            self.engine.retain(|(known, _)| *known != key);
            self.engine.push((key, value));
        }
    }

    impl World for TestWorld {
        fn now_ms(&self) -> u64 {
            self.now
        }

        fn engine_value(&self, key: EngineKey) -> Option<Value> {
            self.engine
                .iter()
                .find(|(known, _)| *known == key)
                .map(|(_, value)| *value)
        }

        fn has_status_effect(&self, effect: &str) -> bool {
            self.effects.iter().any(|known| known == effect)
        }

        fn has_status_category(&self, _category: &str) -> bool {
            false
        }

        fn position(&self, of: PositionType) -> Option<[f32; 3]> {
            match of {
                PositionType::Own => Some([0.0; 3]),
                _ => None,
            }
        }

        fn action_targets(&self, _params: &TrackerParams) -> u32 {
            1
        }

        fn entity_query(&self, _params: &EntityQueryParams, _board: &dyn Blackboard) -> bool {
            true
        }

        fn entities_with_tag(&self, _tag: &str) -> u32 {
            2
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
            Some([1.0, 2.0, 3.0])
        }
    }

    fn played(out: &[Command], data: &AiData) -> Vec<String> {
        out.iter()
            .filter_map(|command| match command {
                Command::PlayTask { task, .. } => Some(data.name(*task).to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Ticks every 33 ms until `until_ms`; returns the tasks started, in order.
    fn run(data: &AiData, brain: &mut Brain, world: &mut TestWorld, until_ms: u64) -> Vec<String> {
        let runtime = Runtime::new(data.clone());
        let mut tasks = Vec::new();
        while world.now <= until_ms {
            let mut out = Vec::new();
            brain.tick(&runtime, world, &mut out);
            tasks.extend(played(&out, data));
            world.now += TICK_MS;
        }
        tasks
    }

    fn brain(data: &AiData) -> Brain {
        Brain::new(&Runtime::new(data.clone()), 0, SEED).expect("brain")
    }

    #[test]
    fn a_selector_runs_the_first_child_that_plans_and_the_gore_lasts_its_fragment() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let near = build.gate("Target_Dist", CompareOp::LessThanOrEqual, NEAR_M, gore);
        let idle = build.idle("Idle");
        let root = build.selector(&[near, idle]);
        let data = build.finish(root);
        let mut world = TestWorld::default();
        world.set(EngineKey::TargetDist, Value::Float(2.0));
        let mut mob = brain(&data);
        let runtime = Runtime::new(data.clone());
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        let Command::PlayTask { done_at_ms, .. } = out[0] else {
            panic!("{out:?}");
        };
        assert_eq!(played(&out, &data), ["Gore"]);
        assert_eq!(done_at_ms, Some(ms_of(f64::from(GORE_S - GORE_OFFSET_S))));
        world.set(EngineKey::TargetDist, Value::Float(9.0));
        let later = run(&data, &mut mob, &mut world, 2_000);
        assert_eq!(
            later,
            ["Idle"],
            "an interruptable gore gives way to the new plan at once"
        );
    }

    #[test]
    fn a_reported_end_finishes_a_looping_task() {
        let mut build = Build::default();
        let idle = build.idle("Wait_Loop");
        let data = build.finish(idle);
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        let runtime = Runtime::new(data.clone());
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        let Command::PlayTask {
            slot, done_at_ms, ..
        } = out[0]
        else {
            panic!("{out:?}");
        };
        assert_eq!(done_at_ms, None, "a loop has no end of its own");
        world.now += TICK_MS;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(played(&out, &data).is_empty(), "it loops on");
        mob.report_motion(slot, MotionOutcome::Arrived);
        world.now += TICK_MS;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert_eq!(
            played(&out, &data),
            ["Wait_Loop"],
            "done: the tree plans it again"
        );
    }

    #[test]
    fn a_non_interruptable_attack_finishes_before_the_plan_changes() {
        let mut build = Build::default();
        let params = build.cage_params("Gore");
        build.tasks.clear();
        let gore_leaf = build.gore("Gore");
        let gore = build.node_with(NodeKind::Cage(Box::new(params)), EPSILON_UTILITY, false);
        assert_ne!(gore, gore_leaf);
        let near = build.gate("Target_Dist", CompareOp::LessThanOrEqual, NEAR_M, gore);
        let idle = build.idle("Idle");
        let root = build.selector(&[near, idle]);
        let data = build.finish(root);
        let mut world = TestWorld::default();
        world.set(EngineKey::TargetDist, Value::Float(2.0));
        let mut mob = brain(&data);
        assert_eq!(run(&data, &mut mob, &mut world, 0), ["Gore"]);
        world.set(EngineKey::TargetDist, Value::Float(9.0));
        let done_ms = ms_of(f64::from(GORE_S - GORE_OFFSET_S));
        assert!(run(&data, &mut mob, &mut world, done_ms - TICK_MS).is_empty());
        assert_eq!(
            run(&data, &mut mob, &mut world, done_ms + TICK_MS),
            ["Idle"]
        );
    }

    #[test]
    fn a_cooldown_blocks_until_its_random_time_ends() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let spec = build.cooldown_spec("AttackCooldown", 2.0, 3.0);
        let cooled = build.node(NodeKind::Cooldown {
            child: Some(gore),
            cooldown: spec,
            omit_on_failure: false,
        });
        let idle = build.idle("Idle");
        let root = build.selector(&[cooled, idle]);
        let data = build.finish(root);
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        let tasks = run(&data, &mut mob, &mut world, 6_000);
        let attack = data.name_id("AttackCooldown").expect("name");
        assert_eq!(
            tasks.iter().filter(|task| *task == "Gore").count(),
            2,
            "{tasks:?}"
        );
        assert_eq!(tasks[..2], ["Gore", "Idle"]);
        let end = mob
            .cooldowns
            .iter()
            .find(|(name, _)| *name == attack)
            .map(|(_, until)| *until)
            .expect("set");
        assert!(end > 1_400, "set after the second gore");
    }

    #[test]
    fn cooldown_overrides_keep_the_right_end() {
        let mut build = Build::default();
        let leaf = build.node(NodeKind::Fake {
            indefinitely: false,
        });
        let data = build.finish(leaf);
        let runtime = Runtime::new(data.clone());
        let world = TestWorld::default();
        let mut mob = brain(&data);
        let mut out = Vec::new();
        let arch = &data.archetypes[0];
        let mut ctx = Ctx {
            rt: &runtime,
            arch,
            world: &world,
            now: 0,
            brain: &mut mob,
            out: &mut out,
        };
        let mut spec = CooldownSpec {
            name: 0,
            min: Param::literal(Value::Float(5.0)),
            max: Param::literal(Value::Float(5.0)),
            override_rule: CooldownOverride::Always,
        };
        ctx.set_cooldown(&spec);
        spec.min = Param::literal(Value::Float(1.0));
        spec.max = Param::literal(Value::Float(1.0));
        for (rule, expected) in [
            (CooldownOverride::Never, 5_000),
            (CooldownOverride::KeepLongest, 5_000),
            (CooldownOverride::KeepShortest, 1_000),
            (CooldownOverride::Always, 1_000),
        ] {
            spec.override_rule = rule;
            ctx.set_cooldown(&spec);
            assert_eq!(ctx.brain.cooldown_until(0, 0), Some(expected), "{rule:?}");
            spec.min = Param::literal(Value::Float(5.0));
            spec.max = Param::literal(Value::Float(5.0));
            spec.override_rule = CooldownOverride::Always;
            ctx.set_cooldown(&spec);
            spec.min = Param::literal(Value::Float(1.0));
            spec.max = Param::literal(Value::Float(1.0));
        }
    }

    #[test]
    fn a_frequency_limiter_and_a_count_limiter_gate_their_child() {
        let mut build = Build::default();
        let fidget = build.gore("Fidget");
        let limited = build.node(NodeKind::FrequencyLimiter {
            child: Some(fidget),
            min: Param::literal(Value::Float(5.0)),
            max: Param::literal(Value::Float(5.0)),
            stamp_on_success: true,
            stamp_on_failure: false,
        });
        let alert = build.gore("Alert");
        let once = build.node(NodeKind::CountLimiter {
            child: Some(alert),
            max: Param::literal(Value::Int(1)),
            count_on_success: true,
            count_on_failure: false,
        });
        let idle = build.idle("Idle");
        let root = build.selector(&[once, limited, idle]);
        let data = build.finish(root);
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        // Alert 0 to 1.4 s, Fidget to 2.8 s (open again at 7.8 s), Idle, Fidget to 9.2 s.
        let tasks = run(&data, &mut mob, &mut world, 10_000);
        assert_eq!(tasks, ["Alert", "Fidget", "Idle", "Fidget", "Idle"]);
    }

    #[test]
    fn a_weighted_utility_selector_draws_by_weight_and_keeps_its_choice() {
        let mut build = Build::default();
        let a = build.cage("A", FragmentLength::Loops, Vec::new());
        let b = build.cage("B", FragmentLength::Loops, Vec::new());
        let low = build.node_with(
            NodeKind::Sequencer(Children { first: 0, count: 0 }),
            1.0,
            true,
        );
        let wrapped_a = build.sequence(&[a]);
        let wrapped_b = build.sequence(&[b]);
        data_set_utility(&mut build, wrapped_a, 3.0);
        data_set_utility(&mut build, wrapped_b, 1.0);
        let children = build.children(&[wrapped_a, wrapped_b]);
        let root = build.node(NodeKind::UtilitySelector {
            children,
            method: SelectionMethod::WeightedRandom,
            top_n: u64::MAX,
            preserve_if_no_change: true,
            always_preserve: true,
        });
        assert_ne!(low, root);
        let data = build.finish(root);
        let mut picks = [0usize; 2];
        for seed in 0..2_000 {
            let mut mob = Brain::new(&Runtime::new(data.clone()), 0, seed).expect("brain");
            let mut world = TestWorld::default();
            let tasks = run(&data, &mut mob, &mut world, 1_000);
            assert_eq!(
                tasks.len(),
                1,
                "the choice is kept while it runs: {tasks:?}"
            );
            picks[usize::from(tasks[0] == "B")] += 1;
        }
        let share_a = picks[0] as f64 / 2_000.0;
        assert!((share_a - 0.75).abs() < 0.04, "A drawn {share_a}");
    }

    fn data_set_utility(build: &mut Build, node: NodeId, utility: f32) {
        build.data.nodes[node as usize].utility = utility;
    }

    #[test]
    fn a_top_utility_selector_takes_the_highest_utility() {
        let mut build = Build::default();
        let a = build.idle("A");
        let b = build.idle("B");
        data_set_utility(&mut build, b, 2.0);
        let children = build.children(&[a, b]);
        let root = build.node(NodeKind::UtilitySelector {
            children,
            method: SelectionMethod::Top,
            top_n: u64::MAX,
            preserve_if_no_change: true,
            always_preserve: false,
        });
        let data = build.finish(root);
        let mut world = TestWorld::default();
        assert_eq!(run(&data, &mut brain(&data), &mut world, 0), ["B"]);
    }

    #[test]
    fn a_failed_continuation_stops_the_running_task() {
        let mut build = Build::default();
        let idle = build.idle("Chase");
        let continuation = build.condition("Target_Visible", CompareOp::Equal, 1.0);
        let watched = build.node(NodeKind::BlackboardCondition {
            child: Some(idle),
            gates: Box::new(Gates {
                continuation: ConditionGroup {
                    any: false,
                    conditions: vec![continuation],
                },
                ..Gates::default()
            }),
        });
        let data = build.finish(watched);
        let runtime = Runtime::new(data.clone());
        let mut world = TestWorld::default();
        world.set(EngineKey::TargetVisible, Value::Bool(true));
        let mut mob = brain(&data);
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        assert_eq!(played(&out, &data), ["Chase"]);
        world.set(EngineKey::TargetVisible, Value::Bool(false));
        world.now = TICK_MS;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(
            out.iter()
                .any(|command| matches!(command, Command::EndTask { .. })),
            "{out:?}"
        );
    }

    #[test]
    fn a_continuation_holds_a_running_child_and_does_not_gate_its_start() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let condition = build.condition("Target_Dist", CompareOp::GreaterThan, NEAR_M);
        let gated = build.node(NodeKind::BlackboardCondition {
            child: Some(gore),
            gates: Box::new(Gates {
                continuation: ConditionGroup {
                    any: false,
                    conditions: vec![condition],
                },
                ..Gates::default()
            }),
        });
        let data = build.finish(gated);
        let mut world = TestWorld::default();
        world.set(EngineKey::TargetDist, Value::Float(NEAR_M));
        let mut mob = brain(&data);
        let runtime = Runtime::new(data.clone());
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        assert_eq!(played(&out, &data), ["Gore"], "it starts: entry holds");
        world.now += TICK_MS;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(
            out.iter()
                .any(|command| matches!(command, Command::EndTask { .. })),
            "its continuation stops it on the next tick: {out:?}"
        );
    }

    #[test]
    fn a_timer_ends_its_child_with_its_result() {
        for (succeed, expected) in [(true, "After"), (false, "Fallback")] {
            let mut build = Build::default();
            let idle = build.idle("Wait");
            let timer = build.node(NodeKind::Timer {
                child: Some(idle),
                min: Param::literal(Value::Float(1.0)),
                max: Param::literal(Value::Float(1.0)),
                succeed_on_timeout: succeed,
            });
            let after = build.idle("After");
            let sequence = build.sequence(&[timer, after]);
            let fallback = build.idle("Fallback");
            let once = build.node(NodeKind::CountLimiter {
                child: Some(sequence),
                max: Param::literal(Value::Int(1)),
                count_on_success: true,
                count_on_failure: true,
            });
            let root = build.selector(&[once, fallback]);
            let data = build.finish(root);
            let mut world = TestWorld::default();
            let tasks = run(&data, &mut brain(&data), &mut world, 1_500);
            assert_eq!(tasks, ["Wait", expected], "succeed {succeed}");
        }
    }

    #[test]
    fn facts_tags_and_cage_tags_change_the_blackboard() {
        let mut build = Build::default();
        let counter = build.key("Hits");
        let add = build.node(NodeKind::SetFact {
            key: counter,
            op: FactOp::Add,
            value: Param::literal(Value::Int(2)),
        });
        let alerted = build.name("Alerted");
        let alert = build.cage(
            "Alert",
            FragmentLength::Seconds(GORE_S),
            vec![TagActivity {
                tag: alerted,
                on_enter: TagEffect::Set,
                on_exit: TagEffect::NoEffect,
                delay_s: 0.0,
            }],
        );
        let chain = build.name("ChainAttack");
        let gore = build.gore("Gore");
        let chained = build.node(NodeKind::SetMannequinTag {
            child: Some(gore),
            tag: chain,
        });
        let root = build.sequence(&[add, alert, chained]);
        let data = build.finish(root);
        let runtime = Runtime::new(data.clone());
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        assert_eq!(mob.fact(counter), Some(Value::Int(2)));
        assert!(mob.has_tag(alerted));
        world.now = 1_500;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(mob.has_tag(chain), "set while the gore runs");
        world.now = 3_000;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(!mob.has_tag(chain), "cleared after it");
        assert_eq!(mob.fact(counter), Some(Value::Int(4)), "the plan ran again");
        assert_eq!(mob.archetype(), 0);
        assert!(mob.tags().contains(&alerted));
        assert_eq!(
            mob.resolve(&runtime, &world, &Param::of_key(counter)),
            Some(Value::Int(4))
        );
    }

    #[test]
    fn a_navigation_waits_for_the_sim_and_uses_the_slice_config() {
        let mut build = Build::default();
        let nav = build.navigation("Move_Combat", Some("RunToTarget"));
        let run_config = build.name("RunToTarget");
        let gore = build.gore("Gore");
        let root = build.sequence(&[nav, gore]);
        let mut data = build.finish(root);
        data.archetypes[0]
            .nav_configs
            .push(crate::build::run_config(run_config));
        let runtime = Runtime::new(data.clone());
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        let mut out = Vec::new();
        mob.tick(&runtime, &world, &mut out);
        let Some(Command::Navigate {
            slot,
            config,
            destination,
            ..
        }) = out.first().cloned()
        else {
            panic!("{out:?}");
        };
        assert_eq!(config, Some(0));
        assert_eq!(destination, Destination::Target);
        world.now = 500;
        out.clear();
        mob.tick(&runtime, &world, &mut out);
        assert!(out.is_empty(), "still moving");
        mob.report_motion(slot, MotionOutcome::Arrived);
        world.now = 533;
        mob.tick(&runtime, &world, &mut out);
        assert_eq!(played(&out, &data), ["Gore"]);
    }

    #[test]
    fn permissive_sequences_parallels_and_always_succeed_finish_as_the_client_logs() {
        let mut build = Build::default();
        let missing = build.cage_params("NotInGrid");
        let failing = build.node(NodeKind::Cage(Box::new(missing)));
        let after = build.gore("After");
        let children = build.children(&[failing, after]);
        let permissive = build.node(NodeKind::PermissiveSequencer(children));
        let heal = build.gore("Heal");
        let spec = build.cooldown_spec("Heal", 30.0, 30.0);
        let set = build.node(NodeKind::SetCooldown(spec));
        let parallel_children = build.children(&[heal, set]);
        let parallel = build.node(NodeKind::Parallel {
            children: parallel_children,
            completion_mode: Param::literal(Value::Int(COMPLETION_ALL)),
        });
        let also_missing = build.cage_params("AlsoNotInGrid");
        let failing_again = build.node(NodeKind::Cage(Box::new(also_missing)));
        let forced = build.node(NodeKind::AlwaysSucceed {
            child: Some(failing_again),
            during_evaluation: false,
            during_execution: true,
        });
        let last = build.gore("Last");
        let root = build.sequence(&[permissive, parallel, forced, last]);
        let data = build.finish(root);
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        let tasks = run(&data, &mut mob, &mut world, 3_000);
        assert_eq!(tasks[..3], ["After", "Heal", "Last"], "{tasks:?}");
        assert!(
            mob.cooldown_until(data.name_id("Heal").expect("name"), 3_000)
                .is_some()
        );
    }

    #[test]
    fn a_random_chance_of_zero_never_plans_and_of_one_always() {
        for (rate, expected) in [(0.0, "Idle"), (1.0, "Gore")] {
            let mut build = Build::default();
            let gore = build.gore("Gore");
            let chance = build.node(NodeKind::RandomChance {
                child: Some(gore),
                rate: Param::literal(Value::Float(rate)),
            });
            let idle = build.idle("Idle");
            let root = build.selector(&[chance, idle]);
            let data = build.finish(root);
            let mut world = TestWorld::default();
            assert_eq!(run(&data, &mut brain(&data), &mut world, 0), [expected]);
        }
    }

    #[test]
    fn a_repeater_runs_its_child_the_drawn_number_of_times() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let repeat = build.node(NodeKind::Repeater {
            child: Some(gore),
            count_min: Param::literal(Value::Int(3)),
            count_max: Param::literal(Value::Int(3)),
            seconds_min: Param::literal(Value::Float(0.0)),
            seconds_max: Param::literal(Value::Float(0.0)),
        });
        let after = build.idle("After");
        let sequence = build.sequence(&[repeat, after]);
        let data = build.finish(sequence);
        let mut world = TestWorld::default();
        let tasks = run(&data, &mut brain(&data), &mut world, 5_000);
        assert_eq!(tasks, ["Gore", "Gore", "Gore", "After"]);
    }

    #[test]
    fn a_delay_waits_and_an_interrupt_cancels_the_plan() {
        let mut build = Build::default();
        let delay = build.node(NodeKind::Delay {
            min: Param::literal(Value::Float(1.0)),
            max: Param::literal(Value::Float(1.0)),
        });
        let gore = build.gore("Gore");
        let root = build.sequence(&[delay, gore]);
        let data = build.finish(root);
        let runtime = Runtime::new(data.clone());
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        assert!(run(&data, &mut mob, &mut world, 900).is_empty());
        assert_eq!(run(&data, &mut mob, &mut world, 1_100), ["Gore"]);
        let mut out = Vec::new();
        mob.interrupt(&runtime, &world, &mut out);
        assert!(
            out.iter()
                .any(|command| matches!(command, Command::EndTask { .. }))
        );
        assert_eq!(mob.running_leaves(&data).count(), 0);
    }

    #[test]
    fn a_variable_monitor_restarts_its_child_when_the_value_changes() {
        let mut build = Build::default();
        let objective = build.key("Attack_Objective_Pos");
        let walk = build.idle("Walk");
        let monitor = build.node(NodeKind::VariableMonitor {
            child: Some(walk),
            key: objective,
        });
        let data = build.finish(monitor);
        let mut world = TestWorld::default();
        world.set(EngineKey::AttackObjectivePos, Value::Vec3([1.0, 0.0, 0.0]));
        let mut mob = brain(&data);
        assert_eq!(run(&data, &mut mob, &mut world, 100), ["Walk"]);
        world.set(EngineKey::AttackObjectivePos, Value::Vec3([5.0, 0.0, 0.0]));
        assert_eq!(
            run(&data, &mut mob, &mut world, 200),
            ["Walk"],
            "started again"
        );
    }

    #[test]
    fn status_effect_keys_read_the_world() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let rooted = build.gate("SE_Root_Effect", CompareOp::Equal, 1.0, gore);
        let idle = build.idle("Idle");
        let root = build.selector(&[rooted, idle]);
        build.name("Root_Effect");
        let data = build.finish(root);
        let mut world = TestWorld::default();
        assert_eq!(run(&data, &mut brain(&data), &mut world, 0), ["Idle"]);
        world.effects.push("Root_Effect".to_owned());
        world.now = 0;
        assert_eq!(run(&data, &mut brain(&data), &mut world, 0), ["Gore"]);
    }

    #[test]
    fn the_brain_state_stays_small() {
        let mut build = Build::default();
        let gore = build.gore("Gore");
        let data = build.finish(gore);
        let mut world = TestWorld::default();
        let mut mob = brain(&data);
        run(&data, &mut mob, &mut world, 1_000);
        assert!(mob.state_bytes() < 1_024, "{} bytes", mob.state_bytes());
    }
}

#[cfg(all(test, feature = "local-data"))]
mod scale_tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;

    const CLIENT_BUILD: &str = "25810808";
    /// Mobs ticked at once.
    const MOBS: usize = 100_000;
    /// Ticks before the measured one, so that every mob has a plan running.
    const WARM_UP_TICKS: u64 = 30;
    const TICK_MS: u64 = 33;
    /// Distances the mobs see their target at: 0.5 to 19.5 m (every band of the trees).
    const DISTANCE_STEPS: usize = 20;
    const DISTANCE_STEP_M: f32 = 1.0;
    const DISTANCE_START_M: f32 = 0.5;
    const ANGLE_STEPS: usize = 7;
    const ANGLE_STEP_DEG: f32 = 30.0;
    /// The budget of one tick of all mobs on one thread of this unoptimised test build (the
    /// sim ticks combat mobs at 30 Hz only near players, spread over its threads; study 5.3).
    const TICK_BUDGET: Duration = Duration::from_secs(2);
    /// The state budget per mob (study section 5.2: 0.5 to 1 KB).
    const STATE_BUDGET_BYTES: usize = 1_024;

    struct MobWorld {
        now: u64,
        distance: f32,
        angle: f32,
        has_target: bool,
    }

    impl World for MobWorld {
        fn now_ms(&self) -> u64 {
            self.now
        }

        fn engine_value(&self, key: EngineKey) -> Option<Value> {
            Some(match key {
                EngineKey::TargetDist => Value::Float(self.distance),
                EngineKey::TargetAngle => Value::Float(self.angle),
                EngineKey::HasTarget => Value::Bool(self.has_target),
                EngineKey::TargetVisible
                | EngineKey::TargetReachable
                | EngineKey::TargetDirectlyReachable => Value::Bool(self.has_target),
                EngineKey::SpawnPos | EngineKey::TargetPos => Value::Vec3([0.0; 3]),
                _ => return None,
            })
        }

        fn has_status_effect(&self, _effect: &str) -> bool {
            false
        }

        fn has_status_category(&self, _category: &str) -> bool {
            false
        }

        fn position(&self, _of: PositionType) -> Option<[f32; 3]> {
            Some([0.0; 3])
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
            Some([1.0, 0.0, 0.0])
        }
    }

    #[test]
    fn a_hundred_thousand_mobs_tick_within_the_budget() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local-data/content");
        let data = AiData::load(&dir, CLIENT_BUILD).expect("run `content-tool ai-data` first");
        let runtime = Runtime::new(data.clone());
        let with_tree: Vec<u32> = (0..data.archetypes.len() as u32)
            .filter(|index| {
                data.archetypes[*index as usize]
                    .trees
                    .iter()
                    .any(|tree| tree.tree.is_some())
            })
            .collect();
        let mut mobs: Vec<(Brain, MobWorld)> = (0..MOBS)
            .map(|index| {
                let archetype = with_tree[index % with_tree.len()];
                let brain = Brain::new(&runtime, archetype, index as u64).expect("brain");
                let world = MobWorld {
                    now: 0,
                    distance: DISTANCE_START_M + (index % DISTANCE_STEPS) as f32 * DISTANCE_STEP_M,
                    angle: (index % ANGLE_STEPS) as f32 * ANGLE_STEP_DEG,
                    has_target: index % 2 == 0,
                };
                (brain, world)
            })
            .collect();
        let mut out = Vec::new();
        for tick in 0..WARM_UP_TICKS {
            for (brain, world) in &mut mobs {
                world.now = tick * TICK_MS;
                out.clear();
                brain.tick(&runtime, world, &mut out);
            }
        }
        let started = Instant::now();
        for (brain, world) in &mut mobs {
            world.now = WARM_UP_TICKS * TICK_MS;
            out.clear();
            brain.tick(&runtime, world, &mut out);
        }
        let elapsed = started.elapsed();
        let state: usize = mobs.iter().map(|(brain, _)| brain.state_bytes()).sum();
        let per_mob = state / MOBS;
        assert!(elapsed < TICK_BUDGET, "{MOBS} mobs took {elapsed:?}");
        assert!(per_mob < STATE_BUDGET_BYTES, "{per_mob} bytes per mob");
        let running = mobs
            .iter()
            .filter(|(brain, _)| brain.running_leaves(&data).next().is_some())
            .count();
        assert!(running * 2 > MOBS, "only {running} mobs run a task");
    }
}
