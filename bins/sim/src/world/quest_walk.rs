// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quest walk's character: a test player that plays one quest in a world of its own
//! (`sim quest-walk`). It takes the quest, plays its steps and turns it in through the calls a
//! game client makes (the talk with the NPC, the accept, the territory entries, the finished
//! topics, the cinematics and screens, the faction pick, the turn-in). A step whose client flow
//! it does not drive (a kill, a gather, a craft, a catch) is played as the game event the server
//! makes of that flow, after a check that the level holds what the step needs: a mob of the
//! type, a gatherable or another source of the item, a recipe, a place. A step it cannot check
//! or play stops the walk of its quest with the reason.

use std::collections::HashMap;

use nw_bus::{
    Admit, BusId, ConnectionRef, PlayerInventory, PlayerProgression, PlayerQuests, Point,
};
use nw_content::factions::{COVENANT, MARAUDERS, SYNDICATE, faction_requirement_met};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::client_input::TALK_KIND;
use nw_messages::{ClientFacetMsg, ConversationClientMsg, FacetHeader, InteractionRequest};
use nw_sim_core::Position;
use nw_world::records::VitalsSpawn;
use uuid::Uuid;

use super::{OWN_CARD_DELAY_MS, Placement, Player, SimWorld};
use crate::objectives::{Event, FishCaught, FishingStep, TaskKind, kills_count};
use crate::player_content::requirement_met;

/// The time of one world tick.
const TICK_MS: u64 = 33;
/// The ticks after each round of steps.
const TICKS_PER_ROUND: u64 = 3;
/// The time a round waits when its steps changed nothing: a timer or a patrol ends meanwhile.
const WAIT_MS: u64 = 60_000;
/// The most rounds of one quest: a quest that still runs after them has a step that never
/// ends.
const MAX_ROUNDS: u32 = 400;
/// The rounds in a row that may change nothing before the walk stops the quest.
const IDLE_ROUNDS: u32 = 2;
/// How far the walk goes from an area it must leave.
const LEAVE_DISTANCE_M: f32 = 500.0;
/// The name mark before an NPC id in a talk (`@12_JeongIseul`).
const NPC_MARK: &str = "@";
/// The length and weight of a fish the walk catches above a catch task's least values.
const CATCH_MARGIN: f32 = 1.0;
/// The item of a catch task that names no fish (Inferred: a task with no item takes any fish).
const ANY_FISH: &str = "AnyFish";
/// The factions a player may join, in the order the walk tries them.
const JOINABLE: [u8; 3] = [SYNDICATE, MARAUDERS, COVENANT];
/// The marks of an achievement expression ([`requirement_met`]).
const EXPRESSION_MARKS: [&str; 4] = ["&&", "||", "(", ")"];
/// The negation mark of an achievement expression.
const NOT_MARK: char = '!';
/// Starts the achievement of a lore note; the rest is its title (`quest_objects`).
const LORE_PREFIX: &str = "Lore_unlock_";
/// The enemy type of a kill step that counts player kills.
const PLAYER_ENEMY: &str = "Player";
/// How near a patrol's place the player stands while its stay counts.
const PATROL_REACH_M: f32 = 1.0;
/// The words of a client step in a `!questplan` line.
const CINEMATIC_START: &str = "cinematic-start";
const CINEMATIC_END: &str = "cinematic-end";
const CUTSCENE: &str = "cutscene";
const SCREEN: &str = "screen";
const FACTION: &str = "faction";
/// The words of a `!questplan` line: `plan <task> <what>`.
const PLAN_LINE: &str = "plan";
const PLAN_WAIT: &str = "wait";
const PLAN_STOP: &str = "stop";
const PLAN_CLIENT: &str = "client";
const PLAN_TALK: &str = "talk";
const PLAN_TOPIC: &str = "topic";
const PLAN_GO: &str = "go";
const PLAN_LEAVE: &str = "leave";
const PLAN_KILL: &str = "kill";
const PLAN_ITEM: &str = "item";
/// The word of a kill plan whose mobs count only as placed (by their spawn category).
const PLAN_PLACED: &str = "placed";
/// The word before the gatherable whose nodes give an item plan's item.
const PLAN_NODE: &str = "node";
const PLAN_ACHIEVEMENTS: &str = "achievements";
const PLAN_PROGRESSION: &str = "progression";
const PLAN_EVENT: &str = "event";
const PLAN_NOT_RUNNING: &str = "not running";

/// What the level holds that a quest's steps need: the places of its NPCs, territories and
/// mobs, who offers each quest, who holds each topic and what gives each achievement. The walk
/// checks the steps against it, and `!questplan` names its places (built at the sim's start by
/// [`crate::quest_walk::quest_guide`]).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestGuide {
    /// Each placed NPC of the level, by the CRC-32 of its lowercase id.
    pub npcs: HashMap<u32, Position>,
    /// A place in each territory: the first record of the level that lies in it.
    pub territory_spots: HashMap<u16, Position>,
    /// The NPCs whose conversation states offer each quest, by lowercase quest id.
    pub givers: HashMap<String, Vec<String>>,
    /// The NPCs that hold each conversation topic, by the CRC-32 of the lowercase topic id.
    pub topic_npcs: HashMap<u32, Vec<String>>,
    /// What gives each achievement (a quest's completion or a step's unlock), by lowercase id.
    pub achievement_sources: HashMap<String, String>,
    /// The success game events of the quests, lowercase: a quest's end fires its event.
    pub success_events: std::collections::HashSet<String>,
    /// Every vitals spawn of the level, also the encounter spawns the open map does not run.
    pub all_spawns: Vec<VitalsSpawn>,
    /// Where a character stands when no NPC gives a place.
    pub start: Position,
}

impl Default for QuestGuide {
    fn default() -> Self {
        Self {
            npcs: HashMap::new(),
            territory_spots: HashMap::new(),
            givers: HashMap::new(),
            topic_npcs: HashMap::new(),
            achievement_sources: HashMap::new(),
            success_events: std::collections::HashSet::new(),
            all_spawns: Vec::new(),
            start: Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
        }
    }
}

impl QuestGuide {
    /// The place of NPC `npc` (any case), when the level has it.
    fn npc_at(&self, npc: &str) -> Option<Position> {
        self.npcs.get(&lowercase_crc32(npc)).copied()
    }
}

/// How the walk of one quest ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// It ran from the accept to its end.
    Completed,
    /// A step cannot end on this server: the stop names it and why.
    Broken,
    /// A step needs something the walk cannot tell is there: the stop names it.
    NeedsLook,
    /// A step needs a flow the walk does not play (an expedition, a house).
    NotWalked,
    /// The quest did not start.
    NotStarted,
}

/// The walk of one quest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestWalk {
    /// The quest id.
    pub quest: String,
    /// How it ended.
    pub outcome: Outcome,
    /// The step it stopped at and why; `None` for a completed quest.
    pub stop: Option<String>,
    /// What the walk did that a reader should know: the steps played as events, the NPCs it
    /// did not find, the requirements it gave.
    pub notes: Vec<String>,
}

/// What one step's play did.
enum Play {
    /// The walk did what the step asks; with a note of how.
    Done(Option<String>),
    /// The step waits for time (a timer, a patrol stay) or for another quest.
    Waits,
    /// The step cannot end on this server.
    Broken(String),
    /// The walk cannot tell whether the level has what the step needs.
    NeedsLook(String),
    /// The step needs a flow the walk does not play.
    NotWalked(String),
}

/// Where one task of a quest stands: its id's CRC-32, its progress and its status.
type TaskMark = (u32, u32, u8);

/// A waiting step of one of the character's quests.
pub(crate) struct WaitingStep {
    quest: String,
    task: String,
    kind: TaskKind,
    progress: u32,
}

/// The connection of walk character `number` on gateway `gateway`.
fn walk_connection(gateway: &BusId, number: u64) -> ConnectionRef {
    ConnectionRef {
        gateway: gateway.clone(),
        connection: number,
    }
}

/// The facet header of walk character `number`'s messages.
fn header(number: u64) -> FacetHeader {
    FacetHeader::of(0, number)
}

