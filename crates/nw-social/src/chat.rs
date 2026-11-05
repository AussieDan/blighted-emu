// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chat: who hears a line, the profanity filter per listener, the rate limit, mutes.
//!
//! Who hears a line (sender included: retail echoed the sender's own global line and whisper):
//!
//! | Channel | Listeners |
//! |---|---|
//! | Area, Emote | the online players of the sender's level and innermost territory (retail sent these as 4118) |
//! | Global, Recruitment, Help, Trading | every online player of the world |
//! | Faction | the online players of the sender's faction (none for a sender in no faction) |
//! | Guild | the online members of the sender's company |
//! | Guild_Officer (consul chat) | its online governor and consuls |
//! | Group | the online members of the sender's group |
//! | Whisper | the online player named by the line, and the sender |
//!
//! Other channels (announcements, system, war, raid, lobby, game mode) are the server's own;
//! a client line on them is refused. A listener who blocks or mutes the sender does not hear it,
//! nor one whose privacy settings, or the sender's, close the channel to the other
//! ([`crate::privacy`]).

use nw_messages::social::chat::{
    MESSAGE_RESERVED, MESSAGE_TRAILER, MUTE_FOREVER, MuteNotice, SENDER_KIND,
};
use nw_messages::social::{ChatCall, ChatChannel, ChatMessage, ChatPush, SocialPush};
use uuid::Uuid;

use crate::companies::RANK_CONSUL;
use crate::effects::StoreOp;
use crate::world::{METRIC_CALLS, REFUSED, RESULT_LABEL, SocialWorld, TAKEN};

/// The platform of a sender without platform data (the player data's `-1`): this server has
/// no platform accounts.
const NO_PLATFORM: u32 = u32::MAX;
/// The flag of a player's line (699 of 701 retail player lines).
const PLAYER_LINE_FLAG: bool = true;
/// The mute flag of every retail 1866 and 1629.
const MUTE_FLAG: u8 = 0;
/// Metric: chat lines by `result`.
const METRIC_LINES: &str = "social_chat_lines_total";
/// A line over the rate limit.
const THROTTLED: &str = "throttled";

impl SocialWorld {
    pub(crate) fn chat_call(&mut self, character: Uuid, call: ChatCall, now_ms: u64) {
        match call {
            ChatCall::Send {
                channel,
                body,
                recipient_name,
                links,
            } => self.say(character, channel, body, &recipient_name, links, now_ms),
            ChatCall::SetProfanityFilter { enabled } => {
                if let Some(person) = self.people.get_mut(&character) {
                    person.settings.profanity_filter = enabled;
                }
                self.store_settings(character);
            }
            ChatCall::SetLanguage { language } => {
                if let Some(person) = self.people.get_mut(&character) {
                    person.settings.language = Some(language);
                }
                self.store_settings(character);
            }
            ChatCall::SetMute {
                character: muted, ..
            } => {
                self.set_mute(character, &muted, true, false);
            }
            ChatCall::ClearMute {
                character: muted, ..
            } => {
                self.set_mute(character, &muted, false, false);
            }
        }
    }

    /// Sets or clears (`on`) the chat mute or the block (`block`) of `character` on the character
    /// of UUID text `other`, stores it, and tells the client (1866 or 1629, as retail).
    pub(crate) fn set_mute(&mut self, character: Uuid, other: &str, on: bool, block: bool) {
        let Some(target) = self.known(other).filter(|target| *target != character) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(person) = self.people.get_mut(&character) else {
            return;
        };
        let list = if block {
            &mut person.blocks
        } else {
            &mut person.mutes
        };
        let changed = if on {
            list.insert(target)
        } else {
            list.remove(&target)
        };
        let kind = if block {
            nw_db::LinkKind::Block
        } else {
            nw_db::LinkKind::Mute
        };
        if changed {
            self.store(if on {
                StoreOp::AddLink {
                    character,
                    other: target,
                    kind,
                }
            } else {
                StoreOp::RemoveLink {
                    character,
                    other: target,
                    kind,
                }
            });
        }
        let notice = MuteNotice {
            character: target.hyphenated().to_string(),
            duration: MUTE_FOREVER,
            flag: MUTE_FLAG,
            social_block: block,
        };
        let push = if on {
            ChatPush::MuteSet(notice)
        } else {
            ChatPush::MuteCleared(notice)
        };
        self.push(&[character], SocialPush::Chat(push));
    }

