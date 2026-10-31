// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat check: one client enters the world, says a line in area chat (1291, the call the
//! retail client sends from its chat box), and the server must answer before the hold ends. A GM
//! chat command (`!help`) of an account with the game master rank gets system chat lines from the
//! sim (`sim::world::gm_chat`); any other line comes back from the social service as the
//! sender's own area line (the sender hears its area lines, `nw_social::chat`).

use std::time::Duration;

use nw_carrier::MESSAGE_CHANNEL;
use nw_messages::social::chat::{ChatCall, ChatChannel, ChatMessage, ChatPush};
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
/// Check: the client knew its own entity and said the line.
pub const SAID: &str = "the line was said";
/// Check: system chat lines, or the line itself, came back after the line.
pub const ANSWERED: &str = "the server answered the line";
/// Time from the own entity's first state to the line: the client is in the world by then.
pub const SAY_DELAY: Duration = Duration::from_secs(2);

/// The account, the line and the timers.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSettings {
    /// The client that says the line.
    pub account: WalkSettings,
    /// The line, as typed in the chat box.
    pub line: String,
    /// Time in the world.
    pub hold: Duration,
    /// Longest wait for a registration response.
    pub registration_timeout: Duration,
}

/// What the chat run saw.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatReport {
    /// What happened on the connection.
    pub outcome: RepOutcome,
    /// Whether the line went out.
    pub said: bool,
    /// The bodies of the system lines the client got after the line, in order.
    pub answers: Vec<String>,
    /// Whether the line came back as a player's line.
    pub echoed: bool,
}

impl ChatReport {
    /// Every check, in order.
    pub fn checks(&self) -> Vec<Check> {
        vec![
            Check {
                name: REGISTERED,
                passed: self.outcome.status == Some(REGISTRATION_STATUS_AUTHORIZED),
            },
            Check {
                name: SAID,
                passed: self.said,
            },
            Check {
                name: ANSWERED,
                passed: !self.answers.is_empty() || self.echoed,
            },
        ]
    }
}

/// The client during its hold: it follows the world messages until it knows its own entity,
/// then says the line once, [`SAY_DELAY`] later.
struct Speaker<'a> {
    line: &'a str,
    view: WorldView,
    read: usize,
    own_since: Option<Duration>,
    said_after: Option<usize>,
}

impl Speaker<'_> {
    fn tick(
        &mut self,
        client: &mut RepClient,
        outcome: &RepOutcome,
        since_registration: Duration,
    ) -> Result<(), RepError> {
        for received in &outcome.messages[self.read..] {
            // A message that does not decode fails nothing here: the check reads the answers.
            let _ = self.view.read(received);
        }
        self.read = outcome.messages.len();
        let Some(own) = self.view.own_number() else {
            return Ok(());
        };
        let since = *self.own_since.get_or_insert(since_registration);
        if self.said_after.is_some() || since_registration.saturating_sub(since) < SAY_DELAY {
            return Ok(());
        }
        let call = ChatCall::Send {
            channel: ChatChannel::Area.wire(),
            body: self.line.to_owned(),
            recipient_name: String::new(),
            links: Vec::new(),
        };
        let body = call.encode(SocialFacet::Chat.header(own))?;
        client.send_message(MESSAGE_CHANNEL, true, call.type_index(), body)?;
        self.said_after = Some(outcome.messages.len());
        Ok(())
    }
}

/// The chat lines among `messages`, in order.
pub fn chat_lines(messages: &[Received]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter_map(|received| {
            let index = received.envelope.type_index()?;
            match SocialPush::decode(index, &received.envelope.body)? {
                Ok((_, SocialPush::Chat(ChatPush::Batch(lines)))) => Some(lines),
                Ok((_, SocialPush::Chat(ChatPush::Message(line)))) => Some(vec![line]),
                _ => None,
            }
        })
        .flatten()
        .collect()
}

/// Whether `line` is a system line: it has no sender.
fn is_system(line: &ChatMessage) -> bool {
    line.sender_id.is_empty()
}

