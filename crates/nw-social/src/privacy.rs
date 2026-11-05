// SPDX-License-Identifier: AGPL-3.0-or-later
//! The privacy settings of each account (global migration 0006), which the player sets through
//! the client's privacy settings service (`edge-http`), and what they change here:
//!
//! | Setting | What it limits |
//! |---|---|
//! | public or group text chat | the area, emote, global, help, trading, recruitment, faction, company, consul and group lines it sends and hears |
//! | private text chat | the whispers it sends and hears |
//! | friend request | the friend invites it gets (one it sent may still be accepted) |
//! | online status | who gets its online notice: friends, company and group members (Everyone), friends (Friends), nobody (OnlyMe) |
//! | voice | the voice tokens it gets: none (NoOne), its group channel only (Friends) |
//!
//! A line passes when the settings of both players allow the other. `Friends` allows the
//! player's friends only. Inferred: the client sends the setting values only; what retail did
//! with each one is Unknown, so each limit is the plain reading of its name. A positional voice
//! channel holds whoever is near, so `Friends` gets no positional token (Inferred).

use std::collections::HashMap;

use nw_db::{
    PRIVACY_EVERYONE, PRIVACY_FRIEND_REQUESTS, PRIVACY_FRIENDS, PRIVACY_NO_ONE,
    PRIVACY_ONLINE_STATUS, PRIVACY_ONLY_ME, PRIVACY_PRIVATE_CHAT, PRIVACY_PUBLIC_CHAT,
    PRIVACY_VOICE,
};
use nw_messages::social::ChatChannel;
use uuid::Uuid;

use crate::world::SocialWorld;

/// Who a privacy setting lets in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Audience {
    /// Every player (the retail default).
    #[default]
    Everyone,
    /// The player's friends.
    Friends,
    /// No other player (`NoOne`, and `OnlyMe` of the online status).
    NoOne,
}

impl Audience {
    /// The audience of setting value `value`; `None` for a value the service does not know.
    pub fn of_value(value: &str) -> Option<Self> {
        match value {
            PRIVACY_EVERYONE => Some(Self::Everyone),
            PRIVACY_FRIENDS => Some(Self::Friends),
            PRIVACY_NO_ONE | PRIVACY_ONLY_ME => Some(Self::NoOne),
            _ => None,
        }
    }
}

/// The privacy settings of one account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Privacy {
    /// Public and group text chat.
    pub public_chat: Audience,
    /// Private text chat (whispers).
    pub private_chat: Audience,
    /// Friend requests to the player.
    pub friend_requests: Audience,
    /// The player's online status.
    pub online_status: Audience,
    /// Voice chat.
    pub voice: Audience,
}

impl Privacy {
    /// Takes setting `alias` with value `value`; false for an unknown alias or value.
    pub fn set(&mut self, alias: &str, value: &str) -> bool {
        let Some(audience) = Audience::of_value(value) else {
            return false;
        };
        let slot = match alias {
            PRIVACY_PUBLIC_CHAT => &mut self.public_chat,
            PRIVACY_PRIVATE_CHAT => &mut self.private_chat,
            PRIVACY_FRIEND_REQUESTS => &mut self.friend_requests,
            PRIVACY_ONLINE_STATUS => &mut self.online_status,
            PRIVACY_VOICE => &mut self.voice,
            _ => return false,
        };
        *slot = audience;
        true
    }
}

/// Which setting a chat channel falls under.
fn chat_audience(privacy: &Privacy, channel: ChatChannel) -> Audience {
    if channel == ChatChannel::Whisper {
        privacy.private_chat
    } else {
        privacy.public_chat
    }
}

impl SocialWorld {
    /// Account `account` set privacy setting `alias` to `value` (the global database). An
    /// unknown alias or value changes nothing and is logged.
    pub fn set_privacy(&mut self, account: Uuid, alias: &str, value: &str) {
        if !self.privacy.entry(account).or_default().set(alias, value) {
            tracing::warn!(%account, alias, value, "unknown privacy setting");
        }
    }

    /// The privacy settings of the account of `character`.
    pub(crate) fn privacy_of(&self, character: Uuid) -> Privacy {
        self.people
            .get(&character)
            .and_then(|person| person.account)
            .and_then(|account| self.privacy.get(&account))
            .copied()
            .unwrap_or_default()
    }

    /// Whether `audience` of `owner` lets `other` in.
    pub(crate) fn lets_in(&self, owner: Uuid, audience: Audience, other: Uuid) -> bool {
        match audience {
            Audience::Everyone => true,
            Audience::Friends => self
                .people
                .get(&owner)
                .is_some_and(|person| person.friends.contains(&other)),
            Audience::NoOne => false,
        }
    }

    /// Whether a line of `sender` on `channel` reaches `listener`: the settings of both allow
    /// the other. The sender's own echo always passes.
    pub(crate) fn may_chat(&self, sender: Uuid, listener: Uuid, channel: ChatChannel) -> bool {
        sender == listener
            || (self.lets_in(
                sender,
                chat_audience(&self.privacy_of(sender), channel),
                listener,
            ) && self.lets_in(
                listener,
                chat_audience(&self.privacy_of(listener), channel),
                sender,
            ))
    }

