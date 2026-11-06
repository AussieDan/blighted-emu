// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sim`: the world simulation of one cell.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/sim.toml";

#[derive(Parser)]
#[command(
    name = "sim",
    about = "World simulation of the Blighted New World emulator."
)]
struct Cli {
    /// Configuration file. The world database password comes from the variable it names.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serves the level until Ctrl-C (the default).
    Serve,
    /// Plays every quest of the level with a test character in a world of its own (no database,
    /// no bus) and writes which quests end and where each other one stops.
    QuestWalk {
        /// The report file (Markdown).
        #[arg(long)]
        report: PathBuf,
        /// Only the quests whose id starts with this text (any case).
        #[arg(long)]
        only: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => sim::run::serve(&cli.config),
        Command::QuestWalk { report, only } => {
            sim::quest_walk::walk(&cli.config, &report, only.as_deref())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            let mut cause = std::error::Error::source(&error);
            while let Some(inner) = cause {
                eprintln!("  caused by: {inner}");
                cause = inner.source();
            }
            ExitCode::FAILURE
        }
    }
}