/// The achievement names of expression `expression`, each with whether it is negated.
pub(super) fn expression_names(expression: &str) -> Vec<(String, bool)> {
    let mut spaced = expression.to_owned();
    for mark in EXPRESSION_MARKS {
        spaced = spaced.replace(mark, " ");
    }
    spaced
        .split_whitespace()
        .map(|word| match word.strip_prefix(NOT_MARK) {
            Some(name) => (name.to_owned(), true),
            None => (word.to_owned(), false),
        })
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

impl SimWorld {
    /// Walks quest `quest` with a new character `number` from `now_ms` on (advanced to the
    /// walk's end), and takes the character out again.
    pub(crate) fn walk_quest(
        &mut self,
        level: &QuestGuide,
        gateway: &BusId,
        number: u64,
        quest: &str,
        now_ms: &mut u64,
    ) -> QuestWalk {
        let connection = walk_connection(gateway, number);
        let mut walk = QuestWalk {
            quest: quest.to_owned(),
            outcome: Outcome::Completed,
            stop: None,
            notes: Vec::new(),
        };
        self.walk_quest_with(level, number, &connection, &mut walk, now_ms);
        self.leave(&connection);
        self.quests.remove(&number);
        walk
    }

    fn walk_quest_with(
        &mut self,
        level: &QuestGuide,
        number: u64,
        connection: &ConnectionRef,
        walk: &mut QuestWalk,
        now_ms: &mut u64,
    ) {
        let Some(def) = self.content.quests.quest(&walk.quest) else {
            walk.outcome = Outcome::NotStarted;
            walk.stop = Some("no such quest, or its root task is not a task row".to_owned());
            return;
        };
        let id = def.id.clone();
        let lower = id.to_lowercase();
        let givers = level.givers.get(&lower).cloned().unwrap_or_default();
        let giver = givers
            .iter()
            .find_map(|npc| level.npc_at(npc).map(|at| (npc.clone(), at)));
        match (&giver, givers.is_empty(), def.mission.is_some()) {
            (_, _, true) => walk
                .notes
                .push("a town board mission: taken without a board".to_owned()),
            (None, true, false) => walk
                .notes
                .push("no NPC offers it (a chain step or a scripted start)".to_owned()),
            (None, false, false) => walk.notes.push(format!(
                "no NPC that offers it is in the open world: {}",
                givers.join(", ")
            )),
            (Some(_), _, false) => {}
        }
        let start = giver.as_ref().map_or(level.start, |(_, at)| *at);
        let backstory = self.walk_backstory(def.requirement.as_deref());
        if let Err(reason) = self.walk_admit(number, connection, start, backstory, now_ms) {
            walk.outcome = Outcome::NotStarted;
            walk.stop = Some(reason);
            return;
        }
        self.walk_meet_requirement(level, number, def.requirement.as_deref(), walk, *now_ms);
        if let Some(faction) = def.required_faction.as_deref() {
            self.walk_join_faction(number, connection, faction, walk, *now_ms);
        }
        if let Some((npc, _)) = &giver {
            self.walk_talk(level, number, connection, npc, *now_ms);
        }
        let objective = lowercase_crc32(&lower);
        let accept = ClientFacetMsg::AcceptObjective {
            header: header(number),
            objective,
        };
        if def.mission.is_none() {
            let _ = self.facet_message(connection, accept, *now_ms);
        }
        if !self.walk_runs(number, &id) {
            match self.accept_quest(number, &id, None, *now_ms) {
                Ok(()) if def.mission.is_none() => walk
                    .notes
                    .push("the accept message did not start it; a direct start did".to_owned()),
                Ok(()) => {}
                Err(refusal) => {
                    walk.outcome = Outcome::NotStarted;
                    walk.stop = Some(refusal.to_string());
                    return;
                }
            }
        }
        let mut idle = 0;
        let mut turned_in = false;
        for _ in 0..MAX_ROUNDS {
            self.walk_ticks(now_ms);
            if !self.walk_runs(number, &id) {
                let done = self
                    .player_by_number(number)
                    .and_then(|entity| self.world.get::<Player>(entity))
                    .is_some_and(|player| {
                        player
                            .done_quests
                            .iter()
                            .any(|done| done.eq_ignore_ascii_case(&id))
                    });
                if !done {
                    walk.outcome = Outcome::Broken;
                    walk.stop = Some("the quest left the journal before its end".to_owned());
                }
                return;
            }
            if self.walk_finished(number, &id) {
                if turned_in {
                    walk.outcome = Outcome::Broken;
                    walk.stop =
                        Some("its tasks are done, but the turn-in does not end it".to_owned());
                    return;
                }
                if let Some(npc) = def.turn_in.as_deref() {
                    if level.npc_at(npc).is_none() {
                        walk.notes
                            .push(format!("its turn-in NPC {npc} is not in the open world"));
                    }
                    self.walk_talk(level, number, connection, npc, *now_ms);
                }
                let complete = ClientFacetMsg::CompleteObjective {
                    header: header(number),
                    objective,
                };
                let _ = self.facet_message(connection, complete, *now_ms);
                turned_in = true;
                continue;
            }
            let before = self.walk_signature(number);
            let steps = self.walk_waiting(number);
            let mut stops = Vec::new();
            let mut waits = false;
            let at = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<super::Body>(entity))
                .map_or(level.start, |body| body.motion.position());
            for step in &steps {
                let plan = self.walk_plan(level, step, at);
                match self.walk_act(level, number, connection, plan, *now_ms) {
                    Play::Done(note) => {
                        if let Some(note) = note {
                            let note = format!("{}: {note}", step.task);
                            if !walk.notes.contains(&note) {
                                walk.notes.push(note);
                            }
                        }
                    }
                    Play::Waits => waits = true,
                    Play::Broken(reason) => stops.push((Outcome::Broken, step, reason)),
                    Play::NeedsLook(reason) => stops.push((Outcome::NeedsLook, step, reason)),
                    Play::NotWalked(reason) => stops.push((Outcome::NotWalked, step, reason)),
                }
            }
            self.walk_ticks(now_ms);
            if self.walk_signature(number) != before {
                idle = 0;
                continue;
            }
            idle += 1;
            if waits || idle < IDLE_ROUNDS {
                *now_ms += WAIT_MS;
                continue;
            }
            stops.sort_by_key(|(outcome, _, _)| *outcome);
            let (outcome, stop) = match stops.first() {
                Some((outcome, step, reason)) => (*outcome, Self::stop_text(step, reason)),
                None => (
                    Outcome::Broken,
                    format!(
                        "no step progresses after the walk did what each asks: {}",
                        steps
                            .iter()
                            .map(|step| format!("{} ({})", step.task, step.kind.name()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
            };
            walk.outcome = outcome;
            walk.stop = Some(stop);
            return;
        }
        walk.outcome = Outcome::Broken;
        walk.stop = Some(format!("still running after {MAX_ROUNDS} rounds"));
    }

    /// The stop text of `step` for `reason`.
    fn stop_text(step: &WaitingStep, reason: &str) -> String {
        format!(
            "{} / {} ({}): {reason}",
            step.quest,
            step.task,
            step.kind.name()
        )
    }

    /// Admits walk character `number` at `at` as a new character and waits until its own card
    /// went out and its first time sync came.
    fn walk_admit(
        &mut self,
        number: u64,
        connection: &ConnectionRef,
        at: Position,
        backstory: Option<String>,
        now_ms: &mut u64,
    ) -> Result<(), String> {
        let admit = Admit {
            connection: connection.clone(),
            account: Uuid::new_v4(),
            character: Uuid::new_v4(),
            name: format!("Walker{number}"),
            entity: number,
            position: Point {
                x: at.x,
                y: at.y,
                z: at.z,
            },
            login: true,
            handoffs: 0,
            bundles_sent: 0,
            look: None,
            backstory,
            items: None,
            inventory: PlayerInventory::default(),
            quests: PlayerQuests::default(),
            progression: PlayerProgression::default(),
            game_master: false,
            account_in_probation: false,
            entitlements: Vec::new(),
        };
        self.admit(&admit, *now_ms, *now_ms)
            .map_err(|error| format!("the character was not admitted: {error}"))?;
        *now_ms += OWN_CARD_DELAY_MS;
        self.walk_ticks(now_ms);
        self.heard_from(connection);
        let top = self.content.progression.max_level();
        self.set_level(number, top);
        Ok(())
    }

    /// The archetype a character of a quest with requirement `requirement` picks: the one whose
    /// unlock the requirement names, else the first one, so that it wears a starter kit.
    fn walk_backstory(&self, requirement: Option<&str>) -> Option<String> {
        let unlocks = &self.content.archetype_unlocks;
        let named = requirement.and_then(|requirement| {
            expression_names(requirement)
                .into_iter()
                .filter(|(_, negated)| !negated)
                .find_map(|(name, _)| {
                    unlocks
                        .iter()
                        .find(|(_, unlock)| unlock.eq_ignore_ascii_case(&name))
                })
        });
        named
            .or_else(|| unlocks.first())
            .map(|(backstory, _)| backstory.clone())
    }

    /// What gives achievement `name` on this level: a quest's end or step, a lore note of the
    /// level, or an archetype pick.
    pub(super) fn walk_achievement_source(&self, level: &QuestGuide, name: &str) -> Option<String> {
        if let Some(source) = level.achievement_sources.get(&name.to_lowercase()) {
            return Some(source.clone());
        }
        let lore = name
            .get(..LORE_PREFIX.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(LORE_PREFIX))
            .and_then(|_| name.get(LORE_PREFIX.len()..));
        if let Some(title) = lore {
            return self
                .content
                .lore_by_title
                .get(&title.to_lowercase())
                .filter(|lore_id| self.content.lore_points.first(**lore_id).is_some())
                .map(|_| format!("the lore note {title} of the level"));
        }
        self.content
            .archetype_unlocks
            .iter()
            .find(|(_, unlock)| unlock.eq_ignore_ascii_case(name))
            .map(|(backstory, _)| format!("the archetype pick {backstory}"))
    }

    /// Runs the world for [`TICKS_PER_ROUND`] ticks from `now_ms` on.
    fn walk_ticks(&mut self, now_ms: &mut u64) {
        for _ in 0..TICKS_PER_ROUND {
            *now_ms += TICK_MS;
            let _ = self.tick(*now_ms, *now_ms);
        }
    }

    /// Gives walk character `number` every achievement that quest requirement `requirement`
    /// names without a negation, as the quests and steps before it would.
    fn walk_meet_requirement(
        &mut self,
        level: &QuestGuide,
        number: u64,
        requirement: Option<&str>,
        walk: &mut QuestWalk,
        now_ms: u64,
    ) {
        let Some(requirement) = requirement else {
            return;
        };
        for (name, negated) in expression_names(requirement) {
            if negated {
                continue;
            }
            match self.walk_achievement_source(level, &name) {
                Some(source) => walk.notes.push(format!(
                    "needs achievement {name}: given, as {source} gives it"
                )),
                None => walk.notes.push(format!(
                    "needs achievement {name}: given, but no quest or step gives it"
                )),
            }
            self.earn_achievement(number, &name, now_ms);
        }
        let met = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Player>(entity))
            .is_some_and(|player| {
                requirement_met(requirement, |id| {
                    player.flow.has_achievement(&self.achievement_rules, id)
                })
            });
        if !met {
            walk.notes
                .push(format!("its requirement {requirement} is still not met"));
        }
    }

    /// Joins walk character `number` to the first faction that `RequiredFaction` cell
    /// `required` takes, with the client's pick.
    fn walk_join_faction(
        &mut self,
        number: u64,
        connection: &ConnectionRef,
        required: &str,
        walk: &mut QuestWalk,
        now_ms: u64,
    ) {
        let Some(faction) = JOINABLE
            .into_iter()
            .find(|faction| faction_requirement_met(Some(required), *faction))
        else {
            walk.notes
                .push(format!("no faction a player may join meets {required}"));
            return;
        };
        let pick = ClientFacetMsg::SetFaction {
            header: header(number),
            faction,
        };
        let _ = self.facet_message(connection, pick, now_ms);
    }

    /// Walk character `number` goes to NPC `npc` when the level has it and talks to it.
    fn walk_talk(
        &mut self,
        level: &QuestGuide,
        number: u64,
        connection: &ConnectionRef,
        npc: &str,
        now_ms: u64,
    ) {
        if let Some(at) = level.npc_at(npc) {
            self.walk_go(number, at, now_ms);
        }
        let request = InteractionRequest {
            target: [0; nw_messages::wire::RAW_ID_LEN],
            target_entity: 0,
            interaction: now_ms,
            kind: TALK_KIND,
            gatherable: None,
        };
        let name = format!("{NPC_MARK}{npc}");
        self.interact(connection, request, name.as_bytes(), now_ms);
    }

    /// Moves walk character `number` to `at`.
    fn walk_go(&mut self, number: u64, at: Position, now_ms: u64) {
        if let Some(entity) = self.player_by_number(number) {
            self.place_player(entity, at, None, Placement::Drop, now_ms);
        }
    }

    /// Whether walk character `number` runs quest `id`.
    fn walk_runs(&self, number: u64, id: &str) -> bool {
        self.quests_of_player(number)
            .iter()
            .any(|quest| quest.def.id.eq_ignore_ascii_case(id))
    }

    /// Whether walk character `number`'s quest `id` has all its tasks done.
    fn walk_finished(&self, number: u64, id: &str) -> bool {
        self.quests_of_player(number)
            .iter()
            .any(|quest| quest.def.id.eq_ignore_ascii_case(id) && quest.run.finished())
    }

    /// What walk character `number`'s quests stand at: the progress of each.
    fn walk_signature(&self, number: u64) -> Vec<(String, Vec<TaskMark>)> {
        self.quests_of_player(number)
            .iter()
            .map(|quest| {
                (
                    quest.def.id.clone(),
                    quest
                        .run
                        .progress()
                        .iter()
                        .map(|task| (task.task, task.progress, task.status as u8))
                        .collect(),
                )
            })
            .collect()
    }

    /// The waiting steps of walk character `number`'s quests.
    fn walk_waiting(&self, number: u64) -> Vec<WaitingStep> {
        self.quests_of_player(number)
            .iter()
            .flat_map(|quest| {
                quest
                    .run
                    .waiting_steps()
                    .into_iter()
                    .map(|(task, kind, progress)| WaitingStep {
                        quest: quest.def.id.clone(),
                        task: task.to_owned(),
                        kind: kind.clone(),
                        progress,
                    })
            })
            .collect()
    }
}

/// The place of record position `at`.
fn position_of(at: [f32; 3]) -> Position {
    Position {
        x: at[0],
        y: at[1],
        z: at[2],
    }
}

/// A step a client plays with its own calls: a cinematic, a cutscene, a screen, a faction pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientStep {
    /// The cinematic video of the id starts.
    CinematicStart(u32),
    /// The cinematic video of the id ends.
    CinematicEnd(u32),
    /// The in-game cutscene of the id plays to its end.
    Cutscene(u32),
    /// The UI screen of the id opens.
    Screen(u32),
    /// The player picks the faction.
    Faction(u8),
}

impl ClientStep {
    /// The calls the client sends for the step, with facet header `header`: a cutscene is asked
    /// for, then ends.
    fn calls(self, header: FacetHeader) -> Vec<ClientFacetMsg> {
        match self {
            Self::CinematicStart(cinematic) => {
                vec![ClientFacetMsg::CinematicStarted { header, cinematic }]
            }
            Self::CinematicEnd(cinematic) => {
                vec![ClientFacetMsg::CinematicEnded { header, cinematic }]
            }
            Self::Cutscene(cutscene) => vec![
                ClientFacetMsg::RequestCutscene { header, cutscene },
                ClientFacetMsg::CutsceneEnded { header },
            ],
            Self::Screen(screen) => vec![ClientFacetMsg::UiScreenOpened { header, screen }],
            Self::Faction(faction) => vec![ClientFacetMsg::SetFaction { header, faction }],
        }
    }

    /// The step's words in a `!questplan` line: its name, then its id in hexadecimal (a
    /// faction's number in decimal).
    fn words(self) -> String {
        match self {
            Self::CinematicStart(id) => format!("{CINEMATIC_START} {id:08x}"),
            Self::CinematicEnd(id) => format!("{CINEMATIC_END} {id:08x}"),
            Self::Cutscene(id) => format!("{CUTSCENE} {id:08x}"),
            Self::Screen(id) => format!("{SCREEN} {id:08x}"),
            Self::Faction(faction) => format!("{FACTION} {faction}"),
        }
    }
}

/// A flow the walk plays as the game event the server makes of it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FlowEvent {
    /// A container was looted.
    ContainerLooted,
    /// The recipe was crafted.
    Crafted(String),
    /// An item of the family was crafted.
    CraftedFamily(String),
    /// The item was equipped.
    Equipped(String),
    /// The game event fired.
    GameEvent(String),
    /// An object of the interact tag was used.
    Interacted(String),
    /// The cyclic gatherable was gathered in the state.
    Cyclic { cyclic: String, state: u32 },
    /// The item was used.
    Used(String),
    /// A step of fishing.
    Fishing(FishingStep),
    /// A fish was caught.
    Caught {
        item: String,
        length: f32,
        weight: f32,
        hotspot: bool,
    },
    /// The item was salvaged.
    Salvaged(String),
    /// The song was performed at the place.
    Song { song: String, at: Position },
    /// The section of the quick course of so many sections was passed.
    Course {
        course: String,
        section: u32,
        sections: u32,
    },
    /// The player died.
    Died,
}

/// What a waiting step needs a player to do next. The plan checks the level (a mob of the type,
/// a source of the item, a recipe, a place, an NPC) and changes nothing. The walk carries it out
/// in its own world ([`SimWorld::walk_act`]); `!questplan` names it to a game master or a test
/// bot ([`SimWorld::plan_line`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StepPlan {
    /// Wait: for a timer, a patrol stay or another quest.
    Wait,
    /// The step cannot be played: how the walk ends, and why.
    Stop(Outcome, String),
    /// Send the client calls of this step.
    Client(ClientStep),
    /// Talk to the NPC, at its place when the level has it.
    Talk { npc: String, at: Option<Position> },
    /// Finish the conversation topic, after a talk with the NPC that holds it, when one does.
    Topic {
        topic: u32,
        npc: Option<(String, Position)>,
    },
    /// Go to the place, entering the territory when one is named.
    Go {
        at: Position,
        territory: Option<u16>,
    },
    /// Leave these territories.
    Leave(Vec<u16>),
    /// Kill so many mobs of the spawn (its index in the spawns the sim runs), in the territory
    /// when one is named; `spawned`: a mob made from the spawn's vitals alone counts (else only
    /// the placed mobs count, by their spawn category).
    Kill {
        spawn: usize,
        count: u32,
        territory: Option<u16>,
        spawned: bool,
    },
    /// Get so many of the item from the source; `gathered`: the step counts gathers; `node`:
    /// the gatherable whose nodes give it, when a gather gives it.
    Item {
        item: String,
        count: u32,
        gathered: bool,
        source: String,
        node: Option<String>,
    },
    /// Earn the achievements, each with what gives it.
    Achievements(Vec<(String, String)>),
    /// Set the progression to the target, a rank or points.
    Progression {
        id: String,
        target: u32,
        points: bool,
    },
    /// Play the flow this many times.
    Event(FlowEvent, u32),
}

