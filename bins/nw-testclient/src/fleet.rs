// SPDX-License-Identifier: AGPL-3.0-or-later
//! What `swarm` and `hunt` share (M5 spec section 11, M6-B spec section 9): the bot accounts, the
//! login of every bot at a steady rate, its REP run with the bot's own logic on each tick, the
//! sim metrics before and after the run, and the checks of those metrics.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nw_common::net::bind_udp;
use nw_reflect::LoginToken;
use nw_sim_core::TICKS_PER_SECOND;
use reqwest::Client;
use sim::server::{
    DEAD_REASON, HANDOFF_MOVED, HANDOFF_REFUSED, HANDOFF_TIMEOUT, KIND_LABEL, METRIC_DEATHS,
    METRIC_HANDOFF_SECONDS, METRIC_HANDOFFS, METRIC_REFUSED_MOVES, METRIC_TICK_SECONDS, MOB_KIND,
    ONE_TICK_SECONDS, REASON_LABEL, RESULT_LABEL,
};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::pair::Check;
use crate::rep::{RepClient, RepError};
use crate::udp::{ANY_LOCAL, Hold, RepOutcome, RunError, run_rep_on};
use crate::walk::{WalkSettings, login_token, walk};
use nw_common::prometheus::Scrape;

/// Check: every bot got registration status 0.
pub const BOTS_REGISTERED: &str = "every bot registered";
/// Check: no bot was closed by the server.
pub const BOTS_CONNECTED: &str = "no bot was closed";
/// Check: no gap between two own bundles was longer than the limit.
pub const BUNDLES_STEADY: &str = "the bundles never stopped";
/// Check: 99 % of the ticks took at most one tick.
pub const TICKS_FAST: &str = "tick p99 within one tick";
/// Check: no sim refused a move.
pub const NO_REFUSED_MOVES: &str = "no move was refused";
/// Check: the metrics of every sim were read before and after the run.
pub const METRICS_READ: &str = "the metrics of every sim were read";
/// The share of the p99 checks.
pub const P99_SHARE: f64 = 0.99;
/// Letters of a character name suffix: `aaa` to `zzz`.
pub const NAME_SUFFIX_LETTERS: u32 = 3;
const ALPHABET: u32 = 26;
/// Most bots: one name suffix each.
pub const MAX_BOTS: usize = 26 * 26 * 26;
/// Digits of the account number.
const ACCOUNT_DIGITS: usize = 5;
/// Metrics count of a histogram.
const COUNT_SUFFIX: &str = "_count";
/// The row name of the UDP counters in `/proc/net/snmp`.
const SNMP_UDP_ROW: &str = "Udp:";
/// The counter of datagrams dropped for a full receive buffer.
const SNMP_RECEIVE_ERRORS: &str = "RcvbufErrors";

/// The bots of a run and how they log in.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetSettings {
    /// Bots to run.
    pub bots: usize,
    /// Account names are this and a number.
    pub account_prefix: String,
    /// Character names are this and three letters.
    pub character_prefix: String,
    /// The creation settings of a new bot character.
    pub creation_params: String,
    /// The password of every bot account.
    pub password: String,
    /// `https://login.<domain>`.
    pub login_url: String,
    /// `https://game.<domain>`.
    pub game_url: String,
    /// New bots per second.
    pub bots_per_second: u32,
    /// Time in the world per bot; `None` until the run is told to stop.
    pub duration: Option<Duration>,
    /// Longest wait for a registration response.
    pub registration_timeout: Duration,
    /// The metrics URL of every sim.
    pub metrics_urls: Vec<String>,
    /// Receive and send buffer of each bot socket, in bytes.
    pub udp_buffer_bytes: usize,
}

/// The logic of one bot of a scenario.
pub trait FleetBot: Send + 'static {
    /// What the bot reports at the end.
    type Report: Send + 'static;

    /// One tick of the REP run, `since` the registration.
    ///
    /// # Errors
    ///
    /// Returns [`RepError`] when a message cannot be sent; the run stops.
    fn tick(
        &mut self,
        client: &mut RepClient,
        outcome: &mut RepOutcome,
        since: Duration,
    ) -> Result<(), RepError>;

    /// The report of a run that ended.
    fn report(self, outcome: &RepOutcome) -> Self::Report;

    /// The report of a bot that could not run.
    fn failed(error: String) -> Self::Report;
}

