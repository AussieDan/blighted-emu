// SPDX-License-Identifier: AGPL-3.0-or-later
//! One player connection in the gateway, with no sockets: DTLS, the carrier, the registration
//! step, pings and their round trip, and the time and rate limits (M3 spec sections 3.1, 6, and
//! 10).
//!
//! The server loop passes in datagrams and the time. When a V3 request arrives, [`Peer::receive`]
//! returns [`PeerEvent::RegistrationRequested`]; the loop checks the ticket (an async store call)
//! and hands the result to [`Peer::complete_registration`]. Any error closes the connection.

use std::collections::VecDeque;
use std::time::SystemTime;

use nw_carrier::{
    CarrierError, Connection, ConnectionState, Event, MESSAGE_CHANNEL, Message, ReceiveLimits,
};
use nw_dtls::{DtlsError, DtlsSession};
use nw_reflect::{
    Envelope, EnvelopeError, FramingError, PING_MSG_TYPE_INDEX,
    REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX, REGISTRATION_RESPONSE_MSG_TYPE_INDEX,
    REGISTRATION_STATUS_AUTHORIZED, REGISTRATION_STATUS_REFUSED, RegistrationError,
    RegistrationRequestV3Msg, TypeId, decode_client_message, encode_server_messages,
};
use thiserror::Error;

use crate::registration::{Admission, Refusal, TicketClaims, response};

/// Length of the rate window.
pub const RATE_WINDOW_MS: u64 = 1000;
/// Sent pings kept for their echo. Inferred: at one ping a second this is more than a minute of
/// pings without an echo; an older ping is forgotten, and its echo is then ignored.
pub const MAX_UNANSWERED_PINGS: usize = 64;
/// Length of a ping body: the gateway's send time, big-endian milliseconds (Observed: carrier
/// captures note section 8, an 8-byte body).
const PING_BODY_LEN: usize = 8;

/// Limits and timers of one connection (config `[limits]`, M3 spec section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerSettings {
    /// DTLS MTU.
    pub mtu: u32,
    /// Time from the first datagram to the registration response.
    pub registration_timeout_ms: u64,
    /// Time without a received datagram before the connection is dropped.
    pub silence_timeout_ms: u64,
    /// Time between server pings after registration.
    pub ping_interval_ms: u64,
    /// Datagrams one connection may send per second.
    pub datagrams_per_second: u32,
}

/// What the server loop must do for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// A V3 request arrived; check the ticket and call [`Peer::complete_registration`].
    RegistrationRequested(TicketClaims),
    /// A registered client sent a game message; forward its envelope bytes to the world.
    ClientMessage(Vec<u8>),
    /// The client echoed a ping: the time from sending it to its echo, on the gateway's clock.
    RoundTrip {
        /// Milliseconds.
        ms: u64,
    },
}

/// Server messages that wait for the registration response.
struct HeldMessages {
    channel: u8,
    envelopes: Vec<Vec<u8>>,
}

