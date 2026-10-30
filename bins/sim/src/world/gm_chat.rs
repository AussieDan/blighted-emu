// SPDX-License-Identifier: AGPL-3.0-or-later
//! GM chat commands. The retail client has no menu or console command that sends its GM calls
//! (docs/re/2026-09-18-admin-studio-s10-gm-study.md), so a game master types them in chat: a
//! line that starts with [`GM_COMMAND_PREFIX`], on any channel. The sim takes the line before
//! the social service does, runs it as the admin command of the same name and answers the
//! sender alone with system lines. A line of a player without the game master rank is chat.
//!
//! A command acts on its sender, or on the player of this sim named by one more word at its
//! end. Levels and ranks are as the client shows them (1 is the first); coins are as the admin
//! site takes them.

use bevy_ecs::entity::Entity;
use nw_bus::{
    AdminAction, AdminReply, ConnectionRef, Point, RunningEvent, SocialDelivery, SocialFacetKind,
    SocialHeader,
};
use nw_messages::social::{ChatCall, ChatChannel, ChatMessage, ChatPush, SocialCall, SocialPush};

use super::{Body, Player, SimWorld};

/// The first character of a GM chat command.
pub const GM_COMMAND_PREFIX: char = '!';
/// The word of [`Change::MasteryRank`] for every weapon mastery.
const EVERY_MASTERY: &str = "all";
/// The words of [`Change::God`].
const ON: &str = "on";
const OFF: &str = "off";
/// The words of the event commands.
const START: &str = "start";
const STOP: &str = "stop";
/// The answer to a command the sim carried out.
const DONE: &str = "Done.";
/// The answer when the named player is not on this sim.
const NOT_HERE: &str = "That player is not on this server.";
/// The answer to a word that is no command.
const UNKNOWN: &str = "Unknown command. Type !help for the list.";
/// The answer when no event runs.
const NO_EVENTS: &str = "No events are running.";

/// One command word: its usage line and its reader of the words after it.
struct Verb {
    name: &'static str,
    usage: &'static str,
    read: fn(&[&str]) -> Option<Command>,
}