/// Makes the REP client of a bot.
pub type NewClient = Arc<dyn Fn(LoginToken) -> Result<RepClient, RepError> + Send + Sync>;

/// Makes bot `n`.
pub type NewBot<B> = Arc<dyn Fn(usize) -> B + Send + Sync>;

/// The sums of the sim metrics over the run.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SimTotals {
    /// Handoffs that moved a player.
    pub moved: f64,
    /// Handoffs refused or not answered in time.
    pub failed: f64,
    /// Handoffs within one tick.
    pub handoffs_within_tick: f64,
    /// Every handoff.
    pub handoffs: f64,
    /// Ticks within one tick.
    pub ticks_within_tick: f64,
    /// Every tick.
    pub ticks: f64,
    /// Moves of living players that a sim refused.
    pub refused_moves: f64,
    /// Moves that arrived after the player's death: the client learns of a death only when its
    /// 4299 arrives, so these are not faults.
    pub dead_moves: f64,
    /// Mobs that died.
    pub mob_deaths: f64,
}

impl SimTotals {
    /// The totals of a scrape of every sim.
    pub fn of(scrape: &Scrape) -> Self {
        let result = |value| Some((RESULT_LABEL, value));
        let count = |histogram: &str| scrape.total(&format!("{histogram}{COUNT_SUFFIX}"), None);
        let dead_moves = scrape.total(METRIC_REFUSED_MOVES, Some((REASON_LABEL, DEAD_REASON)));
        Self {
            moved: scrape.total(METRIC_HANDOFFS, result(HANDOFF_MOVED)),
            failed: scrape.total(METRIC_HANDOFFS, result(HANDOFF_REFUSED))
                + scrape.total(METRIC_HANDOFFS, result(HANDOFF_TIMEOUT)),
            handoffs_within_tick: scrape.bucket(METRIC_HANDOFF_SECONDS, ONE_TICK_SECONDS),
            handoffs: count(METRIC_HANDOFF_SECONDS),
            ticks_within_tick: scrape.bucket(METRIC_TICK_SECONDS, ONE_TICK_SECONDS),
            ticks: count(METRIC_TICK_SECONDS),
            refused_moves: scrape.total(METRIC_REFUSED_MOVES, None) - dead_moves,
            dead_moves,
            mob_deaths: scrape.total(METRIC_DEATHS, Some((KIND_LABEL, MOB_KIND))),
        }
    }

    /// What happened between `before` and `self`.
    pub fn since(&self, before: &Self) -> Self {
        Self {
            moved: self.moved - before.moved,
            failed: self.failed - before.failed,
            handoffs_within_tick: self.handoffs_within_tick - before.handoffs_within_tick,
            handoffs: self.handoffs - before.handoffs,
            ticks_within_tick: self.ticks_within_tick - before.ticks_within_tick,
            ticks: self.ticks - before.ticks,
            refused_moves: self.refused_moves - before.refused_moves,
            dead_moves: self.dead_moves - before.dead_moves,
            mob_deaths: self.mob_deaths - before.mob_deaths,
        }
    }

    /// The checks of the sim metrics: ticks, refused moves of living players, and every page
    /// read.
    pub fn checks(&self, unread_metrics: usize, metrics_urls: &[String]) -> [Check; 3] {
        [
            Check {
                name: TICKS_FAST,
                passed: p99(self.ticks_within_tick, self.ticks),
            },
            Check {
                name: NO_REFUSED_MOVES,
                passed: self.refused_moves == 0.0,
            },
            Check {
                name: METRICS_READ,
                passed: !metrics_urls.is_empty() && unread_metrics == 0,
            },
        ]
    }
}

