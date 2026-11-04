// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: reads the configuration, the world file and the content bundle names, starts the
//! live readers when `[live]` is set, and serves the admin pages until Ctrl-C.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use nw_db::{AdminRank, PgPool};
use uuid::Uuid;
use nw_login::config::TlsConfig;
use nw_login::discord::DiscordHttp;
use nw_login::install_crypto_provider;
use nw_login::password::PasswordHasher;

use nw_common::logging::{LoggingError, init_logging};
use nw_content::client_build::load_client_build;
use tokio::sync::watch;

use crate::access::discord::{AdminDiscord, DiscordClient};
use crate::access::store::Store;
use crate::access::{Access, AccessMode};
use crate::captures::Captures;
use crate::config::{AccessConfig, AdminConfig, AdminConfigError, load_admin_config};
use crate::gm::Commands;
use crate::guard::AdminOrigin;
use crate::live;
use crate::server::{AppState, router};
use crate::studio::{LoadWorld, Studio};
use crate::world::{WorldLayer, WorldLayerError, map_dir};

/// Environment variable that names the New World install folder (for `typeindex.json`).
pub const GAME_DIR_ENV: &str = "NW_GAME_DIR";

/// `blighted-admin serve` failed.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration cannot be used.
    #[error(transparent)]
    Config(#[from] AdminConfigError),
    /// Logging cannot start.
    #[error(transparent)]
    Logging(#[from] LoggingError),
    /// The studio cannot start.
    #[error("studio: {0}")]
    Studio(String),
    /// The pinned client build cannot be read.
    #[error("client build: {0}")]
    ClientBuild(#[source] nw_common::config::ConfigError),
    /// The world layer cannot be loaded.
    #[error(transparent)]
    World(#[from] WorldLayerError),
    /// The runtime, the listener or the server failed.
    #[error("server failed")]
    Io(#[source] std::io::Error),
    /// The variable named by `global_database_password_env` is not set.
    #[error("the variable {0} with the global database password is not set")]
    MissingSecret(String),
    /// The global database cannot be reached or is not migrated (`edge-http migrate`).
    #[error("global database: {0}")]
    Database(String),
    /// The password hasher cannot start.
    #[error(transparent)]
    Password(#[from] nw_login::password::PasswordError),
    /// The variable named by `[access.discord] client_secret_env` is not set.
    #[error("the variable {0} with the Discord client secret is not set")]
    MissingDiscordSecret(String),
    /// The Discord client cannot start.
    #[error(transparent)]
    Discord(#[from] nw_login::discord::DiscordError),
    /// The TLS certificate or key cannot be read (server mode).
    #[error("TLS certificate or key")]
    Tls(#[source] std::io::Error),
}

/// Time the HTTPS server gives open requests after Ctrl-C.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Parallel password hashes in server mode (0: one per CPU).
const HASH_PARALLEL: usize = 0;

/// Serves the admin pages of `config_path` until Ctrl-C.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration or the data cannot be read, or the server fails.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_admin_config(config_path)?;
    init_logging(&config.log_filter)?;
    let world = WorldLayer::load(&config)?;
    let game_dir = std::env::var_os(GAME_DIR_ENV).map(std::path::PathBuf::from);
    let captures = Captures::load(&config.captures, game_dir.as_deref());
    let map = map_dir(&config);
    let client_build =
        load_client_build(&config.client_build_config).map_err(RunError::ClientBuild)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Io)?
        .block_on(async {
            let access = access_of(&config).await?;
            let (close, closing) = watch::channel(false);
            let live = config.live.as_ref().map(|live| live::start(live, closing));
            let live_state = live.as_ref().map(|live| live.state().clone());
            let state = AppState::new(world, map, config.listen, captures, live);
            let reload_config = Arc::new(config.clone());
            let load_world: LoadWorld = Arc::new(move || {
                WorldLayer::load(&reload_config).map_err(|error| error.to_string())
            });
            let studio = config.studio.as_ref().map(|studio| {
                Studio::new(
                    studio,
                    config.content_dir.clone(),
                    &client_build,
                    live_state,
                )
                .map(|studio| studio.with_world_reload(state.world.clone(), load_world))
            });
            let studio = studio.transpose().map_err(RunError::Studio)?;
            let state = match studio {
                Some(studio) => state.with_studio(studio),
                None => state,
            };
            let state = state.with_access(access, origin_of(&config));
            let state = match &config.live {
                Some(live) => {
                    state.with_commands(Commands::over_bus(&live.nats_url, live.world_id))
                }
                None => state,
            };
            if let Some(tls) = config
                .access
                .as_ref()
                .and_then(|access| access.tls.as_ref())
            {
                return serve_https(config.listen, tls, router(state), close).await;
            }
            let listener = tokio::net::TcpListener::bind(config.listen)
                .await
                .map_err(RunError::Io)?;
            tracing::info!(listen = %config.listen, "serving");
            axum::serve(listener, router(state))
                .with_graceful_shutdown(async move {
                    // A failed signal handler means the server runs until killed.
                    let _ = tokio::signal::ctrl_c().await;
                    // The live streams never end by themselves; the server waits for them.
                    let _ = close.send(true);
                })
                .await
                .map_err(RunError::Io)
        })
}

/// An account command (a rank change, Marks of Fortune) could not be made.
#[derive(Debug, thiserror::Error)]
pub enum AccountCommandError {
    /// The configuration cannot be used.
    #[error(transparent)]
    Config(#[from] AdminConfigError),
    /// `[access]` names no global database.
    #[error("[access] needs global_database and global_database_password_env")]
    NoDatabase,
    /// The variable with the password is not set.
    #[error("the variable {0} with the global database password is not set")]
    MissingSecret(String),
    /// The database failed.
    #[error("global database: {0}")]
    Database(String),
    /// No account has this login name, Discord user id or Discord username.
    #[error("no account has the name {0}")]
    UnknownUser(String),
    /// The runtime cannot start.
    #[error("runtime")]
    Io(#[source] std::io::Error),
}

/// Runs `job` on the account named `username` (a login name, a Discord user id or a Discord
/// username) in the global database that the configuration at `config_path` names.
fn on_account<T>(
    config_path: &Path,
    username: &str,
    job: impl AsyncFnOnce(&PgPool, Uuid) -> Result<T, nw_db::DbError>,
) -> Result<T, AccountCommandError> {
    let config = load_admin_config(config_path)?;
    let (database, password_env) = config
        .access
        .as_ref()
        .and_then(|access| {
            access
                .global_database
                .as_ref()
                .zip(access.global_database_password_env.as_ref())
        })
        .ok_or(AccountCommandError::NoDatabase)?;
    let password = std::env::var(password_env)
        .map_err(|_| AccountCommandError::MissingSecret(password_env.clone()))?;
    let username = username.trim().to_owned();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(AccountCommandError::Io)?
        .block_on(async {
            let database_error =
                |error: &dyn std::fmt::Display| AccountCommandError::Database(error.to_string());
            let pool = nw_db::connect(database, &password)
                .await
                .map_err(|error| database_error(&error))?;
            let account = nw_db::account_by_login_name(&pool, &username)
                .await
                .map_err(|error| database_error(&error))?
                .ok_or_else(|| AccountCommandError::UnknownUser(username.clone()))?;
            job(&pool, account.id)
                .await
                .map_err(|error| database_error(&error))
        })
}

/// Gives the account named `username` the rank `rank`, or removes its rank when `rank` is
/// `None` (`blighted-admin grant-rank` / `revoke-rank`). Returns `false` when there was no rank
/// to remove.
pub fn change_rank(
    config_path: &Path,
    username: &str,
    rank: Option<AdminRank>,
) -> Result<bool, AccountCommandError> {
    on_account(config_path, username, async |pool, account| match rank {
        Some(rank) => nw_db::set_admin_rank(pool, account, rank, None)
            .await
            .map(|()| true),
        None => nw_db::remove_admin_rank(pool, account).await,
    })
}

/// Adds `amount` Marks of Fortune, the store currency, to the wallet of the account named
/// `username` (a negative amount takes them; a wallet never goes below 0) and returns the
/// wallet (`blighted-admin give-marks`).
pub fn give_marks(
    config_path: &Path,
    username: &str,
    amount: i64,
) -> Result<nw_db::Wallet, AccountCommandError> {
    on_account(config_path, username, async |pool, account| {
        nw_db::give_marks(pool, account, amount).await
    })
}

/// The access of `config`: local mode without login, or server mode with the global database,
/// the password hasher and, with `[access.discord]`, Discord login. A configured global database
/// also keeps the audit log in local mode.
async fn access_of(config: &AdminConfig) -> Result<Access, RunError> {
    let mode = AccessConfig::mode_of(config.access.as_ref());
    let database = config.access.as_ref().and_then(|access| {
        access
            .global_database
            .as_ref()
            .zip(access.global_database_password_env.as_ref())
    });
    let store = match database {
        Some((database, password_env)) => {
            let password = std::env::var(password_env)
                .map_err(|_| RunError::MissingSecret(password_env.clone()))?;
            let pool = nw_db::connect(database, &password)
                .await
                .map_err(|error| RunError::Database(error.to_string()))?;
            nw_db::check_schema(&pool, &nw_db::GLOBAL_MIGRATOR)
                .await
                .map_err(|error| RunError::Database(error.to_string()))?;
            Some(Store::Postgres(pool))
        }
        None => None,
    };
    let hasher = match mode {
        AccessMode::Server => Some(PasswordHasher::new(HASH_PARALLEL)?),
        AccessMode::Local => None,
    };
    let access = Access::new(mode, store, hasher);
    match config
        .access
        .as_ref()
        .and_then(|access| access.discord.as_ref().zip(access.hosts.first()))
    {
        Some((discord, host)) => {
            let secret = std::env::var(&discord.client_secret_env)
                .map_err(|_| RunError::MissingDiscordSecret(discord.client_secret_env.clone()))?;
            install_crypto_provider();
            let client = DiscordClient::Http(DiscordHttp::new(discord.client_id.clone(), secret)?);
            Ok(access.with_discord(AdminDiscord::new(client, discord.client_id.clone(), host)))
        }
        None => Ok(access),
    }
}

/// The request checks of `config`: the `[access] hosts` over HTTPS in server mode, else the
/// listen address.
fn origin_of(config: &AdminConfig) -> AdminOrigin {
    match config.access.as_ref() {
        Some(access) if access.mode == AccessMode::Server => AdminOrigin::server(&access.hosts),
        _ => AdminOrigin::of(config.listen),
    }
}

/// The TLS settings of `tls`; a missing or bad file is an error, never plain HTTP.
pub async fn tls_config(tls: &TlsConfig) -> Result<RustlsConfig, RunError> {
    install_crypto_provider();
    RustlsConfig::from_pem_file(&tls.certificate_pem_path, &tls.private_key_pem_path)
        .await
        .map_err(RunError::Tls)
}

/// Serves `app` over HTTPS on `listen` until Ctrl-C (server mode).
async fn serve_https(
    listen: SocketAddr,
    tls: &TlsConfig,
    app: axum::Router,
    close: watch::Sender<bool>,
) -> Result<(), RunError> {
    let tls = tls_config(tls).await?;
    let handle = Handle::new();
    let shutdown = handle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = close.send(true);
            shutdown.graceful_shutdown(Some(SHUTDOWN_GRACE));
        }
    });
    tracing::info!(%listen, "serving HTTPS");
    axum_server::bind_rustls(listen, tls)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .map_err(RunError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_certificate_refuses_to_start() {
        let tls = TlsConfig {
            certificate_pem_path: "no-such-cert.pem".into(),
            private_key_pem_path: "no-such-key.pem".into(),
        };
        assert!(matches!(tls_config(&tls).await, Err(RunError::Tls(_))));
    }
}
