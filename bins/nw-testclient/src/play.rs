// SPDX-License-Identifier: AGPL-3.0-or-later
//! `nw-testclient play`: one bot of a game master account plays quests on the live servers
//! through the calls a game client sends, and reports each quest: completed, broken (the step
//! and why), or not played (a step whose flow the bot does not play yet).
//!
//! For each quest the bot asks the sim where the quest stands (`!queststate`) and what its
//! waiting steps need (`!questplan`, `sim::world::quest_walk`). It takes the quest with the
//! client's accept (3772) and turns it in with the client's turn-in (794); it talks to NPCs with
//! a talk interaction in its input, finishes conversation topics (5397), tells the server the
//! territories it enters and leaves (2297, 6416) and sends the cinematic, cutscene, screen and
//! faction calls a client sends. It kills a step's mobs with attack inputs: the placed mobs at
//! the step's spawn when only those count (by their spawn group), else mobs of the step's type
//! that a game master spawns there. It gathers the nodes of a quest object or a gatherable that
//! gives a step's item, with a client's gather interaction. Game master lines move it (`!tp`),
//! keep it alive (`!god on`), and give it the items no gather gives (`!give`), with the source
//! the sim names.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use nw_carrier::MESSAGE_CHANNEL;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::client_input::TALK_KIND;
use nw_messages::social::SocialFacet;
use nw_messages::social::chat::{ChatCall, ChatChannel};
use nw_messages::{
    Bit8, ClientFacetMsg, ClientInputMsg, ConversationClientMsg, FacetHeader, InputPosition,
    InteractionRequest,
};
use nw_reflect::LoginToken;
use nw_sim_core::{Position, yaw_toward};
use reqwest::Client;

use crate::activity::{Act, Activity, ActivityKind, Proof, Step, gather_steps};
use crate::chat::{chat_lines, is_system};
use crate::gather::GatherNode;
use crate::hunt::{planar, send_input};
use crate::pair::{PairError, session_with};
use crate::rep::{RepClient, RepError};
use crate::udp::RepOutcome;
use crate::view::{CombatView, InteractionAnswer, WorldView};
use crate::walk::{WalkSettings, login_token, walk};

/// The game master lines after the tutorial's exit, before the first quest: the bot does not
/// die, lives again when a mob killed it before (a dead player wakes no mob), and is at a high
/// level.
const SETUP_LINES: [&str; 3] = ["!god on", "!heal", "!level 60"];
/// The byte of the client's leave of the tutorial (0 in the capture).
const LEAVE_FLAG: u8 = 0;
/// Time a new character takes from the tutorial's leave into the open world (the leave, the
/// level load and the own card there).
const LEVEL_CHANGE_WAIT: Duration = Duration::from_secs(25);
/// Time from the own entity's first state to the first line: the client is in the world by then.
const FIRST_LINE_DELAY: Duration = Duration::from_secs(2);
/// Longest wait for the first line of an answer to a game master line.
const ANSWER_WAIT: Duration = Duration::from_secs(10);
/// Time after the first line of an answer in which its other lines come.
const ANSWER_SETTLE: Duration = Duration::from_millis(400);
/// Time after an action before the bot asks again where the quest stands.
const ACTION_PAUSE: Duration = Duration::from_millis(1500);
/// Time a step that waits (a timer, a patrol stay) gets before the bot asks again.
const WAIT_PAUSE: Duration = Duration::from_secs(5);
/// Longest fight for one kill step.
const FIGHT_TIME: Duration = Duration::from_secs(45);
/// Time between two swings.
const SWING_INTERVAL: Duration = Duration::from_millis(600);
/// The bot swings at a mob this near, metres.
const REACH_M: f32 = 2.5;
/// Walking speed toward a mob, metres per second.
const SPEED_MPS: f32 = 4.0;
/// Time between two moves toward a mob.
const MOVE_INTERVAL: Duration = Duration::from_millis(100);
/// The level of the mobs the bot spawns for a kill step.
const SPAWN_LEVEL: u32 = 1;
/// Most rounds of one quest: a quest still running after them stops as broken.
const MAX_ROUNDS: u32 = 80;
/// The same plan line this many times in a row stops the quest as broken.
const MAX_SAME_PLAN: u32 = 3;
/// Most fights of one kill step while its hits land: a placed mob of a higher level takes
/// some 200 hits of a starter weapon (live 2026-11-06: 8776 health, 42 a hit).
const MAX_KILL_FIGHTS: u32 = 10;
/// The input records of one attack: the press and the release.
const ATTACK_RECORDS: u16 = 2;
/// The counter of the bot's first input record.
const FIRST_INPUT_COUNTER: u16 = 1;
/// A bot looks level.
const LEVEL_PITCH: f32 = 0.0;
/// The facet component of the bot's calls (Inferred: the sim reads the body only).
const CALL_COMPONENT: u64 = 0;
/// Time between two acts of an activity step: a teleport lands before the next act.
const ACT_GAP: Duration = Duration::from_millis(1500);
/// How near its teleport's place the server's own state shows the bot once it landed, metres.
const TELEPORT_SETTLED_M: f32 = 3.0;
/// The status of a taken interaction (3743).
const ACCEPTED: u8 = 1;
/// The name mark before an NPC id in a talk (`@12_JeongIseul`).
const NPC_MARK: &str = "@";

/// The words of the sim's `!queststate` and `!questplan` lines (`sim::world::gm_chat`,
/// `sim::world::quest_walk`).
const STATE_RUNNING: &str = "running ";
const STATE_FINISHED: &str = "finished ";
const STATE_DONE: &str = "done ";
const STATE_NOT_TAKEN: &str = "not taken ";
const PLAN_LINE: &str = "plan ";
const PLAN_NOT_RUNNING: &str = "not running ";
const AT: &str = "at";
const AFTER: &str = "after";
const TERRITORY: &str = "territory";
const FROM: &str = "from";
const POINTS: &str = "points";
/// The word of a kill plan whose mobs count only as placed: a spawned mob is in no spawn group.
const PLACED: &str = "placed";
/// The word of an item plan before the gatherable whose nodes give the item.
const NODE: &str = "node";
const STOP_BROKEN: &str = "Broken:";
/// The marks before the quest that gives an achievement in an `achievements` plan line.
const QUEST_SOURCE_MARKS: [&str; 2] = ["a step of quest ", "the end of quest "];
/// The mark of a `not taken` line whose quest no NPC offers yet.
const NOT_OFFERED_MARK: &str = ": its ";

/// The bot's account, its quests and its timers.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaySettings {
    /// The bot; its account must have the game master rank.
    pub account: WalkSettings,
    /// The quests to play, in order (a chain in its order).
    pub quests: Vec<String>,
    /// Longest time in the world.
    pub hold: Duration,
    /// Longest wait for a registration response.
    pub registration_timeout: Duration,
}

/// How the play of one quest ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It ran from the accept to the turn-in.
    Completed,
    /// A step did not end on the server: the step and why.
    Broken(String),
    /// A step needs a flow the bot does not play: the step and the flow.
    NotPlayed(String),
}

/// The play of one quest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestPlay {
    /// The quest id.
    pub quest: String,
    /// How it ended.
    pub verdict: Verdict,
    /// What the bot did, in order.
    pub actions: Vec<String>,
}

/// What the play run saw.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayReport {
    /// What happened on the connection.
    pub outcome: RepOutcome,
    /// Each quest the bot played, in order.
    pub quests: Vec<QuestPlay>,
    /// Each activity the bot played, in order (named as the activity).
    pub activities: Vec<QuestPlay>,
}

