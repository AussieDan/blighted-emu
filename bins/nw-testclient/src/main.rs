// SPDX-License-Identifier: AGPL-3.0-or-later
//! `nw-testclient`: logs in over HTTPS like the game, then registers with `rep-gateway`.
//! `registration` (the default) stays connected, then checks that the same ticket is refused a
//! second time. `pair` runs two clients in the world (M4 spec section 9). `swarm` runs many bots
//! across a sim border (M5 spec section 11). `hunt` runs many bots that fight mobs (M6-B spec
//! section 9). `chat` says one line in area chat and waits for the server's answer (system lines
//! for a GM chat command such as `!help` of a game master account, else the line itself). Prints one line per step; exits with
//! 0 only when every step passed.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use clap::{Parser, Subcommand};
use nw_content::client_build::load_client_build;
use nw_dtls::DtlsClientConfig;
use nw_reflect::REGISTRATION_STATUS_AUTHORIZED;
use nw_testclient::chat::{ChatSettings, run_chat};
use nw_testclient::config::{
    PASSWORD_ENV, SECOND_PASSWORD_ENV, SWARM_PASSWORD_ENV, TestClientConfig, load_testclient_config,
};
use nw_testclient::fleet::{NewClient, udp_receive_errors};
use nw_testclient::hunt::run_hunt;
use nw_testclient::pair::{Check, PairSettings, run_pair};
use nw_testclient::rep::{LedgerSlot, RepClient};
use nw_testclient::swarm::run_swarm;
use nw_testclient::udp::{Hold, run_rep};
use nw_testclient::walk::{WalkSettings, login_token, walk};
use nw_world::WorldFile;
use nw_world::world_file::world_file_path;

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/nw-testclient.toml";
/// The line of `chat` without `--line`: the GM command list.
const DEFAULT_CHAT_LINE: &str = "!help";
/// Bytes read from standard input at a time while waiting for its end.
const INPUT_BUFFER_BYTES: usize = 256;
/// The kernel's network counters (Linux).
const SNMP_PATH: &str = "/proc/net/snmp";

#[derive(Parser)]
#[command(
    name = "nw-testclient",
    about = "Test client of the Blighted New World emulator."
)]
struct Cli {
    /// Configuration file. The password comes from `NW_SECRET_TESTCLIENT_PASSWORD`.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// Writes the plain carrier datagrams of the first client to this capture ledger, which
    /// `blighted-admin` opens like a recorded session.
    #[arg(long, global = true)]
    ledger: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// One client: registration, a hold with pings, and the refused second registration
    /// (the default).
    Registration,
    /// Two clients in the world: each sees the other, and the first logs in again at its saved
    /// position. The second password comes from `NW_SECRET_TESTCLIENT_PASSWORD_B`.
    Pair,
    /// Many bots across a sim border (`[swarm]`). The bot password comes from
    /// `NW_SECRET_TESTCLIENT_SWARM_PASSWORD`.
    Swarm {
        /// Bots of this step instead of `[swarm] bots`.
        #[arg(long)]
        bots: Option<usize>,
    },
    /// Many bots that fight mobs (`[hunt]`, with accounts of their own and the sockets of
    /// `[swarm]`). The bot password comes from `NW_SECRET_TESTCLIENT_SWARM_PASSWORD`.
    Hunt {
        /// Bots of this step instead of `[hunt] bots`.
        #[arg(long)]
        bots: Option<usize>,
    },
    /// One client (the first account) says a line in area chat and waits for the server's
    /// answer: system lines for a GM chat command of a game master, else the line itself.
    Chat {
        /// The line, as typed in the chat box.
        #[arg(long, default_value = DEFAULT_CHAT_LINE)]
        line: String,
    },
}