/// Where a player gets an item: the source in words, and the gatherable whose nodes give it
/// when a gather gives it.
struct ItemSource {
    said: String,
    node: Option<String>,
}

impl ItemSource {
    const fn said(said: String) -> Self {
        Self { said, node: None }
    }

    fn gathered(said: String, gatherable: &str) -> Self {
        Self {
            said,
            node: Some(gatherable.to_owned()),
        }
    }
}

impl SimWorld {
    /// What waiting step `step` of a quest needs a player to do next, on the level of `level`.
    /// `at` is where the player stands.
    pub(crate) fn walk_plan(
        &self,
        level: &QuestGuide,
        step: &WaitingStep,
        at: Position,
    ) -> StepPlan {
        let content = &self.content;
        let remaining = |count: u32| count.saturating_sub(step.progress).max(1);
        let event = |flow, count| StepPlan::Event(flow, remaining(count));
        let broken = |reason: String| StepPlan::Stop(Outcome::Broken, reason);
        match &step.kind {
            TaskKind::Consecutive
            | TaskKind::Partial
            | TaskKind::Simple
            | TaskKind::Timer { .. } => StepPlan::Wait,
            TaskKind::UnlockAchievement(_)
            | TaskKind::LockAchievement(_)
            | TaskKind::TemporaryUnlock(_)
            | TaskKind::RemoveItem { .. }
            | TaskKind::GiveItem { .. }
            | TaskKind::TriggerGameEvent(_)
            | TaskKind::Empty => broken("a step that ends at its start still waits".to_owned()),
            TaskKind::HasAchievement(expression) => {
                let mut given = Vec::new();
                for (name, negated) in expression_names(expression) {
                    if negated {
                        continue;
                    }
                    match self.walk_achievement_source(level, &name) {
                        Some(source) => given.push((name, source)),
                        None => {
                            return StepPlan::Stop(
                                Outcome::NeedsLook,
                                format!("waits for achievement {name}; no quest or step gives it"),
                            );
                        }
                    }
                }
                StepPlan::Achievements(given)
            }
            TaskKind::CinematicStart(cinematic) => {
                StepPlan::Client(ClientStep::CinematicStart(*cinematic))
            }
            TaskKind::CinematicComplete(cinematic) => {
                StepPlan::Client(ClientStep::CinematicEnd(*cinematic))
            }
            TaskKind::CutsceneComplete(cutscene) => {
                StepPlan::Client(ClientStep::Cutscene(*cutscene))
            }
            TaskKind::OpenUiScreen(screen) => StepPlan::Client(ClientStep::Screen(*screen)),
            TaskKind::ChooseFaction => StepPlan::Client(ClientStep::Faction(SYNDICATE)),
            TaskKind::TriggerArea { area, leave, place } => match (area, place) {
                (Some(area), _) => {
                    let spot = area.spot();
                    StepPlan::Go {
                        at: if *leave {
                            Position {
                                x: spot.x + LEAVE_DISTANCE_M,
                                ..spot
                            }
                        } else {
                            spot
                        },
                        territory: None,
                    }
                }
                (None, Some(place)) => self.walk_to_poi(level, std::slice::from_ref(place)),
                (None, None) => broken(
                    "no place: its trigger volume, its location and its POI are all unknown"
                        .to_owned(),
                ),
            },
            TaskKind::KillContribution {
                enemy,
                count,
                place,
            } if enemy.eq_ignore_ascii_case(PLAYER_ENEMY) => StepPlan::Stop(
                Outcome::NotWalked,
                format!("needs {} kills of other players (PvP)", remaining(*count)),
            ),
            TaskKind::KillContribution {
                enemy,
                count,
                place,
            } => self.walk_kill(level, enemy, remaining(*count), place.as_deref()),
            TaskKind::HaveItem(Some(item)) => self.walk_get_item(item, 1, None, false),
            TaskKind::HaveItem(None) => event(FlowEvent::ContainerLooted, 1),
            TaskKind::HaveItems {
                item,
                count,
                drop,
                chest,
                ..
            } => {
                let from = drop
                    .as_ref()
                    .map(|drop| drop.enemy.clone())
                    .or_else(|| chest.as_ref().map(|_| String::new()));
                self.walk_get_item(item, remaining(*count), from.as_deref(), false)
            }
            TaskKind::GatherItems {
                item, count, drop, ..
            } => {
                let from = drop.as_ref().map(|drop| drop.enemy.clone());
                self.walk_get_item(item, remaining(*count), from.as_deref(), true)
            }
            TaskKind::RequireGather { item, count } => {
                self.walk_get_item(item, remaining(*count), None, true)
            }
            TaskKind::CraftRecipe(recipe) => {
                if content.recipes.get(lowercase_crc32(recipe)).is_none() {
                    return broken(format!("no recipe {recipe}"));
                }
                event(FlowEvent::Crafted(recipe.clone()), 1)
            }
            TaskKind::CraftAnyRecipe { recipes, count } => {
                match recipes
                    .iter()
                    .find(|recipe| content.recipes.get(lowercase_crc32(recipe)).is_some())
                {
                    Some(recipe) => event(FlowEvent::Crafted(recipe.clone()), *count),
                    None => broken(format!(
                        "none of its recipes exists: {}",
                        recipes.join(", ")
                    )),
                }
            }
            TaskKind::Never => broken(
                "waits on a step that nothing ends (a game event task with no event)".to_owned(),
            ),
            TaskKind::EquipItem(item) => event(FlowEvent::Equipped(item.clone()), 1),
            TaskKind::ConversationTopic(topic) => {
                let npcs = level.topic_npcs.get(topic).cloned().unwrap_or_default();
                let placed = npcs
                    .iter()
                    .find_map(|npc| level.npc_at(npc).map(|at| (npc.clone(), at)));
                if placed.is_none() && !npcs.is_empty() {
                    return broken(format!(
                        "the NPC of its topic is not in the open world: {}",
                        npcs.join(", ")
                    ));
                }
                StepPlan::Topic {
                    topic: *topic,
                    npc: placed,
                }
            }
            TaskKind::GoToPoi(tags) => self.walk_to_poi(level, tags),
            TaskKind::ExitPoi(tag) => StepPlan::Leave(
                content
                    .poi_tags
                    .territories_with(tag)
                    .into_iter()
                    .filter_map(|territory| u16::try_from(territory).ok())
                    .collect(),
            ),
            TaskKind::GameEvent {
                event: fired,
                count,
            } => {
                let object = (0..content.quest_objects.len())
                    .filter_map(|index| content.quest_objects.get(index))
                    .any(|object| object.event.eq_ignore_ascii_case(fired));
                let gatherable = content.gatherables.iter().any(|gatherable| {
                    gatherable
                        .game_event
                        .as_deref()
                        .is_some_and(|known| known.eq_ignore_ascii_case(fired))
                        && content
                            .gather_points
                            .first(lowercase_crc32(&gatherable.id))
                            .is_some()
                });
                let fired_by_server = content.recipes.fires_event(fired)
                    || fired.eq_ignore_ascii_case(super::travel::BIND_CAMP_EVENT)
                    || level.success_events.contains(&fired.to_lowercase());
                if object || gatherable || fired_by_server {
                    event(FlowEvent::GameEvent(fired.clone()), *count)
                } else {
                    StepPlan::Stop(
                        Outcome::NeedsLook,
                        format!(
                            "no quest object, gatherable, recipe or quest end of the level fires game event {fired}"
                        ),
                    )
                }
            }
            TaskKind::Interact { tag, count } => {
                let board_turn_in = super::missions::TURN_IN_TAGS
                    .iter()
                    .any(|turn_in| turn_in.eq_ignore_ascii_case(tag));
                if content.interact_tags.knows(tag) || board_turn_in {
                    event(FlowEvent::Interacted(tag.clone()), *count)
                } else {
                    broken(format!("no object of the level has interact tag {tag}"))
                }
            }
            TaskKind::CyclicState {
                cyclic,
                state,
                count,
            } => {
                if content
                    .cyclic_gatherables
                    .of(lowercase_crc32(cyclic))
                    .is_empty()
                {
                    broken(format!("no cyclic gatherable {cyclic}"))
                } else {
                    event(
                        FlowEvent::Cyclic {
                            cyclic: cyclic.clone(),
                            state: *state,
                        },
                        *count,
                    )
                }
            }
            TaskKind::UseItems { item, count } => event(FlowEvent::Used(item.clone()), *count),
            TaskKind::Objective(_) => StepPlan::Wait,
            TaskKind::GoToTerritory { territory, poi } => {
                let Ok(territory) = u16::try_from(*territory) else {
                    return broken(format!("territory {territory} is no territory id"));
                };
                match (poi, level.territory_spots.get(&territory)) {
                    (Some(poi), _) => self.walk_to_poi(level, std::slice::from_ref(poi)),
                    (None, Some(spot)) => StepPlan::Go {
                        at: *spot,
                        territory: Some(territory),
                    },
                    (None, None) => broken(format!(
                        "no place of the level lies in territory {territory}"
                    )),
                }
            }
            TaskKind::ConversationState(npc) => match level.npc_at(npc) {
                Some(at) => StepPlan::Talk {
                    npc: npc.clone(),
                    at: Some(at),
                },
                None => broken(format!("NPC {npc} is not in the open world")),
            },
            TaskKind::HaveLevel {
                progression,
                target,
                points,
            } => match progression {
                None => broken(format!(
                    "waits for character level {target}; the walk character is at the top level"
                )),
                Some(id) if content.progression.categorical(id).is_none() => {
                    broken(format!("no progression {id}"))
                }
                Some(id) => StepPlan::Progression {
                    id: id.clone(),
                    target: *target,
                    points: *points,
                },
            },
            TaskKind::Patrol { place, .. } => {
                match self.walk_to_poi(level, std::slice::from_ref(place)) {
                    // The stay counts while the player is in the place.
                    StepPlan::Go { at: spot, .. }
                        if spot.horizontal_distance(at) <= PATROL_REACH_M =>
                    {
                        StepPlan::Wait
                    }
                    other => other,
                }
            }
            TaskKind::GameMode(mode) | TaskKind::CompleteGameMode(mode) => {
                if content.instances.by_id(mode).is_none() {
                    broken(format!("no game mode {mode}"))
                } else {
                    StepPlan::Stop(
                        Outcome::NotWalked,
                        format!(
                            "needs a run of game mode {mode} (an expedition, a raid or an arena)"
                        ),
                    )
                }
            }
            TaskKind::Fishing {
                step: fishing,
                count,
            } => event(FlowEvent::Fishing(*fishing), *count),
            TaskKind::CatchFish { rule, count } => event(
                FlowEvent::Caught {
                    item: rule.item.clone().unwrap_or_else(|| ANY_FISH.to_owned()),
                    length: rule.min_length.unwrap_or_default() + CATCH_MARGIN,
                    weight: rule.min_weight.unwrap_or_default() + CATCH_MARGIN,
                    hotspot: rule.hotspot,
                },
                *count,
            ),
            TaskKind::Salvage { item, count } => event(FlowEvent::Salvaged(item.clone()), *count),
            TaskKind::CraftFamily { family, count } => {
                event(FlowEvent::CraftedFamily(family.clone()), *count)
            }
            TaskKind::HaveFamily { family, .. } => StepPlan::Stop(
                Outcome::NotWalked,
                format!("holds items of family {family}: the walk does not pick such items"),
            ),
            TaskKind::PerformSong { song, near, count } => {
                if content.music.song(lowercase_crc32(song)).is_none() {
                    return broken(format!("no song {song}"));
                }
                event(
                    FlowEvent::Song {
                        song: song.clone(),
                        at: near.map_or(at, |area| area.spot()),
                    },
                    *count,
                )
            }
            TaskKind::QuickCourse(course) => match content.quick_course_paths.get(course) {
                Some(path) => {
                    let sections = u32::try_from(path.nodes.len()).unwrap_or(u32::MAX);
                    StepPlan::Event(
                        FlowEvent::Course {
                            course: course.clone(),
                            section: 0,
                            sections,
                        },
                        sections,
                    )
                }
                None => broken(format!("no path of quick course {course}")),
            },
            TaskKind::QuickCourseSection { course, section } => {
                match content.quick_course_paths.get(course) {
                    Some(path) => StepPlan::Event(
                        FlowEvent::Course {
                            course: course.clone(),
                            section: *section,
                            sections: u32::try_from(path.nodes.len()).unwrap_or(u32::MAX),
                        },
                        1,
                    ),
                    None => broken(format!("no path of quick course {course}")),
                }
            }
            TaskKind::OwnHouses(houses) => StepPlan::Stop(
                Outcome::NotWalked,
                format!("owns {houses} houses: the walk does not buy houses"),
            ),
            TaskKind::Die { count } => event(FlowEvent::Died, *count),
            TaskKind::NotRun(kind) => broken(format!("the server does not run step kind {kind}")),
        }
    }