/// Whether `within` is at least [`P99_SHARE`] of `total`, and `total` is above 0.
pub fn p99(within: f64, total: f64) -> bool {
    total > 0.0 && within / total >= P99_SHARE
}

/// `part / whole`, or 0 for no whole.
pub fn ratio(part: f64, whole: f64) -> f64 {
    if whole > 0.0 { part / whole } else { 0.0 }
}

/// When a bot's own bundles came, from its registration on.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BundleClock {
    first: Option<Duration>,
    last: Option<Duration>,
    received: u64,
    max_gap: Duration,
}

impl BundleClock {
    /// No bundle yet.
    pub const fn new() -> Self {
        Self {
            first: None,
            last: None,
            received: 0,
            max_gap: Duration::ZERO,
        }
    }

    /// `count` own bundles arrived `since` the registration.
    pub fn saw(&mut self, count: usize, since: Duration) {
        if count == 0 {
            return;
        }
        if let Some(last) = self.last {
            self.max_gap = self.max_gap.max(since.saturating_sub(last));
        }
        self.first.get_or_insert(since);
        self.last = Some(since);
        self.received += count as u64;
    }

    /// Own bundles received, and those the ticks from the first one to `end` (the end of the
    /// bot's run, after its registration) hold.
    pub fn bundles(&self, end: Duration) -> (u64, u64) {
        let expected = match self.first {
            Some(first) => {
                let ticks = end.saturating_sub(first).as_secs_f64() * TICKS_PER_SECOND as f64;
                // A run is minutes long, so the tick count fits u64.
                ticks.round() as u64 + 1
            }
            None => 0,
        };
        (self.received, expected)
    }

    /// The longest time without an own bundle, also from the last one to `end`.
    pub fn max_gap(&self, end: Duration) -> Duration {
        let tail = self
            .last
            .map_or(Duration::ZERO, |last| end.saturating_sub(last));
        self.max_gap.max(tail)
    }
}

/// Every bot report and the sim totals of a run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FleetRun<R> {
    /// One report per bot.
    pub bots: Vec<R>,
    /// The sim metrics of the run.
    pub sims: SimTotals,
    /// Metrics pages that could not be read, before and after the run.
    pub unread_metrics: usize,
}

/// How many bots stopped with each error, by error text.
pub fn error_kinds<'a>(errors: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, usize> {
    let mut kinds = BTreeMap::new();
    for error in errors {
        *kinds.entry(error).or_default() += 1;
    }
    kinds
}

/// The account and character names of bot `n`: the account prefix and five digits, the
/// character prefix and three letters (character names hold letters only).
pub fn bot_names(account_prefix: &str, character_prefix: &str, n: usize) -> (String, String) {
    let account = format!("{account_prefix}{n:0ACCOUNT_DIGITS$}");
    let mut suffix = String::new();
    let mut rest = u32::try_from(n).unwrap_or(u32::MAX);
    for _ in 0..NAME_SUFFIX_LETTERS {
        suffix.insert(0, char::from(b'a' + (rest % ALPHABET) as u8));
        rest /= ALPHABET;
    }
    (account, format!("{character_prefix}{suffix}"))
}

/// The datagrams the PC dropped for a full UDP receive buffer, from the text of
/// `/proc/net/snmp`.
pub fn udp_receive_errors(snmp: &str) -> Option<u64> {
    let mut rows = snmp.lines().filter(|line| line.starts_with(SNMP_UDP_ROW));
    let names = rows.next()?;
    let values = rows.next()?;
    let column = names
        .split_whitespace()
        .position(|name| name == SNMP_RECEIVE_ERRORS)?;
    values.split_whitespace().nth(column)?.parse().ok()
}

/// The part of the line that says every bot of a run is started; the admin panel reads it to
/// start the next batch.
pub const BOTS_STARTED_MARK: &str = " bots started in ";

/// The line that says `bots` bots started in `took`.
pub fn started_line(bots: usize, took: Duration) -> String {
    format!("{bots}{BOTS_STARTED_MARK}{:.1} s", took.as_secs_f64())
}