impl PlayReport {
    /// The report as Markdown: a count of each verdict, then each quest with its stop and what
    /// the bot did.
    pub fn markdown(&self) -> String {
        let count = |wanted: fn(&Verdict) -> bool| {
            self.quests
                .iter()
                .filter(|quest| wanted(&quest.verdict))
                .count()
        };
        let mut text = String::new();
        let _ = writeln!(text, "# Quest play of the test bot\n");
        let _ = writeln!(
            text,
            "One bot of a game master account played each quest on the live servers through the \
             client's calls: the accept, talks, topics, territory entries, cinematics, gathers, \
             attacks and the turn-in. It fought the placed mobs when only those count. Game \
             master lines moved it, kept it alive, spawned a step's other mobs where the step's \
             spawn is and gave it the items no gather gives.\n"
        );
        let _ = writeln!(
            text,
            "| Verdict | Quests |\n|---|---|\n| Completed | {} |\n| Broken | {} |\n| Not played | {} |\n| Total | {} |\n",
            count(|verdict| *verdict == Verdict::Completed),
            count(|verdict| matches!(verdict, Verdict::Broken(_))),
            count(|verdict| matches!(verdict, Verdict::NotPlayed(_))),
            self.quests.len()
        );
        let _ = writeln!(
            text,
            "| Quest | Verdict | Stop | What the bot did |\n|---|---|---|---|"
        );
        for quest in &self.quests {
            let (verdict, stop) = match &quest.verdict {
                Verdict::Completed => ("Completed", ""),
                Verdict::Broken(stop) => ("Broken", stop.as_str()),
                Verdict::NotPlayed(stop) => ("Not played", stop.as_str()),
            };
            let _ = writeln!(
                text,
                "| {} | {verdict} | {} | {} |",
                cell(&quest.quest),
                cell(stop),
                cell(&quest.actions.join("; "))
            );
        }
        if !self.activities.is_empty() {
            let _ = writeln!(
                text,
                "\n## Activities\n\nEach activity's steps sent the client's calls and waited for the server's proof: an answer, a message of a type, or the answer to an interaction.\n\n| Activity | Verdict | Stop | Steps proved |\n|---|---|---|---|"
            );
            for activity in &self.activities {
                let (verdict, stop) = match &activity.verdict {
                    Verdict::Completed => ("Completed", ""),
                    Verdict::Broken(stop) => ("Broken", stop.as_str()),
                    Verdict::NotPlayed(stop) => ("Not played", stop.as_str()),
                };
                let _ = writeln!(
                    text,
                    "| {} | {verdict} | {} | {} |",
                    cell(&activity.quest),
                    cell(stop),
                    cell(&activity.actions.join("; "))
                );
            }
        }
        text
    }
}

/// A Markdown table cell of `text`.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// One step of a `!questplan` line: what the bot does next.
#[derive(Debug, Clone, PartialEq)]
enum PlanStep {
    Wait,
    Broken(String),
    NotPlayed(String),
    Client(Vec<ClientFacetMsg>),
    Talk {
        npc: String,
        at: Option<Position>,
    },
    Topic {
        topic: u32,
        talk: Option<(String, Position)>,
    },
    Go {
        at: Position,
        territory: Option<u16>,
    },
    Leave(Vec<u16>),
    Kill {
        vitals: String,
        count: u32,
        at: Position,
        territory: Option<u16>,
        placed: bool,
    },
    Item {
        item: String,
        count: u32,
        source: String,
        node: Option<String>,
    },
    Progression {
        id: String,
        target: u32,
        points: bool,
    },
    /// The step waits for achievements; `sources` are the quests that give them.
    Achievements {
        needs: String,
        sources: Vec<String>,
    },
}

/// The place in words `x y z`.
fn place_of(words: &[&str]) -> Option<Position> {
    match words {
        [x, y, z, ..] => Some(Position {
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            z: z.parse().ok()?,
        }),
        _ => None,
    }
}

/// The territory after the word [`TERRITORY`] in `words`.
fn territory_of(words: &[&str]) -> Option<u16> {
    words
        .iter()
        .position(|word| *word == TERRITORY)
        .and_then(|index| words.get(index + 1)?.parse().ok())
}

/// The place after the word [`AT`] in `words`.
fn place_after_at(words: &[&str]) -> Option<Position> {
    let index = words.iter().position(|word| *word == AT)?;
    place_of(&words[index + 1..])
}

/// A hexadecimal id word.
fn hex_of(word: &str) -> Option<u32> {
    u32::from_str_radix(word, 16).ok()
}

/// The client calls of client step words `words` (`cinematic-start 1a2b3c4d`), with facet header
/// `header`.
fn client_calls(words: &[&str], header: FacetHeader) -> Option<Vec<ClientFacetMsg>> {
    let id = || words.get(1).and_then(|word| hex_of(word));
    Some(match *words.first()? {
        "cinematic-start" => vec![ClientFacetMsg::CinematicStarted {
            header,
            cinematic: id()?,
        }],
        "cinematic-end" => vec![ClientFacetMsg::CinematicEnded {
            header,
            cinematic: id()?,
        }],
        "cutscene" => vec![
            ClientFacetMsg::RequestCutscene {
                header,
                cutscene: id()?,
            },
            ClientFacetMsg::CutsceneEnded { header },
        ],
        "screen" => vec![ClientFacetMsg::UiScreenOpened {
            header,
            screen: id()?,
        }],
        "faction" => vec![ClientFacetMsg::SetFaction {
            header,
            faction: words.get(1)?.parse().ok()?,
        }],
        _ => return None,
    })
}

/// The quests that `text` names as the givers of achievements (`a step of quest X`, `the end of
/// quest Y`), in order.
fn quest_sources(text: &str) -> Vec<String> {
    QUEST_SOURCE_MARKS
        .iter()
        .flat_map(|mark| text.split(mark).skip(1))
        .filter_map(|rest| rest.split([')', ',', ' ']).next())
        .filter(|quest| !quest.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The step of plan line `line` (`plan <task> <what>`), with the bot's facet header `header`:
/// `None` for a line that is no plan line or does not read.
fn plan_step(line: &str, header: FacetHeader) -> Option<(String, PlanStep)> {
    let rest = line.strip_prefix(PLAN_LINE)?;
    let (task, what) = rest.split_once(' ')?;
    let words: Vec<&str> = what.split_whitespace().collect();
    let step = match *words.first()? {
        "wait" => PlanStep::Wait,
        "stop" => {
            let reason = what.trim_start_matches("stop").trim().to_owned();
            if reason.starts_with(STOP_BROKEN) {
                PlanStep::Broken(reason)
            } else {
                PlanStep::NotPlayed(reason)
            }
        }
        "client" => PlanStep::Client(client_calls(&words[1..], header)?),
        "talk" => PlanStep::Talk {
            npc: (*words.get(1)?).to_owned(),
            at: place_after_at(&words),
        },
        "topic" => PlanStep::Topic {
            topic: hex_of(words.get(1)?)?,
            talk: words
                .iter()
                .position(|word| *word == AFTER)
                .and_then(|index| {
                    let npc = (*words.get(index + 2)?).to_owned();
                    Some((npc, place_after_at(&words[index..])?))
                }),
        },
        "go" => PlanStep::Go {
            at: place_of(&words[1..])?,
            territory: territory_of(&words),
        },
        "leave" => PlanStep::Leave(
            words[1..]
                .iter()
                .filter_map(|word| word.parse().ok())
                .collect(),
        ),
        "kill" => PlanStep::Kill {
            vitals: (*words.get(1)?).to_owned(),
            count: words.get(2)?.parse().ok()?,
            at: place_after_at(&words)?,
            territory: territory_of(&words),
            placed: words.contains(&PLACED),
        },
        "item" => {
            let (said, source) = what
                .split_once(&format!(" {FROM} "))
                .unwrap_or((what, ""));
            let said: Vec<&str> = said.split_whitespace().collect();
            PlanStep::Item {
                item: (*words.get(1)?).to_owned(),
                count: words.get(2)?.parse().ok()?,
                source: source.to_owned(),
                node: said
                    .iter()
                    .position(|word| *word == NODE)
                    .and_then(|index| said.get(index + 1))
                    .map(|node| (*node).to_owned()),
            }
        }
        "progression" => PlanStep::Progression {
            id: (*words.get(1)?).to_owned(),
            target: words.get(2)?.parse().ok()?,
            points: words.contains(&POINTS),
        },
        "achievements" => {
            let needs = what.trim_start_matches("achievements").trim().to_owned();
            PlanStep::Achievements {
                sources: quest_sources(&needs),
                needs,
            }
        }
        "event" => PlanStep::NotPlayed(format!(
            "needs a flow the bot does not play yet: {}",
            what.trim_start_matches("event").trim()
        )),
        _ => return None,
    };
    Some((task.to_owned(), step))
}

/// What the bot waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    State,
    Plan,
}

/// Where the bot is in its run.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// It leaves the tutorial with the client's `ReturnToMainWorld`, which it sends after the tutorial's last video: a new
    /// character starts in the tutorial; the open world takes no such call.
    LeaveTutorial,
    /// It waits until the time for the open world.
    Leaving { until: Duration },
    /// It sends the setup line of this index.
    Setup(usize),
    /// It asked; the answer's lines start after this many system lines.
    Asked {
        ask: Ask,
        sent: Duration,
        lines_before: usize,
        first_line: Option<Duration>,
    },
    /// It waits until the time, then asks.
    Pause { until: Duration, then: Ask },
    /// It fights the nearest mob until the time.
    Fight { until: Duration },
    /// It gathers a node for a step's item ([`Player::gather`]).
    Gather,
    /// It plays the activities.
    Activities,
    /// Every quest and activity was played.
    Done,
}

