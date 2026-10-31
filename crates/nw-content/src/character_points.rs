// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules of a character's points (progression note, section 6): the attribute points each
//! level gives, the attributes' smallest and largest values, the weapon progression points and
//! the ability tables a character maps abilities in.

use std::collections::{BTreeMap, BTreeSet};

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;
use crate::progression::LEVEL_TABLE;

/// The attributes, by their id: the client enum `CharacterAttributeType` (`FUN_142797540`:
/// Constitution 0, Focus 1, Strength 2, Dexterity 3, Intelligence 4). Each names its
/// `AttributeDefinition` table.
pub const ATTRIBUTE_NAMES: [&str; ATTRIBUTE_COUNT] = [
    "Constitution",
    "Focus",
    "Strength",
    "Dexterity",
    "Intelligence",
];
/// How many attributes there are.
pub const ATTRIBUTE_COUNT: usize = 5;
/// The attribute id of Constitution.
pub const CONSTITUTION: u32 = 0;
/// The `IsAbility` cell of a progression point that is an ability-tree point.
const IS_ABILITY: &str = "1";

/// The values one attribute may take: the `Level` rows of its `AttributeDefinition` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeRange {
    /// Its value without spent points (retail: 5 for every attribute of a new character).
    pub base: u32,
    /// Its largest value.
    pub max: u32,
}

/// One weapon progression point (`ProgressionPointData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointRule {
    /// `ProgressionPointId`.
    pub id: String,
    /// The CRC-32 of the lowercase id: its id on the wire.
    pub crc: u32,
    /// The CRC-32 of the lowercase `PointPoolId`.
    pub pool: u32,
    /// `MaxLevel`.
    pub max_level: u16,
    /// `RequiredProgressionPointId` (its CRC) and `RequiredProgressionPointLevel`.
    pub requires: Option<(u32, u16)>,
    /// Whether it is an active ability (`IsActiveAbility` of its `AbilityData` row): an active
    /// ability goes into a mapped slot when it is applied.
    pub active: bool,
}

/// The rules of a character's points.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharacterPoints {
    /// `AttributePoints` of each level, level 0 first.
    level_points: Vec<u32>,
    /// `AttributeRespecCost` of each level, by level: the coins a refund of the spent
    /// attribute points (2409) costs (Observed, retail 2026-08-26 at level 69: the coins fell
    /// by 5,000 at each of 38 refunds, the level row's cost).
    respec_costs: BTreeMap<u32, u64>,
    /// The range of each attribute, by attribute id; empty when a table is missing.
    attributes: Vec<AttributeRange>,
    /// The ability-tree points of the weapon pools, by CRC.
    points: BTreeMap<u32, PointRule>,
    /// The CRC of each `AbilityData` table id, in table order.
    ability_tables: Vec<u32>,
    /// Each weapon pool (CRC) and its `VersionNumber`, in table order.
    pool_versions: Vec<(u32, u32)>,
    /// The `Health` of each Constitution `Level` row, by value.
    constitution_health: BTreeMap<u32, u32>,
}

/// One `ProgressionPointData` row, as [`CharacterPoints::build`] takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointRow<'a> {
    /// `ProgressionPointId`.
    pub id: &'a str,
    /// `PointPoolId`.
    pub pool: &'a str,
    /// `MaxLevel`.
    pub max_level: i64,
    /// `RequiredProgressionPointId` and `RequiredProgressionPointLevel`, when both are set.
    pub requires: Option<(&'a str, i64)>,
}