    /// A point of interest of one of `tags`: a quest volume of the tag, else a place of a
    /// territory that carries it.
    fn walk_to_poi(&self, level: &QuestGuide, tags: &[String]) -> StepPlan {
        let content = &self.content;
        if let Some(volume) = content
            .quest_volumes
            .iter()
            .find(|volume| tags.iter().any(|tag| volume.tag.eq_ignore_ascii_case(tag)))
        {
            let [x, y, z] = nw_world::collision::transform_point(&volume.place, [0.0; 3]);
            return StepPlan::Go {
                at: Position { x, y, z },
                territory: None,
            };
        }
        tags.iter()
            .find_map(|tag| {
                content
                    .poi_tags
                    .territories_with(tag)
                    .into_iter()
                    .filter_map(|territory| u16::try_from(territory).ok())
                    .find_map(|territory| {
                        level
                            .territory_spots
                            .get(&territory)
                            .map(|spot| StepPlan::Go {
                                at: *spot,
                                territory: Some(territory),
                            })
                    })
            })
            .unwrap_or_else(|| {
                StepPlan::Stop(
                    Outcome::Broken,
                    format!(
                        "no place of the level carries POI tag {}",
                        tags.join(" or ")
                    ),
                )
            })
    }

    /// The vitals id and categories of the spawn that counts as `enemy`, with its spawn
    /// categories, when the kill types know its vitals.
    fn walk_counts(&self, spawn: &VitalsSpawn, enemy: &str) -> bool {
        let content = &self.content;
        content
            .kill_types
            .of(spawn.vitals_id)
            .is_some_and(|(vitals_id, categories)| {
                let spawn_categories = content.kill_types.spawn_categories(Some(spawn.category_id));
                kills_count(enemy, vitals_id, categories, &spawn_categories)
            })
    }

