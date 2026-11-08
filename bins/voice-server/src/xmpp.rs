// SPDX-License-Identifier: AGPL-3.0-or-later
//! The XMPP sign-in of the game's voice SDK, one connection at a time.
//!
//! Observed (live 2026-11-07, `voice-server` log): after the prelogin, the SDK (`ua`
//! `Vivox-SDK-5.23.1.33742.f513647dW_x64`) opens a stream to the realm and, offered SASL `PLAIN`,
//! signs in with authentication id `.<issuer>.<character id>+B.` and, as the password, the login
//! token the social service gave it (2077 → 3051). With no answer it closed the stream after
//! 2 minutes 17 seconds and opened a new one.
//!
//! The steps after the sign-in follow RFC 6120 (Inferred: the SDK names its server an XMPP
//! server): a successful `PLAIN` sign-in, the stream restart with resource binding, and the
//! answers to pings (`XmppPingIntervalSeconds`, 10 s in the prelogin).
//!
//! Observed in the same log, right after the session: a roster get (`jabber:iq:roster`), a block
//! list get (XEP-0191), a stream management enable (XEP-0198, not offered), then the channel
//! joins ([`crate::rooms`]). The roster and the block list are answered empty (voice keeps no
//! contacts: the social service has them), the enable gets XEP-0198's `failed`. Any other `iq`
//! gets RFC 6120's `feature-not-implemented`, and everything is logged, so the next stages can be
//! built from what the SDK asks.

use std::collections::HashSet;

use data_encoding::BASE64;
use nw_social::voice::verified_claims;

use crate::rooms::{refused_presence, vivox_status};

/// The start of an XMPP stream opening.
const STREAM_OPENING: &str = "<stream:stream";
/// The end of a stream.
const STREAM_END: &str = "</stream:stream>";
/// The start of a SASL sign-in and the mechanism the server offers.
const AUTH_START: &str = "<auth ";
const PLAIN_MECHANISM: &str = "PLAIN";
/// What the SDK adds to the token's address in its account name, the authentication id
/// (`.<issuer>.<id>+B.`). Observed (frida trace 2026-11-08, `vivoxsdk.dll` `IsCurrentUser`
/// `0x18007e270`): the SDK's own address is `sip:.<issuer>.<id>+B.@<realm>`, so a room member
/// is "me" only with that account in its jid; a retail login (`Game.log`) also ends in `+B.`.
const SDK_ID_TAG: &str = "+B.";
/// The id of every stream: one per connection, and no client reads it back.
const STREAM_ID: &str = "blighted-voice";
/// The resource a bind without one gets.
const DEFAULT_RESOURCE: &str = "voice";
/// Separator of the `PLAIN` message: authorization id, authentication id, password.
const PLAIN_SEPARATOR: char = '\0';
/// The parts of a `PLAIN` message.
const PLAIN_PARTS: usize = 3;
/// The XML namespaces of the answers.
const SASL_NS: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
const BIND_NS: &str = "urn:ietf:params:xml:ns:xmpp-bind";
const SESSION_NS: &str = "urn:ietf:params:xml:ns:xmpp-session";
pub(crate) const STANZAS_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const PING_NS: &str = "urn:xmpp:ping";
const ROSTER_NS: &str = "jabber:iq:roster";
const BLOCKING_NS: &str = "urn:xmpp:blocking";
const STREAM_MANAGEMENT_NS: &str = "urn:xmpp:sm:3";
/// The start of a stream management enable and of a presence.
const SM_ENABLE_START: &str = "<enable";
const PRESENCE_START: &str = "<presence";
/// The call setup of the SDK: `invite` and `chan_add` in `urn:vivox:muc`
/// ([`crate::media`]).
const VIVOX_MUC_NS: &str = "urn:vivox:muc";
const INVITE_START: &str = "<invite";
const CHAN_ADD_START: &str = "<chan_add";
/// The `chan_add` answer codes: done, and no call to add the room to (the SDK then sends a new
/// invite: `vivoxsdk.dll` `0x18011c5b0`).
pub(crate) const CHAN_ADD_DONE: u16 = 200;
pub(crate) const CHAN_ADD_NO_CALL: u16 = 404;
/// The `chan_add` answer text the SDK answers with a new invite.
pub(crate) const NO_CALL_MESSAGE: &str = "Participant not found in any channel";
/// The `type` of a presence that leaves.
const UNAVAILABLE: &str = "unavailable";

