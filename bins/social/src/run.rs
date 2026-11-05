// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: loads the world's social state, then runs the loop until Ctrl-C. One task owns the
//! state; database writes run in order on one store task, coin moves on their own tasks, so a
//! slow database or sim does not stop the loop.
//!
//! Every change the state makes is stored. A write that fails because the database cannot be
//! reached or is busy runs again, with a longer pause each time, until it is stored; the writes
//! after it wait, so they keep their order. When [`STORE_QUEUE`] writes wait, the loop waits for
//! the store task before it takes more inputs (back pressure): no change is dropped. A write the
//! database refuses for good is logged with the change; a refused company name (another company
//! has it in the database) makes the state take the company as the database holds it. At
//! Ctrl-C the loop stops and the store task writes what waits before the service ends.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use metrics_exporter_prometheus::PrometheusBuilder;
use nw_bus::{
    Bus, BusError, BusId, CoinReply, GroupAnswer, GroupQuery, Publisher, SimHeartbeat, SocialBatch,
    SocialDeliveries, heartbeat_silence, subject,
};
use nw_common::logging::{LoggingError, init_logging};
use nw_common::time::unix_ms;
use nw_db::{DbError, PgPool, WORLD_MIGRATOR, check_schema, load_social};
use nw_formats::pak::PakError;
use nw_social::{
    CompanyRules, Effect, Loaded, SocialRules, SocialWorld, StoreOp, TreasuryOutcome, VoiceService,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{SocialConfig, SocialConfigError, load_social_config};
use crate::lists::{profanity_filter, saved_icons};

/// Deliveries that may wait for NATS.
const OUTPUT_QUEUE: usize = 1024;
/// Stores that may wait for the database.
const STORE_QUEUE: usize = 4096;
/// The first pause before a failed store runs again, and the longest (Inferred: short enough
/// for a database restart, long enough not to load a database that is down).
const STORE_RETRY_FIRST: Duration = Duration::from_millis(100);
const STORE_RETRY_LONGEST: Duration = Duration::from_secs(30);
/// Time between flushes of the waiting deliveries: one sim tick.
const FLUSH: Duration = Duration::from_millis(33);
/// Tries of one coin move before it counts as unanswered (Inferred: with the configured coin
/// timeout, a sim that missed one answer gets four more, well inside the sim's memory of done
/// moves, `sim` `COIN_MOVE_MEMORY_MS`).
const COIN_MOVE_ATTEMPTS: u32 = 5;
/// Longest wait for a connection of the global database (the privacy settings).
const PRIVACY_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
/// Milliseconds per second, for the configured company wait.
const MS_PER_SECOND: u64 = 1000;
/// Metric: stores the database refused for good.
const METRIC_FAILED_STORES: &str = "social_failed_stores_total";
/// Metric: stores that failed for a lost or busy database and run again.
const METRIC_STORE_RETRIES: &str = "social_store_retries_total";

/// Failure while starting or running `social`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration file is missing or not valid.
    #[error(transparent)]
    Config(#[from] SocialConfigError),
    /// A secret variable is missing or empty.
    #[error("environment variable {0} is not set")]
    MissingSecret(String),
    /// Logging could not start.
    #[error(transparent)]
    Logging(#[from] LoggingError),
    /// The async runtime could not start.
    #[error("runtime failed")]
    Runtime(#[source] std::io::Error),
    /// The world database cannot be reached.
    #[error("database connection failed")]
    Database(#[source] sqlx::Error),
    /// The world database schema is not current or the state does not load.
    #[error(transparent)]
    Schema(#[from] DbError),
    /// The profanity lists cannot be read.
    #[error("profanity lists: {0}")]
    Lists(#[from] PakError),
    /// NATS cannot be reached.
    #[error(transparent)]
    Bus(#[from] BusError),
    /// The Prometheus listener could not start.
    #[error("metrics listener failed: {0}")]
    Metrics(String),
}

/// The answer of a coin move, a treasury change or a refused company, back to the loop.
enum Outcome {
    Coins(u64, Option<CoinReply>),
    Treasury(u64, TreasuryOutcome),
    /// The database refused a change of this company and holds this for it.
    Company(Uuid, Option<nw_db::Company>),
    /// Privacy settings that changed.
    Privacy(Vec<nw_db::AccountPrivacySetting>),
    /// The account of a character.
    Account(Uuid, Uuid),
}

/// One job of the store task.
enum StoreJob {
    Op(StoreOp),
    /// Read the account of a character.
    FindAccount(Uuid),
    Treasury {
        ticket: u64,
        company: Uuid,
        character: Uuid,
        change: i64,
        reason: nw_db::TreasuryReason,
    },
}

fn secret(name: &str) -> Result<String, RunError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RunError::MissingSecret(name.to_owned()))
}

/// Runs `social` until Ctrl-C.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, a secret, the database, the paks, NATS or the
/// metrics listener fails.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_social_config(config_path)?;
    let password = secret(&config.world_database_password_env)?;
    let voice = config
        .voice
        .as_ref()
        .map(|voice| {
            Ok::<_, RunError>(VoiceService {
                api_url: voice.api_url.clone(),
                issuer: voice.issuer.clone(),
                domain: voice.domain.clone(),
                key: secret(&voice.key_env)?.into_bytes(),
                channel_suffix: voice.channel_suffix.clone(),
                cell_size_m: voice.cell_size_m,
            })
        })
        .transpose()?;
    init_logging(&config.log_filter)?;
    let filter = profanity_filter(&config.game_dir)?;
    tracing::info!(words = filter.len(), "profanity lists read");
    let rules = SocialRules {
        world_id: config.world_id,
        voice,
        companies: CompanyRules {
            join_cooldown_ms: config
                .companies
                .join_cooldown_s
                .saturating_mul(MS_PER_SECOND),
            join_required_level: config.companies.join_required_level,
            treasury_max: config.companies.treasury_max,
        },
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Runtime)?
        .block_on(serve_async(config, password, rules, filter))
}

async fn serve_async(
    config: SocialConfig,
    password: String,
    rules: SocialRules,
    filter: nw_social::ProfanityFilter,
) -> Result<(), RunError> {
    let db = nw_db::connect(&config.world_database, &password)
        .await
        .map_err(RunError::Database)?;
    check_schema(&db, &WORLD_MIGRATOR).await?;
    let records = load_social(&db).await?;
    let icons = saved_icons(&records.characters);
    tracing::info!(
        characters = records.characters.len(),
        companies = records.companies.len(),
        groups = records.groups.len(),
        "social state loaded"
    );
    let world = SocialWorld::new(rules, filter, Loaded { records, icons });
    let bus = Bus::connect(&config.nats_url).await?;
    PrometheusBuilder::new()
        .with_http_listener(config.metrics_listen)
        .install()
        .map_err(|error| RunError::Metrics(error.to_string()))?;
    let shutdown = async {
        // A failed signal handler means the service runs until killed.
        let _ = nw_common::shutdown::stop_signal().await;
    };
    tracing::info!(world = %config.world_id, "serving");
    let privacy = privacy_database(&config)?;
    serve_loop(&config, world, bus, db, privacy, shutdown).await
}

/// The global database of the privacy settings, when configured. Lazy: a global database that is
/// down at start leaves the defaults until it is back.
fn privacy_database(config: &SocialConfig) -> Result<Option<PgPool>, RunError> {
    let Some(global) = &config.global_database else {
        tracing::info!("no global database: every privacy setting keeps its default");
        return Ok(None);
    };
    let password = secret(&global.password_env)?;
    nw_db::connect_lazy(&global.database, &password, PRIVACY_ACQUIRE_TIMEOUT)
        .map(Some)
        .map_err(RunError::Database)
}

/// Reads the privacy settings that changed since the last read, every `interval`, and sends them
/// to the loop. The first read gets every stored setting.
async fn privacy_task(
    global: PgPool,
    interval: Duration,
    outcomes: mpsc::UnboundedSender<Outcome>,
) {
    let mut since_ms = 0;
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        match nw_db::privacy_changes_since(&global, since_ms).await {
            Ok(changes) if changes.is_empty() => {}
            Ok(changes) => {
                since_ms = changes
                    .iter()
                    .map(|change| change.setting.updated_at_ms)
                    .max()
                    .unwrap_or(since_ms);
                if outcomes.send(Outcome::Privacy(changes)).is_err() {
                    return;
                }
            }
            Err(error) => tracing::warn!(%error, "privacy settings not read; next try soon"),
        }
    }
}

struct Loop {
    world_id: Uuid,
    world: SocialWorld,
    bus: Bus,
    outputs: Publisher<SocialDeliveries>,
    pending: BTreeMap<BusId, SocialDeliveries>,
    stores: mpsc::Sender<StoreJob>,
    outcomes: mpsc::UnboundedSender<Outcome>,
    coin_timeout: Duration,
    sims: HashMap<BusId, Instant>,
}

async fn serve_loop(
    config: &SocialConfig,
    world: SocialWorld,
    bus: Bus,
    db: PgPool,
    privacy: Option<PgPool>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), RunError> {
    let mut inputs = bus
        .subscribe::<SocialBatch>(subject::social_input(config.world_id))
        .await?;
    let mut beats = bus
        .subscribe::<SimHeartbeat>(subject::all_sim_heartbeats(config.world_id))
        .await?;
    let mut group_queries = bus
        .subscribe::<GroupQuery>(subject::social_group(config.world_id))
        .await?;
    bus.flush().await?;
    let (stores, store_jobs) = mpsc::channel(STORE_QUEUE);
    // Unbounded so that the store task never waits for the loop while the loop waits for it: an
    // outcome comes from one store job or one coin move, which are bounded by STORE_QUEUE and by
    // the transfers the state keeps.
    let (outcomes, mut results) = mpsc::unbounded_channel();
    let store_task = tokio::spawn(store_task(db, store_jobs, outcomes.clone()));
    let privacy_reader = privacy.map(|global| {
        let interval = Duration::from_millis(config.privacy_poll_ms);
        tokio::spawn(privacy_task(global, interval, outcomes.clone()))
    });
    let interval = Duration::from_millis(config.heartbeat_interval_ms);
    let silence = heartbeat_silence(interval, config.heartbeat_misses);
    let mut state = Loop {
        world_id: config.world_id,
        world,
        outputs: Publisher::start(bus.clone(), OUTPUT_QUEUE),
        bus,
        pending: BTreeMap::new(),
        stores,
        outcomes,
        coin_timeout: Duration::from_millis(config.coin_timeout_ms),
        sims: HashMap::new(),
    };
    let mut flush = tokio::time::interval(FLUSH);
    let mut check = tokio::time::interval(interval);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            Some(batch) = inputs.next() => match batch {
                Ok(batch) => {
                    let SocialBatch { sim, inputs } = batch.message;
                    state.sims.insert(sim.clone(), Instant::now());
                    for input in inputs {
                        let effects = state.world.input(&sim, input, unix_ms());
                        state.apply(effects).await;
                    }
                }
                Err(error) => tracing::warn!(%error, "bad social batch"),
            },
            Some(query) = group_queries.next() => match query {
                Ok(query) => {
                    let answer = group_answer(&state.world, &query.message);
                    let bus = state.bus.clone();
                    tokio::spawn(async move {
                        if let Err(error) = bus.reply(&query, &answer).await {
                            tracing::warn!(%error, "group query not answered");
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "bad group query"),
            },
            Some(beat) = beats.next() => match beat {
                Ok(beat) => {
                    let SimHeartbeat { id, instance, .. } = beat.message;
                    let phase = instance
                        .and_then(|instance| instance.running)
                        .map(|running| running.phase);
                    state.world.set_sim_phase(&id, phase);
                    state.sims.insert(id, Instant::now());
                }
                Err(error) => tracing::warn!(%error, "bad sim heartbeat"),
            },
            Some(outcome) = results.recv() => {
                let effects = match outcome {
                    Outcome::Coins(ticket, reply) => state.world.coins_moved(ticket, reply, unix_ms()),
                    Outcome::Treasury(ticket, outcome) => {
                        state.world.treasury_changed(ticket, outcome, unix_ms())
                    }
                    Outcome::Company(company, stored) => {
                        state.world.company_from_database(company, stored, unix_ms())
                    }
                    Outcome::Privacy(changes) => {
                        for change in changes {
                            let setting = &change.setting;
                            state
                                .world
                                .set_privacy(change.account, &setting.alias, &setting.value);
                        }
                        Vec::new()
                    }
                    Outcome::Account(character, account) => {
                        state.world.account_found(character, account);
                        Vec::new()
                    }
                };
                state.apply(effects).await;
            }
            _ = flush.tick() => state.flush(),
            _ = check.tick() => state.check_sims(silence).await,
        }
    }
    state.flush();
    if let Some(reader) = privacy_reader {
        reader.abort();
    }
    // The store task ends when the last sender is gone, after it wrote every waiting change.
    let Loop {
        stores, outcomes, ..
    } = state;
    drop((stores, outcomes));
    if let Err(error) = store_task.await {
        tracing::error!(%error, "store task failed; waiting changes may be lost");
    }
    Ok(())
}

/// The answer to the director's group query (group expeditions): the asking character's group,
/// leader and members, or [`GroupAnswer::Alone`].
fn group_answer(world: &SocialWorld, query: &GroupQuery) -> GroupAnswer {
    world.group_of(query.character)
}

impl Loop {
    async fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Deliver { sim, delivery } => {
                    self.pending
                        .entry(sim)
                        .or_default()
                        .deliveries
                        .push(delivery);
                }
                Effect::OwnState { sim, delivery } => {
                    self.pending
                        .entry(sim)
                        .or_default()
                        .own_states
                        .push(delivery);
                }
                Effect::GroupEntity { sim, delivery } => {
                    self.pending
                        .entry(sim)
                        .or_default()
                        .group_entities
                        .push(delivery);
                }
                Effect::Store(op) => self.queue_store(StoreJob::Op(op)).await,
                Effect::FindAccount { character } => {
                    self.queue_store(StoreJob::FindAccount(character)).await;
                }
                Effect::Treasury {
                    ticket,
                    company,
                    character,
                    change,
                    reason,
                } => {
                    self.queue_store(StoreJob::Treasury {
                        ticket,
                        company,
                        character,
                        change,
                        reason,
                    })
                    .await
                }
                Effect::MoveCoins {
                    ticket,
                    sim,
                    transfer,
                } => {
                    let bus = self.bus.clone();
                    let outcomes = self.outcomes.clone();
                    let subject = subject::sim_coins(self.world_id, &sim);
                    let timeout = self.coin_timeout;
                    tokio::spawn(async move {
                        let reply = coin_move(&bus, subject, &transfer, timeout).await;
                        // A closed loop is shutting down.
                        let _ = outcomes.send(Outcome::Coins(ticket, reply));
                    });
                }
            }
        }
    }

    /// Queues `job` for the store task; waits while the queue is full (back pressure).
    async fn queue_store(&self, job: StoreJob) {
        if self.stores.send(job).await.is_err() {
            metrics::counter!(METRIC_FAILED_STORES).increment(1);
            tracing::error!("store task ended; a social change is not stored");
        }
    }

    fn flush(&mut self) {
        for (sim, deliveries) in std::mem::take(&mut self.pending) {
            let subject = subject::sim_social(self.world_id, &sim);
            if !self.outputs.send(subject, deliveries) {
                tracing::warn!(%sim, "social deliveries dropped: NATS is behind");
            }
        }
    }

    async fn check_sims(&mut self, silence: Duration) {
        let now = Instant::now();
        let lost: Vec<BusId> = self
            .sims
            .iter()
            .filter(|(_, seen)| now.duration_since(**seen) > silence)
            .map(|(sim, _)| sim.clone())
            .collect();
        for sim in lost {
            self.sims.remove(&sim);
            tracing::warn!(%sim, "sim silent; its players are offline");
            let effects = self.world.sim_lost(&sim, unix_ms());
            self.apply(effects).await;
        }
    }
}

/// Asks the sim at `subject` to move the coins of `transfer`, up to [`COIN_MOVE_ATTEMPTS`]
/// times with the same move id (the sim moves them once); `None` when no attempt was answered.
async fn coin_move(
    bus: &Bus,
    subject: String,
    transfer: &nw_bus::CoinTransfer,
    timeout: Duration,
) -> Option<CoinReply> {
    for attempt in 1..=COIN_MOVE_ATTEMPTS {
        match bus
            .request::<_, CoinReply>(subject.clone(), transfer, timeout)
            .await
        {
            Ok(reply) => return Some(reply),
            Err(error) => {
                tracing::warn!(%error, id = %transfer.id, attempt, "coin move got no answer");
            }
        }
    }
    None
}

/// Runs `write` until it is stored or the database refuses it for good: a lost or busy database
/// makes it run again after a pause that doubles up to [`STORE_RETRY_LONGEST`].
async fn until_stored<T, F, Fut>(what: &str, mut write: F) -> Result<T, DbError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DbError>>,
{
    let mut pause = STORE_RETRY_FIRST;
    loop {
        match write().await {
            Err(error) if error.is_transient() => {
                metrics::counter!(METRIC_STORE_RETRIES).increment(1);
                tracing::warn!(%error, what, ?pause, "social store failed; it runs again");
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(STORE_RETRY_LONGEST);
            }
            other => return other,
        }
    }
}

/// Runs the stores in order; a treasury change, a refused company and an account read send
/// their outcome back.
async fn store_task(
    db: PgPool,
    mut jobs: mpsc::Receiver<StoreJob>,
    outcomes: mpsc::UnboundedSender<Outcome>,
) {
    while let Some(job) = jobs.recv().await {
        let outcome = match job {
            StoreJob::Op(op) => match until_stored("change", || store(&db, &op)).await {
                Ok(Stored::Done) => None,
                Ok(Stored::NameTaken(company)) => {
                    tracing::error!(%company, "company name taken in the database");
                    let read = until_stored("company", || nw_db::company_by_id(&db, company)).await;
                    match read {
                        Ok(stored) => Some(Outcome::Company(company, stored)),
                        Err(error) => {
                            metrics::counter!(METRIC_FAILED_STORES).increment(1);
                            tracing::error!(%error, %company, "refused company not read");
                            None
                        }
                    }
                }
                Err(error) => {
                    metrics::counter!(METRIC_FAILED_STORES).increment(1);
                    tracing::error!(%error, ?op, "social change refused by the database");
                    None
                }
            },
            StoreJob::FindAccount(character) => {
                let read = until_stored("account", || nw_db::character_by_id(&db, character)).await;
                match read {
                    Ok(Some(found)) => Some(Outcome::Account(character, found.account_id)),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::error!(%error, %character, "character account not read");
                        None
                    }
                }
            }
            StoreJob::Treasury {
                ticket,
                company,
                character,
                change,
                reason,
            } => {
                let changed = until_stored("treasury", || {
                    nw_db::change_treasury(&db, company, character, change, reason)
                })
                .await;
                let outcome = match changed {
                    Ok(Some(balance)) => TreasuryOutcome::Stored(balance),
                    Ok(None) => TreasuryOutcome::Refused,
                    Err(error) => {
                        metrics::counter!(METRIC_FAILED_STORES).increment(1);
                        tracing::error!(%error, %company, "treasury change refused");
                        TreasuryOutcome::Failed
                    }
                };
                Some(Outcome::Treasury(ticket, outcome))
            }
        };
        if let Some(outcome) = outcome {
            // A closed loop is shutting down.
            let _ = outcomes.send(outcome);
        }
    }
}

