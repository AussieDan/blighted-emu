// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library part of `content-tool`: code generation, bundle building, world file building, the
//! capture check, the mob prefab table, the creature animation table, the tutorial static
//! entity table and the level collision files.

pub mod ai_data;
#[cfg(all(test, feature = "ai-replay"))]
mod ai_replay;
pub mod ai_trace;
pub mod build;
pub mod camp_slices;
pub mod capture;
pub mod catacombs;
pub mod codegen;
pub mod collision;
pub mod combat;
pub mod creature_animations;
pub mod dungeon_scripts;
pub mod error;
pub mod extra_members;
pub mod ftue_entities;
pub mod leaderboards;
pub mod map_tiles;
pub mod melee_shapes;
pub mod mob_prefabs;
pub mod music_charts;
#[cfg(all(test, feature = "own-replay"))]
mod own_replay;
pub mod player_abilities;
pub mod player_attacks;
pub mod quick_courses;
pub mod roads;
pub mod source;
pub mod stand_in_trees;
pub mod store;
pub mod water;
pub mod world;

#[cfg(all(test, feature = "local-data"))]
mod local_data_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use nw_formats::azcs::{decode_azcs, is_azcs};
    use nw_formats::heightmap::{HEIGHTMAP_SIZE, SMALL_HEIGHTMAP_SIZE, parse_heightmap};
    use nw_formats::local_data::*;
    use nw_formats::objectstream::parse_object_stream;
    use nw_world::WorldFile;
    use nw_world::records::RecordCounts;

    use crate::build::{default_bundle_path, run_build};
    use crate::codegen::run_codegen;
    use crate::map_tiles::{YIndex, parse_tile_name, run_map_tiles};
    use crate::source::{GameSource, SourceError};
    use crate::world::{LEVELS_PREFIX, run_world};
    use nw_content::client_build::load_client_build;

    const TIMELINE_EXTENSION: &str = ".timeline";
    const METADATA_EXTENSION: &str = ".metadata";
    const HEIGHTMAP_SUFFIX: &str = "/region.heightmap";
    /// Largest share of open world vitals spawns whose ID is in no vitals table row (M1b spec
    /// section 8: at least 99 % match).
    const MAX_UNMATCHED_VITALS_PERCENT: usize = 1;
    const PERCENT: usize = 100;
    /// Largest absolute median of (capital z - ground height) in the open world.
    const MAX_MEDIAN_HEIGHT_ERROR_METRES: f32 = 0.01;

    /// World map tile levels of the open world.
    const MAP_TILE_LEVELS: usize = 8;
    /// Level 1 map tiles of the open world, readable or not.
    const LEVEL_1_MAP_TILES: u64 = 2_156;
    /// Highest level 1 tile index: 56 tiles of 256 m over 14,336 m.
    const LAST_LEVEL_1_INDEX: u16 = 55;
    /// Joints of `player_male.chr`.
    const PLAYER_JOINTS: usize = 130;
    /// The greatsword's light attack clip, its last frame and its root motion
    /// (`m_fDistance` of its motion parameters).
    const GREATSWORD_LIGHT_CLIP: &str = "grtswrd_prim_c1";
    const GREATSWORD_LIGHT_FRAMES: i32 = 73;
    const GREATSWORD_LIGHT_DISTANCE_M: f32 = 1.585_972_3;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read_folder(dir: &Path) -> BTreeMap<String, String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|item| {
                let path = item.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(&path).unwrap())
            })
            .collect()
    }

    #[test]
    fn the_install_has_level_1_tiles_of_256_m() {
        let source = GameSource::open(&game_dir()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let report = run_map_tiles(
            &source,
            PINNED_OPEN_WORLD_LEVEL,
            out.path(),
            YIndex::SouthUp,
            true,
        )
        .unwrap();
        assert_eq!(report.per_level.len(), MAP_TILE_LEVELS);
        assert!(
            report
                .formats
                .keys()
                .all(|format| format == "Bc1" || format == "Bc3"),
            "{:?}",
            report.formats
        );
        let unreadable_level_1 = report
            .unreadable
            .iter()
            .filter(|(path, _)| {
                parse_tile_name(path, PINNED_OPEN_WORLD_LEVEL).is_some_and(|name| name.level == 1)
            })
            .count() as u64;
        assert_eq!(report.per_level[&1] + unreadable_level_1, LEVEL_1_MAP_TILES);
        let mut last = 0;
        source
            .for_each_entry_read(
                |path| {
                    parse_tile_name(path, PINNED_OPEN_WORLD_LEVEL)
                        .is_some_and(|name| name.level == 1)
                },
                |path, _| {
                    let name = parse_tile_name(path, PINNED_OPEN_WORLD_LEVEL).unwrap();
                    last = last.max(name.x).max(name.y);
                    Ok::<(), SourceError>(())
                },
            )
            .unwrap();
        assert_eq!(last, LAST_LEVEL_1_INDEX);
    }

    #[test]
    fn committed_config_names_the_pinned_build() {
        let build = load_client_build(&repo_root().join("config/client-build.toml")).unwrap();
        assert_eq!(build, PINNED_STEAM_BUILD);
    }

    #[test]
    fn committed_generated_code_matches_codegen() {
        let out = tempfile::tempdir().unwrap();
        let source = GameSource::open(&game_dir()).unwrap();
        let summary = run_codegen(&source, PINNED_STEAM_BUILD, out.path()).unwrap();
        assert_eq!(summary.datasheet_files, PINNED_DATASHEET_FILES);
        assert_eq!(summary.table_types, PINNED_TABLE_TYPES);
        let fresh = read_folder(out.path());
        let committed = read_folder(&repo_root().join("crates/nw-content/src/generated"));
        assert_eq!(
            fresh.keys().collect::<Vec<_>>(),
            committed.keys().collect::<Vec<_>>()
        );
        for (name, contents) in &fresh {
            assert!(
                committed[name] == *contents,
                "{name} differs from fresh codegen output"
            );
        }
    }

    #[test]
    fn build_holds_every_pinned_row_and_reloads() {
        let out = tempfile::tempdir().unwrap();
        let source = GameSource::open(&game_dir()).unwrap();
        // run_build reloads the bundle and checks table, row and text counts itself.
        let summary =
            run_build(&source, PINNED_STEAM_BUILD, &out.path().join("bundle.bin")).unwrap();
        assert_eq!(summary.datasheet_files, PINNED_DATASHEET_FILES);
        assert_eq!(summary.rows, PINNED_DATASHEET_ROWS);
        assert_eq!(summary.text_files, PINNED_EN_US_TEXT_FILES);
        assert_eq!(summary.keyless_text_entries, PINNED_EN_US_KEYLESS);
    }

    #[test]
    fn every_timeline_is_azcs_around_an_object_stream() {
        let source = GameSource::open(&game_dir()).unwrap();
        let count = source
            .for_each_entry(
                |path| path.ends_with(TIMELINE_EXTENSION),
                |path, bytes| {
                    assert!(is_azcs(&bytes), "{path} is not AZCS");
                    let stream =
                        decode_azcs(&bytes).unwrap_or_else(|error| panic!("{path}: {error}"));
                    parse_object_stream(&stream).unwrap_or_else(|error| panic!("{path}: {error}"));
                    Ok::<(), SourceError>(())
                },
            )
            .unwrap();
        assert_eq!(count, PINNED_TIMELINE_FILES);
    }

    #[test]
    fn every_world_metadata_file_parses() {
        let source = GameSource::open(&game_dir()).unwrap();
        let count = source
            .for_each_entry(
                |path| path.starts_with(LEVELS_PREFIX) && path.ends_with(METADATA_EXTENSION),
                |path, bytes| {
                    let stream = if is_azcs(&bytes) {
                        decode_azcs(&bytes).unwrap_or_else(|error| panic!("{path}: {error}"))
                    } else {
                        bytes
                    };
                    parse_object_stream(&stream).unwrap_or_else(|error| panic!("{path}: {error}"));
                    Ok::<(), SourceError>(())
                },
            )
            .unwrap();
        assert_eq!(
            count,
            PINNED_REGION_METADATA_FILES + PINNED_CAPITAL_METADATA_FILES
        );
    }

    #[test]
    fn every_heightmap_parses_in_one_of_two_sizes() {
        let source = GameSource::open(&game_dir()).unwrap();
        let (mut full, mut small) = (0, 0);
        source
            .for_each_entry(
                |path| path.starts_with(LEVELS_PREFIX) && path.ends_with(HEIGHTMAP_SUFFIX),
                |path, bytes| {
                    match parse_heightmap(&bytes).map(|map| map.size) {
                        Ok(HEIGHTMAP_SIZE) => full += 1,
                        Ok(SMALL_HEIGHTMAP_SIZE) => small += 1,
                        Ok(size) => panic!("{path}: size {size}"),
                        Err(error) => panic!("{path}: {error}"),
                    }
                    Ok::<(), SourceError>(())
                },
            )
            .unwrap();
        assert_eq!(full, PINNED_FULL_HEIGHTMAPS);
        assert_eq!(small, PINNED_SMALL_HEIGHTMAPS);
    }

    #[test]
    fn world_builds_every_level_with_the_pinned_open_world_counts() {
        let bundle = repo_root().join(default_bundle_path(PINNED_STEAM_BUILD));
        assert!(
            bundle.exists(),
            "{} is missing; run `just bundle` first",
            bundle.display()
        );
        let out = tempfile::tempdir().unwrap();
        let source = GameSource::open(&game_dir()).unwrap();
        let reports = run_world(&source, PINNED_STEAM_BUILD, &bundle, out.path()).unwrap();
        assert_eq!(reports.len(), PINNED_LEVELS);
        let open_world = reports
            .iter()
            .find(|report| report.level == PINNED_OPEN_WORLD_LEVEL)
            .unwrap();
        assert_eq!(
            open_world.counts,
            RecordCounts {
                regions: PINNED_OPEN_WORLD_REGIONS,
                terrain_regions: PINNED_OPEN_WORLD_REGIONS,
                npcs: PINNED_OPEN_WORLD_NPCS,
                vitals: PINNED_OPEN_WORLD_VITALS,
                gatherables: PINNED_OPEN_WORLD_GATHERABLES,
                lore: PINNED_OPEN_WORLD_LORE,
                landmarks: PINNED_OPEN_WORLD_LANDMARKS,
                encounters: PINNED_OPEN_WORLD_ENCOUNTERS,
                capitals: PINNED_OPEN_WORLD_CAPITALS,
                spawned: PINNED_OPEN_WORLD_SPAWNED,
                stations: PINNED_OPEN_WORLD_STATIONS,
            }
        );
        assert!(
            open_world.unmatched.vitals * PERCENT
                <= open_world.counts.vitals * MAX_UNMATCHED_VITALS_PERCENT,
            "{} of {} vitals ids are unmatched",
            open_world.unmatched.vitals,
            open_world.counts.vitals
        );
        let world = WorldFile::load(&open_world.path, PINNED_STEAM_BUILD).unwrap();
        let mut errors: Vec<f32> = world
            .capitals
            .iter()
            .filter_map(|capital| {
                let [x, y, z] = capital.position;
                world.height_at(x, y).map(|ground| z - ground)
            })
            .collect();
        assert!(!errors.is_empty());
        errors.sort_by(f32::total_cmp);
        let median = errors[errors.len() / 2];
        assert!(
            median.abs() < MAX_MEDIAN_HEIGHT_ERROR_METRES,
            "median height error {median} m"
        );
    }

    #[test]
    fn the_player_attack_table_of_the_paks_is_the_committed_one() {
        let bundle = repo_root().join(default_bundle_path(PINNED_STEAM_BUILD));
        let tables = nw_content::ContentBundle::load(&bundle, PINNED_STEAM_BUILD)
            .unwrap()
            .tables;
        let rows = crate::player_attacks::BundleRows::of(&tables);
        let source = GameSource::open(&game_dir()).unwrap();
        let sources = crate::player_attacks::read_sources(&source, &rows).unwrap();
        // The clip, skeleton and attachment readers on the pinned paks: `grtswrd_prim_c1` holds
        // frames 0 to 73 and moves the root 1.5859723 m forward (its motion parameters'
        // `m_fDistance`), the player's skeleton has 130 joints from `Xform`, and the greatsword
        // hangs on `righthandattach`.
        let bodies = &sources.bodies;
        assert_eq!(bodies.player.joints.len(), PLAYER_JOINTS);
        assert_eq!(bodies.player.joints[0].name, "Xform");
        let swing = bodies.clip(GREATSWORD_LIGHT_CLIP).unwrap();
        assert_eq!(
            (swing.start_frame, swing.end_frame),
            (0, GREATSWORD_LIGHT_FRAMES)
        );
        let root = &swing.tracks[&bodies.player.joints[0].controller_id];
        let moved = root.position_at(swing.frame_at(swing.duration_s()));
        assert!((moved.unwrap()[1] - GREATSWORD_LIGHT_DISTANCE_M).abs() < 1e-4);
        assert_eq!(
            bodies
                .equip_attachments
                .get("greatsword")
                .map(String::as_str),
            Some("righthandattach")
        );
        let (stances, _) = crate::player_attacks::build_stances(&sources, &rows);
        let committed: Vec<nw_content::player_attacks::Stance> = serde_json::from_str(
            &std::fs::read_to_string(
                repo_root().join("bins/sim/src/world/testdata/player-attacks-25810808.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(stances, committed);
    }
}