/// What one connection checks the sign-in against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    /// The SIP realm (the stream's `from`, the domain of every address).
    pub realm: String,
    /// The token issuer of the social service's `[voice]`.
    pub issuer: String,
    /// The key that signs the tokens.
    pub key: Vec<u8>,
}

/// Where a connection is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// Waiting for the first stream opening.
    Opening,
    /// The stream is open: waiting for the sign-in.
    SigningIn,
    /// Signed in as `user` (the SDK's account name, its address without the realm): waiting
    /// for the restarted stream.
    Restarting { user: String },
    /// The restarted stream is open; `jid` once bound.
    Open { user: String, jid: Option<String> },
}

/// The answer to some client text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    /// What to send back, in order.
    pub replies: Vec<String>,
    /// The connection ends after the replies.
    pub close: bool,
    /// The user that signed in, when this text did it.
    pub signed_in: Option<String>,
    /// A room this text joins or leaves.
    pub room: Option<RoomChange>,
    /// A call setup this text asks for.
    pub call: Option<CallRequest>,
}

/// A call setup of the SDK in a room it joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRequest {
    /// `invite`: open the call with the SDP `offer` in `room`.
    Invite {
        /// The `iq` id the answer repeats.
        id: String,
        /// The room (`<channel>@muc.<realm>`).
        room: String,
        /// The SDP offer.
        offer: String,
    },
    /// `chan_add`: add `room` to the open call.
    ChanAdd {
        /// The `iq` id the answer repeats.
        id: String,
        /// The room.
        room: String,
    },
}

/// A room join or leave of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomChange {
    /// Join `room` (`<channel>@muc.<realm>`) as `nick` with the full address `jid`.
    Join {
        /// The room.
        room: String,
        /// The nickname.
        nick: String,
        /// The connection's bound address.
        jid: String,
        /// The status children ([`crate::rooms::vivox_status`]).
        status: String,
        /// The client presence's stanza id, which the own presence answer carries.
        id: Option<String>,
    },
    /// Change the status of `nick` in `room` to `status` (the `urn:vivox:presence` children).
    Update {
        /// The room.
        room: String,
        /// The nickname.
        nick: String,
        /// The status children ([`crate::rooms::vivox_status`]).
        status: String,
        /// The client presence's stanza id, which the own presence answer carries.
        id: Option<String>,
    },
    /// Leave `room` as `nick`.
    Leave {
        /// The room.
        room: String,
        /// The nickname.
        nick: String,
        /// The client presence's stanza id, which the own presence answer carries.
        id: Option<String>,
    },
}

/// One connection's sign-in.
#[derive(Debug, Clone)]
pub struct Session {
    sign_in: SignIn,
    phase: Phase,
    /// The rooms this connection joined: a presence to one is a status change.
    joined: HashSet<String>,
}

impl Session {
    /// A new connection checked against `sign_in`.
    pub fn new(sign_in: SignIn) -> Self {
        Self {
            sign_in,
            phase: Phase::Opening,
            joined: HashSet::new(),
        }
    }

    /// The bound full address of the connection, once bound.
    pub fn jid(&self) -> Option<&str> {
        match &self.phase {
            Phase::Open { jid, .. } => jid.as_deref(),
            _ => None,
        }
    }

