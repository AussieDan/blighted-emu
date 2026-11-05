// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool combat-check` (M6-A spec section 11.2): whether every vitals spawn of a level
//! finds its combat rows, and whether the mob max health of spec section 4.1 matches the health
//! that nwdb.info computed for its creature records. It reports counts and datasheet ids only.
//!
//! A health matches within a thousandth (at least 1): nwdb's own rounding differs by a few points
//! in thousands. nwdb sometimes computes a record's health at a level other than its `Level`
//! field; a health that matches our health of the same type at another level is counted apart.

use std::path::Path;

use nw_content::combat::CombatData;
use nw_formats::objectstream::lowercase_crc32;
use nw_sim_core::mob_max_health;
pub use nw_world::records::VitalsSpawn;
use serde::Deserialize;
use thiserror::Error;

/// The nwdb.info creature records, as the operator saved them.
pub const DEFAULT_NWDB_DIR: &str = "local-data/nwdb.info/records/creature";
/// The open world level.
pub const DEFAULT_LEVEL: &str = "newworld_vitaeeterna";
/// Most mismatches a report keeps.
pub const MAX_MISMATCHES: usize = 20;
/// Largest difference, as a share of nwdb's health, that still counts as the same health.
const HEALTH_TOLERANCE_SHARE: f64 = 0.001;
/// Smallest allowed difference (rounding of small healths).
const MIN_HEALTH_TOLERANCE: f64 = 1.0;
/// Extension of a record file.
const RECORD_EXTENSION: &str = "json";

/// One nwdb.info creature record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NwdbCreature {
    /// The vitals id.
    pub vitals_id: String,
    /// The level nwdb computed the health at.
    pub level: u32,
    /// nwdb's max health.
    pub health: u64,
}

/// The fields of a record file that the check reads.
#[derive(Deserialize)]
struct RecordFile {
    data: RecordData,
}

#[derive(Deserialize)]
struct RecordData {
    #[serde(rename = "VitalsID")]
    vitals_id: String,
    #[serde(rename = "Level")]
    level: u32,
    #[serde(rename = "healthFinal")]
    health: u64,
}

/// The records of a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NwdbRecords {
    /// Records that parsed.
    pub creatures: Vec<NwdbCreature>,
    /// Record files that did not parse.
    pub unreadable: usize,
}

/// A record folder that cannot be listed or read.
#[derive(Debug, Error)]
#[error("nwdb records in {path}: {source}")]
pub struct NwdbError {
    /// The folder or file.
    pub path: String,
    /// Why.
    #[source]
    pub source: std::io::Error,
}

/// Reads every `.json` record of `dir`; a file that does not parse is counted, not fatal.
///
/// # Errors
///
/// Returns [`NwdbError`] when the folder or a file cannot be read.
pub fn read_nwdb(dir: &Path) -> Result<NwdbRecords, NwdbError> {
    let failed = |path: &Path| {
        let path = path.display().to_string();
        move |source| NwdbError { path, source }
    };
    let mut paths = std::fs::read_dir(dir)
        .map_err(failed(dir))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed(dir))?;
    paths.retain(|path| path.extension().is_some_and(|ext| ext == RECORD_EXTENSION));
    paths.sort();
    let mut records = NwdbRecords::default();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(failed(&path))?;
        match serde_json::from_str::<RecordFile>(&text) {
            Ok(file) => records.creatures.push(NwdbCreature {
                vitals_id: file.data.vitals_id,
                level: file.data.level,
                health: file.data.health,
            }),
            Err(_) => records.unreadable += 1,
        }
    }
    Ok(records)
}

/// A creature whose health differs from nwdb's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// The vitals id.
    pub vitals_id: String,
    /// The level.
    pub level: u32,
    /// nwdb's health.
    pub nwdb: u64,
    /// Ours; `None` without a type or level row.
    pub ours: Option<u32>,
}

/// The result of the check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CombatCheck {
    /// Vitals spawns of the level.
    pub spawns: usize,
    /// Spawns without a mob type row.
    pub without_type: usize,
    /// Spawns without a level row.
    pub without_level: usize,
    /// nwdb records compared.
    pub nwdb_records: usize,
    /// Records whose health matched.
    pub nwdb_matched: usize,
    /// Records whose health matched our health of the same type at another level.
    pub nwdb_other_level: usize,
    /// The first [`MAX_MISMATCHES`] mismatches.
    pub mismatches: Vec<Mismatch>,
}

