// SPDX-License-Identifier: AGPL-3.0-or-later
//! The voice check: one client enters the world and sets up its voice chat with the calls of the
//! retail client (`nw_messages::social::voice`): the service (1643), a login token (2077), the
//! positional channels of its place (2805), then a join token (2934) for the first channel the
//! answer lists. Each answer must come back in the retail form: 4199, a login token (3051) whose
//! claims name the login, a channel list (405) whose addresses name their channels, and a join
//! token whose target is the joined channel's address. The social service answers only with a
//! `[voice]` service in its configuration (`bins/social/src/config.rs`); the check sends no
//! audio.

use std::time::Duration;

use data_encoding::BASE64URL_NOPAD;
use nw_carrier::MESSAGE_CHANNEL;
use nw_messages::social::voice::{
    CHANNEL_POSITIONAL, TOKEN_JOIN, TOKEN_LOGIN, VoiceCall, VoicePush,
};
use nw_messages::social::{SocialFacet, SocialPush};
use nw_reflect::{LoginToken, REGISTRATION_STATUS_AUTHORIZED};
use reqwest::Client;

use crate::pair::{Check, PairError, session_with};
use crate::rep::{Received, RepClient, RepError};
use crate::udp::RepOutcome;
use crate::view::WorldView;
use crate::walk::{WalkSettings, login_token, walk};

/// Check: the session got registration status 0.
pub const REGISTERED: &str = "the session registered";
/// Check: the voice service came back (4199).
pub const SERVICE: &str = "the voice service came back";
/// Check: a login token came back for the login call.
pub const LOGIN_TOKEN: &str = "a login token came back";
/// Check: the positional channels came back, each address naming its channel.
pub const CHANNELS: &str = "the positional channels came back";
/// Check: a join token for the first channel came back, its target that channel's address.
pub const JOIN_TOKEN: &str = "a join token for the first channel came back";
/// Time from the own entity's first state to the first call: the client is in the world by
/// then.
pub const CALL_DELAY: Duration = Duration::from_secs(2);
/// The request number of the login call and of the join call.
const LOGIN_REQUEST: u16 = 1;
const JOIN_REQUEST: u16 = 2;
/// The parts of a token: header, claims, signature.
const TOKEN_PARTS: usize = 3;
/// The `vxa` claim of a login and of a join token.
const LOGIN_ACTION: &str = "login";
const JOIN_ACTION: &str = "join";

/// The account and the timers.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceSettings {
    /// The client that sets up its voice chat.
    pub account: WalkSettings,
    /// Time in the world.
    pub hold: Duration,
    /// Longest wait for a registration response.
    pub registration_timeout: Duration,
}

/// The claims of a token that the check reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenClaims {
    /// `vxa`: the action.
    pub action: String,
    /// `t`: the channel's address, for a join.
    pub target: Option<String>,
}

/// What the voice run saw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceReport {
    /// What happened on the connection.
    pub outcome: RepOutcome,
    /// The service's API address, when 4199 came back.
    pub service: Option<String>,
    /// The claims of the login token, when it came back.
    pub login: Option<TokenClaims>,
    /// The positional channels (name, address).
    pub channels: Vec<(String, String)>,
    /// The claims of the join token, when it came back.
    pub join: Option<TokenClaims>,
}

impl VoiceReport {
    /// Every check, in order.
    pub fn checks(&self) -> Vec<Check> {
        let first_uri = self.channels.first().map(|(_, uri)| uri.as_str());
        vec![
            Check {
                name: REGISTERED,
                passed: self.outcome.status == Some(REGISTRATION_STATUS_AUTHORIZED),
            },
            Check {
                name: SERVICE,
                passed: self.service.is_some(),
            },
            Check {
                name: LOGIN_TOKEN,
                passed: self
                    .login
                    .as_ref()
                    .is_some_and(|claims| claims.action == LOGIN_ACTION && claims.target.is_none()),
            },
            Check {
                name: CHANNELS,
                passed: !self.channels.is_empty()
                    && self
                        .channels
                        .iter()
                        .all(|(name, uri)| uri.contains(&format!(".{name}"))),
            },
            Check {
                name: JOIN_TOKEN,
                passed: self.join.as_ref().is_some_and(|claims| {
                    claims.action == JOIN_ACTION && claims.target.as_deref() == first_uri
                }),
            },
        ]
    }

