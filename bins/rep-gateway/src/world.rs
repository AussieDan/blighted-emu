// SPDX-License-Identifier: AGPL-3.0-or-later
//! The link from the gateway to the world (M4 spec sections 3.1, 6 and 11; M5 spec sections 3 and
//! 8): joins go to `world-director`; client messages and departures go to the sim of each
//! connection; server messages and closes come in.
//!
//! [`World`] is what the server loop uses; [`BusWorld`] is the NATS version. It keeps the route of
//! each connection: a join sets it, a route message of a handoff changes it, and a connection
//! whose sim leaves the cell table is resumed through the director. Inputs and heartbeats go
//! through publishing queues ([`Publisher`]), so each sim gets the inputs in the order the clients
//! sent them, and a lost NATS connection never blocks the loop. Inputs that do not fit the queue
//! wait for the next flush (up to [`INPUT_BACKLOG_BYTES`] for each sim): above that an unreliable
//! input is dropped and a reliable one closes its connection (a client whose reliable message is
//! lost no longer agrees with its sim).

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use nw_bus::{
    Bus, BusError, BusId, CellTable, ConnectionRef, GatewayOutput, Heartbeat, InputBatch, Join,
    JoinReply, MAX_BATCH_BYTES, OutputBatch, Publisher, ResumeOf, SimInput, Subscription,
    split_batches, subject,
};
use nw_common::time::unix_ms;
use nw_login::queue::EntryRecord;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::events::{Delivery, EventReceiver, EventSender, event_queue};
use crate::registration::{Admission, Refusal};
use crate::routes::{Hold, Resume, Routes, Stray};
use crate::workers::worker_of;

/// Input batches that may wait for the publishing task: about 0.3 s of batches of eight sims.
pub const INPUT_QUEUE: usize = 256;
/// Metric: bundles lost because a worker's queue held the most bundles.
pub const METRIC_EVENTS_DROPPED: &str = "rep_gateway_world_events_dropped_total";
/// Tries of one resume: the first and one more after a request without an answer.
const RESUME_ATTEMPTS: usize = 2;
/// Time between two input batches to one sim (the gateway loop tick).
pub const INPUT_FLUSH: Duration = Duration::from_millis(10);
/// Bundles that may wait for one worker; other events always wait.
pub const EVENT_QUEUE: usize = 4096;
/// Heartbeats that may wait for NATS.
const HEARTBEAT_QUEUE: usize = 16;
/// Metric: unreliable inputs dropped because the publishing task is behind.
pub const METRIC_INPUTS_DROPPED: &str = "rep_gateway_world_inputs_dropped_total";
/// Metric: connections closed because their reliable inputs could not wait.
pub const METRIC_INPUT_CLOSES: &str = "rep_gateway_world_input_closes_total";
/// Bytes of inputs that may wait for one sim when the publishing task is behind. Inferred: four
/// full batches, about 1 s of a busy sim's inputs.
pub const INPUT_BACKLOG_BYTES: usize = 4 * MAX_BATCH_BYTES;
/// Metric: inputs of a connection without a route (resuming, or gone).
pub const METRIC_INPUTS_WITHOUT_ROUTE: &str = "rep_gateway_inputs_without_route_total";
/// Metric: sims told to drop a copy that nobody routes to.
pub const METRIC_STRAYS_EVICTED: &str = "rep_gateway_strays_evicted_total";
/// Metric: resumes by `result`.
pub const METRIC_RESUMES: &str = "rep_gateway_resumes_total";
const RESULT_LABEL: &str = "result";
const RESUMED: &str = "resumed";
const CLOSED: &str = "closed";

/// What the world tells the server loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldEvent {
    /// Server message envelopes for one connection, on one carrier channel.
    Messages {
        /// The sim that sent them.
        sim: BusId,
        /// The connection id.
        connection: u64,
        /// The carrier channel.
        channel: u8,
        /// Whether the carrier sends them reliably.
        reliable: bool,
        /// The envelopes, in order.
        envelopes: Vec<Vec<u8>>,
    },
    /// Close the connection.
    Close {
        /// The connection id.
        connection: u64,
    },
}

/// The world as the server loop sees it.
pub trait World: Clone + Send + Sync + 'static {
    /// Asks the world to take the character of `record` on `connection`; what it said about the
    /// account.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal::WorldRefused`] or [`Refusal::WorldUnavailable`].
    fn join(
        &self,
        connection: u64,
        record: &EntryRecord,
    ) -> impl Future<Output = Result<Admission, Refusal>> + Send;

    /// Forwards one client message envelope, unchanged, without waiting; `reliable` when the
    /// client sent it reliably (the world must not lose it).
    fn client_message(&self, connection: u64, envelope: Vec<u8>, reliable: bool);

    /// Tells the world that a connection is gone, without waiting.
    fn left(&self, connection: u64);

    /// `sim` sent messages for a connection this gateway does not have (for example after a
    /// join that timed out); it hears that the connection left.
    fn unknown(&self, connection: u64, sim: &BusId);
}

