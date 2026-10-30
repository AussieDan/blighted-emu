// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool`: builds generated table code, the content bundle and the world files from a
//! New World install, and checks capture ledgers.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use content_tool::ai_data::{AiDataReport, DEFAULT_SERVER_PAK, build_ai_data};
use content_tool::ai_trace::{
    AiTrace, DEFAULT_TRACE_PATH, read_creatures, trace_ledger, write_trace,
};
use content_tool::build::{
    DEFAULT_CONTENT_DIR, DEFAULT_RETAIL_DIR, default_bundle_path, default_retail_bundle_path,
    run_build,
};
use content_tool::camp_slices::build_camp_slices;
use content_tool::capture::CaptureReport;
use content_tool::catacombs::{
    CatacombCheck, DEFAULT_GAME_MODE as DEFAULT_CATACOMB_MODE,
    DEFAULT_RUNS as DEFAULT_CATACOMB_RUNS, check as catacomb_check, map_rows as catacomb_map_rows,
};
use content_tool::codegen::run_codegen;
use content_tool::collision::{CollisionError, CollisionReport, PakIndex, build_level_collision};
use content_tool::combat::{CombatCheck, DEFAULT_LEVEL, DEFAULT_NWDB_DIR, check, read_nwdb};
use content_tool::creature_animations::{CreatureAnimationsReport, build_rows};
use content_tool::dungeon_scripts::{DungeonScriptsReport, build_dungeon_scripts};
use content_tool::error::ToolError;
use content_tool::ftue_entities::{FtueScan, Timeline, read_ledger, write_table};
use content_tool::map_tiles::{DEFAULT_MAP_DIR, MapTilesReport, YIndex, run_map_tiles};
use content_tool::mob_prefabs::{
    DEFAULT_CAPTURE_ROOTS, LedgerSource, MobPrefabsError, PrefabTable, SourceList,
    find_ledger_sources, run_workers, scan_ledger, vitals_names, worker_process, write_entries,
};
use content_tool::player_attacks::{BundleRows, PlayerAttacksReport, build_stances, read_sources};
use content_tool::roads::{RoadsError, RoadsReport, build_level_roads};
use content_tool::source::GameSource;
use content_tool::water::{WaterError, build_level_water};
use content_tool::world::{LevelReport, UnmatchedIds, run_world, world_totals};
use nw_capture::{CLIENT_TO_SERVER, CaptureError, SERVER_TO_CLIENT, TypeNames};
use nw_content::ContentBundle;
use nw_content::client_build::load_client_build;
use nw_content::combat::{CombatData, CombatRows};
use nw_content::creature_animations::{creature_animations_path, write_rows};
use nw_content::ftue_entities::ftue_entities_path;
use nw_content::mob_prefabs::{MobPrefabs, mob_prefabs_path};
use nw_content::player_attacks::{player_attacks_path, write_stances};
use nw_design::{DesignMode, DesignPaths, DesignReport, run_design};
use nw_world::WorldFile;
use nw_world::metadata::SkipCounts;
use nw_world::records::RecordCounts;
use nw_world::world_file::world_file_path;

const DEFAULT_CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
/// The level the `collision` command builds when none is named.
const DEFAULT_COLLISION_LEVEL: &str = "ftue_v2";
/// The level `roads` builds by default: the open world, whose tract maps hold the roads.
const DEFAULT_ROADS_LEVEL: &str = "newworld_vitaeeterna";
/// The team's design files (admin studio spec section 5).
const DEFAULT_DESIGN_DIR: &str = "content";
const DEFAULT_GENERATED_DIR: &str = "crates/nw-content/src/generated";
/// Client type table in the install folder.
const TYPE_INDEX_FILE: &str = "typeindex.json";
/// Message types printed per direction by `capture-check`.
const TOP_TYPES: usize = 30;
/// Decode failures printed by `capture-check`.
const SHOWN_FAILURES: usize = 20;
/// Failed ledgers and bad archives printed by `mob-prefabs`.
const SHOWN_MOB_FAILURES: usize = 20;
/// Classes printed by `mob-prefabs` that cut spawn card groups short.
const SHOWN_CUT_CLASSES: usize = 10;
/// Unread spawn cards of each kind printed by `ftue-entities`.
const SHOWN_UNREAD_CARDS: usize = 20;
/// Members without a reader left out of spawn groups, printed by `ftue-entities`.
const SHOWN_SKIPPED_MEMBERS: usize = 20;

