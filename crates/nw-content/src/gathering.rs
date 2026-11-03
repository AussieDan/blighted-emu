// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gatherables (gather note, 2026-09-18): a gather request names its gatherable by the CRC-32
//! of its lowercase `GatherableID`, and the gatherable's `FinalLootTable` gives the items (see
//! [`crate::loot`]). A loot crate's items come from its variant instead ([`CrateLoot`]).

use std::collections::{BTreeSet, HashMap};

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;

/// The tradeskill of a gatherable that is skinned.
const SKINNING: &str = "Skinning";
/// The `Restriction` of a gatherable that needs its tradeskill's tool (a corpse needs a skinning
/// knife; bushes say `ItemRequired` and retail gathered them by hand).
const TOOL_REQUIRED: &str = "ToolRequired";
/// The `GatheringType` of a gatherable taken by hand, not with a tool's action.
const BY_HAND: &str = "None";
/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;

/// One gatherable of `javelindata_gatherables`.
#[derive(Debug, Clone, PartialEq)]
pub struct Gatherable {
    /// `GatherableID`.
    pub id: String,
    /// `BaseGatherTime`, in ms.
    pub gather_ms: u64,
    /// `MinRespawnRate` and `MaxRespawnRate`, in ms.
    pub respawn_ms: (u64, u64),
    /// `FinalLootTable`.
    pub loot_table: Option<String>,
    /// `IsLootContainer`: a crate whose items come from its variant ([`CrateLoot`]).
    pub loot_container: bool,
    /// `Tradeskill`: Skinning for a corpse to skin.
    pub tradeskill: Option<String>,
    /// It needs its tradeskill's tool (`Restriction` ToolRequired).
    pub tool_required: bool,
    /// Its `GatheringType` is `None`: it is taken by hand, not with a tool's action (chopping,
    /// cutting, dressing, mining, the Azoth staff). Retail put the drawn weapon away for 1,351
    /// of 1,449 gathers by hand and for none of 827 with a tool (721 ledgers).
    pub by_hand: bool,
    /// `GameEventID`: the game event of a gather of it (`MSQ_CorruptedNodule_1_interact` for a
    /// quest nodule, which a quest's `TaskGameEvent` counts).
    pub game_event: Option<String>,
}

/// The gatherables by the CRC-32 of their lowercase id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gatherables {
    by_crc: HashMap<u32, Gatherable>,
}

impl Gatherable {
    /// It is a corpse to skin (`Tradeskill` Skinning).
    pub fn skinning(&self) -> bool {
        self.tradeskill.as_deref() == Some(SKINNING)
    }
}

/// Seconds of a datasheet as whole ms.
fn ms_of(seconds: f64) -> u64 {
    // Gather and respawn times of a few minutes at most.
    (seconds * MS_PER_SECOND).round().max(0.0) as u64
}

impl Gatherables {
    /// The gatherables of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .gatherable_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| Gatherable {
                    id: row.gatherable_id.clone(),
                    gather_ms: ms_of(row.base_gather_time),
                    respawn_ms: (ms_of(row.min_respawn_rate), ms_of(row.max_respawn_rate)),
                    loot_table: row.final_loot_table.clone(),
                    loot_container: row.is_loot_container.unwrap_or_default(),
                    tradeskill: row.tradeskill.clone(),
                    tool_required: row.restriction.as_deref() == Some(TOOL_REQUIRED),
                    by_hand: row
                        .gathering_type
                        .as_deref()
                        .is_none_or(|kind| kind == BY_HAND),
                    game_event: row.game_event_id.clone().filter(|event| !event.is_empty()),
                }),
        )
    }

    /// The gatherables of `gatherables`.
    pub fn build(gatherables: impl IntoIterator<Item = Gatherable>) -> Self {
        Self {
            by_crc: gatherables
                .into_iter()
                .map(|gatherable| (lowercase_crc32(&gatherable.id), gatherable))
                .collect(),
        }
    }

    /// The gatherable whose lowercase id has the CRC-32 `crc`.
    pub fn get(&self, crc: u32) -> Option<&Gatherable> {
        self.by_crc.get(&crc)
    }
}