    /// Whether `watcher` gets the online notice of `character`.
    pub(crate) fn sees_online(&self, character: Uuid, watcher: Uuid) -> bool {
        self.lets_in(character, self.privacy_of(character).online_status, watcher)
    }
}

/// The privacy settings of every account that set one.
pub(crate) type PrivacyByAccount = HashMap<Uuid, Privacy>;

#[cfg(test)]
mod tests {
    use nw_messages::social::chat::{
        RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX, SEND_CHAT_MESSAGE_TYPE_INDEX,
    };
    use nw_messages::social::players::{
        FRIEND_STATUS_CHANGE_TYPE_INDEX, ONLINE_STATE_CHANGE_TYPE_INDEX,
    };

    use super::*;
    use crate::world::tests::{NOW_MS, call, character, join, pushes, text, world};

    /// The wire value of a friend invite (`FriendRequestTypes` `Invite`).
    const INVITE: u32 = 1;

    /// The account of test character `number` (`world()` gives character n the account n + 1000).
    fn account(number: u128) -> Uuid {
        Uuid::from_u128(number + 1000)
    }

    /// Milliseconds between two test lines: well past the chat rate limit.
    const LINE_GAP_MS: u64 = 60_000;

    fn global_line(world: &mut SocialWorld, from: u128, line_number: u64) -> Vec<Uuid> {
        let mut line = ChatChannel::Global.wire().to_be_bytes().to_vec();
        line.extend(text("hi"));
        line.extend(text(""));
        line.push(0);
        let effects = call(
            world,
            from,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line,
            NOW_MS + line_number * LINE_GAP_MS,
        );
        let mut heard: Vec<Uuid> = pushes(&effects, RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX)
            .into_iter()
            .flat_map(|(to, _)| to)
            .collect();
        heard.sort();
        heard
    }

    #[test]
    fn settings_read_their_retail_values() {
        let mut privacy = Privacy::default();
        assert_eq!(
            privacy.public_chat,
            Audience::Everyone,
            "the retail default"
        );
        assert!(privacy.set(PRIVACY_ONLINE_STATUS, PRIVACY_ONLY_ME));
        assert_eq!(privacy.online_status, Audience::NoOne);
        assert!(privacy.set(PRIVACY_VOICE, PRIVACY_FRIENDS));
        assert_eq!(privacy.voice, Audience::Friends);
        assert!(!privacy.set("who-can-fly", PRIVACY_NO_ONE));
        assert!(!privacy.set(PRIVACY_VOICE, "Martians"));
        assert_eq!(
            privacy.voice,
            Audience::Friends,
            "a bad value changes nothing"
        );
    }

    #[test]
    fn a_character_made_since_the_start_gets_its_account_and_settings() {
        let mut world = world();
        let effects = join(&mut world, 9, "Dee", "sim-a");
        assert!(effects.iter().any(|effect| matches!(
            effect,
            crate::Effect::FindAccount { character: found } if *found == character(9)
        )));
        assert_eq!(world.privacy_of(character(9)), Privacy::default());
        world.set_privacy(account(9), PRIVACY_VOICE, PRIVACY_NO_ONE);
        world.account_found(character(9), account(9));
        assert_eq!(world.privacy_of(character(9)).voice, Audience::NoOne);
    }

    #[test]
    fn a_closed_public_chat_neither_sends_nor_hears() {
        let mut world = world();
        for (number, name) in [(1, "Ann"), (2, "Bob"), (3, "Cid")] {
            join(&mut world, number, name, "sim-a");
        }
        assert_eq!(global_line(&mut world, 1, 0).len(), 3);
        world.set_privacy(account(2), PRIVACY_PUBLIC_CHAT, PRIVACY_NO_ONE);
        assert_eq!(global_line(&mut world, 1, 1), [character(1), character(3)]);
        assert_eq!(
            global_line(&mut world, 2, 2),
            [character(2)],
            "only its own echo"
        );
    }

    #[test]
    fn friend_requests_and_the_online_status_follow_the_settings() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        world.set_privacy(account(2), PRIVACY_FRIEND_REQUESTS, PRIVACY_NO_ONE);
        let mut args = INVITE.to_be_bytes().to_vec();
        args.extend(text(&character(2).hyphenated().to_string()));
        call(
            &mut world,
            1,
            "sim-a",
            FRIEND_STATUS_CHANGE_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        assert!(
            world.people[&character(2)].invites_in.is_empty(),
            "Bob takes no friend requests"
        );
        for (one, other) in [(1, 2), (2, 1)] {
            world
                .people
                .get_mut(&character(one))
                .expect("person")
                .friends
                .insert(character(other));
        }
        world.set_privacy(account(2), PRIVACY_ONLINE_STATUS, PRIVACY_ONLY_ME);
        let effects = join(&mut world, 2, "Bob", "sim-a");
        assert!(pushes(&effects, ONLINE_STATE_CHANGE_TYPE_INDEX).is_empty());
    }
}