/// Why a connection ends.
#[derive(Debug, Error)]
pub enum PeerError {
    /// DTLS failed.
    #[error(transparent)]
    Dtls(#[from] DtlsError),
    /// The carrier failed or got a malformed frame.
    #[error(transparent)]
    Carrier(#[from] CarrierError),
    /// A client message has a bad length or CRC.
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// An envelope did not decode.
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    /// A registration body did not decode or encode.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    /// No random bytes for the session token.
    #[error("no random bytes: {0}")]
    Random(getrandom::Error),
    /// No registration in time.
    #[error("no registration in time")]
    RegistrationTimeout,
    /// No datagram in time.
    #[error("connection silent")]
    Silent,
    /// More datagrams per second than allowed.
    #[error("too many datagrams")]
    TooManyDatagrams,
    /// A message that this phase does not take.
    #[error("unexpected message type {type_index:?}")]
    UnexpectedMessage {
        /// Its type index; `None` for an inline UUID.
        type_index: Option<u64>,
    },
}

impl PeerError {
    /// A short name for logs and metrics.
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Dtls(_) => "dtls",
            Self::Carrier(_) | Self::Framing(_) | Self::Envelope(_) | Self::Registration(_) => {
                "malformed"
            }
            Self::Random(_) => "random",
            Self::RegistrationTimeout => "registration_timeout",
            Self::Silent => "silent",
            Self::TooManyDatagrams => "rate",
            Self::UnexpectedMessage { .. } => "unexpected_message",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    AwaitingRegistration,
    Checking,
    Registered,
    Refused,
}

/// One player connection.
pub struct Peer {
    id: u64,
    settings: PeerSettings,
    dtls: DtlsSession,
    carrier: Connection,
    phase: Phase,
    created_ms: u64,
    last_received_ms: u64,
    last_ping_ms: u64,
    /// Send times of the pings without an echo yet, oldest first.
    unanswered_pings: VecDeque<u64>,
    /// The round trip of the last echoed ping.
    last_round_trip_ms: Option<u64>,
    window_start_ms: u64,
    window_count: u32,
    held: Vec<HeldMessages>,
}

impl Peer {
    /// Connection `id` over a started DTLS session (see [`nw_dtls::VerifiedHello::start`]). The
    /// caller gives each connection its own id.
    pub fn new(id: u64, dtls: DtlsSession, settings: PeerSettings, now_ms: u64) -> Self {
        Self {
            id,
            settings,
            dtls,
            carrier: Connection::server(now_ms),
            phase: Phase::AwaitingRegistration,
            created_ms: now_ms,
            last_received_ms: now_ms,
            last_ping_ms: now_ms,
            unanswered_pings: VecDeque::new(),
            last_round_trip_ms: None,
            window_start_ms: now_ms,
            window_count: 0,
            held: Vec::new(),
        }
    }

    /// Whether the registration was accepted.
    pub fn is_registered(&self) -> bool {
        self.phase == Phase::Registered
    }

    /// Whether DTLS is done (the handshake no longer counts against the handshake limit).
    pub fn is_established(&self) -> bool {
        self.dtls.is_established()
    }

    /// The connection id given to [`Peer::new`].
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The round trip of this player at `now_ms`: the last echoed ping, raised to the age of the
    /// oldest ping without an echo, so a client that stops answering shows its delay before the
    /// silence timer drops it. `None` before the first echo.
    pub fn round_trip_ms(&self, now_ms: u64) -> Option<u64> {
        let waiting = self
            .unanswered_pings
            .front()
            .map_or(0, |sent| now_ms.saturating_sub(*sent));
        self.last_round_trip_ms
            .map(|measured| measured.max(waiting))
    }

    /// Whether the connection is over and its last datagrams are sent.
    pub fn is_finished(&self) -> bool {
        self.carrier.state() == ConnectionState::Closed
    }

    /// Handles one received datagram. After the handshake, only datagrams that DTLS accepts count
    /// against the rate limit and the silence timer, so a spoofer who knows the address can neither
    /// close the connection nor keep a dead one open.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError`] for a rate breach, a failure of any layer, or an unexpected message.
    pub fn receive(&mut self, now_ms: u64, datagram: &[u8]) -> Result<Vec<PeerEvent>, PeerError> {
        let established = self.dtls.is_established();
        if !established {
            self.accept_datagram(now_ms)?;
        }
        let plaintexts = self.dtls.receive(datagram)?;
        if established && !plaintexts.is_empty() {
            self.accept_datagram(now_ms)?;
        }
        let mut events = Vec::new();
        for plaintext in plaintexts {
            for event in self.carrier.receive(now_ms, &plaintext)? {
                match event {
                    Event::Message(message) => {
                        if let Some(event) = self.message(now_ms, &message)? {
                            events.push(event);
                        }
                    }
                    Event::ConnectionWarning { count } => {
                        tracing::debug!(count, "client raised a connection warning");
                    }
                    Event::Connected | Event::Closed { .. } => {}
                }
            }
        }
        Ok(events)
    }

    /// Counts a datagram from the peer and restarts the silence timer.
    fn accept_datagram(&mut self, now_ms: u64) -> Result<(), PeerError> {
        self.last_received_ms = now_ms;
        if now_ms.saturating_sub(self.window_start_ms) >= RATE_WINDOW_MS {
            self.window_start_ms = now_ms;
            self.window_count = 0;
        }
        self.window_count += 1;
        if self.window_count > self.settings.datagrams_per_second {
            return Err(PeerError::TooManyDatagrams);
        }
        Ok(())
    }

    fn message(&mut self, now_ms: u64, message: &Message) -> Result<Option<PeerEvent>, PeerError> {
        let client = decode_client_message(&message.payload)?;
        let envelope = Envelope::decode(client.envelope)?;
        match (self.phase, envelope.type_index()) {
            (Phase::AwaitingRegistration, Some(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX)) => {
                let request = RegistrationRequestV3Msg::decode(&envelope.body)?;
                self.phase = Phase::Checking;
                Ok(Some(PeerEvent::RegistrationRequested(
                    TicketClaims::from_token(&request.login_token()),
                )))
            }
            (Phase::Registered, Some(PING_MSG_TYPE_INDEX)) => {
                Ok(self.ping_echo(now_ms, &envelope.body))
            }
            (Phase::Registered, Some(index)) if index != REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX => {
                Ok(Some(PeerEvent::ClientMessage(client.envelope.to_vec())))
            }
            (_, type_index) => Err(PeerError::UnexpectedMessage { type_index }),
        }
    }

    /// The round trip of an echoed ping. Observed (carrier captures note section 8): the client
    /// sends a `PingMsg` with the same 8 bytes back. The body only picks the ping this gateway
    /// sent; the time comes from the gateway's own send record, so a client cannot make up a
    /// round trip. An echo of no kept ping is ignored. Pings are reliable and in order, so the
    /// pings sent before the echoed one get no echo of their own any more and are forgotten.
    fn ping_echo(&mut self, now_ms: u64, body: &[u8]) -> Option<PeerEvent> {
        let echoed = <[u8; PING_BODY_LEN]>::try_from(body)
            .ok()
            .map(u64::from_be_bytes)?;
        let Some(position) = self
            .unanswered_pings
            .iter()
            .position(|sent| *sent == echoed)
        else {
            tracing::debug!(echoed, "echo of no kept ping");
            return None;
        };
        let sent = self.unanswered_pings[position];
        self.unanswered_pings.drain(..=position);
        let ms = now_ms.saturating_sub(sent);
        self.last_round_trip_ms = Some(ms);
        Some(PeerEvent::RoundTrip { ms })
    }

    /// Keeps the send time of a ping for its echo, forgetting the oldest beyond
    /// [`MAX_UNANSWERED_PINGS`].
    fn remember_ping(&mut self, sent_ms: u64) {
        if self.unanswered_pings.len() == MAX_UNANSWERED_PINGS {
            self.unanswered_pings.pop_front();
        }
        self.unanswered_pings.push_back(sent_ms);
    }

    /// Sends the registration response for the checked ticket. A refused connection sends status
    /// 1 and then a disconnect; a game master's gets the GM flag. A result for a peer that is not
    /// waiting for one is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError`] when the response cannot be built or queued.
    pub fn complete_registration(
        &mut self,
        now: SystemTime,
        result: Result<Admission, Refusal>,
        server_version: &str,
    ) -> Result<(), PeerError> {
        if self.phase != Phase::Checking {
            return Ok(());
        }
        let status = if result.is_ok() {
            REGISTRATION_STATUS_AUTHORIZED
        } else {
            REGISTRATION_STATUS_REFUSED
        };
        let game_master = result.is_ok_and(|admission| admission.game_master);
        let body = response(status, now, server_version, game_master)
            .map_err(PeerError::Random)?
            .encode()?;
        self.send_message(REGISTRATION_RESPONSE_MSG_TYPE_INDEX, body)?;
        let held = std::mem::take(&mut self.held);
        if result.is_ok() {
            self.phase = Phase::Registered;
            self.carrier.set_receive_limits(ReceiveLimits::SESSION);
            for messages in held {
                self.send_now(messages.channel, true, &messages.envelopes)?;
            }
        } else {
            self.phase = Phase::Refused;
            self.carrier.close()?;
        }
        Ok(())
    }

    fn send_message(&mut self, type_index: u64, body: Vec<u8>) -> Result<(), PeerError> {
        let envelope = Envelope::message(TypeId::FileSlot(type_index), body).encode()?;
        self.send_now(MESSAGE_CHANNEL, true, &[envelope])
    }

    fn send_now(
        &mut self,
        channel: u8,
        reliable: bool,
        envelopes: &[Vec<u8>],
    ) -> Result<(), PeerError> {
        let envelopes: Vec<&[u8]> = envelopes.iter().map(Vec::as_slice).collect();
        let payload = encode_server_messages(&envelopes)?;
        self.carrier.send(channel, reliable, &payload)?;
        Ok(())
    }

    /// Sends server message envelopes from the world on a carrier `channel`. While the ticket is
    /// checked, reliable messages wait for the registration response and unreliable ones are
    /// dropped (the world sends the full state again every tick). A refused or closed connection
    /// takes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError`] for a channel above 3 or a payload that does not encode.
    pub fn send_envelopes(
        &mut self,
        channel: u8,
        reliable: bool,
        envelopes: &[Vec<u8>],
    ) -> Result<(), PeerError> {
        match self.phase {
            Phase::Registered => self.send_now(channel, reliable, envelopes),
            Phase::Checking if reliable => {
                self.held.push(HeldMessages {
                    channel,
                    envelopes: envelopes.to_vec(),
                });
                Ok(())
            }
            Phase::AwaitingRegistration | Phase::Checking | Phase::Refused => Ok(()),
        }
    }

    /// Closes the connection; the next poll sends the disconnect.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::Carrier`] when the disconnect cannot be queued.
    pub fn close(&mut self) -> Result<(), PeerError> {
        self.carrier.close()?;
        Ok(())
    }

    /// Datagrams to send now: DTLS flights, carrier datagrams, and pings.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::RegistrationTimeout`] or [`PeerError::Silent`] when a timer runs out,
    /// and any layer's error.
    pub fn poll(&mut self, now_ms: u64) -> Result<Vec<Vec<u8>>, PeerError> {
        let registering = matches!(self.phase, Phase::AwaitingRegistration | Phase::Checking);
        if registering
            && now_ms.saturating_sub(self.created_ms) >= self.settings.registration_timeout_ms
        {
            return Err(PeerError::RegistrationTimeout);
        }
        if now_ms.saturating_sub(self.last_received_ms) >= self.settings.silence_timeout_ms {
            return Err(PeerError::Silent);
        }
        self.dtls.poll_timer()?;
        if self.phase == Phase::Registered
            && now_ms.saturating_sub(self.last_ping_ms) >= self.settings.ping_interval_ms
        {
            self.last_ping_ms = now_ms;
            self.remember_ping(now_ms);
            self.send_message(PING_MSG_TYPE_INDEX, now_ms.to_be_bytes().to_vec())?;
        }
        if self.dtls.is_established() {
            for datagram in self.carrier.poll(now_ms)? {
                self.dtls.send(&datagram)?;
            }
        }
        Ok(self.dtls.take_transmit())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;
    use std::time::UNIX_EPOCH;

    use nw_dtls::{
        COOKIE_SECRET_LEN, DtlsClientConfig, DtlsServerConfig, Identity, self_signed_identity,
    };
    use nw_reflect::LoginToken;
    use nw_testclient::rep::{RepClient, RepEvent};

    use super::*;

    const MTU: u32 = 1200;
    const START_MS: u64 = 50_000;
    const STEP_MS: u64 = 10;
    const ROUNDS: u64 = 400;
    const SECRET: [u8; COOKIE_SECRET_LEN] = [9; COOKIE_SECRET_LEN];
    const PEER: &str = "192.0.2.30:41000";
    const VERSION: &str = "[TEST].Javelin.1.365.6031.1";
    const VALID_DAYS: u32 = 30;
    const CONNECTION_ID: u64 = 1;
    const APPLICATION_DATA_RECORD: u8 = 23;
    const DTLS_1_2: [u8; 2] = [0xfe, 0xfd];
    const FORGED_RECORD_BODY_LEN: u8 = 48;
    /// Client movement (type 1589) goes on channel 1, unreliable.
    const INPUT_CHANNEL: u8 = 1;
    const INPUT_TYPE_INDEX: u64 = 1589;
    /// A login message (type 446, channel 0, reliable).
    const LOGIN_TYPE_INDEX: u64 = 446;
    /// A replication bundle (type 8, channel 1, unreliable).
    const BUNDLE_TYPE_INDEX: u64 = 8;
    const BODY: [u8; 2] = [1, 2];
    const SETTINGS: PeerSettings = PeerSettings {
        mtu: MTU,
        registration_timeout_ms: 10_000,
        silence_timeout_ms: 10_000,
        ping_interval_ms: 1000,
        datagrams_per_second: 400,
    };
    static IDENTITY: LazyLock<Identity> =
        LazyLock::new(|| self_signed_identity("test-gateway", VALID_DAYS).expect("identity"));

    fn record() -> Admission {
        Admission::default()
    }

    fn token() -> LoginToken {
        LoginToken {
            ticket_id: b"ticket".to_vec(),
            signature: b"signature".to_vec(),
            character_id: b"character".to_vec(),
            ..LoginToken::default()
        }
    }

    fn sides(settings: PeerSettings) -> (RepClient, Peer) {
        let server =
            DtlsServerConfig::new(&IDENTITY.certificate_pem, &IDENTITY.private_key_pem, SECRET)
                .expect("server config");
        let client = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        (
            RepClient::new(&client, MTU, token(), START_MS).expect("client"),
            Peer::new(
                CONNECTION_ID,
                DtlsSession::accept(&server, PEER.parse().expect("peer"), settings.mtu)
                    .expect("accept"),
                settings,
                START_MS,
            ),
        )
    }

    /// Runs both sides from `start` until `stop` says so; `answer` decides each registration.
    fn run(
        client: &mut RepClient,
        peer: &mut Peer,
        start: u64,
        answer: Result<Admission, Refusal>,
        stop: impl Fn(&[RepEvent]) -> bool,
    ) -> (Vec<RepEvent>, Vec<PeerEvent>, u64) {
        run_with(client, peer, start, answer, |_| {}, stop)
    }

    /// [`run`], and `before_answer` runs on the peer between a registration request and its
    /// answer.
    fn run_with(
        client: &mut RepClient,
        peer: &mut Peer,
        start: u64,
        answer: Result<Admission, Refusal>,
        before_answer: impl Fn(&mut Peer),
        stop: impl Fn(&[RepEvent]) -> bool,
    ) -> (Vec<RepEvent>, Vec<PeerEvent>, u64) {
        let mut client_events = Vec::new();
        let mut peer_events = Vec::new();
        let mut now = start;
        for _ in 0..ROUNDS {
            now += STEP_MS;
            for datagram in client.poll(now).expect("client poll") {
                for event in peer.receive(now, &datagram).expect("peer receive") {
                    if let PeerEvent::RegistrationRequested(_) = &event {
                        before_answer(peer);
                        peer.complete_registration(UNIX_EPOCH, answer, VERSION)
                            .expect("complete");
                    }
                    peer_events.push(event);
                }
            }
            for datagram in peer.poll(now).expect("peer poll") {
                client_events.extend(client.receive(now, &datagram).expect("client receive"));
            }
            if stop(&client_events) {
                break;
            }
        }
        (client_events, peer_events, now)
    }

    fn status(events: &[RepEvent]) -> Option<u32> {
        events.iter().find_map(|event| match event {
            RepEvent::Registered(response) => Some(response.uint32_at_0x8),
            _ => None,
        })
    }

    #[test]
    fn an_accepted_registration_gets_status_0_and_pings() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (events, peer_events, _) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            events.contains(&RepEvent::Pinged)
        });
        assert_eq!(
            peer_events,
            vec![PeerEvent::RegistrationRequested(TicketClaims::from_token(
                &token()
            ))]
        );
        assert_eq!(status(&events), Some(REGISTRATION_STATUS_AUTHORIZED));
        assert!(peer.is_registered());
        assert!(events.contains(&RepEvent::Pinged));
    }