    /// `count` kills of mobs that count as `enemy` (in POI `place` when it names one): a spawn
    /// of such a mob that the open map runs.
    fn walk_kill(
        &self,
        level: &QuestGuide,
        enemy: &str,
        count: u32,
        place: Option<&str>,
    ) -> StepPlan {
        let territories: Option<Vec<u32>> =
            place.map(|place| self.content.poi_tags.territories_with(place));
        let in_place = |spawn: &VitalsSpawn| {
            territories.as_ref().is_none_or(|territories| {
                spawn
                    .territory_ids
                    .iter()
                    .any(|territory| territories.contains(&u32::from(*territory)))
            })
        };
        let counts = |spawn: &VitalsSpawn| self.walk_counts(spawn, enemy) && in_place(spawn);
        match self.combat.spawns.iter().position(counts) {
            Some(index) => {
                let territory = territories.as_ref().and_then(|territories| {
                    self.combat.spawns[index]
                        .territory_ids
                        .iter()
                        .find(|territory| territories.contains(&u32::from(**territory)))
                        .copied()
                });
                let spawned = self
                    .content
                    .kill_types
                    .of(self.combat.spawns[index].vitals_id)
                    .is_some_and(|(vitals_id, categories)| {
                        kills_count(enemy, vitals_id, categories, &[])
                    });
                StepPlan::Kill {
                    spawn: index,
                    count,
                    territory,
                    spawned,
                }
            }
            None => {
                let where_ = place
                    .map(|place| format!(" in POI {place}"))
                    .unwrap_or_default();
                StepPlan::Stop(
                    Outcome::Broken,
                    if level.all_spawns.iter().any(counts) {
                        format!(
                            "mobs that count as {enemy}{where_} spawn only in encounters the open map does not run"
                        )
                    } else {
                        format!("no mob that counts as {enemy} spawns in the open world{where_}")
                    },
                )
            }
        }
    }