/// What a stored change did.
enum Stored {
    Done,
    /// The database refused the company's name: another company has it there.
    NameTaken(Uuid),
}

async fn store(db: &PgPool, op: &StoreOp) -> Result<Stored, DbError> {
    let done = |result: Result<(), DbError>| result.map(|()| Stored::Done);
    match op {
        StoreOp::Settings {
            character,
            settings,
        } => done(nw_db::save_social_settings(db, *character, settings).await),
        StoreOp::AddLink {
            character,
            other,
            kind,
        } => done(nw_db::add_social_link(db, *character, *other, *kind).await),
        StoreOp::RemoveLink {
            character,
            other,
            kind,
        } => done(nw_db::remove_social_link(db, *character, *other, *kind).await),
        StoreOp::Report(report) => done(nw_db::insert_player_report(db, report).await),
        StoreOp::CreateCompany { company, founder } => {
            Ok(if nw_db::create_company(db, company, *founder).await? {
                Stored::Done
            } else {
                Stored::NameTaken(company.id)
            })
        }
        StoreOp::UpdateCompany(company) => Ok(if nw_db::update_company(db, company).await? {
            Stored::Done
        } else {
            Stored::NameTaken(company.id)
        }),
        StoreOp::DeleteCompany(company) => done(nw_db::delete_company(db, *company).await),
        StoreOp::SetMember(member) => done(nw_db::set_company_member(db, member).await),
        StoreOp::RemoveMember {
            character,
            left_at_ms,
        } => done(nw_db::remove_company_member(db, *character, *left_at_ms).await),
        StoreOp::AddInvite(invite) => done(nw_db::add_company_invite(db, invite).await),
        StoreOp::RemoveInvite { company, character } => {
            done(nw_db::remove_company_invite(db, *company, *character).await)
        }
        StoreOp::SaveGroup(group) => done(nw_db::save_group(db, group).await),
        StoreOp::DeleteGroup(group) => done(nw_db::delete_group(db, *group).await),
        StoreOp::AddCoinReturn(debt) => done(nw_db::add_coin_return(db, debt).await),
        StoreOp::RemoveCoinReturn(debt) => done(nw_db::remove_coin_return(db, *debt).await),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use nw_db::{SocialCharacter, SocialGroup, SocialRecords};
    use nw_social::ProfanityFilter;

    use super::*;

    /// A world whose stored group holds characters 1 (leader) and 2; character 3 is alone.
    fn world() -> (SocialWorld, Uuid) {
        let character = |number: u128| Uuid::from_u128(number);
        let group = Uuid::from_u128(500);
        let characters = (1..=3)
            .map(|number| SocialCharacter {
                id: character(number),
                account: Uuid::from_u128(number + 1000),
                name: format!("Player{number}"),
                appearance: serde_json::json!({}),
            })
            .collect();
        let world = SocialWorld::new(
            SocialRules {
                world_id: Uuid::from_u128(77),
                voice: None,
                companies: CompanyRules {
                    join_cooldown_ms: 0,
                    join_required_level: 0,
                    treasury_max: 0,
                },
            },
            ProfanityFilter::of_lists(std::iter::empty::<&[u8]>()),
            Loaded {
                records: SocialRecords {
                    characters,
                    groups: vec![SocialGroup {
                        id: group,
                        leader: character(1),
                        voice_channel: String::new(),
                        members: vec![character(1), character(2)],
                    }],
                    ..SocialRecords::default()
                },
                icons: HashMap::new(),
            },
        );
        (world, group)
    }

    #[tokio::test]
    async fn a_store_runs_again_after_a_lost_database_and_not_after_a_refusal() {
        let tries = std::cell::Cell::new(0_u32);
        let stored = until_stored("test", || {
            tries.set(tries.get() + 1);
            let count = tries.get();
            async move {
                if count < 3 {
                    Err(DbError::from(sqlx::Error::PoolTimedOut))
                } else {
                    Ok(count)
                }
            }
        })
        .await;
        assert_eq!(stored.expect("stored"), 3, "two lost tries, then stored");
        let refused_tries = std::cell::Cell::new(0_u32);
        let refused = until_stored("test", || {
            refused_tries.set(refused_tries.get() + 1);
            async { Err::<(), _>(DbError::OutOfRange { field: "amount" }) }
        })
        .await;
        assert!(refused.is_err());
        assert_eq!(refused_tries.get(), 1, "a refusal does not run again");
    }

    #[test]
    fn a_group_query_answers_the_stored_group_or_alone() {
        let (world, group) = world();
        let ask = |number: u128| {
            group_answer(
                &world,
                &GroupQuery {
                    character: Uuid::from_u128(number),
                },
            )
        };
        let expected = GroupAnswer::Group {
            group,
            leader: Uuid::from_u128(1),
            members: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
        };
        assert_eq!(ask(2), expected);
        assert_eq!(ask(1), expected);
        assert_eq!(ask(3), GroupAnswer::Alone);
    }
}