/// Runs every bot, `bots_per_second` new ones each second, and reads the sim metrics before and
/// after. A stop ends the starts: bots not yet started do not start.
pub async fn run_fleet<B: FleetBot>(
    http: &Client,
    fleet: Arc<FleetSettings>,
    new_client: NewClient,
    new_bot: NewBot<B>,
    stop: watch::Receiver<bool>,
    mut report: impl FnMut(String),
) -> FleetRun<B::Report> {
    let before = scrape(http, &fleet.metrics_urls, &mut report).await;
    let mut bots = JoinSet::new();
    let pause = Duration::from_secs(1) / fleet.bots_per_second.max(1);
    let started = Instant::now();
    let mut stopped = stop.clone();
    for n in 0..fleet.bots {
        if *stopped.borrow() {
            break;
        }
        let (http, fleet, new_client) = (http.clone(), Arc::clone(&fleet), Arc::clone(&new_client));
        let bot = new_bot(n);
        let hold = fleet
            .duration
            .map_or_else(|| Hold::UntilStopped(stop.clone()), Hold::For);
        bots.spawn(async move { run_bot(&http, &fleet, n, &*new_client, bot, hold).await });
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            // An error means the sender is gone: nobody can stop the run, so only the pause ends.
            Ok(()) = stopped.changed() => {}
        }
    }
    report(started_line(bots.len(), started.elapsed()));
    let mut reports = Vec::with_capacity(fleet.bots);
    while let Some(done) = bots.join_next().await {
        reports.push(done.unwrap_or_else(|error| B::failed(error.to_string())));
    }
    let after = scrape(http, &fleet.metrics_urls, &mut report).await;
    FleetRun {
        bots: reports,
        sims: after.totals.since(&before.totals),
        unread_metrics: before.unread + after.unread,
    }
}

async fn run_bot<B: FleetBot>(
    http: &Client,
    fleet: &FleetSettings,
    n: usize,
    new_client: &(dyn Fn(LoginToken) -> Result<RepClient, RepError> + Send + Sync),
    mut bot: B,
    hold: Hold,
) -> B::Report {
    let (account, character_name) = bot_names(&fleet.account_prefix, &fleet.character_prefix, n);
    let walk_settings = WalkSettings {
        login_url: fleet.login_url.clone(),
        game_url: fleet.game_url.clone(),
        account,
        password: fleet.password.clone(),
        character_name,
        creation_params: fleet.creation_params.clone(),
    };
    let admission = match walk(http, &walk_settings, |_| {}).await {
        Ok(admission) => admission,
        Err(error) => return B::failed(error.to_string()),
    };
    let client = match new_client(login_token(&admission.token)) {
        Ok(client) => client,
        Err(error) => return B::failed(error.to_string()),
    };
    let socket = match socket(fleet.udp_buffer_bytes) {
        Ok(socket) => socket,
        Err(error) => return B::failed(error.to_string()),
    };
    let ran = run_rep_on(
        socket,
        admission.rep_address,
        client,
        hold,
        fleet.registration_timeout,
        |client, outcome, since| bot.tick(client, outcome, since),
    )
    .await;
    match ran {
        Ok(outcome) => bot.report(&outcome),
        Err(error) => B::failed(error.to_string()),
    }
}

/// A bot socket with `buffer_bytes` buffers.
fn socket(buffer_bytes: usize) -> Result<tokio::net::UdpSocket, RunError> {
    let any = ANY_LOCAL.parse().map_err(std::io::Error::other)?;
    Ok(bind_udp(any, buffer_bytes, false)?)
}

/// The metrics of every sim at one time.
struct Scraped {
    totals: SimTotals,
    unread: usize,
}

/// The totals of every sim's metrics; a page that cannot be read is reported and counted.
async fn scrape(http: &Client, urls: &[String], report: &mut impl FnMut(String)) -> Scraped {
    let mut sum = Scrape::default();
    let mut unread = 0;
    for url in urls {
        let text = match http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
        {
            Ok(response) => response.text().await,
            Err(error) => Err(error),
        };
        match text {
            Ok(text) => sum.add(&Scrape::parse(&text)),
            Err(error) => {
                unread += 1;
                report(format!("metrics of {url} not read: {error}"));
            }
        }
    }
    Scraped {
        totals: SimTotals::of(&sum),
        unread,
    }
}