/// Steps the bot plays one act at a time: an activity, or the gather of a quest step's node.
struct RunningSteps {
    steps: Vec<Step>,
    /// The step it is at, and the step's next act.
    step: usize,
    act: usize,
    /// When the last act went out.
    last_act: Option<Duration>,
    /// What came before the step's first act: system lines, messages, interaction answers.
    lines_before: usize,
    messages_before: usize,
    answers_before: usize,
    /// The id of the step's interaction.
    interaction: Option<u64>,
    /// The steps proved so far.
    proved: Vec<String>,
}

impl RunningSteps {
    const fn of(steps: Vec<Step>) -> Self {
        Self {
            steps,
            step: 0,
            act: 0,
            last_act: None,
            lines_before: 0,
            messages_before: 0,
            answers_before: 0,
            interaction: None,
            proved: Vec::new(),
        }
    }
}

/// The node of `nodes` of gatherable `gatherable` nearest to `from` (planar) that is not in
/// `tried`, by its index.
fn nearest_untried(
    nodes: &[GatherNode],
    tried: &HashSet<usize>,
    gatherable: u32,
    from: [f32; 3],
) -> Option<usize> {
    nodes
        .iter()
        .enumerate()
        .filter(|(index, node)| node.gatherable == gatherable && !tried.contains(index))
        .min_by(|(_, a), (_, b)| {
            planar(from, [a.at[0], a.at[1]]).total_cmp(&planar(from, [b.at[0], b.at[1]]))
        })
        .map(|(index, _)| index)
}

/// The bot during its hold.
struct Player {
    world: WorldView,
    combat: CombatView,
    read: usize,
    own_since: Option<Duration>,
    lines: Vec<String>,
    phase: Phase,
    /// The quests still to play, in order.
    queue: std::collections::VecDeque<String>,
    /// The quest the bot plays now; `None` when every quest was played.
    current: Option<String>,
    /// The quests no NPC offered yet, with why: each is tried again after a quest that
    /// completed.
    deferred: Vec<(String, String)>,
    /// Whether a quest completed since the last quest was deferred.
    completed_since_deferral: bool,
    /// The quests played or being played, lowercase.
    played: std::collections::HashSet<String>,
    accepted: bool,
    turned_in: bool,
    rounds: u32,
    last_plan: Option<String>,
    same_plan: u32,
    actions: Vec<String>,
    plays: Vec<QuestPlay>,
    last_swing: Option<Duration>,
    last_move: Option<Duration>,
    input_counter: u16,
    interaction: u64,
    /// The activities still to play, and the one played now.
    activities: std::collections::VecDeque<Activity>,
    running: Option<(ActivityKind, RunningSteps)>,
    /// Every gather node of the level, in world file order.
    nodes: Arc<[GatherNode]>,
    /// The nodes the bot gathered or tried to gather.
    tried_nodes: HashSet<usize>,
    /// The gather of a quest step's node the bot plays now, with what it is for.
    gather: Option<(String, RunningSteps)>,
    activity_plays: Vec<QuestPlay>,
    /// Every interaction answer so far.
    answers: Vec<InteractionAnswer>,
    /// The place of the last teleport, or of the bot's last own move.
    teleported_to: Option<Position>,
    /// The fight the bot fights now.
    fight: Option<Fight>,
    /// The fights for the current kill step.
    fought: Option<Fought>,
}

/// What the bot did in a fight for a kill step.
struct Fight {
    task: String,
    /// Whether it saw a mob at any time of the fight.
    saw_mobs: bool,
    swings: u32,
    /// The view's hits dealt and mob deaths when the fight started.
    hits_before: u64,
    mob_deaths_before: u64,
}

/// The fights for one kill step so far: the hits the bot landed and the mobs that died in view.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fought {
    task: String,
    hits: u64,
    mob_deaths: u64,
    fights: u32,
    /// The hits of the last fight.
    last_hits: u64,
}

impl Fought {
    /// Whether the bot fights on for kill step `task` though its plan did not change: its last
    /// fight landed hits (the mob loses health), within [`MAX_KILL_FIGHTS`].
    fn fights_on(fought: Option<&Self>, task: &str) -> bool {
        fought.is_some_and(|fought| {
            fought.task == task && fought.last_hits > 0 && fought.fights < MAX_KILL_FIGHTS
        })
    }

    /// The verdict of kill step `task` that did not end after its fights: a step whose fights
    /// killed no mob says nothing of its quest (the bot's hits did not kill), else the kills did
    /// not count and the quest is broken.
    fn verdict(fought: Option<&Self>, task: &str, stop: String) -> Verdict {
        match fought.filter(|fought| fought.task == task) {
            Some(fought) if fought.mob_deaths == 0 => Verdict::NotPlayed(format!(
                "{task}: the bot landed {} hits and no mob died ({stop})",
                fought.hits
            )),
            Some(fought) => Verdict::Broken(format!(
                "{stop}; {} mobs died near the bot",
                fought.mob_deaths
            )),
            None => Verdict::Broken(stop),
        }
    }
}

impl Fight {
    /// The verdict of a fight that says nothing of its quest: the bot saw no mob to fight.
    fn not_played(&self) -> Option<Verdict> {
        (!self.saw_mobs).then(|| {
            Verdict::NotPlayed(format!(
                "{}: the bot saw no mob there in {} s",
                self.task,
                FIGHT_TIME.as_secs()
            ))
        })
    }
}

