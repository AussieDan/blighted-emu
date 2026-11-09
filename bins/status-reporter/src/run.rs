// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: checks the config, the secrets and the leaderboard table, then sends the status
//! every `status_interval_s` and the leaderboards every `leaderboards_interval_s` until Ctrl-C or
//! SIGTERM. A report that fails is not sent again: the next one replaces it, and the website
//! refuses a repeated timestamp anyway. A database that is down stops only the leaderboard
//! reports.

use std::path::Path;
use std::time::Duration;

use nw_common::logging::{LoggingError, init_logging};
use nw_common::shutdown::stop_signal;
use nw_common::time::{unix_seconds, unix_seconds_of_date_time};
use nw_content::leaderboard_table::{
    LeaderboardTable, LeaderboardTableError, leaderboard_table_path,
};
use nw_db::connect_lazy;
use nw_login::install_crypto_provider;
use reqwest::{Client, StatusCode};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::boards::{BoardClock, BoardDatabases, read_snapshot};
use crate::config::{PartPages, ReporterConfig, ReporterConfigError, load_reporter_config};
use crate::signing::{
    ReportSecret, SIGNATURE_HEADER, SecretTooShort, TIMESTAMP_HEADER, WORLD_HEADER,
};
use crate::status::{scrape_parts, status_report};

/// The website's status route.
pub const STATUS_PATH: &str = "/api/ingest/status";
/// The website's leaderboard route.
pub const LEADERBOARDS_PATH: &str = "/api/ingest/leaderboards";
/// Longest wait for a database connection (Inferred: well inside one leaderboard interval).
const DATABASE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Most characters of a refused answer written to the log.
const MAX_LOGGED_ANSWER_CHARS: usize = 200;
const JSON_CONTENT_TYPE: &str = "application/json";