/// Fleet settings for tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Two bots for `duration`, with one metrics URL.
    pub(crate) fn test_fleet(duration: Duration) -> FleetSettings {
        FleetSettings {
            bots: 2,
            account_prefix: "bot".to_owned(),
            character_prefix: "Swarm".to_owned(),
            creation_params: crate::walk::CHARACTER_CREATION_PARAMS.to_owned(),
            password: "password-ok".to_owned(),
            login_url: String::new(),
            game_url: String::new(),
            bots_per_second: 50,
            duration: Some(duration),
            registration_timeout: duration,
            metrics_urls: vec!["http://127.0.0.1:9102/metrics".to_owned()],
            udp_buffer_bytes: 1 << 20,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::test_support::test_fleet;
    use super::*;

    struct Idle;

    impl FleetBot for Idle {
        type Report = String;

        fn tick(
            &mut self,
            _: &mut RepClient,
            _: &mut RepOutcome,
            _: Duration,
        ) -> Result<(), RepError> {
            Ok(())
        }

        fn report(self, _: &RepOutcome) -> String {
            String::new()
        }

        fn failed(error: String) -> String {
            error
        }
    }

    #[test]
    fn the_started_line_holds_the_mark_the_panel_reads() {
        assert!(started_line(10, Duration::from_millis(200)).contains(BOTS_STARTED_MARK));
    }

    #[tokio::test]
    async fn a_fleet_nobody_can_stop_keeps_its_pace() {
        const BOTS: usize = 3;
        const PER_SECOND: u32 = 10;
        /// Two pauses of 100 ms between three starts.
        const SHORTEST: Duration = Duration::from_millis(200);
        nw_login::install_crypto_provider();
        let mut fleet = test_fleet(SHORTEST);
        fleet.bots = BOTS;
        fleet.bots_per_second = PER_SECOND;
        fleet.metrics_urls = Vec::new();
        let (stop, stopped) = watch::channel(false);
        drop(stop);
        let new_client: NewClient =
            Arc::new(|_| Err(RepError::Route(crate::route::RouteError::TooShort)));
        let started = Instant::now();
        let run = run_fleet(
            &Client::new(),
            Arc::new(fleet),
            new_client,
            Arc::new(|_| Idle),
            stopped,
            |_| {},
        )
        .await;
        assert_eq!(run.bots.len(), BOTS);
        assert!(started.elapsed() >= SHORTEST, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_stopped_fleet_starts_no_more_bots() {
        /// Far more bots than the wait below lets start at one per second.
        const BOTS: usize = 1000;
        const WAIT: Duration = Duration::from_secs(5);
        nw_login::install_crypto_provider();
        let mut fleet = test_fleet(WAIT);
        fleet.bots = BOTS;
        fleet.bots_per_second = 1;
        fleet.duration = None;
        fleet.metrics_urls = Vec::new();
        let (_stop, stopped) = watch::channel(true);
        let new_client: NewClient =
            Arc::new(|_| Err(RepError::Route(crate::route::RouteError::TooShort)));
        let run = tokio::time::timeout(
            WAIT,
            run_fleet(
                &Client::new(),
                Arc::new(fleet),
                new_client,
                Arc::new(|_| Idle),
                stopped,
                |_| {},
            ),
        )
        .await
        .expect("a stopped fleet ends");
        assert!(run.bots.is_empty(), "{} bots started", run.bots.len());
    }

    #[test]
    fn bot_names_are_letters_and_unique() {
        let names: Vec<(String, String)> = (0..MAX_BOTS)
            .map(|n| bot_names("bot", "Swarm", n))
            .collect();
        assert_eq!(names[0], ("bot00000".to_owned(), "Swarmaaa".to_owned()));
        assert_eq!(names[27].1, "Swarmabb");
        let characters: BTreeSet<&String> = names.iter().map(|(_, character)| character).collect();
        let accounts: BTreeSet<&String> = names.iter().map(|(account, _)| account).collect();
        assert_eq!((characters.len(), accounts.len()), (MAX_BOTS, MAX_BOTS));
        assert!(
            characters
                .iter()
                .all(|name| name.bytes().all(|byte| byte.is_ascii_alphabetic()))
        );
    }

    const TICK: Duration = Duration::from_millis(33);

    #[test]
    fn bundles_are_counted_against_the_ticks() {
        let mut clock = BundleClock::new();
        let second = Duration::from_secs(1);
        clock.saw(1, second);
        for tick in 1..TICKS_PER_SECOND {
            clock.saw(1, second + TICK * tick as u32);
        }
        let end = second + TICK * (TICKS_PER_SECOND - 1) as u32;
        assert_eq!(clock.bundles(end), (TICKS_PER_SECOND, TICKS_PER_SECOND));
        assert!(clock.max_gap(end) <= TICK);
        // The bundles stop halfway through the run: they count as lost, and as a gap.
        let later = end + Duration::from_secs(1);
        let (received, expected) = clock.bundles(later);
        assert_eq!(received, TICKS_PER_SECOND);
        assert!(expected >= 2 * TICKS_PER_SECOND - 1);
        assert!(clock.max_gap(later) >= Duration::from_secs(1));
        clock.saw(0, later * 2);
        assert_eq!(
            clock.bundles(later).0,
            TICKS_PER_SECOND,
            "no bundle, no count"
        );
    }

    #[test]
    fn bot_errors_are_counted_by_kind() {
        const REFUSED: &str = "no registration response";
        const QUEUE: &str = "login queue timed out";
        let kinds: Vec<(&str, usize)> = error_kinds([REFUSED, QUEUE, REFUSED].into_iter())
            .into_iter()
            .collect();
        assert_eq!(kinds, vec![(QUEUE, 1), (REFUSED, 2)]);
        assert!(error_kinds(std::iter::empty()).is_empty());
    }

    #[test]
    fn the_udp_receive_errors_are_read() {
        let snmp = "Ip: Forwarding DefaultTTL\nIp: 1 64\n\
                    Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors\n\
                    Udp: 147 1 50 123 49 0\n";
        assert_eq!(udp_receive_errors(snmp), Some(49));
        assert_eq!(udp_receive_errors("nothing"), None);
    }

    #[test]
    fn sim_totals_come_from_the_metrics_and_their_change() {
        let text = format!(
            "{METRIC_HANDOFFS}{{{RESULT_LABEL}=\"{HANDOFF_MOVED}\"}} 10\n\
             {METRIC_HANDOFFS}{{{RESULT_LABEL}=\"{HANDOFF_TIMEOUT}\"}} 1\n\
             {METRIC_HANDOFF_SECONDS}_bucket{{le=\"0.033\"}} 9\n\
             {METRIC_HANDOFF_SECONDS}_count 11\n\
             {METRIC_REFUSED_MOVES}{{reason=\"too_fast\"}} 2\n\
             {METRIC_REFUSED_MOVES}{{reason=\"dead\"}} 4\n\
             {METRIC_DEATHS}{{{KIND_LABEL}=\"{MOB_KIND}\"}} 5\n\
             {METRIC_DEATHS}{{{KIND_LABEL}=\"player\"}} 3\n"
        );
        let totals = SimTotals::of(&Scrape::parse(&text));
        assert_eq!((totals.moved, totals.failed), (10.0, 1.0));
        assert_eq!(
            (totals.refused_moves, totals.dead_moves),
            (2.0, 4.0),
            "a move that arrives after the death is not a refused live move"
        );
        assert_eq!((totals.handoffs_within_tick, totals.handoffs), (9.0, 11.0));
        assert_eq!(totals.mob_deaths, 5.0, "mob deaths only");
        assert_eq!(totals.since(&totals), SimTotals::default());
    }
}
