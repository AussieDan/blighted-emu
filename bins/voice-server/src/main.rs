// SPDX-License-Identifier: AGPL-3.0-or-later
//! `voice-server`: the community's voice service for the game's voice SDK.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/voice-server.toml";

#[derive(Parser)]
#[command(
    name = "voice-server",
    about = "Voice service of the Blighted New World emulator."
)]
struct Cli {
    /// Configuration file.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match voice_server::serve(&cli.config) {
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