/// What a gather of a gatherable asks of the gatherer besides its tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatherRule {
    /// `RequiredTradeskillLevel`: the rank its `Tradeskill` must reach (0-based, as the rank
    /// tables count).
    pub required_rank: u32,
    /// `ItemRestrictionId` and `ItemRestrictionQuantity`: an item the bag must hold (a quest
    /// item: `0402_GruelBarrel` asks for one `0402_0401_BileSoakedMeat`).
    pub required_item: Option<(String, u32)>,
}

/// The gather rules of the gatherables that have one, by the CRC-32 of their lowercase id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatherRules {
    by_crc: HashMap<u32, GatherRule>,
}

impl GatherRules {
    /// The rules of `tables`: the gatherables with a required rank or item.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .gatherable_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    let count = |cell: Option<i64>| {
                        cell.and_then(|value| u32::try_from(value).ok())
                            .unwrap_or_default()
                    };
                    let required_item = row
                        .item_restriction_id
                        .clone()
                        .filter(|item| !item.is_empty())
                        .map(|item| (item, count(row.item_restriction_quantity).max(1)));
                    (
                        row.gatherable_id.clone(),
                        GatherRule {
                            required_rank: count(row.required_tradeskill_level),
                            required_item,
                        },
                    )
                }),
        )
    }

    /// The rules of `rules` (gatherable id, rule); a rule that asks nothing is left out and the
    /// first row of an id wins.
    pub fn build(rules: impl IntoIterator<Item = (String, GatherRule)>) -> Self {
        let mut by_crc = HashMap::new();
        for (id, rule) in rules {
            if rule != GatherRule::default() {
                by_crc.entry(lowercase_crc32(&id)).or_insert(rule);
            }
        }
        Self { by_crc }
    }

    /// The rule of the gatherable whose lowercase id has the CRC-32 `crc`: none asks nothing.
    pub fn of(&self, crc: u32) -> GatherRule {
        self.by_crc.get(&crc).cloned().unwrap_or_default()
    }
}

/// The detection spheres of the gatherables whose variant has one (`javelindata_variations`
/// `DetectionVolumeSphereRadius`, 238 rows in build 25810808, all quest objects), by the CRC-32 of
/// the lowercase gatherable id. A player who comes into the sphere sets off the gatherable's game
/// event without a gather (Observed: retail `9401_befriendhorse` capture, the player came within
/// 8.9 m of a `94_BeastTorch` (radius 9) at 133.69 s and `9401_Torch` was complete in that
/// record; the client sent no interaction for any torch).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DetectionSpheres {
    by_crc: HashMap<u32, f32>,
}

impl DetectionSpheres {
    /// The spheres of `tables`: each variant row with a positive radius names its gatherable
    /// (`GatherableEntryID`, else `GatherableEntryId`).
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .variation_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let gatherable = row
                        .gatherable_entry_id
                        .as_deref()
                        .or(row.gatherable_entry_id_2.as_deref())
                        .filter(|id| !id.is_empty())?;
                    let radius_m = row.detection_volume_sphere_radius? as f32;
                    Some((gatherable.to_owned(), radius_m))
                }),
        )
    }

    /// The spheres of `spheres` (gatherable id, radius in metres); a radius of 0 or less is left
    /// out and the first row of an id wins.
    pub fn build(spheres: impl IntoIterator<Item = (String, f32)>) -> Self {
        let mut by_crc = HashMap::new();
        for (id, radius_m) in spheres {
            if radius_m > 0.0 {
                by_crc.entry(lowercase_crc32(&id)).or_insert(radius_m);
            }
        }
        Self { by_crc }
    }

    /// The sphere radius of the gatherable whose lowercase id has the CRC-32 `crc`, in metres.
    pub fn radius_of(&self, crc: u32) -> Option<f32> {
        self.by_crc.get(&crc).copied()
    }
}

/// A state of a cyclic gatherable that a gatherable shows (finding "cyclic gatherables",
/// 2026-10-03). A `javelindata_variations` row with `NumCyclicStates` shows the variant
/// `Gatherable_<n>_Variant` in its state n, from 0; a gather of that variant's gatherable turns
/// it to the next state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyclicState {
    /// `VariantID` of the cyclic gatherable.
    pub cyclic: String,
    /// The state this gatherable shows; `None` when it shows in more than one state (a turned
    /// stone shows in all of them), so the gatherer's turns give the state.
    pub state: Option<u32>,
    /// `NumCyclicStates`.
    pub states: u32,
}