/// The link settings (config keys `gateway_id`, `join_timeout_ms`, `heartbeat_interval_ms`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldLinkSettings {
    /// The world.
    pub world_id: Uuid,
    /// This gateway in bus subjects.
    pub gateway_id: BusId,
    /// Longest wait for a join or resume answer.
    pub join_timeout: Duration,
    /// Time between heartbeats.
    pub heartbeat_interval: Duration,
    /// Most resumes that wait for the director at one time.
    pub max_resumes_in_flight: usize,
}

/// The inputs of each sim that wait for the next batch, and their bytes.
type PendingInputs = BTreeMap<BusId, (Vec<SimInput>, usize)>;

/// How an input is treated when its sim's backlog is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backlog {
    /// An unreliable client message: dropped.
    Droppable,
    /// A reliable client message: its connection closes.
    Reliable,
    /// A departure: always kept (a sim must hear that a player left).
    Always,
}

/// Whether an input of `kind` and `size` bytes joins a backlog of `waiting` bytes.
fn backlog_admits(kind: Backlog, waiting: usize, size: usize) -> bool {
    kind == Backlog::Always || waiting.saturating_add(size) <= INPUT_BACKLOG_BYTES
}

/// Puts the `unsent` inputs of a sim back before the ones that came since, so the order holds.
fn requeue(pending: &mut PendingInputs, sim: BusId, unsent: Vec<SimInput>) {
    let bytes: usize = unsent.iter().map(SimInput::size_bytes).sum();
    let (inputs, waiting) = pending.entry(sim).or_default();
    let newer = std::mem::replace(inputs, unsent);
    inputs.extend(newer);
    *waiting = waiting.saturating_add(bytes);
}

/// The world over NATS.
#[derive(Debug, Clone)]
pub struct BusWorld {
    bus: Bus,
    settings: Arc<WorldLinkSettings>,
    inputs: Publisher<InputBatch>,
    pending: Arc<Mutex<PendingInputs>>,
    routes: Arc<Mutex<Routes>>,
    /// Permits of the resumes in flight.
    resumes: Arc<Semaphore>,
    /// The events of each worker, by worker number.
    events: Arc<Vec<EventSender>>,
}

