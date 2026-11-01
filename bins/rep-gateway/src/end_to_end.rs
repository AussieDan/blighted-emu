// SPDX-License-Identifier: AGPL-3.0-or-later
//! The exit tests without the game (`infra` feature), on the real PostgreSQL, Valkey and NATS:
//!
//! - M3 (M3 spec section 12): the test client logs in over HTTP through the `edge-http` routes,
//!   waits in the queue, and registers with the gateway over UDP. The same token is refused the
//!   second time.
//! - M4 (M4 spec section 1.2, criteria 3.1 to 3.5) through M5 (M5 spec section 13):
//!   `world-director` and two `sim` processes serve the world over NATS. Two clients are in the
//!   world together and see each other; the first walks across the border of the two sims and
//!   back, which the second sees the whole way; its far move is refused, and a client that logs
//!   in again appears at its saved position. A client of a sim that stops goes on, connected, on
//!   the other sim.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nw_bus::test_support::test_bus;
use nw_bus::{BusId, CellTable, GatewayOutput, OutputBatch, SimHeartbeat, subject};
use nw_common::time::unix_ms;
use nw_db::test_db::with_test_schema;
use nw_db::{GLOBAL_MIGRATOR, PgPool, SavedPosition, WORLD_MIGRATOR, save_character_position};
use nw_login::config::load_edge_config;
use nw_login::install_crypto_provider;
use nw_login::queue::QueueStore;
use nw_login::test_support::{edge_app, test_valkey_url};
use nw_login::valkey::Valkey;
use nw_messages::WorldPosition;
use nw_messages::{BodyCodec, ClientInputMsg, InputPosition, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX};
use nw_reflect::{REGISTRATION_STATUS_AUTHORIZED, REGISTRATION_STATUS_REFUSED};
use nw_sim_core::test_support::NoGround;
use nw_sim_core::{CellGrid, Position};
use nw_testclient::pair::{
    INPUT_CHANNEL, PairSettings, SEND_INTERVAL, distance, run_pair, session, session_with,
};
use nw_testclient::route::Route;
use nw_testclient::udp::{Hold, run_rep};
use nw_testclient::walk::{WalkSettings, login_token, walk};
use sim::combat::test_support::test_data;
use sim::config::{SimConfig, load_sim_config};
use sim::server::{Databases, LoopSettings, serve as serve_sim};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use uuid::Uuid;
use world_director::server::{DirectorSettings, serve as serve_director};

use crate::server::tests::{
    HOLD, LIMITS, VERSION, WAIT, accepting, client, spawn_gateway, spawn_workers,
};
use crate::world::{BusWorld, WorldLinkSettings};

