// SPDX-License-Identifier: AGPL-3.0-or-later
//! The voice rooms: who is in each channel, shared by every connection.
//!
//! Observed (live 2026-11-07, `voice-server` log): the SDK joins a channel as an XMPP
//! multi-user chat room (XEP-0045): a `presence` to `<channel>@muc.<realm>/<issuer>.<character
//! id>` with `<m xmlns="urn:vivox:presence"/>` and, as the room password, the join token the
//! social service gave it (2934 → 3051). The room's answers follow XEP-0045 (Inferred: the
//! retail room server's answers are not in the captures): each occupant's presence to the one
//! that joins, the own one last with status 110, and the joiner's presence to everyone in the
//! room; a leave or a lost connection sends `unavailable`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::mpsc::UnboundedSender;

use crate::xmpp::STANZAS_NS;

/// The XML namespaces of a room presence.
const MUC_NS: &str = "http://jabber.org/protocol/muc";
const MUC_USER_NS: &str = "http://jabber.org/protocol/muc#user";
const VIVOX_PRESENCE_NS: &str = "urn:vivox:presence";
/// XEP-0045 status codes of a presence about the receiver itself: 110 (it is you) and 100 (the
/// room shows real addresses). The SDK starts audio only after a self presence with both
/// (`vivoxsdk.dll` `MucPresenceHandler` `0x1801266f0`: status `100` and `110` before
/// `DispatchCallAnswered`; local-data/findings/2026-11-07-vivox-invite-chan-add-re.md).
const SELF_STATUSES: [u16; 2] = [100, 110];

/// One connection in a room.
#[derive(Debug, Clone)]
pub struct Occupant {
    /// Its full address (`user@realm/resource`).
    pub jid: String,
    /// What it is sent.
    pub outbox: UnboundedSender<String>,
    /// The children of its `urn:vivox:presence` `m` element ([`vivox_status`]).
    pub status: String,
}

/// Every room, by its address (`<channel>@muc.<realm>`), and its occupants by nickname.
#[derive(Debug, Clone, Default)]
pub struct Rooms(Arc<Mutex<HashMap<String, HashMap<String, Occupant>>>>);

impl Rooms {
    /// `occupant` joins `room` as `nick` with the presence of stanza `id`: everyone already
    /// there gets its presence, and the presences it gets are returned (theirs, then its own).
    pub fn join(
        &self,
        room: &str,
        nick: &str,
        occupant: Occupant,
        id: Option<&str>,
    ) -> Vec<String> {
        let mut rooms = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let occupants = rooms.entry(room.to_owned()).or_default();
        let mut to_joiner = Vec::new();
        for (other_nick, other) in occupants.iter() {
            if other_nick == nick {
                continue;
            }
            let them = Seat {
                room,
                nick: other_nick,
                jid: &other.jid,
            };
            to_joiner.push(presence(
                them,
                &occupant.jid,
                Shown::Other,
                &other.status,
                None,
            ));
            let joiner = Seat {
                room,
                nick,
                jid: &occupant.jid,
            };
            // A closed outbox is a connection on its way out: its leave follows.
            let _ = other.outbox.send(presence(
                joiner,
                &other.jid,
                Shown::Other,
                &occupant.status,
                None,
            ));
        }
        let joiner = Seat {
            room,
            nick,
            jid: &occupant.jid,
        };
        to_joiner.push(presence(
            joiner,
            &occupant.jid,
            Shown::Own,
            &occupant.status,
            id,
        ));
        occupants.insert(nick.to_owned(), occupant);
        to_joiner
    }

    /// `nick` in `room` changes its status to `status` with the presence of stanza `id`:
    /// everyone else there gets its presence, and its own is returned; `None` when it is not
    /// there.
    pub fn update(&self, room: &str, nick: &str, status: &str, id: Option<&str>) -> Option<String> {
        let mut rooms = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let occupants = rooms.get_mut(room)?;
        let changed = occupants.get_mut(nick)?;
        changed.status = status.to_owned();
        let own = changed.jid.clone();
        let changer = Seat {
            room,
            nick,
            jid: &own,
        };
        for (other_nick, other) in occupants.iter() {
            if other_nick != nick {
                let _ =
                    other
                        .outbox
                        .send(presence(changer, &other.jid, Shown::Other, status, None));
            }
        }
        Some(presence(changer, &own, Shown::Own, status, id))
    }