impl BusWorld {
    /// Subscribes to this gateway's output and the cell table, and starts the tasks that publish
    /// inputs and heartbeats. Returns the link and the events of each of `workers` workers.
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] when a subscription cannot start.
    pub async fn start(
        bus: Bus,
        settings: WorldLinkSettings,
        workers: usize,
    ) -> Result<(Self, Vec<EventReceiver>), BusError> {
        let world_id = settings.world_id;
        let outputs = bus
            .subscribe::<OutputBatch>(subject::gateway_output(world_id, &settings.gateway_id))
            .await?;
        let tables = bus
            .subscribe::<CellTable>(subject::director_table(world_id))
            .await?;
        bus.flush().await?;
        let settings = Arc::new(settings);
        let (events, receivers): (Vec<_>, Vec<_>) = (0..workers.max(1))
            .map(|_| event_queue(EVENT_QUEUE))
            .unzip();
        let world = Self {
            inputs: Publisher::start(bus.clone(), INPUT_QUEUE),
            pending: Arc::default(),
            routes: Arc::default(),
            resumes: Arc::new(Semaphore::new(settings.max_resumes_in_flight)),
            bus: bus.clone(),
            settings: Arc::clone(&settings),
            events: Arc::new(events),
        };
        tokio::spawn(world.clone().forward_outputs(outputs));
        tokio::spawn(world.clone().follow_tables(tables));
        tokio::spawn(world.clone().flush_inputs());
        tokio::spawn(heartbeats(
            Publisher::start(bus, HEARTBEAT_QUEUE),
            settings,
            Arc::clone(&world.events),
        ));
        Ok((world, receivers))
    }

    /// Sends `event` to the worker of `connection`. A worker that is behind loses bundles
    /// (counted), so one slow worker never stops the others. Returns `false` when that worker is
    /// gone.
    fn event(&self, connection: u64, event: WorldEvent) -> bool {
        let Some(worker) = self.events.get(worker_of(connection)) else {
            tracing::warn!(connection, "world event for a worker this gateway lacks");
            return true;
        };
        match worker.send(event) {
            Delivery::Queued => true,
            Delivery::Dropped => {
                metrics::counter!(METRIC_EVENTS_DROPPED).increment(1);
                true
            }
            Delivery::Gone => false,
        }
    }

    fn pending(&self) -> MutexGuard<'_, PendingInputs> {
        // A panic while the lock was held leaves at worst some inputs unsent.
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends the inputs that wait for `sim` (or for every sim), in batches. The batches that do
    /// not fit the publishing queue wait for the next flush.
    fn send_inputs(&self, batches: PendingInputs) {
        for (sim, (inputs, _)) in batches {
            let subject = subject::sim_input(self.settings.world_id, &sim);
            let mut unsent = Vec::new();
            for inputs in split_batches(inputs, SimInput::size_bytes) {
                if !unsent.is_empty() {
                    unsent.extend(inputs);
                    continue;
                }
                let batch = InputBatch {
                    forwarded: false,
                    inputs,
                };
                if let Err(batch) = self.inputs.try_send(subject.clone(), batch) {
                    unsent = batch.inputs;
                }
            }
            if !unsent.is_empty() {
                tracing::debug!(%sim, inputs = unsent.len(), "publishing queue full; inputs wait");
                requeue(&mut self.pending(), sim, unsent);
            }
        }
    }

    /// Sends the waiting inputs every [`INPUT_FLUSH`]. Ends when every worker is gone.
    async fn flush_inputs(self) {
        let mut timer = tokio::time::interval(INPUT_FLUSH);
        loop {
            timer.tick().await;
            if self.events.iter().all(EventSender::is_closed) {
                return;
            }
            let waiting = std::mem::take(&mut *self.pending());
            self.send_inputs(waiting);
        }
    }

    fn routes(&self) -> MutexGuard<'_, Routes> {
        // A panic while the lock was held cannot leave a route half written.
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn connection(&self, connection: u64) -> ConnectionRef {
        ConnectionRef {
            gateway: self.settings.gateway_id.clone(),
            connection,
        }
    }

    /// Queues `input` for the next batch of `sim`; a full batch goes out at once. Above
    /// [`INPUT_BACKLOG_BYTES`] an input of `kind` [`Backlog::Droppable`] is dropped and one of
    /// [`Backlog::Reliable`] closes `connection`.
    fn input(&self, sim: &BusId, connection: u64, input: SimInput, kind: Backlog) {
        let size = input.size_bytes();
        let full = {
            let mut pending = self.pending();
            let (inputs, bytes) = pending.entry(sim.clone()).or_default();
            if !backlog_admits(kind, *bytes, size) {
                drop(pending);
                self.refuse_input(sim, connection, kind);
                return;
            }
            *bytes += size;
            inputs.push(input);
            if *bytes >= MAX_BATCH_BYTES {
                pending.remove_entry(sim)
            } else {
                None
            }
        };
        if let Some((sim, waiting)) = full {
            self.send_inputs(BTreeMap::from([(sim, waiting)]));
        }
    }

    /// An input that its sim's full backlog does not take.
    fn refuse_input(&self, sim: &BusId, connection: u64, kind: Backlog) {
        if kind == Backlog::Reliable {
            metrics::counter!(METRIC_INPUT_CLOSES).increment(1);
            tracing::warn!(
                connection,
                %sim,
                "reliable input does not fit the sim's backlog; the connection closes"
            );
            self.event(connection, WorldEvent::Close { connection });
        } else {
            metrics::counter!(METRIC_INPUTS_DROPPED).increment(1);
        }
    }

    fn send_left(&self, connection: u64, sim: &BusId) {
        self.input(
            sim,
            connection,
            SimInput::Left {
                connection: self.connection(connection),
            },
            Backlog::Always,
        );
    }

    /// Queues a client message of `connection` for `sim`.
    fn send_client_message(&self, sim: &BusId, connection: u64, envelope: Vec<u8>, kind: Backlog) {
        self.input(
            sim,
            connection,
            SimInput::ClientMessage {
                connection: self.connection(connection),
                envelope,
            },
            kind,
        );
    }

    /// Asks the director; `resume` is set for a resumed player.
    async fn ask_director(
        &self,
        connection: u64,
        account: Uuid,
        character: Uuid,
        resume: Option<ResumeOf>,
    ) -> Result<JoinReply, BusError> {
        let join = Join {
            connection: self.connection(connection),
            account,
            character,
            resume,
        };
        self.bus
            .request::<Join, JoinReply>(
                subject::director_join(self.settings.world_id),
                &join,
                self.settings.join_timeout,
            )
            .await
    }

    /// Server messages and closes go to the loop; route messages change the routes here. A
    /// stray copy past its grace is told that the connection left.
    async fn forward_outputs(self, mut batches: Subscription<OutputBatch>) {
        while let Some(batch) = batches.next().await {
            let batch = match batch {
                Ok(batch) => batch.message,
                Err(error) => {
                    tracing::warn!(%error, "bad world output batch");
                    continue;
                }
            };
            for output in batch.outputs {
                if !self.output(&batch.sim, output) {
                    return;
                }
            }
        }
    }

    /// One output of `sim`. Returns `false` when the worker of its connection is gone.
    fn output(&self, sim: &BusId, output: GatewayOutput) -> bool {
        let (connection, event) = match output {
            GatewayOutput::ServerMessages {
                connection,
                channel,
                reliable,
                envelopes,
            } => {
                let stray = {
                    let mut routes = self.routes();
                    let stray = routes.stray(connection, sim, Instant::now());
                    if stray != Stray::Evict && !reliable {
                        // Only bundles go unreliable: they carry the bundle sequence.
                        routes.bundle_sent(connection);
                    }
                    stray
                };
                if stray == Stray::Evict {
                    metrics::counter!(METRIC_STRAYS_EVICTED).increment(1);
                    tracing::warn!(connection, %sim, "a sim keeps a copy; it drops it");
                    self.send_left(connection, sim);
                    return true;
                }
                let event = WorldEvent::Messages {
                    sim: sim.clone(),
                    connection,
                    channel,
                    reliable,
                    envelopes,
                };
                (connection, event)
            }
            GatewayOutput::Close { connection } => (connection, WorldEvent::Close { connection }),
            GatewayOutput::Route {
                connection,
                sim,
                generation,
                sequence,
            } => {
                self.routes()
                    .apply_route(connection, sim, generation, sequence);
                return true;
            }
        };
        self.event(connection, event)
    }

    /// Resumes the connections whose sim left the cell table.
    async fn follow_tables(self, mut tables: Subscription<CellTable>) {
        while let Some(table) = tables.next().await {
            let table = match table {
                Ok(table) => table.message,
                Err(error) => {
                    tracing::warn!(%error, "bad cell table");
                    continue;
                }
            };
            let jobs = self.routes().lost(&table);
            for job in jobs {
                tokio::spawn(self.clone().resume(job));
            }
        }
    }

    /// Puts a player of a lost sim on the owner of its cell (M5 spec section 3.6), at most
    /// `max_resumes_in_flight` at one time; a request without an answer is sent once more, and a
    /// refusal closes the connection.
    async fn resume(self, job: Resume) {
        // The semaphore is never closed.
        let Ok(_permit) = Arc::clone(&self.resumes).acquire_owned().await else {
            return;
        };
        let resume = ResumeOf {
            entity: job.entity,
            handoffs: job.handoffs,
            bundles_sent: job.bundles_sent,
        };
        let mut reply = Err(BusError::NoReplySubject);
        for _ in 0..RESUME_ATTEMPTS {
            reply = self
                .ask_director(job.connection, job.account, job.character, Some(resume))
                .await;
            if reply.is_ok() {
                break;
            }
        }
        match reply {
            Ok(JoinReply::Joined {
                sim, generation, ..
            }) => {
                metrics::counter!(METRIC_RESUMES, RESULT_LABEL => RESUMED).increment(1);
                tracing::info!(connection = job.connection, %sim, "player resumed");
                // The held inputs go out under the routes lock, so no newer input of the
                // connection passes them.
                let mut routes = self.routes();
                match routes.resumed(&job, sim.clone(), generation) {
                    Some(held) => {
                        for envelope in held {
                            self.send_client_message(
                                &sim,
                                job.connection,
                                envelope,
                                Backlog::Reliable,
                            );
                        }
                    }
                    None => {
                        drop(routes);
                        self.send_left(job.connection, &sim);
                    }
                }
            }
            Ok(JoinReply::Refused { reason }) => self.close(job.connection, &reason).await,
            Err(error) => self.close(job.connection, &error.to_string()).await,
        }
    }

    async fn close(&self, connection: u64, reason: &str) {
        metrics::counter!(METRIC_RESUMES, RESULT_LABEL => CLOSED).increment(1);
        tracing::warn!(connection, reason, "resume failed; the connection closes");
        self.routes().left(connection);
        // The worker may have stopped; then nobody needs the close.
        self.event(connection, WorldEvent::Close { connection });
    }
}

