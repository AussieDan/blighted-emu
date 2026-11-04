// SPDX-License-Identifier: AGPL-3.0-or-later
//! `blighted-admin`: local admin web tools.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nw_db::AdminRank;

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/blighted-admin.toml";

#[derive(Parser)]
#[command(
    name = "blighted-admin",
    about = "Local admin web tools of the Blighted New World emulator."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serves the admin pages until Ctrl-C (local mode: loopback; server mode: HTTPS).
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
    /// Removes the admin rank of a game account.
    RevokeRank {
        /// Configuration file.
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        /// The account's login name, Discord user id or Discord username.
        username: String,
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