    fn say(
        &mut self,
        sender: Uuid,
        channel: u32,
        body: String,
        recipient_name: &str,
        links: Vec<nw_messages::social::ItemLink>,
        now_ms: u64,
    ) {
        let Some(kind) = ChatChannel::from_wire(channel) else {
            metrics::counter!(METRIC_LINES, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        if body.trim().is_empty() {
            metrics::counter!(METRIC_LINES, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        let Some(listeners) = self.listeners(sender, kind, recipient_name) else {
            metrics::counter!(METRIC_LINES, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(person) = self.people.get_mut(&sender) else {
            return;
        };
        if !person.throttle.allow(now_ms) {
            metrics::counter!(METRIC_LINES, RESULT_LABEL => THROTTLED).increment(1);
            return;
        }
        let sender_name = person.name.clone();
        let listeners: Vec<Uuid> = listeners
            .into_iter()
            .filter(|listener| {
                self.people.get(listener).is_some_and(|heard| {
                    !heard.blocks.contains(&sender) && !heard.mutes.contains(&sender)
                }) && self.may_chat(sender, *listener, kind)
            })
            .collect();
        let filtered = self.filter.filter(&body).unwrap_or_default();
        let message = ChatMessage {
            sender_id: sender.hyphenated().to_string(),
            sender_name,
            sender_kind: SENDER_KIND,
            recipient_name: if kind == ChatChannel::Whisper {
                recipient_name.to_owned()
            } else {
                String::new()
            },
            channel,
            body,
            filtered_body: filtered,
            reserved: MESSAGE_RESERVED,
            links,
            platform: NO_PLATFORM,
            platform_id: String::new(),
            flag: PLAYER_LINE_FLAG,
            trailer: MESSAGE_TRAILER,
        };
        let (filtering, raw): (Vec<Uuid>, Vec<Uuid>) =
            listeners.into_iter().partition(|listener| {
                self.people
                    .get(listener)
                    .is_some_and(|heard| heard.settings.profanity_filter)
            });
        let starred = (!message.filtered_body.is_empty()).then(|| ChatMessage {
            body: String::new(),
            ..message.clone()
        });
        let local = matches!(kind, ChatChannel::Area | ChatChannel::Emote);
        let wrap = |line: ChatMessage| {
            SocialPush::Chat(if local {
                ChatPush::Message(line)
            } else {
                ChatPush::Batch(vec![line])
            })
        };
        match starred {
            Some(starred) => {
                self.push(&filtering, wrap(starred));
                self.push(&raw, wrap(message));
            }
            None => {
                let everyone: Vec<Uuid> = filtering.into_iter().chain(raw).collect();
                self.push(&everyone, wrap(message));
            }
        }
        metrics::counter!(METRIC_LINES, RESULT_LABEL => TAKEN).increment(1);
    }

    /// The listeners of a line of `sender` on `channel` (see the module text); `None` when the
    /// sender may not say it there.
    fn listeners(
        &self,
        sender: Uuid,
        channel: ChatChannel,
        recipient_name: &str,
    ) -> Option<Vec<Uuid>> {
        let person = self.people.get(&sender)?;
        let here = person.online.as_ref()?;
        let online = || {
            self.people.iter().filter_map(|(character, person)| {
                person.online.as_ref().map(|online| (*character, online))
            })
        };
        let listeners = match channel {
            ChatChannel::Area | ChatChannel::Emote => online()
                .filter(|(_, other)| {
                    other.level_name == here.level_name
                        && other.territories.last() == here.territories.last()
                })
                .map(|(character, _)| character)
                .collect(),
            ChatChannel::Global
            | ChatChannel::Recruitment
            | ChatChannel::Help
            | ChatChannel::Trading => online().map(|(character, _)| character).collect(),
            ChatChannel::Faction if here.faction != 0 => online()
                .filter(|(_, other)| other.faction == here.faction)
                .map(|(character, _)| character)
                .collect(),
            ChatChannel::Guild | ChatChannel::GuildOfficer => {
                let (company, rank) = person.company?;
                let officer = channel == ChatChannel::GuildOfficer;
                if officer && rank > RANK_CONSUL {
                    return None;
                }
                self.companies
                    .get(&company)?
                    .members
                    .iter()
                    .copied()
                    .filter(|member| {
                        !officer
                            || self
                                .people
                                .get(member)
                                .and_then(|member| member.company)
                                .is_some_and(|(_, rank)| rank <= RANK_CONSUL)
                    })
                    .collect()
            }
            ChatChannel::Group => self.groups.get(&person.group?)?.members.clone(),
            ChatChannel::Whisper => {
                let target = self.online_named(recipient_name)?;
                if self
                    .people
                    .get(&target)
                    .is_some_and(|heard| heard.blocks.contains(&sender))
                {
                    return None;
                }
                vec![target, sender]
            }
            _ => return None,
        };
        Some(listeners)
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::social::chat::{
        CHAT_MUTE_CLEARED_TYPE_INDEX, CHAT_MUTE_SET_TYPE_INDEX,
        RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX, RECEIVE_CHAT_MESSAGE_TYPE_INDEX,
        SEND_CHAT_MESSAGE_TYPE_INDEX, SET_CHAT_MUTE_TYPE_INDEX, SET_PROFANITY_FILTER_TYPE_INDEX,
    };
    use nw_messages::social::players::SET_SOCIAL_BLOCK_TYPE_INDEX;

    use super::*;
    use crate::throttle::MIN_GAP_MS;
    use crate::world::tests::{NOW_MS, call, character, join, pushes, text, world};

    fn line(channel: ChatChannel, body: &str, recipient: &str) -> Vec<u8> {
        let mut args = channel.wire().to_be_bytes().to_vec();
        args.extend(text(body));
        args.extend(text(recipient));
        args.push(0);
        args
    }

    fn lines(effects: &[crate::Effect], type_index: u64) -> Vec<(Vec<Uuid>, ChatMessage)> {
        pushes(effects, type_index)
            .into_iter()
            .flat_map(|(recipients, push)| match push {
                SocialPush::Chat(ChatPush::Batch(lines)) => lines
                    .into_iter()
                    .map(|line| (recipients.clone(), line))
                    .collect::<Vec<_>>(),
                SocialPush::Chat(ChatPush::Message(line)) => vec![(recipients, line)],
                other => panic!("a chat line: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_global_line_reaches_every_online_player_and_its_sender() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-b");
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line(ChatChannel::Global, "hello", ""),
            NOW_MS,
        );
        let heard = lines(&effects, RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX);
        assert_eq!(heard.len(), 2, "one delivery per sim");
        let mut recipients: Vec<Uuid> = heard.iter().flat_map(|(to, _)| to.clone()).collect();
        recipients.sort();
        assert_eq!(recipients, [character(1), character(2)]);
        assert_eq!(heard[0].1.body, "hello");
        assert_eq!(heard[0].1.sender_name, "Ann");
        assert!(heard[0].1.filtered_body.is_empty());
    }

    #[test]
    fn a_listed_word_is_starred_for_listeners_with_the_filter_on() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        call(
            &mut world,
            2,
            "sim-a",
            SET_PROFANITY_FILTER_TYPE_INDEX,
            &[0],
            NOW_MS,
        );
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line(ChatChannel::Area, "a bad day", ""),
            NOW_MS,
        );
        let heard = lines(&effects, RECEIVE_CHAT_MESSAGE_TYPE_INDEX);
        let for_ann = heard
            .iter()
            .find(|(to, _)| to.contains(&character(1)))
            .expect("Ann");
        let for_bob = heard
            .iter()
            .find(|(to, _)| to.contains(&character(2)))
            .expect("Bob");
        assert_eq!(
            (for_ann.1.body.as_str(), for_ann.1.filtered_body.as_str()),
            ("", "a *** day")
        );
        assert_eq!(
            (for_bob.1.body.as_str(), for_bob.1.filtered_body.as_str()),
            ("a bad day", "a *** day")
        );
    }

    #[test]
    fn a_whisper_reaches_its_target_and_not_a_blocker() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        join(&mut world, 3, "Cid", "sim-a");
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line(ChatChannel::Whisper, "psst", "bob"),
            NOW_MS,
        );
        let heard = lines(&effects, RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX);
        assert_eq!(heard[0].0, [character(1), character(2)]);
        assert_eq!(heard[0].1.recipient_name, "bob");
        let mut block = text(&character(1).hyphenated().to_string());
        let blocked = call(
            &mut world,
            2,
            "sim-a",
            SET_SOCIAL_BLOCK_TYPE_INDEX,
            &block,
            NOW_MS,
        );
        assert_eq!(pushes(&blocked, CHAT_MUTE_SET_TYPE_INDEX).len(), 1);
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line(ChatChannel::Whisper, "again", "Bob"),
            NOW_MS + MIN_GAP_MS,
        );
        assert!(lines(&effects, RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX).is_empty());
        block.push(0);
        let muted = call(
            &mut world,
            3,
            "sim-a",
            SET_CHAT_MUTE_TYPE_INDEX,
            &block,
            NOW_MS,
        );
        let SocialPush::Chat(ChatPush::MuteSet(notice)) =
            &pushes(&muted, CHAT_MUTE_SET_TYPE_INDEX)[0].1
        else {
            panic!("a mute notice");
        };
        assert!(!notice.social_block);
        let cleared = call(
            &mut world,
            3,
            "sim-a",
            nw_messages::social::chat::CLEAR_CHAT_MUTE_TYPE_INDEX,
            &block,
            NOW_MS,
        );
        assert_eq!(pushes(&cleared, CHAT_MUTE_CLEARED_TYPE_INDEX).len(), 1);
    }

    #[test]
    fn lines_over_the_rate_limit_and_on_server_channels_are_dropped() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        let say = |world: &mut crate::SocialWorld, channel, at| {
            call(
                world,
                1,
                "sim-a",
                SEND_CHAT_MESSAGE_TYPE_INDEX,
                &line(channel, "x", ""),
                at,
            )
        };
        assert!(!say(&mut world, ChatChannel::Global, NOW_MS).is_empty());
        assert!(
            say(&mut world, ChatChannel::Global, NOW_MS + 1).is_empty(),
            "too soon"
        );
        assert!(say(&mut world, ChatChannel::System, NOW_MS + MIN_GAP_MS).is_empty());
        assert!(
            say(&mut world, ChatChannel::Guild, NOW_MS + MIN_GAP_MS).is_empty(),
            "no company"
        );
        assert!(
            say(&mut world, ChatChannel::Group, NOW_MS + MIN_GAP_MS).is_empty(),
            "no group"
        );
    }
}