    /// The answer to the client text `text` (one read) at Unix seconds `now_s`.
    pub fn on_text(&mut self, text: &str, now_s: u64) -> Answer {
        let mut answer = Answer::default();
        if text.contains(STREAM_END) {
            answer.replies.push(STREAM_END.to_owned());
            answer.close = true;
            return answer;
        }
        match std::mem::replace(&mut self.phase, Phase::Opening) {
            Phase::Opening if text.contains(STREAM_OPENING) => {
                answer.replies.push(self.stream_header(&sasl_features()));
                self.phase = Phase::SigningIn;
            }
            Phase::SigningIn if text.contains(AUTH_START) => {
                match self.signed_in_user(text, now_s) {
                    Some(user) => {
                        answer.replies.push(format!("<success xmlns='{SASL_NS}'/>"));
                        answer.signed_in = Some(user.clone());
                        self.phase = Phase::Restarting { user };
                    }
                    None => {
                        answer.replies.push(format!(
                            "<failure xmlns='{SASL_NS}'><not-authorized/></failure>"
                        ));
                        answer.replies.push(STREAM_END.to_owned());
                        answer.close = true;
                    }
                }
            }
            Phase::Restarting { user } if text.contains(STREAM_OPENING) => {
                answer.replies.push(self.stream_header(&bound_features()));
                self.phase = Phase::Open { user, jid: None };
            }
            Phase::Open { user, jid } => {
                let jid = if text.contains(PRESENCE_START) {
                    if let Some(jid) = &jid {
                        self.room_presence(text, &user, jid, now_s, &mut answer);
                    }
                    jid
                } else if text.contains(SM_ENABLE_START) && text.contains(STREAM_MANAGEMENT_NS) {
                    answer.replies.push(format!(
                        "<failed xmlns='{STREAM_MANAGEMENT_NS}'><feature-not-implemented \
                         xmlns='{STANZAS_NS}'/></failed>"
                    ));
                    jid
                } else {
                    self.open_stanza(text, &user, jid, &mut answer)
                };
                self.phase = Phase::Open { user, jid };
            }
            phase => self.phase = phase,
        }
        answer
    }

    /// The answers in the restarted stream: the bind, the session, pings, and an error for
    /// any other `iq`; the bound address after it.
    fn open_stanza(
        &self,
        text: &str,
        user: &str,
        jid: Option<String>,
        answer: &mut Answer,
    ) -> Option<String> {
        let Some(id) = attribute(text, "<iq", "id") else {
            return jid;
        };
        if text.contains(VIVOX_MUC_NS)
            && let Some(room) = attribute(text, "<iq", "to")
        {
            let (id, room) = (id.to_owned(), room.to_owned());
            if text.contains(INVITE_START) {
                let offer = element_text(text, "invite").unwrap_or_default().to_owned();
                answer.call = Some(CallRequest::Invite { id, room, offer });
                return jid;
            }
            if text.contains(CHAN_ADD_START) {
                answer.call = Some(CallRequest::ChanAdd { id, room });
                return jid;
            }
        }
        if text.contains(BIND_NS) {
            let resource = element_text(text, "resource").unwrap_or(DEFAULT_RESOURCE);
            let bound = format!("{user}@{}/{resource}", self.sign_in.realm);
            answer.replies.push(format!(
                "<iq type='result' id='{id}'><bind xmlns='{BIND_NS}'><jid>{bound}</jid></bind></iq>"
            ));
            return Some(bound);
        }
        if text.contains(SESSION_NS) || text.contains(PING_NS) {
            answer
                .replies
                .push(format!("<iq type='result' id='{id}'/>"));
        } else if text.contains(ROSTER_NS) {
            answer.replies.push(format!(
                "<iq type='result' id='{id}'><query xmlns='{ROSTER_NS}'/></iq>"
            ));
        } else if text.contains(BLOCKING_NS) {
            answer.replies.push(format!(
                "<iq type='result' id='{id}'><blocklist xmlns='{BLOCKING_NS}'/></iq>"
            ));
        } else if attribute(text, "<iq", "type").is_some_and(|kind| kind == "get" || kind == "set")
        {
            answer.replies.push(format!(
                "<iq type='error' id='{id}'><error type='cancel'><feature-not-implemented \
                 xmlns='{STANZAS_NS}'/></error></iq>"
            ));
        }
        jid
    }

