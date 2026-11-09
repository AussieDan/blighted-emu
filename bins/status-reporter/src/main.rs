// SPDX-License-Identifier: AGPL-3.0-or-later
//! `status-reporter`: sends one world's status and leaderboards to the public website.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/status-reporter.toml";

#[derive(Parser)]
#[command(
    name = "status-reporter",
    about = "Sends the world status and leaderboards of the Blighted New World emulator to the \
             public website."
)]
struct Cli {
    /// Configuration file. The report secret and the database passwords come from the
    /// variables it names.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match status_reporter::run::serve(&cli.config) {
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
