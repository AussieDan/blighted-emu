// SPDX-License-Identifier: AGPL-3.0-or-later
//! Top-level error of `content-tool`.

use nw_common::config::ConfigError;
use nw_content::ContainerError;
use nw_content::combat::CombatDataError;
use nw_content::fill::FillError;
use nw_design::DesignRunError;
use nw_protocol::store_api::StoreTableError;

use crate::ai_data::AiDataError;
use crate::ai_trace::AiTraceError;
use crate::catacombs::CatacombCheckError;
use crate::codegen::CodegenError;
use crate::collision::CollisionError;
use crate::combat::NwdbError;
use crate::creature_animations::CreatureAnimationsError;
use crate::dungeon_scripts::DungeonScriptsBuildError;
use crate::ftue_entities::FtueEntitiesError;
use crate::map_tiles::MapTilesError;
use crate::mob_prefabs::MobPrefabsError;
use crate::roads::RoadsError;
use crate::source::SourceError;
use crate::water::WaterError;
use crate::world::WorldBuildError;
use nw_capture::CaptureError;

/// Any error of a `content-tool` command.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A map tile cannot be decoded or written.
    #[error(transparent)]
    MapTiles(#[from] MapTilesError),
    /// The catacomb check cannot run.
    #[error(transparent)]
    Catacombs(#[from] CatacombCheckError),
    /// Reading the install failed.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// A music chart cannot be read or the chart file written.
    #[error(transparent)]
    MusicCharts(#[from] nw_content::music_charts::MusicChartError),
    /// Code generation failed.
    #[error(transparent)]
    Codegen(#[from] CodegenError),
    /// A datasheet does not fit its generated struct.
    #[error(transparent)]
    Fill(#[from] FillError),
    /// The client build file is missing or invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The bundle cannot be written or read back.
    #[error("content bundle: {0}")]
    Bundle(#[from] ContainerError),
    /// A world file cannot be built, written or read back.
    #[error(transparent)]
    World(#[from] WorldBuildError),
    /// A collision file cannot be built, written or read back.
    #[error(transparent)]
    Collision(#[from] CollisionError),
    /// A road file cannot be built, written or read back.
    #[error(transparent)]
    Roads(#[from] RoadsError),
    /// The store table cannot be written.
    #[error(transparent)]
    StoreTable(#[from] StoreTableError),
    /// The leaderboard table cannot be written.
    #[error(transparent)]
    LeaderboardTable(#[from] nw_content::leaderboard_table::LeaderboardTableError),
    /// A water file cannot be built, written or read back.
    #[error(transparent)]
    Water(#[from] WaterError),
    /// The dungeon script file cannot be built.
    #[error(transparent)]
    DungeonScripts(#[from] DungeonScriptsBuildError),
    /// The camp slice file cannot be written.
    #[error(transparent)]
    CampSlices(#[from] nw_world::camps::CampSlicesError),
    /// The quick course path file cannot be written.
    #[error(transparent)]
    QuickCourses(#[from] nw_world::quick_courses::QuickCoursesError),
    /// The reloaded bundle does not hold what was written.
    #[error("bundle check failed: {what} is {found} after reload, expected {expected}")]
    CountMismatch {
        /// What was counted.
        what: &'static str,
        /// Count before writing.
        expected: usize,
        /// Count after reload.
        found: usize,
    },
    /// A capture ledger or a type name file cannot be read.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// A config row of the combat check is not in the bundle.
    #[error(transparent)]
    Combat(#[from] CombatDataError),
    /// The nwdb.info records cannot be read.
    #[error(transparent)]
    Nwdb(#[from] NwdbError),
    /// The mob prefab scan failed.
    #[error(transparent)]
    MobPrefabs(#[from] MobPrefabsError),
    /// The creature animation build failed.
    #[error(transparent)]
    CreatureAnimations(#[from] CreatureAnimationsError),
    /// The player attack table cannot be written.
    #[error(transparent)]
    PlayerAttacks(#[from] nw_content::player_attacks::PlayerAttacksError),
    /// The player ability table cannot be written.
    #[error(transparent)]
    PlayerAbilities(#[from] nw_content::player_abilities::PlayerAbilitiesError),
    /// The tutorial static entity scan failed.
    #[error(transparent)]
    FtueEntities(#[from] FtueEntitiesError),
    /// The AI data build failed.
    #[error(transparent)]
    AiData(#[from] AiDataError),
    /// The mob AI trace cannot read its inputs or write its output.
    #[error(transparent)]
    AiTrace(#[from] AiTraceError),
    /// The design step cannot read or write a retail or merged file.
    #[error("design: {0}")]
    Design(#[from] DesignRunError),
    /// The design step found problems in design files.
    #[error("design check found {count} problems")]
    DesignErrors {
        /// Problems found.
        count: usize,
    },
    /// The command reads the game install, and none was given.
    #[error("this command reads the game install: set NW_GAME_DIR or pass --game-dir")]
    MissingGameDir,
    /// The capture check found decode failures.
    #[error("capture check found {count} decode failures")]
    CaptureFailures {
        /// Failures found.
        count: usize,
    },
}