    /// A presence to a room in the restarted stream of `user` bound as `jid`: a join with a
    /// valid join token for that channel, or a leave; a join with another token gets
    /// XEP-0045's `not-authorized` error.
    fn room_presence(
        &mut self,
        text: &str,
        user: &str,
        jid: &str,
        now_s: u64,
        answer: &mut Answer,
    ) {
        let Some(to) = attribute(text, PRESENCE_START, "to") else {
            return;
        };
        let Some((room, nick)) = to.split_once('/') else {
            return;
        };
        let (room, nick) = (room.to_owned(), nick.to_owned());
        let id = attribute(text, PRESENCE_START, "id").map(str::to_owned);
        if attribute(text, PRESENCE_START, "type") == Some(UNAVAILABLE) {
            self.joined.remove(&room);
            answer.room = Some(RoomChange::Leave { room, nick, id });
            return;
        }
        let status = vivox_status(text);
        if self.joined.contains(&room) {
            // Observed (live 2026-11-07): after the invite answer the SDK sends
            // `<m xmlns="urn:vivox:presence"><audio/></m>` with no password; a refusal there
            // ended its call ("voice channel join failed").
            answer.room = Some(RoomChange::Update {
                room,
                nick,
                status,
                id,
            });
            return;
        }
        let Some((channel, _)) = room.split_once('@') else {
            return;
        };
        let Some(token_user) = token_user(user) else {
            return;
        };
        let from = format!("sip:{token_user}@{}", self.sign_in.realm);
        let target = format!("sip:{channel}@{}", self.sign_in.realm);
        let allowed = element_text(text, "password")
            .and_then(|token| verified_claims(&self.sign_in.key, token.trim()))
            .is_some_and(|claims| claims.allow_join(&self.sign_in.issuer, &from, &target, now_s));
        if allowed {
            self.joined.insert(room.clone());
            answer.room = Some(RoomChange::Join {
                room,
                nick,
                jid: jid.to_owned(),
                status,
                id,
            });
        } else {
            answer.replies.push(refused_presence(&room, &nick, jid));
        }
    }

    /// The server's stream header from the realm with `features`.
    fn stream_header(&self, features: &str) -> String {
        format!(
            "<?xml version='1.0'?><stream:stream xmlns='jabber:client' \
             xmlns:stream='http://etherx.jabber.org/streams' id='{STREAM_ID}' from='{}' \
             version='1.0'><stream:features>{features}</stream:features>",
            self.sign_in.realm
        )
    }

    /// The user a `PLAIN` sign-in `text` proves at Unix seconds `now_s`: its authentication id,
    /// when its password is a valid login token for that id without the SDK's tag.
    fn signed_in_user(&self, text: &str, now_s: u64) -> Option<String> {
        if attribute(text, AUTH_START.trim_end(), "mechanism") != Some(PLAIN_MECHANISM) {
            return None;
        }
        let payload = element_text(text, "auth")?;
        let plain = BASE64.decode(payload.trim().as_bytes()).ok()?;
        let plain = String::from_utf8(plain).ok()?;
        let parts: Vec<&str> = plain.splitn(PLAIN_PARTS, PLAIN_SEPARATOR).collect();
        let [_, authentication, password] = parts[..] else {
            return None;
        };
        let from = format!("sip:{}@{}", token_user(authentication)?, self.sign_in.realm);
        verified_claims(&self.sign_in.key, password)?
            .allow_login(&self.sign_in.issuer, &from, now_s)
            .then(|| authentication.to_owned())
    }
}

/// The address user the social service's tokens name for the SDK account `user`: the account
/// without the SDK's tag (`.<issuer>.<id>+B.` gives `.<issuer>.<id>.`).
fn token_user(user: &str) -> Option<String> {
    Some(format!("{}.", user.strip_suffix(SDK_ID_TAG)?))
}

/// The answer to an invite `id` from `room` to the client `to`: the SDP answer `sdp`
/// (`vivoxsdk.dll` `0x18011cb00` needs the `invite` child, a `to` and a valid SDP).
pub fn invite_result(id: &str, to: &str, room: &str, sdp: &str) -> String {
    format!(
        "<iq type='result' id='{id}' to='{to}' from='{room}'><invite \
         xmlns='{VIVOX_MUC_NS}'>{}</invite></iq>",
        escape(sdp)
    )
}