impl Player {
    fn tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        since: Duration,
    ) -> Result<(), RepError> {
        for received in &outcome.messages[self.read..] {
            // A message that does not decode fails nothing here: the plays read the answers.
            let _ = self.world.read(received);
            let _ = self.combat.read(received);
        }
        self.lines.extend(
            chat_lines(&outcome.messages[self.read..])
                .into_iter()
                .filter(is_system)
                .map(|line| line.body),
        );
        self.read = outcome.messages.len();
        self.answers.extend(self.combat.take_interactions());
        let Some(own) = self.world.own_number() else {
            return Ok(());
        };
        let first = *self.own_since.get_or_insert(since);
        if since.saturating_sub(first) < FIRST_LINE_DELAY {
            return Ok(());
        }
        let header = FacetHeader::of(CALL_COMPONENT, own);
        match self.phase.clone() {
            Phase::Done => Ok(()),
            Phase::Activities => self.activity_tick(client, outcome, own, since),
            Phase::LeaveTutorial => {
                // The first time sync marks the spawn (the sim takes no leave before it).
                Self::call(client, &ClientFacetMsg::TimeSyncRequest { header })?;
                Self::call(
                    client,
                    &ClientFacetMsg::ReturnToMainWorld {
                        header,
                        flag: LEAVE_FLAG,
                    },
                )?;
                self.phase = Phase::Leaving {
                    until: since + LEVEL_CHANGE_WAIT,
                };
                Ok(())
            }
            Phase::Leaving { until } if since >= until => {
                self.phase = Phase::Setup(0);
                Ok(())
            }
            Phase::Leaving { .. } => Ok(()),
            Phase::Setup(index) => match SETUP_LINES.get(index) {
                Some(line) => {
                    self.say(client, own, line)?;
                    self.phase = Phase::Setup(index + 1);
                    Ok(())
                }
                None => self.ask(client, own, Ask::State, since),
            },
            Phase::Pause { until, then } if since >= until => self.ask(client, own, then, since),
            Phase::Pause { .. } => Ok(()),
            Phase::Fight { until } if since >= until => self.fight_ended(client, own, since),
            Phase::Fight { .. } => self.fight(client, since),
            Phase::Gather => self.gather_tick(client, outcome, own, since),
            Phase::Asked {
                ask,
                sent,
                lines_before,
                first_line,
            } => {
                let arrived = self.lines.len() > lines_before;
                match (arrived, first_line) {
                    (false, _) if since.saturating_sub(sent) > ANSWER_WAIT => {
                        self.end_quest(Verdict::Broken(format!(
                            "no answer from the server to {}",
                            self.ask_line(ask)
                        )));
                        self.next_quest(client, own, since)
                    }
                    (false, _) => Ok(()),
                    (true, None) => {
                        self.phase = Phase::Asked {
                            ask,
                            sent,
                            lines_before,
                            first_line: Some(since),
                        };
                        Ok(())
                    }
                    (true, Some(first)) if since.saturating_sub(first) >= ANSWER_SETTLE => {
                        let answer = self.lines[lines_before..].to_vec();
                        match ask {
                            Ask::State => self.on_state(client, own, header, &answer, since),
                            Ask::Plan => self.on_plan(client, own, header, &answer, since),
                        }
                    }
                    (true, Some(_)) => Ok(()),
                }
            }
        }
    }

    /// The quest the bot plays now.
    fn current(&self) -> &str {
        self.current.as_deref().unwrap_or_default()
    }

    /// The game master line of `ask` for the current quest.
    fn ask_line(&self, ask: Ask) -> String {
        match ask {
            Ask::State => format!("!queststate {}", self.current()),
            Ask::Plan => format!("!questplan {}", self.current()),
        }
    }

    fn ask(
        &mut self,
        client: &mut RepClient,
        own: u64,
        ask: Ask,
        since: Duration,
    ) -> Result<(), RepError> {
        if self.current.is_none() {
            self.phase = if self.activities.is_empty() {
                Phase::Done
            } else {
                Phase::Activities
            };
            return Ok(());
        }
        let line = self.ask_line(ask);
        self.say(client, own, &line)?;
        self.phase = Phase::Asked {
            ask,
            sent: since,
            lines_before: self.lines.len(),
            first_line: None,
        };
        Ok(())
    }

    /// Says `line` in area chat.
    fn say(&mut self, client: &mut RepClient, own: u64, line: &str) -> Result<(), RepError> {
        let call = ChatCall::Send {
            channel: ChatChannel::Area.wire(),
            body: line.to_owned(),
            recipient_name: String::new(),
            links: Vec::new(),
        };
        let body = call.encode(SocialFacet::Chat.header(own))?;
        client.send_message(MESSAGE_CHANNEL, true, call.type_index(), body)
    }

    /// Sends client call `call`.
    fn call(client: &mut RepClient, call: &ClientFacetMsg) -> Result<(), RepError> {
        client.send_message(MESSAGE_CHANNEL, true, call.type_index(), call.encode()?)
    }

    fn pause(&mut self, since: Duration, wait: Duration, then: Ask) {
        self.phase = Phase::Pause {
            until: since + wait,
            then,
        };
    }

    fn on_state(
        &mut self,
        client: &mut RepClient,
        own: u64,
        header: FacetHeader,
        answer: &[String],
        since: Duration,
    ) -> Result<(), RepError> {
        let objective = lowercase_crc32(&self.current().to_lowercase());
        // Answers to earlier lines (a setup line's `Done.`) may come in the same batch.
        let line = answer
            .iter()
            .map(String::as_str)
            .find(|line| {
                [STATE_RUNNING, STATE_FINISHED, STATE_DONE, STATE_NOT_TAKEN]
                    .iter()
                    .any(|start| line.starts_with(start))
            })
            .unwrap_or_default();
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            self.end_quest(Verdict::Broken(format!(
                "still running after {MAX_ROUNDS} rounds"
            )));
            return self.next_quest(client, own, since);
        }
        if line.starts_with(STATE_DONE) {
            // A quest taken in an earlier run that this run turned in ended in this run too.
            self.end_quest(if self.accepted || self.turned_in {
                Verdict::Completed
            } else {
                Verdict::NotPlayed("the character had done it before this run".to_owned())
            });
            return self.next_quest(client, own, since);
        }
        let first: Vec<String> = quest_sources(line)
            .into_iter()
            .filter(|quest| !self.played.contains(&quest.to_lowercase()))
            .collect();
        if line.starts_with(STATE_NOT_TAKEN) && !first.is_empty() && !self.accepted {
            // The quests that give the missing achievements go first; this one comes after.
            self.actions.clear();
            let current = self.current().to_owned();
            self.queue.push_front(current);
            for quest in first.into_iter().rev() {
                self.queue.push_front(quest);
            }
            self.current = None;
            return self.advance(client, own, since);
        }
        if line.starts_with(STATE_NOT_TAKEN) && line.contains(NOT_OFFERED_MARK) && !self.accepted {
            self.deferred
                .push((self.current().to_owned(), line.to_owned()));
            self.completed_since_deferral = false;
            self.actions.clear();
            return self.advance(client, own, since);
        }
        if line.starts_with(STATE_NOT_TAKEN) {
            if self.accepted {
                self.end_quest(Verdict::Broken(format!(
                    "the accept did not start it ({line})"
                )));
                return self.next_quest(client, own, since);
            }
            Self::call(
                client,
                &ClientFacetMsg::AcceptObjective { header, objective },
            )?;
            self.accepted = true;
            self.actions.push("accepted".to_owned());
            self.pause(since, ACTION_PAUSE, Ask::State);
            return Ok(());
        }
        if line.starts_with(STATE_FINISHED) {
            if self.turned_in {
                self.end_quest(Verdict::Broken(
                    "its tasks are done, but the turn-in does not end it".to_owned(),
                ));
                return self.next_quest(client, own, since);
            }
            Self::call(
                client,
                &ClientFacetMsg::CompleteObjective { header, objective },
            )?;
            self.turned_in = true;
            self.actions.push("turned in".to_owned());
            self.pause(since, ACTION_PAUSE, Ask::State);
            return Ok(());
        }
        if line.starts_with(STATE_RUNNING) {
            return self.ask(client, own, Ask::Plan, since);
        }
        self.end_quest(Verdict::Broken(format!(
            "an answer the bot does not read: {line}"
        )));
        self.next_quest(client, own, since)
    }

    fn on_plan(
        &mut self,
        client: &mut RepClient,
        own: u64,
        header: FacetHeader,
        answer: &[String],
        since: Duration,
    ) -> Result<(), RepError> {
        if answer.iter().any(|line| line.starts_with(PLAN_NOT_RUNNING)) {
            return self.ask(client, own, Ask::State, since);
        }
        let steps: Vec<(String, PlanStep)> = answer
            .iter()
            .filter_map(|line| plan_step(line, header))
            .collect();
        let Some((task, step)) = steps
            .iter()
            .find(|(_, step)| !matches!(step, PlanStep::Wait))
            .or_else(|| steps.first())
            .cloned()
        else {
            self.end_quest(Verdict::Broken(format!(
                "a plan the bot does not read: {}",
                answer.join(" / ")
            )));
            return self.next_quest(client, own, since);
        };
        let line = answer
            .iter()
            .find(|line| line.starts_with(&format!("{PLAN_LINE}{task} ")))
            .cloned()
            .unwrap_or_default();
        let fights_on = matches!(step, PlanStep::Kill { .. })
            && Fought::fights_on(self.fought.as_ref(), &task);
        if self.last_plan.as_deref() == Some(line.as_str()) && step != PlanStep::Wait && !fights_on
        {
            self.same_plan += 1;
        } else {
            if self.last_plan.as_deref() != Some(line.as_str()) {
                // The step moved on (a kill counted): its fights start over.
                self.fought = None;
            }
            self.same_plan = 1;
            self.last_plan = Some(line.clone());
        }
        if self.same_plan > MAX_SAME_PLAN {
            let stop = format!(
                "{task}: the step did not end after the bot did it {MAX_SAME_PLAN} times ({line})"
            );
            let verdict = if matches!(step, PlanStep::Kill { .. }) {
                Fought::verdict(self.fought.as_ref(), &task, stop)
            } else {
                Verdict::Broken(stop)
            };
            self.end_quest(verdict);
            return self.next_quest(client, own, since);
        }
        match step {
            PlanStep::Wait => self.pause(since, WAIT_PAUSE, Ask::State),
            PlanStep::Broken(reason) => {
                self.end_quest(Verdict::Broken(format!("{task}: {reason}")));
                return self.next_quest(client, own, since);
            }
            PlanStep::NotPlayed(reason) => {
                self.end_quest(Verdict::NotPlayed(format!("{task}: {reason}")));
                return self.next_quest(client, own, since);
            }
            PlanStep::Client(calls) => {
                for call in &calls {
                    Self::call(client, call)?;
                }
                self.actions.push(format!("{task}: client calls"));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Talk { npc, at } => {
                if let Some(at) = at {
                    self.teleport(client, own, at)?;
                }
                self.talk(client, own, &npc)?;
                self.actions.push(format!("{task}: talked to {npc}"));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Topic { topic, talk } => {
                if let Some((npc, at)) = &talk {
                    self.teleport(client, own, *at)?;
                    self.talk(client, own, npc)?;
                }
                let finished = ConversationClientMsg::FinishedTopics {
                    header,
                    topics: vec![topic],
                };
                client.send_message(
                    MESSAGE_CHANNEL,
                    true,
                    finished.type_index(),
                    finished.encode(),
                )?;
                self.actions
                    .push(format!("{task}: finished topic {topic:08x}"));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Go { at, territory } => {
                self.teleport(client, own, at)?;
                if let Some(territory) = territory {
                    Self::call(client, &entered(header, territory))?;
                }
                self.actions
                    .push(format!("{task}: went to {:.0} {:.0}", at.x, at.y));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Leave(territories) => {
                for territory in territories {
                    Self::call(client, &exited(header, territory))?;
                }
                self.actions.push(format!("{task}: left its place"));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Kill {
                vitals,
                count,
                at,
                territory,
                placed,
            } => {
                self.teleport(client, own, at)?;
                if let Some(territory) = territory {
                    Self::call(client, &entered(header, territory))?;
                }
                if placed {
                    self.actions
                        .push(format!("{task}: fought the placed {vitals} for {count} kills"));
                } else {
                    self.say(
                        client,
                        own,
                        &format!("!spawn {vitals} {SPAWN_LEVEL} {count}"),
                    )?;
                    self.actions
                        .push(format!("{task}: fought {count} spawned {vitals}"));
                }
                self.fight = Some(Fight {
                    task,
                    saw_mobs: false,
                    swings: 0,
                    hits_before: self.combat.hits_dealt(),
                    mob_deaths_before: self.combat.mob_deaths(),
                });
                self.phase = Phase::Fight {
                    until: since + FIGHT_TIME,
                };
            }
            PlanStep::Item {
                node: Some(node),
                ..
            } => {
                let Some(index) = self.untried_node(lowercase_crc32(&node)) else {
                    self.end_quest(Verdict::Broken(format!(
                        "{task}: the bot gathered each {node} node of the level and the step did not end"
                    )));
                    return self.next_quest(client, own, since);
                };
                self.tried_nodes.insert(index);
                let steps = gather_steps(&node, self.nodes[index], index);
                self.gather = Some((
                    format!("{task}: gathered {node} node {index}"),
                    RunningSteps::of(steps),
                ));
                self.phase = Phase::Gather;
            }
            PlanStep::Item {
                item,
                count,
                source,
                node: None,
            } => {
                self.say(client, own, &format!("!give {item} {count}"))?;
                self.actions.push(format!(
                    "{task}: given {count} {item} by a game master ({source})"
                ));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
            PlanStep::Achievements { needs, sources } => {
                let first: Vec<String> = sources
                    .into_iter()
                    .filter(|quest| !self.played.contains(&quest.to_lowercase()))
                    .collect();
                if first.is_empty() {
                    self.end_quest(Verdict::NotPlayed(format!(
                        "{task}: needs achievements no quest the bot can play gives: {needs}"
                    )));
                    return self.next_quest(client, own, since);
                }
                // The quests that give the achievements go first; this one waits in the journal.
                self.actions.push(format!(
                    "{task}: needs {needs}; plays {} first",
                    first.join(", ")
                ));
                let current = self.current().to_owned();
                self.queue.push_front(current);
                for quest in first.into_iter().rev() {
                    self.queue.push_front(quest);
                }
                self.current = None;
                return self.advance(client, own, since);
            }
            PlanStep::Progression { id, target, points } => {
                let line = if points {
                    format!("!masteryxp {id} {target}")
                } else {
                    format!("!mastery {id} {target}")
                };
                self.say(client, own, &line)?;
                self.actions.push(format!("{task}: {line}"));
                self.pause(since, ACTION_PAUSE, Ask::State);
            }
        }
        Ok(())
    }

    /// Plays the activities: each step's acts one at a time, [`ACT_GAP`] apart, then its proof.
    fn activity_tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        own: u64,
        since: Duration,
    ) -> Result<(), RepError> {
        if self.running.is_none() {
            let Some(activity) = self.activities.pop_front() else {
                self.phase = Phase::Done;
                return Ok(());
            };
            match activity.steps {
                Ok(steps) => self.running = Some((activity.kind, RunningSteps::of(steps))),
                Err(why) => {
                    self.activity_plays.push(QuestPlay {
                        quest: format!("{:?}", activity.kind),
                        verdict: Verdict::NotPlayed(why),
                        actions: Vec::new(),
                    });
                    return Ok(());
                }
            }
        }
        let Some((kind, mut running)) = self.running.take() else {
            return Ok(());
        };
        match self.steps_tick(client, outcome, own, since, &mut running)? {
            Some(verdict) => self.activity_plays.push(QuestPlay {
                quest: format!("{kind:?}"),
                verdict,
                actions: running.proved,
            }),
            None => self.running = Some((kind, running)),
        }
        Ok(())
    }

    /// Plays the gather of a quest step's node; once it ended, notes how and asks where the
    /// quest stands.
    fn gather_tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        own: u64,
        since: Duration,
    ) -> Result<(), RepError> {
        let Some((what, mut running)) = self.gather.take() else {
            return self.ask(client, own, Ask::State, since);
        };
        match self.steps_tick(client, outcome, own, since, &mut running)? {
            Some(Verdict::Completed) => self.actions.push(what),
            Some(Verdict::Broken(why) | Verdict::NotPlayed(why)) => {
                self.actions.push(format!("{what}: {why}"));
            }
            None => {
                self.gather = Some((what, running));
                return Ok(());
            }
        }
        self.ask(client, own, Ask::State, since)
    }

    /// The nearest node of gatherable `gatherable` to the bot that it has not tried yet.
    fn untried_node(&self, gatherable: u32) -> Option<usize> {
        let own = self.own_place()?;
        nearest_untried(&self.nodes, &self.tried_nodes, gatherable, [own.x, own.y, own.z])
    }

    /// Plays one tick of `running`; its verdict once it ended.
    fn steps_tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        own: u64,
        since: Duration,
        running: &mut RunningSteps,
    ) -> Result<Option<Verdict>, RepError> {
        let Some(step) = running.steps.get(running.step) else {
            return Ok(Some(Verdict::Completed));
        };
        if running
            .last_act
            .is_some_and(|last| since.saturating_sub(last) < ACT_GAP)
        {
            return Ok(None);
        }
        if let Some(act) = step.acts.get(running.act) {
            if running.act == 0 {
                running.lines_before = self.lines.len();
                running.messages_before = outcome.messages.len();
                running.answers_before = self.answers.len();
            }
            match act {
                Act::Gm(line) => self.say(client, own, line)?,
                Act::SummonKey => {
                    if let Some(position) = self.own_input_position() {
                        let counter = self.input_counter;
                        self.input_counter = counter.wrapping_add(ATTACK_RECORDS);
                        send_input(client, &ClientInputMsg::summon_mount(position, counter))?;
                    }
                }
                Act::Pause(wait) => {
                    running.act += 1;
                    running.last_act = Some((since + *wait).saturating_sub(ACT_GAP));
                    return Ok(None);
                }
                Act::Teleport([x, y, z]) => self.teleport(
                    client,
                    own,
                    Position {
                        x: *x,
                        y: *y,
                        z: *z,
                    },
                )?,
                Act::Call(make) => {
                    let (type_index, body) = make(own)?;
                    client.send_message(MESSAGE_CHANNEL, true, type_index, body)?;
                }
                Act::Interact { request, name } => {
                    self.interaction += 1;
                    running.interaction = Some(self.interaction);
                    let request = InteractionRequest {
                        interaction: self.interaction,
                        target_entity: if request.target_entity == 0 {
                            own
                        } else {
                            request.target_entity
                        },
                        ..*request
                    };
                    if let Some(position) = self.own_input_position() {
                        let body = Bit8::of_interaction(request, name)?;
                        send_input(
                            client,
                            &ClientInputMsg::position(position).interacting(body),
                        )?;
                    }
                }
            }
            running.act += 1;
            running.last_act = Some(since);
            return Ok(None);
        }
        let started = running.last_act.unwrap_or(since);
        let new_answers = &self.answers[running.answers_before..];
        let proof = match &step.proof {
            Proof::Sent => Some(Ok(())),
            Proof::Line(takes, _) => self.lines[running.lines_before..]
                .iter()
                .any(|line| takes(line))
                .then_some(Ok(())),
            Proof::Message(type_index) => outcome.messages[running.messages_before..]
                .iter()
                .any(|received| received.envelope.type_index() == Some(*type_index))
                .then_some(Ok(())),
            Proof::Accepted => new_answers.iter().find_map(|answer| match answer {
                InteractionAnswer::Validated {
                    interaction,
                    status,
                } if Some(*interaction) == running.interaction => Some(if *status == ACCEPTED {
                    Ok(())
                } else {
                    Err(format!("{}: refused (status {status})", step.what))
                }),
                _ => None,
            }),
            Proof::Ended => new_answers
                .contains(&InteractionAnswer::Ended)
                .then_some(Ok(())),
        };
        match proof {
            Some(Ok(())) => {
                running.proved.push(step.what.clone());
                running.step += 1;
                running.act = 0;
                Ok(None)
            }
            Some(Err(why)) => Ok(Some(Verdict::Broken(why))),
            None if since.saturating_sub(started) > step.wait => {
                let wanted = match &step.proof {
                    Proof::Line(_, described) => format!("an answer line: {described}"),
                    Proof::Message(type_index) => format!("a {type_index} message"),
                    Proof::Accepted => "the interaction's answer".to_owned(),
                    Proof::Ended => "the interaction's end".to_owned(),
                    Proof::Sent => String::new(),
                };
                let answered = self.lines[running.lines_before..].join(" / ");
                Ok(Some(Verdict::Broken(format!(
                    "{}: no proof within {} s (waited for {wanted}; the server answered: {})",
                    step.what,
                    step.wait.as_secs(),
                    if answered.is_empty() {
                        "nothing"
                    } else {
                        &answered
                    }
                ))))
            }
            None => Ok(None),
        }
    }

    /// Moves the bot to `at` with a game master line.
    fn teleport(&mut self, client: &mut RepClient, own: u64, at: Position) -> Result<(), RepError> {
        self.teleported_to = Some(at);
        self.say(
            client,
            own,
            &format!("!tp {:.1} {:.1} {:.1}", at.x, at.y, at.z),
        )
    }

    /// Where the bot stands: the place of its last teleport until the server's own state shows
    /// it there (an input at the old place would move it back), else the place it last saw.
    fn own_place(&self) -> Option<Position> {
        let seen = self.world.own_position().map(|own| Position {
            x: own.x,
            y: own.y,
            z: own.z(),
        });
        match (self.teleported_to, seen) {
            (Some(target), Some(seen))
                if target.horizontal_distance(seen) <= TELEPORT_SETTLED_M =>
            {
                Some(seen)
            }
            (Some(target), _) => Some(target),
            (None, seen) => seen,
        }
    }

    /// Talks to NPC `npc` with a talk interaction in the bot's input.
    fn talk(&mut self, client: &mut RepClient, own: u64, npc: &str) -> Result<(), RepError> {
        self.interaction += 1;
        let request = InteractionRequest {
            target: [0; nw_messages::wire::RAW_ID_LEN],
            target_entity: own,
            interaction: self.interaction,
            kind: TALK_KIND,
            gatherable: None,
        };
        let name = format!("{NPC_MARK}{npc}");
        let Some(position) = self.own_input_position() else {
            return Ok(());
        };
        let body = Bit8::of_interaction(request, name.as_bytes())?;
        send_input(
            client,
            &ClientInputMsg::position(position).interacting(body),
        )
    }

    /// The bot's place as an input position.
    fn own_input_position(&self) -> Option<InputPosition> {
        let own = self.own_place()?;
        InputPosition::from_metres(own.x, own.y, own.z)
    }

    /// Notes what the fight did and asks where the quest stands; a fight in which the bot saw
    /// no mob says nothing of the quest, so the quest is not played.
    fn fight_ended(
        &mut self,
        client: &mut RepClient,
        own: u64,
        since: Duration,
    ) -> Result<(), RepError> {
        let Some(fight) = self.fight.take() else {
            return self.ask(client, own, Ask::State, since);
        };
        if let Some(verdict) = fight.not_played() {
            self.end_quest(verdict);
            return self.next_quest(client, own, since);
        }
        let hits = self.combat.hits_dealt() - fight.hits_before;
        let mob_deaths = self.combat.mob_deaths() - fight.mob_deaths_before;
        self.actions.push(format!(
            "{}: swung {} times, landed {hits} hits, saw {mob_deaths} mobs die",
            fight.task, fight.swings
        ));
        self.fought = Some(match self.fought.take() {
            Some(fought) if fought.task == fight.task => Fought {
                hits: fought.hits + hits,
                mob_deaths: fought.mob_deaths + mob_deaths,
                fights: fought.fights + 1,
                last_hits: hits,
                ..fought
            },
            _ => Fought {
                task: fight.task,
                hits,
                mob_deaths,
                fights: 1,
                last_hits: hits,
            },
        });
        self.ask(client, own, Ask::State, since)
    }

    /// Walks to the nearest mob and swings at it.
    fn fight(&mut self, client: &mut RepClient, since: Duration) -> Result<(), RepError> {
        let Some(own) = self.own_place() else {
            return Ok(());
        };
        let from = [own.x, own.y, own.z];
        let Some((_, mob)) = self
            .combat
            .mobs()
            .min_by(|(_, a), (_, b)| planar(from, [a.x, a.y]).total_cmp(&planar(from, [b.x, b.y])))
        else {
            return Ok(());
        };
        if let Some(fight) = &mut self.fight {
            fight.saw_mobs = true;
        }
        let distance = planar(from, [mob.x, mob.y]);
        if distance > REACH_M {
            if self
                .last_move
                .is_some_and(|last| since.saturating_sub(last) < MOVE_INTERVAL)
            {
                return Ok(());
            }
            self.last_move = Some(since);
            let length = (SPEED_MPS * MOVE_INTERVAL.as_secs_f32()).min(distance - REACH_M / 2.0);
            let x = own.x + (mob.x - own.x) / distance * length;
            let y = own.y + (mob.y - own.y) / distance * length;
            let Some(position) = InputPosition::from_metres(x, y, mob.z()) else {
                return Ok(());
            };
            // The server sends no own place for the bot's own moves: the bot stands where it
            // sent itself.
            self.teleported_to = Some(Position { x, y, z: mob.z() });
            return send_input(client, &ClientInputMsg::position(position));
        }
        if self
            .last_swing
            .is_some_and(|last| since.saturating_sub(last) < SWING_INTERVAL)
        {
            return Ok(());
        }
        self.last_swing = Some(since);
        let Some(at) = InputPosition::from_metres(own.x, own.y, own.z) else {
            return Ok(());
        };
        if let Some(fight) = &mut self.fight {
            fight.swings += 1;
        }
        let yaw = yaw_toward(
            own,
            Position {
                x: mob.x,
                y: mob.y,
                z: mob.z(),
            },
        );
        let counter = self.input_counter;
        self.input_counter = counter.wrapping_add(ATTACK_RECORDS);
        send_input(
            client,
            &ClientInputMsg::attack(at, counter).looking([yaw, LEVEL_PITCH]),
        )
    }

    fn end_quest(&mut self, verdict: Verdict) {
        if verdict == Verdict::Completed {
            self.completed_since_deferral = true;
        }
        self.plays.push(QuestPlay {
            quest: self.current().to_owned(),
            verdict,
            actions: std::mem::take(&mut self.actions),
        });
    }

    fn next_quest(
        &mut self,
        client: &mut RepClient,
        own: u64,
        since: Duration,
    ) -> Result<(), RepError> {
        self.current = None;
        self.advance(client, own, since)
    }

    /// Goes on with the next quest to play: the queue's next, else the deferred ones again when
    /// a quest completed since they were deferred. Each deferred quest that no NPC ever offered
    /// ends as not played, with its reason.
    fn advance(
        &mut self,
        client: &mut RepClient,
        own: u64,
        since: Duration,
    ) -> Result<(), RepError> {
        if self.queue.is_empty() && !self.deferred.is_empty() {
            if self.completed_since_deferral {
                self.completed_since_deferral = false;
                self.queue
                    .extend(self.deferred.drain(..).map(|(quest, _)| quest));
            } else {
                for (quest, why) in std::mem::take(&mut self.deferred) {
                    self.plays.push(QuestPlay {
                        quest,
                        verdict: Verdict::NotPlayed(format!("no NPC offers it: {why}")),
                        actions: Vec::new(),
                    });
                }
            }
        }
        self.current = self.queue.pop_front();
        if let Some(quest) = &self.current {
            self.played.insert(quest.to_lowercase());
        }
        self.accepted = false;
        self.turned_in = false;
        self.fought = None;
        self.rounds = 0;
        self.last_plan = None;
        self.same_plan = 0;
        self.ask(client, own, Ask::State, since)
    }
}

/// The client's entry into territory `territory` (2297).
fn entered(header: FacetHeader, territory: u16) -> ClientFacetMsg {
    ClientFacetMsg::TerritoryEntered {
        header,
        area: [0; nw_messages::wire::RAW_ID_LEN],
        area_entity: 0,
        extra: 0,
        territory,
        unknown: 0,
    }
}

/// The client's exit from territory `territory` (6416).
fn exited(header: FacetHeader, territory: u16) -> ClientFacetMsg {
    ClientFacetMsg::TerritoryExited {
        header,
        area: [0; nw_messages::wire::RAW_ID_LEN],
        area_entity: 0,
        extra: 0,
        territory,
    }
}

/// Runs the play. `new_client` makes a client for a token; `report` gets one line per HTTPS
/// step.
///
/// # Errors
///
/// Returns [`PairError`] for the first step that fails.
pub async fn run_play(
    http: &Client,
    settings: &PlaySettings,
    activities: Vec<Activity>,
    nodes: Arc<[GatherNode]>,
    new_client: impl Fn(LoginToken) -> Result<RepClient, RepError>,
    mut report: impl FnMut(String),
) -> Result<PlayReport, PairError> {
    let admission = walk(http, &settings.account, &mut report).await?;
    let mut player = Player {
        world: WorldView::default(),
        combat: CombatView::default(),
        read: 0,
        own_since: None,
        lines: Vec::new(),
        phase: Phase::LeaveTutorial,
        queue: settings.quests.iter().skip(1).cloned().collect(),
        current: settings.quests.first().cloned(),
        deferred: Vec::new(),
        completed_since_deferral: false,
        played: settings
            .quests
            .first()
            .map(|quest| quest.to_lowercase())
            .into_iter()
            .collect(),
        accepted: false,
        turned_in: false,
        rounds: 0,
        last_plan: None,
        same_plan: 0,
        actions: Vec::new(),
        plays: Vec::new(),
        last_swing: None,
        last_move: None,
        input_counter: FIRST_INPUT_COUNTER,
        interaction: 0,
        activities: activities.into(),
        running: None,
        nodes,
        tried_nodes: HashSet::new(),
        gather: None,
        activity_plays: Vec::new(),
        answers: Vec::new(),
        teleported_to: None,
        fight: None,
        fought: None,
    };
    let session = session_with(
        admission.rep_address,
        new_client(login_token(&admission.token))?,
        settings.hold,
        settings.registration_timeout,
        |client, outcome, since| {
            if player.phase == Phase::Done {
                return client.close();
            }
            player.tick(client, outcome, since)
        },
    )
    .await?;
    // A quest still being played when the hold ended stops as broken.
    if player.current.is_some() && player.phase != Phase::Done {
        player.end_quest(Verdict::Broken("the hold ended first".to_owned()));
    }
    if let Some((kind, running)) = player.running.take() {
        player.activity_plays.push(QuestPlay {
            quest: format!("{kind:?}"),
            verdict: Verdict::Broken("the hold ended first".to_owned()),
            actions: running.proved,
        });
    }
    Ok(PlayReport {
        outcome: session.outcome,
        quests: player.plays,
        activities: player.activity_plays,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u64 = 7;

    fn header() -> FacetHeader {
        FacetHeader::of(CALL_COMPONENT, OWN)
    }

    #[test]
    fn plan_lines_read_into_steps() {
        let read = |line: &str| plan_step(line, header()).map(|(_, step)| step);
        assert_eq!(
            read("plan g_talk talk Walk_Npc at 1029.0 10.0 50.0"),
            Some(PlanStep::Talk {
                npc: "Walk_Npc".to_owned(),
                at: Some(Position {
                    x: 1029.0,
                    y: 10.0,
                    z: 50.0
                })
            })
        );
        assert_eq!(
            read("plan g_kill kill Test_Boar 2 at 1054.0 10.0 50.0 territory 42"),
            Some(PlanStep::Kill {
                vitals: "Test_Boar".to_owned(),
                count: 2,
                at: Position {
                    x: 1054.0,
                    y: 10.0,
                    z: 50.0
                },
                territory: Some(42),
                placed: false
            })
        );
        assert!(
            matches!(
                read("plan w_kill kill Test_Boar 3 at 1064.0 10.0 50.0 placed"),
                Some(PlanStep::Kill { placed: true, .. })
            ),
            "only the placed mobs count"
        );
        assert_eq!(
            read("plan t topic 0308314e after talk Grace at 1.0 2.0 3.0"),
            Some(PlanStep::Topic {
                topic: 0x0308_314e,
                talk: Some((
                    "Grace".to_owned(),
                    Position {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0
                    }
                ))
            })
        );
        assert_eq!(
            read("plan t item 12A_Key 1 from the drop of quest object Box"),
            Some(PlanStep::Item {
                item: "12A_Key".to_owned(),
                count: 1,
                source: "the drop of quest object Box".to_owned(),
                node: None
            })
        );
        assert_eq!(
            read("plan t item Orb 2 gathered node Walk_Orb from the drop of quest object Walk_Orb"),
            Some(PlanStep::Item {
                item: "Orb".to_owned(),
                count: 2,
                source: "the drop of quest object Walk_Orb".to_owned(),
                node: Some("Walk_Orb".to_owned())
            }),
            "the bot gathers the nodes of Walk_Orb"
        );
        assert_eq!(
            read("plan t client cutscene 1a2b3c4d"),
            Some(PlanStep::Client(vec![
                ClientFacetMsg::RequestCutscene {
                    header: header(),
                    cutscene: 0x1a2b_3c4d
                },
                ClientFacetMsg::CutsceneEnded { header: header() }
            ]))
        );
        assert_eq!(
            read("plan t stop Broken: no mob that counts as Dragon spawns in the open world"),
            Some(PlanStep::Broken(
                "Broken: no mob that counts as Dragon spawns in the open world".to_owned()
            ))
        );
        assert!(matches!(
            read("plan t stop NotWalked: needs a run of game mode X"),
            Some(PlanStep::NotPlayed(_))
        ));
        assert_eq!(read("plan t wait"), Some(PlanStep::Wait));
        assert_eq!(
            read(
                "plan t achievements A (from a step of quest 06_beach), B (from the end of quest 12_x)"
            ),
            Some(PlanStep::Achievements {
                needs: "A (from a step of quest 06_beach), B (from the end of quest 12_x)"
                    .to_owned(),
                sources: vec!["06_beach".to_owned(), "12_x".to_owned()]
            })
        );
        assert_eq!(read("running q"), None);
    }

    #[test]
    fn a_kill_step_that_killed_nothing_is_not_played_and_one_whose_kills_did_not_count_is_broken()
    {
        let fought = |mob_deaths| Fought {
            task: "w_kill".to_owned(),
            hits: 170,
            mob_deaths,
            fights: 3,
            last_hits: 0,
        };
        assert_eq!(
            Fought::verdict(Some(&fought(0)), "w_kill", "stop".to_owned()),
            Verdict::NotPlayed("w_kill: the bot landed 170 hits and no mob died (stop)".to_owned())
        );
        assert_eq!(
            Fought::verdict(Some(&fought(4)), "w_kill", "stop".to_owned()),
            Verdict::Broken("stop; 4 mobs died near the bot".to_owned())
        );
        assert_eq!(
            Fought::verdict(Some(&fought(0)), "other", "stop".to_owned()),
            Verdict::Broken("stop".to_owned()),
            "the fights of another step"
        );
    }

    #[test]
    fn the_bot_fights_on_while_its_hits_land() {
        let fought = |fights, last_hits| Fought {
            task: "w_kill".to_owned(),
            hits: 56,
            mob_deaths: 0,
            fights,
            last_hits,
        };
        assert!(Fought::fights_on(Some(&fought(1, 56)), "w_kill"));
        assert!(!Fought::fights_on(Some(&fought(1, 0)), "w_kill"), "no hit landed");
        assert!(
            !Fought::fights_on(Some(&fought(MAX_KILL_FIGHTS, 56)), "w_kill"),
            "the most fights"
        );
        assert!(!Fought::fights_on(Some(&fought(1, 56)), "other"));
        assert!(!Fought::fights_on(None, "w_kill"));
    }

    #[test]
    fn a_fight_without_a_mob_in_view_plays_nothing_of_its_quest() {
        let mut fight = Fight {
            task: "w_kill".to_owned(),
            saw_mobs: false,
            swings: 0,
            hits_before: 0,
            mob_deaths_before: 0,
        };
        assert_eq!(
            fight.not_played(),
            Some(Verdict::NotPlayed(
                "w_kill: the bot saw no mob there in 45 s".to_owned()
            ))
        );
        fight.saw_mobs = true;
        assert_eq!(fight.not_played(), None, "the quest's state decides");
    }

    #[test]
    fn the_bot_gathers_the_nearest_node_of_the_kind_it_did_not_try() {
        const ORB: u32 = 0x0b0b;
        const STONE: u32 = 0x5705;
        let nodes = [
            GatherNode {
                gatherable: ORB,
                at: [100.0, 0.0, 0.0],
            },
            GatherNode {
                gatherable: STONE,
                at: [1.0, 0.0, 0.0],
            },
            GatherNode {
                gatherable: ORB,
                at: [10.0, 0.0, 0.0],
            },
        ];
        let mut tried = HashSet::new();
        let from = [0.0, 0.0, 0.0];
        assert_eq!(nearest_untried(&nodes, &tried, ORB, from), Some(2));
        tried.insert(2);
        assert_eq!(nearest_untried(&nodes, &tried, ORB, from), Some(0));
        tried.insert(0);
        assert_eq!(nearest_untried(&nodes, &tried, ORB, from), None, "each tried");
    }

    #[test]
    fn the_report_counts_each_verdict_and_lists_each_quest() {
        let report = PlayReport {
            outcome: RepOutcome::default(),
            activities: Vec::new(),
            quests: vec![
                QuestPlay {
                    quest: "A".to_owned(),
                    verdict: Verdict::Completed,
                    actions: vec!["accepted".to_owned(), "turned in".to_owned()],
                },
                QuestPlay {
                    quest: "B".to_owned(),
                    verdict: Verdict::Broken("t: no | place".to_owned()),
                    actions: Vec::new(),
                },
            ],
        };
        let text = report.markdown();
        assert!(text.contains("| Completed | 1 |"));
        assert!(text.contains("| Broken | 1 |"));
        assert!(text.contains("| A | Completed |  | accepted; turned in |"));
        assert!(text.contains("| B | Broken | t: no \\| place |  |"));
    }
}
