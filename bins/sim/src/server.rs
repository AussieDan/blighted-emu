// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loop of `sim` (M4 spec sections 6, 7.1, 7.5, 11; M5 spec sections 3, 7; M5 load spec
//! sections 4, 5, 8): admits, inputs, the cell table, handoffs, ghosts, the 30 Hz tick, position
//! saves, leaderboard stats, and heartbeats. One task owns the world. Database work, handoff requests and replies
//! run in their own tasks, and bus messages go out through queues, so neither a slow database, a
//! slow sim nor a lost NATS connection stops the tick.
//!
//! Gateway outputs and forwarded inputs wait in batches and go out once per tick, one batch per
//! receiver (split at [`nw_bus::MAX_BATCH_BYTES`]).

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::world::transmog::SkinJob;
use nw_bus::InstanceCommand;
use nw_bus::ItemState;
use nw_bus::{
    AdminAction, AdminCommand, AdminReply, Admit, AdmitReply, Bus, BusError, BusId, CarriedItem,
    CellTable, ConnectionRef, GatewayOutput, Ghost, Ghosts, HandoffMessage, HandoffReply,
    Heartbeat, Hit, Incoming, InputBatch, ItemPlace, OutputBatch, PlayerInventory,
    PlayerProgression, PlayerQuests, Point, Publisher, ScriptLog, SimHeartbeat, SimInput, SimSlot,
    SimSnapshot, snapshot_parts, split_batches, subject,
};
use nw_bus::{CoinTransfer, LeaveReason, SocialBatch, SocialDeliveries, SocialInput};
use nw_bus::{GhostAssembly, ghost_parts};
use nw_common::time::{unix_ms, unix_seconds};
use nw_content::leaderboards::RotationCalendar;
use nw_db::{AccountSkins, collect_account_skins};
use nw_db::{
    PgPool, SaveOutcome, SavedCamp, SavedHome, SavedHomeRecord, SavedHousingItem, SavedInventory,
    SavedItem, SavedItemRolls, SavedLoadout, SavedLootLimit, SavedMount, SavedMounts, SavedPlace,
    SavedPosition, SavedProgression, SavedQuests, SavedStoredItem, SavedTravel, StatUpdate,
    save_character_items, save_character_position, save_character_progression,
    save_character_quests, submit_stats,
};
use nw_db::{clear_new_account_titles, company_listings, earn_account_titles, stored_players};
use nw_messages::ArenaClientMsg;
use nw_messages::EconomyCall;
use nw_messages::TransmogClientMsg;
use nw_messages::facet::TIME_SYNC_REQUEST_TYPE_INDEX;
use nw_messages::guild_lookup::REQUEST_GET_GUILDS_TYPE_INDEX;
use nw_messages::social::SocialCall;
use nw_messages::{
    BodyCodec, ClientFacetMsg, ClientInputMsg, ConversationClientMsg, GmClientMsg, InputRecord,
    LookupRequest, MetaAchievementRequest, MountClientMsg, NS_PER_MS, ObjectiveInteractorClientMsg,
    PointsClientMsg, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX, REQUEST_RESPAWN_TYPE_INDEX,
    RequestRespawnMsg, SPREADSHOT_REQUEST_TYPE_INDEX, SetTitleRequest, SpreadshotRequestMsg,
    TravelClientMsg,
};
use nw_messages::{
    ClientOptionMsg, GuildsRequest, ItemCallMsg, ItemRepairMsg, KickClientMsg, PlayerDataRequest,
    PlayerManagerRejectedMsg, RETAIL_KICK_REASON, ServerMessage, TitleRequest,
};
use nw_messages::{FishingClientMsg, MusicClientMsg};
use nw_messages::{GameModeClientMsg, TitlesUnlockMsg, WarClientMsg, WarSignupClientMsg};
use nw_reflect::Envelope;
use nw_sim_core::{Ground, GroundCheck, TickClock};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::combat::{AttackOutcome, AttackRefusal, CombatSettings, RemoteHit};
use crate::forward::{Forward, ForwardTable};
use crate::gateways::GatewayTracker;
use crate::instance_link::InstanceLink;
use crate::login::WorldIds;
use crate::mob_ai::SpawnAi;
use crate::mobs::{MobChanges, spawn_index};
use crate::player_content::PlayerContent;
use crate::saves::{ItemSave, ProgressionSave, QuestSave, SaveQueue};
use crate::social::{
    guilds_answers, player_data_answer, requested_characters, stored_view, titles_answer,
};
use crate::stats::stat_updates;
use crate::world::economy::{EconomyResult, run_job};
use crate::world::send_now;
use crate::world::{
    Departure, GmChat, HandoffEnd, HandoffStart, MoveError, Outgoing, SimWorld, WorldSettings,
    answer_lines,
};
use nw_world::script::LevelDesign;

/// Time between checks of the tick clock.
pub const POLL: Duration = Duration::from_millis(5);
/// Unix milliseconds of 2000-01-01, the epoch of the server clock in time messages.
pub const SERVER_EPOCH_UNIX_MS: u64 = 946_684_800_000;
/// Handoff answers that may wait for the loop.
const RESULT_QUEUE: usize = 1024;
/// Output batches that may wait for NATS (one per gateway per tick); more are dropped and
/// counted, so a NATS outage never sends minutes of old bundles.
pub const OUTPUT_QUEUE: usize = 256;
/// Ghost lists, forwarded input batches and cancels that may wait for NATS.
const SIM_QUEUE: usize = 256;
/// Heartbeats that may wait for NATS.
const HEARTBEAT_QUEUE: usize = 16;
/// Longest wait at shutdown for the last saves.
pub const SHUTDOWN_SAVE_WAIT: Duration = Duration::from_secs(10);
/// One tick at 30 Hz, rounded down to whole milliseconds: the M5 limit of a handoff and a tick.
pub const ONE_TICK_SECONDS: f64 = 0.033;
/// Buckets of the handoff and tick time histograms, in seconds (M5 spec section 7.6).
pub const LATENCY_BUCKETS_SECONDS: [f64; 9] = [
    0.001,
    0.002,
    0.005,
    0.010,
    0.020,
    ONE_TICK_SECONDS,
    0.050,
    0.100,
    0.250,
];
/// Metric: players in the world.
pub const METRIC_PLAYERS: &str = "sim_players";
/// Metric: ghosts of other sims.
pub const METRIC_GHOSTS: &str = "sim_ghosts";
/// Metric: ticks that were due but not run on time.
pub const METRIC_LATE_TICKS: &str = "sim_late_ticks_total";
/// Metric: time of one world tick.
pub const METRIC_TICK_SECONDS: &str = "sim_tick_seconds";
/// Metric: admits by `result`.
pub const METRIC_ADMITS: &str = "sim_admits_total";
/// Metric: handoffs by `result`.
pub const METRIC_HANDOFFS: &str = "sim_handoffs_total";
/// Metric: time from the freeze to the answer of a handoff.
pub const METRIC_HANDOFF_SECONDS: &str = "sim_handoff_seconds";
/// Metric: inputs sent on to the sim that took their player.
pub const METRIC_FORWARDED_INPUTS: &str = "sim_forwarded_inputs_total";
/// Metric: forwarded client messages dropped because a direct one came first.
pub const METRIC_STALE_FORWARDS: &str = "sim_stale_forwards_total";
/// Metric: failed saves.
pub const METRIC_FAILED_SAVES: &str = "sim_failed_saves_total";
/// Metric: database look-ups of client calls (titles, other players) that failed.
pub const METRIC_FAILED_LOOKUPS: &str = "sim_failed_lookups_total";
/// Prometheus counter of leaderboard stat values added.
pub const METRIC_STAT_UPDATES: &str = "sim_stat_updates_total";
/// Prometheus counter of leaderboard stat batches the database refused.
pub const METRIC_FAILED_STATS: &str = "sim_failed_stat_batches_total";
/// Metric: client messages of a type that `sim` does not read yet.
pub const METRIC_IGNORED_MESSAGES: &str = "sim_ignored_client_messages_total";
/// Metric: client messages that do not decode.
pub const METRIC_BAD_MESSAGES: &str = "sim_bad_client_messages_total";
/// Metric: refused moves by `reason`.
pub const METRIC_REFUSED_MOVES: &str = "sim_moves_refused_total";
/// Metric: moves of a player frozen for its handoff.
pub const METRIC_FROZEN_MOVES: &str = "sim_moves_while_frozen_total";
/// Metric: accepted moves where the level has no height data.
pub const METRIC_MOVES_WITHOUT_GROUND: &str = "sim_moves_without_ground_total";
/// Metric: bus messages dropped because NATS is behind.
pub const METRIC_DROPPED_OUTPUTS: &str = "sim_dropped_outputs_total";
/// Metric: mobs by `state` (M6-A spec section 10).
pub const METRIC_MOBS: &str = "sim_mobs";
/// Metric: attacks of players and mobs by `result`: an outcome or a refusal reason.
pub const METRIC_ATTACKS: &str = "sim_attacks_total";
/// Metric: hits from other sims by `result`.
pub const METRIC_REMOTE_HITS: &str = "sim_remote_hits_total";
/// Metric: respawn requests by `result`.
pub const METRIC_RESPAWNS: &str = "sim_respawns_total";
/// A respawn request that respawned.
const RESPAWNED: &str = "respawned";
/// A remote hit that landed.
const APPLIED: &str = "applied";
/// A remote hit whose target is gone, dead or returning.
const DROPPED: &str = "dropped";
/// Metric: deaths by `kind`.
pub const METRIC_DEATHS: &str = "sim_deaths_total";
/// Label of a death's kind.
pub const KIND_LABEL: &str = "kind";
/// A player's death.
const PLAYER_KIND: &str = "player";
/// A mob's death.
pub const MOB_KIND: &str = "mob";
/// Reason of a move of a dead player.
pub const DEAD_REASON: &str = "dead";
/// Metric: owned spawns without a mob type or level row.
pub const METRIC_MOBS_UNSPAWNED: &str = "sim_mobs_unspawned_total";
/// Metric: mob brain steps.
pub const METRIC_MOB_BRAIN_RUNS: &str = "sim_mob_brain_runs_total";
/// Metric: player scans for mobs to wake.
pub const METRIC_MOB_WAKE_SCANS: &str = "sim_mob_wake_scans_total";
/// Metric: time to build and queue one admin snapshot.
pub const METRIC_SNAPSHOT_SECONDS: &str = "sim_snapshot_seconds";
/// Metric: members put in bundles: each player's own member and every other entity.
pub const METRIC_BUNDLE_MEMBERS: &str = "sim_bundle_members_total";
/// Metric: time of each part of a tick, by `part`.
pub const METRIC_TICK_PART_SECONDS: &str = "sim_tick_part_seconds";
/// Label of a tick part.
const PART_LABEL: &str = "part";
/// Label of a mob's state.
const STATE_LABEL: &str = "state";
/// Label of a refused move's reason.
pub const REASON_LABEL: &str = "reason";
/// Label of the handoff result.
pub const RESULT_LABEL: &str = "result";
const ADMITTED: &str = "admitted";
const REFUSED: &str = "refused";
/// Handoff result: the other sim took the player.
pub const HANDOFF_MOVED: &str = "moved";
/// Handoff result: the other sim refused the player.
pub const HANDOFF_REFUSED: &str = "refused";
/// Handoff result: the other sim did not answer in time.
pub const HANDOFF_TIMEOUT: &str = "timeout";
const HANDOFF_GONE: &str = "gone";
/// The type index of a player data request, for the logs of its look-up.
const PLAYER_DATA_CALL: u64 = nw_messages::player_data::REQUEST_GET_PLAYERS_TYPE_INDEX;
/// Yaw of a saved position; the sim has no facing yet.
const NO_YAW: f32 = 0.0;

/// What the loop needs from the configuration.
#[derive(Debug, Clone)]
pub struct LoopSettings {
    /// This sim.
    pub sim_id: BusId,
    /// The world.
    pub world_id: Uuid,
    /// The open map level.
    pub level: String,
    /// The world state settings.
    pub world: WorldSettings,
    /// Longest wait for the answer to a handoff.
    pub handoff_timeout: Duration,
    /// How long inputs of a handed-off connection go on to the new sim, in milliseconds.
    pub forward_ttl_ms: u64,
    /// Time between position saves.
    pub save_interval: Duration,
    /// Time between heartbeats.
    pub heartbeat_interval: Duration,
    /// Missed gateway heartbeats before its players leave.
    pub heartbeat_misses: u32,
    /// Mobs and combat.
    pub combat: CombatSettings,
    /// The starter kits and achievement bits.
    pub content: Arc<PlayerContent>,
    /// Time between two admin snapshots on the bus; `None` is off.
    pub snapshot_interval: Option<Duration>,
    /// The design AI of the level's spawns, in spawn order.
    pub mob_ai: Arc<[Option<SpawnAi>]>,
    /// The scripts and events of the level.
    pub design: Arc<LevelDesign>,
    /// The leaderboard rotations, as the config layers give them to the client.
    pub leaderboard_calendar: RotationCalendar,
}

/// The databases of the loop.
#[derive(Debug, Clone)]
pub struct Databases {
    /// The world database: positions, items and quests.
    pub world: PgPool,
    /// The global database: leaderboard stats.
    pub global: PgPool,
}

/// The loop's failure.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The bus failed.
    #[error(transparent)]
    Bus(#[from] BusError),
    /// No random bytes for the world ids.
    #[error("no random bytes: {0}")]
    Random(#[from] getrandom::Error),
}

/// What one save task stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveKind {
    Position,
    Items,
    Quests,
    Progression,
}

type SaveResult = (SaveKind, Uuid, bool);

/// The end of one look-up task of a client call (titles, other players): the call's type and
/// the answer to send, if it has one.
struct LookupAnswer {
    connection: ConnectionRef,
    call: u64,
    answer: Result<Vec<ServerMessage>, nw_db::DbError>,
}

/// The answer to one handoff offer.
struct HandoffResult {
    connection: ConnectionRef,
    target: SimSlot,
    sequence: u64,
    reply: Result<HandoffReply, BusError>,
    elapsed: Duration,
}

/// The subscriptions of one sim.
struct Inbox {
    admits: nw_bus::Subscription<Admit>,
    inputs: nw_bus::Subscription<InputBatch>,
    handoffs: nw_bus::Subscription<HandoffMessage>,
    ghosts: nw_bus::Subscription<Ghosts>,
    hits: nw_bus::Subscription<Hit>,
    tables: nw_bus::Subscription<CellTable>,
    beats: nw_bus::Subscription<Heartbeat>,
    admin: nw_bus::Subscription<AdminCommand>,
    /// The social service's messages for this sim's players.
    social: nw_bus::Subscription<SocialDeliveries>,
    /// The social service's coin moves (company treasury).
    coins: nw_bus::Subscription<CoinTransfer>,
    /// The director's instance commands (open, close, add a member).
    instance: nw_bus::Subscription<InstanceCommand>,
}

impl Inbox {
    async fn subscribe(bus: &Bus, world_id: Uuid, sim: &BusId) -> Result<Self, BusError> {
        let inbox = Self {
            admits: bus.subscribe(subject::sim_admit(world_id, sim)).await?,
            inputs: bus.subscribe(subject::sim_input(world_id, sim)).await?,
            handoffs: bus.subscribe(subject::sim_handoff(world_id, sim)).await?,
            ghosts: bus.subscribe(subject::sim_ghosts(world_id, sim)).await?,
            hits: bus.subscribe(subject::sim_hit(world_id, sim)).await?,
            tables: bus.subscribe(subject::director_table(world_id)).await?,
            beats: bus
                .subscribe(subject::all_gateway_heartbeats(world_id))
                .await?,
            admin: bus.subscribe(subject::sim_admin(world_id, sim)).await?,
            social: bus.subscribe(subject::sim_social(world_id, sim)).await?,
            coins: bus.subscribe(subject::sim_coins(world_id, sim)).await?,
            instance: bus.subscribe(subject::sim_instance(world_id, sim)).await?,
        };
        bus.flush().await?;
        Ok(inbox)
    }
}

