// SPDX-License-Identifier: AGPL-3.0-or-later
//! `edge-http`: the HTTPS login server. `serve` runs it; `migrate` updates the databases.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use edge_http::{link_discord, make_mirror_certificate, migrate, serve};

/// The committed example configuration.
const DEFAULT_CONFIG: &str = "config/edge.toml";

#[derive(Parser)]
#[command(
    name = "edge-http",
    about = "Login server of the Blighted New World emulator."
)]
struct Cli {
    /// Configuration file. Secrets come from `NW_SECRET_*` environment variables.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serves HTTPS.
    Serve,
    /// Applies the database migrations of the global and every world database.
    Migrate,
    /// Makes the self-signed certificate of the `[aws_mirror]` hosts. Never replaces a file.
    AwsMirrorCertificate,
    /// Points a Discord user at a password account, so Discord login opens its characters.
    LinkDiscord {
        /// Username of the password account.
        #[arg(long)]
        username: String,
        /// Discord user id (Discord: Developer Mode on, right-click your name, Copy User ID).
        #[arg(long)]
        discord_user_id: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve => serve(&cli.config),
        Command::Migrate => migrate(&cli.config),
        Command::AwsMirrorCertificate => make_mirror_certificate(&cli.config),
        Command::LinkDiscord {
            username,
            discord_user_id,
        } => link_discord(&cli.config, &username, &discord_user_id),
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