    /// `count` of item `item` from a source the level has; `from` names the step's own drop
    /// (`Some("")` for a container); `gathered`: the step counts gathers.
    fn walk_get_item(
        &self,
        item: &str,
        count: u32,
        from: Option<&str>,
        gathered: bool,
    ) -> StepPlan {
        match self.walk_item_source(item, from) {
            Some(ItemSource { said, node }) => StepPlan::Item {
                item: item.to_owned(),
                count,
                gathered,
                source: said,
                node,
            },
            None => StepPlan::Stop(
                Outcome::Broken,
                format!(
                    "no source of {item}: no drop, gatherable, mob loot, recipe, shop or loot table of the level gives it"
                ),
            ),
        }
    }

    /// Where a player gets item `item` on this level; `from` names the step's own drop.
    fn walk_item_source(&self, item: &str, from: Option<&str>) -> Option<ItemSource> {
        let content = &self.content;
        match from {
            Some("") => return Some(ItemSource::said("a container drop".to_owned())),
            Some(enemy)
                if self
                    .combat
                    .spawns
                    .iter()
                    .any(|spawn| self.walk_counts(spawn, enemy)) =>
            {
                return Some(ItemSource::said(format!("the drop of {enemy}")));
            }
            Some(object)
                if content
                    .gather_points
                    .first(lowercase_crc32(object))
                    .is_some() =>
            {
                return Some(ItemSource::gathered(
                    format!("the drop of quest object {object}"),
                    object,
                ));
            }
            _ => {}
        }
        if let Some(gatherable) = content.gatherables.iter().find(|gatherable| {
            gatherable
                .loot_table
                .as_deref()
                .is_some_and(|table| content.loot.table_drops(table, item))
                && content
                    .gather_points
                    .first(lowercase_crc32(&gatherable.id))
                    .is_some()
        }) {
            return Some(ItemSource::gathered(
                format!("gathered from {}", gatherable.id),
                &gatherable.id,
            ));
        }
        if let Some(vitals_id) = self.combat.spawns.iter().find_map(|spawn| {
            let drop = content.mob_loot.of(spawn.vitals_id)?;
            content
                .loot
                .table_drops(&drop.table, item)
                .then(|| content.kill_types.of(spawn.vitals_id).map(|(id, _)| id))
                .flatten()
        }) {
            return Some(ItemSource::said(format!("the loot of {vitals_id}")));
        }
        if let Some(recipe) = content.recipes.making(item) {
            return Some(ItemSource::said(format!("crafted with {}", recipe.id)));
        }
        if content.shops.sells(item) {
            return Some(ItemSource::said("bought at a shop".to_owned()));
        }
        content
            .loot
            .drops_item(item)
            .then(|| ItemSource::said("a loot table holds it".to_owned()))
    }