    /// `nick` leaves `room` with the presence of stanza `id`: everyone left there gets its
    /// `unavailable`, which is returned for the one that left; `None` when it was not there.
    pub fn leave(&self, room: &str, nick: &str, id: Option<&str>) -> Option<String> {
        let mut rooms = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let occupants = rooms.get_mut(room)?;
        let gone = occupants.remove(nick)?;
        let leaver = Seat {
            room,
            nick,
            jid: &gone.jid,
        };
        for other in occupants.values() {
            let _ = other
                .outbox
                .send(presence(leaver, &other.jid, Shown::OtherLeft, "", None));
        }
        if occupants.is_empty() {
            rooms.remove(room);
        }
        Some(presence(leaver, &gone.jid, Shown::OwnLeft, "", id))
    }

    /// The connection of `jid` is gone: it leaves every room it is in.
    pub fn leave_everywhere(&self, jid: &str) {
        let seats: Vec<(String, String)> = {
            let rooms = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            rooms
                .iter()
                .flat_map(|(room, occupants)| {
                    occupants
                        .iter()
                        .filter(|(_, occupant)| occupant.jid == jid)
                        .map(|(nick, _)| (room.clone(), nick.clone()))
                })
                .collect()
        };
        for (room, nick) in seats {
            self.leave(&room, &nick, None);
        }
    }

    /// The nicknames in `room`, sorted.
    pub fn occupants(&self, room: &str) -> Vec<String> {
        let rooms = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut nicks: Vec<String> = rooms
            .get(room)
            .map(|occupants| occupants.keys().cloned().collect())
            .unwrap_or_default();
        nicks.sort();
        nicks
    }
}

/// Whom a room presence is about, as its receiver sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The receiver itself (statuses 100 and 110).
    Own,
    /// Another occupant.
    Other,
    /// The receiver itself, leaving.
    OwnLeft,
    /// Another occupant, leaving.
    OtherLeft,
}

/// The occupant a room presence is about.
#[derive(Debug, Clone, Copy)]
pub struct Seat<'a> {
    /// The room (`<channel>@muc.<realm>`).
    pub room: &'a str,
    /// Its nickname in the room.
    pub nick: &'a str,
    /// Its full address (`user@realm/resource`).
    pub jid: &'a str,
}

/// The presence of `seat` sent to `to`, about `shown`, with the `urn:vivox:presence` children
/// `status` and, when given, the stanza `id` of the client presence it answers.
///
/// The `item` carries the occupant's full address, as status 100 promises. The SDK makes the
/// participant from it (`vivoxsdk.dll` `MucPresenceHandler` `0x1801266f0`, address split
/// `FUN_18011aa80`: `sip:<user>@<realm>`, the resource as the URI tag) and marks the own one
/// "current user" only when that `sip:` address is its own (`IsCurrentUser` `0x18007e270`).
/// The game takes a channel as joined only on that own participant
/// (`NewWorld.exe` `VivoxClientApi::Channel::HandleEvent` `0x14118d2f0`); with no `jid` the SDK
/// made up a name and the game gave up after 60 s ("voice channel join failed").
///
/// The SDK sends its status change (`<audio/>`) with an `id` and waits 32000 ms for the
/// presence with that `id` (`vivoxsdk.dll` `SendUpdatedMucPresence` `0x180129660`, callback
/// `DispatchUpdatedMucPresenceResponse` `0x180125570`). Observed (live 2026-11-07, `Game.log`):
/// an answer without it ended every channel join 32 s later with error 5043 (Timeout).
pub fn presence(seat: Seat<'_>, to: &str, shown: Shown, status: &str, id: Option<&str>) -> String {
    let Seat { room, nick, jid } = seat;
    let left = matches!(shown, Shown::OwnLeft | Shown::OtherLeft);
    let kind = if left { " type='unavailable'" } else { "" };
    let id = id
        .map(|id| format!(" id='{}'", id.replace('\'', "&apos;")))
        .unwrap_or_default();
    let codes: String = if matches!(shown, Shown::Own | Shown::OwnLeft) {
        SELF_STATUSES
            .iter()
            .map(|code| format!("<status code='{code}'/>"))
            .collect()
    } else {
        String::new()
    };
    let role = if left { "none" } else { "participant" };
    format!(
        "<presence from='{room}/{nick}' to='{to}'{id}{kind}><m xmlns='{VIVOX_PRESENCE_NS}'>{status}</m>\
         <x xmlns='{MUC_USER_NS}'><item affiliation='none' role='{role}' jid='{jid}'/>{codes}</x></presence>"
    )
}