    /// Reads the voice pushes among `messages` into this report.
    fn read(&mut self, messages: &[Received]) {
        for push in voice_pushes(messages) {
            match push {
                VoicePush::Registered { api_url, .. } => self.service = Some(api_url),
                VoicePush::Token {
                    token,
                    kind,
                    request,
                } => {
                    let claims = token_claims(&token);
                    match (kind, request) {
                        (TOKEN_LOGIN, LOGIN_REQUEST) => self.login = claims,
                        (TOKEN_JOIN, JOIN_REQUEST) => self.join = claims,
                        _ => {}
                    }
                }
                VoicePush::WorldChannels { channels } => self.channels = channels,
                VoicePush::GroupChannel { .. } => {}
            }
        }
    }
}

/// The voice pushes among `messages`, in order.
pub fn voice_pushes(messages: &[Received]) -> Vec<VoicePush> {
    messages
        .iter()
        .filter_map(|received| {
            let index = received.envelope.type_index()?;
            match SocialPush::decode(index, &received.envelope.body)? {
                Ok((_, SocialPush::Voice(push))) => Some(push),
                _ => None,
            }
        })
        .collect()
}

/// The claims of token `token` (`e30.<claims>.<signature>`); `None` for another form.
pub fn token_claims(token: &str) -> Option<TokenClaims> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != TOKEN_PARTS {
        return None;
    }
    let claims = BASE64URL_NOPAD.decode(parts[1].as_bytes()).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&claims).ok()?;
    Some(TokenClaims {
        action: claims.get("vxa")?.as_str()?.to_owned(),
        target: claims
            .get("t")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

/// The client during its hold: once it knows its own entity, [`CALL_DELAY`] later it asks for
/// the service, a login token and the channels; once the channels came back, a join token for
/// the first one.
struct Voicer {
    view: WorldView,
    read: usize,
    own_since: Option<Duration>,
    asked: bool,
    joined: bool,
}

impl Voicer {
    fn tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        since_registration: Duration,
    ) -> Result<(), RepError> {
        let new = &outcome.messages[self.read..];
        for received in new {
            // A message that does not decode fails nothing here: the checks read the answers.
            let _ = self.view.read(received);
        }
        let channel = voice_pushes(new).into_iter().find_map(|push| match push {
            VoicePush::WorldChannels { channels } => channels.into_iter().next(),
            _ => None,
        });
        self.read = outcome.messages.len();
        let Some(own) = self.view.own_number() else {
            return Ok(());
        };
        let header = SocialFacet::VoiceChat.header(own);
        let mut send = |call: VoiceCall| -> Result<(), RepError> {
            let body = call.encode(header)?;
            client.send_message(MESSAGE_CHANNEL, true, call.type_index(), body)
        };
        if let Some((name, _)) = channel.filter(|_| !self.joined) {
            send(VoiceCall::JoinToken {
                request: JOIN_REQUEST,
                channel: name,
                kind: CHANNEL_POSITIONAL,
            })?;
            self.joined = true;
        }
        let since = *self.own_since.get_or_insert(since_registration);
        if self.asked || since_registration.saturating_sub(since) < CALL_DELAY {
            return Ok(());
        }
        send(VoiceCall::Register)?;
        send(VoiceCall::LoginToken {
            request: LOGIN_REQUEST,
        })?;
        send(VoiceCall::WorldChannels)?;
        self.asked = true;
        Ok(())
    }
}

/// Runs the voice check. `new_client` makes a client for a token; `report` gets one line per
/// HTTPS step.
///
/// # Errors
///
/// Returns [`PairError`] for the first step that fails.
pub async fn run_voice(
    http: &Client,
    settings: &VoiceSettings,
    new_client: impl Fn(LoginToken) -> Result<RepClient, RepError>,
    mut report: impl FnMut(String),
) -> Result<VoiceReport, PairError> {
    let admission = walk(http, &settings.account, &mut report).await?;
    let mut voicer = Voicer {
        view: WorldView::default(),
        read: 0,
        own_since: None,
        asked: false,
        joined: false,
    };
    let session = session_with(
        admission.rep_address,
        new_client(login_token(&admission.token))?,
        settings.hold,
        settings.registration_timeout,
        |client, outcome, since| voicer.tick(client, outcome, since),
    )
    .await?;
    let mut voice = VoiceReport::default();
    voice.read(&session.outcome.messages);
    voice.outcome = session.outcome;
    Ok(voice)
}

#[cfg(test)]
mod tests {
    use nw_messages::social::push_envelope;
    use nw_reflect::Envelope;

    use super::*;

    const OWN: u64 = 7;
    const CHANNEL: &str = "013007_0";
    const CHANNEL_URI: &str = "sip:confctl-d-issuer.013007_0SUFFIX@voice.example";

    fn pushed(push: VoicePush) -> Received {
        let push = SocialPush::Voice(push);
        let bytes = push_envelope(
            push.type_index(),
            SocialFacet::VoiceChat.header(OWN),
            &push.encode_args().expect("args"),
        )
        .expect("envelope");
        Received {
            channel: MESSAGE_CHANNEL,
            envelope: Envelope::decode(&bytes).expect("decode"),
        }
    }

    /// A token with the claims `claims` (JSON) and any signature.
    fn token(claims: &str) -> String {
        format!(
            "e30.{}.signature",
            BASE64URL_NOPAD.encode(claims.as_bytes())
        )
    }

    fn answers() -> Vec<Received> {
        vec![
            pushed(VoicePush::Registered {
                api_url: "https://voice.example/api2/".to_owned(),
                issuer: "issuer".to_owned(),
                domain: "@voice.example".to_owned(),
                flag: 0,
            }),
            pushed(VoicePush::Token {
                token: token(r#"{"vxa":"login","vxi":1}"#),
                kind: TOKEN_LOGIN,
                request: LOGIN_REQUEST,
            }),
            pushed(VoicePush::WorldChannels {
                channels: vec![(CHANNEL.to_owned(), CHANNEL_URI.to_owned())],
            }),
            pushed(VoicePush::Token {
                token: token(&format!(r#"{{"vxa":"join","vxi":2,"t":"{CHANNEL_URI}"}}"#)),
                kind: TOKEN_JOIN,
                request: JOIN_REQUEST,
            }),
        ]
    }

    #[test]
    fn the_retail_answers_pass_every_check() {
        let mut report = VoiceReport {
            outcome: RepOutcome {
                status: Some(REGISTRATION_STATUS_AUTHORIZED),
                ..RepOutcome::default()
            },
            ..VoiceReport::default()
        };
        report.read(&answers());
        assert_eq!(
            report.channels,
            [(CHANNEL.to_owned(), CHANNEL_URI.to_owned())]
        );
        assert!(
            report.checks().iter().all(|check| check.passed),
            "{report:?}"
        );
    }

    #[test]
    fn a_join_token_for_another_channel_fails_the_join_check() {
        let mut messages = answers();
        messages.pop();
        messages.push(pushed(VoicePush::Token {
            token: token(r#"{"vxa":"join","vxi":2,"t":"sip:other"}"#),
            kind: TOKEN_JOIN,
            request: JOIN_REQUEST,
        }));
        let mut report = VoiceReport {
            outcome: RepOutcome {
                status: Some(REGISTRATION_STATUS_AUTHORIZED),
                ..RepOutcome::default()
            },
            ..VoiceReport::default()
        };
        report.read(&messages);
        let failed: Vec<&str> = report
            .checks()
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name)
            .collect();
        assert_eq!(failed, [JOIN_TOKEN]);
    }

    #[test]
    fn no_voice_service_fails_every_voice_check() {
        let report = VoiceReport {
            outcome: RepOutcome {
                status: Some(REGISTRATION_STATUS_AUTHORIZED),
                ..RepOutcome::default()
            },
            ..VoiceReport::default()
        };
        let failed: Vec<&str> = report
            .checks()
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name)
            .collect();
        assert_eq!(failed, [SERVICE, LOGIN_TOKEN, CHANNELS, JOIN_TOKEN]);
    }

    #[test]
    fn a_token_that_is_not_three_parts_has_no_claims() {
        assert_eq!(token_claims("e30.only"), None);
        assert_eq!(
            token_claims(&token(r#"{"vxa":"join","t":"sip:t"}"#)),
            Some(TokenClaims {
                action: JOIN_ACTION.to_owned(),
                target: Some("sip:t".to_owned()),
            })
        );
    }
}