    /// Carries out `plan` for waiting step `step` of walk character `number` in this world at
    /// `now_ms`.
    fn walk_act(
        &mut self,
        level: &QuestGuide,
        number: u64,
        connection: &ConnectionRef,
        plan: StepPlan,
        now_ms: u64,
    ) -> Play {
        match plan {
            StepPlan::Wait => Play::Waits,
            StepPlan::Stop(Outcome::NeedsLook, reason) => Play::NeedsLook(reason),
            StepPlan::Stop(Outcome::NotWalked, reason) => Play::NotWalked(reason),
            StepPlan::Stop(_, reason) => Play::Broken(reason),
            StepPlan::Client(step) => {
                for call in step.calls(header(number)) {
                    let _ = self.facet_message(connection, call, now_ms);
                }
                Play::Done(None)
            }
            StepPlan::Talk { npc, .. } => {
                self.walk_talk(level, number, connection, &npc, now_ms);
                Play::Done(None)
            }
            StepPlan::Topic { topic, npc } => {
                if let Some((npc, _)) = &npc {
                    self.walk_talk(level, number, connection, npc, now_ms);
                }
                let finished = ConversationClientMsg::FinishedTopics {
                    header: header(number),
                    topics: vec![topic],
                };
                self.conversation_call(connection, finished, now_ms);
                Play::Done(npc.is_none().then(|| "no NPC holds its topic".to_owned()))
            }
            StepPlan::Go { at, territory } => {
                self.walk_go(number, at, now_ms);
                if let Some(territory) = territory {
                    self.walk_territory(number, connection, territory, true, now_ms);
                }
                Play::Done(None)
            }
            StepPlan::Leave(territories) => {
                for territory in territories {
                    self.walk_territory(number, connection, territory, false, now_ms);
                }
                Play::Done(None)
            }
            StepPlan::Kill {
                spawn,
                count,
                territory,
                ..
            } => {
                let spawns = std::sync::Arc::clone(&self.combat.spawns);
                let content = std::sync::Arc::clone(&self.content);
                let spawn = &spawns[spawn];
                self.walk_go(number, position_of(spawn.position), now_ms);
                if let Some(territory) = territory {
                    self.walk_territory(number, connection, territory, true, now_ms);
                    // The territory list feeds the places at the next tick.
                    let _ = self.tick(now_ms, now_ms);
                }
                let Some((vitals_id, categories)) = content.kill_types.of(spawn.vitals_id) else {
                    return Play::Waits;
                };
                let spawn_categories = content.kill_types.spawn_categories(Some(spawn.category_id));
                for _ in 0..count {
                    self.quest_event(
                        number,
                        Event::Killed {
                            vitals_id,
                            categories,
                            spawn_categories: &spawn_categories,
                        },
                        now_ms,
                    );
                }
                Play::Done(Some(format!("killed {vitals_id} at its spawn, by event")))
            }
            StepPlan::Item {
                item,
                count,
                gathered,
                source,
                ..
            } => {
                self.give_loot(number, vec![(item.clone(), count)], now_ms);
                if gathered {
                    self.quest_event(
                        number,
                        Event::Gathered {
                            item: &item,
                            quantity: count,
                        },
                        now_ms,
                    );
                }
                Play::Done(Some(format!("got {item} ({source})")))
            }
            StepPlan::Achievements(given) => {
                for (name, _) in &given {
                    self.earn_achievement(number, name, now_ms);
                }
                Play::Done(Some(format!(
                    "achievements given: {}",
                    given
                        .iter()
                        .map(|(name, source)| format!("{name} (from {source})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
            StepPlan::Progression { id, target, points } => {
                if points {
                    self.give_mastery_experience(number, &id, target);
                } else {
                    self.set_mastery_rank(
                        number,
                        Some(&id),
                        u16::try_from(target).unwrap_or(u16::MAX),
                    );
                }
                Play::Done(Some(format!("progression {id} set to {target}")))
            }
            StepPlan::Event(flow, count) => {
                let note = self.walk_flow(number, &flow, count, now_ms);
                Play::Done(Some(format!("{note}, by event")))
            }
        }
    }

    /// Plays `flow` `count` times for walk character `number` as the game event the server
    /// makes of it, and names what it played.
    fn walk_flow(&mut self, number: u64, flow: &FlowEvent, count: u32, now_ms: u64) -> String {
        if let FlowEvent::Equipped(item) | FlowEvent::Used(item) = flow {
            self.give_loot(number, vec![(item.clone(), count)], now_ms);
        }
        if let FlowEvent::Song { at, .. } = flow {
            self.walk_go(number, *at, now_ms);
        }
        let families = match flow {
            FlowEvent::CraftedFamily(family) => vec![family.clone()],
            _ => Vec::new(),
        };
        for index in 0..count {
            let event = match flow {
                FlowEvent::ContainerLooted => Event::ContainerLooted { items: &[] },
                FlowEvent::Crafted(recipe) => Event::Crafted(recipe),
                FlowEvent::CraftedFamily(_) => Event::CraftedFamilies(&families),
                FlowEvent::Equipped(item) => Event::Equipped(item),
                FlowEvent::GameEvent(fired) => Event::GameEvent(fired),
                FlowEvent::Interacted(tag) => Event::Interacted(tag),
                FlowEvent::Cyclic { cyclic, state } => Event::CyclicGathered {
                    cyclic,
                    state: *state,
                },
                FlowEvent::Used(item) => Event::ItemUsed(item),
                FlowEvent::Fishing(step) => Event::Fishing(*step),
                FlowEvent::Caught {
                    item,
                    length,
                    weight,
                    hotspot,
                } => Event::FishCaught(FishCaught {
                    item,
                    length: *length,
                    weight: *weight,
                    hotspot: *hotspot,
                }),
                FlowEvent::Salvaged(item) => Event::Salvaged { item, quantity: 1 },
                FlowEvent::Song { song, at } => Event::SongPerformed { song, at: *at },
                FlowEvent::Course {
                    course,
                    section,
                    sections,
                } => Event::QuickCourseSection {
                    course,
                    section: section + index,
                    sections: *sections,
                },
                FlowEvent::Died => Event::Died,
            };
            self.quest_event(number, event, now_ms);
        }
        format!("{flow:?}")
    }

    /// The `!questplan` answer of player `number` for its running quest `objective`: one line
    /// per waiting step, with what the player does next ([`Self::plan_line`]); `not running`
    /// when the player does not run it.
    pub(crate) fn quest_plan_lines(&mut self, number: u64, objective: &str) -> Vec<String> {
        let at = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<super::Body>(entity))
            .map(|body| body.motion.position());
        let content = std::sync::Arc::clone(&self.content);
        let steps: Vec<WaitingStep> = self
            .walk_waiting(number)
            .into_iter()
            .filter(|step| step.quest.eq_ignore_ascii_case(objective))
            .collect();
        match at {
            Some(at) if self.walk_runs(number, objective) => steps
                .iter()
                .map(|step| {
                    let plan = self.walk_plan(&content.quest_guide, step, at);
                    self.plan_line(&step.task, &plan)
                })
                .collect(),
            _ => vec![format!("{PLAN_NOT_RUNNING} {objective}")],
        }
    }

    /// The `!questplan` line of `plan` for task `task`: what a player does next, with the
    /// places it goes to.
    pub(crate) fn plan_line(&self, task: &str, plan: &StepPlan) -> String {
        let at = |at: &Position| format!("{:.1} {:.1} {:.1}", at.x, at.y, at.z);
        let territory = |territory: &Option<u16>| {
            territory
                .map(|territory| format!(" territory {territory}"))
                .unwrap_or_default()
        };
        let what = match plan {
            StepPlan::Wait => PLAN_WAIT.to_owned(),
            StepPlan::Stop(outcome, reason) => format!("{PLAN_STOP} {outcome:?}: {reason}"),
            StepPlan::Client(step) => format!("{PLAN_CLIENT} {}", step.words()),
            StepPlan::Talk { npc, at: place } => format!(
                "{PLAN_TALK} {npc}{}",
                place
                    .map(|place| format!(" at {}", at(&place)))
                    .unwrap_or_default()
            ),
            StepPlan::Topic { topic, npc } => format!(
                "{PLAN_TOPIC} {topic:08x}{}",
                npc.as_ref()
                    .map(|(npc, place)| format!(" after {PLAN_TALK} {npc} at {}", at(place)))
                    .unwrap_or_default()
            ),
            StepPlan::Go {
                at: place,
                territory: entered,
            } => format!("{PLAN_GO} {}{}", at(place), territory(entered)),
            StepPlan::Leave(territories) => format!(
                "{PLAN_LEAVE} {}",
                territories
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            StepPlan::Kill {
                spawn,
                count,
                territory: entered,
                spawned,
            } => {
                let spawn = &self.combat.spawns[*spawn];
                let vitals = self.content.kill_types.of(spawn.vitals_id).map_or_else(
                    || format!("{:08x}", spawn.vitals_id),
                    |(id, _)| id.to_owned(),
                );
                format!(
                    "{PLAN_KILL} {vitals} {count} at {}{}{}",
                    at(&position_of(spawn.position)),
                    territory(entered),
                    if *spawned {
                        String::new()
                    } else {
                        format!(" {PLAN_PLACED}")
                    }
                )
            }
            StepPlan::Item {
                item,
                count,
                gathered,
                source,
                node,
            } => format!(
                "{PLAN_ITEM} {item} {count}{}{} from {source}",
                if *gathered { " gathered" } else { "" },
                node.as_ref()
                    .map(|node| format!(" {PLAN_NODE} {node}"))
                    .unwrap_or_default()
            ),
            StepPlan::Achievements(given) => format!(
                "{PLAN_ACHIEVEMENTS} {}",
                given
                    .iter()
                    .map(|(name, source)| format!("{name} (from {source})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            StepPlan::Progression { id, target, points } => format!(
                "{PLAN_PROGRESSION} {id} {target}{}",
                if *points { " points" } else { "" }
            ),
            StepPlan::Event(flow, count) => format!("{PLAN_EVENT} {count} {flow:?}"),
        };
        format!("{PLAN_LINE} {task} {what}")
    }

    /// Walk character `number`'s client tells it entered (or left) territory `territory`.
    fn walk_territory(
        &mut self,
        number: u64,
        connection: &ConnectionRef,
        territory: u16,
        entered: bool,
        now_ms: u64,
    ) {
        let area = [0; nw_messages::wire::RAW_ID_LEN];
        let message = if entered {
            ClientFacetMsg::TerritoryEntered {
                header: header(number),
                area,
                area_entity: 0,
                extra: 0,
                territory,
                unknown: 0,
            }
        } else {
            ClientFacetMsg::TerritoryExited {
                header: header(number),
                area,
                area_entity: 0,
                extra: 0,
                territory,
            }
        };
        let _ = self.facet_message(connection, message, now_ms);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_content::poi_tags::PoiTags;
    use nw_sim_core::test_support::NoGround;

    use super::super::tests::{CELL_M, MANY, MAX_PLAYERS, NOW, settings, slot, table, test_spawn};
    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB};
    use crate::combat::tests::test_combat;
    use crate::login::WorldIds;
    use crate::objectives::{KillTypes, TaskBook, TaskRow};
    use crate::placed_points::PlacedPoints;
    use crate::player_content::{ObjectiveRow, PlayerContent, QuestBook};

    const NPC: &str = "Walk_Npc";
    const CAMP: &str = "walk_camp";
    const CAMP_TERRITORY: u16 = 4_242;
    const GOOD: &str = "walk_good";
    const NO_MOB: &str = "walk_dragon";
    const NO_NPC: &str = "walk_gone";
    const NOT_RUN: &str = "walk_emote";
    const BOARD: &str = "walk_board";
    const PVP: &str = "walk_pvp";
    const ORB: &str = "walk_orb";
    const ORB_OBJECT: &str = "Walk_Orb";
    const DEN: &str = "walk_den";
    const DEN_GROUP: &str = "Walk_DenBoars";

    fn gateway() -> BusId {
        BusId::try_from("gw-walk".to_owned()).expect("id")
    }

    fn at(x: f32) -> Position {
        Position {
            x,
            y: 10.0,
            z: 50.0,
        }
    }

    fn task(id: &str, kind: &str, children: &[&str]) -> TaskRow {
        TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        }
    }

    /// A world with a quest that a player can end (talk, two boar kills, a camp, a turn-in),
    /// one whose mob spawns nowhere, one whose NPC is not on the level, one with a step kind
    /// the server does not run, one whose item drops from a quest object and one whose boars
    /// count only by their spawn group.
    fn walk_world() -> (SimWorld, QuestGuide) {
        let objective = |id: &str, root: &str, turn_in: Option<&str>| ObjectiveRow {
            id: id.to_owned(),
            root: root.to_owned(),
            turn_in: turn_in.map(str::to_owned),
            ..ObjectiveRow::default()
        };
        let quests = QuestBook::build(
            [
                objective(GOOD, "g_root", Some(NPC)),
                objective(NO_MOB, "d_root", None),
                objective(NO_NPC, "n_root", None),
                objective(NOT_RUN, "e_root", None),
                objective(BOARD, "b_root", None),
                objective(PVP, "p_root", None),
                objective(ORB, "o_root", None),
                objective(DEN, "w_root", None),
            ],
            TaskBook::new(vec![
                task(
                    "g_root",
                    "ConsecutiveTaskContainer",
                    &["g_talk", "g_kill", "g_camp"],
                ),
                TaskRow {
                    item_name: Some(NPC.to_owned()),
                    ..task("g_talk", "TaskConversationState", &[])
                },
                TaskRow {
                    kill_enemy_type: Some("Boar".to_owned()),
                    target_qty: Some(2),
                    ..task("g_kill", "TaskKillContribution", &[])
                },
                TaskRow {
                    poi_tag: Some(CAMP.to_owned()),
                    ..task("g_camp", "TaskGoToPOITag", &[])
                },
                task("d_root", "ConsecutiveTaskContainer", &["d_kill"]),
                TaskRow {
                    kill_enemy_type: Some("Dragon".to_owned()),
                    target_qty: Some(1),
                    ..task("d_kill", "TaskKillContribution", &[])
                },
                task("n_root", "ConsecutiveTaskContainer", &["n_talk"]),
                TaskRow {
                    item_name: Some("Gone_Npc".to_owned()),
                    ..task("n_talk", "TaskConversationState", &[])
                },
                task("e_root", "ConsecutiveTaskContainer", &["e_emote"]),
                task("e_emote", "TaskPerformEmote", &[]),
                task("b_root", "ConsecutiveTaskContainer", &["b_return"]),
                TaskRow {
                    interact_tag: Some("ReturnToGiver".to_owned()),
                    ..task("b_return", "TaskInteract", &[])
                },
                task("p_root", "ConsecutiveTaskContainer", &["p_kill"]),
                TaskRow {
                    kill_enemy_type: Some("Player".to_owned()),
                    target_qty: Some(3),
                    ..task("p_kill", "TaskKillContribution", &[])
                },
                task("o_root", "ConsecutiveTaskContainer", &["o_get"]),
                TaskRow {
                    item_name: Some("Walk_OrbItem".to_owned()),
                    item_drop_vc: Some(ORB_OBJECT.to_owned()),
                    ..task("o_get", "TaskGatherAndReturnItems", &[])
                },
                task("w_root", "ConsecutiveTaskContainer", &["w_kill"]),
                TaskRow {
                    kill_enemy_type: Some(DEN_GROUP.to_owned()),
                    target_qty: Some(3),
                    ..task("w_kill", "TaskKillContribution", &[])
                },
            ]),
        );
        let mut combat = test_combat();
        combat.spawns = vec![
            test_spawn(CELL_M / 2.0 + 30.0),
            VitalsSpawn {
                category_id: lowercase_crc32(DEN_GROUP),
                ..test_spawn(CELL_M / 2.0 + 40.0)
            },
        ]
        .into();
        let level = QuestGuide {
            npcs: HashMap::from([(lowercase_crc32(NPC), at(CELL_M / 2.0 + 5.0))]),
            territory_spots: HashMap::from([(CAMP_TERRITORY, at(CELL_M / 2.0 + 60.0))]),
            givers: HashMap::from([(GOOD.to_owned(), vec![NPC.to_owned()])]),
            topic_npcs: HashMap::new(),
            achievement_sources: HashMap::new(),
            success_events: std::collections::HashSet::new(),
            all_spawns: Vec::new(),
            start: at(CELL_M / 2.0),
        };
        let world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                quests,
                kill_types: KillTypes::build([(TEST_MOB.to_owned(), "Boar,Hostile".to_owned())]),
                poi_tags: PoiTags::build([(u32::from(CAMP_TERRITORY), CAMP.to_owned())]),
                gather_points: PlacedPoints::of([(
                    lowercase_crc32(ORB_OBJECT),
                    at(CELL_M / 2.0 + 20.0),
                )]),
                quest_guide: level.clone(),
                ..PlayerContent::default()
            }),
        );
        (world, level)
    }

    #[test]
    fn the_walk_ends_a_quest_a_player_can_end_and_names_the_step_of_each_other_one() {
        let (mut world, level) = walk_world();
        let mut now = NOW;
        let good = world.walk_quest(&level, &gateway(), 1, GOOD, &mut now);
        assert_eq!(good.outcome, Outcome::Completed, "{good:?}");
        assert!(
            good.notes
                .iter()
                .any(|note| note.contains("killed Test_Boar")),
            "the kills went in at the boar's spawn: {good:?}"
        );
        let no_mob = world.walk_quest(&level, &gateway(), 2, NO_MOB, &mut now);
        assert_eq!(no_mob.outcome, Outcome::Broken);
        assert!(
            no_mob.stop.as_deref().is_some_and(
                |stop| stop.contains("d_kill") && stop.contains("no mob that counts as Dragon")
            ),
            "{no_mob:?}"
        );
        let no_npc = world.walk_quest(&level, &gateway(), 3, NO_NPC, &mut now);
        assert_eq!(no_npc.outcome, Outcome::Broken);
        assert!(
            no_npc
                .stop
                .as_deref()
                .is_some_and(|stop| stop.contains("NPC Gone_Npc is not in the open world")),
            "{no_npc:?}"
        );
        assert!(
            no_npc
                .notes
                .iter()
                .any(|note| note.contains("no NPC offers it")),
            "{no_npc:?}"
        );
        let not_run = world.walk_quest(&level, &gateway(), 4, NOT_RUN, &mut now);
        assert_eq!(not_run.outcome, Outcome::Broken);
        assert!(
            not_run
                .stop
                .as_deref()
                .is_some_and(|stop| stop.contains("does not run step kind TaskPerformEmote")),
            "{not_run:?}"
        );
        let board = world.walk_quest(&level, &gateway(), 6, BOARD, &mut now);
        assert_eq!(
            board.outcome,
            Outcome::Completed,
            "the board turn-in: {board:?}"
        );
        let pvp = world.walk_quest(&level, &gateway(), 7, PVP, &mut now);
        assert_eq!(pvp.outcome, Outcome::NotWalked, "{pvp:?}");
        let unknown = world.walk_quest(&level, &gateway(), 5, "no_such_quest", &mut now);
        assert_eq!(unknown.outcome, Outcome::NotStarted);
        assert_eq!(world.players(), 0, "each walk character left");
    }

    #[test]
    fn the_plan_of_each_step_names_what_a_player_does_and_where() {
        let (mut world, _) = walk_world();
        world
            .admit(
                &super::super::tests::admit_at(1, CELL_M / 2.0, true),
                NOW,
                NOW,
            )
            .expect("admit");
        assert_eq!(world.quest_plan_lines(1, GOOD), ["not running walk_good"]);
        world.start_quest(1, GOOD, NOW).expect("start");
        assert_eq!(
            world.quest_plan_lines(1, GOOD),
            ["plan g_talk talk Walk_Npc at 1029.0 10.0 50.0"]
        );
        world.quest_event(1, Event::Talked(NPC), NOW);
        assert_eq!(
            world.quest_plan_lines(1, GOOD),
            ["plan g_kill kill Test_Boar 2 at 1054.0 10.0 50.0"]
        );
        world.start_quest(1, ORB, NOW).expect("start");
        assert_eq!(
            world.quest_plan_lines(1, ORB),
            [
                "plan o_get item Walk_OrbItem 1 gathered node Walk_Orb from the drop of quest object Walk_Orb"
            ],
            "the bot gathers the quest object's nodes"
        );
        world.start_quest(1, DEN, NOW).expect("start");
        assert_eq!(
            world.quest_plan_lines(1, DEN),
            ["plan w_kill kill Test_Boar 3 at 1064.0 10.0 50.0 placed"],
            "a boar made from its vitals is in no spawn group: only the placed ones count"
        );
        assert_eq!(
            ClientStep::Cutscene(0x1a2b_3c4d).words(),
            "cutscene 1a2b3c4d"
        );
        assert_eq!(ClientStep::Faction(SYNDICATE).words(), "faction 1");
    }

    #[test]
    fn a_placed_mob_of_the_spawn_group_counts_and_a_spawned_one_does_not() {
        const LETHAL: u32 = 100_000;
        let (mut world, _) = walk_world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(
                &super::super::tests::admit_at(1, CELL_M / 2.0 + 40.0, true),
                NOW,
                NOW,
            )
            .expect("admit");
        world.start_quest(1, DEN, NOW).expect("start");
        let at = nw_bus::Point {
            x: CELL_M / 2.0 + 40.0,
            y: 10.0,
            z: 50.0,
        };
        let spawned = world.spawn_gm(TEST_MOB, TEST_LEVEL, at, 1).expect("spawn")[0];
        world.apply_hit(spawned, 1, LETHAL, 0, NOW);
        assert_eq!(
            world.quest_plan_lines(1, DEN),
            ["plan w_kill kill Test_Boar 3 at 1064.0 10.0 50.0 placed"],
            "a GM mob is in no spawn group"
        );
        world.apply_hit(crate::mobs::mob_entity(1), 1, LETHAL, 0, NOW);
        assert_eq!(
            world.quest_plan_lines(1, DEN),
            ["plan w_kill kill Test_Boar 2 at 1064.0 10.0 50.0 placed"],
            "the placed boar of the group counts"
        );
    }

    #[test]
    fn an_expression_names_each_achievement_and_its_negation() {
        assert_eq!(
            expression_names("(A && !B) || C"),
            [
                ("A".to_owned(), false),
                ("B".to_owned(), true),
                ("C".to_owned(), false)
            ]
        );
    }
}