/// The answer to a `chan_add` `id` from `room` to the client `to`: a response document with
/// `code` and `msg`, and the 3D properties of a positional room on success, as escaped text
/// (`vivoxsdk.dll` `0x18011c5b0` reads `//response/code`, `//response/msg` and
/// `//response/body/vx_chn_properties/*`).
pub fn chan_add_result(id: &str, to: &str, room: &str, code: u16, msg: &str) -> String {
    let body = if code == CHAN_ADD_DONE && crate::media::is_positional(room) {
        format!("<body>{}</body>", crate::media::positional_properties_xml())
    } else {
        String::new()
    };
    let response = format!("<response><code>{code}</code><msg>{msg}</msg>{body}</response>");
    format!(
        "<iq type='result' id='{id}' to='{to}' from='{room}'><chan_add \
         xmlns='{VIVOX_MUC_NS}'>{}</chan_add></iq>",
        escape(&response)
    )
}

/// The refusal of a call setup `id` for `room`, which the client `to` did not join: RFC 6120's
/// `item-not-found`.
pub fn not_in_room(id: &str, to: &str, room: &str) -> String {
    format!(
        "<iq type='error' id='{id}' to='{to}' from='{room}'><error type='cancel'><item-not-found \
         xmlns='{STANZAS_NS}'/></error></iq>"
    )
}

/// `text` with the XML special characters of element text escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The features before the sign-in: SASL `PLAIN`.
fn sasl_features() -> String {
    format!("<mechanisms xmlns='{SASL_NS}'><mechanism>{PLAIN_MECHANISM}</mechanism></mechanisms>")
}

/// The features of the restarted stream: resource binding and the session.
fn bound_features() -> String {
    format!("<bind xmlns='{BIND_NS}'/><session xmlns='{SESSION_NS}'><optional/></session>")
}

/// The value of attribute `name` of the first element that starts with `element` in `text`
/// (either quote).
fn attribute<'a>(text: &'a str, element: &str, name: &str) -> Option<&'a str> {
    let start = text.find(element)?;
    let tag = &text[start..start + text[start..].find('>')?];
    for quote in ['\'', '"'] {
        let key = format!(" {name}={quote}");
        if let Some(at) = tag.find(&key) {
            let value = &tag[at + key.len()..];
            return value.split(quote).next();
        }
    }
    None
}

/// The text of the first element `name` in `text`.
fn element_text<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}"))?;
    let body = &text[start + text[start..].find('>')? + 1..];
    body.split(&format!("</{name}>")).next()
}

#[cfg(test)]
mod tests {
    use nw_social::voice::{TokenAction, VoiceService};
    use uuid::Uuid;

    use super::*;

    const REALM: &str = "game.example.org";
    const ISSUER: &str = "issuer-1";
    const NOW_S: u64 = 1_000;

    fn service() -> VoiceService {
        VoiceService {
            api_url: "https://game.example.org/vivox/api2/".to_owned(),
            issuer: ISSUER.to_owned(),
            domain: REALM.to_owned(),
            key: b"secret".to_vec(),
            channel_suffix: String::new(),
            cell_size_m: 512.0,
        }
    }

    fn session() -> Session {
        Session::new(SignIn {
            realm: REALM.to_owned(),
            issuer: ISSUER.to_owned(),
            key: b"secret".to_vec(),
        })
    }

    /// The SDK's sign-in as the live log has it: `\0.<issuer>.<id>+B.\0<token>`.
    fn auth(character: Uuid, token: &str) -> String {
        let plain = format!("\0.{ISSUER}.{}+B.\0{token}", character.hyphenated());
        format!(
            "<auth mechanism=\"PLAIN\" xmlns=\"{SASL_NS}\">{}</auth>",
            BASE64.encode(plain.as_bytes())
        )
    }

    const OPENING: &str = "<?xml version='1.0'?><stream:stream to='game.example.org'  \
        xml:lang='en' version='1.0' xmlns='jabber:client' \
        xmlns:stream='http://etherx.jabber.org/streams' ua='Vivox-SDK-5.23.1.33742.f513647dW_x64'>";