impl World for BusWorld {
    async fn join(&self, connection: u64, record: &EntryRecord) -> Result<Admission, Refusal> {
        let reply = self
            .ask_director(connection, record.account_id, record.character_id, None)
            .await;
        match reply {
            Ok(JoinReply::Joined {
                sim,
                generation,
                entity,
                game_master,
            }) => {
                self.routes()
                    .joined(connection, record, sim, generation, entity);
                Ok(Admission { game_master })
            }
            Ok(JoinReply::Refused { reason }) => {
                tracing::warn!(%reason, "world refused the character");
                Err(Refusal::WorldRefused)
            }
            Err(error) => {
                tracing::warn!(%error, "world join failed");
                Err(Refusal::WorldUnavailable)
            }
        }
    }

    fn client_message(&self, connection: u64, envelope: Vec<u8>, reliable: bool) {
        let mut routes = self.routes();
        if let Some(sim) = routes.target(connection).cloned() {
            drop(routes);
            let kind = if reliable {
                Backlog::Reliable
            } else {
                Backlog::Droppable
            };
            self.send_client_message(&sim, connection, envelope, kind);
            return;
        }
        let hold = if reliable {
            routes.hold(connection, envelope)
        } else {
            Hold::Unknown
        };
        drop(routes);
        match hold {
            Hold::Held => {}
            Hold::Full => {
                metrics::counter!(METRIC_INPUT_CLOSES).increment(1);
                tracing::warn!(
                    connection,
                    "reliable inputs during a resume exceed the hold; the connection closes"
                );
                self.event(connection, WorldEvent::Close { connection });
            }
            Hold::Unknown => metrics::counter!(METRIC_INPUTS_WITHOUT_ROUTE).increment(1),
        }
    }

    fn left(&self, connection: u64) {
        let sim = self.routes().left(connection);
        if let Some(sim) = sim {
            self.send_left(connection, &sim);
        }
    }