/// Runs the chat check. `new_client` makes a client for a token; `report` gets one line per
/// HTTPS step.
///
/// # Errors
///
/// Returns [`PairError`] for the first step that fails.
pub async fn run_chat(
    http: &Client,
    settings: &ChatSettings,
    new_client: impl Fn(LoginToken) -> Result<RepClient, RepError>,
    mut report: impl FnMut(String),
) -> Result<ChatReport, PairError> {
    let admission = walk(http, &settings.account, &mut report).await?;
    let mut speaker = Speaker {
        line: &settings.line,
        view: WorldView::default(),
        read: 0,
        own_since: None,
        said_after: None,
    };
    let session = session_with(
        admission.rep_address,
        new_client(login_token(&admission.token))?,
        settings.hold,
        settings.registration_timeout,
        |client, outcome, since| speaker.tick(client, outcome, since),
    )
    .await?;
    let heard = speaker
        .said_after
        .map(|after| chat_lines(&session.outcome.messages[after..]))
        .unwrap_or_default();
    let echoed = heard
        .iter()
        .any(|line| !is_system(line) && line.body == settings.line);
    let answers = heard
        .into_iter()
        .filter(is_system)
        .map(|line| line.body)
        .collect();
    Ok(ChatReport {
        outcome: session.outcome,
        said: speaker.said_after.is_some(),
        answers,
        echoed,
    })
}

#[cfg(test)]
mod tests {
    use nw_messages::FacetHeader;
    use nw_messages::social::chat::ChatMessage;
    use nw_messages::social::push_envelope;
    use nw_reflect::Envelope;

    use super::*;

    const OWN: u64 = 7;

    fn pushed(push: &SocialPush) -> Received {
        let header = SocialFacet::Chat.header(OWN);
        let bytes = push_envelope(
            push.type_index(),
            header,
            &push.encode_args().expect("args"),
        )
        .expect("envelope");
        Received {
            channel: MESSAGE_CHANNEL,
            envelope: Envelope::decode(&bytes).expect("decode"),
        }
    }

    #[test]
    fn the_lines_of_both_pushes_are_read_and_the_system_ones_have_no_sender() {
        let system = ChatMessage::system(ChatChannel::System, "Done.".to_owned());
        let player = ChatMessage {
            sender_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            ..ChatMessage::system(ChatChannel::Area, "hello".to_owned())
        };
        let messages = [
            pushed(&SocialPush::Chat(ChatPush::Batch(vec![
                system.clone(),
                player,
            ]))),
            pushed(&SocialPush::Chat(ChatPush::Message(system))),
        ];
        let lines = chat_lines(&messages);
        let system: Vec<bool> = lines.iter().map(is_system).collect();
        assert_eq!(system, [true, false, true]);
        assert_eq!(lines[1].body, "hello");
    }

    #[test]
    fn the_checks_need_the_registration_the_line_and_an_answer() {
        let report = ChatReport {
            outcome: RepOutcome {
                status: Some(REGISTRATION_STATUS_AUTHORIZED),
                ..RepOutcome::default()
            },
            said: true,
            answers: vec!["Done.".to_owned()],
            echoed: false,
        };
        assert!(report.checks().iter().all(|check| check.passed));
        let echoed = ChatReport {
            answers: Vec::new(),
            echoed: true,
            ..report.clone()
        };
        assert!(
            echoed.checks().iter().all(|check| check.passed),
            "a plain line's echo"
        );
        let silent = ChatReport {
            answers: Vec::new(),
            ..report
        };
        let failed: Vec<&str> = silent
            .checks()
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name)
            .collect();
        assert_eq!(failed, [ANSWERED]);
    }

    #[test]
    fn the_header_of_the_line_is_the_chat_facet_of_the_own_entity() {
        let header: FacetHeader = SocialFacet::Chat.header(OWN);
        let body = ChatCall::Send {
            channel: ChatChannel::Area.wire(),
            body: "!help".to_owned(),
            recipient_name: String::new(),
            links: Vec::new(),
        }
        .encode(header)
        .expect("encode");
        let read = nw_messages::social::SocialCall::decode(
            nw_messages::social::chat::SEND_CHAT_MESSAGE_TYPE_INDEX,
            &body,
        )
        .expect("a call")
        .expect("decode");
        assert_eq!(read.0, header);
    }
}
