// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client side of one REP connection, with no sockets: DTLS, the carrier, V3 registration,
//! and ping echo (M3 spec section 3.1).
//!
//! The caller passes in received datagrams and the time and sends what [`RepClient::poll`]
//! returns. The registration request goes out as soon as the carrier is connected.

use std::io::Write;

use nw_capture::{CLIENT_TO_SERVER, LedgerWriter, SERVER_TO_CLIENT};
use nw_carrier::{CarrierError, Connection, ConnectionState, Event, MESSAGE_CHANNEL};
use nw_common::time::unix_ms;
use nw_dtls::{DtlsClientConfig, DtlsError, DtlsSession};
use nw_reflect::{
    CLIENT_SESSION_LEN, Envelope, EnvelopeError, FramingError, LoginToken, PING_MSG_TYPE_INDEX,
    REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX, REGISTRATION_RESPONSE_MSG_TYPE_INDEX,
    RegistrationError, RegistrationRequestV3Msg, RegistrationResponseMsg, TypeId,
    decode_server_messages, encode_client_message,
};
use thiserror::Error;

/// The 16 bytes after the length of a client message: zero during registration (carrier captures
/// note section 5).
pub const ZERO_SESSION: [u8; CLIENT_SESSION_LEN] = [0; CLIENT_SESSION_LEN];