/// The cyclic states of each gatherable, by the CRC-32 of its lowercase id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CyclicGatherables {
    by_crc: HashMap<u32, Vec<CyclicState>>,
}

impl CyclicGatherables {
    /// The cyclic gatherables of `tables`: each state's variant row names its gatherable
    /// (`GatherableEntryID`, else `GatherableEntryId`).
    pub fn from_tables(tables: &Tables) -> Self {
        let rows: Vec<_> = tables
            .variation_data
            .iter()
            .flat_map(|table| &table.rows)
            .collect();
        let gatherable_of: HashMap<String, &str> = rows
            .iter()
            .filter_map(|row| {
                let gatherable = row
                    .gatherable_entry_id
                    .as_deref()
                    .or(row.gatherable_entry_id_2.as_deref())
                    .filter(|id| !id.is_empty())?;
                Some((row.variant_id.as_deref()?.to_lowercase(), gatherable))
            })
            .collect();
        Self::build(rows.iter().filter_map(|row| {
            let states = row
                .num_cyclic_states
                .and_then(|states| usize::try_from(states).ok())
                .filter(|states| *states > 0)?;
            let variants = [
                &row.gatherable_0_variant,
                &row.gatherable_1_variant,
                &row.gatherable_2_variant,
                &row.gatherable_3_variant,
                &row.gatherable_4_variant,
            ];
            let gatherables = variants
                .into_iter()
                .take(states)
                .map(|variant| {
                    variant
                        .as_deref()
                        .and_then(|variant| gatherable_of.get(&variant.to_lowercase()))
                        .map(|gatherable| (*gatherable).to_owned())
                })
                .collect();
            Some((row.variant_id.clone()?, gatherables))
        }))
    }

    /// The cyclic gatherables of `cyclics` (variant id, the gatherable of each state).
    pub fn build(cyclics: impl IntoIterator<Item = (String, Vec<Option<String>>)>) -> Self {
        let mut by_crc: HashMap<u32, Vec<CyclicState>> = HashMap::new();
        for (cyclic, gatherables) in cyclics {
            let states = u32::try_from(gatherables.len()).unwrap_or(u32::MAX);
            let mut shown: HashMap<u32, Vec<u32>> = HashMap::new();
            for (state, gatherable) in (0..states).zip(&gatherables) {
                if let Some(gatherable) = gatherable {
                    shown
                        .entry(lowercase_crc32(gatherable))
                        .or_default()
                        .push(state);
                }
            }
            for (crc, in_states) in shown {
                let state = match in_states.as_slice() {
                    [state] => Some(*state),
                    _ => None,
                };
                by_crc.entry(crc).or_default().push(CyclicState {
                    cyclic: cyclic.clone(),
                    state,
                    states,
                });
            }
        }
        Self { by_crc }
    }

    /// The cyclic states the gatherable whose lowercase id has the CRC-32 `crc` shows.
    pub fn of(&self, crc: u32) -> &[CyclicState] {
        self.by_crc.get(&crc).map_or(&[], Vec::as_slice)
    }
}

/// A gathering tool's efficiency range and the gear score range it spans.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolEfficiency {
    /// `MinGatherEFF` and `MaxGatherEFF` of the tool's weapon row (`ItemStatsRef`).
    pub efficiency: (f64, f64),
    /// `MinGearScore` and `MaxGearScore` of the tool's item row.
    pub gear_score: (u32, u32),
}

impl ToolEfficiency {
    /// The efficiency of the tool at gear score `gear_score`: the minimum at the item's lowest
    /// gear score, the maximum at its highest, and in between in proportion (Inferred: retail
    /// gathered with the T1 skinning knife, whose range is 2 to 2, at efficiency 2.0, newchar
    /// 2026-09-18 1792696324270).
    pub fn at(&self, gear_score: u32) -> f64 {
        let (low, high) = self.efficiency;
        let (lowest, highest) = self.gear_score;
        if highest <= lowest {
            return high;
        }
        let share =
            f64::from(gear_score.clamp(lowest, highest) - lowest) / f64::from(highest - lowest);
        low + (high - low) * share
    }
}