const EDGE_CONFIG: &str = "../../config/edge.toml";
const LOOPBACK: &str = "127.0.0.1:0";
const PASSWORD: &str = "password-ok";
const CHARACTER_NAME: &str = "Gateway";
const FIRST_CHARACTER_NAME: &str = "Pairone";
const SECOND_CHARACTER_NAME: &str = "Pairtwo";
/// Each run makes new accounts: a prefix and 12 random hex digits.
const ACCOUNT_PREFIX: &str = "e2e";
const ACCOUNT_RANDOM_DIGITS: usize = 12;
const LEVEL: &str = "newworld_vitaeeterna";
/// The sea height of that level (`terrain.json` `oceanLevel`).
const OCEAN_LEVEL_M: f32 = 40.0;
const SIM_CONFIG: &str = "../../config/sim.toml";
const LEVEL_M: f32 = 14_336.0;
const CELL_M: f32 = 2048.0;
/// The first column of the second of two sims.
const BORDER_X: f32 = 3.0 * CELL_M;
/// Where new characters appear: west of the border by more than the handoff margin.
const START: Position = Position {
    x: BORDER_X - 6.0,
    y: 7305.9,
    z: 170.0,
};
/// Below the gateway's join timeout.
const ADMIT_TIMEOUT: Duration = Duration::from_secs(1);
/// A saved position away from the start, inside the interest radius.
const SAVED_OFFSET_M: f32 = 25.0;
const SAVE_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT: Duration = Duration::from_millis(200);
const HEARTBEAT_MISSES: u32 = 3;
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const SIM_NAMES: [&str; 2] = ["sim-a", "sim-b"];
/// Gateway workers of the end-to-end stack (M5 load spec section 3).
const GATEWAY_WORKERS: usize = 2;
/// Resumes in flight of the end-to-end gateway.
const RESUMES_IN_FLIGHT: usize = 64;
/// Time in the world per session: the walk (about 5.6 s), the settle time and the far move
/// (0.5 s each), and many bundles at 30 Hz after them.
const PAIR_HOLD: Duration = Duration::from_secs(8);
/// Time in the world of the last check.
const SHORT_HOLD: Duration = Duration::from_secs(1);
/// The walk: 14 m east, past the border and the margin, and back, at a run below the limit.
const ROUTE_M: [[f32; 2]; 3] = [[0.0, 0.0], [14.0, 0.0], [0.0, 0.0]];
const WALK_SPEED_MPS: f32 = 5.0;
/// One handoff east, one back.
const HANDOFFS: usize = 2;
/// Time in the world of the client whose sim stops.
const STOP_HOLD: Duration = Duration::from_secs(5);
/// When its sim stops, after its registration.
const STOP_AFTER: Duration = Duration::from_millis(1500);
/// The client moves this long after its registration: after the silence window and the resume.
const RESUMED_AFTER: Duration = Duration::from_millis(3500);
/// The move after the resume, east of the start.
const RESUME_STEP_M: f32 = 1.0;
const JUMP_M: f32 = 50.0;
/// M4 spec section 1.2 criterion 3.3.
const PATH_TOLERANCE_M: f32 = 1.0;
/// M4 spec section 1.2 criterion 3.5.
const TOLERANCE_M: f32 = 0.1;
const NO_YAW: f32 = 0.0;
/// Free player slots of the test world: more than the tests use.
const FREE_SLOTS: u32 = 100;
/// The slot count outlives every test.
const FREE_SLOTS_TTL_MS: u64 = 600_000;

/// `edge-http` on a loopback port, for one world and one gateway.
struct Edge {
    http: reqwest::Client,
    login_url: String,
    game_url: String,
}

impl Edge {
    async fn start(
        global: &PgPool,
        world_pool: &PgPool,
        valkey: Valkey,
        world_id: Uuid,
        gateway: SocketAddr,
    ) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(EDGE_CONFIG);
        let mut config = load_edge_config(&path).expect("edge config");
        config.worlds[0].id = world_id;
        config.worlds[0].rep_address = gateway.to_string();
        let config = Arc::new(config);
        valkey
            .set_free_slots(world_id, FREE_SLOTS, FREE_SLOTS_TTL_MS)
            .await
            .expect("free slots");
        let app = edge_app(Arc::clone(&config), global, world_pool, valkey);
        let listener = TcpListener::bind(LOOPBACK).await.expect("bind HTTP");
        let http_address = listener.local_addr().expect("HTTP address");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let login_host = config.login_host();
        let game_host = config.game_host();
        let http = reqwest::Client::builder()
            .resolve(&login_host, http_address)
            .resolve(&game_host, http_address)
            .build()
            .expect("HTTP client");
        let port = http_address.port();
        Self {
            http,
            login_url: format!("http://{login_host}:{port}"),
            game_url: format!("http://{game_host}:{port}"),
        }
    }

    /// The settings of a new account with one character.
    fn new_account(&self, character_name: &str) -> WalkSettings {
        let random = Uuid::new_v4().simple().to_string();
        WalkSettings {
            login_url: self.login_url.clone(),
            game_url: self.game_url.clone(),
            account: format!("{ACCOUNT_PREFIX}{}", &random[..ACCOUNT_RANDOM_DIGITS]),
            password: PASSWORD.to_owned(),
            character_name: character_name.to_owned(),
            creation_params: nw_testclient::walk::CHARACTER_CREATION_PARAMS.to_owned(),
        }
    }
}