impl CharacterPoints {
    /// The rules of `tables`; a missing table leaves its part empty.
    pub fn of_tables(tables: &Tables) -> Self {
        let levels = tables
            .experience_data
            .iter()
            .filter(|table| table.unique_id.eq_ignore_ascii_case(LEVEL_TABLE))
            .flat_map(|table| &table.rows)
            .map(|row| (row.level_number, row.attribute_points));
        let attribute_levels = tables.attribute_definition.iter().flat_map(|table| {
            table
                .rows
                .iter()
                .map(move |row| (table.unique_id.as_str(), row.level))
        });
        let active = tables
            .ability_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.is_active_ability == Some(true))
            .map(|row| row.ability_id.as_str());
        let pools = tables
            .progression_pool_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| Some((row.progression_pool_id.as_str(), row.version_number?)));
        let points = tables
            .progression_point_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.is_ability == IS_ABILITY)
            .filter_map(|row| {
                Some(PointRow {
                    id: &row.progression_point_id,
                    pool: row.point_pool_id.as_deref()?,
                    max_level: row.max_level,
                    requires: row
                        .required_progression_point_id
                        .as_deref()
                        .zip(row.required_progression_point_level),
                })
            });
        let respec_costs = tables
            .experience_data
            .iter()
            .filter(|table| table.unique_id.eq_ignore_ascii_case(LEVEL_TABLE))
            .flat_map(|table| &table.rows)
            .map(|row| (row.level_number, row.attribute_respec_cost));
        let constitution_health = tables
            .attribute_definition
            .iter()
            .filter(|table| {
                table
                    .unique_id
                    .eq_ignore_ascii_case(ATTRIBUTE_NAMES[CONSTITUTION as usize])
            })
            .flat_map(|table| &table.rows)
            .map(|row| (row.level, row.health));
        Self::build(
            levels,
            attribute_levels,
            tables
                .ability_data
                .iter()
                .map(|table| table.unique_id.as_str()),
            active,
            pools,
            points,
        )
        .with_respec_costs(respec_costs)
        .with_constitution_health(constitution_health)
    }

    /// These rules with the Constitution `rows` (`Level`, `Health`); a negative cell is 0.
    #[must_use]
    pub fn with_constitution_health(mut self, rows: impl IntoIterator<Item = (i64, i64)>) -> Self {
        self.constitution_health = rows
            .into_iter()
            .map(|(level, health)| (clamp(level), clamp(health)))
            .collect();
        self
    }

    /// The max health that Constitution `value` gives: its row's `Health`, else the row below
    /// it; 0 below the first row or without the table.
    pub fn constitution_health(&self, value: u32) -> u32 {
        self.constitution_health
            .range(..=value)
            .next_back()
            .map_or(0, |(_, health)| *health)
    }

    /// These rules with the attribute refund cost `costs` of each level (level number,
    /// `AttributeRespecCost`); a negative cost is 0.
    #[must_use]
    pub fn with_respec_costs(mut self, costs: impl IntoIterator<Item = (i64, i64)>) -> Self {
        self.respec_costs = costs
            .into_iter()
            .filter_map(|(level, cost)| {
                Some((u32::try_from(level).ok()?, u64::try_from(cost.max(0)).ok()?))
            })
            .collect();
        self
    }

    /// The coins a refund of the spent attribute points costs a character of `level`; 0 for a
    /// level the table does not list.
    pub fn attribute_respec_cost(&self, level: u32) -> u64 {
        self.respec_costs.get(&level).copied().unwrap_or_default()
    }

    /// The rules of the level rows `levels` (level number, `AttributePoints`), the attribute
    /// rows `attribute_levels` (`AttributeDefinition` table id, `Level`), the ability tables
    /// `ability_tables` (ids, in table order), the ids of the `active` abilities, the weapon
    /// `pools` (id, `VersionNumber`) and the ability-tree `points` of every pool; the points
    /// of a pool without a version are not weapon points and are left out.
    pub fn build<'a>(
        levels: impl IntoIterator<Item = (i64, i64)>,
        attribute_levels: impl IntoIterator<Item = (&'a str, i64)> + Clone,
        ability_tables: impl IntoIterator<Item = &'a str>,
        active: impl IntoIterator<Item = &'a str>,
        pools: impl IntoIterator<Item = (&'a str, i64)>,
        points: impl IntoIterator<Item = PointRow<'a>>,
    ) -> Self {
        let mut levels: Vec<(i64, i64)> = levels.into_iter().collect();
        levels.sort_unstable();
        let ranges: Option<Vec<AttributeRange>> = ATTRIBUTE_NAMES
            .iter()
            .map(|name| {
                let levels: Vec<u32> = attribute_levels
                    .clone()
                    .into_iter()
                    .filter(|(table, _)| table.eq_ignore_ascii_case(name))
                    .map(|(_, level)| clamp(level))
                    .collect();
                Some(AttributeRange {
                    base: *levels.iter().min()?,
                    max: *levels.iter().max()?,
                })
            })
            .collect();
        let active: BTreeSet<u32> = active.into_iter().map(lowercase_crc32).collect();
        let pool_versions: Vec<(u32, u32)> = pools
            .into_iter()
            .map(|(pool, version)| (lowercase_crc32(pool), clamp(version)))
            .collect();
        let points = points
            .into_iter()
            .filter_map(|row| {
                let pool = lowercase_crc32(row.pool);
                pool_versions
                    .iter()
                    .any(|(weapon, _)| *weapon == pool)
                    .then(|| {
                        let crc = lowercase_crc32(row.id);
                        let rule = PointRule {
                            id: row.id.to_owned(),
                            crc,
                            pool,
                            max_level: small(row.max_level),
                            requires: row
                                .requires
                                .map(|(id, level)| (lowercase_crc32(id), small(level))),
                            active: active.contains(&crc),
                        };
                        (crc, rule)
                    })
            })
            .collect();
        Self {
            level_points: levels
                .into_iter()
                .map(|(_, points)| clamp(points))
                .collect(),
            attributes: ranges.unwrap_or_default(),
            points,
            ability_tables: ability_tables.into_iter().map(lowercase_crc32).collect(),
            pool_versions,
            respec_costs: BTreeMap::new(),
            constitution_health: BTreeMap::new(),
        }
    }

    /// The attribute points a character of `level` (zero-based) has been given: the
    /// `AttributePoints` of every level up to it (retail: 10 at level 2, 480 at level 66).
    pub fn attribute_points_at(&self, level: u32) -> u32 {
        let levels = usize::try_from(level).map_or(usize::MAX, |level| level.saturating_add(1));
        self.level_points
            .iter()
            .take(levels)
            .fold(0, |sum, points| sum.saturating_add(*points))
    }

    /// The range of attribute `id`; `None` for an id the client does not have, or without the
    /// attribute tables.
    pub fn attribute(&self, id: u32) -> Option<AttributeRange> {
        self.attributes.get(usize::try_from(id).ok()?).copied()
    }

    /// The weapon progression point whose wire id is `crc`.
    pub fn point(&self, crc: u32) -> Option<&PointRule> {
        self.points.get(&crc)
    }

    /// The weapon progression point `id` (any case).
    pub fn point_by_id(&self, id: &str) -> Option<&PointRule> {
        self.points.get(&lowercase_crc32(id))
    }

    /// The CRC of every ability table, in table order.
    pub fn ability_tables(&self) -> &[u32] {
        &self.ability_tables
    }

    /// Each weapon pool (CRC) and its `VersionNumber`.
    pub fn pool_versions(&self) -> &[(u32, u32)] {
        &self.pool_versions
    }
}