/// Every command, in the order of `!help`.
const VERBS: [Verb; 19] = [
    Verb {
        name: "help",
        usage: "!help",
        read: |words| words.is_empty().then_some(Command::Help),
    },
    Verb {
        name: "where",
        usage: "!where [player]",
        read: |words| target(words, 0).map(|(_, target)| Command::Where { target }),
    },
    Verb {
        name: "level",
        usage: "!level <level> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            let level = number::<u32>(words[0])?.checked_sub(1)?;
            Some(Command::Change {
                target,
                change: Change::Level(level),
            })
        },
    },
    Verb {
        name: "xp",
        usage: "!xp <amount> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            Some(Command::Change {
                target,
                change: Change::Experience(number(words[0])?),
            })
        },
    },
    Verb {
        name: "coins",
        usage: "!coins <amount> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            Some(Command::Change {
                target,
                change: Change::Coins(number(words[0])?),
            })
        },
    },
    Verb {
        name: "mastery",
        usage: "!mastery <progression id|all> <rank> [player]",
        read: |words| {
            let (words, target) = target(words, 2)?;
            let mastery =
                (!words[0].eq_ignore_ascii_case(EVERY_MASTERY)).then(|| words[0].to_owned());
            let rank = number::<u16>(words[1])?.checked_sub(1)?;
            Some(Command::Change {
                target,
                change: Change::MasteryRank { mastery, rank },
            })
        },
    },
    Verb {
        name: "masteryxp",
        usage: "!masteryxp <mastery id> <amount> [player]",
        read: |words| {
            let (words, target) = target(words, 2)?;
            Some(Command::Change {
                target,
                change: Change::MasteryExperience {
                    mastery: words[0].to_owned(),
                    amount: number(words[1])?,
                },
            })
        },
    },
    Verb {
        name: "heal",
        usage: "!heal [player]",
        read: |words| {
            target(words, 0).map(|(_, target)| Command::Change {
                target,
                change: Change::Heal,
            })
        },
    },
    Verb {
        name: "kill",
        usage: "!kill [player]",
        read: |words| {
            target(words, 0).map(|(_, target)| Command::Change {
                target,
                change: Change::Kill,
            })
        },
    },
    Verb {
        name: "god",
        usage: "!god <on|off> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            let on = match words[0].to_ascii_lowercase().as_str() {
                ON => true,
                OFF => false,
                _ => return None,
            };
            Some(Command::Change {
                target,
                change: Change::God(on),
            })
        },
    },
    Verb {
        name: "kick",
        usage: "!kick <player>",
        read: |words| match words {
            [name] => Some(Command::Kick {
                name: (*name).to_owned(),
            }),
            _ => None,
        },
    },
    Verb {
        name: "give",
        usage: "!give <item id> <quantity> [player]",
        read: |words| {
            let (words, target) = target(words, 2)?;
            Some(Command::Change {
                target,
                change: Change::Give {
                    item: words[0].to_owned(),
                    quantity: number(words[1])?,
                },
            })
        },
    },
    Verb {
        name: "take",
        usage: "!take <item id> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            Some(Command::Change {
                target,
                change: Change::Take {
                    item: words[0].to_owned(),
                },
            })
        },
    },
    Verb {
        name: "quest",
        usage: "!quest <objective id> [player]",
        read: |words| {
            let (words, target) = target(words, 1)?;
            Some(Command::Change {
                target,
                change: Change::Quest {
                    objective: words[0].to_owned(),
                },
            })
        },
    },
    Verb {
        name: "tp",
        usage: "!tp <x> <y> <z> [player] | !tp <player>",
        read: |words| match words {
            [name] => Some(Command::TeleportToPlayer {
                name: (*name).to_owned(),
            }),
            _ => {
                let (words, target) = target(words, 3)?;
                Some(Command::Teleport {
                    target,
                    to: Point {
                        x: number(words[0])?,
                        y: number(words[1])?,
                        z: number(words[2])?,
                    },
                })
            }
        },
    },
    Verb {
        name: "spawn",
        usage: "!spawn <vitals id> <level> <count>",
        read: |words| match words {
            [vitals, level, count] => Some(Command::Spawn {
                vitals: (*vitals).to_owned(),
                level: number(level)?,
                count: number(count)?,
            }),
            _ => None,
        },
    },
    Verb {
        name: "despawn",
        usage: "!despawn",
        read: |words| words.is_empty().then_some(Command::Despawn),
    },
    Verb {
        name: "event",
        usage: "!event <start|stop> <event id>",
        read: |words| match words {
            [action, event] if action.eq_ignore_ascii_case(START) => Some(Command::StartEvent {
                event: (*event).to_owned(),
            }),
            [action, event] if action.eq_ignore_ascii_case(STOP) => Some(Command::StopEvent {
                event: (*event).to_owned(),
            }),
            _ => None,
        },
    },
    Verb {
        name: "events",
        usage: "!events",
        read: |words| words.is_empty().then_some(Command::Events),
    },
];

/// Who a command acts on.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Sender,
    Named(String),
}

/// One read GM chat command.
#[derive(Debug, Clone, PartialEq)]
enum Command {
    Help,
    Where {
        target: Target,
    },
    Change {
        target: Target,
        change: Change,
    },
    Teleport {
        target: Target,
        to: Point,
    },
    TeleportToPlayer {
        name: String,
    },
    Kick {
        name: String,
    },
    Spawn {
        vitals: String,
        level: u32,
        count: u32,
    },
    Despawn,
    StartEvent {
        event: String,
    },
    StopEvent {
        event: String,
    },
    Events,
}

/// A command that changes one player: an admin action of that player.
#[derive(Debug, Clone, PartialEq)]
enum Change {
    /// Zero-based.
    Level(u32),
    Experience(u32),
    Coins(u64),
    /// `None` is every weapon mastery; the rank is zero-based.
    MasteryRank {
        mastery: Option<String>,
        rank: u16,
    },
    MasteryExperience {
        mastery: String,
        amount: u32,
    },
    Heal,
    Kill,
    God(bool),
    Give {
        item: String,
        quantity: u32,
    },
    Take {
        item: String,
    },
    Quest {
        objective: String,
    },
}