    fn unknown(&self, connection: u64, sim: &BusId) {
        self.routes().left(connection);
        self.send_left(connection, sim);
    }
}

/// Queues this gateway's heartbeat every interval. Ends when every worker is gone.
async fn heartbeats(
    beats: Publisher<Heartbeat>,
    settings: Arc<WorldLinkSettings>,
    events: Arc<Vec<EventSender>>,
) {
    let subject = subject::gateway_heartbeat(settings.world_id, &settings.gateway_id);
    let own = Heartbeat {
        id: settings.gateway_id.clone(),
        started_at_ms: unix_ms(),
    };
    let mut timer = tokio::time::interval(settings.heartbeat_interval);
    loop {
        timer.tick().await;
        if events.iter().all(EventSender::is_closed) {
            return;
        }
        if !beats.send(subject.clone(), own.clone()) {
            tracing::warn!("heartbeat dropped; NATS is behind");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The sim of a [`TestWorld`].
    pub(crate) fn test_sim() -> BusId {
        BusId::try_from("sim-test".to_owned()).expect("id")
    }

    /// What a [`TestWorld`] was sent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Sent {
        ClientMessage(u64, Vec<u8>),
        Left(u64),
    }

    /// A world in memory: it accepts or refuses every join, sends `login` to each joined
    /// connection before it answers (as `sim` does), and records what the gateway sends.
    #[derive(Clone)]
    pub(crate) struct TestWorld {
        refuse: bool,
        login: Vec<Vec<u8>>,
        events: EventSender,
        pub(crate) joins: Arc<Mutex<Vec<u64>>>,
        pub(crate) sent: Arc<Mutex<Vec<Sent>>>,
    }

    impl TestWorld {
        /// A world and the events it sends to the gateway loop.
        pub(crate) fn new(refuse: bool, login: Vec<Vec<u8>>) -> (Self, EventReceiver) {
            let (events, receiver) = event_queue(EVENT_QUEUE);
            let world = Self {
                refuse,
                login,
                events,
                joins: Arc::default(),
                sent: Arc::default(),
            };
            (world, receiver)
        }

        /// Sends `event` to the gateway loop.
        pub(crate) fn emit(&self, event: WorldEvent) {
            assert_eq!(
                self.events.send(event),
                Delivery::Queued,
                "gateway loop runs"
            );
        }
    }

    impl World for TestWorld {
        async fn join(&self, connection: u64, _: &EntryRecord) -> Result<Admission, Refusal> {
            self.joins.lock().expect("lock").push(connection);
            if self.refuse {
                return Err(Refusal::WorldRefused);
            }
            self.emit(WorldEvent::Messages {
                sim: test_sim(),
                connection,
                channel: LOGIN_CHANNEL,
                reliable: true,
                envelopes: self.login.clone(),
            });
            Ok(Admission::default())
        }

        fn client_message(&self, connection: u64, envelope: Vec<u8>, _: bool) {
            self.sent
                .lock()
                .expect("lock")
                .push(Sent::ClientMessage(connection, envelope));
        }

        fn left(&self, connection: u64) {
            self.sent.lock().expect("lock").push(Sent::Left(connection));
        }

        fn unknown(&self, connection: u64, _: &BusId) {
            self.sent.lock().expect("lock").push(Sent::Left(connection));
        }
    }

    /// The carrier channel of the login messages.
    const LOGIN_CHANNEL: u8 = 0;

    fn left_of(connection: u64) -> SimInput {
        SimInput::Left {
            connection: ConnectionRef {
                gateway: test_sim(),
                connection,
            },
        }
    }

    #[test]
    fn a_full_backlog_keeps_departures_only_and_unsent_inputs_keep_their_order() {
        assert!(backlog_admits(Backlog::Reliable, 0, INPUT_BACKLOG_BYTES));
        assert!(!backlog_admits(Backlog::Reliable, INPUT_BACKLOG_BYTES, 1));
        assert!(!backlog_admits(Backlog::Droppable, INPUT_BACKLOG_BYTES, 1));
        assert!(backlog_admits(Backlog::Always, INPUT_BACKLOG_BYTES, 1));
        let mut pending = PendingInputs::new();
        requeue(&mut pending, test_sim(), vec![left_of(3)]);
        requeue(&mut pending, test_sim(), vec![left_of(1), left_of(2)]);
        let (inputs, bytes) = &pending[&test_sim()];
        assert_eq!(inputs, &vec![left_of(1), left_of(2), left_of(3)]);
        assert_eq!(
            *bytes,
            inputs.iter().map(SimInput::size_bytes).sum::<usize>()
        );
    }
}

#[cfg(all(test, feature = "infra"))]
mod bus_tests {
    use nw_bus::SimSlot;
    use nw_bus::test_support::test_bus;

    use super::*;
    use crate::workers::first_connection;

    const JOIN_TIMEOUT: Duration = Duration::from_millis(500);
    const RESUMES_IN_FLIGHT: usize = 64;
    const HEARTBEAT: Duration = Duration::from_millis(50);
    const WAIT: Duration = Duration::from_secs(5);
    const PAUSE: Duration = Duration::from_millis(50);
    const CONNECTION: u64 = 3;
    const UNKNOWN_CONNECTION: u64 = 77;
    const GENERATION: u64 = 1_800_000_000_000;
    const ENTITY: u64 = 12;
    const ENVELOPE: [u8; 2] = [4, 5];

    fn id(prefix: &str) -> BusId {
        BusId::try_from(format!("{prefix}-{}", Uuid::new_v4().simple())).expect("id")
    }

    fn settings() -> WorldLinkSettings {
        WorldLinkSettings {
            world_id: Uuid::new_v4(),
            gateway_id: id("gw"),
            join_timeout: JOIN_TIMEOUT,
            heartbeat_interval: HEARTBEAT,
            max_resumes_in_flight: RESUMES_IN_FLIGHT,
        }
    }

    fn record() -> EntryRecord {
        EntryRecord {
            ticket_id: "ticket".to_owned(),
            account_id: Uuid::new_v4(),
            world_id: Uuid::new_v4(),
            character_id: Uuid::new_v4(),
            expires_at: 0,
        }
    }

    fn joined(sim: &BusId) -> JoinReply {
        JoinReply::Joined {
            sim: sim.clone(),
            generation: GENERATION,
            entity: ENTITY,
            game_master: false,
        }
    }

    async fn next(receiver: &mut EventReceiver) -> WorldEvent {
        tokio::time::timeout(WAIT, receiver.recv())
            .await
            .expect("in time")
            .expect("open")
    }

    /// A director that answers new joins with `first` and resumes with `resume`.
    async fn director(bus: &Bus, world_id: Uuid, first: JoinReply, resume: JoinReply) {
        let mut joins = bus
            .subscribe::<Join>(subject::director_join(world_id))
            .await
            .expect("subscribe");
        bus.flush().await.expect("flush");
        let answerer = bus.clone();
        tokio::spawn(async move {
            while let Some(Ok(join)) = joins.next().await {
                let reply = if join.message.resume.is_some() {
                    &resume
                } else {
                    &first
                };
                answerer.reply(&join, reply).await.expect("reply");
            }
        });
    }

    /// One output of `sim` as the sim sends it.
    fn batch_of(sim: &BusId, output: GatewayOutput) -> OutputBatch {
        OutputBatch {
            sim: sim.clone(),
            outputs: vec![output],
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn events_reach_the_worker_of_the_connection() {
        const WORKERS: usize = 2;
        let bus = test_bus().await;
        let settings = settings();
        let (_world, mut events) = BusWorld::start(bus.clone(), settings.clone(), WORKERS)
            .await
            .expect("start");
        let ids = [first_connection(0) + 5, first_connection(1) + 5];
        let outputs: Vec<GatewayOutput> = ids
            .iter()
            .map(|connection| GatewayOutput::Close {
                connection: *connection,
            })
            .collect();
        let batch = OutputBatch {
            sim: id("sim"),
            outputs,
        };
        let subject = subject::gateway_output(settings.world_id, &settings.gateway_id);
        bus.publish(subject, &batch).await.expect("publish");
        for (worker, connection) in ids.into_iter().enumerate() {
            assert_eq!(
                next(&mut events[worker]).await,
                WorldEvent::Close { connection }
            );
            assert!(
                events[worker].try_recv().is_none(),
                "nothing of the other worker"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_behind_loses_bundles_only_and_never_stops_the_others() {
        const WORKERS: usize = 2;
        let bus = test_bus().await;
        let settings = settings();
        let (_world, mut events) = BusWorld::start(bus.clone(), settings.clone(), WORKERS)
            .await
            .expect("start");
        let behind = first_connection(0);
        let messages = |reliable| GatewayOutput::ServerMessages {
            connection: behind,
            channel: 0,
            reliable,
            envelopes: vec![vec![u8::from(reliable)]],
        };
        let mut outputs: Vec<GatewayOutput> = (0..=EVENT_QUEUE).map(|_| messages(false)).collect();
        outputs.push(messages(true));
        outputs.push(GatewayOutput::Close {
            connection: first_connection(1),
        });
        let subject = subject::gateway_output(settings.world_id, &settings.gateway_id);
        bus.publish(
            subject,
            &OutputBatch {
                sim: id("sim"),
                outputs,
            },
        )
        .await
        .expect("publish");
        assert_eq!(
            next(&mut events[1]).await,
            WorldEvent::Close {
                connection: first_connection(1)
            }
        );
        let reliable =
            |event: &WorldEvent| matches!(event, WorldEvent::Messages { reliable: true, .. });
        for _ in 0..EVENT_QUEUE {
            assert!(!reliable(&next(&mut events[0]).await));
        }
        assert!(
            reliable(&next(&mut events[0]).await),
            "the reliable message comes after the bundles that had a place"
        );
        assert!(events[0].try_recv().is_none(), "one bundle was lost");
    }

    /// A link with one worker, and that worker's events.
    async fn start_one(bus: &Bus, settings: WorldLinkSettings) -> (BusWorld, EventReceiver) {
        let (world, mut events) = BusWorld::start(bus.clone(), settings, 1)
            .await
            .expect("start");
        (world, events.remove(0))
    }

    /// The inputs that reach one sim, one at a time.
    struct Inputs {
        batches: Subscription<InputBatch>,
        waiting: std::collections::VecDeque<SimInput>,
    }

    async fn inputs_of(bus: &Bus, world_id: Uuid, sim: &BusId) -> Inputs {
        let batches = bus
            .subscribe::<InputBatch>(subject::sim_input(world_id, sim))
            .await
            .expect("subscribe");
        bus.flush().await.expect("flush");
        Inputs {
            batches,
            waiting: std::collections::VecDeque::new(),
        }
    }

    async fn next_input(inputs: &mut Inputs) -> SimInput {
        while inputs.waiting.is_empty() {
            let batch = tokio::time::timeout(WAIT, inputs.batches.next())
                .await
                .expect("in time")
                .expect("open")
                .expect("decode")
                .message;
            assert!(!batch.forwarded, "the gateway sends direct inputs");
            inputs.waiting.extend(batch.inputs);
        }
        inputs.waiting.pop_front().expect("an input waits")
    }

    /// Sends client messages until one reaches `inputs`; the route changes in another task.
    async fn message_reaches(world: &BusWorld, inputs: &mut Inputs) {
        reaches(world, inputs, CONNECTION).await;
    }

    /// Sends client messages of `connection` until one reaches `inputs`.
    async fn reaches(world: &BusWorld, inputs: &mut Inputs, connection: u64) {
        let reached = async {
            loop {
                world.client_message(connection, ENVELOPE.to_vec(), true);
                if let Ok(Some(Ok(_))) = tokio::time::timeout(PAUSE, inputs.batches.next()).await {
                    return;
                }
            }
        };
        tokio::time::timeout(WAIT, reached)
            .await
            .expect("routed in time");
    }

    fn table_of(sims: &[&BusId]) -> CellTable {
        CellTable {
            version: 1,
            cell_size_m: 2048.0,
            columns: 0,
            rows: 0,
            sims: sims
                .iter()
                .map(|sim| SimSlot {
                    id: (*sim).clone(),
                    generation: GENERATION,
                })
                .collect(),
            owners: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_join_goes_through_the_director_or_times_out() {
        let bus = test_bus().await;
        let settings = settings();
        let (world, _events) = start_one(&bus, settings.clone()).await;
        assert_eq!(
            world.join(CONNECTION, &record()).await,
            Err(Refusal::WorldUnavailable)
        );
        let sim = id("sim");
        let refused = JoinReply::Refused {
            reason: "no".to_owned(),
        };
        director(&bus, settings.world_id, joined(&sim), refused).await;
        assert_eq!(
            world.join(CONNECTION, &record()).await,
            Ok(Admission { game_master: false })
        );

        let refusing = self::settings();
        let (world, _events) = start_one(&bus, refusing.clone()).await;
        let reason = "no".to_owned();
        director(
            &bus,
            refusing.world_id,
            JoinReply::Refused {
                reason: reason.clone(),
            },
            JoinReply::Refused { reason },
        )
        .await;
        assert_eq!(
            world.join(CONNECTION, &record()).await,
            Err(Refusal::WorldRefused)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inputs_follow_the_route_in_order_and_outputs_come_back() {
        let bus = test_bus().await;
        let settings = settings();
        let sim = id("sim");
        director(&bus, settings.world_id, joined(&sim), joined(&sim)).await;
        let mut inputs = inputs_of(&bus, settings.world_id, &sim).await;
        let (world, mut events) = start_one(&bus, settings.clone()).await;
        world.client_message(CONNECTION, ENVELOPE.to_vec(), true);
        world.join(CONNECTION, &record()).await.expect("join");
        world.client_message(CONNECTION, ENVELOPE.to_vec(), true);
        world.left(CONNECTION);
        world.unknown(UNKNOWN_CONNECTION, &sim);
        let connection = |number| ConnectionRef {
            gateway: settings.gateway_id.clone(),
            connection: number,
        };
        assert_eq!(
            next_input(&mut inputs).await,
            SimInput::ClientMessage {
                connection: connection(CONNECTION),
                envelope: ENVELOPE.to_vec(),
            }
        );
        assert_eq!(
            next_input(&mut inputs).await,
            SimInput::Left {
                connection: connection(CONNECTION)
            }
        );
        assert_eq!(
            next_input(&mut inputs).await,
            SimInput::Left {
                connection: connection(UNKNOWN_CONNECTION)
            }
        );
        let output = subject::gateway_output(settings.world_id, &settings.gateway_id);
        let close = GatewayOutput::Close {
            connection: CONNECTION,
        };
        bus.publish(output, &batch_of(&sim, close))
            .await
            .expect("publish");
        assert_eq!(
            next(&mut events).await,
            WorldEvent::Close {
                connection: CONNECTION
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_route_moves_the_inputs() {
        let bus = test_bus().await;
        let settings = settings();
        let (first, second) = (id("sim-a"), id("sim-b"));
        director(&bus, settings.world_id, joined(&first), joined(&first)).await;
        let mut inputs = inputs_of(&bus, settings.world_id, &second).await;
        let (world, _events) = start_one(&bus, settings.clone()).await;
        world.join(CONNECTION, &record()).await.expect("join");
        let route = GatewayOutput::Route {
            connection: CONNECTION,
            sim: second.clone(),
            generation: GENERATION,
            sequence: 1,
        };
        let output = subject::gateway_output(settings.world_id, &settings.gateway_id);
        bus.publish(output, &batch_of(&first, route))
            .await
            .expect("publish");
        message_reaches(&world, &mut inputs).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_resume_moves_the_inputs_and_a_refused_one_closes() {
        let bus = test_bus().await;
        let settings = settings();
        let (lost, other) = (id("sim-a"), id("sim-b"));
        director(&bus, settings.world_id, joined(&lost), joined(&other)).await;
        let mut inputs = inputs_of(&bus, settings.world_id, &other).await;
        let (world, _events) = start_one(&bus, settings.clone()).await;
        world.join(CONNECTION, &record()).await.expect("join");
        let tables = subject::director_table(settings.world_id);
        bus.publish(tables, &table_of(&[&other]))
            .await
            .expect("table");
        message_reaches(&world, &mut inputs).await;

        let refusing = self::settings();
        let refused = JoinReply::Refused {
            reason: "no cell".to_owned(),
        };
        director(&bus, refusing.world_id, joined(&lost), refused).await;
        let (world, mut events) = start_one(&bus, refusing.clone()).await;
        world.join(CONNECTION, &record()).await.expect("join");
        let tables = subject::director_table(refusing.world_id);
        bus.publish(tables, &table_of(&[])).await.expect("table");
        assert_eq!(
            next(&mut events).await,
            WorldEvent::Close {
                connection: CONNECTION
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resumes_are_paced_and_retried_once() {
        const CONNECTIONS: [u64; 3] = [1, 2, 3];
        const ANSWER_DELAY: Duration = Duration::from_millis(100);
        let bus = test_bus().await;
        let settings = WorldLinkSettings {
            max_resumes_in_flight: 1,
            ..settings()
        };
        let (lost, other) = (id("sim-a"), id("sim-b"));
        let mut joins = bus
            .subscribe::<Join>(subject::director_join(settings.world_id))
            .await
            .expect("subscribe");
        bus.flush().await.expect("flush");
        let attempts: Arc<Mutex<BTreeMap<u64, usize>>> = Arc::default();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let most = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let answerer = bus.clone();
        let (seen, now, peak) = (
            Arc::clone(&attempts),
            Arc::clone(&active),
            Arc::clone(&most),
        );
        let (first, resumed) = (joined(&lost), joined(&other));
        tokio::spawn(async move {
            while let Some(Ok(join)) = joins.next().await {
                if join.message.resume.is_none() {
                    answerer.reply(&join, &first).await.expect("reply");
                    continue;
                }
                let connection = join.message.connection.connection;
                let attempt = {
                    let mut seen = seen.lock().expect("lock");
                    let count = seen.entry(connection).or_default();
                    *count += 1;
                    *count
                };
                if connection == CONNECTIONS[0] && attempt == 1 {
                    continue;
                }
                let running = now.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                peak.fetch_max(running, std::sync::atomic::Ordering::SeqCst);
                let (answerer, resumed, now) =
                    (answerer.clone(), resumed.clone(), Arc::clone(&now));
                tokio::spawn(async move {
                    tokio::time::sleep(ANSWER_DELAY).await;
                    now.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    answerer.reply(&join, &resumed).await.expect("reply");
                });
            }
        });
        let mut inputs = inputs_of(&bus, settings.world_id, &other).await;
        let (world, _events) = start_one(&bus, settings.clone()).await;
        for connection in CONNECTIONS {
            world.join(connection, &record()).await.expect("join");
        }
        let tables = subject::director_table(settings.world_id);
        bus.publish(tables, &table_of(&[&other]))
            .await
            .expect("table");
        for connection in CONNECTIONS {
            reaches(&world, &mut inputs, connection).await;
        }
        assert_eq!(
            attempts.lock().expect("lock")[&CONNECTIONS[0]],
            2,
            "one retry"
        );
        assert_eq!(
            most.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one at a time"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gateway_beats() {
        let bus = test_bus().await;
        let settings = settings();
        let mut beats = bus
            .subscribe::<Heartbeat>(subject::gateway_heartbeat(
                settings.world_id,
                &settings.gateway_id,
            ))
            .await
            .expect("subscribe");
        bus.flush().await.expect("flush");
        let (_world, _events) = start_one(&bus, settings.clone()).await;
        let beat = tokio::time::timeout(WAIT, beats.next())
            .await
            .expect("in time");
        assert_eq!(
            beat.expect("open").expect("decode").message.id,
            settings.gateway_id
        );
    }
}