/// Failure while starting `status-reporter`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration file is missing or not valid.
    #[error(transparent)]
    Config(#[from] ReporterConfigError),
    /// A secret variable is missing or empty.
    #[error("environment variable {0} is not set")]
    MissingSecret(String),
    /// The report secret is too short for the website.
    #[error(transparent)]
    Secret(#[from] SecretTooShort),
    /// The pinned client build cannot be read.
    #[error("client build: {0}")]
    ClientBuild(#[source] nw_common::config::ConfigError),
    /// The leaderboard table cannot be read (`content-tool leaderboards` writes it).
    #[error(transparent)]
    LeaderboardTable(#[from] LeaderboardTableError),
    /// The client's gameplay values (the season end) cannot be read.
    #[error(transparent)]
    Gameplay(#[from] sim::gameplay::GameplayError),
    /// A database URL does not parse.
    #[error("database config")]
    Database(#[source] sqlx::Error),
    /// Logging could not start.
    #[error(transparent)]
    Logging(#[from] LoggingError),
    /// The HTTP client could not start.
    #[error("HTTP client")]
    Http(#[source] reqwest::Error),
    /// The async runtime or the stop signal failed.
    #[error("runtime failed")]
    Runtime(#[source] std::io::Error),
}

/// A report the website did not take.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The website did not answer.
    #[error("the website did not answer: {0}")]
    Http(#[from] reqwest::Error),
    /// The website refused the report.
    #[error("the website refused the report ({status}): {answer}")]
    Refused {
        /// Its answer status.
        status: StatusCode,
        /// The start of its answer.
        answer: String,
    },
}

/// Signs and sends reports of one world to the website.
#[derive(Debug, Clone)]
pub struct Sender {
    http: Client,
    site_url: String,
    world_id: String,
    secret: ReportSecret,
}

impl Sender {
    /// A sender to `site_url` (no trailing `/`) for world `world_id`.
    pub fn new(http: Client, site_url: &str, world_id: Uuid, secret: ReportSecret) -> Self {
        Self {
            http,
            site_url: site_url.to_owned(),
            world_id: world_id.to_string(),
            secret,
        }
    }

    /// Sends `body` to the route `path`, signed with the time now.
    ///
    /// # Errors
    ///
    /// Returns [`SendError`] when the website does not answer or does not take the report.
    pub async fn send(&self, path: &str, body: Vec<u8>) -> Result<(), SendError> {
        let timestamp = unix_seconds();
        let signature = self.secret.sign(timestamp, &body);
        let response = self
            .http
            .post(format!("{}{path}", self.site_url))
            .header(reqwest::header::CONTENT_TYPE, JSON_CONTENT_TYPE)
            .header(WORLD_HEADER, &self.world_id)
            .header(TIMESTAMP_HEADER, timestamp.to_string())
            .header(SIGNATURE_HEADER, signature)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let answer = response.text().await.unwrap_or_default();
        Err(SendError::Refused {
            status,
            answer: answer.chars().take(MAX_LOGGED_ANSWER_CHARS).collect(),
        })
    }
}

/// An HTTP client whose requests wait at most `timeout`. Installs the TLS crypto provider that
/// the client needs.
///
/// # Errors
///
/// Returns the `reqwest` error when the client cannot start.
pub fn http_client(timeout: Duration) -> Result<Client, reqwest::Error> {
    install_crypto_provider();
    Client::builder().timeout(timeout).build()
}

/// A timer that ticks now, then every `period`; a late tick does not make the next ones early.
fn timer(period: Duration) -> tokio::time::Interval {
    let mut timer = tokio::time::interval(period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    timer
}

/// Logs a send result; a failure after a success, or the first success, is logged once.
fn log_send(kind: &str, result: &Result<(), SendError>, last_ok: &mut Option<bool>) {
    let ok = result.is_ok();
    match result {
        Ok(()) if *last_ok != Some(true) => tracing::info!(kind, "report accepted"),
        Err(error) if *last_ok != Some(false) => tracing::warn!(kind, %error, "report not sent"),
        Err(error) => tracing::debug!(kind, %error, "report not sent"),
        Ok(()) => tracing::debug!(kind, "report accepted"),
    }
    *last_ok = Some(ok);
}

/// Sends the status every `period`, read from `pages`. Runs until the task is dropped.
pub async fn status_loop(sender: Sender, http: Client, pages: PartPages, period: Duration) {
    const KIND: &str = "status";
    let mut timer = timer(period);
    let mut last_ok = None;
    loop {
        timer.tick().await;
        let report = status_report(&scrape_parts(&http, &pages).await);
        let result = match serde_json::to_vec(&report) {
            Ok(body) => sender.send(STATUS_PATH, body).await,
            Err(error) => {
                tracing::error!(%error, "status report not encoded");
                continue;
            }
        };
        log_send(KIND, &result, &mut last_ok);
    }
}

/// What the leaderboard loop reads.
#[derive(Debug, Clone)]
pub struct BoardSource {
    /// The databases.
    pub databases: BoardDatabases,
    /// The boards.
    pub table: LeaderboardTable,
    /// The rotations and the season end.
    pub clock: BoardClock,
    /// The world.
    pub world_id: Uuid,
}

/// Sends the leaderboards every `period`, read from `source`. Runs until the task is dropped.
pub async fn leaderboards_loop(sender: Sender, source: BoardSource, period: Duration) {
    const KIND: &str = "leaderboards";
    let mut timer = timer(period);
    let mut last_ok = None;
    loop {
        timer.tick().await;
        let snapshot = match read_snapshot(
            &source.databases,
            &source.table,
            &source.clock,
            source.world_id,
            unix_seconds(),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "leaderboards not read; no report this time");
                continue;
            }
        };
        let rows = snapshot.rows.len();
        let result = match serde_json::to_vec(&snapshot) {
            Ok(body) => sender.send(LEADERBOARDS_PATH, body).await,
            Err(error) => {
                tracing::error!(%error, "leaderboard report not encoded");
                continue;
            }
        };
        tracing::debug!(boards = snapshot.boards.len(), rows, "leaderboard report");
        log_send(KIND, &result, &mut last_ok);
    }
}

fn secret(name: &str) -> Result<String, RunError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RunError::MissingSecret(name.to_owned()))
}

/// Everything `serve` needs, checked before the loops start.
struct Ready {
    sender: Sender,
    http: Client,
    pages: PartPages,
    source: BoardSource,
    status_period: Duration,
    leaderboards_period: Duration,
}

fn prepare(config: ReporterConfig) -> Result<Ready, RunError> {
    let report_secret = ReportSecret::new(secret(&config.secret_env)?)?;
    let global_password = secret(&config.global_database.password_env)?;
    let world_password = secret(&config.world_database_password_env)?;
    let build = nw_content::client_build::load_client_build(&config.client_build_config)
        .map_err(RunError::ClientBuild)?;
    let table = LeaderboardTable::load(&leaderboard_table_path(&config.content_dir, &build))?;
    let calendar = config
        .leaderboards
        .calendar()
        .map_err(ReporterConfigError::from)?;
    let gameplay = sim::gameplay::load_gameplay(&config.edge_config, config.world_id)?;
    let season_end = gameplay
        .seasons
        .leaderboard_season_end
        .as_deref()
        .and_then(|text| {
            let end = unix_seconds_of_date_time(text);
            if end.is_none() {
                tracing::warn!(%text, "the season end is no date-time; boards get no season reset");
            }
            end
        });
    let http = http_client(config.request_timeout()).map_err(RunError::Http)?;
    let databases = BoardDatabases {
        global: connect_lazy(
            &config.global_database.database,
            &global_password,
            DATABASE_ACQUIRE_TIMEOUT,
        )
        .map_err(RunError::Database)?,
        world: connect_lazy(
            &config.world_database,
            &world_password,
            DATABASE_ACQUIRE_TIMEOUT,
        )
        .map_err(RunError::Database)?,
    };
    Ok(Ready {
        sender: Sender::new(
            http.clone(),
            &config.site_url,
            config.world_id,
            report_secret,
        ),
        http,
        pages: config.parts,
        source: BoardSource {
            databases,
            table,
            clock: BoardClock {
                calendar,
                season_end,
            },
            world_id: config.world_id,
        },
        status_period: Duration::from_secs(config.status_interval_s),
        leaderboards_period: Duration::from_secs(config.leaderboards_interval_s),
    })
}

/// Loads the configuration at `config_path`, then sends reports until Ctrl-C or SIGTERM.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, a secret or the leaderboard table is missing or
/// not valid, or the runtime cannot start.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_reporter_config(config_path)?;
    init_logging(&config.log_filter)?;
    let site_url = config.site_url.clone();
    let world_id = config.world_id;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Runtime)?;
    runtime.block_on(async move {
        let ready = prepare(config)?;
        tracing::info!(
            %site_url,
            %world_id,
            boards = ready.source.table.boards.len(),
            "sending reports"
        );
        let status = tokio::spawn(status_loop(
            ready.sender.clone(),
            ready.http,
            ready.pages,
            ready.status_period,
        ));
        let leaderboards = tokio::spawn(leaderboards_loop(
            ready.sender,
            ready.source,
            ready.leaderboards_period,
        ));
        let stopped = stop_signal().await.map_err(RunError::Runtime);
        status.abort();
        leaderboards.abort();
        tracing::info!("stopped");
        stopped
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode as AxumStatus};

    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";
    const WORLD: Uuid = Uuid::from_u128(0x8d0f_4c2e_0000_4000_8000_0000_0000_0001);
    const TIMEOUT: Duration = Duration::from_secs(2);
    const BODY: &[u8] = br#"{"x":1}"#;
    /// The website's clock window.
    const MAX_CLOCK_SKEW_SECS: i64 = 60;

    type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

    async fn site(answer: AxumStatus) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let app = axum::Router::new()
            .route(
                STATUS_PATH,
                axum::routing::post(
                    move |State(seen): State<Seen>, headers: HeaderMap, body: Bytes| async move {
                        seen.lock().expect("lock").push((headers, body));
                        answer
                    },
                ),
            )
            .with_state(seen.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{address}"), seen)
    }

    fn sender(site_url: &str) -> Sender {
        let http = http_client(TIMEOUT).expect("client");
        let secret = ReportSecret::new(SECRET.to_owned()).expect("secret");
        Sender::new(http, site_url, WORLD, secret)
    }

    fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .expect("header")
    }

    #[tokio::test]
    async fn a_report_carries_the_world_the_time_and_a_valid_signature() {
        let (url, seen) = site(AxumStatus::NO_CONTENT).await;
        sender(&url)
            .send(STATUS_PATH, BODY.to_vec())
            .await
            .expect("accepted");
        let (headers, body) = seen.lock().expect("lock").pop().expect("one report");
        assert_eq!(body.as_ref(), BODY);
        assert_eq!(header(&headers, WORLD_HEADER), WORLD.to_string());
        let timestamp: i64 = header(&headers, TIMESTAMP_HEADER).parse().expect("seconds");
        assert!((unix_seconds() - timestamp).abs() <= MAX_CLOCK_SKEW_SECS);
        let secret = ReportSecret::new(SECRET.to_owned()).expect("secret");
        assert_eq!(
            header(&headers, SIGNATURE_HEADER),
            secret.sign(timestamp, BODY)
        );
        assert_eq!(header(&headers, "content-type"), JSON_CONTENT_TYPE);
    }

    #[tokio::test]
    async fn a_refused_report_is_an_error_with_the_status() {
        let (url, _) = site(AxumStatus::UNAUTHORIZED).await;
        let result = sender(&url).send(STATUS_PATH, BODY.to_vec()).await;
        assert!(matches!(
            result,
            Err(SendError::Refused { status, .. }) if status == StatusCode::UNAUTHORIZED
        ));
    }

    #[tokio::test]
    async fn the_status_loop_sends_a_report_at_once() {
        let (url, seen) = site(AxumStatus::NO_CONTENT).await;
        let http = http_client(TIMEOUT).expect("client");
        let pages = PartPages {
            login: vec![format!("{url}/missing")],
            gateway: vec![format!("{url}/missing")],
            director: vec![format!("{url}/missing")],
            sims: vec![format!("{url}/missing")],
            social: vec![format!("{url}/missing")],
        };
        let task = tokio::spawn(status_loop(
            sender(&url),
            http,
            pages,
            Duration::from_secs(3600),
        ));
        let body = tokio::time::timeout(TIMEOUT, async {
            loop {
                if let Some((_, body)) = seen.lock().expect("lock").first() {
                    return body.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a report");
        task.abort();
        let report: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(report["parts"]["gateway"], "down");
        assert_eq!(report["playersOnline"], 0);
    }
}