    #[test]
    fn the_sdk_signs_in_with_its_login_token_and_binds() {
        let character = Uuid::from_u128(1);
        let token = service().token(TokenAction::Login, character, 1, None, NOW_S);
        let mut session = session();
        let open = session.on_text(OPENING, NOW_S);
        assert!(open.replies[0].contains("<mechanism>PLAIN</mechanism>"));
        let signed = session.on_text(&auth(character, &token), NOW_S);
        assert_eq!(signed.replies, [format!("<success xmlns='{SASL_NS}'/>")]);
        let user = format!(".{ISSUER}.{}+B.", character.hyphenated());
        assert_eq!(signed.signed_in.as_deref(), Some(user.as_str()));
        let restarted = session.on_text(OPENING, NOW_S);
        assert!(restarted.replies[0].contains(BIND_NS));
        let bound = session.on_text(
            &format!(
                "<iq type='set' id='b1'><bind xmlns='{BIND_NS}'><resource>r1</resource></bind></iq>"
            ),
            NOW_S,
        );
        assert_eq!(
            bound.replies,
            [format!(
                "<iq type='result' id='b1'><bind xmlns='{BIND_NS}'><jid>{user}@{REALM}/r1</jid></bind></iq>"
            )]
        );
        let ping = session.on_text(
            &format!("<iq type=\"get\" id=\"p1\"><ping xmlns=\"{PING_NS}\"/></iq>"),
            NOW_S,
        );
        assert_eq!(ping.replies, ["<iq type='result' id='p1'/>"]);
        let unknown = session.on_text("<iq type='set' id='x1'><query xmlns='other'/></iq>", NOW_S);
        assert!(unknown.replies[0].contains("feature-not-implemented"));
        let end = session.on_text(STREAM_END, NOW_S);
        assert!(end.close);
    }

    /// A session signed in and bound as `character`, and its bound address.
    fn bound(character: Uuid) -> (Session, String) {
        let token = service().token(TokenAction::Login, character, 1, None, NOW_S);
        let mut session = session();
        session.on_text(OPENING, NOW_S);
        session.on_text(&auth(character, &token), NOW_S);
        session.on_text(OPENING, NOW_S);
        session.on_text(
            &format!("<iq id=\"_xmpp_bind1\" type=\"set\"><bind xmlns=\"{BIND_NS}\"/></iq>"),
            NOW_S,
        );
        let jid = session.jid().expect("bound").to_owned();
        (session, jid)
    }

    #[test]
    fn the_sdk_s_side_requests_get_empty_answers() {
        let (mut session, _) = bound(Uuid::from_u128(1));
        let roster = session.on_text(
            &format!("<iq id=\"r1\" type=\"get\"><query xmlns=\"{ROSTER_NS}\"/></iq>"),
            NOW_S,
        );
        assert_eq!(
            roster.replies,
            [format!(
                "<iq type='result' id='r1'><query xmlns='{ROSTER_NS}'/></iq>"
            )]
        );
        let blocks = session.on_text(
            &format!("<iq id=\"b1\" type=\"get\"><blocklist xmlns=\"{BLOCKING_NS}\"/></iq>"),
            NOW_S,
        );
        assert!(blocks.replies[0].contains("<blocklist"));
        let resume = session.on_text(
            &format!("<enable xmlns=\"{STREAM_MANAGEMENT_NS}\" resume=\"true\"/>"),
            NOW_S,
        );
        assert!(resume.replies[0].starts_with("<failed"));
    }

