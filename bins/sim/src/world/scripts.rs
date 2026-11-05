// SPDX-License-Identifier: AGPL-3.0-or-later
//! The script engine (admin studio spec section 6.6): each script of the level is compiled once
//! into nodes with resolved links. A trigger starts a run (with the player it is about, when
//! there is one); a run walks the nodes inside the tick, waits at a `wait` block, takes the
//! `true` or `false` output of a condition, and forks where an output has several links. Each
//! script may take at most [`STEP_LIMIT`] steps per tick, so a bad script cannot stall the sim.

use std::collections::{HashMap, HashSet, VecDeque};

use nw_bus::{Point, ScriptLogLine};
use nw_common::time::unix_ms;
use nw_formats::objectstream::lowercase_crc32;
use nw_world::script::{FALSE, LevelDesign, NEXT, ScriptGraph, ScriptValue, TRUE};

use super::{Player, SimWorld};
use crate::world::Body;
use nw_sim_core::Position;

/// Block steps one script may take in one tick (spec section 6.6).
pub(crate) const STEP_LIMIT: u32 = 1_000;
/// Runs one script may have at once; a trigger or a fork beyond it is dropped with a log line,
/// so a fork inside a loop cannot grow the runs for ever.
pub(crate) const MAX_RUNS_PER_SCRIPT: usize = 1_000;
/// The note of a run that was not started because its script has [`MAX_RUNS_PER_SCRIPT`].
const TOO_MANY_RUNS: &str = "too many runs: this one is dropped";
/// Run log lines kept between two ticks; older ones are dropped.
const LOG_LIMIT: usize = 512;
/// The `quest_state` values.
const STATE_NONE: &str = "none";
const STATE_ACTIVE: &str = "active";
const STATE_DONE: &str = "done";
const PERCENT: u32 = 100;
/// The note of a block that needs a player in a run without one.
const NO_PLAYER: &str = "no player: nothing done";
/// The note of a block whose point is in another sim's cells.
const OTHER_SIM: &str = "the point is another sim's: nothing done";
/// The note of a `give_item` whose item did not all go into the player's bag (an unknown item,
/// no bag, or a full one: what did not fit drops at the player's feet).
const NOT_ALL_GIVEN: &str = "not all in the bag";

/// The state of an objective a `quest_state` block asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestWanted {
    None,
    Active,
    Done,
}

/// A compiled block.
#[derive(Debug, Clone, PartialEq)]
enum Block {
    EntersArea {
        center: Position,
        radius_m: f32,
    },
    LeavesArea {
        center: Position,
        radius_m: f32,
    },
    MobDies {
        vitals: Option<u32>,
    },
    Timer {
        every_ms: u64,
    },
    QuestDone {
        objective: String,
    },
    EventStarts {
        event: String,
    },
    RandomChance {
        percent: u32,
    },
    HasItem {
        item: String,
        quantity: u32,
    },
    QuestState {
        objective: String,
        wanted: QuestWanted,
    },
    FlagIs {
        flag: String,
        value: i64,
    },
    MobsAlive {
        at_most: u32,
    },
    SpawnMobs {
        vitals: String,
        level: u32,
        count: u32,
        at: Point,
    },
    DespawnMobs,
    GiveItem {
        item: String,
        quantity: u32,
    },
    StartQuest {
        objective: String,
    },
    SetFlag {
        flag: String,
        value: i64,
    },
    StartEvent {
        event: String,
        at: Point,
    },
    Wait {
        ms: u64,
    },
}

/// One compiled node.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    id: String,
    block_name: String,
    block: Block,
    next: Vec<usize>,
    on_true: Vec<usize>,
    on_false: Vec<usize>,
}

/// One compiled script.
#[derive(Debug, Clone, PartialEq)]
struct Script {
    id: String,
    nodes: Vec<Node>,
}

/// What a script keeps between runs.
#[derive(Debug, Clone, Default)]
struct ScriptState {
    /// The players inside each area trigger.
    inside: HashMap<usize, HashSet<u64>>,
    /// When each timer trigger fires next.
    timer_due: HashMap<usize, u64>,
    flags: HashMap<String, i64>,
    /// The GM mobs the script spawned.
    mobs: Vec<u64>,
}

/// One run of a script.
#[derive(Debug, Clone, PartialEq)]
struct Run {
    script: usize,
    node: usize,
    player: Option<u64>,
    wait_until: Option<u64>,
}

/// A game event that triggers scripts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScriptEvent {
    /// A player of this sim killed a mob of the vitals CRC.
    MobDied { vitals: u32, killer: u64 },
    /// A player finished an objective.
    QuestDone { player: u64, objective: String },
    /// An event started.
    EventStarted { event: String },
}