#[derive(Parser)]
#[command(
    name = "content-tool",
    about = "Builds content data from a New World client install."
)]
struct Cli {
    /// New World install folder (the folder that holds `assets/` and `Bin64/`).
    #[arg(long, env = "NW_GAME_DIR")]
    game_dir: Option<PathBuf>,
    /// File with the pinned client build.
    #[arg(long, default_value = DEFAULT_CLIENT_BUILD_CONFIG)]
    client_build_config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Writes the generated table structs.
    Codegen {
        /// Output folder.
        #[arg(long, default_value = DEFAULT_GENERATED_DIR)]
        out_dir: PathBuf,
    },
    /// Writes the content bundle and checks it by reloading it.
    Build {
        /// Bundle file (default: `local-data/content/retail/bundle-<build>.bin`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Writes one world file per level and checks each by reloading it.
    World {
        /// Output folder for `world-<build>-<level>.bin`.
        #[arg(long, default_value = DEFAULT_RETAIL_DIR)]
        out_dir: PathBuf,
        /// Content bundle for the spawn ID check (default:
        /// `local-data/content/retail/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// Writes the collision file of each level (its solid shapes from the game paks) and checks
    /// it by reloading it.
    Collision {
        /// Levels to build.
        #[arg(default_values = [DEFAULT_COLLISION_LEVEL])]
        levels: Vec<String>,
        /// Output folder for `collision-<build>-<level>.bin`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
    },
    /// Writes the road file of each level (the road cells of its tract maps from the game paks)
    /// and checks it by reloading it.
    Roads {
        /// Levels to build.
        #[arg(default_values = [DEFAULT_ROADS_LEVEL])]
        levels: Vec<String>,
        /// Output folder for `roads-<build>-<level>.bin`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
    },
    /// Writes the water file of each level (the water surfaces of its region water trees from
    /// the game paks) and checks it by reloading it.
    Water {
        /// Levels to build.
        #[arg(default_values = [DEFAULT_COLLISION_LEVEL, DEFAULT_ROADS_LEVEL])]
        levels: Vec<String>,
        /// Output folder for `water-<build>-<level>.bin`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
    },
    /// Merges the design files over the retail files and writes the bundle and world files the
    /// servers read. Needs no game install.
    Design {
        /// The design files.
        #[arg(long, default_value = DEFAULT_DESIGN_DIR)]
        design: PathBuf,
        /// The retail bundle and world files from `build` and `world`.
        #[arg(long, default_value = DEFAULT_RETAIL_DIR)]
        retail_dir: PathBuf,
        /// Where the merged files go.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
        /// Runs every check and writes nothing.
        #[arg(long)]
        check: bool,
    },
    /// Decodes capture ledgers with the carrier and message codecs; prints counts only.
    CaptureCheck {
        /// `ledger.bin` files (unpacked into `local-data/`).
        #[arg(required = true)]
        ledgers: Vec<PathBuf>,
        /// Aeternum-World `Catalog/uuid_to_class.json`, to print class names next to type indexes.
        #[arg(long)]
        names: Option<PathBuf>,
    },
    /// Writes the world map tiles of a level as PNG files with a `tiles.json` index.
    MapTiles {
        /// Level whose map tiles are written.
        #[arg(long, default_value = DEFAULT_LEVEL)]
        level: String,
        /// Output folder; the level gets a folder in it.
        #[arg(long, default_value = DEFAULT_MAP_DIR)]
        out_dir: PathBuf,
        /// Which edge tile row 0 is (README, "Admin tools").
        #[arg(long, value_enum, default_value_t = YIndex::SouthUp)]
        y_index: YIndex,
        /// Only count the pixel formats of the tiles; write nothing.
        #[arg(long)]
        formats_only: bool,
    },
    /// Checks that every vitals spawn finds its combat rows, and our mob health against nwdb.info.
    CombatCheck {
        /// Level whose world file is checked.
        #[arg(long, default_value = DEFAULT_LEVEL)]
        level: String,
        /// Content bundle (default: `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Folder of nwdb.info creature records.
        #[arg(long, default_value = DEFAULT_NWDB_DIR)]
        nwdb_dir: PathBuf,
    },
    /// Builds the mob prefab table from the capture ledgers; each ledger is read by its own
    /// worker process (run it under a memory limit, for example `ulimit -v 4000000`).
    MobPrefabs {
        /// Folders searched for `ledger.bin` files and zip archives that hold them.
        #[arg(default_values = DEFAULT_CAPTURE_ROOTS)]
        roots: Vec<PathBuf>,
        /// Output file (default: `local-data/content/mob-prefabs-<build>.json`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Content bundle for the vitals names (default: `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Worker processes at a time (default: the number of CPU threads).
        #[arg(long)]
        jobs: Option<NonZeroUsize>,
    },
    /// Builds the tutorial entity table (static entities, asset entities and the mobs they name)
    /// from the capture of a new character's tutorial run (run it under a memory limit, for
    /// example `ulimit -v 4000000`).
    FtueEntities {
        /// The `ledger.bin` file.
        ledger: PathBuf,
        /// Output file (default: `local-data/content/ftue-entities-<build>.json`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Builds the creature animation table from the game paks, for the creatures of the mob
    /// prefab table (`local-data/content/mob-prefabs-<build>.json`).
    CreatureAnimations {
        /// Output file (default: `local-data/content/creature-animations-<build>.json`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Content bundle for the weapon tags (default: `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// Builds the AI data file (every Server.pak behavior tree, the AI of every creature slice
    /// and the CAGE task resolution of their grids) from Server.pak and the game paks, for the
    /// creatures of the creature animation table.
    AiData {
        /// The Server.pak file.
        #[arg(long, default_value = DEFAULT_SERVER_PAK)]
        server_pak: PathBuf,
        /// Output folder for `ai-data-<build>.bin`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
    },
    /// Writes what the retail mobs of a capture ledger did (ALC states, FullBody animations),
    /// the own player's positions and the hits on the player, as JSON (run it under a memory
    /// limit, for example `ulimit -v 4000000`).
    AiTrace {
        /// The `ledger.bin` file.
        ledger: PathBuf,
        /// Output file.
        #[arg(long, default_value = DEFAULT_TRACE_PATH)]
        out: PathBuf,
    },
    /// Builds the player attack table (each weapon stance's basic attacks, dodge and item uses)
    /// from the game paks.
    PlayerAttacks {
        /// Output file (default: `local-data/content/player-attacks-<build>.json`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Content bundle for the weapon, spell, ammo and consumable rows (default:
        /// `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// Makes the layouts of many runs of a generated game mode (the catacombs) and checks their
    /// tile slices against the paks; prints the counts and the first run's map.
    CatacombCheck {
        /// The game mode.
        #[arg(long, default_value = DEFAULT_CATACOMB_MODE)]
        game_mode: String,
        /// Runs to make (seeds 0 and up).
        #[arg(long, default_value_t = DEFAULT_CATACOMB_RUNS)]
        runs: u64,
        /// Content bundle (default: `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// Writes the dungeon script file: the encounters and loot containers of every instance
    /// level's script slice, from the game paks and the content bundle.
    DungeonScripts {
        /// Output folder for `dungeon-scripts-<build>.json`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
        /// Content bundle (default: `local-data/content/bundle-<build>.bin`).
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// Writes the camp slice file: the spawn group components, asset id and respawn point of
    /// every camp slice, from the game paks.
    CampSlices {
        /// Output folder for `camp-slices-<build>.json`.
        #[arg(long, default_value = DEFAULT_CONTENT_DIR)]
        out_dir: PathBuf,
    },
    /// Worker of `mob-prefabs`: reads one ledger and prints its tally as JSON.
    #[command(name = "mob-prefabs-ledger", hide = true)]
    MobPrefabsLedger {
        /// The ledger file, or the zip archive that holds it.
        #[arg(long)]
        ledger: PathBuf,
        /// The ledger's entry name in the zip archive.
        #[arg(long)]
        entry: Option<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
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

fn print_roads(report: &RoadsReport) {
    println!(
        "roads {}: {} tract maps, {} regions with roads, {} road cells -> {}",
        report.level,
        report.tractmaps,
        report.road_regions,
        report.road_cells,
        report.path.display()
    );
}

fn print_collision(report: &CollisionReport, took: std::time::Duration) {
    println!(
        "collision {}: {} placements ({} variant capitals, {} of an unknown variant), {} slices, {} solid entities as the level loads (no shape named {}, query shape kind not read {}, inactive {}, mesh without physics geometry {}, mesh with physics geometry but no .rnr {}, asset missing {}), {} physics assets ({} shapes of kinds not read), {} shapes, {} bodies, {} deferred slices with {} bodies, {} quest volumes, {} placed triangles, {} file bytes, {} index bytes, {:.1} s -> {}",
        report.level,
        report.placements,
        report.variant_placements,
        report.unknown_variants,
        report.slices,
        report.solid_entities,
        report.shapeless_entities,
        report.unread_entities,
        report.inactive_entities,
        report.meshes_without_physics,
        report.meshes_with_physics_without_rnr,
        report.missing_assets,
        report.assets,
        report.unsupported_shapes,
        report.geometries,
        report.bodies,
        report.deferred_slices,
        report.deferred_bodies,
        report.volumes,
        report.placed_triangles,
        report.file_bytes,
        report.index_bytes,
        took.as_secs_f64(),
        report.path.display()
    );
}

fn print_design(report: &DesignReport, out_dir: &Path) {
    let target = if report.written {
        out_dir.display().to_string()
    } else {
        "nothing written".to_owned()
    };
    println!(
        "design: {} files, {} levels, {} errors -> {target}",
        report.files,
        report.levels,
        report.errors.len()
    );
    for error in &report.errors {
        println!("  {error}");
    }
}

/// The install folder, for the commands that read it.
fn game_dir(game_dir: Option<&Path>) -> Result<&Path, ToolError> {
    game_dir.ok_or(ToolError::MissingGameDir)
}

fn run(cli: Cli) -> Result<(), ToolError> {
    let client_build = load_client_build(&cli.client_build_config)?;
    match cli.command {
        Command::Codegen { out_dir } => {
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            let summary = run_codegen(&source, &client_build, &out_dir)?;
            println!(
                "codegen: {} datasheets, {} table types, {} files written, {} stale files removed",
                summary.datasheet_files,
                summary.table_types,
                summary.files_written,
                summary.stale_files_removed
            );
        }
        Command::Build { out } => {
            let out = out.unwrap_or_else(|| default_retail_bundle_path(&client_build));
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            let summary = run_build(&source, &client_build, &out)?;
            println!(
                "build: {} datasheets, {} rows, {} text files, {} text entries, {} duplicate text keys, {} keyless text entries -> {}",
                summary.datasheet_files,
                summary.rows,
                summary.text_files,
                summary.text_entries,
                summary.duplicate_text_keys,
                summary.keyless_text_entries,
                summary.path.display()
            );
        }
        Command::World { out_dir, bundle } => {
            let bundle = bundle.unwrap_or_else(|| default_retail_bundle_path(&client_build));
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            print_world(&run_world(&source, &client_build, &bundle, &out_dir)?);
        }
        Command::Collision { levels, out_dir } => {
            let index =
                PakIndex::open(game_dir(cli.game_dir.as_deref())?).map_err(CollisionError::from)?;
            for level in &levels {
                let started = std::time::Instant::now();
                let report = build_level_collision(&index, level, &client_build, &out_dir)?;
                print_collision(&report, started.elapsed());
            }
        }
        Command::Roads { levels, out_dir } => {
            let index =
                PakIndex::open(game_dir(cli.game_dir.as_deref())?).map_err(RoadsError::from)?;
            for level in &levels {
                print_roads(&build_level_roads(&index, level, &client_build, &out_dir)?);
            }
        }
        Command::Water { levels, out_dir } => {
            let index =
                PakIndex::open(game_dir(cli.game_dir.as_deref())?).map_err(WaterError::from)?;
            for level in &levels {
                let report = build_level_water(&index, level, &client_build, &out_dir)?;
                println!(
                    "water {}: {} regions, {} nodes -> {}",
                    report.level,
                    report.regions,
                    report.nodes,
                    report.path.display()
                );
            }
        }
        Command::Design {
            design,
            retail_dir,
            out_dir,
            check,
        } => {
            let mode = if check {
                DesignMode::Check
            } else {
                DesignMode::Write
            };
            let paths = DesignPaths {
                design_dir: design,
                retail_dir,
                out_dir,
            };
            let report = run_design(&paths, &client_build, mode)?;
            print_design(&report, &paths.out_dir);
            if !report.errors.is_empty() {
                return Err(ToolError::DesignErrors {
                    count: report.errors.len(),
                });
            }
        }
        Command::CaptureCheck { ledgers, names } => {
            let names = match names {
                Some(names) => TypeNames::load(
                    &game_dir(cli.game_dir.as_deref())?.join(TYPE_INDEX_FILE),
                    &names,
                )?,
                None => TypeNames::default(),
            };
            let report = run_capture_check(&ledgers)?;
            print_capture(&report, &names);
            if !report.failures.is_empty() {
                return Err(ToolError::CaptureFailures {
                    count: report.failures.len(),
                });
            }
        }
        Command::MapTiles {
            level,
            out_dir,
            y_index,
            formats_only,
        } => {
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            print_map_tiles(&run_map_tiles(
                &source,
                &level,
                &out_dir,
                y_index,
                formats_only,
            )?);
        }
        Command::CombatCheck {
            level,
            bundle,
            nwdb_dir,
        } => {
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let tables = ContentBundle::load(&bundle, &client_build)?.tables;
            let data = CombatData::from_tables(&tables, &CombatRows::default())?;
            drop(tables);
            let world_path = world_file_path(Path::new(DEFAULT_CONTENT_DIR), &client_build, &level);
            let world = WorldFile::load(&world_path, &client_build)?;
            let nwdb = read_nwdb(&nwdb_dir)?;
            print_combat(
                &check(&data, &world.vitals, &nwdb.creatures),
                nwdb.unreadable,
            );
        }
        Command::MobPrefabs {
            roots,
            out,
            bundle,
            jobs,
        } => {
            let out = out
                .unwrap_or_else(|| mob_prefabs_path(Path::new(DEFAULT_CONTENT_DIR), &client_build));
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let global_args = [
                OsString::from("--game-dir"),
                game_dir(cli.game_dir.as_deref())?.as_os_str().to_owned(),
                OsString::from("--client-build-config"),
                cli.client_build_config.into_os_string(),
            ];
            run_mob_prefabs(&roots, &out, &bundle, &client_build, jobs, &global_args)?;
        }
        Command::FtueEntities { ledger, out } => {
            let out = out.unwrap_or_else(|| {
                ftue_entities_path(Path::new(DEFAULT_CONTENT_DIR), &client_build)
            });
            let scan = content_tool::ftue_entities::scan_ledger(&read_ledger(&ledger)?)?;
            write_table(&out, &scan.table)?;
            print_ftue_entities(&scan, &out);
        }
        Command::CreatureAnimations { out, bundle } => {
            let content_dir = Path::new(DEFAULT_CONTENT_DIR);
            let out = out.unwrap_or_else(|| creature_animations_path(content_dir, &client_build));
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let tables = ContentBundle::load(&bundle, &client_build)?.tables;
            let prefabs = MobPrefabs::load(content_dir, &client_build)
                .map_err(content_tool::creature_animations::CreatureAnimationsError::from)?;
            let dir = game_dir(cli.game_dir.as_deref())?;
            let source = GameSource::open(dir)?;
            let (rows, report) = build_rows(&source, dir, &prefabs, &tables)?;
            write_rows(&out, &rows)
                .map_err(content_tool::creature_animations::CreatureAnimationsError::from)?;
            print_creature_animations(&report, &out);
        }
        Command::AiData {
            server_pak,
            out_dir,
        } => {
            let creatures = read_creatures(&creature_animations_path(
                Path::new(DEFAULT_CONTENT_DIR),
                &client_build,
            ))?;
            let dir = game_dir(cli.game_dir.as_deref())?;
            let source = GameSource::open(dir)?;
            let (data, report) = build_ai_data(&source, dir, &server_pak, &creatures)?;
            let path = data.save(&out_dir, &client_build)?;
            let reloaded = nw_ai::AiData::load(&out_dir, &client_build)?;
            if reloaded != data {
                return Err(ToolError::CountMismatch {
                    what: "AI data nodes",
                    expected: data.nodes.len(),
                    found: reloaded.nodes.len(),
                });
            }
            print_ai_data(&report, &path);
        }
        Command::AiTrace { ledger, out } => {
            let creatures = read_creatures(&creature_animations_path(
                Path::new(DEFAULT_CONTENT_DIR),
                &client_build,
            ))?;
            let trace = trace_ledger(&read_ledger(&ledger)?, &creatures);
            write_trace(&out, &trace)?;
            print_ai_trace(&trace, &out);
        }
        Command::PlayerAttacks { out, bundle } => {
            let out = out.unwrap_or_else(|| {
                player_attacks_path(Path::new(DEFAULT_CONTENT_DIR), &client_build)
            });
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let rows = BundleRows::of(&ContentBundle::load(&bundle, &client_build)?.tables);
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            let (stances, report) = build_stances(&read_sources(&source, &rows)?, &rows);
            write_stances(&out, &stances)?;
            print_player_attacks(&report, &out);
        }
        Command::CatacombCheck {
            game_mode,
            runs,
            bundle,
        } => {
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let tables = ContentBundle::load(&bundle, &client_build)?.tables;
            let index =
                PakIndex::open(game_dir(cli.game_dir.as_deref())?).map_err(CollisionError::from)?;
            let report = catacomb_check(&tables, &game_mode, runs, |wanted| {
                index
                    .paths(|path| wanted.contains(path))
                    .into_iter()
                    .collect()
            })?;
            print_catacombs(&report);
        }
        Command::DungeonScripts { out_dir, bundle } => {
            let bundle = bundle.unwrap_or_else(|| default_bundle_path(&client_build));
            let tables = ContentBundle::load(&bundle, &client_build)?.tables;
            let index =
                PakIndex::open(game_dir(cli.game_dir.as_deref())?).map_err(CollisionError::from)?;
            print_dungeon_scripts(&build_dungeon_scripts(
                &index,
                &tables,
                &client_build,
                &out_dir,
            )?);
        }
        Command::CampSlices { out_dir } => {
            let source = GameSource::open(game_dir(cli.game_dir.as_deref())?)?;
            let report = build_camp_slices(&source, &client_build, &out_dir)?;
            println!(
                "{}: {} camps ({} PvP) of {} camp folder slices",
                report.path.display(),
                report.camps,
                report.pvp_camps,
                report.slices
            );
        }
        Command::MobPrefabsLedger { ledger, entry } => {
            let tally = scan_ledger(
                &LedgerSource {
                    path: ledger,
                    entry,
                }
                .read()?,
            );
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &tally).map_err(MobPrefabsError::from)?;
            stdout.flush().map_err(|source| MobPrefabsError::Io {
                path: PathBuf::from("stdout"),
                source,
            })?;
        }
    }
    Ok(())
}

fn print_dungeon_scripts(report: &DungeonScriptsReport) {
    println!(
        "{}: {} levels, {} encounters, {} stages, {} spawns, {} chests",
        report.path.display(),
        report.levels,
        report.encounters,
        report.stages,
        report.spawns,
        report.chests
    );
    println!(
        "target slices: {} spawner mobs ({} without a level), {} other slices",
        report.spawner_mobs, report.levelless_mobs, report.other_slices
    );
    if !report.skipped_modes.is_empty() {
        println!(
            "game modes without a script: {}",
            report.skipped_modes.join(", ")
        );
    }
    for path in report.missing_slices.iter().take(SHOWN_FAILURES) {
        println!("not in the paks: {path}");
    }
}

fn print_catacombs(report: &CatacombCheck) {
    println!(
        "{}: {} runs from cell {:?}: {} to {} tiles, {} to {} connectors",
        report.game_mode,
        report.runs,
        report.start_cell,
        report.tiles.0,
        report.tiles.1,
        report.connectors.0,
        report.connectors.1
    );
    println!(
        "run 0: cluster {}, theme {}, 1635 frame {:?}",
        report.first.cluster,
        report.first.theme.as_deref().unwrap_or("-"),
        report.first.frame()
    );
    for row in catacomb_map_rows(&report.first) {
        println!("  {row}");
    }
    if report.missing_slices.is_empty() {
        println!("every tile variation slice is in the paks");
    }
    for slice in &report.missing_slices {
        println!("missing tile slice: {slice}");
    }
}

/// Missing values printed per kind by `ai-data`.
const SHOWN_AI_GAPS: usize = 40;

fn print_ai_data(report: &AiDataReport, out: &Path) {
    println!(
        "ai-data: {} trees, {} shared nodes ({} expanded), {} creatures, {} archetypes, {} grids, {} CAGE actions -> {}",
        report.trees,
        report.nodes,
        report.expanded_nodes,
        report.creatures,
        report.archetypes,
        report.grids,
        report.cage_actions,
        out.display()
    );
    println!(
        "archetypes with a Server.pak tree {}, a grid {}, an animation database {}; fragment entries {}, with a length {}",
        report.with_tree,
        report.with_grid,
        report.with_animation_database,
        report.variants,
        report.timed_variants
    );
    println!(
        "trees named by slices and not in Server.pak: {}",
        report.missing_trees.len()
    );
    for (tree, slices) in report.missing_trees.iter().take(SHOWN_AI_GAPS) {
        println!("  {tree}: {} slices", slices.len());
    }
    println!(
        "slices whose tree runs a task their grid lacks: {}",
        report.missing_tasks.len()
    );
    for (slice, tasks) in report.missing_tasks.iter().take(SHOWN_AI_GAPS) {
        println!("  {slice}: {tasks:?}");
    }
    println!("clips of Unknown length: {}", report.unknown_clips.len());
    for (clip, reason) in report.unknown_clips.iter().take(SHOWN_AI_GAPS) {
        println!("  {clip}: {reason}");
    }
    println!(
        "fragment entries of Unknown length: {} in {} animation databases",
        report.unknown_fragments.values().sum::<usize>(),
        report.unknown_fragments.len()
    );
    println!("files not used: {}", report.skipped.len());
    for (file, reason) in report.skipped.iter().take(SHOWN_AI_GAPS) {
        println!("  {file}: {reason}");
    }
}

fn print_ai_trace(trace: &AiTrace, out: &Path) {
    let stats = &trace.stats;
    println!(
        "ai-trace: {} mob lifetimes, {} mob states, {} player positions, {} player hits ({} by an unknown attacker) -> {}",
        trace.mobs.len(),
        stats.mob_states,
        trace.player.len(),
        trace.player_hits.len(),
        stats.unknown_attackers,
        out.display()
    );
    println!(
        "  {} bundles, {} groups, {} resyncs, {} other-component states, {} bad scope blobs",
        stats.bundles,
        stats.groups,
        stats.resyncs,
        stats.other_component_states,
        stats.bad_scope_blobs
    );
}

fn print_player_attacks(report: &PlayerAttacksReport, out: &Path) {
    println!(
        "player-attacks: {} stances -> {}",
        report.stances,
        out.display()
    );
    for (stance, why) in &report.skipped {
        println!("  skipped {stance}: {why}");
    }
}

fn print_creature_animations(report: &CreatureAnimationsReport, out: &Path) {
    println!(
        "creature-animations: {} of {} creature assets -> {}",
        report.creatures,
        report.assets,
        out.display()
    );
    for (asset, why) in &report.skipped {
        println!("  skipped {asset}: {why}");
    }
}

fn run_mob_prefabs(
    roots: &[PathBuf],
    out: &Path,
    bundle: &Path,
    client_build: &str,
    jobs: Option<NonZeroUsize>,
    global_args: &[OsString],
) -> Result<(), ToolError> {
    let list = find_ledger_sources(roots)?;
    let exe = std::env::current_exe().map_err(|source| MobPrefabsError::Io {
        path: PathBuf::from("content-tool"),
        source,
    })?;
    let jobs = jobs
        .or_else(|| std::thread::available_parallelism().ok())
        .unwrap_or(NonZeroUsize::MIN);
    let results = run_workers(&list.sources, jobs, |source| {
        worker_process(&exe, global_args, source)
    });
    let mut table = PrefabTable::default();
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(tally) => table.add(tally),
            Err(error) => failures.push(error.to_string()),
        }
    }
    let names = vitals_names(&ContentBundle::load(bundle, client_build)?.tables);
    let entries = table.entries(&names);
    write_entries(out, &entries)?;
    let assets: HashSet<&str> = entries.iter().map(|entry| entry.asset.as_str()).collect();
    print_mob_prefabs(&list, &table, &failures);
    println!(
        "entries: {}, distinct assets: {}, with a vitals name: {}, with a death: {} -> {}",
        entries.len(),
        assets.len(),
        entries
            .iter()
            .filter(|entry| entry.vitals_name.is_some())
            .count(),
        entries
            .iter()
            .filter(|entry| !entry.death_members.is_empty())
            .count(),
        out.display()
    );
    Ok(())
}

fn print_mob_prefabs(list: &SourceList, table: &PrefabTable, failures: &[String]) {
    let stats = &table.stats;
    println!(
        "mob-prefabs: {} ledgers found, {} merged, {} duplicates, {} failed; {} rar archives not read, {} bad zip archives",
        list.sources.len(),
        table.ledgers,
        table.duplicates,
        failures.len(),
        list.rar_archives,
        list.bad_archives.len()
    );
    println!(
        "bundles {} ({} bad), mob first groups {}, player cards {}, cards without vitals {}, cut card groups {}, deaths {}, resyncs {}, lost payload tails {}, stream failures {}",
        stats.bundles,
        stats.bad_bundles,
        stats.first_groups,
        stats.player_cards,
        stats.cards_without_vitals,
        stats.cut_cards,
        stats.deaths,
        stats.resyncs,
        stats.lost_tails,
        stats.stream_failures
    );
    let mut cut: Vec<(u64, u64)> = stats.cut_classes.iter().map(|(c, n)| (*c, *n)).collect();
    cut.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    println!(
        "classes without a reader that cut card groups (class, groups): {:?}",
        &cut[..cut.len().min(SHOWN_CUT_CLASSES)]
    );
    for failure in failures.iter().take(SHOWN_MOB_FAILURES) {
        println!("  failed: {failure}");
    }
    for (path, reason) in list.bad_archives.iter().take(SHOWN_MOB_FAILURES) {
        println!("  bad archive: {}: {reason}", path.display());
    }
}

fn print_ftue_entities(scan: &FtueScan, out: &Path) {
    let stats = &scan.stats;
    let table = &scan.table;
    println!(
        "ftue-entities: {} bundles, {} static and {} asset entities, {} mobs ({} mob cards), {} spawns ({} sent again), {} changes ({} sent again), {} static and {} asset cards in unread groups, {} members without a reader left out -> {}",
        stats.bundles,
        table.entities.len(),
        table.asset_entities.len(),
        table.mobs.len(),
        stats.mob_cards,
        stats.spawns,
        stats.repeated_spawns,
        stats.changes,
        stats.repeated_changes,
        stats.unread_cards.len(),
        stats.unread_asset_cards.len(),
        stats.skipped_members.len(),
        out.display()
    );
    for entity in &table.entities {
        print_timeline(&entity.gde_ref_prefix, &entity.timeline);
    }
    for entity in &table.asset_entities {
        print_timeline(&entity.asset, &entity.timeline);
    }
    for mob in &table.mobs {
        println!(
            "  mob {} (vitals {:08x}) first card at {} ms",
            mob.asset, mob.vitals_id, mob.first_card_ms
        );
    }
    for (time_ms, prefix) in stats.unread_cards.iter().take(SHOWN_UNREAD_CARDS) {
        println!("  unread card at {time_ms} ms: {}", hex::encode(prefix));
    }
    for (time_ms, asset) in stats.unread_asset_cards.iter().take(SHOWN_UNREAD_CARDS) {
        println!(
            "  unread asset card at {time_ms} ms: {}",
            hex::encode(asset)
        );
    }
    for (time_ms, class) in stats.skipped_members.iter().take(SHOWN_SKIPPED_MEMBERS) {
        println!("  member of class {class} without a reader left out at {time_ms} ms");
    }
}

fn print_timeline(name: &str, timeline: &Timeline) {
    println!(
        "  {name}: spawns at {:?}, {} changes, despawns at {:?}",
        timeline
            .spawns
            .iter()
            .map(|spawn| (spawn.time_ms, spawn.interest, spawn.members.len()))
            .collect::<Vec<_>>(),
        timeline.changes.len(),
        timeline.despawns
    );
}

fn print_map_tiles(report: &MapTilesReport) {
    println!("map-tiles: tiles per level {:?}", report.per_level);
    println!("formats: {:?}", report.formats);
    println!("unreadable tiles (left out): {}", report.unreadable.len());
    for (path, reason) in &report.unreadable {
        println!("  {path}: {reason}");
    }
}

fn print_combat(report: &CombatCheck, unreadable: usize) {
    println!(
        "combat-check: {} spawns, {} without a mob type row, {} without a level row",
        report.spawns, report.without_type, report.without_level
    );
    println!(
        "nwdb: {} of {} creature records match our max health, {} more match it at another level ({} files unreadable)",
        report.nwdb_matched, report.nwdb_records, report.nwdb_other_level, unreadable
    );
    for mismatch in &report.mismatches {
        let ours = mismatch
            .ours
            .map_or_else(|| "no rows".to_owned(), |ours| ours.to_string());
        println!(
            "  {} level {}: nwdb {}, ours {ours}",
            mismatch.vitals_id, mismatch.level, mismatch.nwdb
        );
    }
}

fn run_capture_check(ledgers: &[PathBuf]) -> Result<CaptureReport, ToolError> {
    let mut report = CaptureReport::default();
    for path in ledgers {
        let bytes = std::fs::read(path).map_err(|source| CaptureError::Io {
            path: path.clone(),
            source,
        })?;
        report.check_ledger(path, &bytes)?;
    }
    Ok(report)
}

fn print_capture(report: &CaptureReport, names: &TypeNames) {
    println!(
        "capture-check: {} files, {} + {} datagrams (server, client), {} + {} messages, {} inline-UUID types, {} decode failures",
        report.files,
        report.datagrams[SERVER_TO_CLIENT as usize],
        report.datagrams[CLIENT_TO_SERVER as usize],
        report.messages[SERVER_TO_CLIENT as usize],
        report.messages[CLIENT_TO_SERVER as usize],
        report.inline_uuid_messages,
        report.failures.len()
    );
    let registration = &report.registration;
    println!(
        "registration: {} responses; status {:?}; four bytes {:?}; versions {:?}; token lengths {:?}; clock within {} ms",
        registration.responses,
        registration.statuses,
        registration.flags,
        registration.versions,
        registration.token_lengths,
        registration.max_clock_offset_ms
    );
    println!(
        "M4 bodies decoded and encoded to the same bytes, by type: {:?}",
        report.bodies
    );
    println!(
        "bundle members decoded and encoded to the same bytes, by class: {:?}; payloads read to the end: {}",
        report.members, report.complete_payloads
    );
    println!(
        "new connections inside a capture context: {}",
        report.connections
    );
    for (direction, label) in [
        (SERVER_TO_CLIENT, "server to client"),
        (CLIENT_TO_SERVER, "client to server"),
    ] {
        let mut types: Vec<(u64, u64)> = report
            .types
            .iter()
            .filter(|((from, _), _)| *from == direction)
            .map(|((_, index), count)| (*index, *count))
            .collect();
        types.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        println!("types {label}: {} kinds", types.len());
        for (index, count) in types.iter().take(TOP_TYPES) {
            println!(
                "  {index:>5} {count:>9} {}",
                names.name(*index).unwrap_or("")
            );
        }
    }
    for failure in report.failures.iter().take(SHOWN_FAILURES) {
        println!(
            "  failure: {} record {}: {}",
            failure.file.display(),
            failure.record,
            failure.reason
        );
    }
}

fn print_world(reports: &[LevelReport]) {
    for report in reports {
        print_level_summary(
            &format!("world {}", report.level),
            &report.counts,
            report.file_bytes,
            &report.skipped,
            &report.unmatched,
        );
        println!("  file: {}", report.path.display());
    }
    let totals = world_totals(reports);
    print_level_summary(
        &format!("world total ({} levels)", totals.levels),
        &totals.counts,
        totals.file_bytes,
        &totals.skipped,
        &totals.unmatched,
    );
}

fn print_level_summary(
    label: &str,
    counts: &RecordCounts,
    file_bytes: u64,
    skipped: &SkipCounts,
    unmatched: &UnmatchedIds,
) {
    println!(
        "{label}: {} regions ({} with terrain), {} npcs, {} vitals, {} gatherables, {} lore, {} landmarks, {} encounters, {} capitals, {} spawned slices, {} stations, {file_bytes} bytes",
        counts.regions,
        counts.terrain_regions,
        counts.npcs,
        counts.vitals,
        counts.gatherables,
        counts.lore,
        counts.landmarks,
        counts.encounters,
        counts.capitals,
        counts.spawned,
        counts.stations,
    );
    for (section, records) in &skipped.sections {
        println!("  skipped section {section:08x}: {records} records");
    }
    for ((section, field), count) in &skipped.fields {
        println!("  skipped field {section:08x}/{field:08x}: {count}");
    }
    println!(
        "  unmatched ids: {} npcs, {} vitals, {} gatherables, {} lore",
        unmatched.npcs, unmatched.vitals, unmatched.gatherables, unmatched.lore
    );
}