/// The gathering efficiency of every tool item, by lowercase item id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolEfficiencies {
    by_item: HashMap<String, ToolEfficiency>,
}

impl ToolEfficiencies {
    /// The tools of `tables`: the item rows whose `ItemStatsRef` names a weapon row with a
    /// gathering efficiency range.
    pub fn from_tables(tables: &Tables) -> Self {
        let ranges: HashMap<String, (f64, f64)> = tables
            .weapon_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                Some((
                    row.weapon_id.to_lowercase(),
                    (row.min_gather_eff?, row.max_gather_eff?),
                ))
            })
            .collect();
        let gear_score = |cell: Option<i64>| {
            cell.and_then(|value| u32::try_from(value).ok())
                .unwrap_or_default()
        };
        Self::build(
            tables
                .master_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let efficiency = *ranges.get(&row.item_stats_ref.as_deref()?.to_lowercase())?;
                    Some((
                        row.item_id.clone(),
                        ToolEfficiency {
                            efficiency,
                            gear_score: (
                                gear_score(row.min_gear_score),
                                gear_score(row.max_gear_score),
                            ),
                        },
                    ))
                }),
        )
    }

    /// The tools of `tools` (item id, efficiency); the first row of an item wins.
    pub fn build(tools: impl IntoIterator<Item = (String, ToolEfficiency)>) -> Self {
        let mut by_item = HashMap::new();
        for (item, tool) in tools {
            by_item.entry(item.to_lowercase()).or_insert(tool);
        }
        Self { by_item }
    }

    /// The efficiency of tool item `item` (any case); `None` for an item that is no tool.
    pub fn of(&self, item: &str) -> Option<&ToolEfficiency> {
        self.by_item.get(&item.to_lowercase())
    }
}

/// The low bits of a crate's facet entity that its capital's GUID does not give (loot note
/// 2026-09-18: retail requests differ from the GUID key in them by 0 to 7; their meaning is
/// Unknown).
pub(crate) const FACET_FREE_BITS: u64 = 0b111;
/// Hex digits of a GUID.
const GUID_HEX_DIGITS: usize = 32;
/// The GUID text's separator.
const GUID_SEPARATOR: char = '-';
/// Bytes of each half of a GUID.
const GUID_HALF: usize = 8;

/// One row of `javelindata_variations` that gives a loot crate its table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrateVariant {
    /// `VariantID`: the `variant_name` of the crate's capital in the world file.
    pub variant_id: String,
    /// `GatherableEntryID` (else `GatherableEntryId`, else `Gatherable_0`).
    pub gatherable: String,
    /// `Name`: the display name the client's gather request carries.
    pub name: String,
    /// `LootTableComponent`, else `LootTableID`.
    pub table: String,
}

impl CrateVariant {
    /// The crate variants of `tables`: the rows with a gatherable and a loot table.
    pub fn from_tables(tables: &Tables) -> Vec<Self> {
        tables
            .variation_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                let gatherable = row
                    .gatherable_entry_id
                    .clone()
                    .or_else(|| row.gatherable_entry_id_2.clone())
                    .or_else(|| row.gatherable_0.clone())
                    .filter(|id| !id.is_empty())?;
                let table = row
                    .loot_table_component
                    .clone()
                    .or_else(|| row.loot_table_id.clone())
                    .filter(|table| !table.is_empty())?;
                Some(Self {
                    variant_id: row.variant_id.clone()?,
                    gatherable,
                    name: row.name.clone().unwrap_or_default(),
                    table,
                })
            })
            .collect()
    }
}