/// A table number as a count: negative is 0, above `u32::MAX` is `u32::MAX`.
fn clamp(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// A table number as a small count: negative is 0, above `u16::MAX` is `u16::MAX`.
fn small(value: i64) -> u16 {
    u16::try_from(value.max(0)).unwrap_or(u16::MAX)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Retail `AttributePoints` of levels 0 to 3 (`javelindata_xpamountsbylevel`).
    const LEVEL_POINTS: [i64; 4] = [0, 5, 5, 5];
    /// Retail `AttributeDefinition` levels run 5 to 500.
    const BASE: i64 = 5;
    const MAX: i64 = 500;
    const POOL: &str = "GreatSwordAbilityTable";
    /// The ability table of the pool (another case of the same id).
    const TABLE: &str = "GreatswordAbilityTable";
    const COMBO: &str = "Ability_Greatsword_Combo";
    const UPGRADE: &str = "Upgrade_Greatsword_Combo_1";
    /// Its retail `VersionNumber`.
    const POOL_VERSION: i64 = 1;
    const MAX_LEVEL: i64 = 10;
    /// A territory pool: no version.
    const TERRITORY_POINT: &str = "Territory_Standing_Point";

    /// The retail rows of a new character's greatsword tree start.
    pub(crate) fn rules() -> CharacterPoints {
        let attribute_levels: Vec<(&str, i64)> = ATTRIBUTE_NAMES
            .iter()
            .flat_map(|name| [(*name, MAX), (*name, BASE)])
            .collect();
        let point = |id, requires| PointRow {
            id,
            pool: POOL,
            max_level: MAX_LEVEL,
            requires,
        };
        CharacterPoints::build(
            (0..)
                .zip(LEVEL_POINTS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
            attribute_levels,
            [TABLE],
            [COMBO],
            [(POOL, POOL_VERSION)],
            [
                point(COMBO, None),
                point(UPGRADE, Some((COMBO, 1))),
                PointRow {
                    id: TERRITORY_POINT,
                    pool: "TerritoryRankBonus_2",
                    max_level: MAX_LEVEL,
                    requires: None,
                },
            ],
        )
    }

    #[test]
    fn the_rules_come_from_the_rows() {
        let rules = rules();
        assert_eq!(rules.attribute_points_at(0), 0);
        // Retail: 10 unspent points after the first level up (level 0 to 2).
        assert_eq!(rules.attribute_points_at(2), 10);
        assert_eq!(rules.attribute_points_at(u32::MAX), 15);
        assert_eq!(
            rules.attribute(2),
            Some(AttributeRange {
                base: clamp(BASE),
                max: clamp(MAX)
            })
        );
        assert_eq!(rules.attribute(5), None);
        let combo = rules.point_by_id(COMBO).expect("combo");
        assert!(combo.active);
        assert_eq!(combo.pool, lowercase_crc32(POOL));
        assert_eq!(combo.requires, None);
        let upgrade = rules.point(lowercase_crc32(UPGRADE)).expect("upgrade");
        assert!(!upgrade.active);
        assert_eq!(upgrade.requires, Some((combo.crc, 1)));
        assert_eq!(upgrade.max_level, 10);
        assert!(rules.point_by_id(TERRITORY_POINT).is_none());
        assert_eq!(rules.ability_tables(), [lowercase_crc32(POOL)]);
        assert_eq!(rules.pool_versions(), [(lowercase_crc32(POOL), 1)]);
    }

    /// Retail `AttributeRespecCost` of levels 58 and 59 (content bundle 25810808).
    const RESPEC_COSTS: [(i64, i64); 3] = [(59, 5_000), (58, 0), (-1, 7)];

    #[test]
    fn a_refund_costs_the_level_rows_coins() {
        let rules = rules().with_respec_costs(RESPEC_COSTS);
        assert_eq!(rules.attribute_respec_cost(59), 5_000);
        assert_eq!(rules.attribute_respec_cost(58), 0);
        assert_eq!(
            rules.attribute_respec_cost(60),
            0,
            "a level the table does not list"
        );
        assert_eq!(rules.attribute_points_at(2), 10, "the other rules stay");
    }

    /// Retail Constitution `Health` of a few `Level` rows (content bundle 25810808).
    const CONSTITUTION_ROWS: [(i64, i64); 4] = [(5, 0), (10, 200), (200, 6_419), (-1, 9)];

    #[test]
    fn constitution_gives_its_rows_health() {
        let rules = rules().with_constitution_health(CONSTITUTION_ROWS);
        assert_eq!(rules.constitution_health(5), 0);
        assert_eq!(rules.constitution_health(10), 200);
        assert_eq!(rules.constitution_health(200), 6_419);
        assert_eq!(rules.constitution_health(150), 200, "the row below");
        assert_eq!(rules.constitution_health(0), 9, "a negative level is 0");
        assert_eq!(CharacterPoints::default().constitution_health(200), 0);
    }

    #[test]
    fn tables_without_the_rows_give_no_rules() {
        let rules = CharacterPoints::of_tables(&Tables::default());
        assert_eq!(rules.attribute_points_at(10), 0);
        assert_eq!(rules.attribute(0), None);
        assert!(rules.point_by_id(COMBO).is_none());
        assert!(rules.ability_tables().is_empty());
    }
}
