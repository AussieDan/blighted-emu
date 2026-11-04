// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: loads the world's social state, then runs the loop until Ctrl-C. One task owns the
//! state; database writes run in order on one store task, coin moves on their own tasks, so a
//! slow database or sim never stops the loop.

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
use nw_social::{CompanyRules, Effect, Loaded, SocialRules, SocialWorld, StoreOp, VoiceService};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{SocialConfig, SocialConfigError, load_social_config};
use crate::lists::{profanity_filter, saved_icons};

/// Deliveries that may wait for NATS.
const OUTPUT_QUEUE: usize = 1024;
/// Stores that may wait for the database.
const STORE_QUEUE: usize = 4096;
/// Answers of coin moves and treasury changes that may wait for the loop.
const RESULT_QUEUE: usize = 1024;
/// Time between flushes of the waiting deliveries: one sim tick.
const FLUSH: Duration = Duration::from_millis(33);
/// Milliseconds per second, for the configured company wait.
const MS_PER_SECOND: u64 = 1000;
/// Metric: stores the database refused.
const METRIC_FAILED_STORES: &str = "social_failed_stores_total";

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

/// The answer of a coin move or treasury change, back to the loop.
enum Outcome {
    Coins(u64, Option<CoinReply>),
    Treasury(u64, Option<u64>),
}

/// One job of the store task.
enum StoreJob {
    Op(StoreOp),
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
    serve_loop(&config, world, bus, db, shutdown).await
}

struct Loop {
    world_id: Uuid,
    world: SocialWorld,
    bus: Bus,
    outputs: Publisher<SocialDeliveries>,
    pending: BTreeMap<BusId, SocialDeliveries>,
    stores: mpsc::Sender<StoreJob>,
    outcomes: mpsc::Sender<Outcome>,
    coin_timeout: Duration,
    sims: HashMap<BusId, Instant>,
}

async fn serve_loop(
    config: &SocialConfig,
    world: SocialWorld,
    bus: Bus,
    db: PgPool,
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
    let (outcomes, mut results) = mpsc::channel(RESULT_QUEUE);
    tokio::spawn(store_task(db, store_jobs, outcomes.clone()));
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
                        state.apply(effects);
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
                    state.sims.insert(beat.message.id, Instant::now());
                }
                Err(error) => tracing::warn!(%error, "bad sim heartbeat"),
            },
            Some(outcome) = results.recv() => {
                let effects = match outcome {
                    Outcome::Coins(ticket, reply) => state.world.coins_moved(ticket, reply, unix_ms()),
                    Outcome::Treasury(ticket, balance) => {
                        state.world.treasury_changed(ticket, balance, unix_ms())
                    }
                };
                state.apply(effects);
            }
            _ = flush.tick() => state.flush(),
            _ = check.tick() => state.check_sims(silence),
        }
    }
    state.flush();
    Ok(())
}

/// The answer to the director's group query (group expeditions): the asking character's group,
/// leader and members, or [`GroupAnswer::Alone`].
fn group_answer(world: &SocialWorld, query: &GroupQuery) -> GroupAnswer {
    world.group_of(query.character)
}

impl Loop {
    fn apply(&mut self, effects: Vec<Effect>) {
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
                Effect::Store(op) => self.queue_store(StoreJob::Op(op)),
                Effect::Treasury {
                    ticket,
                    company,
                    character,
                    change,
                    reason,
                } => self.queue_store(StoreJob::Treasury {
                    ticket,
                    company,
                    character,
                    change,
                    reason,
                }),
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
                        let reply = match bus
                            .request::<_, CoinReply>(subject, &transfer, timeout)
                            .await
                        {
                            Ok(reply) => Some(reply),
                            Err(error) => {
                                tracing::warn!(%error, "coin move got no answer");
                                None
                            }
                        };
                        // A closed loop is shutting down.
                        let _ = outcomes.send(Outcome::Coins(ticket, reply)).await;
                    });
                }
            }
        }
    }

    fn queue_store(&self, job: StoreJob) {
        if self.stores.try_send(job).is_err() {
            metrics::counter!(METRIC_FAILED_STORES).increment(1);
            tracing::error!("store queue full; a social change is not stored");
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

    fn check_sims(&mut self, silence: Duration) {
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
            self.apply(effects);
        }
    }
}

/// Runs the stores in order; a treasury change sends its outcome back.
async fn store_task(
    db: PgPool,
    mut jobs: mpsc::Receiver<StoreJob>,
    outcomes: mpsc::Sender<Outcome>,
) {
    while let Some(job) = jobs.recv().await {
        match job {
            StoreJob::Op(op) => {
                if let Err(error) = store(&db, &op).await {
                    metrics::counter!(METRIC_FAILED_STORES).increment(1);
                    tracing::error!(%error, ?op, "social change not stored");
                }
            }
            StoreJob::Treasury {
                ticket,
                company,
                character,
                change,
                reason,
            } => {
                let balance =
                    match nw_db::change_treasury(&db, company, character, change, reason).await {
                        Ok(balance) => balance,
                        Err(error) => {
                            metrics::counter!(METRIC_FAILED_STORES).increment(1);
                            tracing::error!(%error, %company, "treasury change failed");
                            None
                        }
                    };
                // A closed loop is shutting down.
                let _ = outcomes.send(Outcome::Treasury(ticket, balance)).await;
            }
        }
    }
}

async fn store(db: &PgPool, op: &StoreOp) -> Result<(), DbError> {
    match op {
        StoreOp::Settings {
            character,
            settings,
        } => nw_db::save_social_settings(db, *character, settings).await,
        StoreOp::AddLink {
            character,
            other,
            kind,
        } => nw_db::add_social_link(db, *character, *other, *kind).await,
        StoreOp::RemoveLink {
            character,
            other,
            kind,
        } => nw_db::remove_social_link(db, *character, *other, *kind).await,
        StoreOp::Report(report) => nw_db::insert_player_report(db, report).await,
        StoreOp::CreateCompany { company, founder } => {
            if !nw_db::create_company(db, company, *founder).await? {
                tracing::error!(name = %company.name, "company name taken in the database");
            }
            Ok(())
        }
        StoreOp::UpdateCompany(company) => {
            if !nw_db::update_company(db, company).await? {
                tracing::error!(name = %company.name, "company name taken in the database");
            }
            Ok(())
        }
        StoreOp::DeleteCompany(company) => nw_db::delete_company(db, *company).await,
        StoreOp::SetMember(member) => nw_db::set_company_member(db, member).await,
        StoreOp::RemoveMember {
            character,
            left_at_ms,
        } => nw_db::remove_company_member(db, *character, *left_at_ms).await,
        StoreOp::AddInvite(invite) => nw_db::add_company_invite(db, invite).await,
        StoreOp::RemoveInvite { company, character } => {
            nw_db::remove_company_invite(db, *company, *character).await
        }
        StoreOp::SaveGroup(group) => nw_db::save_group(db, group).await,
        StoreOp::DeleteGroup(group) => nw_db::delete_group(db, *group).await,
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