impl Change {
    fn action(self, entity: u64) -> AdminAction {
        match self {
            Self::Level(level) => AdminAction::SetLevel { entity, level },
            Self::Experience(amount) => AdminAction::GiveExperience { entity, amount },
            Self::Coins(coins) => AdminAction::SetCoins { entity, coins },
            Self::MasteryRank { mastery, rank } => AdminAction::SetMasteryRank {
                entity,
                mastery,
                rank,
            },
            Self::MasteryExperience { mastery, amount } => AdminAction::GiveMasteryExperience {
                entity,
                mastery,
                amount,
            },
            Self::Heal => AdminAction::Heal { entity },
            Self::Kill => AdminAction::Kill { entity },
            Self::God(on) => AdminAction::GodMode { entity, on },
            Self::Give { item, quantity } => AdminAction::GiveItem {
                entity,
                item,
                quantity,
            },
            Self::Take { item } => AdminAction::TakeItem { entity, item },
            Self::Quest { objective } => AdminAction::StartQuest { entity, objective },
        }
    }
}

/// The words before the target and the target: `count` words for the sender, one more for a
/// named player; `None` for any other count.
fn target<'a>(words: &'a [&'a str], count: usize) -> Option<(&'a [&'a str], Target)> {
    match words.len() {
        len if len == count => Some((words, Target::Sender)),
        len if len == count + 1 => Some((&words[..count], Target::Named(words[count].to_owned()))),
        _ => None,
    }
}

fn number<T: std::str::FromStr>(word: &str) -> Option<T> {
    word.parse().ok()
}

/// The command of chat line `line`: `None` when the line is no GM command; the usage or
/// [`UNKNOWN`] when it does not read.
fn read(line: &str) -> Option<Result<Command, String>> {
    let rest = line.trim().strip_prefix(GM_COMMAND_PREFIX)?;
    let mut words = rest.split_whitespace();
    let name = words.next().unwrap_or_default();
    let words: Vec<&str> = words.collect();
    let Some(verb) = VERBS
        .iter()
        .find(|verb| verb.name.eq_ignore_ascii_case(name))
    else {
        return Some(Err(UNKNOWN.to_owned()));
    };
    Some((verb.read)(&words).ok_or_else(|| format!("Usage: {}", verb.usage)))
}

/// What the server loop does after [`SimWorld::gm_chat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmChat {
    /// The sim carried the command out and queued its answer.
    Answered,
    /// Kick the player with this entity number, then answer with
    /// [`SimWorld::answer_gm_chat`].
    Kick(u64),
}

/// What a command gives.
enum Outcome {
    Lines(Vec<String>),
    Kick(u64),
}

impl SimWorld {
    /// Takes the client call `type_index` with `body` of `connection` when it is a chat line of
    /// a game master that starts with [`GM_COMMAND_PREFIX`]: runs it at `now_ms` and queues the
    /// answer. `None` for every other call: it goes to the social service.
    pub fn gm_chat(
        &mut self,
        connection: &ConnectionRef,
        type_index: u64,
        body: &[u8],
        now_ms: u64,
    ) -> Option<GmChat> {
        let Some(Ok((_, SocialCall::Chat(ChatCall::Send { body: line, .. })))) =
            SocialCall::decode(type_index, body)
        else {
            return None;
        };
        let sender = *self.by_connection.get(connection)?;
        let player = self.world.get::<Player>(sender)?;
        if !player.game_master {
            return None;
        }
        let number = player.number;
        let command = read(&line)?;
        tracing::info!(entity = number, %line, "GM chat command");
        let outcome = match command {
            Ok(command) => self.run_gm_chat(sender, number, command, now_ms),
            Err(usage) => Outcome::Lines(vec![usage]),
        };
        match outcome {
            Outcome::Lines(lines) => {
                self.answer_gm_chat(connection, lines);
                Some(GmChat::Answered)
            }
            Outcome::Kick(entity) => Some(GmChat::Kick(entity)),
        }
    }