/// Failure of the client side.
#[derive(Debug, Error)]
pub enum RepError {
    /// DTLS failed.
    #[error(transparent)]
    Dtls(#[from] DtlsError),
    /// The carrier failed.
    #[error(transparent)]
    Carrier(#[from] CarrierError),
    /// A server payload did not split into envelopes.
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// An envelope did not decode.
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    /// A registration body did not decode or encode.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    /// A game message body cannot be built.
    #[error(transparent)]
    Message(#[from] nw_messages::MessageError),
    /// The route of a walking client cannot be walked.
    #[error(transparent)]
    Route(#[from] crate::route::RouteError),
}

/// Something the caller must know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepEvent {
    /// The carrier handshake is done; the registration request is on its way.
    Connected,
    /// The registration response arrived.
    Registered(RegistrationResponseMsg),
    /// The server sent a ping; the client sent the same 8 bytes back.
    Pinged,
    /// The server closed the connection.
    Closed {
        /// Reason byte.
        reason: u8,
    },
    /// Any other server message.
    Message(Received),
}

/// A server message and the carrier channel it came on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The carrier channel.
    pub channel: u8,
    /// The message.
    pub envelope: Envelope,
}

/// Context of every record of our own ledgers.
pub const OWN_LEDGER_CONTEXT: u64 = 1;

/// Where a client writes its plain carrier datagrams, as a capture ledger.
pub type OwnLedger = LedgerWriter<Box<dyn Write + Send>>;

/// A ledger for the first client made (`--ledger`); later clients get none.
pub struct LedgerSlot(std::sync::Mutex<Option<OwnLedger>>);

impl LedgerSlot {
    /// A slot that holds `ledger`, or nothing.
    pub fn new(ledger: Option<OwnLedger>) -> Self {
        Self(std::sync::Mutex::new(ledger))
    }

    /// `client` with the ledger when the slot still holds it.
    pub fn give(&self, client: RepClient) -> RepClient {
        let ledger = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match ledger {
            Some(ledger) => client.with_ledger(ledger),
            None => client,
        }
    }
}

/// The client side of one REP connection.
pub struct RepClient {
    dtls: DtlsSession,
    carrier: Connection,
    token: Option<LoginToken>,
    ledger: Option<OwnLedger>,
}

impl RepClient {
    /// A client that registers with `token` once DTLS and the carrier are up.
    ///
    /// # Errors
    ///
    /// Returns [`RepError::Dtls`] when the DTLS session cannot start.
    pub fn new(
        config: &DtlsClientConfig,
        mtu: u32,
        token: LoginToken,
        now_ms: u64,
    ) -> Result<Self, RepError> {
        Ok(Self {
            dtls: DtlsSession::connect(config, mtu)?,
            carrier: Connection::client(now_ms),
            token: Some(token),
            ledger: None,
        })
    }

    /// This client with every plain carrier datagram written to `ledger` (admin tools spec
    /// section 4.6): received ones after DTLS decryption, sent ones before DTLS encryption.
    #[must_use]
    pub fn with_ledger(mut self, ledger: OwnLedger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Whether the client still writes its ledger (a write error stops it; the run goes on).
    pub fn records_ledger(&self) -> bool {
        self.ledger.is_some()
    }

    fn record(&mut self, direction: u32, datagram: &[u8]) {
        let Some(ledger) = &mut self.ledger else {
            return;
        };
        if let Err(error) = ledger.record(unix_ms(), direction, OWN_LEDGER_CONTEXT, datagram) {
            eprintln!("the ledger stops: {error}");
            self.ledger = None;
        }
    }

    /// Handles one received datagram.
    ///
    /// # Errors
    ///
    /// Returns [`RepError`] when a layer fails; the caller drops the connection.
    pub fn receive(&mut self, now_ms: u64, datagram: &[u8]) -> Result<Vec<RepEvent>, RepError> {
        let mut events = Vec::new();
        for plaintext in self.dtls.receive(datagram)? {
            self.record(SERVER_TO_CLIENT, &plaintext);
            for event in self.carrier.receive(now_ms, &plaintext)? {
                match event {
                    Event::Connected => {
                        self.register()?;
                        events.push(RepEvent::Connected);
                    }
                    Event::Message(message) => {
                        for bytes in decode_server_messages(&message.payload)? {
                            let envelope = Envelope::decode(bytes)?;
                            events.push(self.message(message.channel, envelope)?);
                        }
                    }
                    Event::Closed { reason } => events.push(RepEvent::Closed { reason }),
                    Event::ConnectionWarning { .. } => {}
                }
            }
        }
        Ok(events)
    }

    fn register(&mut self) -> Result<(), RepError> {
        let Some(token) = self.token.take() else {
            return Ok(());
        };
        let body = RegistrationRequestV3Msg::with_login_token(token).encode()?;
        self.send(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX, body)
    }

    fn message(&mut self, channel: u8, envelope: Envelope) -> Result<RepEvent, RepError> {
        Ok(match envelope.type_index() {
            Some(REGISTRATION_RESPONSE_MSG_TYPE_INDEX) => {
                RepEvent::Registered(RegistrationResponseMsg::decode(&envelope.body)?)
            }
            Some(PING_MSG_TYPE_INDEX) => {
                self.send(PING_MSG_TYPE_INDEX, envelope.body)?;
                RepEvent::Pinged
            }
            _ => RepEvent::Message(Received { channel, envelope }),
        })
    }

    fn send(&mut self, type_index: u64, body: Vec<u8>) -> Result<(), RepError> {
        self.send_message(MESSAGE_CHANNEL, true, type_index, body)
    }

    /// Queues one client message of `type_index` on a carrier `channel`; the next poll sends it.
    ///
    /// # Errors
    ///
    /// Returns [`RepError`] when the carrier is not connected or the message does not encode.
    pub fn send_message(
        &mut self,
        channel: u8,
        reliable: bool,
        type_index: u64,
        body: Vec<u8>,
    ) -> Result<(), RepError> {
        let envelope = Envelope::message(TypeId::FileSlot(type_index), body).encode()?;
        let message = encode_client_message(&ZERO_SESSION, &envelope)?;
        self.carrier.send(channel, reliable, &message)?;
        Ok(())
    }

    /// Closes the carrier; the next poll sends the disconnect.
    ///
    /// # Errors
    ///
    /// Returns [`RepError::Carrier`] when the disconnect cannot be queued.
    pub fn close(&mut self) -> Result<(), RepError> {
        self.carrier.close()?;
        Ok(())
    }

    /// Whether the client closed its carrier ([`RepClient::close`]).
    pub fn is_closed(&self) -> bool {
        self.carrier.state() == ConnectionState::Closed
    }

    /// Datagrams to send now: DTLS handshake flights, then carrier datagrams once DTLS is up.
    ///
    /// # Errors
    ///
    /// Returns [`RepError`] when a layer fails.
    pub fn poll(&mut self, now_ms: u64) -> Result<Vec<Vec<u8>>, RepError> {
        self.dtls.poll_timer()?;
        if self.dtls.is_established() {
            for datagram in self.carrier.poll(now_ms)? {
                self.record(CLIENT_TO_SERVER, &datagram);
                self.dtls.send(&datagram)?;
            }
        }
        Ok(self.dtls.take_transmit())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use std::sync::LazyLock;

    use nw_carrier::ReceiveLimits;
    use nw_dtls::{COOKIE_SECRET_LEN, DtlsServerConfig, Identity, self_signed_identity};
    use nw_reflect::{
        REGISTRATION_RESPONSE_OBSERVED_FLAGS, REGISTRATION_STATUS_AUTHORIZED,
        decode_client_message, encode_server_messages,
    };

    use super::*;

    const MTU: u32 = 1200;
    const START_MS: u64 = 10_000;
    const STEP_MS: u64 = 10;
    const ROUNDS: u64 = 300;
    const SECRET: [u8; COOKIE_SECRET_LEN] = [3; COOKIE_SECRET_LEN];
    const PEER: &str = "192.0.2.20:50000";
    const PING_BODY: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 42];
    /// A game message the client does not read: channel 1, unreliable, as the replication.
    const OTHER_CHANNEL: u8 = 1;
    const OTHER_TYPE_INDEX: u64 = 446;
    const OTHER_BODY: [u8; 3] = [7, 8, 9];
    const TEST_NAME: &str = "test-rep";
    const VALID_DAYS: u32 = 30;
    /// One server identity for every test (RSA key generation is slow).
    pub(crate) static IDENTITY: LazyLock<Identity> =
        LazyLock::new(|| self_signed_identity(TEST_NAME, VALID_DAYS).expect("identity"));

    /// A minimal server: DTLS and the carrier only.
    struct Server {
        dtls: DtlsSession,
        carrier: Connection,
    }

    impl Server {
        fn new() -> Self {
            let config =
                DtlsServerConfig::new(&IDENTITY.certificate_pem, &IDENTITY.private_key_pem, SECRET)
                    .expect("server config");
            let mut carrier = Connection::server(START_MS);
            carrier.set_receive_limits(ReceiveLimits::REGISTRATION);
            Self {
                dtls: DtlsSession::accept(&config, PEER.parse::<SocketAddr>().expect("peer"), MTU)
                    .expect("accept"),
                carrier,
            }
        }

        fn receive(&mut self, now: u64, datagram: &[u8]) -> Vec<Envelope> {
            let mut envelopes = Vec::new();
            for plaintext in self.dtls.receive(datagram).expect("server dtls") {
                for event in self
                    .carrier
                    .receive(now, &plaintext)
                    .expect("server carrier")
                {
                    if let Event::Message(message) = event {
                        let client =
                            decode_client_message(&message.payload).expect("client message");
                        envelopes.push(Envelope::decode(client.envelope).expect("envelope"));
                    }
                }
            }
            envelopes
        }

        fn send(&mut self, type_index: u64, body: Vec<u8>) {
            self.send_on(MESSAGE_CHANNEL, true, type_index, body);
        }

        fn send_on(&mut self, channel: u8, reliable: bool, type_index: u64, body: Vec<u8>) {
            let envelope = Envelope::message(TypeId::FileSlot(type_index), body)
                .encode()
                .expect("envelope");
            let payload = encode_server_messages(&[&envelope]).expect("payload");
            self.carrier
                .send(channel, reliable, &payload)
                .expect("send");
        }

        fn poll(&mut self, now: u64) -> Vec<Vec<u8>> {
            self.dtls.poll_timer().expect("timer");
            if self.dtls.is_established() {
                for datagram in self.carrier.poll(now).expect("carrier poll") {
                    self.dtls.send(&datagram).expect("dtls send");
                }
            }
            self.dtls.take_transmit()
        }
    }

    fn token() -> LoginToken {
        LoginToken {
            ticket_id: b"ticket".to_vec(),
            signature: b"signature".to_vec(),
            ..LoginToken::default()
        }
    }

    /// Runs both sides until `done` returns true; returns the client events and server envelopes.
    fn run(
        client: &mut RepClient,
        server: &mut Server,
        mut on_server: impl FnMut(&mut Server, &Envelope),
        done: impl Fn(&[RepEvent]) -> bool,
    ) -> (Vec<RepEvent>, Vec<Envelope>) {
        let mut events = Vec::new();
        let mut envelopes = Vec::new();
        for round in 0..ROUNDS {
            let now = START_MS + round * STEP_MS;
            for datagram in client.poll(now).expect("client poll") {
                for envelope in server.receive(now, &datagram) {
                    on_server(server, &envelope);
                    envelopes.push(envelope);
                }
            }
            for datagram in server.poll(now) {
                events.extend(client.receive(now, &datagram).expect("client receive"));
            }
            if done(&events) {
                break;
            }
        }
        (events, envelopes)
    }

    #[test]
    fn a_closed_client_says_so() {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let mut client = RepClient::new(&config, MTU, token(), START_MS).expect("client");
        assert!(!client.is_closed());
        client.close().expect("close");
        assert!(client.is_closed());
    }

    #[test]
    fn the_client_connects_registers_and_reads_the_response() {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let mut client = RepClient::new(&config, MTU, token(), START_MS).expect("client");
        let mut server = Server::new();
        let response = RegistrationResponseMsg {
            uint32_at_0x8: REGISTRATION_STATUS_AUTHORIZED,
            uint64: 1,
            bytes_at_0x18: b"TOKEN".to_vec(),
            bytes_at_0x38: b"version".to_vec(),
            bytes_at_0x58: REGISTRATION_RESPONSE_OBSERVED_FLAGS,
        };
        let reply = response.encode().expect("response");
        let (events, envelopes) = run(
            &mut client,
            &mut server,
            |server, envelope| {
                if envelope.type_index() == Some(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX) {
                    server.send(REGISTRATION_RESPONSE_MSG_TYPE_INDEX, reply.clone());
                }
            },
            |events| {
                events
                    .iter()
                    .any(|event| matches!(event, RepEvent::Registered(_)))
            },
        );
        assert_eq!(events.first(), Some(&RepEvent::Connected));
        assert_eq!(events.last(), Some(&RepEvent::Registered(response)));
        let v3 = RegistrationRequestV3Msg::decode(&envelopes[0].body).expect("v3");
        assert_eq!(v3.login_token(), token());
    }

    #[test]
    fn the_client_reports_other_messages_with_their_channel() {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let mut client = RepClient::new(&config, MTU, token(), START_MS).expect("client");
        let mut server = Server::new();
        let (events, _) = run(
            &mut client,
            &mut server,
            |server, envelope| {
                if envelope.type_index() == Some(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX) {
                    server.send_on(OTHER_CHANNEL, false, OTHER_TYPE_INDEX, OTHER_BODY.to_vec());
                }
            },
            |events| {
                events
                    .iter()
                    .any(|event| matches!(event, RepEvent::Message(_)))
            },
        );
        let expected = Received {
            channel: OTHER_CHANNEL,
            envelope: Envelope::message(TypeId::FileSlot(OTHER_TYPE_INDEX), OTHER_BODY.to_vec()),
        };
        assert_eq!(events.last(), Some(&RepEvent::Message(expected)));
    }

    #[test]
    fn the_client_sends_a_message_on_any_channel() {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let mut client = RepClient::new(&config, MTU, token(), START_MS).expect("client");
        let mut server = Server::new();
        let (events, _) = run(
            &mut client,
            &mut server,
            |_, _| {},
            |events| events.contains(&RepEvent::Connected),
        );
        assert_eq!(events.last(), Some(&RepEvent::Connected));
        client
            .send_message(OTHER_CHANNEL, false, OTHER_TYPE_INDEX, OTHER_BODY.to_vec())
            .expect("send");
        let (_, envelopes) = run(&mut client, &mut server, |_, _| {}, |_| false);
        assert!(envelopes.iter().any(|envelope| {
            envelope.type_index() == Some(OTHER_TYPE_INDEX) && envelope.body == OTHER_BODY
        }));
    }

    #[test]
    fn the_client_echoes_pings() {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let mut client = RepClient::new(&config, MTU, token(), START_MS).expect("client");
        let mut server = Server::new();
        let (events, envelopes) = run(
            &mut client,
            &mut server,
            |server, envelope| {
                if envelope.type_index() == Some(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX) {
                    server.send(PING_MSG_TYPE_INDEX, PING_BODY.to_vec());
                }
            },
            |_| false,
        );
        assert!(events.contains(&RepEvent::Pinged));
        let echo = envelopes
            .iter()
            .find(|envelope| envelope.type_index() == Some(PING_MSG_TYPE_INDEX))
            .expect("echo");
        assert_eq!(echo.body, PING_BODY);
    }

    /// A writer into a buffer the test keeps.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that always fails.
    struct Broken;

    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn registered_with(client: &mut RepClient) {
        let mut server = Server::new();
        let response = RegistrationResponseMsg {
            uint32_at_0x8: REGISTRATION_STATUS_AUTHORIZED,
            uint64: 1,
            bytes_at_0x18: b"TOKEN".to_vec(),
            bytes_at_0x38: b"version".to_vec(),
            bytes_at_0x58: REGISTRATION_RESPONSE_OBSERVED_FLAGS,
        };
        let reply = response.encode().expect("response");
        let (events, _) = run(
            client,
            &mut server,
            |server, envelope| {
                if envelope.type_index() == Some(REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX) {
                    server.send(REGISTRATION_RESPONSE_MSG_TYPE_INDEX, reply.clone());
                }
            },
            |events| {
                events
                    .iter()
                    .any(|event| matches!(event, RepEvent::Registered(_)))
            },
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RepEvent::Registered(_)))
        );
    }

    fn ledger_client(out: impl std::io::Write + Send + 'static) -> RepClient {
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        RepClient::new(&config, MTU, token(), START_MS)
            .expect("client")
            .with_ledger(nw_capture::LedgerWriter::new(Box::new(out)))
    }

    #[test]
    fn a_ledger_client_records_both_directions() {
        let buffer = Shared::default();
        let mut client = ledger_client(buffer.clone());
        registered_with(&mut client);
        drop(client);
        let bytes = buffer.0.lock().expect("buffer").clone();
        let records = nw_capture::parse_ledger(&bytes).expect("ledger");
        for direction in [nw_capture::SERVER_TO_CLIENT, nw_capture::CLIENT_TO_SERVER] {
            assert!(records.iter().any(|record| record.direction == direction));
        }
        assert!(
            records
                .iter()
                .all(|record| record.context == OWN_LEDGER_CONTEXT)
        );
    }

    #[test]
    fn the_recorded_ledger_indexes_with_the_login_messages() {
        let buffer = Shared::default();
        let mut client = ledger_client(buffer.clone());
        registered_with(&mut client);
        drop(client);
        let bytes = buffer.0.lock().expect("buffer").clone();
        let index = nw_capture::SessionIndex::build(&bytes);
        assert!(index.failures.is_empty(), "{:?}", index.failures);
        let has = |type_index, direction| {
            index
                .types
                .iter()
                .any(|stats| stats.type_index == type_index && stats.direction == direction)
        };
        assert!(has(
            REGISTRATION_REQUEST_V3_MSG_TYPE_INDEX,
            nw_capture::CLIENT_TO_SERVER
        ));
        assert!(has(
            REGISTRATION_RESPONSE_MSG_TYPE_INDEX,
            nw_capture::SERVER_TO_CLIENT
        ));
    }

    #[test]
    fn a_failed_write_stops_recording_not_the_run() {
        let mut client = ledger_client(Broken);
        registered_with(&mut client);
        assert!(!client.records_ledger());
    }

    #[test]
    fn a_ledger_slot_goes_to_the_first_client_only() {
        let slot = LedgerSlot::new(Some(nw_capture::LedgerWriter::new(Box::new(
            Shared::default(),
        ))));
        let config = DtlsClientConfig::new(&IDENTITY.certificate_pem).expect("client config");
        let first = slot.give(RepClient::new(&config, MTU, token(), START_MS).expect("client"));
        let second = slot.give(RepClient::new(&config, MTU, token(), START_MS).expect("client"));
        assert!(first.records_ledger());
        assert!(!second.records_ledger());
        let empty = LedgerSlot::new(None);
        assert!(
            !empty
                .give(RepClient::new(&config, MTU, token(), START_MS).expect("client"))
                .records_ledger()
        );
    }
}
