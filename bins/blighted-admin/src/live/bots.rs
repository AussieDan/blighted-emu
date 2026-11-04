// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot batches (admin tools spec section 5.3): each batch is one `nw-testclient swarm` process
//! that runs until its standard input closes. The page sends only a size from [`BATCH_SIZES`]
//! and a batch number; the program, its config and its secrets come from `[live]`.
//!
//! Batches log in one after another: a new batch waits while another one still starts its bots,
//! because thousands of logins at once overflow the PC's UDP buffers and the bots already in
//! the world lose their link.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use nw_common::time::unix_ms;
use nw_testclient::fleet::BOTS_STARTED_MARK;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use crate::config::LiveConfig;
use crate::live::{SharedState, lock};
use crate::server::AppState;

/// The batch sizes the page can ask for.
pub const BATCH_SIZES: [u32; 4] = [1, 10, 100, 1_000];
/// Time a stopped batch has to exit before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(30);
/// List (GET) and start (POST `{ "size": n }`) batches.
pub const BOTS_ROUTE: &str = "/api/live/bots";
/// Stop one batch.
pub const STOP_ROUTE: &str = "/api/live/bots/{batch}/stop";
/// Stop every batch.
pub const STOP_ALL_ROUTE: &str = "/api/live/bots/stop-all";
const SECRET_PREFIX: &str = "NW_SECRET_";
const COMMENT: char = '#';
const CONFIG_ARG: &str = "--config";
const SWARM_COMMAND: &str = "swarm";
const BOTS_ARG: &str = "--bots";
const DURATION_ENV: &str = "NW_TESTCLIENT_SWARM__DURATION_SECONDS";
const ACCOUNT_PREFIX_ENV: &str = "NW_TESTCLIENT_SWARM__ACCOUNT_PREFIX";
const CHARACTER_PREFIX_ENV: &str = "NW_TESTCLIENT_SWARM__CHARACTER_PREFIX";
/// `duration_seconds = 0`: the swarm runs until its standard input closes.
const UNTIL_STOPPED: &str = "0";
const LETTERS: u32 = 26;
const FIRST_LETTER: u8 = b'a';
const LINE_END: u8 = b'\n';

/// Where the batches come from.
#[derive(Debug, Clone)]
pub struct BotSettings {
    /// The `nw-testclient` program.
    pub testclient_bin: PathBuf,
    /// Its configuration.
    pub testclient_config: PathBuf,
    /// The `KEY=VALUE` file whose `NW_SECRET_*` lines the batches get.
    pub testclient_env_file: PathBuf,
    /// Account prefix; the batch letters follow it.
    pub account_prefix: String,
    /// Character prefix; the batch letters follow it.
    pub character_prefix: String,
    /// Most bots of all batches together.
    pub max_bots: u32,
    /// Time a stopped batch has to exit before it is killed.
    pub stop_grace: Duration,
}

impl BotSettings {
    /// The settings of `[live]`, with [`STOP_GRACE`].
    pub fn of(config: &LiveConfig) -> Self {
        Self {
            testclient_bin: config.testclient_bin.clone(),
            testclient_config: config.testclient_config.clone(),
            testclient_env_file: config.testclient_env_file.clone(),
            account_prefix: config.bot_account_prefix.clone(),
            character_prefix: config.bot_character_prefix.clone(),
            max_bots: config.max_bots,
            stop_grace: STOP_GRACE,
        }
    }
}

/// What a batch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    /// It waits for the batch before it to start its bots.
    Waiting,
    /// Its bots run.
    Running,
    /// Its input is closed; it has not exited yet.
    Stopping,
}

/// One batch, for the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchView {
    /// The batch number.
    pub batch: u32,
    /// Its bots.
    pub size: u32,
    /// Unix milliseconds of its start, or of its request while it waits.
    pub started_ms: u64,
    /// What it does.
    pub state: BatchState,
}

