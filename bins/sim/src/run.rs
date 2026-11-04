// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`: reads the configuration, the world file and the level collision file, connects to the
//! world database and NATS, and runs the loop until Ctrl-C.

use std::path::Path;
use std::sync::Arc;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use nw_ai::AiData;
use nw_bus::{Bus, BusError};
use nw_common::config::ConfigError;
use nw_common::logging::{LoggingError, init_logging};
use nw_content::ContentBundle;
use nw_content::bundle::bundle_path;
use nw_content::client_build::load_client_build;
use nw_content::combat::{CombatData, CombatDataError};
use nw_content::container::ContainerError;
use nw_content::creature_animations::CreatureAnimations;
use nw_content::datatable_overrides::{self, DatatableOverrideError};
use nw_content::ftue_entities::FtueLevel;
use nw_content::gameplay_config::GameplayConfig;
use nw_content::mob_prefabs::MobPrefabs;
use nw_content::player_abilities::PlayerAbilities;
use nw_content::player_attacks::PlayerAttacks;
use nw_content::player_sequences::PlayerSequences;
use nw_content::store::store_table;
use nw_db::{DbError, GLOBAL_MIGRATOR, WORLD_MIGRATOR, check_schema};
use nw_sim_core::{LevelGround, level_bounds};
use nw_world::WorldFile;
use nw_world::camps::{CampSlices, camp_slices_path};
use nw_world::collision::{LevelCollision, collision_file_path};
use nw_world::collision_index::CollisionIndex;
use nw_world::dungeon_scripts::{DungeonScripts, dungeon_scripts_path};
use nw_world::quick_courses::{QuickCoursePaths, quick_courses_path};
use nw_world::roads::{RoadMap, road_file_path};
use nw_world::script::{DesignData, LevelDesign, design_data_path};
use nw_world::water::{WaterMap, water_file_path};
use nw_world::world_file::world_file_path;

use crate::asset_solids::AssetSolids;
use crate::config::{SimConfig, SimConfigError, SimRole, load_sim_config};
use crate::gameplay::{GameplayError, load_gameplay, load_shop};
use crate::mob_ai::{LookAssets, MobAi, SliceVitals, resolve_ai};
use crate::mobs::{SpawnFilter, sim_spawns, spawns_of_level};
use crate::player_content::{MobLooks, PlayerContent};
use crate::server::{
    Databases, LATENCY_BUCKETS_SECONDS, LoopSettings, METRIC_HANDOFF_SECONDS,
    METRIC_SNAPSHOT_SECONDS, METRIC_TICK_PART_SECONDS, METRIC_TICK_SECONDS, ServeError,
    serve as serve_loop,
};