/// Runs `test` with migrated global and world schemas.
async fn with_schemas(test: impl AsyncFnOnce(&PgPool, &PgPool)) {
    install_crypto_provider();
    with_test_schema(async |global| {
        GLOBAL_MIGRATOR.run(global).await.expect("migrate global");
        with_test_schema(async |world| {
            WORLD_MIGRATOR.run(world).await.expect("migrate world");
            test(global, world).await;
        })
        .await;
    })
    .await;
}

async fn valkey() -> Valkey {
    Valkey::connect(&test_valkey_url())
        .await
        .expect("connect to Valkey")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_logs_in_and_registers_with_the_gateway_once() {
    with_schemas(async |global, world_pool| {
        // A new world id keeps this run's queue apart from other runs in the shared Valkey.
        let world_id = Uuid::new_v4();
        let valkey = valkey().await;
        let (world, events) = accepting();
        let gateway = spawn_gateway(valkey.clone(), world_id, LIMITS, world, events).await;
        let edge = Edge::start(global, world_pool, valkey, world_id, gateway).await;
        let admission = walk(&edge.http, &edge.new_account(CHARACTER_NAME), |_| {})
            .await
            .expect("walk");
        assert_eq!(admission.rep_address, gateway);

        let token = login_token(&admission.token);
        let outcome = run_rep(gateway, client(token.clone()), Hold::For(HOLD), WAIT)
            .await
            .expect("register");
        assert_eq!(outcome.status, Some(REGISTRATION_STATUS_AUTHORIZED));
        assert_eq!(outcome.server_version.as_deref(), Some(VERSION));
        assert!(outcome.pings > 0);
        let again = run_rep(gateway, client(token), Hold::For(HOLD), WAIT)
            .await
            .expect("register again");
        assert_eq!(again.status, Some(REGISTRATION_STATUS_REFUSED));
    })
    .await;
}

/// The committed `config/sim.toml`.
fn sim_config() -> SimConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SIM_CONFIG);
    load_sim_config(&path).expect("sim config")
}

/// The world of an end-to-end test: the director, two sims in two stripes (west "sim-a", east
/// "sim-b") and the gateway behind the edge routes.
struct Stack {
    edge: Edge,
    gateway: SocketAddr,
    sims: Vec<JoinHandle<()>>,
    routes: nw_bus::Subscription<OutputBatch>,
}

impl Stack {
    async fn start(global: &PgPool, world_pool: &PgPool) -> Self {
        let world_id = Uuid::new_v4();
        let unique = Uuid::new_v4().simple().to_string();
        let bus = test_bus().await;
        let mut tables = bus
            .subscribe::<CellTable>(subject::director_table(world_id))
            .await
            .expect("tables");
        let gateway_id = BusId::try_from(format!("gw-{unique}")).expect("id");
        let routes = bus
            .subscribe::<OutputBatch>(subject::gateway_output(world_id, &gateway_id))
            .await
            .expect("outputs");
        bus.flush().await.expect("flush");
        let director = DirectorSettings {
            world_id,
            level: LEVEL.to_owned(),
            starts: vec![START],
            grid: CellGrid::new(LEVEL_M, LEVEL_M, CELL_M).expect("grid"),
            admit_timeout: ADMIT_TIMEOUT,
            heartbeat_interval: HEARTBEAT,
            heartbeat_misses: HEARTBEAT_MISSES,
            instances: None,
        };
        tokio::spawn(serve_director(
            director,
            test_bus().await,
            world_pool.clone(),
            None,
            valkey().await,
            std::future::pending(),
        ));
        let mut sims = Vec::new();
        for name in SIM_NAMES {
            let sim_id = BusId::try_from(format!("{name}-{unique}")).expect("id");
            sims.push(start_sim(world_id, sim_id, global, world_pool).await);
        }
        let both = async {
            loop {
                let table = tables.next().await.expect("open").expect("decode").message;
                if table.sims.len() == SIM_NAMES.len() {
                    return;
                }
            }
        };
        tokio::time::timeout(WAIT, both)
            .await
            .expect("both sims in the table");
        let (world, events) = BusWorld::start(
            test_bus().await,
            WorldLinkSettings {
                world_id,
                gateway_id,
                join_timeout: JOIN_TIMEOUT,
                heartbeat_interval: HEARTBEAT,
                max_resumes_in_flight: RESUMES_IN_FLIGHT,
            },
            GATEWAY_WORKERS,
        )
        .await
        .expect("world link");
        let valkey = valkey().await;
        let gateway = spawn_workers(valkey.clone(), world_id, LIMITS, world, events).await;
        let edge = Edge::start(global, world_pool, valkey, world_id, gateway).await;
        Self {
            edge,
            gateway,
            sims,
            routes,
        }
    }