/// A batch request that cannot be done.
#[derive(Debug, thiserror::Error)]
pub enum BotsError {
    /// The size is not one of [`BATCH_SIZES`].
    #[error("batch size {0} is not one of {BATCH_SIZES:?}")]
    Size(u32),
    /// The batch would pass `max_bots`.
    #[error("{running} bots run; the batch would pass the limit of {max}")]
    Limit {
        /// Bots of the running batches.
        running: u32,
        /// `max_bots`.
        max: u32,
    },
    /// No running batch has this number.
    #[error("no running batch has this number")]
    UnknownBatch,
    /// The process did not start.
    #[error("batch not started: {0}")]
    Spawn(String),
    /// The environment file cannot be read.
    #[error("environment file not read: {0}")]
    EnvFile(String),
}

impl BotsError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Size(_) | Self::Limit { .. } => StatusCode::BAD_REQUEST,
            Self::UnknownBatch => StatusCode::NOT_FOUND,
            Self::Spawn(_) | Self::EnvFile(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The `NW_SECRET_*` lines of a `KEY=VALUE` file; comments, blank lines and other keys are
/// skipped.
///
/// # Errors
///
/// [`BotsError::EnvFile`] when the file cannot be read.
pub fn read_secret_env(path: &Path) -> Result<Vec<(String, String)>, BotsError> {
    let text =
        std::fs::read_to_string(path).map_err(|error| BotsError::EnvFile(error.to_string()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with(COMMENT))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .filter(|(key, _)| key.starts_with(SECRET_PREFIX))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

/// The batch number in lower-case base 26: 0 is `a`, 25 is `z`, 26 is `ba`.
pub fn batch_letters(batch: u32) -> String {
    let mut rest = batch;
    let mut letters = Vec::new();
    loop {
        // The remainder is below 26.
        letters.push(char::from(FIRST_LETTER + (rest % LETTERS) as u8));
        rest /= LETTERS;
        if rest == 0 {
            break;
        }
    }
    letters.iter().rev().collect()
}

fn ended_text(batch: u32, status: std::io::Result<ExitStatus>) -> String {
    match status.map(|status| status.code()) {
        Ok(Some(code)) => format!("batch {batch} ended with code {code}"),
        Ok(None) => format!("batch {batch} ended by a signal"),
        Err(error) => format!("batch {batch} ended: {error}"),
    }
}

#[derive(Debug)]
struct Batch {
    view: BatchView,
    stop: Option<oneshot::Sender<()>>,
    /// Its process runs and has not started all its bots yet.
    logging_in: bool,
}

#[derive(Debug, Default)]
struct Batches {
    next: u32,
    running: BTreeMap<u32, Batch>,
}

impl Batches {
    /// Bots of the started batches.
    fn bots(&self) -> u32 {
        self.running
            .values()
            .filter(|batch| batch.view.state != BatchState::Waiting)
            .map(|batch| batch.view.size)
            .sum()
    }

    /// Bots of every batch, the waiting ones too.
    fn asked(&self) -> u32 {
        self.running.values().map(|batch| batch.view.size).sum()
    }

    /// The batch that starts its bots now, if any.
    fn logging_in(&self) -> Option<u32> {
        self.running
            .values()
            .find(|batch| batch.logging_in)
            .map(|batch| batch.view.batch)
    }

    /// The oldest waiting batch.
    fn first_waiting(&self) -> Option<u32> {
        self.running
            .values()
            .find(|batch| batch.view.state == BatchState::Waiting)
            .map(|batch| batch.view.batch)
    }
}

/// The bot batches.
#[derive(Debug, Clone)]
pub struct Bots {
    settings: Arc<BotSettings>,
    batches: Arc<Mutex<Batches>>,
    state: SharedState,
}

impl Bots {
    /// No batch yet; events and the bot total go to `state`.
    pub fn new(settings: BotSettings, state: SharedState) -> Self {
        Self {
            settings: Arc::new(settings),
            batches: Arc::default(),
            state,
        }
    }

    fn batches(&self) -> MutexGuard<'_, Batches> {
        self.batches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn event(&self, text: String) {
        lock(&self.state).event(text, unix_ms());
    }

    fn total_changed(&self, total: u32, text: String) {
        let mut state = lock(&self.state);
        state.bots(total);
        state.event(text, unix_ms());
    }

    /// Asks for a batch of `size` bots: it starts now, or after the batches before it have
    /// started their bots.
    ///
    /// # Errors
    ///
    /// [`BotsError::Size`], [`BotsError::Limit`] or [`BotsError::EnvFile`]; then nothing is
    /// asked. [`BotsError::Spawn`] when it should start now and does not. A waiting batch that
    /// later fails to start is an event.
    pub fn start(&self, size: u32) -> Result<BatchView, BotsError> {
        if !BATCH_SIZES.contains(&size) {
            return Err(BotsError::Size(size));
        }
        // Read now, so that a missing file is an answer to this request.
        read_secret_env(&self.settings.testclient_env_file)?;
        let mut batches = self.batches();
        let asked = batches.asked();
        if asked.saturating_add(size) > self.settings.max_bots {
            return Err(BotsError::Limit {
                running: asked,
                max: self.settings.max_bots,
            });
        }
        let batch = batches.next;
        batches.next = batches.next.saturating_add(1);
        let view = BatchView {
            batch,
            size,
            started_ms: unix_ms(),
            state: BatchState::Waiting,
        };
        batches.running.insert(
            batch,
            Batch {
                view,
                stop: None,
                logging_in: false,
            },
        );
        let ahead = batches.logging_in();
        drop(batches);
        if let Some(ahead) = ahead {
            self.event(format!(
                "batch {batch} waits for batch {ahead} to start its bots: {size} bots"
            ));
        }
        self.start_next();
        self.batches()
            .running
            .get(&batch)
            .map(|found| found.view.clone())
            .ok_or_else(|| BotsError::Spawn(format!("batch {batch} did not start; see the events")))
    }

    /// Starts the oldest waiting batch when no batch is starting its bots.
    fn start_next(&self) {
        loop {
            let mut batches = self.batches();
            if batches.logging_in().is_some() {
                return;
            }
            let Some(batch) = batches.first_waiting() else {
                return;
            };
            match self.spawn(&mut batches, batch) {
                Ok((child, stopped, size, total)) => {
                    drop(batches);
                    self.run(batch, child, stopped);
                    self.total_changed(total, format!("batch {batch} started: {size} bots"));
                    return;
                }
                Err(error) => {
                    batches.running.remove(&batch);
                    drop(batches);
                    self.event(format!("batch {batch} {error}"));
                }
            }
        }
    }

    /// Starts the process of waiting batch `batch`; returns it, its stop, its size and the new
    /// bot total.
    fn spawn(
        &self,
        batches: &mut Batches,
        batch: u32,
    ) -> Result<(Child, oneshot::Receiver<()>, u32, u32), BotsError> {
        let secrets = read_secret_env(&self.settings.testclient_env_file)?;
        let size = batches
            .running
            .get(&batch)
            .map(|found| found.view.size)
            .ok_or(BotsError::UnknownBatch)?;
        let letters = batch_letters(batch);
        let child = Command::new(&self.settings.testclient_bin)
            .arg(CONFIG_ARG)
            .arg(&self.settings.testclient_config)
            .arg(SWARM_COMMAND)
            .arg(BOTS_ARG)
            .arg(size.to_string())
            .env(DURATION_ENV, UNTIL_STOPPED)
            .env(
                ACCOUNT_PREFIX_ENV,
                format!("{}{letters}", self.settings.account_prefix),
            )
            .env(
                CHARACTER_PREFIX_ENV,
                format!("{}{letters}", self.settings.character_prefix),
            )
            .envs(secrets)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| BotsError::Spawn(error.to_string()))?;
        let (stop, stopped) = oneshot::channel();
        if let Some(found) = batches.running.get_mut(&batch) {
            found.view.state = BatchState::Running;
            found.view.started_ms = unix_ms();
            found.stop = Some(stop);
            found.logging_in = true;
        }
        Ok((child, stopped, size, batches.bots()))
    }

    /// Reads the output of a started batch and waits for its end.
    fn run(&self, batch: u32, mut child: Child, stopped: oneshot::Receiver<()>) {
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(self.clone().report_lines(batch, stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(self.clone().report_lines(batch, stderr));
        }
        let stdin = child.stdin.take();
        tokio::spawn(self.clone().supervise(batch, child, stdin, stopped));
    }

    /// Batch `batch` started all its bots: the next waiting batch may start.
    fn bots_started(&self, batch: u32) {
        if let Some(found) = self.batches().running.get_mut(&batch) {
            found.logging_in = false;
        }
        self.start_next();
    }

    /// Closes the input of batch `batch`; it is killed when it does not exit in time.
    ///
    /// # Errors
    ///
    /// [`BotsError::UnknownBatch`] when no running batch has this number.
    pub fn stop(&self, batch: u32) -> Result<(), BotsError> {
        let mut batches = self.batches();
        let found = batches
            .running
            .get_mut(&batch)
            .ok_or(BotsError::UnknownBatch)?;
        if found.view.state == BatchState::Waiting {
            batches.running.remove(&batch);
            drop(batches);
            self.event(format!("batch {batch} no longer waits"));
            return Ok(());
        }
        if let Some(stop) = found.stop.take() {
            found.view.state = BatchState::Stopping;
            // A failed send means the batch task ended already.
            let _ = stop.send(());
            drop(batches);
            self.event(format!("batch {batch} stopping"));
        }
        Ok(())
    }

    /// Stops every running batch.
    pub fn stop_all(&self) {
        let numbers: Vec<u32> = self.batches().running.keys().copied().collect();
        for batch in numbers {
            // Only a batch that ended meanwhile is unknown.
            let _ = self.stop(batch);
        }
    }

    /// The running batches.
    pub fn list(&self) -> Vec<BatchView> {
        self.batches()
            .running
            .values()
            .map(|batch| batch.view.clone())
            .collect()
    }

    async fn report_lines(self, batch: u32, output: impl AsyncRead + Unpin) {
        let mut reader = BufReader::new(output);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(LINE_END, &mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    self.event(format!("batch {batch}: {}", text.trim_end()));
                    if text.contains(BOTS_STARTED_MARK) {
                        self.bots_started(batch);
                    }
                }
            }
        }
    }

    async fn supervise(
        self,
        batch: u32,
        mut child: Child,
        stdin: Option<ChildStdin>,
        stopped: oneshot::Receiver<()>,
    ) {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = stopped => {
                drop(stdin);
                match tokio::time::timeout(self.settings.stop_grace, child.wait()).await {
                    Ok(status) => status,
                    Err(_) => {
                        self.event(format!("batch {batch} did not stop in time; killed"));
                        // A failed kill means the process ended meanwhile.
                        let _ = child.start_kill();
                        child.wait().await
                    }
                }
            }
        };
        let total = {
            let mut batches = self.batches();
            batches.running.remove(&batch);
            batches.bots()
        };
        self.total_changed(total, ended_text(batch, status));
        self.start_next();
    }
}

