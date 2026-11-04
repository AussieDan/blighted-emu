// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: reads the configuration and the world file, connects to the world database, Valkey
//! and NATS, and runs the loop until Ctrl-C.

use std::path::Path;
use std::time::Duration;

use metrics_exporter_prometheus::PrometheusBuilder;
use nw_bus::{Bus, BusError};
use nw_common::config::ConfigError;
use nw_common::logging::{LoggingError, init_logging};
use nw_content::client_build::load_client_build;
use nw_content::container::ContainerError;
use nw_db::{DbError, GLOBAL_MIGRATOR, WORLD_MIGRATOR, check_schema};
use nw_login::store::StoreError;
use nw_login::valkey::Valkey;
use nw_sim_core::{CellGrid, CellGridError};
use nw_world::WorldFile;
use nw_world::world_file::world_file_path;

use crate::config::{DirectorConfig, DirectorConfigError, InstancesConfig, load_director_config};
use crate::instances::{InstanceSettings, LaunchSettings};
use crate::server::{DirectorSettings, ServeError, serve as serve_loop};

/// Failure while starting or running `world-director`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration file is missing or not valid.
    #[error(transparent)]
    Config(#[from] DirectorConfigError),
    /// The client build file cannot be read.
    #[error("client build file: {0}")]
    ClientBuild(#[source] ConfigError),
    /// The world file cannot be read.
    #[error("world file: {0}")]
    WorldFile(#[from] ContainerError),
    /// The cell size does not fit the level.
    #[error("cell grid: {0}")]
    Grid(#[from] CellGridError),
    /// The database password variable is missing or empty.
    #[error("environment variable {0} is not set")]
    MissingSecret(String),
    /// Logging could not start.
    #[error(transparent)]
    Logging(#[from] LoggingError),
    /// The async runtime could not start.
    #[error("runtime failed")]
    Runtime(#[source] std::io::Error),
    /// The world database cannot be reached.
    #[error("database connection failed")]
    Database(#[source] sqlx::Error),
    /// The world database schema is not current.
    #[error(transparent)]
    Schema(#[from] DbError),
    /// Valkey cannot be reached.
    #[error("Valkey connection failed")]
    Valkey(#[source] StoreError),
    /// NATS cannot be reached.
    #[error(transparent)]
    Bus(#[from] BusError),
    /// The loop failed.
    #[error(transparent)]
    Serve(#[from] ServeError),
    /// The Prometheus listener could not start.
    #[error("metrics listener failed: {0}")]
    Metrics(String),
}

/// Longest wait for a connection of the global database (the admin ranks).
const RANK_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(250);

/// Runs `world-director` until Ctrl-C.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, the world file, the start position, a secret,
/// the database, Valkey, NATS, or the loop fails.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_director_config(config_path)?;
    let password = std::env::var(&config.world_database_password_env)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RunError::MissingSecret(config.world_database_password_env.clone()))?;
    init_logging(&config.log_filter)?;
    let build = load_client_build(&config.client_build_config).map_err(RunError::ClientBuild)?;
    let path = world_file_path(&config.content_dir, &build, &config.level);
    let world_file = WorldFile::load(&path, &build)?;
    let settings = DirectorSettings {
        world_id: config.world_id,
        level: config.level.clone(),
        starts: config.starts(&world_file)?,
        grid: CellGrid::of_level(&world_file.level, config.cell_size_m)?,
        admit_timeout: Duration::from_millis(config.admit_timeout_ms),
        heartbeat_interval: Duration::from_millis(config.heartbeat_interval_ms),
        heartbeat_misses: config.heartbeat_misses,
        instances: config.instances.as_ref().map(instance_settings),
    };
    // The heights are needed only for the start position.
    drop(world_file);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Runtime)?
        .block_on(serve_async(config, password, settings))
}

/// The instance rules of the `[instances]` section.
fn instance_settings(config: &InstancesConfig) -> InstanceSettings {
    InstanceSettings {
        empty_timeout: Duration::from_millis(config.empty_timeout_ms),
        open_timeout: Duration::from_millis(config.open_timeout_ms),
        group_timeout: Duration::from_millis(config.group_timeout_ms),
        launch: config.launch.as_ref().map(|launch| LaunchSettings {
            program: launch.program.clone(),
            args: launch.args.clone(),
            spare_per_level: launch.spare_per_level,
            max_sims: launch.max_sims,
            id_prefix: launch.id_prefix.clone(),
            forget_after: Duration::from_millis(launch.forget_after_ms),
        }),
    }
}

async fn serve_async(
    config: DirectorConfig,
    password: String,
    settings: DirectorSettings,
) -> Result<(), RunError> {
    let db = nw_db::connect(&config.world_database, &password)
        .await
        .map_err(RunError::Database)?;
    check_schema(&db, &WORLD_MIGRATOR).await?;
    let ranks = match &config.global_database {
        Some(global) => {
            let password = std::env::var(&global.password_env)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| RunError::MissingSecret(global.password_env.clone()))?;
            // Lazy: the admin ranks are optional, so a global database that is down at start
            // gives no GM rights instead of stopping the world.
            let pool = nw_db::connect_lazy(&global.database, &password, RANK_ACQUIRE_TIMEOUT)
                .map_err(RunError::Database)?;
            if let Err(error) = check_schema(&pool, &GLOBAL_MIGRATOR).await {
                tracing::warn!(%error, "global database not ready; no GM rights until it is");
            }
            Some(pool)
        }
        None => None,
    };
    let store = Valkey::connect(&config.valkey_url)
        .await
        .map_err(RunError::Valkey)?;
    let bus = Bus::connect(&config.nats_url).await?;
    PrometheusBuilder::new()
        .with_http_listener(config.metrics_listen)
        .install()
        .map_err(|error| RunError::Metrics(error.to_string()))?;
    // The trading post's orders are the world's; one director expires them for every sim.
    tokio::spawn(crate::trading_post::sweep_expired_orders(
        db.clone(),
        crate::trading_post::EXPIRY_SWEEP_INTERVAL,
    ));
    tracing::info!(world = %config.world_id, level = %config.level, "serving");
    let shutdown = async {
        // A failed signal handler means the director runs until killed.
        let _ = nw_common::shutdown::stop_signal().await;
    };
    Ok(serve_loop(settings, bus, db, ranks, store, shutdown).await?)
}