    /// Queues `lines` for the client of `connection` as system chat lines.
    pub fn answer_gm_chat(&mut self, connection: &ConnectionRef, lines: Vec<String>) {
        let Some(character) = self.character_of(connection) else {
            return;
        };
        tracing::info!(?lines, "GM chat answer");
        let push = SocialPush::Chat(ChatPush::Batch(
            lines
                .into_iter()
                .map(|line| ChatMessage::system(ChatChannel::System, line))
                .collect(),
        ));
        let args = match push.encode_args() {
            Ok(args) => args,
            Err(error) => {
                tracing::warn!(%error, "GM chat answer not sent");
                return;
            }
        };
        let delivery = SocialDelivery {
            recipients: vec![character],
            header: SocialHeader::Facet(SocialFacetKind::Chat),
            type_index: push.type_index(),
            args,
        };
        let outgoing = self.social_outgoing(&[delivery]);
        self.combat_messages.extend(outgoing);
    }

    fn run_gm_chat(
        &mut self,
        sender: Entity,
        number: u64,
        command: Command,
        now_ms: u64,
    ) -> Outcome {
        let here = |world: &Self| world.point_of(sender);
        let lines = match command {
            Command::Help => VERBS.iter().map(|verb| verb.usage.to_owned()).collect(),
            Command::Where { target } => match self.gm_target(&target, number) {
                Some(entity) => match self
                    .player_by_number(entity)
                    .and_then(|entity| self.point_of(entity))
                {
                    Some(at) => vec![format!("{:.1} {:.1} {:.1}", at.x, at.y, at.z)],
                    None => vec![NOT_HERE.to_owned()],
                },
                None => vec![NOT_HERE.to_owned()],
            },
            Command::Change { target, change } => match self.gm_target(&target, number) {
                Some(entity) => self.answer_of(&change.action(entity), now_ms),
                None => vec![NOT_HERE.to_owned()],
            },
            Command::Teleport { target, to } => match self.gm_target(&target, number) {
                Some(entity) => self.answer_of(&AdminAction::TeleportPlayer { entity, to }, now_ms),
                None => vec![NOT_HERE.to_owned()],
            },
            Command::TeleportToPlayer { name } => match self
                .player_named(&name)
                .and_then(|other| self.point_of(other))
            {
                Some(to) => {
                    self.answer_of(&AdminAction::TeleportPlayer { entity: number, to }, now_ms)
                }
                None => vec![NOT_HERE.to_owned()],
            },
            Command::Kick { name } => {
                return match self.gm_target(&Target::Named(name), number) {
                    Some(entity) => Outcome::Kick(entity),
                    None => Outcome::Lines(vec![NOT_HERE.to_owned()]),
                };
            }
            Command::Spawn {
                vitals,
                level,
                count,
            } => match here(self) {
                Some(at) => self.answer_of(
                    &AdminAction::SpawnMobs {
                        vitals,
                        level,
                        at,
                        count,
                    },
                    now_ms,
                ),
                None => vec![NOT_HERE.to_owned()],
            },
            Command::Despawn => self.answer_of(
                &AdminAction::DespawnMobs {
                    entities: Vec::new(),
                },
                now_ms,
            ),
            Command::StartEvent { event } => match here(self) {
                Some(at) => self.answer_of(&AdminAction::StartEvent { event, at }, now_ms),
                None => vec![NOT_HERE.to_owned()],
            },
            Command::StopEvent { event } => {
                self.answer_of(&AdminAction::StopEvent { event }, now_ms)
            }
            Command::Events => self.answer_of(&AdminAction::ListEvents, now_ms),
        };
        Outcome::Lines(lines)
    }

    /// Carries out `action` and gives its answer lines.
    fn answer_of(&mut self, action: &AdminAction, now_ms: u64) -> Vec<String> {
        answer_lines(self.admin_action(action, now_ms))
    }

    /// The entity number of `target`; `sender` is the sender's.
    fn gm_target(&mut self, target: &Target, sender: u64) -> Option<u64> {
        match target {
            Target::Sender => Some(sender),
            Target::Named(name) => {
                let entity = self.player_named(name)?;
                self.world.get::<Player>(entity).map(|player| player.number)
            }
        }
    }

    /// The player of this sim named `name` (any letter case).
    fn player_named(&mut self, name: &str) -> Option<Entity> {
        let mut query = self.world.query::<(Entity, &Player)>();
        query
            .iter(&self.world)
            .find(|(_, player)| player.name.eq_ignore_ascii_case(name))
            .map(|(entity, _)| entity)
    }