/// Bots of a `[live]` section with no program, for route tests.
#[cfg(test)]
pub(crate) fn idle_bots(state: SharedState) -> Bots {
    Bots::new(
        BotSettings {
            testclient_bin: PathBuf::new(),
            testclient_config: PathBuf::new(),
            testclient_env_file: PathBuf::new(),
            account_prefix: String::new(),
            character_prefix: String::new(),
            max_bots: 0,
            stop_grace: STOP_GRACE,
        },
        state,
    )
}

/// The bot routes; every `POST` needs the admin header (the guard).
pub fn bot_routes() -> Router<AppState> {
    Router::new()
        .route(BOTS_ROUTE, get(list_batches).post(start_batch))
        .route(STOP_ROUTE, post(stop_batch))
        .route(STOP_ALL_ROUTE, post(stop_all))
}

#[derive(Debug, Deserialize)]
struct StartRequest {
    size: u32,
}

fn bots_of(state: &AppState) -> Option<&Bots> {
    state.live.as_ref().map(|live| &live.bots)
}

fn error_response(error: &BotsError) -> Response {
    (error.status(), error.to_string()).into_response()
}

async fn list_batches(State(state): State<AppState>) -> Response {
    match bots_of(&state) {
        Some(bots) => Json(bots.list()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn start_batch(State(state): State<AppState>, Json(request): Json<StartRequest>) -> Response {
    match bots_of(&state).map(|bots| bots.start(request.size)) {
        Some(Ok(view)) => Json(view).into_response(),
        Some(Err(error)) => error_response(&error),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn stop_batch(State(state): State<AppState>, UrlPath(batch): UrlPath<u32>) -> Response {
    match bots_of(&state).map(|bots| bots.stop(batch)) {
        Some(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Some(Err(error)) => error_response(&error),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn stop_all(State(state): State<AppState>) -> Response {
    match bots_of(&state) {
        Some(bots) => {
            bots.stop_all();
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::guard::AdminOrigin;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use nw_testclient::fleet::started_line;
    use tower::ServiceExt;

    use super::*;
    use crate::captures::Captures;
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE};
    use crate::live::{Live, lock, shared};
    use crate::server::{AppState, router};
    use crate::world::tests::test_layer;

    /// Writing a script while another test starts a process can make the start fail
    /// ("text file busy"): writes and starts take this lock.
    static SPAWN: Mutex<()> = Mutex::new(());
    const GRACE: Duration = Duration::from_millis(100);
    const STEP: Duration = Duration::from_millis(20);
    const WAIT: Duration = Duration::from_secs(10);
    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const MAX_BOTS: u32 = 10;
    const FAKE_SECRET: &str = "fake-test-secret";
    const PRINTING: &str = r#"#!/bin/sh
echo "args: $*"
echo "account: $NW_TESTCLIENT_SWARM__ACCOUNT_PREFIX"
echo "character: $NW_TESTCLIENT_SWARM__CHARACTER_PREFIX"
echo "duration: $NW_TESTCLIENT_SWARM__DURATION_SECONDS"
echo "secret: ${NW_SECRET_TEST_PASSWORD:-none}"
echo "other: ${OTHER_TEST_VALUE:-none}"
echo "to stderr" >&2
cat > /dev/null
echo "input closed"
"#;
    const IGNORING: &str = "#!/bin/sh\necho started\nexec sleep 30\n";
    /// Starts its bots at once (the line of `nw-testclient`), then runs until its input closes.
    /// A batch that starts its bots at once (its fifth argument is the bot count).
    fn quick_script() -> String {
        format!("#!/bin/sh\necho \"$5{BOTS_STARTED_MARK}0.0 s\"\ncat > /dev/null\n")
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        bots: Bots,
        state: SharedState,
    }

    fn fixture(script: &str) -> Fixture {
        let _spawn = SPAWN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("dir");
        let bin = dir.path().join("fake-testclient");
        std::fs::write(&bin, script).expect("write");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let env_file = dir.path().join("test.env");
        std::fs::write(
            &env_file,
            format!(
                "# comment\n\nNW_SECRET_TEST_PASSWORD={FAKE_SECRET}\nOTHER_TEST_VALUE=kept-out\n"
            ),
        )
        .expect("write");
        let state = shared();
        let settings = BotSettings {
            testclient_bin: bin,
            testclient_config: dir.path().join("nw-testclient.toml"),
            testclient_env_file: env_file,
            account_prefix: "adminbot".to_owned(),
            character_prefix: "Adminbot".to_owned(),
            max_bots: MAX_BOTS,
            stop_grace: GRACE,
        };
        let bots = Bots::new(settings, state.clone());
        Fixture {
            _dir: dir,
            bots,
            state,
        }
    }

    fn start(fixture: &Fixture, size: u32) -> Result<BatchView, BotsError> {
        let _spawn = SPAWN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fixture.bots.start(size)
    }

    async fn wait_for(state: &SharedState, texts: &[&str]) -> Vec<String> {
        tokio::time::timeout(WAIT, async {
            loop {
                let events: Vec<String> = lock(state)
                    .view()
                    .events
                    .into_iter()
                    .map(|event| event.text)
                    .collect();
                if texts
                    .iter()
                    .all(|text| events.iter().any(|event| event == text))
                {
                    return events;
                }
                tokio::time::sleep(STEP).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("events {texts:?} did not come"))
    }

    #[test]
    fn batch_letters_count_in_base_26() {
        assert_eq!(batch_letters(0), "a");
        assert_eq!(batch_letters(25), "z");
        assert_eq!(batch_letters(26), "ba");
        assert_eq!(batch_letters(675), "zz");
        assert_eq!(batch_letters(676), "baa");
    }

    #[test]
    fn only_secret_lines_reach_the_batch() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("test.env");
        std::fs::write(
            &path,
            "# NW_SECRET_COMMENTED=no\n\n NW_SECRET_A = one\nNW_SECRET_B=two=three\nOTHER=x\nnot a line\n",
        )
        .expect("write");
        assert_eq!(
            read_secret_env(&path).expect("read"),
            vec![
                ("NW_SECRET_A".to_owned(), "one".to_owned()),
                ("NW_SECRET_B".to_owned(), "two=three".to_owned()),
            ]
        );
        assert!(matches!(
            read_secret_env(&dir.path().join("missing.env")),
            Err(BotsError::EnvFile(_))
        ));
    }

    #[tokio::test]
    async fn a_batch_runs_the_swarm_with_its_prefixes() {
        let fixture = fixture(PRINTING);
        let batch = start(&fixture, MAX_BOTS).expect("start");
        assert_eq!((batch.batch, batch.size), (0, MAX_BOTS));
        let config = fixture
            .bots
            .settings
            .testclient_config
            .display()
            .to_string();
        wait_for(
            &fixture.state,
            &[
                &format!("batch 0: args: --config {config} swarm --bots {MAX_BOTS}"),
                "batch 0: account: adminbota",
                "batch 0: character: Adminbota",
                "batch 0: duration: 0",
                &format!("batch 0: secret: {FAKE_SECRET}"),
                "batch 0: other: none",
            ],
        )
        .await;
        assert_eq!(lock(&fixture.state).view().tiles.bots, MAX_BOTS);
        fixture.bots.stop_all();
        wait_for(&fixture.state, &["batch 0 ended with code 0"]).await;
    }

    #[tokio::test]
    async fn batch_output_becomes_events() {
        let fixture = fixture(PRINTING);
        start(&fixture, 1).expect("start");
        wait_for(
            &fixture.state,
            &["batch 0 started: 1 bots", "batch 0: to stderr"],
        )
        .await;
        fixture.bots.stop_all();
        wait_for(&fixture.state, &["batch 0 ended with code 0"]).await;
    }

    #[tokio::test]
    async fn stopping_a_batch_closes_its_input_and_it_exits() {
        let fixture = fixture(PRINTING);
        start(&fixture, 1).expect("start");
        assert_eq!(fixture.bots.list().len(), 1);
        assert!(matches!(fixture.bots.stop(9), Err(BotsError::UnknownBatch)));
        fixture.bots.stop(0).expect("stop");
        wait_for(
            &fixture.state,
            &["batch 0: input closed", "batch 0 ended with code 0"],
        )
        .await;
        assert!(fixture.bots.list().is_empty());
        assert_eq!(lock(&fixture.state).view().tiles.bots, 0);
    }

    #[tokio::test]
    async fn a_batch_that_does_not_stop_is_killed() {
        let fixture = fixture(IGNORING);
        start(&fixture, 1).expect("start");
        wait_for(&fixture.state, &["batch 0: started"]).await;
        fixture.bots.stop(0).expect("stop");
        wait_for(
            &fixture.state,
            &[
                "batch 0 did not stop in time; killed",
                "batch 0 ended by a signal",
            ],
        )
        .await;
        assert!(fixture.bots.list().is_empty());
    }

    fn states(fixture: &Fixture) -> Vec<(u32, BatchState)> {
        fixture
            .bots
            .list()
            .iter()
            .map(|batch| (batch.batch, batch.state))
            .collect()
    }

    #[tokio::test]
    async fn a_batch_waits_while_another_starts_its_bots() {
        let fixture = fixture(PRINTING);
        start(&fixture, 1).expect("first");
        let second = start(&fixture, 1).expect("second");
        assert_eq!(second.state, BatchState::Waiting);
        wait_for(
            &fixture.state,
            &["batch 1 waits for batch 0 to start its bots: 1 bots"],
        )
        .await;
        assert_eq!(
            states(&fixture),
            vec![(0, BatchState::Running), (1, BatchState::Waiting)]
        );
        assert_eq!(
            lock(&fixture.state).view().tiles.bots,
            1,
            "a waiting batch is not started"
        );
        fixture.bots.stop(0).expect("stop");
        wait_for(
            &fixture.state,
            &["batch 0 ended with code 0", "batch 1 started: 1 bots"],
        )
        .await;
        fixture.bots.stop_all();
        wait_for(&fixture.state, &["batch 1 ended with code 0"]).await;
    }

    #[tokio::test]
    async fn the_next_batch_starts_when_the_bots_are_started() {
        let fixture = fixture(&quick_script());
        start(&fixture, 1).expect("first");
        start(&fixture, 1).expect("second");
        let line = started_line(1, Duration::ZERO);
        wait_for(
            &fixture.state,
            &[&format!("batch 0: {line}"), &format!("batch 1: {line}")],
        )
        .await;
        assert_eq!(
            states(&fixture),
            vec![(0, BatchState::Running), (1, BatchState::Running)]
        );
        assert_eq!(lock(&fixture.state).view().tiles.bots, 2);
        fixture.bots.stop_all();
        wait_for(
            &fixture.state,
            &["batch 0 ended with code 0", "batch 1 ended with code 0"],
        )
        .await;
    }

    #[tokio::test]
    async fn a_waiting_batch_can_be_taken_back_and_counts_to_the_limit() {
        let fixture = fixture(PRINTING);
        start(&fixture, MAX_BOTS).expect("first");
        assert!(
            matches!(start(&fixture, 1), Err(BotsError::Limit { .. })),
            "the limit counts every asked bot"
        );
        fixture.bots.stop(0).expect("stop");
        wait_for(&fixture.state, &["batch 0 ended with code 0"]).await;
        start(&fixture, 1).expect("running");
        start(&fixture, 1).expect("waiting");
        fixture.bots.stop(2).expect("take back");
        wait_for(&fixture.state, &["batch 2 no longer waits"]).await;
        assert_eq!(states(&fixture), vec![(1, BatchState::Running)]);
        fixture.bots.stop_all();
        wait_for(&fixture.state, &["batch 1 ended with code 0"]).await;
    }

    fn app(fixture: &Fixture) -> axum::Router {
        router(AppState::new(
            test_layer(CAP),
            std::env::temp_dir(),
            AdminOrigin::of(LISTEN.parse().expect("address")),
            Captures::default(),
            Some(Live::new(
                fixture.state.clone(),
                GRACE,
                fixture.bots.clone(),
                tokio::sync::watch::channel(false).1,
            )),
        ))
    }

    fn post(uri: &str, body: &str, admin: bool) -> Request<Body> {
        let builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, LISTEN)
            .header(header::CONTENT_TYPE, "application/json");
        let builder = if admin {
            builder.header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
        } else {
            builder
        };
        builder.body(Body::from(body.to_owned())).expect("request")
    }

    async fn status(app: axum::Router, request: Request<Body>) -> StatusCode {
        app.oneshot(request).await.expect("response").status()
    }

    #[tokio::test]
    async fn bots_past_the_limit_are_refused() {
        let fixture = fixture(PRINTING);
        let past = BATCH_SIZES
            .iter()
            .find(|size| **size > MAX_BOTS)
            .expect("size");
        let status = status(
            app(&fixture),
            post(BOTS_ROUTE, &format!("{{\"size\":{past}}}"), true),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(fixture.bots.list().is_empty());
    }

    #[tokio::test]
    async fn a_size_not_in_the_list_is_refused() {
        let fixture = fixture(PRINTING);
        let status = status(app(&fixture), post(BOTS_ROUTE, "{\"size\":5}", true)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(fixture.bots.list().is_empty());
    }

    #[tokio::test]
    async fn the_bot_routes_need_the_admin_header() {
        let fixture = fixture(PRINTING);
        for uri in [BOTS_ROUTE, STOP_ALL_ROUTE, "/api/live/bots/0/stop"] {
            let status = status(app(&fixture), post(uri, "{\"size\":1}", false)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        }
        let status = status(app(&fixture), post("/api/live/bots/0/stop", "", true)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(fixture.bots.list().is_empty());
    }
}
