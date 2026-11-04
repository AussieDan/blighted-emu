// SPDX-License-Identifier: AGPL-3.0-or-later
//! `blighted-admin`: local admin web tools.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nw_db::AdminRank;

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/blighted-admin.toml";
/// The committed example relay configuration.
const DEFAULT_RELAY_CONFIG: &str = "config/blighted-admin-relay.toml";
/// Where `relay-keys` writes by default.
const DEFAULT_RELAY_KEYS_DIR: &str = "local-data/admin-relay";

#[derive(Parser)]
#[command(
    name = "blighted-admin",
    about = "Admin web tools of the Blighted New World emulator."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serves the admin pages until Ctrl-C (local mode: loopback; server mode: HTTPS, or through
    /// the relay of `[access.relay]`).
    Serve {
        /// Configuration file.
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Gives a game account an admin rank: csr, game_master or head_admin (server mode).
    GrantRank {
        /// Configuration file (its `[access]` names the global database).
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        /// The account's login name, Discord user id or Discord username.
        username: String,
        /// The rank.
        #[arg(value_parser = parse_rank)]
        rank: AdminRank,
    },
    /// Runs the admin relay until Ctrl-C, on a machine other than the game servers: it takes the
    /// browsers and the connection of the admin site on the game machine.
    Relay {
        /// Relay configuration file.
        #[arg(long, default_value = DEFAULT_RELAY_CONFIG)]
        config: PathBuf,
    },
    /// Makes the relay and agent certificates (relay.pem, relay-key.pem, agent.pem,
    /// agent-key.pem). Existing files are never replaced.
    RelayKeys {
        /// The DNS name the admin site uses to reach the relay (`[access.relay] server_name`).
        #[arg(long)]
        relay_name: String,
        /// Folder for the four files.
        #[arg(long, default_value = DEFAULT_RELAY_KEYS_DIR)]
        out: PathBuf,
    },
    /// Removes the admin rank of a game account.
    RevokeRank {
        /// Configuration file.
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        /// The account's login name, Discord user id or Discord username.
        username: String,
    },
    /// Gives a game account Marks of Fortune, the in-game store's currency (a negative amount
    /// takes them; a wallet never goes below 0).
    GiveMarks {
        /// Configuration file (its `[access]` names the global database).
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        /// The account's login name, Discord user id or Discord username.
        username: String,
        /// How many Marks of Fortune.
        #[arg(allow_hyphen_values = true)]
        amount: i64,
    },
}

fn parse_rank(name: &str) -> Result<AdminRank, String> {
    AdminRank::from_name(name).ok_or_else(|| "csr, game_master or head_admin".to_owned())
}

fn report(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("error: {error}");
    let mut cause = error.source();
    while let Some(inner) = cause {
        eprintln!("  caused by: {inner}");
        cause = inner.source();
    }
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Serve { config } => match blighted_admin::run::serve(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(&error),
        },
        Command::Relay { config } => match blighted_admin::run::relay(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(&error),
        },
        Command::RelayKeys { relay_name, out } => {
            match blighted_admin::run::relay_keys(&relay_name, &out) {
                Ok(()) => {
                    println!(
                        "wrote the relay and agent certificates to {}",
                        out.display()
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => report(&error),
            }
        }
        Command::GrantRank {
            config,
            username,
            rank,
        } => match blighted_admin::run::change_rank(&config, &username, Some(rank)) {
            Ok(_) => {
                println!("{username} is now {}", rank.name());
                ExitCode::SUCCESS
            }
            Err(error) => report(&error),
        },
        Command::GiveMarks {
            config,
            username,
            amount,
        } => match blighted_admin::run::give_marks(&config, &username, amount) {
            Ok(wallet) => {
                println!("{username} has {} Marks of Fortune now", wallet.marks);
                ExitCode::SUCCESS
            }
            Err(error) => report(&error),
        },
        Command::RevokeRank { config, username } => {
            match blighted_admin::run::change_rank(&config, &username, None) {
                Ok(true) => {
                    println!("{username} has no admin rank now");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    println!("{username} had no admin rank");
                    ExitCode::SUCCESS
                }
                Err(error) => report(&error),
            }
        }
    }
}