    /// Where the body of `entity` is.
    fn point_of(&self, entity: Entity) -> Option<Point> {
        let at = self.world.get::<Body>(entity)?.motion.position();
        Some(Point {
            x: at.x,
            y: at.y,
            z: at.z,
        })
    }
}

/// The answer lines of an admin reply.
pub fn answer_lines(reply: AdminReply) -> Vec<String> {
    match reply {
        AdminReply::Done => vec![DONE.to_owned()],
        AdminReply::Spawned { entities } => vec![format!("Spawned {} mobs.", entities.len())],
        AdminReply::NotMine => vec![NOT_HERE.to_owned()],
        AdminReply::Failed { reason } => vec![reason],
        AdminReply::Events { running } if running.is_empty() => vec![NO_EVENTS.to_owned()],
        AdminReply::Events { running } => running.iter().map(event_line).collect(),
    }
}

fn event_line(event: &RunningEvent) -> String {
    format!(
        "{}: {} mobs alive, {} waves left",
        event.event, event.mobs_alive, event.waves_left
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_content::progression::Progression;
    use nw_messages::social::chat::SEND_CHAT_MESSAGE_TYPE_INDEX;
    use nw_messages::{FacetHeader, Writer};
    use nw_reflect::Envelope;

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    const GM: u64 = 41;
    const PLAYER: u64 = 42;
    const SWORD: &str = "GreatSwordAbilityTable";
    /// The bytes of a facet header.
    const HEADER_LEN: usize = 16;

    fn world() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            progression: Progression::build([(0, 0), (1, 150)], [(0, 10), (1, 50)], [(SWORD, 2)]),
            ..PlayerContent::default()
        });
        let gm = nw_bus::Admit {
            game_master: true,
            ..admit_at(GM, 0.0, true)
        };
        world.admit(&gm, NOW, SERVER_TIME).expect("gm");
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("player");
        world
    }

    /// The body of a 1291 that says `line` in area chat.
    fn say(line: &str) -> Vec<u8> {
        let mut writer = Writer::new();
        FacetHeader {
            target: 0,
            entity: 0,
        }
        .write(&mut writer);
        writer.u32(ChatChannel::Area.wire());
        writer
            .prefixed(line.len() as u64)
            .expect("line")
            .bytes(line.as_bytes());
        writer.prefixed(0).expect("no receiver");
        writer.prefixed(0).expect("no link");
        writer.finish()
    }

    fn chat(world: &mut SimWorld, number: u64, line: &str) -> Option<GmChat> {
        world.gm_chat(
            &connection(number),
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &say(line),
            NOW,
        )
    }

    /// The system lines queued for the player `number`.
    fn answers(world: &mut SimWorld, number: u64) -> Vec<ChatMessage> {
        world
            .take_messages()
            .into_iter()
            .filter(|outgoing| outgoing.connection == connection(number))
            .flat_map(|outgoing| outgoing.envelopes)
            .flat_map(|bytes| {
                let envelope = Envelope::decode(&bytes).expect("envelope");
                let index = envelope.type_index().expect("type");
                match SocialPush::decode_args(index, &envelope.body[HEADER_LEN..]) {
                    Some(Ok(SocialPush::Chat(ChatPush::Batch(lines)))) => lines,
                    other => panic!("not a chat batch: {other:?}"),
                }
            })
            .collect()
    }

    /// The changed progress of the players `numbers`, from one periodic save.
    fn progress<const N: usize>(
        world: &mut SimWorld,
        numbers: [u64; N],
    ) -> [Option<nw_bus::PlayerProgression>; N] {
        let saves = world.positions();
        numbers.map(|number| {
            saves
                .iter()
                .find(|departure| departure.connection == connection(number))
                .and_then(|departure| departure.progression.clone())
        })
    }

    #[test]
    fn commands_read_with_and_without_a_named_player() {
        assert_eq!(read("hello"), None);
        assert_eq!(
            read(" !LEVEL 2 "),
            Some(Ok(Command::Change {
                target: Target::Sender,
                change: Change::Level(1),
            }))
        );
        assert_eq!(
            read("!give IronIngotT2 5 Bob"),
            Some(Ok(Command::Change {
                target: Target::Named("Bob".to_owned()),
                change: Change::Give {
                    item: "IronIngotT2".to_owned(),
                    quantity: 5,
                },
            }))
        );
        assert_eq!(
            read("!mastery all 20"),
            Some(Ok(Command::Change {
                target: Target::Sender,
                change: Change::MasteryRank {
                    mastery: None,
                    rank: 19,
                },
            }))
        );
        assert_eq!(
            read("!tp Bob"),
            Some(Ok(Command::TeleportToPlayer {
                name: "Bob".to_owned()
            }))
        );
        assert_eq!(
            read("!tp 1 2.5 3"),
            Some(Ok(Command::Teleport {
                target: Target::Sender,
                to: Point {
                    x: 1.0,
                    y: 2.5,
                    z: 3.0,
                },
            }))
        );
        assert_eq!(
            read("!event stop Siege"),
            Some(Ok(Command::StopEvent {
                event: "Siege".to_owned()
            }))
        );
    }

    #[test]
    fn a_bad_command_gives_its_usage_and_an_unknown_one_the_hint() {
        assert_eq!(
            read("!level 0"),
            Some(Err("Usage: !level <level> [player]".to_owned()))
        );
        assert_eq!(
            read("!god maybe"),
            Some(Err("Usage: !god <on|off> [player]".to_owned()))
        );
        assert_eq!(read("!fly"), Some(Err(UNKNOWN.to_owned())));
        assert_eq!(read("!"), Some(Err(UNKNOWN.to_owned())));
    }

    #[test]
    fn help_lists_every_command() {
        let mut world = world();
        assert_eq!(chat(&mut world, GM, "!help"), Some(GmChat::Answered));
        let lines = answers(&mut world, GM);
        assert_eq!(lines.len(), VERBS.len());
        assert!(lines.iter().all(|line| {
            line.channel == ChatChannel::System.wire() && line.sender_name.is_empty()
        }));
    }

    #[test]
    fn a_game_master_changes_itself_and_a_named_player() {
        let mut world = world();
        assert_eq!(chat(&mut world, GM, "!level 2"), Some(GmChat::Answered));
        assert_eq!(
            chat(&mut world, GM, &format!("!coins 9 player{PLAYER}")),
            Some(GmChat::Answered)
        );
        let lines = answers(&mut world, GM);
        let bodies: Vec<&str> = lines.iter().map(|line| line.body.as_str()).collect();
        assert_eq!(bodies, [DONE, DONE]);
        let [gm, player] = progress(&mut world, [GM, PLAYER]);
        assert_eq!(gm.expect("gm").experience, 150);
        assert_eq!(player.expect("player").coins, 9);
    }

    #[test]
    fn a_line_of_a_player_without_the_rank_is_chat() {
        let mut world = world();
        assert_eq!(chat(&mut world, PLAYER, "!level 2"), None);
        assert_eq!(progress(&mut world, [PLAYER]), [None]);
        assert!(answers(&mut world, PLAYER).is_empty());
    }

    #[test]
    fn a_game_masters_plain_line_is_chat() {
        let mut world = world();
        assert_eq!(chat(&mut world, GM, "hello all"), None);
    }

    #[test]
    fn a_kick_goes_to_the_server_loop_and_a_missing_player_is_not_here() {
        let mut world = world();
        assert_eq!(
            chat(&mut world, GM, &format!("!kick Player{PLAYER}")),
            Some(GmChat::Kick(PLAYER))
        );
        assert_eq!(chat(&mut world, GM, "!heal Nobody"), Some(GmChat::Answered));
        let lines = answers(&mut world, GM);
        assert_eq!(lines[0].body, NOT_HERE);
    }

    #[test]
    fn where_gives_the_senders_place() {
        let mut world = world();
        chat(&mut world, GM, "!where");
        let lines = answers(&mut world, GM);
        assert_eq!(lines[0].body, "0.0 10.0 50.0");
    }

    #[test]
    fn no_events_is_said() {
        assert_eq!(
            answer_lines(AdminReply::Events {
                running: Vec::new()
            }),
            [NO_EVENTS]
        );
    }
}