struct Loop {
    settings: LoopSettings,
    bus: Bus,
    outputs: Publisher<OutputBatch>,
    sims: Publisher<InputBatch>,
    /// Gateway outputs of this tick, per gateway, in order.
    pending_outputs: BTreeMap<BusId, Vec<GatewayOutput>>,
    /// Inputs to send on, per sim, in order.
    pending_forwards: BTreeMap<BusId, Vec<SimInput>>,
    /// Connections with a direct input since they came here.
    direct: HashSet<ConnectionRef>,
    ghosts: Publisher<Ghosts>,
    /// The number of the last round of ghosts this sim sent.
    ghost_round: u64,
    /// The parts of other sims' ghost rounds that came so far.
    ghost_assembly: GhostAssembly,
    hits: Publisher<Hit>,
    cancels: Publisher<HandoffMessage>,
    beats: Publisher<SimHeartbeat>,
    snapshots: Publisher<SimSnapshot>,
    script_logs: Publisher<ScriptLog>,
    /// Social calls and presence to the world's social service.
    social: Publisher<SocialBatch>,
    /// The social inputs of this tick, in order.
    pending_social: Vec<SocialInput>,
    db: PgPool,
    global_db: PgPool,
    world: SimWorld,
    gateways: GatewayTracker,
    forwards: ForwardTable,
    queue: SaveQueue<SavedPosition>,
    item_queue: SaveQueue<ItemSave>,
    quest_queue: SaveQueue<QuestSave>,
    progression_queue: SaveQueue<ProgressionSave>,
    saves: JoinSet<SaveResult>,
    /// The database look-ups of client calls.
    lookups: JoinSet<LookupAnswer>,
    /// The trading post jobs that run (economy module).
    economy_jobs: JoinSet<Option<EconomyResult>>,
    /// The skin collection jobs that run (transmog module).
    skin_jobs: JoinSet<(SkinJob, Result<AccountSkins, nw_db::DbError>)>,
    /// The expedition requests that wait for the director.
    instance_link: InstanceLink,
    handoff_results: mpsc::Sender<HandoffResult>,
    started: Instant,
    started_at_ms: u64,
}

/// Serves admits, inputs, handoffs, ghosts, the cell table, ticks, saves and heartbeats on the
/// ground of `ground` until `shutdown` finishes; then saves every player once more and waits at
/// most [`SHUTDOWN_SAVE_WAIT`] for the saves.
///
/// # Errors
///
/// Returns [`ServeError`] when a subscription cannot start.
pub async fn serve(
    settings: LoopSettings,
    ground: Arc<dyn Ground + Send + Sync>,
    bus: Bus,
    databases: Databases,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let mut inbox = Inbox::subscribe(&bus, settings.world_id, &settings.sim_id).await?;
    let started_at_ms = unix_ms();
    let me = SimSlot {
        id: settings.sim_id.clone(),
        generation: started_at_ms,
    };
    let mob_ai = Arc::clone(&settings.mob_ai);
    let design = Arc::clone(&settings.design);
    let (handoff_results, mut answers) = mpsc::channel(RESULT_QUEUE);
    let mut state = Loop {
        world: SimWorld::new(
            settings.world,
            WorldIds::random(started_at_ms)?,
            ground,
            me,
            settings.combat.clone(),
            Arc::clone(&settings.content),
        ),
        gateways: GatewayTracker::new(settings.heartbeat_interval, settings.heartbeat_misses),
        forwards: ForwardTable::new(settings.forward_ttl_ms),
        outputs: Publisher::start(bus.clone(), OUTPUT_QUEUE),
        sims: Publisher::start(bus.clone(), SIM_QUEUE),
        pending_outputs: BTreeMap::new(),
        pending_forwards: BTreeMap::new(),
        direct: HashSet::new(),
        ghosts: Publisher::start(bus.clone(), SIM_QUEUE),
        ghost_round: 0,
        ghost_assembly: GhostAssembly::default(),
        hits: Publisher::start(bus.clone(), SIM_QUEUE),
        cancels: Publisher::start(bus.clone(), SIM_QUEUE),
        beats: Publisher::start(bus.clone(), HEARTBEAT_QUEUE),
        snapshots: Publisher::start(bus.clone(), SIM_QUEUE),
        script_logs: Publisher::start(bus.clone(), SIM_QUEUE),
        social: Publisher::start(bus.clone(), SIM_QUEUE),
        pending_social: Vec::new(),
        settings,
        bus,
        db: databases.world,
        global_db: databases.global,
        queue: SaveQueue::default(),
        item_queue: SaveQueue::default(),
        quest_queue: SaveQueue::default(),
        progression_queue: SaveQueue::default(),
        saves: JoinSet::new(),
        lookups: JoinSet::new(),
        economy_jobs: JoinSet::new(),
        skin_jobs: JoinSet::new(),
        instance_link: InstanceLink::default(),
        handoff_results,
        started: Instant::now(),
        started_at_ms,
    };
    state.world.set_mob_ai(mob_ai);
    state.world.set_design(&design);
    let mut clock = TickClock::new(0);
    let mut poll = tokio::time::interval(POLL);
    let mut save_timer = tokio::time::interval(state.settings.save_interval);
    let mut beat_timer = tokio::time::interval(state.settings.heartbeat_interval);
    let mut snapshot_timer = state.settings.snapshot_interval.map(tokio::time::interval);
    let mut delivery_timer = tokio::time::interval(Duration::from_millis(
        state.settings.content.trading_post.delivery_poll_ms,
    ));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            Some(admit) = inbox.admits.next() => match admit {
                Ok(admit) => state.admit(admit),
                Err(error) => tracing::warn!(%error, "bad admit message"),
            },
            Some(command) = inbox.admin.next() => match command {
                Ok(command) => state.admin(command),
                Err(error) => tracing::warn!(%error, "bad admin command"),
            },
            Some(batch) = inbox.inputs.next() => match batch {
                Ok(batch) => state.inputs(batch.message),
                Err(error) => tracing::warn!(%error, "bad input batch"),
            },
            Some(handoff) = inbox.handoffs.next() => match handoff {
                Ok(handoff) => state.handoff(handoff),
                Err(error) => tracing::warn!(%error, "bad handoff message"),
            },
            Some(ghosts) = inbox.ghosts.next() => match ghosts {
                Ok(ghosts) => state.ghosts_in(ghosts.message),
                Err(error) => tracing::warn!(%error, "bad ghost message"),
            },
            Some(hit) = inbox.hits.next() => match hit {
                Ok(hit) => state.hit_in(&hit.message),
                Err(error) => tracing::warn!(%error, "bad hit message"),
            },
            Some(table) = inbox.tables.next() => match table {
                Ok(table) => state.table(&table.message),
                Err(error) => tracing::warn!(%error, "bad cell table"),
            },
            Some(beat) = inbox.beats.next() => match beat {
                Ok(beat) => state.gateway_beat(&beat.message),
                Err(error) => tracing::warn!(%error, "bad heartbeat"),
            },
            Some(deliveries) = inbox.social.next() => match deliveries {
                Ok(deliveries) => state.social_deliveries(&deliveries.message),
                Err(error) => tracing::warn!(%error, "bad social deliveries"),
            },
            Some(transfer) = inbox.coins.next() => match transfer {
                Ok(transfer) => state.move_coins(transfer),
                Err(error) => tracing::warn!(%error, "bad coin move"),
            },
            Some(answer) = answers.recv() => state.handoff_done(answer),
            Some(saved) = state.saves.join_next() => state.saved(saved),
            Some(answer) = state.lookups.join_next() => state.looked_up(answer),
            Some(done) = state.economy_jobs.join_next() => state.economy_done(done),
            Some(done) = state.skin_jobs.join_next() => state.skins_done(done),
            _ = delivery_timer.tick() => state.world.economy_round(),
            Some(command) = inbox.instance.next() => match command {
                Ok(command) => {
                    let now_ms = state.now_ms();
                    crate::instance_link::command(&mut state.world, &state.bus, command, now_ms);
                }
                Err(error) => tracing::warn!(%error, "bad instance command"),
            },
            Some((connection, reply)) = state.instance_link.next() => {
                state.world.instance_reply(&connection, reply);
            }
            _ = poll.tick() => state.tick(&mut clock),
            _ = save_timer.tick() => state.save_all(),
            _ = beat_timer.tick() => state.heartbeat(),
            () = next_snapshot(snapshot_timer.as_mut()) => state.snapshot(),
        }
    }
    state.finish().await;
    Ok(())
}

impl Loop {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Adds the player of an admit. A position, an item list, quests or a progression of this
    /// sim that is still waiting to be saved is newer than the database, so it wins.
    fn admit(&mut self, admit: Incoming<Admit>) {
        let mut message = admit.message.clone();
        if let Some(unsaved) = self.queue.latest(message.character) {
            let [x, y, z] = unsaved.metres();
            message.position = Point { x, y, z };
        }
        if let Some(unsaved) = self.item_queue.latest(message.character) {
            message.items = Some(unsaved.items.clone());
        }
        if let Some(unsaved) = self.quest_queue.latest(message.character) {
            message.quests = unsaved.quests.clone();
        }
        if let Some(unsaved) = self.progression_queue.latest(message.character) {
            message.progression = unsaved.progression.clone();
        }
        let reply = match self.world.admit(&message, self.now_ms(), server_time()) {
            Ok(login) => {
                metrics::counter!(METRIC_ADMITS, RESULT_LABEL => ADMITTED).increment(1);
                tracing::info!(
                    character = %message.character,
                    resumed = !message.login,
                    game_master = message.game_master,
                    "joined"
                );
                self.gateways
                    .joined(&message.connection.gateway, Instant::now());
                self.forwards.remove(&message.connection);
                if let Some(login) = login {
                    self.send(login);
                }
                if let Some(player) = self
                    .world
                    .social_player(&message.connection, &self.settings.sim_id)
                {
                    self.pending_social.push(SocialInput::Joined(player));
                }
                AdmitReply::Admitted
            }
            Err(error) => {
                metrics::counter!(METRIC_ADMITS, RESULT_LABEL => REFUSED).increment(1);
                tracing::warn!(character = %message.character, %error, "admit refused");
                AdmitReply::Refused {
                    reason: error.to_string(),
                }
            }
        };
        self.reply(admit, reply);
        self.count_players();
    }

    /// Answers a request from its own task, so a slow NATS link never stops the tick.
    /// A GM command from the admin tool, between ticks.
    fn admin(&mut self, command: Incoming<AdminCommand>) {
        let reply = match &command.message.action {
            AdminAction::Kick { entity } => self.kick(*entity),
            _ => self.world.admin(&command.message, self.now_ms()),
        };
        tracing::info!(request = %command.message.request, ?reply, "admin command");
        self.reply(command, reply);
    }

    fn reply<T: Send + Sync + 'static, R: serde::Serialize + Send + Sync + 'static>(
        &self,
        request: Incoming<T>,
        answer: R,
    ) {
        let bus = self.bus.clone();
        tokio::spawn(async move {
            if let Err(error) = bus.reply(&request, &answer).await {
                tracing::warn!(%error, "reply failed");
            }
        });
    }

    /// The inputs of one batch, in order. A forwarded client message of a connection that has
    /// had a direct input is older than that input, so it is dropped.
    fn inputs(&mut self, batch: InputBatch) {
        for input in batch.inputs {
            if let SimInput::ClientMessage { connection, .. } = &input {
                if !batch.forwarded {
                    self.direct.insert(connection.clone());
                } else if self.direct.contains(connection) {
                    metrics::counter!(METRIC_STALE_FORWARDS).increment(1);
                    continue;
                }
            }
            self.input(input);
        }
    }

    /// A client message or a departure. Those of a handed-off connection go on to its new sim.
    fn input(&mut self, input: SimInput) {
        let connection = match &input {
            SimInput::ClientMessage { connection, .. } | SimInput::Left { connection } => {
                connection.clone()
            }
        };
        if let Some(forward) = self.forwards.target(&connection, self.now_ms()).cloned() {
            if matches!(input, SimInput::Left { .. }) {
                self.forwards.remove(&connection);
            }
            metrics::counter!(METRIC_FORWARDED_INPUTS).increment(1);
            self.pending_forwards
                .entry(forward.sim.id.clone())
                .or_default()
                .push(input);
            // The gateway still sends here, so it may have lost the route: send it again.
            self.route(&connection, &forward);
            return;
        }
        match input {
            SimInput::ClientMessage {
                connection,
                envelope,
            } => self.client_message(&connection, &envelope),
            SimInput::Left { connection } => {
                if let Some(departure) = self.world.leave(&connection) {
                    self.save(&departure);
                    self.social_left(departure.character);
                }
                self.count_players();
            }
        }
    }