/// The loot table of each placed loot crate (loot note 2026-09-18). Generic crates
/// (`LootMediumT1_Outpost`, `LootSmallT1`, `LootLargeT1`, ...) have `FinalLootTable` `Empty`: the
/// items come from the table of the crate's variant. The client names the crate by its facet
/// entity. A crate placed as a capital has the capital's key: the two big-endian halves of the
/// GUID XOR-ed, low 3 bits free (Observed on 26 retail requests, no two of the 268,113
/// open-world keys equal); the capital's `variant_name` is the variant. A crate that a slice
/// spawns has its spawned slice's id (GDE id note, `nw_world::slice_data`) and the spawner's
/// variation name. Any other crate takes the table of its gatherable and name when only one
/// variant has them; else its table is Unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrateLoot {
    /// The variant of each capital key.
    by_key: HashMap<u64, usize>,
    /// The tables of each (gatherable CRC, name).
    by_name: HashMap<(u32, String), BTreeSet<String>>,
    variants: Vec<CrateVariant>,
}

/// The key of a capital `guid` (text, with or without dashes); `None` for another text.
pub(crate) fn capital_key(guid: &str) -> Option<u64> {
    let digits: String = guid.chars().filter(|c| *c != GUID_SEPARATOR).collect();
    if digits.len() != GUID_HEX_DIGITS {
        return None;
    }
    let bytes = hex::decode(digits).ok()?;
    let (high, low) = bytes.split_at(GUID_HALF);
    let half = |bytes: &[u8]| {
        <[u8; GUID_HALF]>::try_from(bytes)
            .ok()
            .map(u64::from_be_bytes)
    };
    Some((half(high)? ^ half(low)?) & !FACET_FREE_BITS)
}

impl CrateLoot {
    /// The crates of `variants` placed by `capitals` (GUID text, `variant_name`) and by spawners
    /// (`spawned`: spawned slice id, variation name). Variant names match in any case.
    pub fn build<'a>(
        variants: Vec<CrateVariant>,
        capitals: impl IntoIterator<Item = (&'a str, &'a str)>,
        spawned: impl IntoIterator<Item = (u64, &'a str)>,
    ) -> Self {
        let by_variant: HashMap<String, usize> = variants
            .iter()
            .enumerate()
            .map(|(index, variant)| (variant.variant_id.to_lowercase(), index))
            .collect();
        let variant_of = |variant: &str| by_variant.get(&variant.to_lowercase()).copied();
        let by_key =
            capitals
                .into_iter()
                .filter_map(|(guid, variant)| Some((capital_key(guid)?, variant_of(variant)?)))
                .chain(spawned.into_iter().filter_map(|(id, variant)| {
                    Some((id & !FACET_FREE_BITS, variant_of(variant)?))
                }))
                .collect();
        let mut by_name: HashMap<(u32, String), BTreeSet<String>> = HashMap::new();
        for variant in &variants {
            by_name
                .entry((lowercase_crc32(&variant.gatherable), variant.name.clone()))
                .or_default()
                .insert(variant.table.clone());
        }
        Self {
            by_key,
            by_name,
            variants,
        }
    }