/// The `urn:vivox:presence` children of the client's presence `text` that the room passes on:
/// `audio`, `text` and `display_name`, the ones the SDK reads (`vivoxsdk.dll`
/// `MucPresenceHandler` `0x1801266f0`); nothing else of the client's XML goes to others.
pub fn vivox_status(text: &str) -> String {
    let Some(start) = text.find("<m ") else {
        return String::new();
    };
    let tag_end = start + text[start..].find('>').unwrap_or(0);
    if text[..tag_end].ends_with('/') {
        return String::new();
    }
    let body = &text[tag_end + 1..];
    let body = body.split("</m>").next().unwrap_or_default();
    let mut status = String::new();
    if body.contains("<audio") {
        status.push_str("<audio/>");
    }
    if body.contains("<text") {
        status.push_str("<text/>");
    }
    if let Some(name) = body
        .split_once("<display_name>")
        .and_then(|(_, rest)| rest.split_once("</display_name>"))
        .map(|(name, _)| name)
    {
        let name = name
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        status.push_str(&format!("<display_name>{name}</display_name>"));
    }
    status
}

/// The refusal of a join of `room` as `nick` sent to `to`: XEP-0045's `not-authorized`, the
/// answer to a wrong room password.
pub fn refused_presence(room: &str, nick: &str, to: &str) -> String {
    format!(
        "<presence from='{room}/{nick}' to='{to}' type='error'><x xmlns='{MUC_NS}'/>\
         <error type='auth'><not-authorized xmlns='{STANZAS_NS}'/></error></presence>"
    )
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[test]
    fn only_the_known_status_children_pass_on() {
        assert_eq!(
            vivox_status("<presence><m xmlns='urn:vivox:presence'/></presence>"),
            ""
        );
        assert_eq!(
            vivox_status(
                "<presence><m xmlns=\"urn:vivox:presence\"><audio/><evil/><display_name>A&lt;b\
                 </display_name></m></presence>"
            ),
            "<audio/><display_name>A&amp;lt;b</display_name>"
        );
    }

    const ROOM: &str = "confctl-d-issuer.018012_0@muc.game.example.org";

    #[test]
    fn a_joiner_sees_the_room_and_the_room_sees_it_come_and_go() {
        let rooms = Rooms::default();
        let (ann_out, mut ann_in) = unbounded_channel();
        let ann = Occupant {
            jid: "ann@game.example.org/voice".to_owned(),
            outbox: ann_out,
            status: String::new(),
        };
        let first = rooms.join(ROOM, "ann", ann, None);
        assert_eq!(first.len(), 1, "an empty room: the own presence only");
        assert!(first[0].contains("code='100'") && first[0].contains("code='110'"));
        assert!(
            first[0].contains("jid='ann@game.example.org/voice'"),
            "the SDK knows itself only by its real address"
        );
        let (bob_out, _bob_in) = unbounded_channel();
        let bob = Occupant {
            jid: "bob@game.example.org/voice".to_owned(),
            outbox: bob_out,
            status: String::new(),
        };
        let second = rooms.join(ROOM, "bob", bob, None);
        assert_eq!(second.len(), 2);
        assert!(second[0].contains(&format!("from='{ROOM}/ann'")));
        assert!(second[0].contains("jid='ann@game.example.org/voice'"));
        assert!(second[1].contains("code='110'"));
        let told = ann_in.try_recv().expect("ann hears bob");
        assert!(told.contains(&format!("from='{ROOM}/bob'")) && !told.contains("110"));
        assert_eq!(rooms.occupants(ROOM), ["ann", "bob"]);
        let own = rooms
            .update(ROOM, "bob", "<audio/>", Some("u1"))
            .expect("bob is there");
        assert!(own.contains("<audio/>") && own.contains("code='110'"));
        assert!(
            own.contains(" id='u1'"),
            "the SDK waits for its own stanza id"
        );
        let heard = ann_in.try_recv().expect("ann hears bob's audio");
        assert!(heard.contains("<audio/>") && !heard.contains("110"));
        assert!(!heard.contains(" id="), "others do not get the stanza id");
        assert!(rooms.update(ROOM, "cid", "<audio/>", None).is_none());
        rooms.leave_everywhere("bob@game.example.org/voice");
        let gone = ann_in.try_recv().expect("ann hears bob go");
        assert!(gone.contains("type='unavailable'"));
        assert_eq!(rooms.occupants(ROOM), ["ann"]);
        let left = rooms.leave(ROOM, "ann", None).expect("ann was there");
        assert!(left.contains("type='unavailable'") && left.contains("110"));
        assert!(rooms.occupants(ROOM).is_empty());
        assert!(rooms.leave(ROOM, "ann", None).is_none());
    }
}