/// Our max health of a mob type at `level`.
fn our_health(data: &CombatData, vitals_crc: u32, level: u32) -> Option<u32> {
    let kind = data.mob_type(vitals_crc)?;
    let row = data.level(level)?;
    Some(mob_max_health(
        row.base_max_health,
        kind.health_mod,
        kind.category_health_mod,
    ))
}

/// Whether `ours` is `nwdb`'s health within the tolerance.
fn same_health(ours: u32, nwdb: u64) -> bool {
    let tolerance = (nwdb as f64 * HEALTH_TOLERANCE_SHARE).max(MIN_HEALTH_TOLERANCE);
    u64::from(ours).abs_diff(nwdb) as f64 <= tolerance
}

/// Whether our health of the type at some other level is `nwdb`'s.
fn at_other_level(data: &CombatData, vitals_crc: u32, nwdb: u64) -> bool {
    data.mob_type(vitals_crc).is_some_and(|kind| {
        data.levels().any(|(_, row)| {
            same_health(
                mob_max_health(
                    row.base_max_health,
                    kind.health_mod,
                    kind.category_health_mod,
                ),
                nwdb,
            )
        })
    })
}

/// Checks `spawns` against `data`, and our health against `nwdb`.
pub fn check(data: &CombatData, spawns: &[VitalsSpawn], nwdb: &[NwdbCreature]) -> CombatCheck {
    let mut report = CombatCheck {
        spawns: spawns.len(),
        without_type: spawns
            .iter()
            .filter(|spawn| data.mob_type(spawn.vitals_id).is_none())
            .count(),
        without_level: spawns
            .iter()
            .filter(|spawn| data.level(spawn.level).is_none())
            .count(),
        nwdb_records: nwdb.len(),
        ..CombatCheck::default()
    };
    for creature in nwdb {
        let crc = lowercase_crc32(&creature.vitals_id);
        let ours = our_health(data, crc, creature.level);
        if ours.is_some_and(|ours| same_health(ours, creature.health)) {
            report.nwdb_matched += 1;
        } else if at_other_level(data, crc, creature.health) {
            report.nwdb_other_level += 1;
        } else if report.mismatches.len() < MAX_MISMATCHES {
            report.mismatches.push(Mismatch {
                vitals_id: creature.vitals_id.clone(),
                level: creature.level,
                nwdb: creature.health,
                ours,
            });
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use nw_content::combat::{CombatRows, CombatSource, LevelRow, MobType, WeaponRow};
    use nw_formats::objectstream::lowercase_crc32;

    use super::*;

    const LEVEL: u32 = 20;
    const LEVEL_HEALTH: f64 = 1000.0;
    const HALF: f64 = 0.5;

    fn data() -> CombatData {
        CombatData::build(source(), &CombatRows::default()).expect("data")
    }

    fn source() -> CombatSource {
        let rows = CombatRows::default();
        CombatSource {
            base_types: vec![(
                "Boar".to_owned(),
                MobType {
                    health_mod: HALF,
                    category_health_mod: 1.0,
                    damage_mod: 1.0,
                    creature_type: None,
                },
            )],
            levels: vec![(
                LEVEL,
                LevelRow {
                    base_max_health: LEVEL_HEALTH,
                    base_damage: 1.0,
                    physical_armor_rating: 0.0,
                    elemental_armor_rating: 0.0,
                    gear_score: 0,
                },
            )],
            weapons: vec![(
                rows.weapon_id.clone(),
                WeaponRow {
                    base_damage: 1.0,
                    crit_chance: 0.0,
                    crit_multiplier: 1.0,
                    spread_shot: false,
                    max_loaded_ammo: 0,
                    reload_ms: 0,
                    stance: 0,
                    block_stamina_damage: 0.0,
                    block_stability: 0.0,
                    ranged_block: None,
                },
            )],
            player_health: vec![(rows.player_vitals_id.clone(), 1)],
            ..CombatSource::default()
        }
    }

    fn spawn(id: &str, level: u32) -> VitalsSpawn {
        VitalsSpawn {
            vitals_id: lowercase_crc32(id),
            level,
            ..VitalsSpawn::default()
        }
    }

    fn creature(id: &str, level: u32, health: u64) -> NwdbCreature {
        NwdbCreature {
            vitals_id: id.to_owned(),
            level,
            health,
        }
    }

    #[test]
    fn every_spawn_with_rows_counts() {
        let report = check(&data(), &[spawn("Boar", LEVEL), spawn("boar", LEVEL)], &[]);
        assert_eq!(
            (report.spawns, report.without_type, report.without_level),
            (2, 0, 0)
        );
    }

    #[test]
    fn a_spawn_without_a_type_or_level_is_counted() {
        let report = check(
            &data(),
            &[spawn("Wolf", LEVEL), spawn("Boar", LEVEL + 1)],
            &[],
        );
        assert_eq!((report.without_type, report.without_level), (1, 1));
    }

    #[test]
    fn nwdb_health_within_a_thousandth_matches() {
        let nwdb = [
            creature("Boar", LEVEL, 501),
            creature("Boar", LEVEL, 502),
            creature("Wolf", LEVEL, 500),
        ];
        let report = check(&data(), &[], &nwdb);
        assert_eq!((report.nwdb_records, report.nwdb_matched), (3, 1));
        assert_eq!(
            report.mismatches,
            vec![
                Mismatch {
                    vitals_id: "Boar".to_owned(),
                    level: LEVEL,
                    nwdb: 502,
                    ours: Some(500),
                },
                Mismatch {
                    vitals_id: "Wolf".to_owned(),
                    level: LEVEL,
                    nwdb: 500,
                    ours: None,
                },
            ]
        );
    }

    #[test]
    fn a_large_health_matches_within_a_thousandth() {
        let big = CombatData::build(
            CombatSource {
                levels: vec![(
                    LEVEL,
                    LevelRow {
                        base_max_health: 20_000.0,
                        base_damage: 1.0,
                        physical_armor_rating: 0.0,
                        elemental_armor_rating: 0.0,
                        gear_score: 0,
                    },
                )],
                ..source()
            },
            &CombatRows::default(),
        )
        .expect("data");
        let report = check(
            &big,
            &[],
            &[
                creature("Boar", LEVEL, 10_009),
                creature("Boar", LEVEL, 10_011),
            ],
        );
        assert_eq!(report.nwdb_matched, 1);
    }

    #[test]
    fn a_health_of_another_level_is_counted_apart() {
        let two_levels = CombatData::build(
            CombatSource {
                levels: vec![
                    (
                        LEVEL,
                        LevelRow {
                            base_max_health: LEVEL_HEALTH,
                            base_damage: 1.0,
                            physical_armor_rating: 0.0,
                            elemental_armor_rating: 0.0,
                            gear_score: 0,
                        },
                    ),
                    (
                        LEVEL + 10,
                        LevelRow {
                            base_max_health: 3000.0,
                            base_damage: 1.0,
                            physical_armor_rating: 0.0,
                            elemental_armor_rating: 0.0,
                            gear_score: 0,
                        },
                    ),
                ],
                ..source()
            },
            &CombatRows::default(),
        )
        .expect("data");
        let report = check(&two_levels, &[], &[creature("Boar", LEVEL, 1500)]);
        assert_eq!((report.nwdb_matched, report.nwdb_other_level), (0, 1));
        assert!(report.mismatches.is_empty());
    }

    #[test]
    fn mismatches_are_capped() {
        let nwdb: Vec<NwdbCreature> = (0..MAX_MISMATCHES + 5)
            .map(|_| creature("Wolf", LEVEL, 1))
            .collect();
        let report = check(&data(), &[], &nwdb);
        assert_eq!(report.mismatches.len(), MAX_MISMATCHES);
        assert_eq!(
            report.nwdb_records - report.nwdb_matched,
            MAX_MISMATCHES + 5
        );
    }

    #[test]
    fn nwdb_records_are_read_from_their_folder() {
        let dir = tempfile::tempdir().expect("dir");
        let record = r#"{"success":true,"data":{"VitalsID":"Boar","Level":20,"healthFinal":500,"Name":"x"}}"#;
        std::fs::write(dir.path().join("boar.json"), record).expect("write");
        std::fs::write(dir.path().join("broken.json"), "{").expect("write");
        std::fs::write(dir.path().join("notes.txt"), "skip").expect("write");
        let read = read_nwdb(dir.path()).expect("read");
        assert_eq!(read.creatures, vec![creature("Boar", LEVEL, 500)]);
        assert_eq!(read.unreadable, 1);
    }
}