/// The scripts of the level, their state, their runs and their log.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scripts {
    scripts: Vec<Script>,
    states: Vec<ScriptState>,
    runs: Vec<Run>,
    pending: Vec<ScriptEvent>,
    log: VecDeque<ScriptLogLine>,
    /// The players on this sim at the last tick. One that arrives (a login or a handoff from
    /// another sim) is not "entering" an area it already stands in.
    present: HashSet<u64>,
}

fn number(graph_node: &nw_world::script::ScriptNode, name: &str) -> Option<f64> {
    graph_node.values.get(name).and_then(ScriptValue::number)
}

fn text(graph_node: &nw_world::script::ScriptNode, name: &str) -> Option<String> {
    graph_node
        .values
        .get(name)
        .and_then(ScriptValue::text)
        .map(str::to_owned)
}

fn position(graph_node: &nw_world::script::ScriptNode, name: &str) -> Option<[f32; 3]> {
    graph_node.values.get(name).and_then(ScriptValue::position)
}

/// A whole number value; the check made it a whole number 0 or more.
fn whole(graph_node: &nw_world::script::ScriptNode, name: &str) -> Option<u64> {
    number(graph_node, name).map(|value| value.max(0.0) as u64)
}

fn point([x, y, z]: [f32; 3]) -> Point {
    Point { x, y, z }
}

/// The block of a node; `None` for an unknown block or a missing value (the design check
/// refuses both).
fn compile_block(node: &nw_world::script::ScriptNode) -> Option<Block> {
    let small = |name: &str| whole(node, name).and_then(|value| u32::try_from(value).ok());
    let area = || {
        Some((
            Position::from(point(position(node, "center")?)),
            number(node, "radius_m")? as f32,
        ))
    };
    Some(match node.block.as_str() {
        "player_enters_area" => {
            let (center, radius_m) = area()?;
            Block::EntersArea { center, radius_m }
        }
        "player_leaves_area" => {
            let (center, radius_m) = area()?;
            Block::LeavesArea { center, radius_m }
        }
        "mob_dies" => Block::MobDies {
            vitals: text(node, "vitals").map(|id| lowercase_crc32(&id)),
        },
        "timer" => Block::Timer {
            every_ms: whole(node, "every_ms")?.max(1),
        },
        "quest_done" => Block::QuestDone {
            objective: text(node, "objective")?,
        },
        "event_starts" => Block::EventStarts {
            event: text(node, "event")?,
        },
        "random_chance" => Block::RandomChance {
            percent: small("percent")?,
        },
        "has_item" => Block::HasItem {
            item: text(node, "item")?,
            quantity: small("quantity").unwrap_or(1).max(1),
        },
        "quest_state" => Block::QuestState {
            objective: text(node, "objective")?,
            wanted: match text(node, "state")?.as_str() {
                STATE_NONE => QuestWanted::None,
                STATE_ACTIVE => QuestWanted::Active,
                STATE_DONE => QuestWanted::Done,
                _ => return None,
            },
        },
        "flag_is" => Block::FlagIs {
            flag: text(node, "flag")?,
            value: i64::try_from(whole(node, "value")?).ok()?,
        },
        "mobs_alive" => Block::MobsAlive {
            at_most: small("at_most")?,
        },
        "spawn_mobs" => Block::SpawnMobs {
            vitals: text(node, "vitals")?,
            level: small("level")?,
            count: small("count")?,
            at: point(position(node, "at")?),
        },
        "despawn_mobs" => Block::DespawnMobs,
        "give_item" => Block::GiveItem {
            item: text(node, "item")?,
            quantity: small("quantity").unwrap_or(1).max(1),
        },
        "start_quest" => Block::StartQuest {
            objective: text(node, "objective")?,
        },
        "set_flag" => Block::SetFlag {
            flag: text(node, "flag")?,
            value: i64::try_from(whole(node, "value")?).ok()?,
        },
        "start_event" => Block::StartEvent {
            event: text(node, "event")?,
            at: point(position(node, "at")?),
        },
        "wait" => Block::Wait {
            ms: whole(node, "ms")?,
        },
        _ => return None,
    })
}