    /// The loot table of the crate with facet entity `facet` that a gather request names by
    /// gatherable `gatherable` (the CRC-32 of its lowercase id) and `name`; `None` for no crate
    /// variant, or a crate without a capital whose gatherable and name several variants share.
    pub fn table(&self, facet: u64, gatherable: u32, name: &str) -> Option<&str> {
        let placed = self
            .by_key
            .get(&(facet & !FACET_FREE_BITS))
            .map(|index| &self.variants[*index])
            .filter(|variant| {
                lowercase_crc32(&variant.gatherable) == gatherable && variant.name == name
            });
        if let Some(variant) = placed {
            return Some(&variant.table);
        }
        let tables = self.by_name.get(&(gatherable, name.to_owned()))?;
        let mut tables = tables.iter();
        match (tables.next(), tables.next()) {
            (Some(table), None) => Some(table),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gatherable_is_found_by_the_crc_of_its_lowercase_id() {
        let gatherables = Gatherables::build([Gatherable {
            id: "SingleStone".to_owned(),
            gather_ms: 250,
            respawn_ms: (60_000, 90_000),
            loot_table: Some("SingleStone".to_owned()),
            loot_container: false,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: None,
        }]);
        // Retail flint request: crc `a417137f`.
        let flint = gatherables.get(0xa417_137f).cloned().expect("flint");
        assert_eq!(flint.id, "SingleStone");
        assert_eq!(ms_of(0.25), 250);
    }

    #[test]
    fn a_detection_sphere_is_found_by_its_gatherable_and_an_empty_one_is_left_out() {
        /// `94_BeastTorch`'s variant radius in build 25810808.
        const TORCH_RADIUS_M: f32 = 9.0;
        let spheres = DetectionSpheres::build([
            ("94_BeastTorch".to_owned(), TORCH_RADIUS_M),
            ("94_BeastTorch".to_owned(), 1.0),
            ("94_BeastMural".to_owned(), 0.0),
        ]);
        assert_eq!(
            spheres.radius_of(lowercase_crc32("94_beasttorch")),
            Some(TORCH_RADIUS_M)
        );
        assert_eq!(spheres.radius_of(lowercase_crc32("94_beastmural")), None);
    }

    #[test]
    fn a_gather_rule_asks_for_a_rank_or_an_item() {
        let rules = GatherRules::build([
            (
                "0402_GruelBarrel".to_owned(),
                GatherRule {
                    required_rank: 0,
                    required_item: Some(("0402_0401_BileSoakedMeat".to_owned(), 1)),
                },
            ),
            ("FlintSmall".to_owned(), GatherRule::default()),
        ]);
        assert_eq!(
            rules.of(lowercase_crc32("0402_gruelbarrel")).required_item,
            Some(("0402_0401_BileSoakedMeat".to_owned(), 1))
        );
        assert_eq!(
            rules.of(lowercase_crc32("FlintSmall")),
            GatherRule::default()
        );
    }

    #[test]
    fn a_cyclic_gatherable_names_the_state_of_each_gatherable() {
        // Client rows: the staff ritual shows one gatherable per state, a puzzle stone one
        // gatherable in all four.
        let cyclic = CyclicGatherables::build([
            (
                "Cyclic_09A_M13_ArcaneRitual".to_owned(),
                vec![
                    Some("09A_M13_ArcaneRitual_0".to_owned()),
                    Some("09A_M13_ArcaneRitual_1".to_owned()),
                    Some("09A_M13_ArcaneRitual_2".to_owned()),
                ],
            ),
            (
                "Cyclic_99C_MSQ_puzzle1_stone1".to_owned(),
                vec![Some("Cyclic_99C_MSQ_puzzle1_stone1".to_owned()); 4],
            ),
            ("Cyclic_13_Summer_Carpenter".to_owned(), vec![None, None]),
        ]);
        assert_eq!(
            cyclic.of(lowercase_crc32("09a_m13_arcaneritual_1")),
            [CyclicState {
                cyclic: "Cyclic_09A_M13_ArcaneRitual".to_owned(),
                state: Some(1),
                states: 3,
            }]
        );
        assert_eq!(
            cyclic.of(lowercase_crc32("cyclic_99c_msq_puzzle1_stone1")),
            [CyclicState {
                cyclic: "Cyclic_99C_MSQ_puzzle1_stone1".to_owned(),
                state: None,
                states: 4,
            }]
        );
        assert!(
            cyclic
                .of(lowercase_crc32("13_summer_carpenter1"))
                .is_empty()
        );
    }

    #[test]
    fn a_tools_efficiency_follows_its_gear_score() {
        // Retail rows of `1hSkinningKnifeT1` (2 to 2) and `2hPickT2` (2 to 3.5).
        let tools = ToolEfficiencies::build([
            (
                "1hSkinningKnifeT1".to_owned(),
                ToolEfficiency {
                    efficiency: (2.0, 2.0),
                    gear_score: (100, 100),
                },
            ),
            (
                "PickaxeT2".to_owned(),
                ToolEfficiency {
                    efficiency: (2.0, 3.5),
                    gear_score: (100, 200),
                },
            ),
        ]);
        let knife = tools.of("1hskinningknifet1").expect("knife");
        assert!((knife.at(100) - 2.0).abs() < f64::EPSILON);
        let pick = tools.of("PickaxeT2").expect("pick");
        assert!((pick.at(100) - 2.0).abs() < f64::EPSILON);
        assert!((pick.at(150) - 2.75).abs() < f64::EPSILON);
        assert!((pick.at(500) - 3.5).abs() < f64::EPSILON, "above the range");
        assert!(tools.of("WoodT1").is_none());
    }

    fn variant(id: &str, gatherable: &str, name: &str, table: &str) -> CrateVariant {
        CrateVariant {
            variant_id: id.to_owned(),
            gatherable: gatherable.to_owned(),
            name: name.to_owned(),
            table: table.to_owned(),
        }
    }

    /// The retail start-beach armour chest: its capital, and the facet entity of the retail
    /// request (ledger newchar-retail-2026-09-18, 1792696522177: raw id `1b3c94a8bff4b7dc...`).
    const BEACH_CHEST_GUID: &str = "aabcf082-9526-2edf-760b-043d3db212c4";
    const BEACH_CHEST_FACET: u64 = 0xdcb7_f4bf_a894_3c1b;
    const ARMOR_CHEST_NAME: &str = "@LootContainer_ArmorChestBeach_DisplayName";
    const SUPPLIES_NAME: &str = "@LootContainer_SuppliesSmall_DisplayName";
    /// A crate a watchtower's spawner places (spawned slice id; the data lowercases the
    /// variation name).
    const OUTPOST_CRATE: u64 = 0x5838_b791_0785_61b7;

    fn crates() -> CrateLoot {
        // Retail variation rows: two variants share LootLargeT1 and the chest name.
        CrateLoot::build(
            vec![
                variant(
                    "WeaponsChest_StarterBeach_1",
                    "LootLargeT1",
                    ARMOR_CHEST_NAME,
                    "BeachChestMB1",
                ),
                variant(
                    "WeaponsChest_StarterBeach_2",
                    "LootLargeT1",
                    ARMOR_CHEST_NAME,
                    "BeachChestMB2",
                ),
                variant(
                    "Abandoned_T1_S_Outpost",
                    "LootMediumT1_Outpost",
                    SUPPLIES_NAME,
                    "ContainerLootSmallLeveled",
                ),
            ],
            [
                (BEACH_CHEST_GUID, "WeaponsChest_StarterBeach_1"),
                ("not-a-guid", "WeaponsChest_StarterBeach_2"),
                ("00000000-0000-0000-0000-000000000001", "NotAVariant"),
            ],
            [(OUTPOST_CRATE, "abandoned_t1_s_outpost")],
        )
    }

    #[test]
    fn a_placed_crate_takes_the_table_of_its_capitals_variant() {
        let crates = crates();
        let chest = lowercase_crc32("LootLargeT1");
        assert_eq!(
            crates.table(BEACH_CHEST_FACET, chest, ARMOR_CHEST_NAME),
            Some("BeachChestMB1")
        );
        assert_eq!(
            crates.table(BEACH_CHEST_FACET | FACET_FREE_BITS, chest, ARMOR_CHEST_NAME),
            Some("BeachChestMB1"),
            "the low 3 bits are free"
        );
        assert_eq!(
            crates.table(BEACH_CHEST_FACET + 8, chest, ARMOR_CHEST_NAME),
            None,
            "no capital, and two variants share the gatherable and name: Unknown"
        );
        assert_eq!(
            crates.table(BEACH_CHEST_FACET, chest, SUPPLIES_NAME),
            None,
            "a request whose name is not its capital's variant's"
        );
    }

    #[test]
    fn a_spawned_crate_takes_the_table_of_its_spawners_variant() {
        let crates = crates();
        let outpost = lowercase_crc32("LootMediumT1_Outpost");
        assert_eq!(
            crates.table(OUTPOST_CRATE, outpost, SUPPLIES_NAME),
            Some("ContainerLootSmallLeveled"),
            "the variation name matches in any case"
        );
    }

    #[test]
    fn a_crate_without_a_capital_takes_its_only_variants_table() {
        let crates = crates();
        let outpost = lowercase_crc32("LootMediumT1_Outpost");
        assert_eq!(
            crates.table(0, outpost, SUPPLIES_NAME),
            Some("ContainerLootSmallLeveled")
        );
        assert_eq!(crates.table(0, outpost, ARMOR_CHEST_NAME), None);
        assert_eq!(
            capital_key(BEACH_CHEST_GUID),
            Some(BEACH_CHEST_FACET & !FACET_FREE_BITS)
        );
        assert_eq!(capital_key("aabcf082"), None);
    }
}