    #[test]
    fn a_room_join_needs_the_join_token_of_that_channel() {
        const CHANNEL: &str = "018012_0";
        let character = Uuid::from_u128(1);
        let (mut session, jid) = bound(character);
        let service = service();
        let target = service.positional_uri(CHANNEL);
        let token = service.token(TokenAction::Join, character, 2, Some(&target), NOW_S);
        let room = format!("confctl-d-{ISSUER}.{CHANNEL}@muc.{REALM}");
        let nick = format!("{ISSUER}.{}", character.hyphenated());
        let join = |token: &str| {
            format!(
                "<presence to=\"{room}/{nick}\"><m xmlns=\"urn:vivox:presence\"/><x \
                 xmlns=\"http://jabber.org/protocol/muc\"><password>{token}</password></x>\
                 </presence>"
            )
        };
        let joined = session.on_text(&join(&token), NOW_S);
        assert_eq!(
            joined.room,
            Some(RoomChange::Join {
                room: room.clone(),
                nick: nick.clone(),
                jid,
                status: String::new(),
                id: None,
            })
        );
        let audio = session.on_text(
            &format!(
                "<presence id=\"u1\" to=\"{room}/{nick}\"><m xmlns=\"urn:vivox:presence\">\
                 <audio/></m></presence>"
            ),
            NOW_S,
        );
        assert_eq!(
            audio.room,
            Some(RoomChange::Update {
                room: room.clone(),
                nick: nick.clone(),
                status: "<audio/>".to_owned(),
                id: Some("u1".to_owned()),
            }),
            "a joined room's presence without a password is a status change"
        );
        // The token of this room does not open the next one.
        let next_room = format!("confctl-d-{ISSUER}.018013_0@muc.{REALM}");
        let refused = session.on_text(
            &format!(
                "<presence to=\"{next_room}/{nick}\"><m xmlns=\"urn:vivox:presence\"/><x \
                 xmlns=\"http://jabber.org/protocol/muc\"><password>{token}</password></x>\
                 </presence>"
            ),
            NOW_S,
        );
        assert_eq!(refused.room, None);
        assert!(refused.replies[0].contains("not-authorized"));
        let left = session.on_text(
            &format!("<presence to=\"{room}/{nick}\" type=\"unavailable\"/>"),
            NOW_S,
        );
        assert_eq!(
            left.room,
            Some(RoomChange::Leave {
                room,
                nick,
                id: None
            })
        );
    }

    #[test]
    fn invite_and_chan_add_are_read_and_answered() {
        let (mut session, jid) = bound(Uuid::from_u128(1));
        let room = format!("confctl-d-{ISSUER}.018012_0@muc.{REALM}");
        let offer = "v=0\r\nm=audio 0 RTP/AVP 127\r\n";
        let invite = session.on_text(
            &format!(
                "<iq type=\"set\" to=\"{room}\" id=\"i1\"><invite callid=\"7\" v=\"2\" \
                 xmlns=\"{VIVOX_MUC_NS}\">{offer}</invite></iq>"
            ),
            NOW_S,
        );
        assert_eq!(
            invite.call,
            Some(CallRequest::Invite {
                id: "i1".to_owned(),
                room: room.clone(),
                offer: offer.to_owned(),
            })
        );
        assert!(invite.replies.is_empty(), "the call answers it");
        let add = session.on_text(
            &format!(
                "<iq type=\"set\" to=\"{room}\" id=\"c1\"><chan_add callid=\"7\" v=\"2\" \
                 xmlns=\"{VIVOX_MUC_NS}\"/></iq>"
            ),
            NOW_S,
        );
        assert_eq!(
            add.call,
            Some(CallRequest::ChanAdd {
                id: "c1".to_owned(),
                room: room.clone(),
            })
        );
        let result = invite_result("i1", &jid, &room, "v=0\r\nc=IN IP4 127.0.0.1\r\n");
        assert!(result.starts_with(&format!("<iq type='result' id='i1' to='{jid}'")));
        assert!(result.contains("<invite xmlns='urn:vivox:muc'>v=0\r\n"));
        let done = chan_add_result("c1", &jid, &room, CHAN_ADD_DONE, "OK");
        assert!(done.contains("&lt;code&gt;200&lt;/code&gt;"));
        assert!(done.contains("&lt;vx_chan_type&gt;positional"));
        assert!(not_in_room("i2", &jid, &room).contains("item-not-found"));
        let missing = chan_add_result("c1", &jid, &room, CHAN_ADD_NO_CALL, NO_CALL_MESSAGE);
        assert!(missing.contains(NO_CALL_MESSAGE) && !missing.contains("vx_chn_properties"));
    }

    #[test]
    fn a_wrong_or_expired_token_is_refused() {
        let character = Uuid::from_u128(1);
        let token = service().token(TokenAction::Login, character, 1, None, NOW_S);
        let mut other = session();
        other.on_text(OPENING, NOW_S);
        let refused = other.on_text(&auth(Uuid::from_u128(2), &token), NOW_S);
        assert!(
            refused.close && refused.replies[0].contains("not-authorized"),
            "another player"
        );
        let mut late = session();
        late.on_text(OPENING, NOW_S);
        let expired = late.on_text(&auth(character, &token), NOW_S + 10_000);
        assert!(expired.close, "expired");
    }
}