    /// Reads one client message: an input (1589), a respawn request (1938), a lookup
    /// ([`LookupRequest`]) or a facet call of the level flow ([`ClientFacetMsg`]); every other
    /// type is counted and ignored. The
    /// messages it makes wait in the world for the next tick. Bad bytes are counted and never
    /// close the connection.
    fn client_message(&mut self, connection: &ConnectionRef, bytes: &[u8]) {
        let Ok(envelope) = Envelope::decode(bytes) else {
            metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
            return;
        };
        if says_in_game(envelope.type_index()) {
            self.world.heard_from(connection);
        }
        match envelope.type_index() {
            Some(RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX) => {
                self.client_input(connection, &envelope.body);
            }
            Some(REQUEST_RESPAWN_TYPE_INDEX) => self.respawn_request(connection, &envelope.body),
            Some(SPREADSHOT_REQUEST_TYPE_INDEX) => self.spreadshot(connection, &envelope.body),
            Some(index) if let Some(request) = LookupRequest::decode(index, &envelope.body) => {
                match request {
                    Ok(request) => self.world.lookup(connection, &request),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(type_index = index, %error, "lookup does not read");
                    }
                }
            }
            Some(index) if let Some(call) = GmClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.gm_call(connection, call, self.now_ms()),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(
                            type_index = index,
                            %error,
                            body = %hex_of(&envelope.body),
                            "GM call does not read"
                        );
                    }
                }
            }
            Some(index)
                if let Some(request) = MetaAchievementRequest::decode(index, &envelope.body) =>
            {
                match request {
                    Ok(request) => self.world.meta_achievement_call(connection, &request),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(
                            type_index = index,
                            %error,
                            body = %hex_of(&envelope.body),
                            "meta achievement call does not read"
                        );
                    }
                }
            }
            Some(index) if let Some(request) = SetTitleRequest::decode(index, &envelope.body) => {
                match request {
                    Ok(request) => self.world.set_title(connection, &request),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(type_index = index, %error, "title choice does not read");
                    }
                }
            }
            Some(index) if let Some(call) = MountClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.mount_call(connection, call, self.now_ms()),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(
                            type_index = index,
                            %error,
                            body = %hex_of(&envelope.body),
                            "mount call does not read"
                        );
                    }
                }
            }
            Some(index) if let Some(call) = PointsClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.points_call(connection, call),
                    Err(error) => {
                        metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                        tracing::warn!(
                            type_index = index,
                            %error,
                            body = %hex_of(&envelope.body),
                            "point call does not read"
                        );
                    }
                }
            }
            Some(nw_messages::notifications::NOTIFICATION_SENT_TYPE_INDEX) => {
                match nw_messages::NotificationSentMsg::decode(&envelope.body) {
                    Ok(sent) => self.world.notification_sent(connection, sent.notification),
                    Err(error) => bad_call(
                        nw_messages::notifications::NOTIFICATION_SENT_TYPE_INDEX,
                        &error,
                        &envelope.body,
                    ),
                }
            }
            Some(index)
                if let Some(call) = ConversationClientMsg::decode(index, &envelope.body) =>
            {
                match call {
                    Ok(call) => self
                        .world
                        .conversation_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index)
                if let Some(call) = ObjectiveInteractorClientMsg::decode(index, &envelope.body) =>
            {
                match call {
                    Ok(call) => self.world.board_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = ItemCallMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.item_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = TransmogClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.transmog_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = ItemRepairMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.repair_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(option) = ClientOptionMsg::decode(index, &envelope.body) => {
                match option {
                    Ok(option) => self.world.client_option(connection, &option),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(request) = TitleRequest::decode(index, &envelope.body) => {
                match request {
                    Ok(request) => self.title_call(connection, request),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(request) = PlayerDataRequest::decode(index, &envelope.body) => {
                match request {
                    Ok(request) => self.player_data_call(connection, request),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(request) = GuildsRequest::decode(index, &envelope.body) => {
                match request {
                    Ok(request) => self.guilds_call(connection, &request),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = TravelClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.travel_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = FishingClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.fishing_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = MusicClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.music_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if SocialCall::is_call(index) => {
                match self
                    .world
                    .gm_chat(connection, index, &envelope.body, self.now_ms())
                {
                    Some(GmChat::Answered) => {}
                    Some(GmChat::Kick(entity)) => {
                        let reply = self.kick(entity);
                        self.world.answer_gm_chat(connection, answer_lines(reply));
                    }
                    None => {
                        if let Some(character) = self.world.character_of(connection) {
                            self.pending_social.push(SocialInput::Call {
                                character,
                                type_index: index,
                                body: envelope.body.clone(),
                            });
                        }
                    }
                }
            }
            Some(index) if let Some(call) = GameModeClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.game_mode_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = WarClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.war_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = WarSignupClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.war_signup_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = ArenaClientMsg::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.arena_call(connection, call, self.now_ms()),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(call) = EconomyCall::decode(index, &envelope.body) => {
                match call {
                    Ok(call) => self.world.economy_call(connection, call),
                    Err(error) => bad_call(index, &error, &envelope.body),
                }
            }
            Some(index) if let Some(facet) = ClientFacetMsg::decode(index, &envelope.body) => {
                let Ok(facet) = facet else {
                    metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
                    return;
                };
                tracing::debug!(?facet, "client facet call");
                if let Err(error) = self.world.facet_message(connection, facet, self.now_ms()) {
                    tracing::warn!(%error, "facet answer not sent");
                }
            }
            _ => {
                tracing::debug!(
                    type_id = ?envelope.type_id,
                    body = %hex_of(&envelope.body),
                    "ignored client message"
                );
                metrics::counter!(METRIC_IGNORED_MESSAGES).increment(1);
            }
        }
    }

    /// Moves the player by the input's new records (movement study 2026-09-20), then attacks
    /// once per rising edge of its attack bits (M6-B spec section 6).
    fn client_input(&mut self, connection: &ConnectionRef, body: &[u8]) {
        let Ok(input) = ClientInputMsg::decode(body) else {
            metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
            return;
        };
        self.world
            .travel_input(connection, &input.records, self.now_ms());
        // The mount keys first: a dash or a ride changes the speeds the move takes.
        self.world
            .mount_input(connection, &input.records, self.now_ms());
        self.move_player(connection, &input.records);
        let now_ms = self.now_ms();
        match input.interaction() {
            Some(request) => {
                self.world
                    .interact(connection, request, &input.interaction_name(), now_ms);
            }
            None => self.world.interaction_over(connection, now_ms),
        }
        self.world
            .swing_at_quest_objects(connection, &input.records, now_ms);
        // With the fishing pole out the attack key casts and reels, and while performing it
        // plays notes: no swing.
        self.world.fishing_input(connection, &input.records, now_ms);
        if self.world.fishing_holds_attack(connection) || self.world.music_holds_attack(connection)
        {
            return;
        }
        for result in self.world.attack_input(connection, &input.records, now_ms) {
            self.attack_result(result);
        }
    }

    /// Answers a title call (4143 → 4147, 4144): the titles the player earned now are stored
    /// on its account, and the account's titles go back with their new marks.
    fn title_call(&mut self, connection: &ConnectionRef, request: TitleRequest) {
        let call = request.type_index();
        let db = self.db.clone();
        match request {
            TitleRequest::Available { header } => {
                let Some(earned) = self.world.earned_titles(connection) else {
                    return;
                };
                self.look_up(connection, call, async move {
                    let account =
                        earn_account_titles(&db, earned.character, &earned.account).await?;
                    // The titles this earn made new go first as an unlock (3585).
                    let unlocked: Vec<u32> = account
                        .iter()
                        .filter(|title| title.is_new && earned.account.contains(&title.title))
                        .map(|title| title.title)
                        .collect();
                    let mut answers = Vec::new();
                    if !unlocked.is_empty() {
                        answers.push(ServerMessage::TitlesUnlock(TitlesUnlockMsg {
                            header,
                            titles: unlocked,
                        }));
                    }
                    answers.push(titles_answer(header, &account, &earned.character_titles));
                    Ok(answers)
                });
            }
            TitleRequest::ClearNew { .. } => {
                let Some(character) = self.world.character_of(connection) else {
                    return;
                };
                self.look_up(connection, call, async move {
                    clear_new_account_titles(&db, character).await?;
                    Ok(Vec::new())
                });
            }
        }
    }

    /// Answers a player data request (4150 → 4140): the players of this sim from their live
    /// state, the others from the world database.
    fn player_data_call(&mut self, connection: &ConnectionRef, request: PlayerDataRequest) {
        if !self.world.has(connection) {
            return;
        }
        let world_id = self.settings.world_id;
        let characters = requested_characters(&request);
        let live = self.world.live_player_views(&characters, world_id);
        let stored: Vec<Uuid> = characters
            .iter()
            .filter(|character| !live.contains_key(*character))
            .copied()
            .collect();
        if stored.is_empty() {
            let answer = player_data_answer(&request, &characters, live);
            self.send_messages(connection, &[answer]);
            return;
        }
        let db = self.db.clone();
        let content = Arc::clone(&self.settings.content);
        self.look_up(connection, PLAYER_DATA_CALL, async move {
            let mut found = live;
            for player in stored_players(&db, &stored).await? {
                found.insert(
                    player.id,
                    stored_view(&player, &content.progression, world_id),
                );
            }
            Ok(vec![player_data_answer(&request, &characters, found)])
        });
    }

    /// Answers a company look-up (3639 → 3610) from the companies of the world database
    /// ([`guilds_answers`]).
    fn guilds_call(&mut self, connection: &ConnectionRef, request: &GuildsRequest) {
        if !self.world.has(connection) {
            return;
        }
        let ids: Vec<Uuid> = request
            .guilds
            .iter()
            .map(|id| Uuid::from_bytes(*id))
            .collect();
        let request = request.clone();
        let db = self.db.clone();
        self.look_up(connection, REQUEST_GET_GUILDS_TYPE_INDEX, async move {
            let listings = company_listings(&db, &ids).await?;
            Ok(guilds_answers(&request, &listings))
        });
    }

    /// Runs `lookup`, the database work of call `call` of the client of `connection`, as a task.
    fn look_up(
        &mut self,
        connection: &ConnectionRef,
        call: u64,
        lookup: impl Future<Output = Result<Vec<ServerMessage>, nw_db::DbError>> + Send + 'static,
    ) {
        let connection = connection.clone();
        self.lookups.spawn(async move {
            LookupAnswer {
                connection,
                call,
                answer: lookup.await,
            }
        });
    }

    /// Runs the economy jobs of the world, each in its own task (economy module).
    fn run_economy_jobs(&mut self) {
        for job in self.world.take_economy_jobs() {
            let db = self.db.clone();
            self.economy_jobs
                .spawn(async move { run_job(&db, job).await });
        }
    }

    /// Runs the skin collection jobs of the world, each in its own task (transmog module).
    fn run_skin_jobs(&mut self) {
        let now_ms = self.now_ms();
        for job in self.world.take_skin_jobs(now_ms) {
            let db = self.db.clone();
            self.skin_jobs.spawn(async move {
                let collected = collect_account_skins(&db, job.character, &job.collect).await;
                (job, collected)
            });
        }
    }

    /// Gives the answer of a skin collection job to the world.
    fn skins_done(
        &mut self,
        done: Result<(SkinJob, Result<AccountSkins, nw_db::DbError>), tokio::task::JoinError>,
    ) {
        match done {
            Ok((job, collected)) => self.world.skins_collected(&job, collected),
            Err(error) => tracing::error!(%error, "skin job task failed"),
        }
    }

    /// Gives the answer of an economy job to the world.
    fn economy_done(&mut self, done: Result<Option<EconomyResult>, tokio::task::JoinError>) {
        match done {
            Ok(Some(result)) => self.world.economy_done(result),
            Ok(None) => {}
            Err(error) => tracing::error!(%error, "economy job task failed"),
        }
    }

    /// A look-up ended: its answer goes to the client while it is still here.
    fn looked_up(&mut self, result: Result<LookupAnswer, tokio::task::JoinError>) {
        let LookupAnswer {
            connection,
            call,
            answer,
        } = match result {
            Ok(answer) => answer,
            Err(error) => {
                tracing::error!(%error, "look-up task failed");
                return;
            }
        };
        match answer {
            Ok(messages) if self.world.has(&connection) => {
                self.send_messages(&connection, &messages);
            }
            Ok(_) => {}
            Err(error) => {
                metrics::counter!(METRIC_FAILED_LOOKUPS).increment(1);
                tracing::warn!(%error, call, "look-up failed; the call stays unanswered");
            }
        }
    }

    /// Sends `messages` to the client of `connection` with the next flush, reliably.
    fn send_messages(&mut self, connection: &ConnectionRef, messages: &[ServerMessage]) {
        let mut outgoing = Vec::new();
        send_now(&mut outgoing, connection, messages);
        for message in outgoing {
            self.send(message);
        }
    }

    /// The player fires a spread shot (ranged note).
    fn spreadshot(&mut self, connection: &ConnectionRef, body: &[u8]) {
        let Ok(request) = SpreadshotRequestMsg::decode(body) else {
            metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
            return;
        };
        let now_ms = self.now_ms();
        tracing::debug!(pellets = request.pellets.len(), "spread shot");
        let results = self.world.spreadshot(connection, &request, now_ms);
        if results.iter().any(Result::is_ok) {
            self.world.shoot_at_quest_objects(connection, now_ms);
        }
        for result in results {
            self.attack_result(result);
        }
    }

    /// The dead player asks to live again (M6-B spec section 8.2).
    fn respawn_request(&mut self, connection: &ConnectionRef, body: &[u8]) {
        let Ok(request) = RequestRespawnMsg::decode(body) else {
            metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
            return;
        };
        let point = request.respawn_point();
        let result = match self.world.request_respawn(connection, point, self.now_ms()) {
            Ok(at) => {
                tracing::info!(?point, x = at.x, y = at.y, z = at.z, "respawned");
                RESPAWNED
            }
            Err(refusal) => {
                tracing::info!(?point, reason = refusal.reason(), "respawn refused");
                refusal.reason()
            }
        };
        metrics::counter!(METRIC_RESPAWNS, RESULT_LABEL => result).increment(1);
    }

    /// Moves the player of `connection` by the new records of its input `records`
    /// ([`SimWorld::move_from_input`]), and counts a refusal.
    fn move_player(&mut self, connection: &ConnectionRef, records: &[InputRecord]) {
        let Some((result, next)) = self
            .world
            .move_from_input(connection, records, self.now_ms())
        else {
            return;
        };
        if result.is_ok() {
            tracing::trace!(x = next.x, y = next.y, z = next.z, "client move");
        }
        match result {
            Ok(GroundCheck::Checked) | Err(MoveError::NotInWorld | MoveError::Pushed) => {}
            Ok(GroundCheck::NoGround) => {
                metrics::counter!(METRIC_MOVES_WITHOUT_GROUND).increment(1);
            }
            Err(MoveError::Frozen) => metrics::counter!(METRIC_FROZEN_MOVES).increment(1),
            Err(MoveError::Refused(refusal)) => {
                tracing::debug!(
                    reason = refusal.reason(),
                    x = next.x,
                    y = next.y,
                    z = next.z,
                    "refused client move"
                );
                metrics::counter!(METRIC_REFUSED_MOVES, REASON_LABEL => refusal.reason())
                    .increment(1);
            }
            Err(MoveError::Dead) => {
                metrics::counter!(METRIC_REFUSED_MOVES, REASON_LABEL => DEAD_REASON).increment(1);
            }
        }
    }

    /// A handoff offer from another sim, or the cancel of one that this sim took too late.
    fn handoff(&mut self, handoff: Incoming<HandoffMessage>) {
        let reply = match &handoff.message {
            HandoffMessage::Offer(offer) => {
                Some(match self.world.take(offer, self.now_ms(), server_time()) {
                    Ok(level_change) => {
                        if let Some(level_change) = level_change {
                            tracing::info!(character = %offer.character, "entered the level");
                            self.send(level_change);
                        }
                        self.forwards.remove(&offer.connection);
                        self.gateways
                            .joined(&offer.connection.gateway, Instant::now());
                        HandoffReply::Accepted
                    }
                    Err(error) => {
                        tracing::warn!(character = %offer.character, %error, "handoff refused");
                        HandoffReply::Refused {
                            reason: error.to_string(),
                        }
                    }
                })
            }
            HandoffMessage::Cancel {
                connection,
                handoffs,
            } => {
                if self.world.cancel(connection, *handoffs) {
                    tracing::info!(?connection, "late handoff undone");
                }
                None
            }
        };
        if let Some(reply) = reply {
            self.reply(handoff, reply);
        }
        self.count_players();
    }

    fn ghosts_in(&mut self, part: Ghosts) {
        // A round goes to the world once all its parts came.
        let Some(ghosts) = self.ghost_assembly.take(part) else {
            return;
        };
        self.world.adopt_ghost_ids(&ghosts.gde_refs);
        self.world
            .set_ghosts(&ghosts.from, ghosts.entities, self.now_ms());
    }

    /// A hit from another sim on an entity of this one; a target that is gone, dead or returning
    /// drops it (M6-A spec section 7.3). A mob's hit on a player takes its block and dodge here.
    fn hit_in(&mut self, hit: &Hit) {
        let now_ms = self.now_ms();
        let applied = match hit.block_stamina() {
            Some(block_stamina) => self.world.remote_mob_hit(
                hit.target,
                hit.attacker,
                hit.amount,
                crate::world::Blow {
                    stamina: block_stamina,
                    ranged: hit.ranged,
                    blockable: !hit.unblockable,
                },
                now_ms,
            ),
            None => self
                .world
                .apply_hit(hit.target, hit.attacker, hit.amount, hit.threat, now_ms),
        };
        if let Some(outcome) = &applied {
            count_mob_death(outcome);
        }
        let result = if applied.is_some() { APPLIED } else { DROPPED };
        metrics::counter!(METRIC_REMOTE_HITS, RESULT_LABEL => result).increment(1);
    }

    /// Counts an attack of a player or a mob; a hit on a target of another sim goes there.
    fn attack_result(&self, result: Result<AttackOutcome, AttackRefusal>) {
        let label = match &result {
            Ok(outcome) => {
                count_mob_death(outcome);
                outcome.label()
            }
            Err(refusal) => {
                tracing::debug!(reason = refusal.reason(), "attack refused");
                refusal.reason()
            }
        };
        metrics::counter!(METRIC_ATTACKS, RESULT_LABEL => label).increment(1);
        if let Ok(AttackOutcome::Remote(remote)) = result {
            self.send_hit(remote);
        }
    }

    /// Sends a hit to the sim that owns its target.
    fn send_hit(&self, remote: RemoteHit) {
        let subject = subject::sim_hit(self.settings.world_id, &remote.owner);
        if !self.hits.send(subject, remote.hit) {
            metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
        }
    }

    /// A new cell table. A table without this sim makes its players leave without `Close`: the
    /// gateways resume them on other sims (M5 spec section 3.6).
    fn table(&mut self, table: &CellTable) {
        let result = self.world.set_table(table);
        let mobs = self.world.take_mob_changes();
        if mobs != MobChanges::default() {
            tracing::info!(
                spawned = mobs.spawned,
                removed = mobs.removed,
                unspawned = mobs.unspawned,
                "mobs follow the cells"
            );
            metrics::counter!(METRIC_MOBS_UNSPAWNED).increment(mobs.unspawned as u64);
        }
        match result {
            Ok(departures) => {
                if !departures.is_empty() {
                    tracing::warn!(
                        players = departures.len(),
                        "this sim is not in the cell table; its players leave"
                    );
                }
                for departure in departures {
                    self.save(&departure);
                }
                self.count_players();
            }
            Err(error) => tracing::warn!(%error, version = table.version, "cell table ignored"),
        }
    }

    fn gateway_beat(&mut self, beat: &Heartbeat) {
        if self.gateways.beat(beat, Instant::now()) {
            tracing::warn!(gateway = %beat.id, "gateway restarted; its players leave");
            self.drop_gateway(&beat.id);
        }
    }

    /// Saves and removes the player with entity number `entity`, tells its client that its
    /// session ends (271, the retail kick) and its gateway to close its connection (a GM kick).
    fn kick(&mut self, entity: u64) -> AdminReply {
        let Some(connection) = self.world.connection_of(entity) else {
            return AdminReply::NotMine;
        };
        let kick = ServerMessage::KickClient(KickClientMsg {
            reason: RETAIL_KICK_REASON,
        });
        self.send_messages(&connection, &[kick]);
        if let Some(departure) = self.world.leave(&connection) {
            self.save(&departure);
        }
        self.publish(
            &connection.gateway,
            GatewayOutput::Close {
                connection: connection.connection,
            },
        );
        self.count_players();
        AdminReply::Done
    }

    /// Saves and removes every player of `gateway`, and tells the gateway to close them.
    fn drop_gateway(&mut self, gateway: &BusId) {
        for departure in self.world.leave_gateway(gateway) {
            self.save(&departure);
            self.social_left(departure.character);
            self.publish(
                gateway,
                GatewayOutput::Close {
                    connection: departure.connection.connection,
                },
            );
        }
        self.count_players();
    }

    /// Runs the world once when ticks are due. The world changes only on admits, inputs, ghosts
    /// and departures, so a late call sends the current state once instead of every missed tick.
    fn tick(&mut self, clock: &mut TickClock) {
        let now_ms = self.now_ms();
        let now_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let due = clock.due(now_us);
        if due.count == 0 {
            return;
        }
        let missed = due.count - 1;
        if missed > 0 {
            metrics::counter!(METRIC_LATE_TICKS).increment(missed);
            tracing::warn!(missed, "ticks ran late");
        }
        let started = Instant::now();
        let output = match self.world.tick(now_ms, server_time()) {
            Ok(output) => output,
            Err(error) => {
                tracing::error!(%error, "tick failed");
                return;
            }
        };
        metrics::histogram!(METRIC_TICK_SECONDS).record(started.elapsed().as_secs_f64());
        for (part, seconds) in output.parts.labelled() {
            metrics::histogram!(METRIC_TICK_PART_SECONDS, PART_LABEL => part).record(seconds);
        }
        metrics::counter!(METRIC_BUNDLE_MEMBERS).increment(output.bundle_members);
        let timeout = Duration::from_millis(self.settings.world.instance_request_timeout_ms);
        for (connection, request) in self.world.take_instance_requests() {
            self.instance_link.ask(
                &self.bus,
                self.settings.world_id,
                timeout,
                connection,
                request,
            );
        }
        for (state, count) in self.world.mob_counts() {
            metrics::gauge!(METRIC_MOBS, STATE_LABEL => state.label()).set(count as f64);
        }
        let (brain_runs, wake_scans) = self.world.mob_work();
        metrics::counter!(METRIC_MOB_BRAIN_RUNS).absolute(brain_runs);
        metrics::counter!(METRIC_MOB_WAKE_SCANS).absolute(wake_scans);
        for hit in output.mob_hits {
            self.attack_result(Ok(hit));
        }
        for result in output.player_attacks {
            self.attack_result(result);
        }
        for death in &output.deaths {
            metrics::counter!(METRIC_DEATHS, KIND_LABEL => PLAYER_KIND).increment(1);
            self.save(death);
        }
        self.add_stats();
        self.run_economy_jobs();
        self.run_skin_jobs();
        if !output.script_log.is_empty() {
            let subject = subject::sim_script_log(self.settings.world_id, &self.settings.sim_id);
            let log = ScriptLog {
                sim: self.settings.sim_id.clone(),
                lines: output.script_log,
            };
            if !self.script_logs.send(subject, log) {
                metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
            }
        }
        for message in output.messages {
            self.send(message);
        }
        for start in output.handoffs {
            self.offer(start);
        }
        self.flush();
        // One round for every receiver: its parts each fit one bus message (OPEN D3).
        self.ghost_round = self.ghost_round.wrapping_add(1);
        for (target, entities) in output.ghosts {
            let subject = subject::sim_ghosts(self.settings.world_id, &target);
            for mut part in ghost_parts(&self.settings.sim_id, self.ghost_round, entities) {
                part.gde_refs = ghost_ids(&part.entities);
                if !self.ghosts.send(subject.clone(), part) {
                    metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
                }
            }
        }
    }

    /// Adds the leaderboard values of what the players did since the last call, in a task. The
    /// values only add up, so tasks may end in any order; a refused batch is lost and counted
    /// (a retry could count a value twice).
    fn add_stats(&mut self) {
        let events = self.world.take_stat_events();
        if events.is_empty() {
            return;
        }
        let updates: Vec<StatUpdate> = stat_updates(
            &self.settings.content.leaderboards,
            &self.settings.leaderboard_calendar,
            self.settings.world_id,
            &events,
            unix_seconds(),
        );
        if updates.is_empty() {
            return;
        }
        let global = self.global_db.clone();
        tokio::spawn(async move {
            match submit_stats(&global, &updates).await {
                Ok(()) => metrics::counter!(METRIC_STAT_UPDATES)
                    .increment(u64::try_from(updates.len()).unwrap_or(u64::MAX)),
                Err(error) => {
                    metrics::counter!(METRIC_FAILED_STATS).increment(1);
                    tracing::error!(%error, values = updates.len(), "leaderboard stats lost");
                }
            }
        });
    }

    /// Asks the target of a handoff in its own task.
    fn offer(&self, start: HandoffStart) {
        let bus = self.bus.clone();
        let results = self.handoff_results.clone();
        let subject = subject::sim_handoff(self.settings.world_id, &start.target.id);
        let timeout = self.settings.handoff_timeout;
        tokio::spawn(async move {
            let asked = Instant::now();
            let connection = start.offer.connection.clone();
            let sequence = start.offer.handoffs;
            let reply = bus
                .request::<HandoffMessage, HandoffReply>(
                    subject,
                    &HandoffMessage::Offer(Box::new(start.offer)),
                    timeout,
                )
                .await;
            let result = HandoffResult {
                connection,
                target: start.target,
                sequence,
                reply,
                elapsed: asked.elapsed(),
            };
            // The loop may have stopped; then nobody waits for the answer.
            let _ = results.send(result).await;
        });
    }

    /// Ends a handoff: the gateway gets the new route, or the player stays. A target that may
    /// hold the player although this sim keeps it, or although it left, gets a cancel.
    fn handoff_done(&mut self, result: HandoffResult) {
        metrics::histogram!(METRIC_HANDOFF_SECONDS).record(result.elapsed.as_secs_f64());
        let accepted = matches!(result.reply, Ok(HandoffReply::Accepted));
        let late = result.reply.is_err();
        let end = self
            .world
            .handoff_done(&result.connection, accepted, self.now_ms());
        let label = match end {
            HandoffEnd::Moved => {
                let forward = Forward {
                    sim: result.target.clone(),
                    sequence: result.sequence,
                };
                self.route(&result.connection, &forward);
                self.forwards
                    .insert(result.connection.clone(), forward, self.now_ms());
                HANDOFF_MOVED
            }
            HandoffEnd::Kept if late => HANDOFF_TIMEOUT,
            HandoffEnd::Kept => HANDOFF_REFUSED,
            HandoffEnd::Gone => HANDOFF_GONE,
        };
        let taken_elsewhere = matches!(end, HandoffEnd::Gone) && accepted;
        if late || taken_elsewhere {
            let subject = subject::sim_handoff(self.settings.world_id, &result.target.id);
            let cancel = HandoffMessage::Cancel {
                connection: result.connection,
                handoffs: result.sequence,
            };
            if !self.cancels.send(subject, cancel) {
                metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
            }
        }
        metrics::counter!(METRIC_HANDOFFS, RESULT_LABEL => label).increment(1);
        self.count_players();
    }

    /// Tells the gateway of `connection` that its player moved by `forward`.
    fn route(&mut self, connection: &ConnectionRef, forward: &Forward) {
        self.publish(
            &connection.gateway,
            GatewayOutput::Route {
                connection: connection.connection,
                sim: forward.sim.id.clone(),
                generation: forward.sim.generation,
                sequence: forward.sequence,
            },
        );
    }

    /// Publishes the admin snapshot of this sim, in parts (admin tools spec section 5).
    fn snapshot(&mut self) {
        let started = Instant::now();
        let (players, mobs, counts) = self.world.snapshot();
        let subject = subject::sim_snapshot(self.settings.world_id, &self.settings.sim_id);
        let parts = snapshot_parts(
            self.settings.sim_id.clone(),
            unix_ms(),
            players,
            mobs,
            counts,
        );
        for part in parts {
            if !self.snapshots.send(subject.clone(), part) {
                metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
            }
        }
        metrics::histogram!(METRIC_SNAPSHOT_SECONDS).record(started.elapsed().as_secs_f64());
    }

    fn heartbeat(&mut self) {
        let now_ms = self.now_ms();
        let beat = SimHeartbeat {
            id: self.settings.sim_id.clone(),
            started_at_ms: self.started_at_ms,
            players: u32::try_from(self.world.players()).unwrap_or(u32::MAX),
            max_players: self.settings.world.max_players,
            level: self.settings.level.clone(),
            instance: self.world.instance_beat(now_ms),
        };
        let subject = subject::sim_heartbeat(self.settings.world_id, &self.settings.sim_id);
        if !self.beats.send(subject, beat) {
            tracing::warn!("heartbeat dropped; NATS is behind");
        }
        for gateway in self.gateways.silent(Instant::now()) {
            tracing::warn!(%gateway, "gateway silent; its players leave");
            self.drop_gateway(&gateway);
        }
        self.forwards.expire(self.now_ms());
        let world = &self.world;
        self.direct.retain(|connection| world.has(connection));
        metrics::gauge!(METRIC_GHOSTS).set(self.world.ghosts() as f64);
    }

    fn count_players(&self) {
        metrics::gauge!(METRIC_PLAYERS).set(self.world.players() as f64);
    }

    fn send(&mut self, outgoing: Outgoing) {
        self.publish(
            &outgoing.connection.gateway,
            GatewayOutput::ServerMessages {
                connection: outgoing.connection.connection,
                channel: outgoing.channel,
                reliable: outgoing.reliable,
                envelopes: outgoing.envelopes,
            },
        );
    }

    /// Queues `message` for the batch of `gateway`.
    fn publish(&mut self, gateway: &BusId, message: GatewayOutput) {
        self.pending_outputs
            .entry(gateway.clone())
            .or_default()
            .push(message);
    }

    /// Sends the waiting gateway outputs and forwarded inputs, one batch per receiver.
    fn flush(&mut self) {
        for (gateway, outputs) in std::mem::take(&mut self.pending_outputs) {
            let subject = subject::gateway_output(self.settings.world_id, &gateway);
            for outputs in split_batches(outputs, GatewayOutput::size_bytes) {
                let batch = OutputBatch {
                    sim: self.settings.sim_id.clone(),
                    outputs,
                };
                if !self.outputs.send(subject.clone(), batch) {
                    metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
                }
            }
        }
        let vitals = self.world.social_vitals();
        self.pending_social.extend(vitals);
        if !self.pending_social.is_empty() {
            let batch = SocialBatch {
                sim: self.settings.sim_id.clone(),
                inputs: std::mem::take(&mut self.pending_social),
            };
            if !self
                .social
                .send(subject::social_input(self.settings.world_id), batch)
            {
                metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
            }
        }
        for (sim, inputs) in std::mem::take(&mut self.pending_forwards) {
            let subject = subject::sim_input(self.settings.world_id, &sim);
            for inputs in split_batches(inputs, SimInput::size_bytes) {
                let batch = InputBatch {
                    forwarded: true,
                    inputs,
                };
                if !self.sims.send(subject.clone(), batch) {
                    metrics::counter!(METRIC_DROPPED_OUTPUTS).increment(1);
                }
            }
        }
    }

    /// Saves every player and retries every save that failed before.
    /// The save round: every position, and the items that changed.
    fn save_all(&mut self) {
        let departures = self.world.positions();
        self.save_round(&departures);
        let players = self.world.social_players(&self.settings.sim_id);
        self.pending_social
            .extend(players.into_iter().map(SocialInput::Updated));
    }

    /// The player of `character` logged out: the social service hears it.
    fn social_left(&mut self, character: Uuid) {
        self.pending_social.push(SocialInput::Left {
            character,
            reason: LeaveReason::Logout,
        });
    }

    /// Sends the social service's messages to this sim's players.
    fn social_deliveries(&mut self, deliveries: &SocialDeliveries) {
        for outgoing in self.world.social_outgoing(&deliveries.deliveries) {
            self.send(outgoing);
        }
        self.world.apply_social_states(deliveries);
    }

    /// Moves a player's coins for the social service and answers it.
    fn move_coins(&mut self, transfer: Incoming<CoinTransfer>) {
        let reply = self.world.move_coins(&transfer.message);
        tracing::info!(character = %transfer.message.character, change = transfer.message.change, ?reply, "treasury coins");
        self.reply(transfer, reply);
    }

    /// Saves `departures`, and starts the waiting saves again.
    fn save_round(&mut self, departures: &[Departure]) {
        for departure in departures {
            self.save(departure);
        }
        for (character, position) in self.queue.retry() {
            self.spawn_save(character, position);
        }
        for (character, items) in self.item_queue.retry() {
            self.spawn_item_save(character, items);
        }
        for (character, quests) in self.quest_queue.retry() {
            self.spawn_quest_save(character, quests);
        }
        for (character, progression) in self.progression_queue.retry() {
            self.spawn_progression_save(character, progression);
        }
    }

    fn save(&mut self, departure: &Departure) {
        let position = SavedPosition {
            level: self.settings.level.clone(),
            x: f64::from(departure.position.x),
            y: f64::from(departure.position.y),
            z: f64::from(departure.position.z),
            yaw: NO_YAW,
            // The Unix time of the last good move, so a stale copy loses (M5 spec section 7.5).
            saved_at_ms: self.started_at_ms.saturating_add(departure.moved_at_ms),
        };
        if let Some(position) = self.queue.want(departure.character, position) {
            self.spawn_save(departure.character, position);
        }
        if let Some(items) = &departure.items {
            let save = ItemSave {
                items: items.clone(),
                inventory: departure.inventory.clone().unwrap_or_default(),
                taken_at_ms: unix_ms(),
            };
            if let Some(save) = self.item_queue.want(departure.character, save) {
                self.spawn_item_save(departure.character, save);
            }
        }
        if let Some(quests) = &departure.quests {
            let save = QuestSave {
                quests: quests.clone(),
                taken_at_ms: unix_ms(),
            };
            if let Some(save) = self.quest_queue.want(departure.character, save) {
                self.spawn_quest_save(departure.character, save);
            }
        }
        if let Some(progression) = &departure.progression {
            let save = ProgressionSave {
                progression: progression.clone(),
                taken_at_ms: unix_ms(),
            };
            if let Some(save) = self.progression_queue.want(departure.character, save) {
                self.spawn_progression_save(departure.character, save);
            }
        }
    }

    /// Runs `store`, a save of `kind` of `character`, as a task; a newer stored value is a
    /// finished save too.
    fn spawn_store(
        &mut self,
        kind: SaveKind,
        character: Uuid,
        store: impl Future<Output = Result<SaveOutcome, nw_db::DbError>> + Send + 'static,
    ) {
        self.saves.spawn(async move {
            let ok = match store.await {
                Ok(SaveOutcome::Saved | SaveOutcome::Older) => true,
                Err(error) => {
                    tracing::error!(%error, %character, ?kind, "save failed");
                    false
                }
            };
            (kind, character, ok)
        });
    }

    fn spawn_save(&mut self, character: Uuid, position: SavedPosition) {
        let db = self.db.clone();
        self.spawn_store(SaveKind::Position, character, async move {
            save_character_position(&db, character, &position).await
        });
    }

    fn spawn_item_save(&mut self, character: Uuid, save: ItemSave) {
        let db = self.db.clone();
        self.spawn_store(SaveKind::Items, character, async move {
            save_character_items(
                &db,
                character,
                &saved_items(&save.items),
                &saved_inventory(&save.inventory),
                save.taken_at_ms,
            )
            .await
        });
    }

    fn spawn_quest_save(&mut self, character: Uuid, save: QuestSave) {
        let db = self.db.clone();
        self.spawn_store(SaveKind::Quests, character, async move {
            save_character_quests(
                &db,
                character,
                &saved_quests(&save.quests),
                save.taken_at_ms,
            )
            .await
        });
    }

    fn spawn_progression_save(&mut self, character: Uuid, save: ProgressionSave) {
        let db = self.db.clone();
        self.spawn_store(SaveKind::Progression, character, async move {
            save_character_progression(
                &db,
                character,
                &saved_progression(&save.progression, save.taken_at_ms),
                save.taken_at_ms,
            )
            .await
        });
    }

    /// A save ended; the next wanted value of that character and kind starts now.
    fn saved(&mut self, result: Result<SaveResult, tokio::task::JoinError>) {
        let (kind, character, ok) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "save task failed");
                return;
            }
        };
        if !ok {
            metrics::counter!(METRIC_FAILED_SAVES).increment(1);
        }
        match kind {
            SaveKind::Position => {
                if let Some(next) = self.queue.finished(character, ok) {
                    self.spawn_save(character, next);
                }
            }
            SaveKind::Items => {
                if let Some(next) = self.item_queue.finished(character, ok) {
                    self.spawn_item_save(character, next);
                }
            }
            SaveKind::Quests => {
                if let Some(next) = self.quest_queue.finished(character, ok) {
                    self.spawn_quest_save(character, next);
                }
            }
            SaveKind::Progression => {
                if let Some(next) = self.progression_queue.finished(character, ok) {
                    self.spawn_progression_save(character, next);
                }
            }
        }
    }

    /// Tells every client that the world closes (1630), sends what waits, saves every player
    /// once more, and waits at most [`SHUTDOWN_SAVE_WAIT`] for every save.
    async fn finish(&mut self) {
        let departures = self.world.departures();
        // Retail took its players out so at a maintenance (`MSQ5-lv25`).
        let closing = [ServerMessage::PlayerManagerRejected(
            PlayerManagerRejectedMsg::maintenance(),
        )];
        for departure in &departures {
            self.send_messages(&departure.connection, &closing);
        }
        self.flush();
        self.save_round(&departures);
        let drained = tokio::time::timeout(SHUTDOWN_SAVE_WAIT, async {
            while let Some(saved) = self.saves.join_next().await {
                self.saved(saved);
            }
        })
        .await;
        let waiting = self.queue.waiting()
            + self.item_queue.waiting()
            + self.quest_queue.waiting()
            + self.progression_queue.waiting();
        if drained.is_err() || waiting > 0 {
            tracing::error!(
                waiting,
                "positions, items, quests or progression not saved at shutdown"
            );
        }
    }
}

/// The quests as the database stores them: the run as JSON.
fn saved_quests(quests: &PlayerQuests) -> SavedQuests {
    SavedQuests {
        active: quests
            .active
            .iter()
            .filter_map(|(objective, run)| {
                serde_json::to_value(run)
                    .ok()
                    .map(|run| (objective.clone(), run))
            })
            .collect(),
        done: quests.done.clone(),
    }
}

/// The progression as the database stores it, taken at unix ms `taken_at_ms`.
fn saved_progression(progression: &PlayerProgression, taken_at_ms: u64) -> SavedProgression {
    SavedProgression {
        experience: progression.experience,
        coins: progression.coins,
        achievements: progression.achievements.clone(),
        loot_limits: progression
            .loot_limits
            .iter()
            .map(|limit| SavedLootLimit {
                limit_id: limit.limit_id.clone(),
                count: limit.count,
                counted_ns: limit.counted_ns,
                ends_ns: limit.ends_ns,
            })
            .collect(),
        masteries: progression.masteries.clone(),
        attributes: progression.attributes.clone(),
        weapon_points: progression.weapon_points.clone(),
        territory_points: progression.territory_points.clone(),
        travel: saved_travel(&progression.travel),
        ability_slots: progression.ability_slots.clone(),
        meta_counters: progression.meta_counters.clone(),
        meta_completed: progression.meta_completed.clone(),
        title: progression.title,
        mounts: saved_mounts(&progression.mounts),
        arena: saved_arena(&progression.arena),
        music: nw_db::SavedMusic {
            next_performance_ms: progression.music.next_performance_ms,
        },
        notifications: nw_db::SavedNotifications {
            shown: progression.notifications.clone(),
        },
        recipe_cooldowns: progression
            .recipe_cooldowns
            .iter()
            .map(|cooldown| nw_db::SavedRecipeCooldown {
                recipe_id: cooldown.recipe_id.clone(),
                count: cooldown.count,
                ends_ns: cooldown.ends_ns,
            })
            .collect(),
        faction: nw_db::SavedFaction {
            faction: progression.faction.faction,
            changes: progression.faction.changes,
            changed_ns: progression.faction.changed_ns,
        },
        status_effects: progression
            .status_effects
            .iter()
            .map(|effect| nw_db::SavedStatusEffect {
                status_id: effect.status_id.clone(),
                consumable_id: effect.consumable_id.clone(),
                ends_ns: effect.ends_ns,
                stacks: effect.stacks,
                runs_offline: effect.runs_offline,
                repeat: effect.repeat.map(|repeat| (repeat.next_ns, repeat.ends_ns)),
                saved_ns: taken_at_ms.saturating_mul(NS_PER_MS),
            })
            .collect(),
        consumable_cooldowns: progression
            .consumable_cooldowns
            .iter()
            .map(|cooldown| nw_db::SavedConsumableCooldown {
                group: cooldown.group,
                consumable: cooldown.consumable,
                end_us: cooldown.end_us,
            })
            .collect(),
    }
}

/// The arena state as the database stores it.
fn saved_arena(arena: &nw_bus::PlayerArena) -> nw_db::SavedArena {
    nw_db::SavedArena {
        dungeon_cooldown_since_2000_ns: arena.dungeon_cooldown_since_2000_ns,
        solo_trial_cooldown_since_2000_ns: arena.solo_trial_cooldown_since_2000_ns,
        ranks: arena.ranks.clone(),
        last_dungeon: arena.last_dungeon,
        last_mutated_dungeon: arena.last_mutated_dungeon,
        base_entered: arena.base_entered,
        mutated_entered: arena.mutated_entered,
        group_trials_entered: arena.group_trials_entered,
        next_base_refresh_ns: arena.next_base_refresh_ns,
        next_mutated_refresh_ns: arena.next_mutated_refresh_ns,
        next_group_trial_refresh_ns: arena.next_group_trial_refresh_ns,
        mutation_award: arena.mutation_award,
        last_dungeon_mutated: arena.last_dungeon_mutated,
        stay: arena.stay.as_ref().map(|stay| nw_db::SavedStay {
            instance: stay.instance,
            game_mode: stay.game_mode.clone(),
            phase: stay.phase,
            return_level: stay.return_level.clone(),
            return_position: stay.return_position.bits(),
            activated_ns: stay.activated_ns,
        }),
    }
}

/// The mounts as the database stores them: the chosen mount and the customized ones; the ride
/// is not kept (a login starts on foot).
fn saved_mounts(mounts: &nw_bus::PlayerMounts) -> SavedMounts {
    SavedMounts {
        selected: mounts.selected,
        customized: mounts
            .customized
            .iter()
            .map(|mount| SavedMount {
                mount_id: mount.mount_id,
                dye: mount.dye,
                name: mount.name.clone(),
            })
            .collect(),
        summon_cooldown_end_ns: mounts.summon_cooldown_end_ns,
    }
}

/// The travel state as the database stores it.
fn saved_travel(travel: &nw_bus::PlayerTravel) -> SavedTravel {
    SavedTravel {
        waypoint: travel.waypoint.map(nw_bus::StoredPoint::bits),
        inn: travel.inn,
        camp: travel.camp,
        camp_skin: travel.camp_skin,
        unstuck_ready_ms: travel.unstuck_ready_ms,
        homes: travel
            .homes
            .iter()
            .map(|home| SavedHome {
                plot: home.plot,
                position: home.position.bits(),
                purchased_ms: home.purchased_ms,
                items: home
                    .items
                    .iter()
                    .map(|item| SavedHousingItem {
                        index: item.index,
                        item: item.item,
                        position: item.position.bits(),
                        rotation: item.rotation,
                    })
                    .collect(),
                record: SavedHomeRecord {
                    plot_facet: home.record.plot_facet,
                    plot_extra: home.record.plot_extra,
                    house_data: home.record.house_data,
                    house_data_facet: home.record.house_data_facet,
                    house_type: home.record.house_type,
                    territory: home.record.territory,
                    taxes_due_ms: home.record.taxes_due_ms,
                    fast_travel_ready_ms: home.record.fast_travel_ready_ms,
                },
            })
            .collect(),
        played_ms: travel.played_ms,
        inn_recalled_ms: travel.inn_recalled_ms,
        placed_camp: travel.placed_camp.map(|camp| SavedCamp {
            position: camp.position.bits(),
            tier: camp.tier,
            placed_ms: camp.placed_ms,
            rotation: camp.rotation,
            blueprint: camp.blueprint,
        }),
    }
}

/// The item list as the database stores it.
fn saved_items(items: &[CarriedItem]) -> Vec<SavedItem> {
    items
        .iter()
        .map(|item| SavedItem {
            item_id: item.item_id.clone(),
            quantity: item.quantity,
            place: match item.place {
                ItemPlace::Worn(slot) => SavedPlace::Worn(slot),
                ItemPlace::Bag(slot) => SavedPlace::Bag(slot),
            },
            instance: item.state.instance.map(Uuid::from_bytes),
            locked: item.state.locked,
            rolls: saved_rolls(&item.state),
        })
        .collect()
}

/// The rolls and look of a carried item as the database stores them.
fn saved_rolls(state: &ItemState) -> SavedItemRolls {
    SavedItemRolls {
        gear_score: state.gear_score,
        durability: state.durability,
        perks: state.perks.clone(),
        bound: state.bound,
        dye: state.look.dye,
        appearance: state.look.appearance,
        skin_dye: state.look.skin_dye,
    }
}

/// The storage chests and loadouts as the database stores them.
fn saved_inventory(inventory: &PlayerInventory) -> SavedInventory {
    SavedInventory {
        stored: inventory
            .stored
            .iter()
            .map(|item| SavedStoredItem {
                storage: Uuid::from_bytes(item.storage),
                slot: item.slot,
                item_id: item.item_id.clone(),
                quantity: item.quantity,
                instance: item.state.instance.map(Uuid::from_bytes),
                locked: item.state.locked,
                rolls: saved_rolls(&item.state),
            })
            .collect(),
        loadouts: inventory
            .loadouts
            .iter()
            .map(|loadout| SavedLoadout {
                index: loadout.index,
                name: loadout.name.clone(),
                items: loadout
                    .items
                    .iter()
                    .map(|(slot, instance)| (*slot, Uuid::from_bytes(*instance)))
                    .collect(),
                attributes: loadout.attributes.clone(),
            })
            .collect(),
    }
}

/// Milliseconds since 2000-01-01. The unit of the retail `TimeSynchMsg` is Unknown.
/// The next tick of the snapshot timer; never, when snapshots are off.
async fn next_snapshot(timer: Option<&mut tokio::time::Interval>) {
    match timer {
        Some(timer) => {
            timer.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Counts a mob that `outcome` killed; a killed player is counted with its departure.
fn count_mob_death(outcome: &AttackOutcome) {
    if let AttackOutcome::Killed { target } = outcome
        && spawn_index(*target).is_some()
    {
        metrics::counter!(METRIC_DEATHS, KIND_LABEL => MOB_KIND).increment(1);
    }
}

/// `bytes` as lowercase hex, for the debug log.
/// Counts and logs a client call of type `index` whose body does not read.
fn bad_call(index: u64, error: &nw_messages::MessageError, body: &[u8]) {
    metrics::counter!(METRIC_BAD_MESSAGES).increment(1);
    tracing::warn!(type_index = index, %error, body = %hex_of(body), "client call does not read");
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A client message of `type_index` shows that its client is in the game: a time sync
/// request (see [`SimWorld::heard_from`]). The teardown calls of a level change do not,
/// and movement input does not either: the client sends it on a fresh connection before
/// the new level has loaded (live 2026-09-22, the beach switch).
const fn says_in_game(type_index: Option<u64>) -> bool {
    matches!(type_index, Some(TIME_SYNC_REQUEST_TYPE_INDEX))
}

fn server_time() -> u64 {
    unix_ms().saturating_sub(SERVER_EPOCH_UNIX_MS)
}

/// The raw id of each ghost, by entity number, for the receiving sim ([`Ghosts::gde_refs`]).
fn ghost_ids(ghosts: &[Ghost]) -> Vec<(u64, [u8; nw_messages::RAW_ID_LEN])> {
    ghosts
        .iter()
        .map(|ghost| (ghost.entity, nw_messages::raw_id_of_entity(ghost.entity)))
        .collect()
}

#[cfg(test)]
mod ghost_id_tests {
    use nw_bus::Point;

    use super::*;

    /// An entity number no other test of this process gives a raw id.
    const OWNED: u64 = 0x3e00_0000_0001;

    #[test]
    fn a_ghost_batch_names_each_ghost_by_its_owners_id() {
        let ghost = Ghost {
            entity: OWNED,
            position: Point {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            mob: None,
            player: None,
        };
        assert_eq!(
            ghost_ids(&[ghost]),
            [(OWNED, nw_messages::raw_id_of_entity(OWNED))]
        );
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;
    use crate::mobs::mob_entity;

    #[test]
    fn a_killed_mob_counts_a_mob_death() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            count_mob_death(&AttackOutcome::Killed {
                target: mob_entity(0),
            });
            count_mob_death(&AttackOutcome::Killed { target: 7 });
            count_mob_death(&AttackOutcome::Hit {
                target: mob_entity(1),
                amount: 1,
            });
        });
        let text = handle.render();
        let mob = format!("{METRIC_DEATHS}{{{KIND_LABEL}=\"{MOB_KIND}\"}} 1");
        assert!(text.contains(&mob), "{text}");
        assert!(
            !text.contains(PLAYER_KIND),
            "a player death counts with its departure"
        );
    }

    #[test]
    fn only_a_time_sync_shows_the_client_in_the_game() {
        /// A teardown call of a level change (live 2026-09-18).
        const TEARDOWN_TYPE_INDEX: u64 = 4143;
        assert!(says_in_game(Some(TIME_SYNC_REQUEST_TYPE_INDEX)));
        // Movement input comes on a fresh connection before the level has loaded
        // (live 2026-09-22, the beach switch), so it shows nothing.
        assert!(!says_in_game(Some(RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX)));
        assert!(!says_in_game(Some(TEARDOWN_TYPE_INDEX)));
        assert!(!says_in_game(None));
    }
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use nw_bus::test_support::test_bus;
    use nw_bus::{HandoffOffer, Subscription};
    use nw_content::respawn_points::nearest_respawn_point;
    use nw_db::test_db::with_test_schema;
    use nw_db::{
        NewCharacter, WORLD_MIGRATOR, character_items, character_position, character_progression,
        character_quests, insert_character,
    };

    /// A finished objective a test player carries.
    const DONE_QUEST: &str = "01_SoleSurvivor2";
    use nw_messages::{
        ALC_TYPE_INDEX, DAMAGE_DEALT_TYPE_INDEX, DEATH_RECAP_TYPE_INDEX, EntitySlots,
        GDE_METADATA_TYPE_INDEX, InputPosition, LOCAL_PLAYER_INTEREST, LOGIN_MESSAGE_TYPES,
        ServerMessage, SlotRead,
    };
    use nw_reflect::TypeId;
    use nw_sim_core::test_support::NoGround;
    use nw_sim_core::{CellBounds, CellGrid, CellOwners};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::*;
    use nw_formats::objectstream::lowercase_crc32;

    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB, TEST_PLAYER_HEALTH};
    use crate::combat::tests::test_combat;
    use crate::config::tests::committed;
    use crate::mobs::VitalsSpawn;
    use crate::mobs::mob_entity;
    use crate::world::{MESSAGE_CHANNEL, REPLICATION_CHANNEL};

    const WAIT: Duration = Duration::from_secs(5);
    const LEVEL: &str = "newworld_vitaeeterna";
    const LEVEL_M: f32 = 14_336.0;
    const CELL_M: f32 = 2048.0;
    /// The first column of the second of two sims.
    const BORDER_X: f32 = 3.0 * CELL_M;
    const Y: f32 = 7000.0;
    const Z: f32 = 100.0;
    const CONNECTION: u64 = 9;
    const SAVE_INTERVAL: Duration = Duration::from_secs(60);
    /// Every test process beats fast, so silence shows in 0.3 s.
    const HEARTBEAT: Duration = Duration::from_millis(100);
    const MISSES: u32 = 3;
    /// The sim subscribes at start; an admit may come before that.
    const TRIES: usize = 50;
    const PAUSE: Duration = Duration::from_millis(100);
    const SAVE_TOLERANCE_M: f64 = 0.01;
    const CHARACTER_NAME: &str = "Ada";
    /// A step the limits allow at once.
    const STEP_M: f32 = 2.0;
    const MOVE_TOLERANCE_M: f32 = 0.01;
    /// A crossing the full budget allows (21.45 m): from 10 m west to 10 m east of the border.
    const CROSSING_M: f32 = 10.0;
    /// A step the budget left after the crossing allows at once.
    const LATE_STEP_M: f32 = 1.0;
    /// Time to watch for a message that must not come.
    const QUIET: Duration = Duration::from_millis(500);
    const FIRST_HANDOFF: u64 = 1;
    const FAKE_GENERATION: u64 = 1;

    fn bounds() -> CellBounds {
        CellBounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: LEVEL_M,
            max_y: LEVEL_M,
        }
    }

    fn id(prefix: &str) -> BusId {
        BusId::try_from(format!("{prefix}-{}", Uuid::new_v4().simple())).expect("id")
    }

    /// The sea height of the test level (`terrain.json` `oceanLevel` of the open world).
    const TEST_OCEAN_LEVEL_M: f32 = 40.0;

    fn settings(world_id: Uuid, name: &str) -> LoopSettings {
        let config = committed();
        let mut world = config
            .world_settings(
                bounds(),
                TEST_OCEAN_LEVEL_M,
                &nw_content::instances::InstanceCatalog::default(),
            )
            .expect("world settings");
        // The live path under test: own player card on, other entities held until the client
        // sends movement (local-player-check note).
        world.local_player_card = true;
        LoopSettings {
            sim_id: id(name),
            world_id,
            level: LEVEL.to_owned(),
            world,
            handoff_timeout: Duration::from_millis(config.handoff_timeout_ms),
            forward_ttl_ms: config.forward_ttl_ms,
            save_interval: SAVE_INTERVAL,
            heartbeat_interval: HEARTBEAT,
            heartbeat_misses: MISSES,
            combat: test_combat(),
            content: Arc::default(),
            snapshot_interval: None,
            mob_ai: Arc::from([]),
            design: Arc::default(),
            leaderboard_calendar: config.leaderboard_calendar().expect("calendar"),
        }
    }

    fn at(x: f32) -> Point {
        Point { x, y: Y, z: Z }
    }

    /// A character of a new account: (account, character).
    async fn character(db: &PgPool) -> (Uuid, Uuid) {
        let account = Uuid::new_v4();
        let character = insert_character(
            db,
            &NewCharacter {
                id: Uuid::new_v4(),
                account_id: account,
                name: CHARACTER_NAME.to_owned(),
                appearance: serde_json::json!({}),
            },
        )
        .await
        .expect("character");
        (account, character.id)
    }

    /// A running sim and the sender that stops it.
    struct Running {
        settings: LoopSettings,
        stop: oneshot::Sender<()>,
        server: JoinHandle<Result<(), ServeError>>,
    }

    impl Running {
        async fn start(bus: &Bus, settings: LoopSettings, db: &PgPool) -> Self {
            let (stop, stopped) = oneshot::channel::<()>();
            let server = tokio::spawn(serve(
                settings.clone(),
                Arc::new(NoGround),
                bus.clone(),
                Databases {
                    world: db.clone(),
                    global: db.clone(),
                },
                async {
                    let _ = stopped.await;
                },
            ));
            Self {
                settings,
                stop,
                server,
            }
        }

        fn id(&self) -> &BusId {
            &self.settings.sim_id
        }

        async fn stop(self) {
            let _ = self.stop.send(());
            self.server.await.expect("join").expect("serve");
        }
    }

    /// The test's gateway: its output batches, read one output at a time, and a task that beats
    /// for it.
    struct TestGateway {
        id: BusId,
        outputs: Subscription<OutputBatch>,
        waiting: VecDeque<(BusId, GatewayOutput)>,
        /// The entity slots each connection saw so far.
        entities: BTreeMap<u64, EntitySlots>,
        beats: JoinHandle<()>,
    }

    impl TestGateway {
        async fn start(bus: &Bus, world_id: Uuid, beating: bool) -> Self {
            let id = id("gw");
            let outputs = bus
                .subscribe::<OutputBatch>(subject::gateway_output(world_id, &id))
                .await
                .expect("outputs");
            bus.flush().await.expect("flush");
            let beater = bus.clone();
            let beat = Heartbeat {
                id: id.clone(),
                started_at_ms: unix_ms(),
            };
            let beats = tokio::spawn(async move {
                if !beating {
                    return;
                }
                loop {
                    let subject = subject::gateway_heartbeat(world_id, &beat.id);
                    beater.publish(subject, &beat).await.expect("beat");
                    tokio::time::sleep(HEARTBEAT).await;
                }
            });
            Self {
                id,
                outputs,
                waiting: VecDeque::new(),
                entities: BTreeMap::new(),
                beats,
            }
        }

        fn connection(&self, number: u64) -> ConnectionRef {
            ConnectionRef {
                gateway: self.id.clone(),
                connection: number,
            }
        }

        /// The next output, the sim that sent it, and its bundles read with the slots its
        /// connection saw so far.
        async fn next_from(&mut self) -> (BusId, GatewayOutput, Vec<SlotRead>) {
            while self.waiting.is_empty() {
                let batch = tokio::time::timeout(WAIT, self.outputs.next())
                    .await
                    .expect("output in time")
                    .expect("open")
                    .expect("decode")
                    .message;
                let sim = batch.sim;
                self.waiting.extend(
                    batch
                        .outputs
                        .into_iter()
                        .map(|output| (sim.clone(), output)),
                );
            }
            let (sim, output) = self.waiting.pop_front().expect("an output waits");
            let reads = match &output {
                GatewayOutput::ServerMessages {
                    connection,
                    channel: REPLICATION_CHANNEL,
                    envelopes,
                    ..
                } => {
                    let slots = self.entities.entry(*connection).or_default();
                    envelopes
                        .iter()
                        .map(|envelope| {
                            let mut read = read_bundle(envelope);
                            slots.resolve(&mut read);
                            read
                        })
                        .collect()
                }
                GatewayOutput::ServerMessages {
                    connection,
                    channel: MESSAGE_CHANNEL,
                    envelopes,
                    ..
                } => {
                    for envelope in envelopes {
                        let Ok(envelope) = Envelope::decode(envelope) else {
                            continue;
                        };
                        let Some(Ok(ServerMessage::ClientAddEntry(entry))) =
                            ServerMessage::from_envelope(&envelope)
                        else {
                            continue;
                        };
                        let Some(entity) = entry.entity_number() else {
                            continue;
                        };
                        self.entities
                            .entry(*connection)
                            .or_default()
                            .bind(LOCAL_PLAYER_INTEREST, entity);
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            (sim, output, reads)
        }

        /// The next bundles (with their sender and connection) that `wanted` picks, within
        /// [`WAIT`].
        async fn until_bundles<T>(
            &mut self,
            mut wanted: impl FnMut(&BusId, u64, &[SlotRead]) -> Option<T>,
        ) -> T {
            let deadline = Instant::now() + WAIT;
            loop {
                assert!(Instant::now() < deadline, "no wanted bundles in time");
                let (sim, output, reads) = self.next_from().await;
                if let GatewayOutput::ServerMessages {
                    connection,
                    channel: REPLICATION_CHANNEL,
                    ..
                } = output
                    && let Some(found) = wanted(&sim, connection, &reads)
                {
                    return found;
                }
            }
        }

        /// The next output (with its sender) that `wanted` picks, within [`WAIT`].
        async fn until_from<T>(
            &mut self,
            mut wanted: impl FnMut(BusId, GatewayOutput) -> Option<T>,
        ) -> T {
            let deadline = Instant::now() + WAIT;
            loop {
                assert!(Instant::now() < deadline, "no wanted output in time");
                let (sim, output, _) = self.next_from().await;
                if let Some(found) = wanted(sim, output) {
                    return found;
                }
            }
        }

        /// Drops the outputs of the next [`PAUSE`]: every older output that waits, and some new.
        async fn drain(&mut self) {
            self.waiting.clear();
            let deadline = tokio::time::Instant::now() + PAUSE;
            while tokio::time::timeout_at(deadline, self.outputs.next())
                .await
                .is_ok()
            {}
        }

        /// The next output that `wanted` picks, within [`WAIT`].
        async fn until<T>(&mut self, mut wanted: impl FnMut(GatewayOutput) -> Option<T>) -> T {
            self.until_from(|_, output| wanted(output)).await
        }

        /// The entity numbers of the next bundle that `sim` sends to `connection`.
        async fn bundle_from(&mut self, sim: &BusId, connection: u64) -> Vec<u64> {
            self.until_bundles(|from, to, reads| {
                (from == sim && to == connection).then(|| entities_in(&reads[0]))
            })
            .await
        }
    }

    impl Drop for TestGateway {
        fn drop(&mut self) {
            self.beats.abort();
        }
    }

    fn read_bundle(envelope: &[u8]) -> SlotRead {
        let envelope = Envelope::decode(envelope).expect("envelope");
        let Some(Ok(ServerMessage::Bundle(bundle))) = ServerMessage::from_envelope(&envelope)
        else {
            panic!("expected a bundle");
        };
        SlotRead::of(&bundle).expect("read")
    }

    /// The entity numbers of a bundle, in order.
    fn entities_in(read: &SlotRead) -> Vec<u64> {
        read.updates
            .iter()
            .map(|update| update.entity.expect("every slot is tied"))
            .collect()
    }

    /// The own x of a bundle; `None` when its own states carry no position (a state that only
    /// numbers the newest input record).
    fn own_x(read: &SlotRead) -> Option<f32> {
        read.local_player_position().map(|position| position.x)
    }

    /// Whether the own x of a bundle is `x`.
    fn own_x_is(read: &SlotRead, x: f32) -> bool {
        own_x(read).is_some_and(|own| (own - x).abs() < MOVE_TOLERANCE_M)
    }

    /// The slots of `sims` from their heartbeats.
    async fn slots(bus: &Bus, world_id: Uuid, sims: &[&BusId]) -> Vec<SimSlot> {
        let mut beats = bus
            .subscribe::<SimHeartbeat>(subject::all_sim_heartbeats(world_id))
            .await
            .expect("beats");
        let mut found: BTreeMap<BusId, u64> = BTreeMap::new();
        let heard = async {
            while found.len() < sims.len() {
                let beat = beats.next().await.expect("open").expect("decode").message;
                if sims.contains(&&beat.id) {
                    found.insert(beat.id, beat.started_at_ms);
                }
            }
        };
        tokio::time::timeout(WAIT, heard)
            .await
            .expect("beats in time");
        sims.iter()
            .map(|sim| SimSlot {
                id: (*sim).clone(),
                generation: found[*sim],
            })
            .collect()
    }

    /// The column stripes of `slots`, published every heartbeat like the director does.
    fn publish_tables(bus: &Bus, world_id: Uuid, slots: Vec<SimSlot>) -> JoinHandle<()> {
        let grid = CellGrid::new(LEVEL_M, LEVEL_M, CELL_M).expect("grid");
        let indexes: Vec<u16> = (0..).take(slots.len()).collect();
        let table = CellTable {
            version: 1,
            cell_size_m: CELL_M,
            columns: grid.columns(),
            rows: grid.rows(),
            owners: CellOwners::columns(grid, &indexes).cells().to_vec(),
            sims: slots,
        };
        let bus = bus.clone();
        tokio::spawn(async move {
            loop {
                let subject = subject::director_table(world_id);
                bus.publish(subject, &table).await.expect("table");
                tokio::time::sleep(HEARTBEAT).await;
            }
        })
    }

    /// Sends `admit` to `sim` until it answers.
    async fn admit(bus: &Bus, world_id: Uuid, sim: &BusId, admit: &Admit) -> AdmitReply {
        let subject = subject::sim_admit(world_id, sim);
        let mut reply: Result<AdmitReply, BusError> = Err(BusError::NoReplySubject);
        for _ in 0..TRIES {
            reply = bus.request(subject.clone(), admit, WAIT).await;
            if reply.is_ok() {
                break;
            }
            tokio::time::sleep(PAUSE).await;
        }
        reply.expect("admit reply")
    }

    fn admit_at(connection: ConnectionRef, character: (Uuid, Uuid), x: f32) -> Admit {
        Admit {
            connection,
            account: character.0,
            character: character.1,
            name: CHARACTER_NAME.to_owned(),
            entity: CONNECTION,
            position: at(x),
            login: true,
            handoffs: 0,
            bundles_sent: 0,
            look: None,
            backstory: None,
            items: None,
            inventory: PlayerInventory::default(),
            quests: PlayerQuests::default(),
            progression: PlayerProgression::default(),
            game_master: false,
            account_in_probation: false,
        }
    }

    /// Ends spawn protection: the first movement is how a real client marks itself in-game.
    async fn spawn_client(
        bus: &Bus,
        world_id: Uuid,
        sim: &BusId,
        connection: ConnectionRef,
        x: f32,
    ) {
        // The client's time sync comes after its own card, as retail's does: the sim takes a
        // time sync before the card as nothing.
        tokio::time::sleep(OWN_CARD_WAIT).await;
        input(bus, world_id, sim, &time_sync(connection.clone())).await;
        input(bus, world_id, sim, &move_to(connection, x)).await;
    }

    /// How long a test client waits for its own card before it says it is in the game: the
    /// card's delay and a few ticks.
    const OWN_CARD_WAIT: Duration =
        Duration::from_millis(crate::world::OWN_CARD_DELAY_MS + OWN_CARD_MARGIN_MS);
    /// The ticks of margin after the card's delay, ms.
    const OWN_CARD_MARGIN_MS: u64 = 200;

    /// The client's time sync request (4248): the message that tells the sim its client is in
    /// the game ([`says_in_game`]).
    fn time_sync(connection: ConnectionRef) -> SimInput {
        let mut writer = nw_messages::wire::Writer::new();
        nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 0).write(&mut writer);
        client_message(connection, TIME_SYNC_REQUEST_TYPE_INDEX, writer.finish())
    }

    /// A position-only input to `x`.
    fn move_to(connection: ConnectionRef, x: f32) -> SimInput {
        move_to_point(connection, [x, Y, Z])
    }

    /// A movement input to `[x, y, z]`.
    fn move_to_point(connection: ConnectionRef, [x, y, z]: [f32; 3]) -> SimInput {
        let target = InputPosition::from_metres(x, y, z).expect("finite");
        let body = ClientInputMsg::position(target).encode().expect("input");
        client_message(connection, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX, body)
    }

    /// An attack input at `x`, looking west: the test mobs stand west of the player, and the
    /// swing's shape points where the client looks.
    fn attack_at(connection: ConnectionRef, x: f32) -> SimInput {
        /// The yaw that looks west (`-x`).
        const LOOKING_WEST: [f32; 2] = [std::f32::consts::FRAC_PI_2, 0.0];
        let at = InputPosition::from_metres(x, Y, Z).expect("finite");
        let body = ClientInputMsg::attack(at, 1)
            .looking(LOOKING_WEST)
            .encode()
            .expect("input");
        client_message(connection, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX, body)
    }

    /// A client message of `type_index` with `body`, as the gateway forwards it.
    fn client_message(connection: ConnectionRef, type_index: u64, body: Vec<u8>) -> SimInput {
        let envelope = Envelope::message(TypeId::FileSlot(type_index), body)
            .encode()
            .expect("envelope");
        SimInput::ClientMessage {
            connection,
            envelope,
        }
    }

    /// One test mob at `x`.
    fn one_mob(x: f32) -> Arc<[VitalsSpawn]> {
        vec![VitalsSpawn {
            vitals_id: lowercase_crc32(TEST_MOB),
            level: TEST_LEVEL,
            position: [x, Y, Z],
            ..VitalsSpawn::default()
        }]
        .into()
    }

    /// The entity numbers and class type indexes of the members of every bundle in `reads`.
    fn member_classes(reads: &[SlotRead]) -> Vec<(u64, u64)> {
        reads
            .iter()
            .flat_map(|read| &read.updates)
            .flat_map(|update| {
                let entity = update.entity.expect("every slot is tied");
                update
                    .members
                    .iter()
                    .map(move |member| (entity, member.state.type_index()))
            })
            .collect()
    }

    /// The member classes of the next bundles of the test's connection that `wanted` picks.
    async fn bundle_classes(
        gateway: &mut TestGateway,
        mut wanted: impl FnMut(&[(u64, u64)]) -> bool,
    ) -> Vec<(u64, u64)> {
        gateway
            .until_bundles(|_, connection, reads| {
                if connection != CONNECTION {
                    return None;
                }
                let classes = member_classes(reads);
                wanted(&classes).then_some(classes)
            })
            .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sim_with_snapshots_publishes_them() {
        const SNAPSHOT: Duration = Duration::from_millis(100);
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut snapshots = bus
                .subscribe::<SimSnapshot>(subject::all_sim_snapshots(world_id))
                .await
                .expect("subscribe");
            bus.flush().await.expect("flush");
            let sim = Running::start(
                &bus,
                LoopSettings {
                    snapshot_interval: Some(SNAPSHOT),
                    ..settings(world_id, "sim")
                },
                db,
            )
            .await;
            let first = tokio::time::timeout(WAIT, snapshots.next())
                .await
                .expect("a snapshot in time")
                .expect("open")
                .expect("decode")
                .message;
            assert_eq!(&first.sim, sim.id());
            assert_eq!((first.part, first.parts), (0, 1));
            assert!(first.mob_counts.is_some());
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mob_enters_a_players_bundles_with_its_card() {
        const MOB_X: f32 = 1000.0;
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let mut combat = test_combat();
            combat.spawns = one_mob(MOB_X);
            let sim = Running::start(
                &bus,
                LoopSettings {
                    combat,
                    ..settings(world_id, "sim")
                },
                db,
            )
            .await;
            let tables = publish_tables(&bus, world_id, slots(&bus, world_id, &[sim.id()]).await);
            let player = admit_at(gateway.connection(CONNECTION), ada, MOB_X + 2.0);
            admit(&bus, world_id, sim.id(), &player).await;
            spawn_client(
                &bus,
                world_id,
                sim.id(),
                gateway.connection(CONNECTION),
                MOB_X + 2.0,
            )
            .await;
            let mob = mob_entity(0);
            let first = bundle_classes(&mut gateway, |classes| {
                classes.contains(&(mob, GDE_METADATA_TYPE_INDEX))
            })
            .await;
            assert!(first.contains(&(mob, ALC_TYPE_INDEX)), "{first:?}");
            // The card comes again once per second of bundles; between, the mob is its position.
            bundle_classes(&mut gateway, |classes| {
                classes.contains(&(mob, ALC_TYPE_INDEX))
                    && !classes.contains(&(mob, GDE_METADATA_TYPE_INDEX))
            })
            .await;
            tables.abort();
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_attack_input_hits_a_mob_and_the_gateway_gets_a_2071() {
        const MOB_X: f32 = 1000.0;
        /// Within the light sword swing's reach of the mob's middle: its capsule passes 1.06 m
        /// ahead, 0.3 m long each way and 0.3 m round, and the test mob is 0.5 m round
        /// (`player-attacks-25810808.json`).
        const SWORD_REACH_M: f32 = 1.2;
        const PLAYER_X: f32 = MOB_X + SWORD_REACH_M;
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let mut combat = test_combat();
            combat.spawns = one_mob(MOB_X);
            let sim = Running::start(
                &bus,
                LoopSettings {
                    combat,
                    ..settings(world_id, "sim")
                },
                db,
            )
            .await;
            let tables = publish_tables(&bus, world_id, slots(&bus, world_id, &[sim.id()]).await);
            let player = admit_at(gateway.connection(CONNECTION), ada, PLAYER_X);
            admit(&bus, world_id, sim.id(), &player).await;
            spawn_client(
                &bus,
                world_id,
                sim.id(),
                gateway.connection(CONNECTION),
                PLAYER_X,
            )
            .await;
            let mob = mob_entity(0);
            bundle_classes(&mut gateway, |classes| {
                classes.contains(&(mob, GDE_METADATA_TYPE_INDEX))
            })
            .await;
            let attack = attack_at(gateway.connection(CONNECTION), PLAYER_X);
            input(&bus, world_id, sim.id(), &attack).await;
            gateway
                .until(|output| match output {
                    GatewayOutput::ServerMessages {
                        connection: CONNECTION,
                        channel: MESSAGE_CHANNEL,
                        reliable: true,
                        envelopes,
                    } => has_type(&envelopes, DAMAGE_DEALT_TYPE_INDEX).then_some(()),
                    _ => None,
                })
                .await;
            tables.abort();
            sim.stop().await;
        })
        .await;
    }

    /// Sends `input` to `sim` as the gateway does, in a batch.
    async fn input(bus: &Bus, world_id: Uuid, sim: &BusId, input: &SimInput) {
        send_inputs(bus, world_id, sim, false, vec![input.clone()]).await;
    }

    async fn send_inputs(
        bus: &Bus,
        world_id: Uuid,
        sim: &BusId,
        forwarded: bool,
        inputs: Vec<SimInput>,
    ) {
        let batch = InputBatch { forwarded, inputs };
        bus.publish(subject::sim_input(world_id, sim), &batch)
            .await
            .expect("input");
    }

    async fn saved_position(db: &PgPool, character: Uuid) -> SavedPosition {
        for _ in 0..TRIES {
            if let Some(saved) = character_position(db, character).await.expect("load") {
                return saved;
            }
            tokio::time::sleep(PAUSE).await;
        }
        panic!("no saved position");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admit_gets_login_messages_bundles_and_a_saved_position() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let first = admit_at(gateway.connection(CONNECTION), ada, BORDER_X);
            assert_eq!(
                admit(&bus, world_id, sim.id(), &first).await,
                AdmitReply::Admitted
            );
            let login = gateway
                .until(|output| match output {
                    GatewayOutput::ServerMessages {
                        channel: MESSAGE_CHANNEL,
                        reliable: true,
                        envelopes,
                        ..
                    } => Some(envelopes.len()),
                    _ => None,
                })
                .await;
            /// 4970 follows the seven login messages.
            const LEFT_QUEUE_MESSAGE: usize = 1;
            assert_eq!(login, LOGIN_MESSAGE_TYPES.len() + LEFT_QUEUE_MESSAGE);
            assert_eq!(
                gateway.bundle_from(sim.id(), CONNECTION).await,
                vec![CONNECTION]
            );
            let again = Admit {
                connection: gateway.connection(CONNECTION + 1),
                ..first
            };
            assert!(matches!(
                admit(&bus, world_id, sim.id(), &again).await,
                AdmitReply::Refused { .. }
            ));
            let left = SimInput::Left {
                connection: gateway.connection(CONNECTION),
            };
            input(&bus, world_id, sim.id(), &left).await;
            let saved = saved_position(db, ada.1).await;
            assert_eq!(saved.level, LEVEL);
            assert!((saved.x - f64::from(BORDER_X)).abs() < SAVE_TOLERANCE_M);
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_input_moves_the_player() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let connection = gateway.connection(CONNECTION);
            let start = admit_at(connection.clone(), ada, BORDER_X);
            admit(&bus, world_id, sim.id(), &start).await;
            let target = BORDER_X + STEP_M;
            input(&bus, world_id, sim.id(), &move_to(connection, target)).await;
            gateway
                .until_bundles(|_, _, reads| own_x_is(&reads[0], target).then_some(()))
                .await;
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_last_saves_finish_before_serve_returns() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let first = admit_at(gateway.connection(CONNECTION), ada, BORDER_X);
            admit(&bus, world_id, sim.id(), &first).await;
            sim.stop().await;
            let saved = character_position(db, ada.1).await.expect("load");
            assert!(saved.is_some(), "the shutdown save is missing");
        })
        .await;
    }

    #[tokio::test]
    async fn a_leave_saves_the_items_quests_and_progression() {
        /// An achievement bit the player carries in.
        const ACHIEVEMENT_BIT: u32 = 15_317;
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            assert_eq!(character_items(db, ada.1).await.expect("load"), None);
            let mut first = admit_at(gateway.connection(CONNECTION), ada, BORDER_X);
            first.items = Some(Vec::new());
            first.quests = PlayerQuests {
                done: vec![DONE_QUEST.to_owned()],
                ..PlayerQuests::default()
            };
            // The retail newchar loot limit count.
            let limit = nw_bus::LootLimitCount {
                limit_id: "2hwarhammer_ftue_corruptedsailer_v2".to_owned(),
                count: 1,
                counted_ns: 1_793_091_600_894_621_000,
                ends_ns: 1_793_091_600_894_621_000,
            };
            first.progression = PlayerProgression {
                experience: 1_250,
                coins: 375,
                achievements: vec![ACHIEVEMENT_BIT],
                loot_limits: vec![limit.clone()],
                masteries: BTreeMap::from([("BowAbilityTable".to_owned(), 30)]),
                attributes: vec![(2, 15)],
                weapon_points: Vec::new(),
                travel: nw_bus::PlayerTravel::default(),
                ..PlayerProgression::default()
            };
            admit(&bus, world_id, sim.id(), &first).await;
            sim.stop().await;
            let progression = character_progression(db, ada.1)
                .await
                .expect("load")
                .expect("saved");
            assert_eq!((progression.experience, progression.coins), (1_250, 375));
            assert!(progression.achievements.contains(&ACHIEVEMENT_BIT));
            assert_eq!(
                progression.loot_limits,
                vec![nw_db::SavedLootLimit {
                    limit_id: limit.limit_id,
                    count: limit.count,
                    counted_ns: limit.counted_ns,
                    ends_ns: limit.ends_ns,
                }],
                "a limit this sim does not apply keeps its saved count"
            );
            assert_eq!(
                character_items(db, ada.1).await.expect("load"),
                Some(Vec::new()),
                "an empty bag is saved as empty, not as never saved"
            );
            assert_eq!(
                character_quests(db, ada.1).await.expect("load"),
                Some(SavedQuests {
                    active: Vec::new(),
                    done: vec![DONE_QUEST.to_owned()],
                })
            );
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_silent_gateway_loses_its_players_and_hears_close() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, false).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let first = admit_at(gateway.connection(CONNECTION), ada, BORDER_X);
            admit(&bus, world_id, sim.id(), &first).await;
            let closed = gateway
                .until(|output| match output {
                    GatewayOutput::Close { connection } => Some(connection),
                    _ => None,
                })
                .await;
            assert_eq!(closed, CONNECTION);
            saved_position(db, ada.1).await;
            sim.stop().await;
        })
        .await;
    }

    /// Two sims sharing the level in two stripes, and the test's gateway.
    struct Pair {
        bus: Bus,
        world_id: Uuid,
        gateway: TestGateway,
        west: Running,
        east: Running,
        tables: JoinHandle<()>,
    }

    impl Pair {
        async fn start(db: &PgPool) -> Self {
            Self::start_with(db, test_combat()).await
        }

        /// A pair whose east sim has the combat settings `east_combat`.
        async fn start_with(db: &PgPool, east_combat: CombatSettings) -> Self {
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let gateway = TestGateway::start(&bus, world_id, true).await;
            // Sorted ids give the stripes: "sim-a" is west, "sim-b" east.
            let west = Running::start(&bus, settings(world_id, "sim-a"), db).await;
            let east_settings = LoopSettings {
                combat: east_combat,
                ..settings(world_id, "sim-b")
            };
            let east = Running::start(&bus, east_settings, db).await;
            let slots = slots(&bus, world_id, &[west.id(), east.id()]).await;
            let tables = publish_tables(&bus, world_id, slots);
            Self {
                bus,
                world_id,
                gateway,
                west,
                east,
                tables,
            }
        }

        async fn stop(self) {
            self.tables.abort();
            self.west.stop().await;
            self.east.stop().await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_player_crosses_to_the_other_sim_and_its_inputs_follow() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let mut pair = Pair::start(db).await;
            let (bus, world_id) = (pair.bus.clone(), pair.world_id);
            let connection = pair.gateway.connection(CONNECTION);
            let first = admit_at(connection.clone(), ada, BORDER_X - CROSSING_M);
            admit(&bus, world_id, pair.west.id(), &first).await;
            pair.gateway.bundle_from(pair.west.id(), CONNECTION).await;
            let east = pair.east.id().clone();
            let crossing = move_to(connection.clone(), BORDER_X + CROSSING_M);
            input(&bus, world_id, pair.west.id(), &crossing).await;
            let route = pair
                .gateway
                .until(|output| match output {
                    GatewayOutput::Route { sim, sequence, .. } => Some((sim, sequence)),
                    _ => None,
                })
                .await;
            assert_eq!(route, (east.clone(), FIRST_HANDOFF));
            assert_eq!(
                pair.gateway.bundle_from(&east, CONNECTION).await,
                vec![CONNECTION]
            );
            assert!(character_position(db, ada.1).await.expect("load").is_none());
            // The gateway still sends to the west sim: it forwards the move, and the route again.
            let further = BORDER_X + CROSSING_M + LATE_STEP_M;
            let late_move = move_to(connection.clone(), further);
            input(&bus, world_id, pair.west.id(), &late_move).await;
            let again = pair
                .gateway
                .until(|output| match output {
                    GatewayOutput::Route { sim, sequence, .. } => Some((sim, sequence)),
                    _ => None,
                })
                .await;
            assert_eq!(again, (east.clone(), FIRST_HANDOFF));
            pair.gateway
                .until_bundles(|sim, _, reads| {
                    (*sim == east && own_x_is(&reads[0], further)).then_some(())
                })
                .await;
            let left = SimInput::Left { connection };
            input(&bus, world_id, pair.west.id(), &left).await;
            let saved = saved_position(db, ada.1).await;
            assert!((saved.x - f64::from(further)).abs() < SAVE_TOLERANCE_M);
            pair.stop().await;
        })
        .await;
    }

    /// A respawn request (1938) with an empty payload.
    fn respawn_request(connection: ConnectionRef) -> SimInput {
        let body = RequestRespawnMsg::default().encode().expect("request");
        client_message(connection, REQUEST_RESPAWN_TYPE_INDEX, body)
    }

    /// Whether `envelopes` hold a message of type `type_index`.
    fn has_type(envelopes: &[Vec<u8>], type_index: u64) -> bool {
        envelopes.iter().any(|bytes| {
            Envelope::decode(bytes).is_ok_and(|envelope| envelope.type_index() == Some(type_index))
        })
    }

    /// The test combat with a mob hit of 100 x the level damage: more than a player's health.
    fn lethal_combat() -> CombatSettings {
        const LETHAL_FACTOR: f64 = 100.0;
        let mut source = crate::combat::test_support::test_source();
        for row in &mut source.attacks {
            if row.damage_id == crate::mob_ai::test_support::TEST_MOB_ROW {
                row.coefficient = LETHAL_FACTOR;
            }
        }
        CombatSettings {
            data: Arc::new(
                nw_content::combat::CombatData::build(
                    source,
                    &nw_content::combat::CombatRows::default(),
                )
                .expect("rows")
                .with_player_attacks(crate::combat::test_support::test_player_attacks()),
            ),
            ..test_combat()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_killed_player_gets_a_4299_and_respawns_on_request() {
        const RESPAWN: Duration = Duration::from_millis(300);
        const MOB_X: f32 = 1000.0;
        const PLAYER_X: f32 = MOB_X + 2.0;
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let mut combat = lethal_combat();
            combat.min_respawn_ms = u64::try_from(RESPAWN.as_millis()).expect("ms");
            combat.spawns = one_mob(MOB_X);
            let sim = Running::start(
                &bus,
                LoopSettings {
                    combat,
                    ..settings(world_id, "sim")
                },
                db,
            )
            .await;
            let tables = publish_tables(&bus, world_id, slots(&bus, world_id, &[sim.id()]).await);
            let player = admit_at(gateway.connection(CONNECTION), ada, PLAYER_X);
            admit(&bus, world_id, sim.id(), &player).await;
            spawn_client(
                &bus,
                world_id,
                sim.id(),
                gateway.connection(CONNECTION),
                PLAYER_X,
            )
            .await;
            let saved = saved_position(db, ada.1).await;
            assert!(
                (saved.x - f64::from(PLAYER_X)).abs() < SAVE_TOLERANCE_M,
                "saved at once where it died"
            );
            gateway
                .until(|output| match output {
                    GatewayOutput::ServerMessages {
                        connection: CONNECTION,
                        channel: MESSAGE_CHANNEL,
                        envelopes,
                        ..
                    } => has_type(&envelopes, DEATH_RECAP_TYPE_INDEX).then_some(()),
                    _ => None,
                })
                .await;
            tokio::time::sleep(RESPAWN + PAUSE).await;
            let step = move_to(gateway.connection(CONNECTION), PLAYER_X + STEP_M);
            input(&bus, world_id, sim.id(), &step).await;
            gateway.drain().await;
            assert!(
                tokio::time::timeout(PAUSE, wait_for_x(&mut gateway, sim.id(), PLAYER_X + STEP_M))
                    .await
                    .is_err(),
                "dead until it asks"
            );
            // The respawn puts it at the retail respawn point nearest to where it died.
            let [x, y, z] = nearest_respawn_point([PLAYER_X, Y, Z]);
            let request = respawn_request(gateway.connection(CONNECTION));
            input(&bus, world_id, sim.id(), &request).await;
            let step = move_to_point(gateway.connection(CONNECTION), [x + STEP_M, y, z]);
            input(&bus, world_id, sim.id(), &step).await;
            tokio::time::timeout(WAIT, wait_for_x(&mut gateway, sim.id(), x + STEP_M))
                .await
                .expect("it moves again after its respawn");
            tables.abort();
            sim.stop().await;
        })
        .await;
    }

    /// The next answer of type `T` to the test's connection.
    async fn next_answer<T>(
        gateway: &mut TestGateway,
        mut wanted: impl FnMut(ServerMessage) -> Option<T>,
    ) -> T {
        tokio::time::timeout(
            WAIT,
            gateway.until(|output| match output {
                GatewayOutput::ServerMessages {
                    connection: CONNECTION,
                    envelopes,
                    ..
                } => envelopes.iter().find_map(|bytes| {
                    let envelope = Envelope::decode(bytes).ok()?;
                    wanted(ServerMessage::from_envelope(&envelope)?.ok()?)
                }),
                _ => None,
            }),
        )
        .await
        .expect("an answer in time")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn social_calls_get_the_players_and_the_titles() {
        const PLAYER_X: f32 = 1000.0;
        const REQUEST_ID: [u8; nw_messages::RAW_ID_LEN] = [5; nw_messages::RAW_ID_LEN];
        let header = nw_messages::FacetHeader {
            target: 1,
            entity: 2,
        };
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let stored = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let tables = publish_tables(&bus, world_id, slots(&bus, world_id, &[sim.id()]).await);
            let player = admit_at(gateway.connection(CONNECTION), ada, PLAYER_X);
            admit(&bus, world_id, sim.id(), &player).await;
            let text = |id: Uuid| id.hyphenated().to_string().into_bytes();
            let request = PlayerDataRequest {
                header,
                request: REQUEST_ID,
                characters: vec![text(stored.1), text(Uuid::new_v4()), text(ada.1)],
            };
            let call = client_message(
                gateway.connection(CONNECTION),
                nw_messages::player_data::REQUEST_GET_PLAYERS_TYPE_INDEX,
                request.encode().expect("encode"),
            );
            input(&bus, world_id, sim.id(), &call).await;
            let answer = next_answer(&mut gateway, |message| match message {
                ServerMessage::PlayerData(answer) => Some(answer),
                _ => None,
            })
            .await;
            assert_eq!((answer.header, answer.request), (header, REQUEST_ID));
            let found: Vec<Vec<u8>> = answer
                .players
                .iter()
                .map(|player| player.character.clone())
                .collect();
            assert_eq!(
                found,
                [text(stored.1), text(ada.1)],
                "the unknown id is left out"
            );
            assert!(
                answer
                    .players
                    .iter()
                    .all(|player| player.name == CHARACTER_NAME.as_bytes()
                        && player.world == text(world_id))
            );
            let call = client_message(
                gateway.connection(CONNECTION),
                nw_messages::titles::SEND_AVAILABLE_TITLES_TYPE_INDEX,
                TitleRequest::Available { header }.encode(),
            );
            input(&bus, world_id, sim.id(), &call).await;
            let titles = next_answer(&mut gateway, |message| match message {
                ServerMessage::AvailableTitles(titles) => Some(titles),
                _ => None,
            })
            .await;
            assert_eq!(titles.header, header);
            assert!(
                titles
                    .new
                    .iter()
                    .all(|title| titles.available.contains(title))
            );
            tables.abort();
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mob_of_the_other_sim_kills_a_player_through_the_bus() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let mut combat = lethal_combat();
            combat.spawns = one_mob(BORDER_X + 2.0);
            let pair = Pair::start_with(db, combat).await;
            let ada = character(db).await;
            let west_player = admit_at(pair.gateway.connection(CONNECTION), ada, BORDER_X - 1.0);
            admit(&pair.bus, pair.world_id, pair.west.id(), &west_player).await;
            spawn_client(
                &pair.bus,
                pair.world_id,
                pair.west.id(),
                pair.gateway.connection(CONNECTION),
                BORDER_X - 1.0,
            )
            .await;
            let saved = saved_position(db, ada.1).await;
            assert!(
                (saved.x - f64::from(BORDER_X - 1.0)).abs() < SAVE_TOLERANCE_M,
                "saved at once where it died"
            );
            pair.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ghosts_cross_the_border() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let mut pair = Pair::start(db).await;
            let (bus, world_id) = (pair.bus.clone(), pair.world_id);
            let west_player = admit_at(
                pair.gateway.connection(CONNECTION),
                character(db).await,
                BORDER_X - CROSSING_M,
            );
            let east_player = Admit {
                connection: pair.gateway.connection(CONNECTION + 1),
                entity: CONNECTION + 1,
                ..admit_at(
                    pair.gateway.connection(CONNECTION + 1),
                    character(db).await,
                    BORDER_X + CROSSING_M,
                )
            };
            admit(&bus, world_id, pair.west.id(), &west_player).await;
            spawn_client(
                &bus,
                world_id,
                pair.west.id(),
                pair.gateway.connection(CONNECTION),
                BORDER_X - CROSSING_M,
            )
            .await;
            admit(&bus, world_id, pair.east.id(), &east_player).await;
            spawn_client(
                &bus,
                world_id,
                pair.east.id(),
                pair.gateway.connection(CONNECTION + 1),
                BORDER_X + CROSSING_M,
            )
            .await;
            let east = pair.east.id().clone();
            let seen = async {
                loop {
                    let numbers = pair.gateway.bundle_from(&east, CONNECTION + 1).await;
                    if numbers == vec![CONNECTION + 1, CONNECTION] {
                        return;
                    }
                }
            };
            tokio::time::timeout(WAIT, seen)
                .await
                .expect("ghost in time");
            pair.stop().await;
        })
        .await;
    }

    /// A sim that takes the place of the east sim in the table: it records every handoff
    /// message and answers offers with `answer`, or not at all.
    async fn fake_east(
        bus: &Bus,
        world_id: Uuid,
        answer: Option<HandoffReply>,
    ) -> (SimSlot, Subscription<HandoffMessage>) {
        let fake = SimSlot {
            id: id("sim-b"),
            generation: FAKE_GENERATION,
        };
        let messages = bus
            .subscribe::<HandoffMessage>(subject::sim_handoff(world_id, &fake.id))
            .await
            .expect("handoffs");
        let mut offers = bus
            .subscribe::<HandoffMessage>(subject::sim_handoff(world_id, &fake.id))
            .await
            .expect("handoffs");
        bus.flush().await.expect("flush");
        if let Some(answer) = answer {
            let answerer = bus.clone();
            tokio::spawn(async move {
                while let Some(Ok(message)) = offers.next().await {
                    if matches!(message.message, HandoffMessage::Offer(_)) {
                        answerer.reply(&message, &answer).await.expect("reply");
                    }
                }
            });
        }
        (fake, messages)
    }

    async fn next_handoff(messages: &mut Subscription<HandoffMessage>) -> HandoffMessage {
        tokio::time::timeout(WAIT, messages.next())
            .await
            .expect("handoff message in time")
            .expect("open")
            .expect("decode")
            .message
    }

    /// One real west sim, a fake east sim in the table, and a player 10 m inside the east.
    async fn west_with_fake_east(
        db: &PgPool,
        answer: Option<HandoffReply>,
    ) -> (
        Running,
        Subscription<HandoffMessage>,
        TestGateway,
        JoinHandle<()>,
    ) {
        let bus = test_bus().await;
        let world_id = Uuid::new_v4();
        let gateway = TestGateway::start(&bus, world_id, true).await;
        let west = Running::start(&bus, settings(world_id, "sim-a"), db).await;
        let (fake, messages) = fake_east(&bus, world_id, answer).await;
        let west_slot = slots(&bus, world_id, &[west.id()]).await.remove(0);
        let tables = publish_tables(&bus, world_id, vec![west_slot, fake]);
        let player = admit_at(
            gateway.connection(CONNECTION),
            character(db).await,
            BORDER_X + CROSSING_M,
        );
        admit(&bus, world_id, west.id(), &player).await;
        (west, messages, gateway, tables)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_handoff_keeps_the_player() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let refusal = HandoffReply::Refused {
                reason: "full".to_owned(),
            };
            let (west, mut messages, mut gateway, tables) =
                west_with_fake_east(db, Some(refusal)).await;
            assert!(matches!(
                next_handoff(&mut messages).await,
                HandoffMessage::Offer(_)
            ));
            assert_eq!(
                gateway.bundle_from(west.id(), CONNECTION).await,
                vec![CONNECTION]
            );
            assert!(matches!(
                next_handoff(&mut messages).await,
                HandoffMessage::Offer(_)
            ));
            let route = tokio::time::timeout(
                QUIET,
                gateway.until(|output| matches!(output, GatewayOutput::Route { .. }).then_some(())),
            )
            .await;
            assert!(route.is_err(), "a refused handoff sends no route");
            tables.abort();
            west.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_late_handoff_is_cancelled() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let (west, mut messages, mut gateway, tables) = west_with_fake_east(db, None).await;
            let HandoffMessage::Offer(offer) = next_handoff(&mut messages).await else {
                panic!("expected an offer first");
            };
            assert_eq!(
                next_handoff(&mut messages).await,
                HandoffMessage::Cancel {
                    connection: offer.connection,
                    handoffs: offer.handoffs,
                }
            );
            assert_eq!(
                gateway.bundle_from(west.id(), CONNECTION).await,
                vec![CONNECTION]
            );
            tables.abort();
            west.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sim_dropped_from_the_table_saves_and_removes_its_players() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let ada = character(db).await;
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let first = admit_at(gateway.connection(CONNECTION), ada, BORDER_X);
            admit(&bus, world_id, sim.id(), &first).await;
            gateway.bundle_from(sim.id(), CONNECTION).await;
            let other = SimSlot {
                id: id("sim-other"),
                generation: FAKE_GENERATION,
            };
            let tables = publish_tables(&bus, world_id, vec![other]);
            saved_position(db, ada.1).await;
            let close = tokio::time::timeout(
                QUIET,
                gateway.until(|output| matches!(output, GatewayOutput::Close { .. }).then_some(())),
            )
            .await;
            assert!(close.is_err(), "the gateways resume the players; no close");
            tables.abort();
            sim.stop().await;
        })
        .await;
    }

    /// The own x of the next bundle of `sim` for the test's connection that has one.
    async fn own_x_from(gateway: &mut TestGateway, sim: &BusId) -> f32 {
        gateway
            .until_bundles(|from, connection, reads| {
                (from == sim && connection == CONNECTION)
                    .then(|| own_x(&reads[0]))
                    .flatten()
            })
            .await
    }

    /// Reads bundles of `sim` until the own x is `x`.
    async fn wait_for_x(gateway: &mut TestGateway, sim: &BusId, x: f32) {
        while (own_x_from(gateway, sim).await - x).abs() > MOVE_TOLERANCE_M {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forwarded_input_after_a_direct_one_is_dropped() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let sim = Running::start(&bus, settings(world_id, "sim"), db).await;
            let connection = gateway.connection(CONNECTION);
            let player = admit_at(connection.clone(), character(db).await, BORDER_X);
            admit(&bus, world_id, sim.id(), &player).await;
            let id = sim.id().clone();
            let step = |metres: f32| move_to(connection.clone(), BORDER_X + metres);
            send_inputs(&bus, world_id, &id, true, vec![step(1.0)]).await;
            wait_for_x(&mut gateway, &id, BORDER_X + 1.0).await;
            send_inputs(&bus, world_id, &id, false, vec![step(2.0)]).await;
            wait_for_x(&mut gateway, &id, BORDER_X + 2.0).await;
            send_inputs(&bus, world_id, &id, true, vec![step(3.0)]).await;
            assert!(
                tokio::time::timeout(QUIET, wait_for_x(&mut gateway, &id, BORDER_X + 3.0))
                    .await
                    .is_err(),
                "a direct input after a forwarded one is dropped"
            );
            sim.stop().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_after_a_take_removes_the_player() {
        with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let bus = test_bus().await;
            let world_id = Uuid::new_v4();
            let mut gateway = TestGateway::start(&bus, world_id, true).await;
            let taker = Running::start(&bus, settings(world_id, "sim"), db).await;
            let id = taker.id().clone();
            let offer = HandoffOffer {
                connection: gateway.connection(CONNECTION),
                character: character(db).await.1,
                name: CHARACTER_NAME.to_owned(),
                entity: CONNECTION,
                gde_ref: nw_messages::raw_id_of_entity(CONNECTION),
                position: at(BORDER_X),
                move_age_ms: 0,
                budget_m: 0.0,
                bundles_sent: 0,
                slots: Vec::new(),
                pending_stops: Vec::new(),
                time_synch_age_ms: 0,
                handoffs: FIRST_HANDOFF,
                health: TEST_PLAYER_HEALTH,
                look: None,
                backstory: None,
                phasing_sequence: 0,
                client_context: 1,
                enter_level: false,
                items: None,
                inventory: PlayerInventory::default(),
                drawn_weapon: None,
                active_weapon: None,
                mastery_versions: (0, 0),
                attribute_version: 0,
                fishing_sequence: 0,
                game_master: false,
                account_in_probation: false,
                quests: PlayerQuests::default(),
                progression: PlayerProgression::default(),
                achievement_version: 0,
            };
            let subject = subject::sim_handoff(world_id, &id);
            let mut reply: Result<HandoffReply, BusError> = Err(BusError::NoReplySubject);
            for _ in 0..TRIES {
                reply = bus
                    .request(
                        subject.clone(),
                        &HandoffMessage::Offer(Box::new(offer.clone())),
                        WAIT,
                    )
                    .await;
                if reply.is_ok() {
                    break;
                }
                tokio::time::sleep(PAUSE).await;
            }
            assert_eq!(reply.expect("offer reply"), HandoffReply::Accepted);
            own_x_from(&mut gateway, &id).await;
            let cancel = HandoffMessage::Cancel {
                connection: offer.connection.clone(),
                handoffs: FIRST_HANDOFF,
            };
            bus.publish(subject, &cancel).await.expect("cancel");
            tokio::time::sleep(QUIET).await;
            gateway.drain().await;
            let bundle = tokio::time::timeout(QUIET, own_x_from(&mut gateway, &id)).await;
            assert!(bundle.is_err(), "no bundle after the cancel");
            taker.stop().await;
        })
        .await;
    }
}