/// Script `id` of `graph`; `None` when a node does not compile or the graph has a problem.
fn compile(id: &str, graph: &ScriptGraph) -> Option<Script> {
    if !graph.problems().is_empty() {
        return None;
    }
    let index: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(at, node)| (node.id.as_str(), at))
        .collect();
    let targets = |from: &str, output: &str| -> Vec<usize> {
        graph
            .links
            .iter()
            .filter(|link| link.from == from && link.output == output)
            .filter_map(|link| index.get(link.to.as_str()).copied())
            .collect()
    };
    let nodes = graph
        .nodes
        .iter()
        .map(|node| {
            Some(Node {
                id: node.id.clone(),
                block_name: node.block.clone(),
                block: compile_block(node)?,
                next: targets(&node.id, NEXT),
                on_true: targets(&node.id, TRUE),
                on_false: targets(&node.id, FALSE),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Script {
        id: id.to_owned(),
        nodes,
    })
}

impl Scripts {
    /// The scripts of `design`; one that does not compile is left out with a warning.
    pub(crate) fn new(design: &LevelDesign) -> Self {
        let scripts: Vec<Script> = design
            .scripts
            .iter()
            .filter_map(|(id, graph)| {
                let script = compile(id, graph);
                if script.is_none() {
                    tracing::warn!(script = %id, "script does not compile");
                }
                script
            })
            .collect();
        Self {
            states: vec![ScriptState::default(); scripts.len()],
            scripts,
            ..Self::default()
        }
    }

    /// Queues `event` for the triggers of the next script tick.
    pub(crate) fn push(&mut self, event: ScriptEvent) {
        self.pending.push(event);
    }

    /// The run log lines since the last call.
    pub(crate) fn take_log(&mut self) -> Vec<ScriptLogLine> {
        self.log.drain(..).collect()
    }

    fn note(&mut self, run: &Run, note: &str) {
        let script = &self.scripts[run.script];
        let node = &script.nodes[run.node];
        if self.log.len() == LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(ScriptLogLine {
            time_ms: unix_ms(),
            script: script.id.clone(),
            node: node.id.clone(),
            block: node.block_name.clone(),
            player: run.player,
            note: note.to_owned(),
        });
    }

    /// The runs the triggers start now: area triggers from `players` (number, position),
    /// timers at `now_ms`, and the queued events.
    fn fire_triggers(&mut self, players: &[(u64, Position)], now_ms: u64) {
        let pending = std::mem::take(&mut self.pending);
        let mut started = Vec::new();
        for (script, compiled) in self.scripts.iter().enumerate() {
            let state = &mut self.states[script];
            for (node, compiled_node) in compiled.nodes.iter().enumerate() {
                let run = |player| Run {
                    script,
                    node,
                    player,
                    wait_until: None,
                };
                match &compiled_node.block {
                    Block::EntersArea { center, radius_m }
                    | Block::LeavesArea { center, radius_m } => {
                        let now: HashSet<u64> = players
                            .iter()
                            .filter(|(_, at)| at.horizontal_distance(*center) <= *radius_m)
                            .map(|(number, _)| *number)
                            .collect();
                        let before = state.inside.insert(node, now.clone()).unwrap_or_default();
                        let entering = matches!(compiled_node.block, Block::EntersArea { .. });
                        for (number, _) in players {
                            if !self.present.contains(number) {
                                continue;
                            }
                            let fires = if entering {
                                now.contains(number) && !before.contains(number)
                            } else {
                                before.contains(number) && !now.contains(number)
                            };
                            if fires {
                                started.push(run(Some(*number)));
                            }
                        }
                    }
                    Block::Timer { every_ms } => {
                        let due = state
                            .timer_due
                            .entry(node)
                            .or_insert_with(|| now_ms.saturating_add(*every_ms));
                        if now_ms >= *due {
                            *due = now_ms.saturating_add(*every_ms);
                            started.push(run(None));
                        }
                    }
                    Block::MobDies { vitals } => {
                        for event in &pending {
                            if let ScriptEvent::MobDied {
                                vitals: died,
                                killer,
                            } = event
                                && vitals.is_none_or(|wanted| wanted == *died)
                            {
                                started.push(run(Some(*killer)));
                            }
                        }
                    }
                    Block::QuestDone { objective } => {
                        for event in &pending {
                            if let ScriptEvent::QuestDone {
                                player,
                                objective: done,
                            } = event
                                && done.eq_ignore_ascii_case(objective)
                            {
                                started.push(run(Some(*player)));
                            }
                        }
                    }
                    Block::EventStarts { event } => {
                        for queued in &pending {
                            if let ScriptEvent::EventStarted {
                                event: started_event,
                            } = queued
                                && started_event.eq_ignore_ascii_case(event)
                            {
                                started.push(run(None));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.present = players.iter().map(|(number, _)| *number).collect();
        for run in started {
            self.add_run(run);
        }
    }

    /// Adds `run` unless its script has [`MAX_RUNS_PER_SCRIPT`] runs.
    fn add_run(&mut self, run: Run) {
        let running = self
            .runs
            .iter()
            .filter(|other| other.script == run.script)
            .count();
        if running >= MAX_RUNS_PER_SCRIPT {
            self.note(&run, TOO_MANY_RUNS);
        } else {
            self.runs.push(run);
        }
    }
}

/// Where a step leaves a run.
enum Step {
    /// Go on to these nodes (none: the run ends).
    To(Vec<usize>),
    /// Wait here.
    Hold,
}

impl SimWorld {
    /// The scripts of the level: compiled now, with no runs.
    pub fn set_design(&mut self, design: &LevelDesign) {
        self.scripts = Scripts::new(design);
        self.events = super::events::Events::new(design.events.clone());
    }

    /// The run log lines since the last call, for the admin tool.
    pub fn take_script_log(&mut self) -> Vec<ScriptLogLine> {
        self.scripts.take_log()
    }

    /// A player of this sim killed mob `mob`: the `mob_dies` scripts run.
    pub(super) fn note_kill(&mut self, mob: u64, killer: u64) {
        if self.player_by_number(killer).is_none() {
            return;
        }
        if let Some(vitals) = self.mobs.vitals_of(mob) {
            self.scripts.push(ScriptEvent::MobDied { vitals, killer });
        }
    }

    /// Fires the triggers and walks the runs.
    pub(super) fn tick_scripts(&mut self, now_ms: u64) {
        if self.scripts.scripts.is_empty() {
            self.scripts.pending.clear();
            return;
        }
        let players: Vec<(u64, Position)> = {
            let mut query = self.world.query::<(&Player, &Body)>();
            query
                .iter(&self.world)
                .map(|(player, body)| (player.number, body.motion.position()))
                .collect()
        };
        let mut engine = std::mem::take(&mut self.scripts);
        engine.fire_triggers(&players, now_ms);
        let mut queue: VecDeque<Run> = std::mem::take(&mut engine.runs).into();
        let mut steps = vec![0_u32; engine.scripts.len()];
        let mut runs = vec![0_usize; engine.scripts.len()];
        for run in &queue {
            runs[run.script] += 1;
        }
        let mut kept = Vec::new();
        while let Some(mut run) = queue.pop_front() {
            loop {
                // A waiting run costs no step.
                if run.wait_until.is_some_and(|until| now_ms < until) {
                    kept.push(run);
                    break;
                }
                if steps[run.script] >= STEP_LIMIT {
                    kept.push(run);
                    break;
                }
                steps[run.script] += 1;
                match self.step(&mut engine, &mut run, now_ms) {
                    Step::Hold => {
                        kept.push(run);
                        break;
                    }
                    Step::To(next) => {
                        let mut next = next.into_iter();
                        let Some(first) = next.next() else {
                            break;
                        };
                        for fork in next {
                            if runs[run.script] >= MAX_RUNS_PER_SCRIPT {
                                engine.note(&run, TOO_MANY_RUNS);
                                break;
                            }
                            runs[run.script] += 1;
                            queue.push_back(Run {
                                node: fork,
                                wait_until: None,
                                ..run.clone()
                            });
                        }
                        run.node = first;
                        run.wait_until = None;
                    }
                }
            }
        }
        engine.runs = kept;
        // Scripts may have queued events (an event started) for the next tick.
        engine.pending.append(&mut self.scripts.pending);
        self.scripts = engine;
    }

    /// Carries out the node of `run`.
    fn step(&mut self, engine: &mut Scripts, run: &mut Run, now_ms: u64) -> Step {
        let node = engine.scripts[run.script].nodes[run.node].clone();
        let state = run.script;
        let pick = |holds: bool| {
            Step::To(if holds {
                node.on_true.clone()
            } else {
                node.on_false.clone()
            })
        };
        let next = || Step::To(node.next.clone());
        match &node.block {
            Block::EntersArea { .. }
            | Block::LeavesArea { .. }
            | Block::MobDies { .. }
            | Block::Timer { .. }
            | Block::QuestDone { .. }
            | Block::EventStarts { .. } => {
                engine.note(run, "");
                next()
            }
            Block::Wait { ms } => match run.wait_until {
                None => {
                    run.wait_until = Some(now_ms.saturating_add(*ms));
                    engine.note(run, "waits");
                    Step::Hold
                }
                Some(until) if now_ms < until => Step::Hold,
                Some(_) => next(),
            },
            Block::RandomChance { percent } => {
                let holds = self.dice.below(PERCENT) < *percent;
                engine.note(run, if holds { TRUE } else { FALSE });
                pick(holds)
            }
            Block::HasItem { item, quantity } => {
                let held = run.player.and_then(|number| self.carried(number, item));
                let holds = held.is_some_and(|count| count >= *quantity);
                engine.note(
                    run,
                    if run.player.is_none() {
                        NO_PLAYER
                    } else if holds {
                        TRUE
                    } else {
                        FALSE
                    },
                );
                pick(holds)
            }
            Block::QuestState { objective, wanted } => {
                let found = run
                    .player
                    .map(|number| self.quest_state_of(number, objective));
                let holds = found.is_some_and(|state| state == *wanted);
                engine.note(
                    run,
                    if run.player.is_none() {
                        NO_PLAYER
                    } else if holds {
                        TRUE
                    } else {
                        FALSE
                    },
                );
                pick(holds)
            }
            Block::FlagIs { flag, value } => {
                let holds = engine.states[state].flags.get(flag).copied().unwrap_or(0) == *value;
                engine.note(run, if holds { TRUE } else { FALSE });
                pick(holds)
            }
            Block::MobsAlive { at_most } => {
                let alive = self.alive(&engine.states[state].mobs);
                let holds = alive <= *at_most as usize;
                engine.note(run, &format!("{alive} alive"));
                pick(holds)
            }
            Block::SpawnMobs {
                vitals,
                level,
                count,
                at,
            } => {
                // Every sim of the level runs a timer; only the owner of the point spawns.
                if !self.owns(*at) {
                    engine.note(run, OTHER_SIM);
                    return next();
                }
                match self.spawn_gm(vitals, *level, *at, *count) {
                    Ok(entities) => {
                        engine.note(run, &format!("{} spawned", entities.len()));
                        engine.states[state].mobs.extend(entities);
                    }
                    Err(reason) => engine.note(run, &reason),
                }
                next()
            }
            Block::DespawnMobs => {
                let mobs = std::mem::take(&mut engine.states[state].mobs);
                let removed = self.remove_gm(&mobs);
                engine.note(run, &format!("{removed} removed"));
                next()
            }
            Block::GiveItem { item, quantity } => {
                match run.player {
                    Some(number) => {
                        let given = self.give_loot(number, vec![(item.clone(), *quantity)], now_ms);
                        engine.note(run, if given { "" } else { NOT_ALL_GIVEN });
                    }
                    None => engine.note(run, NO_PLAYER),
                }
                next()
            }
            Block::StartQuest { objective } => {
                match run.player {
                    Some(number) => match self.start_quest(number, objective, now_ms) {
                        Ok(()) => engine.note(run, ""),
                        Err(refusal) => engine.note(run, &refusal.to_string()),
                    },
                    None => engine.note(run, NO_PLAYER),
                }
                next()
            }
            Block::SetFlag { flag, value } => {
                engine.states[state].flags.insert(flag.clone(), *value);
                engine.note(run, "");
                next()
            }
            Block::StartEvent { event, at } => {
                match self.start_event(event, *at, now_ms) {
                    Ok(()) => engine.note(run, ""),
                    Err(reason) => engine.note(run, &reason),
                }
                next()
            }
        }
    }

    /// Whether this sim owns the cell of `at`.
    pub(super) fn owns(&self, at: Point) -> bool {
        self.owns_position(Position::from(at))
    }

    /// Whether this sim owns the cell of `position`.
    pub(super) fn owns_position(&self, position: Position) -> bool {
        self.cells
            .as_ref()
            .is_some_and(|cells| cells.owners.owner_at(position) == Some(cells.me))
    }

    /// How many of `item` player `number` carries; `None` when it is not here or has no bag.
    fn carried(&mut self, number: u64, item: &str) -> Option<u32> {
        let entity = self.player_by_number(number)?;
        let player = self.world.get::<Player>(entity)?;
        let gear = player.gear.as_ref()?;
        Some(
            gear.carried()
                .iter()
                .filter(|stack| stack.item_id.eq_ignore_ascii_case(item))
                .map(|stack| stack.quantity)
                .sum(),
        )
    }

    /// Where objective `objective` stands for player `number`.
    fn quest_state_of(&mut self, number: u64, objective: &str) -> QuestWanted {
        if self
            .quests_of_player(number)
            .iter()
            .any(|quest| quest.def.id.eq_ignore_ascii_case(objective))
        {
            return QuestWanted::Active;
        }
        let done = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Player>(entity))
            .is_some_and(|player| {
                player
                    .done_quests
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(objective))
            });
        if done {
            QuestWanted::Done
        } else {
            QuestWanted::None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nw_bus::{AdminAction, AdminCommand};
    use nw_world::script::{ScriptLink, ScriptNode};
    use uuid::Uuid;

    use super::super::tests::{NOW, admit_at, connection, table, world_with_spawns};
    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB};

    const PLAYER: u64 = 7;
    /// `admit_at` puts a player at (x, 10, 50).
    const PLAYER_X: f32 = 100.0;
    const AREA_RADIUS_M: f64 = 5.0;
    const TIMER_MS: f64 = 1_000.0;
    const WAIT_MS: f64 = 500.0;
    const TICK_MS: u64 = 33;

    fn node(id: &str, block: &str, values: &[(&str, ScriptValue)]) -> ScriptNode {
        ScriptNode {
            id: id.to_owned(),
            block: block.to_owned(),
            values: values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        }
    }

    fn link(from: &str, output: &str, to: &str) -> ScriptLink {
        ScriptLink {
            from: from.to_owned(),
            output: output.to_owned(),
            to: to.to_owned(),
        }
    }

    fn flag(name: &str, value: f64) -> Vec<(&'static str, ScriptValue)> {
        vec![
            ("flag", ScriptValue::Text(name.to_owned())),
            ("value", ScriptValue::Number(value)),
        ]
    }

    /// A world with one player and the scripts of `scripts`.
    fn script_world(scripts: Vec<(&str, ScriptGraph)>) -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a"])).expect("table");
        world
            .admit(&admit_at(PLAYER, PLAYER_X, true), NOW, NOW)
            .expect("admit");
        let design = LevelDesign {
            scripts: scripts
                .into_iter()
                .map(|(id, graph)| (id.to_owned(), graph))
                .collect::<BTreeMap<_, _>>(),
            ..LevelDesign::default()
        };
        world.set_design(&design);
        world
    }

    fn flag_of(world: &SimWorld, name: &str) -> i64 {
        world.scripts.states[0]
            .flags
            .get(name)
            .copied()
            .unwrap_or_default()
    }

    fn area_values() -> Vec<(&'static str, ScriptValue)> {
        vec![
            ("center", ScriptValue::Position([PLAYER_X, 10.0, 50.0])),
            ("radius_m", ScriptValue::Number(AREA_RADIUS_M)),
        ]
    }

    fn area_script() -> ScriptGraph {
        ScriptGraph {
            nodes: vec![
                node("enter", "player_enters_area", &area_values()),
                node("leave", "player_leaves_area", &area_values()),
                node("mark_in", "set_flag", &flag("in", 1.0)),
                node("mark_out", "set_flag", &flag("out", 1.0)),
            ],
            links: vec![
                link("enter", NEXT, "mark_in"),
                link("leave", NEXT, "mark_out"),
            ],
        }
    }

    fn teleport(world: &mut SimWorld, x: f32) {
        let command = AdminCommand {
            request: Uuid::nil(),
            actor: None,
            action: AdminAction::TeleportPlayer {
                entity: PLAYER,
                to: Point {
                    x,
                    y: 10.0,
                    z: 50.0,
                },
            },
        };
        world.admin(&command, NOW);
    }

    #[test]
    fn an_area_fires_on_leaving_and_on_entry() {
        let mut world = script_world(vec![("area", area_script())]);
        world.tick_scripts(NOW);
        assert_eq!(
            (flag_of(&world, "in"), flag_of(&world, "out")),
            (0, 0),
            "a player who arrives inside does not enter"
        );
        teleport(&mut world, PLAYER_X + 50.0);
        world.tick_scripts(NOW + TICK_MS);
        teleport(&mut world, PLAYER_X);
        world.tick_scripts(NOW + 2 * TICK_MS);
        assert_eq!((flag_of(&world, "in"), flag_of(&world, "out")), (1, 1));
        let blocks: Vec<String> = world
            .take_script_log()
            .into_iter()
            .map(|line| line.block)
            .collect();
        assert_eq!(
            blocks,
            [
                "player_leaves_area",
                "set_flag",
                "player_enters_area",
                "set_flag"
            ]
        );
    }

    /// A `give_item` whose item does not reach the bag (a player without a bag) says so in the
    /// run log, not "passed".
    #[test]
    fn an_item_not_given_is_noted_as_not_given() {
        let gift = ScriptGraph {
            nodes: vec![
                node("enter", "player_enters_area", &area_values()),
                node(
                    "give",
                    "give_item",
                    &[(
                        "item",
                        ScriptValue::Text(super::super::gear::tests::FLINT.to_owned()),
                    )],
                ),
            ],
            links: vec![link("enter", NEXT, "give")],
        };
        let mut world = script_world(vec![("gift", gift)]);
        std::sync::Arc::make_mut(&mut world.content).kits = super::super::gear::tests::kits();
        let entity = world.player_by_number(PLAYER).expect("player");
        world.world.get_mut::<Player>(entity).expect("player").gear = None;
        world.tick_scripts(NOW);
        teleport(&mut world, PLAYER_X + 50.0);
        world.tick_scripts(NOW + TICK_MS);
        teleport(&mut world, PLAYER_X);
        world.tick_scripts(NOW + 2 * TICK_MS);
        let notes: Vec<String> = world
            .take_script_log()
            .into_iter()
            .filter(|line| line.block == "give_item")
            .map(|line| line.note)
            .collect();
        assert_eq!(notes, [NOT_ALL_GIVEN]);
    }

    #[test]
    fn a_player_handed_in_inside_an_area_does_not_enter_it() {
        let mut world = script_world(vec![("area", area_script())]);
        world.tick_scripts(NOW);
        world.leave(&connection(PLAYER)).expect("left");
        world.tick_scripts(NOW + TICK_MS);
        world
            .admit(&admit_at(PLAYER, PLAYER_X, false), NOW, NOW)
            .expect("back");
        world.tick_scripts(NOW + 2 * TICK_MS);
        assert_eq!(flag_of(&world, "in"), 0);
    }

    #[test]
    fn an_event_started_by_a_script_runs_its_event_starts_scripts() {
        use nw_world::script::{EventWave, GameEvent};
        let starter = ScriptGraph {
            nodes: vec![
                node(
                    "tick",
                    "timer",
                    &[("every_ms", ScriptValue::Number(TIMER_MS))],
                ),
                node(
                    "start",
                    "start_event",
                    &[
                        ("event", ScriptValue::Text("raid".to_owned())),
                        ("at", ScriptValue::Position([PLAYER_X, 10.0, 50.0])),
                    ],
                ),
            ],
            links: vec![link("tick", NEXT, "start")],
        };
        let reaction = ScriptGraph {
            nodes: vec![
                node(
                    "began",
                    "event_starts",
                    &[("event", ScriptValue::Text("raid".to_owned()))],
                ),
                node("mark", "set_flag", &flag("raided", 1.0)),
            ],
            links: vec![link("began", NEXT, "mark")],
        };
        let mut world = script_world(Vec::new());
        world.set_design(&LevelDesign {
            scripts: BTreeMap::from([
                ("a_reaction".to_owned(), reaction),
                ("b_starter".to_owned(), starter),
            ]),
            events: BTreeMap::from([(
                "raid".to_owned(),
                GameEvent {
                    waves: vec![EventWave {
                        vitals: TEST_MOB.to_owned(),
                        level: TEST_LEVEL,
                        count: 1,
                        delay_ms: 0,
                        offset: [0.0, 0.0, 0.0],
                    }],
                    end_after_ms: None,
                    end_when_cleared: false,
                },
            )]),
        });
        let fire = NOW + TIMER_MS as u64;
        world.tick_scripts(NOW);
        world.tick_scripts(fire);
        world.tick_scripts(fire + TICK_MS);
        assert_eq!(flag_of(&world, "raided"), 1);
    }

    #[test]
    fn a_fork_in_a_loop_stops_at_the_run_limit() {
        let graph = ScriptGraph {
            nodes: vec![
                node(
                    "tick",
                    "timer",
                    &[("every_ms", ScriptValue::Number(TIMER_MS))],
                ),
                node("pause", "wait", &[("ms", ScriptValue::Number(0.0))]),
                node("split", "set_flag", &flag("x", 1.0)),
            ],
            links: vec![
                link("tick", NEXT, "pause"),
                link("pause", NEXT, "split"),
                link("split", NEXT, "pause"),
                link("split", NEXT, "pause"),
            ],
        };
        let mut world = script_world(vec![("fork", graph)]);
        let mut now = NOW;
        for _ in 0..40 {
            now += TIMER_MS as u64;
            world.tick_scripts(now);
        }
        assert!(world.scripts.runs.len() <= MAX_RUNS_PER_SCRIPT);
    }

    #[test]
    fn a_player_who_leaves_the_sim_fires_no_leave() {
        let mut world = script_world(vec![("area", area_script())]);
        world.tick_scripts(NOW);
        world.leave(&connection(PLAYER)).expect("left");
        world.tick_scripts(NOW + TICK_MS);
        assert_eq!(flag_of(&world, "out"), 0);
    }

    #[test]
    fn a_timer_waits_branches_spawns_and_despawns() {
        let graph = ScriptGraph {
            nodes: vec![
                node(
                    "tick",
                    "timer",
                    &[("every_ms", ScriptValue::Number(TIMER_MS))],
                ),
                node("pause", "wait", &[("ms", ScriptValue::Number(WAIT_MS))]),
                node(
                    "spawn",
                    "spawn_mobs",
                    &[
                        ("vitals", ScriptValue::Text(TEST_MOB.to_owned())),
                        ("level", ScriptValue::Number(f64::from(TEST_LEVEL))),
                        ("count", ScriptValue::Number(2.0)),
                        ("at", ScriptValue::Position([PLAYER_X, 10.0, 50.0])),
                    ],
                ),
                node(
                    "few",
                    "mobs_alive",
                    &[("at_most", ScriptValue::Number(5.0))],
                ),
                node("clear", "despawn_mobs", &[]),
                node("mark", "set_flag", &flag("many", 1.0)),
            ],
            links: vec![
                link("tick", NEXT, "pause"),
                link("pause", NEXT, "spawn"),
                link("spawn", NEXT, "few"),
                link("few", TRUE, "clear"),
                link("few", FALSE, "mark"),
            ],
        };
        let mut world = script_world(vec![("waves", graph)]);
        world.tick_scripts(NOW);
        let timer_ms = TIMER_MS as u64;
        let wait_ms = WAIT_MS as u64;
        world.tick_scripts(NOW + timer_ms);
        assert!(world.mobs.gm_mobs().is_empty(), "the run waits");
        world.tick_scripts(NOW + timer_ms + wait_ms);
        assert!(
            world.mobs.gm_mobs().is_empty(),
            "spawned, 2 alive, then cleared"
        );
        assert_eq!(flag_of(&world, "many"), 0);
        let notes: Vec<String> = world
            .take_script_log()
            .into_iter()
            .map(|line| format!("{} {}", line.block, line.note))
            .collect();
        assert_eq!(
            notes,
            [
                "timer ",
                "wait waits",
                "spawn_mobs 2 spawned",
                "mobs_alive 2 alive",
                "despawn_mobs 2 removed"
            ]
        );
    }

    #[test]
    fn only_the_owner_of_the_point_spawns() {
        let graph = ScriptGraph {
            nodes: vec![
                node(
                    "tick",
                    "timer",
                    &[("every_ms", ScriptValue::Number(TIMER_MS))],
                ),
                node(
                    "spawn",
                    "spawn_mobs",
                    &[
                        ("vitals", ScriptValue::Text(TEST_MOB.to_owned())),
                        ("level", ScriptValue::Number(f64::from(TEST_LEVEL))),
                        ("count", ScriptValue::Number(1.0)),
                        ("at", ScriptValue::Position([PLAYER_X, 10.0, 50.0])),
                    ],
                ),
            ],
            links: vec![link("tick", NEXT, "spawn")],
        };
        let mut world = script_world(vec![("spawner", graph)]);
        // The point's column now belongs to another sim.
        world
            .set_table(&table(&["sim-other", "sim-a"]))
            .expect("table");
        world.tick_scripts(NOW);
        world.tick_scripts(NOW + TIMER_MS as u64);
        assert!(world.mobs.gm_mobs().is_empty());
        assert_eq!(world.take_script_log()[1].note, OTHER_SIM);
    }

    #[test]
    fn the_step_limit_holds_a_long_run_to_the_next_tick() {
        const CHAIN: usize = 1_500;
        let mut nodes = vec![node(
            "tick",
            "timer",
            &[("every_ms", ScriptValue::Number(TIMER_MS))],
        )];
        let mut links = Vec::new();
        let mut previous = "tick".to_owned();
        for step in 0..CHAIN {
            let id = format!("n{step}");
            nodes.push(node(&id, "set_flag", &flag("last", step as f64)));
            links.push(link(&previous, NEXT, &id));
            previous = id;
        }
        let mut world = script_world(vec![("long", ScriptGraph { nodes, links })]);
        let fire = NOW + TIMER_MS as u64;
        world.tick_scripts(NOW);
        world.tick_scripts(fire);
        assert_eq!(
            world.scripts.runs.len(),
            1,
            "the run waits for the next tick"
        );
        assert!(flag_of(&world, "last") < (CHAIN - 1) as i64);
        world.tick_scripts(fire + TICK_MS);
        assert!(world.scripts.runs.is_empty());
        assert_eq!(flag_of(&world, "last"), (CHAIN - 1) as i64);
    }

    #[test]
    fn a_kill_runs_the_mob_dies_trigger_with_the_killer() {
        let graph = ScriptGraph {
            nodes: vec![
                node(
                    "died",
                    "mob_dies",
                    &[("vitals", ScriptValue::Text(TEST_MOB.to_owned()))],
                ),
                node("mark", "set_flag", &flag("kills", 1.0)),
            ],
            links: vec![link("died", NEXT, "mark")],
        };
        let mut world = script_world(vec![("kills", graph)]);
        let at = Point {
            x: PLAYER_X,
            y: 10.0,
            z: 50.0,
        };
        let mob = world.spawn_gm(TEST_MOB, TEST_LEVEL, at, 1).expect("spawn")[0];
        world.note_kill(mob, PLAYER);
        world.note_kill(mob, PLAYER + 1);
        world.tick_scripts(NOW);
        assert_eq!(flag_of(&world, "kills"), 1);
        assert_eq!(world.take_script_log()[0].player, Some(PLAYER));
    }
}