fn main() -> ExitCode {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => finish(runtime, run(Cli::parse())),
        Err(error) => {
            eprintln!("error: runtime failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs `future` to its end on `runtime`, then drops the runtime without waiting for its
/// blocking tasks: the read of standard input of an until-stopped run ends only with the input,
/// and the program must end after its report.
fn finish<F: Future>(runtime: tokio::runtime::Runtime, future: F) -> F::Output {
    let output = runtime.block_on(future);
    runtime.shutdown_background();
    output
}

async fn run(cli: Cli) -> ExitCode {
    match steps(cli).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn steps(cli: Cli) -> Result<bool, Box<dyn std::error::Error>> {
    let config = load_testclient_config(&cli.config)?;
    nw_login::install_crypto_provider();
    let http = reqwest::Client::builder().build()?;
    let trusted = DtlsClientConfig::new(&std::fs::read(&config.certificate_file)?)?;
    let ledger = Arc::new(LedgerSlot::new(match &cli.ledger {
        Some(path) => Some(nw_capture::LedgerWriter::new(
            Box::new(std::io::BufWriter::new(std::fs::File::create(path)?))
                as Box<dyn std::io::Write + Send>,
        )),
        None => None,
    }));
    match cli.command.unwrap_or(Command::Registration) {
        Command::Registration => {
            registration(&config, &http, &trusted, &own(&config)?, &ledger).await
        }
        Command::Pair => pair(&config, &http, &trusted, own(&config)?, &ledger).await,
        Command::Swarm { bots } => {
            let mut config = config;
            if let Some(bots) = bots {
                config.swarm.bots = bots;
            }
            swarm(&config, &http, trusted, ledger).await
        }
        Command::Hunt { bots } => {
            let mut config = config;
            if let Some(bots) = bots {
                config.hunt.bots = bots;
            }
            hunt(&config, &http, trusted, ledger).await
        }
        Command::Chat { line } => {
            chat(&config, &http, &trusted, own(&config)?, line, &ledger).await
        }
    }
}

/// The test account of `registration` and `pair`.
fn own(config: &TestClientConfig) -> Result<WalkSettings, String> {
    let password = secret(PASSWORD_ENV)?;
    Ok(walk_settings(
        config,
        &config.account,
        &config.character_name,
        password,
    ))
}

async fn swarm(
    config: &TestClientConfig,
    http: &reqwest::Client,
    trusted: DtlsClientConfig,
    ledger: Arc<LedgerSlot>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let settings = Arc::new(config.swarm_settings(secret(SWARM_PASSWORD_ENV)?)?);
    let (ground, new_client) = fleet_parts(config, trusted, ledger)?;
    let errors_before = receive_errors();
    let stop = stop_signal(settings.fleet.duration.is_none());
    let report = run_swarm(
        http,
        Arc::clone(&settings),
        ground,
        new_client,
        stop,
        |line| {
            println!("{line}");
        },
    )
    .await;
    println!("{}", report.summary());
    Ok(print_end(
        report.error_kinds(),
        errors_before,
        &report.checks(&settings),
    ))
}

async fn hunt(
    config: &TestClientConfig,
    http: &reqwest::Client,
    trusted: DtlsClientConfig,
    ledger: Arc<LedgerSlot>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let settings = Arc::new(config.hunt_settings(secret(SWARM_PASSWORD_ENV)?)?);
    let (ground, new_client) = fleet_parts(config, trusted, ledger)?;
    let errors_before = receive_errors();
    let stop = stop_signal(settings.fleet.duration.is_none());
    let report = run_hunt(
        http,
        Arc::clone(&settings),
        ground,
        new_client,
        stop,
        |line| {
            println!("{line}");
        },
    )
    .await;
    println!("{}", report.summary());
    Ok(print_end(
        report.error_kinds(),
        errors_before,
        &report.checks(&settings),
    ))
}

/// The stop of a run without a duration: it turns `true` on Ctrl-C or when standard input
/// reaches its end (the admin panel closes it to stop a batch). A timed run never stops early.
fn stop_signal(until_stopped: bool) -> watch::Receiver<bool> {
    let (stop, stopped) = watch::channel(false);
    if until_stopped {
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                () = input_end() => {}
            }
            println!("stopping: every bot closes");
            // The receivers live as long as the run.
            let _ = stop.send(true);
        });
    }
    stopped
}

/// Waits until standard input reaches its end or fails.
async fn input_end() {
    let mut input = tokio::io::stdin();
    let mut buffer = [0_u8; INPUT_BUFFER_BYTES];
    while matches!(input.read(&mut buffer).await, Ok(read) if read > 0) {}
}

/// The ground of the `[swarm]` level and the maker of bot clients.
fn fleet_parts(
    config: &TestClientConfig,
    trusted: DtlsClientConfig,
    ledger: Arc<LedgerSlot>,
) -> Result<(Arc<WorldFile>, NewClient), Box<dyn std::error::Error>> {
    let swarm = &config.swarm;
    let build = load_client_build(&swarm.client_build_config)?;
    let path = world_file_path(&swarm.content_dir, &build, &swarm.level);
    let ground = Arc::new(WorldFile::load(&path, &build)?);
    let mtu = config.mtu;
    let trusted = Arc::new(trusted);
    let new_client: NewClient = Arc::new(move |token| {
        RepClient::new(&trusted, mtu, token, 0).map(|client| ledger.give(client))
    });
    Ok((ground, new_client))
}

/// Prints the bot errors, the PC's UDP drops since `errors_before` and the checks; returns
/// whether every check passed.
fn print_end(
    error_kinds: std::collections::BTreeMap<&str, usize>,
    errors_before: Option<u64>,
    checks: &[Check],
) -> bool {
    for (error, bots) in error_kinds {
        println!("{bots} bots failed to start: {error}");
    }
    match (errors_before, receive_errors()) {
        (Some(before), Some(after)) => println!(
            "UDP receive buffer errors of the PC during the step: {}",
            after.saturating_sub(before)
        ),
        _ => println!("UDP receive buffer errors of the PC: Unknown"),
    }
    print_checks(checks)
}

/// Prints each check; returns whether every check passed.
fn print_checks(checks: &[Check]) -> bool {
    for check in checks {
        let result = if check.passed { "pass" } else { "FAIL" };
        println!("{}: {result}", check.name);
    }
    checks.iter().all(|check| check.passed)
}

/// The PC's count of UDP datagrams dropped for a full receive buffer (Linux).
fn receive_errors() -> Option<u64> {
    let snmp = std::fs::read_to_string(SNMP_PATH).ok()?;
    udp_receive_errors(&snmp)
}

fn secret(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn walk_settings(
    config: &TestClientConfig,
    account: &str,
    character_name: &str,
    password: String,
) -> WalkSettings {
    WalkSettings {
        login_url: config.login_url.clone(),
        game_url: config.game_url.clone(),
        account: account.to_owned(),
        password,
        character_name: character_name.to_owned(),
    }
}

async fn pair(
    config: &TestClientConfig,
    http: &reqwest::Client,
    trusted: &DtlsClientConfig,
    first: WalkSettings,
    ledger: &LedgerSlot,
) -> Result<bool, Box<dyn std::error::Error>> {
    let second = walk_settings(
        config,
        &config.second_account,
        &config.second_character_name,
        secret(SECOND_PASSWORD_ENV)?,
    );
    let settings = PairSettings {
        first,
        second,
        route: config.route()?,
        jump_m: config.jump_m,
        hold: Duration::from_secs(config.hold_seconds),
        registration_timeout: Duration::from_secs(config.registration_timeout_seconds),
    };
    let report = run_pair(
        http,
        &settings,
        |token| RepClient::new(trusted, config.mtu, token, 0).map(|client| ledger.give(client)),
        |line| println!("{line}"),
    )
    .await?;
    let tolerances = (config.path_tolerance_m, config.position_tolerance_m);
    Ok(print_checks(&report.checks(tolerances.0, tolerances.1)))
}

async fn chat(
    config: &TestClientConfig,
    http: &reqwest::Client,
    trusted: &DtlsClientConfig,
    account: WalkSettings,
    line: String,
    ledger: &LedgerSlot,
) -> Result<bool, Box<dyn std::error::Error>> {
    let settings = ChatSettings {
        account,
        line,
        hold: Duration::from_secs(config.hold_seconds),
        registration_timeout: Duration::from_secs(config.registration_timeout_seconds),
    };
    let report = run_chat(
        http,
        &settings,
        |token| RepClient::new(trusted, config.mtu, token, 0).map(|client| ledger.give(client)),
        |line| println!("{line}"),
    )
    .await?;
    for answer in &report.answers {
        println!("answer: {answer}");
    }
    if report.echoed {
        println!("the line came back as area chat");
    }
    Ok(print_checks(&report.checks()))
}

async fn registration(
    config: &TestClientConfig,
    http: &reqwest::Client,
    trusted: &DtlsClientConfig,
    settings: &WalkSettings,
    ledger: &LedgerSlot,
) -> Result<bool, Box<dyn std::error::Error>> {
    let admission = walk(http, settings, |line| println!("{line}")).await?;
    let token = login_token(&admission.token);
    let registration_timeout = Duration::from_secs(config.registration_timeout_seconds);
    let new_client =
        || RepClient::new(trusted, config.mtu, token.clone(), 0).map(|client| ledger.give(client));
    let outcome = run_rep(
        admission.rep_address,
        new_client()?,
        Hold::For(Duration::from_secs(config.hold_seconds)),
        registration_timeout,
    )
    .await?;
    let Some(status) = outcome.status else {
        println!("no registration response");
        return Ok(false);
    };
    println!(
        "registration status {status}, server version {}",
        outcome.server_version.as_deref().unwrap_or_default()
    );
    println!(
        "held {:.1} s: {} pings echoed, closed by server: {}",
        outcome.held.as_secs_f64(),
        outcome.pings,
        outcome.closed_by_server
    );
    // The ticket is single-use: the same token must be refused now.
    let again = run_rep(
        admission.rep_address,
        new_client()?,
        Hold::For(Duration::ZERO),
        registration_timeout,
    )
    .await?;
    let Some(second_status) = again.status else {
        println!("no response to the second registration");
        return Ok(false);
    };
    println!("second registration with the same ticket: status {second_status}");
    Ok(status == REGISTRATION_STATUS_AUTHORIZED
        && outcome.pings_kept_up()
        && !outcome.closed_by_server
        && second_status != REGISTRATION_STATUS_AUTHORIZED)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Longer than any end of a run may wait.
    const BLOCKED: Duration = Duration::from_secs(10);
    const QUICK: Duration = Duration::from_secs(2);

    #[test]
    fn a_waiting_input_read_does_not_hold_the_end() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .build()
            .expect("runtime");
        let started = Instant::now();
        let code = finish(runtime, async {
            // Stands for the read of standard input, which waits until the input ends.
            tokio::task::spawn_blocking(|| std::thread::sleep(BLOCKED));
            ExitCode::SUCCESS
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    }
}