/// Failure while starting or running `sim`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The configuration file is missing or not valid.
    #[error(transparent)]
    Config(#[from] SimConfigError),
    /// The client build file cannot be read.
    #[error("client build file: {0}")]
    ClientBuild(#[source] ConfigError),
    /// The world file cannot be read.
    #[error("world file: {0}")]
    WorldFile(#[from] ContainerError),
    /// The level collision file cannot be read (`content-tool collision` writes it).
    #[error("level collision file: {0}")]
    Collision(#[source] ContainerError),
    /// The gameplay values of the remote config sets cannot be loaded.
    #[error(transparent)]
    Gameplay(#[from] GameplayError),
    /// A datasheet override of the remote config sets cannot be applied.
    #[error(transparent)]
    DatatableOverride(#[from] DatatableOverrideError),
    /// The content bundle cannot be read.
    #[error("content bundle: {0}")]
    Bundle(#[source] ContainerError),
    /// A combat row of the config is not in the bundle.
    #[error(transparent)]
    Combat(#[from] CombatDataError),
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

/// Runs `sim` until Ctrl-C.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, the world file, a secret, the database, NATS, or
/// the loop fails.
pub fn serve(config_path: &Path) -> Result<(), RunError> {
    let config = load_sim_config(config_path)?;
    let passwords = DatabasePasswords {
        world: secret(&config.world_database_password_env)?,
        global: secret(&config.global_database_password_env)?,
    };
    init_logging(&config.log_filter)?;
    let build = load_client_build(&config.client_build_config).map_err(RunError::ClientBuild)?;
    let path = world_file_path(&config.content_dir, &build, &config.level);
    let world_file = Arc::new(WorldFile::load(&path, &build)?);
    let collision_path = collision_file_path(&config.content_dir, &build, &config.level);
    let level_collision =
        LevelCollision::load(&collision_path, &build).map_err(RunError::Collision)?;
    let deferred = level_collision.deferred.clone();
    let quest_volumes = level_collision.volumes.clone();
    let collision = Arc::new(CollisionIndex::new(LevelCollision {
        deferred: Vec::new(),
        ..level_collision
    }));
    let deferred_level = LevelCollision {
        deferred,
        ..LevelCollision::default()
    };
    tracing::info!(
        bodies = collision.body_count(),
        shapes = collision.geometry_count(),
        bytes = collision.memory_bytes(),
        "level collision"
    );
    let roads_path = road_file_path(&config.content_dir, &build, &config.level);
    let level_ground = LevelGround::new(Arc::clone(&world_file), Arc::clone(&collision));
    let water_path = water_file_path(&config.content_dir, &build, &config.level);
    let level_ground = match WaterMap::load(&water_path, &build) {
        Ok(water) => {
            tracing::info!(regions = water.regions.len(), "level water");
            level_ground.with_water(Arc::new(water))
        }
        Err(error) => {
            tracing::warn!(%error, "no water file: the sea level is the only water");
            level_ground
        }
    };
    let ground = match RoadMap::load(&roads_path, &build) {
        Ok(roads) => {
            tracing::info!(regions = roads.regions.len(), "level roads");
            level_ground.with_roads(Arc::new(roads))
        }
        Err(error) => {
            tracing::warn!(%error, "no road file: no road sprint boost");
            level_ground
        }
    };
    let gameplay = load_gameplay(&config.edge_config, config.world_id)?;
    let mut bundle = ContentBundle::load(&bundle_path(&config.content_dir, &build), &build)
        .map_err(RunError::Bundle)?;
    datatable_overrides::apply(&mut bundle.tables, &gameplay.datatable_overrides)?;
    log_gameplay(&gameplay);
    let player_attacks = match PlayerAttacks::load(&config.content_dir, &build) {
        Ok(table) => {
            tracing::info!(stances = table.len(), "player attack table");
            table
        }
        Err(error) => {
            tracing::warn!(%error, "no player attack table: players cannot attack");
            PlayerAttacks::default()
        }
    };
    let player_abilities = match PlayerAbilities::load(&config.content_dir, &build) {
        Ok(table) => {
            tracing::info!(lists = table.lists.len(), "player ability table");
            table
        }
        Err(error) => {
            tracing::warn!(%error, "no player ability table: players cannot use abilities");
            PlayerAbilities::default()
        }
    };
    let player_sequences = match PlayerSequences::load(&config.content_dir, &build) {
        Ok(table) => {
            tracing::info!(stances = table.len(), "player sequence table");
            table
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "no player sequence table: the server starts no attack, block, aim, dodge or jump"
            );
            PlayerSequences::default()
        }
    };
    // Only the combat rows and the player content stay; the rest of the bundle is dropped here.
    let combat = Arc::new(
        CombatData::from_tables(&bundle.tables, &config.combat.rows())?
            .with_player_attacks(player_attacks)
            .with_player_abilities(player_abilities)
            .with_player_sequences(player_sequences),
    );
    let (mob_looks, look_assets, slice_vitals) = match MobPrefabs::load(&config.content_dir, &build)
    {
        Ok(prefabs) => {
            let (looks, skipped) = MobLooks::of(&prefabs);
            tracing::info!(prefabs = looks.len(), skipped, "mob prefab table");
            (
                looks,
                prefabs.default_assets().collect::<LookAssets>(),
                prefabs
                    .iter()
                    .map(|((vitals_id, asset), _)| (asset, vitals_id))
                    .collect::<SliceVitals>(),
            )
        }
        Err(error) => {
            tracing::warn!(%error, "no mob prefab table: mobs are not shown to clients");
            (MobLooks::default(), LookAssets::new(), SliceVitals::new())
        }
    };
    let mob_ai = match MobAi::load(
        &config.content_dir,
        &build,
        config.mob_ai,
        look_assets.clone(),
    ) {
        Ok(ai) => {
            let data = ai.data();
            tracing::info!(
                trees = data.trees.len(),
                archetypes = data.archetypes.len(),
                creatures = data.creatures.len(),
                "AI data"
            );
            ai
        }
        Err(error) => {
            tracing::warn!(%error, "no AI data: every mob stands at its spawn");
            MobAi::new(AiData::default(), config.mob_ai, look_assets)
        }
    }
    .with_slice_vitals(slice_vitals);
    let creature_animations = match CreatureAnimations::load(&config.content_dir, &build) {
        Ok(table) => {
            tracing::info!(creatures = table.len(), "creature animation table");
            table
        }
        Err(error) => {
            tracing::warn!(%error, "no creature animation table: mobs move without animations");
            CreatureAnimations::default()
        }
    };
    let tutorial_level = match FtueLevel::load(&config.content_dir, &build) {
        Ok(level) => {
            tracing::info!(
                statics = level.statics.entities.len(),
                assets = level.assets.len(),
                "tutorial level entities"
            );
            level
        }
        Err(error) => {
            tracing::warn!(%error, "no tutorial level entity table: the tutorial's level entities are not sent");
            FtueLevel::default()
        }
    };
    let mut content = PlayerContent::new(
        &bundle.tables,
        mob_looks,
        creature_animations,
        tutorial_level,
        &world_file,
        gameplay,
        config.loot_limits,
    );
    content.asset_solids =
        AssetSolids::of(&deferred_level, &collision, &content.tutorial_level.assets);
    content.trading_post = config.trading_post.clone();
    content.store = store_table(&bundle.tables, &bundle.text);
    content.shop = load_shop(&config.edge_config)?;
    for entitlement in content.shop.mount_prices.keys() {
        if content.store.mount_of(entitlement).is_none() {
            tracing::warn!(
                entitlement,
                "[shop] mount_prices names no store mount: left out"
            );
        }
    }
    content.start_tools = config.start_tools()?;
    for tool in &content.start_tools {
        if content.kits.item(&tool.item).is_none() {
            tracing::warn!(item = %tool.item, "start tool is no item of the data: left out");
        }
    }
    content.dungeon_scripts =
        match DungeonScripts::load(&dungeon_scripts_path(&config.content_dir, &build)) {
            Ok(scripts) => {
                tracing::info!(game_modes = scripts.game_modes.len(), "dungeon scripts");
                scripts
            }
            Err(error) => {
                tracing::warn!(%error, "no dungeon scripts: instances run without encounters");
                DungeonScripts::default()
            }
        };
    content.camp_slices = match CampSlices::load(&camp_slices_path(&config.content_dir, &build)) {
        Ok(slices) => {
            tracing::info!(
                slices = slices.len(),
                blueprints = content.camps.len(),
                "camp slices"
            );
            slices
        }
        Err(error) => {
            tracing::warn!(%error, "no camp slices: no camp can be placed");
            CampSlices::default()
        }
    };
    content.quick_course_paths =
        match QuickCoursePaths::load(&quick_courses_path(&config.content_dir, &build)) {
            Ok(paths) => {
                tracing::info!(courses = paths.len(), "quick course paths");
                paths
            }
            Err(error) => {
                tracing::warn!(%error, "no quick course paths: no course can be ridden");
                QuickCoursePaths::default()
            }
        };
    tracing::info!(
        tradable_items = content.trading.len(),
        trading_tax = content.trading_post.trading_tax,
        posting_fees = content.trading_post.posting_fees.len(),
        shop_offers = content.shops.len(),
        "trading post and shop rules"
    );
    if content.trading.is_empty() {
        tracing::warn!("no tradable items in the content: the trading post takes no order");
    }
    if content.shops.is_empty() {
        tracing::warn!("no shop offers in the content: the shops sell nothing");
    }
    tracing::info!(
        assets = content.asset_solids.solid_assets(),
        "tutorial asset entities with solids"
    );
    content.quest_volumes = quest_volumes;
    tracing::info!(volumes = content.quest_volumes.len(), "level quest volumes");
    let content = Arc::new(content);
    tracing::info!(
        disabled_items = content.disabled_items.len(),
        "items of the disabled item classes"
    );
    if let Some(rules) = &content.loot_limits {
        tracing::info!(
            applied = rules.limits.len(),
            unknown_reset = ?rules.limits.unknown_reset(),
            "loot limits; those of an unknown reset are not applied"
        );
    }
    tracing::info!(objects = content.quest_objects.len(), "level quest objects");
    match &content.tutorial_quest {
        Some(quest) => tracing::info!(tasks = quest.len(), "tutorial quest"),
        None => tracing::warn!("no tutorial quest: the tutorial ends at its exit"),
    }
    let design = match DesignData::load(&design_data_path(&config.content_dir, &build)) {
        Ok(mut data) => data.levels.remove(&config.level).unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "no design data: no scripts or events");
            LevelDesign::default()
        }
    };
    tracing::info!(
        scripts = design.scripts.len(),
        events = design.events.len(),
        "design scripts and events"
    );
    drop(bundle);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RunError::Runtime)?
        .block_on(serve_async(
            config,
            passwords,
            Arc::new(ground),
            combat,
            content,
            design,
            Arc::new(mob_ai),
        ))
}

/// Logs the gameplay values: those the sim applies, and those kept for systems it does not
/// have yet.
fn log_gameplay(gameplay: &GameplayConfig) {
    tracing::info!(
        character_xp = gameplay.xp.character,
        progression_xp = ?gameplay.xp.progression,
        disabled_item_classes = ?gameplay.disabled_item_classes,
        wallet_cap = ?gameplay.wallet_cap,
        datatable_overrides = gameplay.datatable_overrides.len(),
        "remote config gameplay values applied"
    );
    tracing::info!(
        paid_loadouts = ?gameplay.paid_loadouts,
        territory_entry_requirement = ?gameplay.territory_entry_requirement,
        storage_search = ?gameplay.storage_search,
        house_level_reqs = ?gameplay.house_level_reqs,
        transmog_own_all = ?gameplay.transmog_own_all,
        red_button = ?gameplay.red_button,
        schedule_overrides = ?gameplay.schedule_overrides,
        seasons = ?gameplay.seasons,
        game_modes = ?gameplay.game_modes,
        company_only_war_slots = ?gameplay.company_only_war_slots,
        twitch_system = ?gameplay.twitch_system,
        "remote config gameplay values loaded for systems not built yet"
    );
}

/// The database passwords from the environment.
struct DatabasePasswords {
    world: String,
    global: String,
}

/// The value of secret variable `name`; a missing or empty one is an error.
fn secret(name: &str) -> Result<String, RunError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RunError::MissingSecret(name.to_owned()))
}

async fn serve_async(
    config: SimConfig,
    passwords: DatabasePasswords,
    ground: Arc<LevelGround<Arc<WorldFile>>>,
    combat: Arc<CombatData>,
    content: Arc<PlayerContent>,
    design: LevelDesign,
    mob_ai: Arc<MobAi>,
) -> Result<(), RunError> {
    let db = nw_db::connect(&config.world_database, &passwords.world)
        .await
        .map_err(RunError::Database)?;
    check_schema(&db, &WORLD_MIGRATOR).await?;
    let global = nw_db::connect(&config.global_database, &passwords.global)
        .await
        .map_err(RunError::Database)?;
    check_schema(&global, &GLOBAL_MIGRATOR).await?;
    let bus = Bus::connect(&config.nats_url).await?;
    PrometheusBuilder::new()
        .with_http_listener(config.metrics_listen)
        .set_buckets_for_metric(
            Matcher::Full(METRIC_HANDOFF_SECONDS.to_owned()),
            &LATENCY_BUCKETS_SECONDS,
        )
        .and_then(|builder| {
            builder.set_buckets_for_metric(
                Matcher::Full(METRIC_TICK_SECONDS.to_owned()),
                &LATENCY_BUCKETS_SECONDS,
            )
        })
        .and_then(|builder| {
            builder.set_buckets_for_metric(
                Matcher::Full(METRIC_TICK_PART_SECONDS.to_owned()),
                &LATENCY_BUCKETS_SECONDS,
            )
        })
        .and_then(|builder| {
            builder.set_buckets_for_metric(
                Matcher::Full(METRIC_SNAPSHOT_SECONDS.to_owned()),
                &LATENCY_BUCKETS_SECONDS,
            )
        })
        .and_then(PrometheusBuilder::install)
        .map_err(|error| RunError::Metrics(error.to_string()))?;
    let world_file = ground.terrain();
    tracing::info!(sim = %config.sim_id, world = %config.world_id, level = %config.level, "serving");
    let level_spawns = spawns_of_level(&config.level, &world_file.vitals);
    let (spawns, spawn_ai) = sim_spawns(
        &level_spawns,
        &resolve_ai(world_file, &mob_ai),
        SpawnFilter {
            scripted: config.role == SimRole::Instance
                && content.dungeon_scripts.has_level(&config.level),
            open_map: config.role == SimRole::OpenMap,
            kill_types: &content.kill_types,
        },
    );
    tracing::info!(
        spawns = spawns.len(),
        left_out = level_spawns.len() - spawns.len(),
        "mob spawns; the encounter spawns the open map does not run are left out"
    );
    let settings = LoopSettings {
        sim_id: config.sim_id.clone(),
        world_id: config.world_id,
        level: config.level.clone(),
        world: config.world_settings(
            level_bounds(&world_file.level),
            world_file.level.ocean_level,
            &content.instances,
        )?,
        handoff_timeout: std::time::Duration::from_millis(config.handoff_timeout_ms),
        forward_ttl_ms: config.forward_ttl_ms,
        save_interval: std::time::Duration::from_millis(config.save_interval_ms),
        heartbeat_interval: std::time::Duration::from_millis(config.heartbeat_interval_ms),
        heartbeat_misses: config.heartbeat_misses,
        mob_ai: spawn_ai,
        combat: config.combat_settings(combat, spawns, mob_ai),
        design: Arc::new(design),
        leaderboard_calendar: config.leaderboard_calendar()?,
        content,
        snapshot_interval: (config.admin.snapshot_interval_ms != 0)
            .then(|| std::time::Duration::from_millis(config.admin.snapshot_interval_ms)),
    };
    let shutdown = async {
        // A failed signal handler means sim runs until killed.
        let _ = nw_common::shutdown::stop_signal().await;
    };
    let databases = Databases { world: db, global };
    Ok(serve_loop(settings, ground, bus, databases, shutdown).await?)
}