    /// Whether a sim whose id starts with `name` sent server messages to the gateway.
    async fn sends_from(&mut self, name: &str) -> bool {
        let mut sent = false;
        while let Ok(Some(Ok(batch))) = tokio::time::timeout(SHORT_HOLD, self.routes.next()).await {
            let messages = batch
                .message
                .outputs
                .iter()
                .any(|output| matches!(output, GatewayOutput::ServerMessages { .. }));
            sent |= messages && batch.message.sim.as_str().starts_with(name);
        }
        sent
    }

    /// The route messages that reached the gateway so far.
    async fn handoffs(&mut self) -> usize {
        let mut count = 0;
        while let Ok(Some(Ok(batch))) = tokio::time::timeout(SHORT_HOLD, self.routes.next()).await {
            count += batch
                .message
                .outputs
                .iter()
                .filter(|output| matches!(output, GatewayOutput::Route { .. }))
                .count();
        }
        count
    }
}

/// Starts a sim for `world_id` on the global and world databases and waits for its first
/// heartbeat; the handle stops it as a crash would, with no last save.
async fn start_sim(
    world_id: Uuid,
    sim_id: BusId,
    global: &PgPool,
    world_pool: &PgPool,
) -> JoinHandle<()> {
    let bus = test_bus().await;
    let mut beats = bus
        .subscribe::<SimHeartbeat>(subject::sim_heartbeat(world_id, &sim_id))
        .await
        .expect("subscribe");
    bus.flush().await.expect("flush");
    let config = sim_config();
    let bounds = CellGrid::new(LEVEL_M, LEVEL_M, CELL_M)
        .expect("grid")
        .bounds();
    let settings = LoopSettings {
        sim_id,
        world_id,
        level: LEVEL.to_owned(),
        world: config
            .world_settings(
                bounds,
                OCEAN_LEVEL_M,
                &nw_content::instances::InstanceCatalog::default(),
            )
            .expect("world settings"),
        handoff_timeout: Duration::from_millis(config.handoff_timeout_ms),
        forward_ttl_ms: config.forward_ttl_ms,
        save_interval: SAVE_INTERVAL,
        heartbeat_interval: HEARTBEAT,
        heartbeat_misses: HEARTBEAT_MISSES,
        combat: config.combat_settings(
            Arc::new(test_data()),
            Arc::from([]),
            Arc::new(sim::mob_ai::MobAi::empty()),
        ),
        content: Arc::default(),
        snapshot_interval: None,
        mob_ai: Arc::from([]),
        design: Arc::default(),
        leaderboard_calendar: config.leaderboard_calendar().expect("calendar"),
    };
    let databases = Databases {
        world: world_pool.clone(),
        global: global.clone(),
    };
    let sim = tokio::spawn(async move {
        let served = serve_sim(
            settings,
            Arc::new(NoGround),
            bus,
            databases,
            std::future::pending(),
        );
        // A serve error ends the sim like a crash; the tests see it as a silent sim.
        let _ = served.await;
    });
    tokio::time::timeout(WAIT, beats.next())
        .await
        .expect("sim heartbeat in time")
        .expect("open")
        .expect("decode");
    sim
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_see_each_other_across_the_border_and_a_saved_position_returns() {
    with_schemas(async |global, world_pool| {
        let mut stack = Stack::start(global, world_pool).await;
        let second = stack.edge.new_account(SECOND_CHARACTER_NAME);
        let settings = PairSettings {
            first: stack.edge.new_account(FIRST_CHARACTER_NAME),
            second: second.clone(),
            route: Route::new(ROUTE_M.to_vec(), WALK_SPEED_MPS).expect("route"),
            jump_m: JUMP_M,
            hold: PAIR_HOLD,
            registration_timeout: WAIT,
        };
        let report = run_pair(
            &stack.edge.http,
            &settings,
            |token| Ok(client(token)),
            |_| {},
        )
        .await
        .expect("pair");
        assert!(
            report.passed(PATH_TOLERANCE_M, TOLERANCE_M),
            "{:?}",
            report.checks(PATH_TOLERANCE_M, TOLERANCE_M)
        );
        assert!(stack.handoffs().await >= HANDOFFS, "east and back");

        // The second client left before the first one's second session (one hold ago), so its
        // last save is done. A position written now is what its next login must show.
        let admission = walk(&stack.edge.http, &second, |_| {}).await.expect("walk");
        let token = login_token(&admission.token);
        let character = String::from_utf8(token.character_id.clone()).expect("text");
        let character = Uuid::parse_str(&character).expect("character id");
        let saved = SavedPosition {
            level: LEVEL.to_owned(),
            x: f64::from(START.x + SAVED_OFFSET_M),
            y: f64::from(START.y),
            z: f64::from(START.z),
            yaw: NO_YAW,
            saved_at_ms: unix_ms(),
        };
        save_character_position(world_pool, character, &saved)
            .await
            .expect("save");
        let back = session(stack.gateway, client(token), SHORT_HOLD, WAIT)
            .await
            .expect("session");
        let expected =
            WorldPosition::from_metres(START.x + SAVED_OFFSET_M, START.y, START.z).expect("finite");
        let own = back.view.own_position().expect("own position");
        assert!(distance(own, expected) <= TOLERANCE_M);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_player_of_a_stopped_sim_continues_on_the_other() {
    with_schemas(async |global, world_pool| {
        let mut stack = Stack::start(global, world_pool).await;
        let account = stack.edge.new_account(FIRST_CHARACTER_NAME);
        let admission = walk(&stack.edge.http, &account, |_| {})
            .await
            .expect("walk");
        let token = login_token(&admission.token);
        // The start is in the west, so "sim-a" has the player.
        let west = &stack.sims[0];
        let mut stopped = false;
        let mut last_sent: Option<Duration> = None;
        let moved =
            InputPosition::from_metres(START.x + RESUME_STEP_M, START.y, START.z).expect("finite");
        let ran = session_with(
            stack.gateway,
            client(token),
            STOP_HOLD,
            WAIT,
            |client, _, since| {
                if since >= STOP_AFTER && !stopped {
                    west.abort();
                    stopped = true;
                }
                let due = last_sent.is_none_or(|sent| since.saturating_sub(sent) >= SEND_INTERVAL);
                if since >= RESUMED_AFTER && due {
                    last_sent = Some(since);
                    let body = ClientInputMsg::position(moved).encode()?;
                    client.send_message(
                        INPUT_CHANNEL,
                        false,
                        RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX,
                        body,
                    )?;
                }
                Ok(())
            },
        )
        .await
        .expect("session");
        assert_eq!(ran.outcome.status, Some(REGISTRATION_STATUS_AUTHORIZED));
        assert!(!ran.outcome.closed_by_server, "the player stays connected");
        let own = ran.view.own_position().expect("own position");
        assert!(
            (own.x - (START.x + RESUME_STEP_M)).abs() <= TOLERANCE_M,
            "the same entity moves after the resume"
        );
        assert!(
            stack.sends_from(SIM_NAMES[1]).await,
            "bundles come from the other sim"
        );
    })
    .await;
}