    #[test]
    fn a_refused_registration_gets_status_1_and_a_disconnect() {
        let (mut client, mut peer) = sides(SETTINGS);
        let (events, _, _) = run(
            &mut client,
            &mut peer,
            START_MS,
            Err(Refusal::UnknownTicket),
            |events| {
                events
                    .iter()
                    .any(|event| matches!(event, RepEvent::Closed { .. }))
            },
        );
        assert_eq!(status(&events), Some(REGISTRATION_STATUS_REFUSED));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RepEvent::Closed { .. }))
        );
        assert!(peer.is_finished());
    }

    #[test]
    fn a_connection_without_registration_times_out() {
        let (_, mut peer) = sides(SETTINGS);
        let late = START_MS + SETTINGS.registration_timeout_ms;
        assert!(matches!(
            peer.poll(late),
            Err(PeerError::RegistrationTimeout)
        ));
    }

    #[test]
    fn a_silent_registered_connection_is_dropped() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            status(events).is_some()
        });
        assert!(matches!(
            peer.poll(now + SETTINGS.silence_timeout_ms),
            Err(PeerError::Silent)
        ));
    }

    /// An application data record in epoch 1 with a wrong MAC, as a spoofer can send it.
    fn forged_record() -> Vec<u8> {
        let mut record = vec![
            APPLICATION_DATA_RECORD,
            DTLS_1_2[0],
            DTLS_1_2[1],
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            1,
            0,
            FORGED_RECORD_BODY_LEN,
        ];
        record.resize(record.len() + usize::from(FORGED_RECORD_BODY_LEN), 0);
        record
    }

    #[test]
    fn forged_datagrams_neither_close_nor_keep_alive_a_connection() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            status(events).is_some()
        });
        let later = now + SETTINGS.silence_timeout_ms / 2;
        for _ in 0..2 * SETTINGS.datagrams_per_second {
            let events = peer
                .receive(later, &forged_record())
                .expect("forged datagram ignored");
            assert!(events.is_empty());
        }
        assert!(matches!(
            peer.poll(now + SETTINGS.silence_timeout_ms),
            Err(PeerError::Silent)
        ));
    }

    #[test]
    fn a_result_for_a_peer_that_is_not_checking_is_ignored() {
        let (mut client, mut peer) = sides(SETTINGS);
        let (_, _, now) = run(
            &mut client,
            &mut peer,
            START_MS,
            Err(Refusal::Expired),
            |events| events.contains(&RepEvent::Connected),
        );
        assert!(!peer.is_registered());
        let entry = record();
        peer.complete_registration(SystemTime::now(), Ok(entry), VERSION)
            .expect("ignored");
        assert!(!peer.is_registered());
        let mut events = Vec::new();
        for datagram in peer.poll(now).expect("poll") {
            events.extend(client.receive(now, &datagram).expect("client"));
        }
        assert_eq!(status(&events), None);
    }

    /// An envelope of `type_index` with a short body.
    fn envelope(type_index: u64) -> Vec<u8> {
        Envelope::message(TypeId::FileSlot(type_index), BODY.to_vec())
            .encode()
            .expect("envelope")
    }

    fn messages(events: &[RepEvent]) -> Vec<(u8, Option<u64>)> {
        events
            .iter()
            .filter_map(|event| match event {
                RepEvent::Message(received) => {
                    Some((received.channel, received.envelope.type_index()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn registered_client_messages_are_forwarded_but_pings_are_not() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (events, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            events.contains(&RepEvent::Pinged)
        });
        assert!(events.contains(&RepEvent::Pinged));
        client
            .send_message(INPUT_CHANNEL, false, INPUT_TYPE_INDEX, BODY.to_vec())
            .expect("send");
        let (_, peer_events, _) = run(&mut client, &mut peer, now, Ok(entry), |_| false);
        let forwarded: Vec<Option<u64>> = peer_events
            .iter()
            .filter_map(|event| match event {
                PeerEvent::ClientMessage(bytes) => {
                    Some(Envelope::decode(bytes).expect("envelope").type_index())
                }
                PeerEvent::RegistrationRequested(_) | PeerEvent::RoundTrip { .. } => None,
            })
            .collect();
        assert_eq!(forwarded, vec![Some(INPUT_TYPE_INDEX)]);
    }

    fn round_trips(events: &[PeerEvent]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|event| match event {
                PeerEvent::RoundTrip { ms } => Some(*ms),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_echoed_ping_gives_its_round_trip_on_the_gateway_clock() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            events.contains(&RepEvent::Pinged)
        });
        assert_eq!(peer.round_trip_ms(now), None);
        let (_, peer_events, now) = run(&mut client, &mut peer, now, Ok(entry), |_| false);
        // The test client echoes at once; the echo arrives with its next poll, one step later.
        let measured = round_trips(&peer_events);
        assert!(!measured.is_empty());
        assert!(measured.iter().all(|ms| *ms == STEP_MS), "{measured:?}");
        assert!(peer.round_trip_ms(now).is_some_and(|ms| ms >= STEP_MS));
    }

    #[test]
    fn a_ping_without_an_echo_raises_the_round_trip() {
        const LAG_MS: u64 = 2500;
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, peer_events, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |_| false);
        assert!(!round_trips(&peer_events).is_empty());
        // The next ping goes out but the client never sees it.
        let sent = now + SETTINGS.ping_interval_ms;
        peer.poll(sent).expect("ping");
        assert!(
            peer.round_trip_ms(sent + LAG_MS)
                .is_some_and(|ms| ms >= LAG_MS)
        );
    }

    #[test]
    fn only_an_echo_of_a_kept_ping_counts() {
        const FORGED_SEND_MS: u64 = 1;
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            status(events).is_some()
        });
        let sent = now + SETTINGS.ping_interval_ms;
        peer.poll(sent).expect("ping");
        let later = sent + STEP_MS;
        assert_eq!(peer.ping_echo(later, &FORGED_SEND_MS.to_be_bytes()), None);
        assert_eq!(peer.ping_echo(later, &BODY), None);
        assert_eq!(peer.round_trip_ms(later), None);
        assert_eq!(
            peer.ping_echo(later, &sent.to_be_bytes()),
            Some(PeerEvent::RoundTrip { ms: STEP_MS })
        );
        // A second echo of the same ping finds nothing.
        assert_eq!(peer.ping_echo(later, &sent.to_be_bytes()), None);
        assert_eq!(peer.round_trip_ms(later), Some(STEP_MS));
    }

    #[test]
    fn at_most_the_last_pings_wait_for_an_echo() {
        let (_, mut peer) = sides(SETTINGS);
        let first = START_MS;
        let mut now = first;
        peer.remember_ping(now);
        for _ in 0..MAX_UNANSWERED_PINGS {
            now += SETTINGS.ping_interval_ms;
            peer.remember_ping(now);
        }
        assert_eq!(peer.unanswered_pings.len(), MAX_UNANSWERED_PINGS);
        assert_eq!(peer.ping_echo(now, &first.to_be_bytes()), None);
        assert!(peer.ping_echo(now, &now.to_be_bytes()).is_some());
        assert!(peer.unanswered_pings.is_empty());
    }

    #[test]
    fn server_messages_wait_for_the_registration_response() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (events, _, now) = run_with(
            &mut client,
            &mut peer,
            START_MS,
            Ok(entry),
            |peer| {
                peer.send_envelopes(MESSAGE_CHANNEL, true, &[envelope(LOGIN_TYPE_INDEX)])
                    .expect("held");
                peer.send_envelopes(INPUT_CHANNEL, false, &[envelope(BUNDLE_TYPE_INDEX)])
                    .expect("dropped");
            },
            |events| !messages(events).is_empty(),
        );
        assert!(matches!(events.first(), Some(RepEvent::Connected)));
        assert!(matches!(events.get(1), Some(RepEvent::Registered(_))));
        assert_eq!(
            messages(&events),
            vec![(MESSAGE_CHANNEL, Some(LOGIN_TYPE_INDEX))]
        );
        peer.send_envelopes(INPUT_CHANNEL, false, &[envelope(BUNDLE_TYPE_INDEX)])
            .expect("sent");
        let (events, _, _) = run(&mut client, &mut peer, now, Ok(entry), |events| {
            !messages(events).is_empty()
        });
        assert_eq!(
            messages(&events),
            vec![(INPUT_CHANNEL, Some(BUNDLE_TYPE_INDEX))]
        );
    }

    #[test]
    fn a_refused_registration_drops_the_held_messages() {
        let (mut client, mut peer) = sides(SETTINGS);
        let (events, _, _) = run_with(
            &mut client,
            &mut peer,
            START_MS,
            Err(Refusal::WorldRefused),
            |peer| {
                peer.send_envelopes(MESSAGE_CHANNEL, true, &[envelope(LOGIN_TYPE_INDEX)])
                    .expect("held");
            },
            |events| {
                events
                    .iter()
                    .any(|event| matches!(event, RepEvent::Closed { .. }))
            },
        );
        assert_eq!(status(&events), Some(REGISTRATION_STATUS_REFUSED));
        assert!(messages(&events).is_empty());
    }

    #[test]
    fn close_ends_a_registered_connection() {
        let (mut client, mut peer) = sides(SETTINGS);
        let entry = record();
        let (_, _, now) = run(&mut client, &mut peer, START_MS, Ok(entry), |events| {
            status(events).is_some()
        });
        peer.close().expect("close");
        let (events, _, _) = run(&mut client, &mut peer, now, Ok(entry), |events| {
            events
                .iter()
                .any(|event| matches!(event, RepEvent::Closed { .. }))
        });
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RepEvent::Closed { .. }))
        );
        assert!(peer.is_finished());
    }

    #[test]
    fn too_many_datagrams_close_the_connection() {
        let few = PeerSettings {
            datagrams_per_second: 2,
            ..SETTINGS
        };
        let (_, mut peer) = sides(few);
        let noise = [0_u8; 4];
        let _ = peer.receive(START_MS, &noise);
        let _ = peer.receive(START_MS, &noise);
        assert!(matches!(
            peer.receive(START_MS, &noise),
            Err(PeerError::TooManyDatagrams)
        ));
    }
}
