// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an item rolls and keeps of its own (run-15 items2 study): its gear score range, its
//! fixed perks and perk buckets, its durability and repair data, and the perk roll of a new
//! crafted or dropped item.
//!
//! **Perk roll (best guess, retail evidence: 18 crafted T4 items of gear score 420 to 430 had
//! 0 perks twice, 2 three times and 3 thirteen times; rolling each slot on its own
//! `PerkChance` would give 0/1/2/3 at 3/22/42/33 %).** The target count is [`perk_count`] of the
//! item's tier and gear score (raised to the floor of its `ForceRarity`). The fixed perks
//! (`Perk1..5`) come first, then the forced ones (perk items of a craft), then free slots are
//! drawn without replacement with their bucket's `PerkChance` as the weight, each slot picking
//! one qualifying perk of its bucket ([`PerkTable::pick`]); a slot whose bucket has no
//! qualifying perk stays empty, which leaves the item below its target. An item without
//! `CanHavePerks` keeps its fixed perks only.

use std::collections::HashMap;

use crate::Tables;
use crate::perks::{PERK_SLOTS, PerkTable, chance_hit, parts, perk_count};

/// The lowest perk count of each `ForceRarity` (0 common to 4 legendary), from the rarity of a
/// perk count (`ItemRarityData`): uncommon 1, rare 3, epic 4, legendary 5 (best guess: the
/// loot note's bag byte maps `ForceRarity` to rarity, the count is the least of that rarity).
const PERKS_OF_FORCED_RARITY: [u32; 5] = [0, 1, 3, 4, 5];
/// The `SocketType` of a gem.
pub const SOCKET_GEM: &str = "Gem";

/// One item's roll data (`MasterItemDefinitions`, and `ResourceItemDefinitions` for perk
/// items, gems and charms).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemRollRow {
    /// `ItemID`.
    pub item_id: String,
    /// The parts of `ItemClass`.
    pub classes: Vec<String>,
    /// `Tier`; 0 when unset.
    pub tier: u32,
    /// `MinGearScore`.
    pub min_gear_score: u32,
    /// `MaxGearScore`; the minimum when unset.
    pub max_gear_score: u32,
    /// `GearScoreOverride`: the gear score is fixed.
    pub gear_score_override: Option<u32>,
    /// `MaxCraftGS`: the most a craft gives.
    pub max_craft_gear_score: Option<u32>,
    /// `Perk1..5` in column order (unswapped).
    pub fixed: Vec<String>,
    /// `PerkBucket1..5` by perk slot (empty: no bucket).
    pub buckets: [String; PERK_SLOTS],
    /// `CanHavePerks`.
    pub can_have_perks: bool,
    /// `ForceRarity`.
    pub force_rarity: Option<u32>,
    /// `Durability`: the full durability (0: the item has none).
    pub durability: u32,
    /// `DurabilityDmgOnDeath`: the share of the full durability a death takes.
    pub durability_loss_on_death: f64,
    /// `IsRepairable`.
    pub repairable: bool,
    /// `RepairDustModifier`.
    pub repair_dust_modifier: Option<f64>,
    /// `UiItemClass` (`UI_Weapon`, `UI_Consumable`, ...).
    pub ui_item_class: Option<String>,
    /// `BindOnPickup`.
    pub bind_on_pickup: bool,
    /// `BindOnEquip`.
    pub bind_on_equip: bool,
    /// `CanReplaceGem`.
    pub can_replace_gem: bool,
    /// `IngredientGearScoreBaseBonus`: added to both ends of a craft's range.
    pub ingredient_base_bonus: u32,
    /// `IngredientGearScoreMaxBonus`: added to the top of a craft's range.
    pub ingredient_max_bonus: u32,
    /// The `PerkBucket` of a resource (a perk item, a gem or a charm).
    pub resource_bucket: Option<String>,
    /// The `SocketType` of a resource: `Gem`, `Offense`, `Defense`, `Skill`.
    pub socket_type: Option<String>,
}

impl ItemRollRow {
    /// Whether a new item of it rolls values of its own: it may have perks, or its gear score
    /// is a range.
    pub fn rolls(&self) -> bool {
        self.can_have_perks
            || (self.gear_score_override.is_none() && self.max_gear_score > self.min_gear_score)
    }
}

/// A new item's own values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolledItem {
    /// Its gear score.
    pub gear_score: u32,
    /// Its perk ids in slot order.
    pub perks: Vec<String>,
}

/// Every item's roll data and the perk table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemRolls {
    items: HashMap<String, ItemRollRow>,
    /// The perks, swaps and buckets.
    pub perks: PerkTable,
}

/// A non-negative number cell.
fn number(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

/// A flag cell: set when non-zero.
fn flag(value: Option<i64>) -> bool {
    value.is_some_and(|value| value != 0)
}

impl ItemRolls {
    /// The roll data of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let resources: HashMap<String, (Option<String>, Option<String>)> = tables
            .resource_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| {
                (
                    row.resource_id.to_lowercase(),
                    (
                        Some(row.perk_bucket.clone()).filter(|bucket| !bucket.trim().is_empty()),
                        row.socket_type
                            .clone()
                            .filter(|socket| !socket.trim().is_empty()),
                    ),
                )
            })
            .collect();
        let rows = tables
            .master_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| {
                let (resource_bucket, socket_type) = resources
                    .get(&row.item_id.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                let min_gear_score = number(row.min_gear_score).unwrap_or_default();
                ItemRollRow {
                    item_id: row.item_id.clone(),
                    classes: parts(row.item_class.as_deref()),
                    tier: number(row.tier).unwrap_or_default(),
                    min_gear_score,
                    max_gear_score: number(row.max_gear_score).unwrap_or(min_gear_score),
                    gear_score_override: number(row.gear_score_override),
                    max_craft_gear_score: number(row.max_craft_gs),
                    fixed: [&row.perk1, &row.perk2, &row.perk3, &row.perk4, &row.perk5]
                        .into_iter()
                        .flatten()
                        .filter(|cell| !cell.trim().is_empty())
                        .cloned()
                        .collect(),
                    buckets: [
                        &row.perk_bucket1,
                        &row.perk_bucket2,
                        &row.perk_bucket3,
                        &row.perk_bucket4,
                        &row.perk_bucket5,
                    ]
                    .map(|cell| cell.as_deref().unwrap_or_default().trim().to_owned()),
                    can_have_perks: flag(row.can_have_perks),
                    force_rarity: number(row.force_rarity),
                    durability: number(row.durability).unwrap_or_default(),
                    durability_loss_on_death: row.durability_dmg_on_death.unwrap_or_default(),
                    repairable: flag(row.is_repairable),
                    repair_dust_modifier: row.repair_dust_modifier,
                    ui_item_class: row.ui_item_class.clone(),
                    bind_on_pickup: flag(row.bind_on_pickup),
                    bind_on_equip: flag(row.bind_on_equip),
                    can_replace_gem: flag(row.can_replace_gem),
                    ingredient_base_bonus: number(row.ingredient_gear_score_base_bonus)
                        .unwrap_or_default(),
                    ingredient_max_bonus: row
                        .ingredient_gear_score_max_bonus
                        .as_deref()
                        .and_then(|cell| cell.trim().parse().ok())
                        .unwrap_or_default(),
                    resource_bucket,
                    socket_type,
                }
            });
        Self::build(rows, PerkTable::from_tables(tables))
    }

    /// The roll data of `rows` with the perk table `perks`.
    pub fn build(rows: impl IntoIterator<Item = ItemRollRow>, perks: PerkTable) -> Self {
        Self {
            items: rows
                .into_iter()
                .map(|row| (row.item_id.to_lowercase(), row))
                .collect(),
            perks,
        }
    }

    /// The roll data of `item_id` (any case).
    pub fn item(&self, item_id: &str) -> Option<&ItemRollRow> {
        self.items.get(&item_id.to_lowercase())
    }

    /// The best item of `item_id`'s kind: of the items whose `ItemClass` holds the same classes
    /// (the same slot, weapon type or armor weight) and whose gear score is not fixed (a fixed
    /// `GearScoreOverride` marks the playtest and blank items), the one with the highest
    /// `MaxGearScore`, the lowest id first on a tie; `None` for an unknown item.
    pub fn best_of_kind(&self, item_id: &str) -> Option<&ItemRollRow> {
        let kind = sorted_classes(&self.item(item_id)?.classes);
        self.items
            .values()
            .filter(|row| row.gear_score_override.is_none() && sorted_classes(&row.classes) == kind)
            .max_by(|a, b| {
                a.max_gear_score
                    .cmp(&b.max_gear_score)
                    .then_with(|| b.item_id.to_lowercase().cmp(&a.item_id.to_lowercase()))
            })
    }

    /// The gear score `wanted` held to `item_id`'s range: its override when it has one, else at
    /// most its maximum gear score; `wanted` for an unknown item. The minimum is no floor
    /// (Observed: 1,060 retail item records of 300 ledgers lie below their item's
    /// `MinGearScore`, `MediumLegs_UndeadBerserkerT5` at 100 of 500, `MediumHead_RescuerT5` at
    /// 123; a drop takes the player level's `GSLimitT{tier}` of a `UseLevelGS` table, 100 for a
    /// tier 2 item at level 1, where the floor made the start beach drop 200 to 250).
    pub fn clamp_gear_score(&self, item_id: &str, wanted: u32) -> u32 {
        match self.item(item_id) {
            Some(row) => row
                .gear_score_override
                .unwrap_or_else(|| wanted.min(row.max_gear_score.max(row.min_gear_score))),
            None => wanted,
        }
    }

    /// The fixed perks of `item_id`, swapped and in slot order.
    pub fn fixed_perks(&self, item_id: &str) -> Vec<String> {
        self.item(item_id).map_or_else(Vec::new, |row| {
            self.perks.fixed_perks(
                &row.item_id,
                &row.classes,
                row.fixed.iter().map(|cell| Some(cell.as_str())),
            )
        })
    }

    /// The perks of a new `item_id` at `gear_score` with the `forced` perks (perk items of a
    /// craft), in slot order (module notes). `below(n)` is a random number below `n`.
    pub fn roll_perks(
        &self,
        item_id: &str,
        gear_score: u32,
        forced: &[String],
        below: &mut impl FnMut(u32) -> u32,
    ) -> Vec<String> {
        let Some(row) = self.item(item_id) else {
            return Vec::new();
        };
        let mut perks = self.fixed_perks(item_id);
        if !row.can_have_perks {
            return perks;
        }
        let forced_floor = row
            .force_rarity
            .and_then(|rarity| PERKS_OF_FORCED_RARITY.get(usize::try_from(rarity).ok()?))
            .copied()
            .unwrap_or_default();
        let target = usize::try_from(perk_count(row.tier, gear_score).max(forced_floor))
            .unwrap_or(PERK_SLOTS)
            .min(PERK_SLOTS);
        for perk_id in forced {
            let perk_id = self.perks.swapped(&row.item_id, &row.classes, perk_id);
            let fits = self
                .perks
                .perk(&perk_id)
                .is_some_and(|perk| perk.fits(&row.classes));
            if fits && perks.len() < PERK_SLOTS && !perks.contains(&perk_id) {
                perks.push(perk_id);
            }
        }
        let mut free: Vec<(&str, f64)> = row
            .buckets
            .iter()
            .enumerate()
            .filter(|(slot, bucket)| *slot >= row.fixed.len() && !bucket.is_empty())
            .filter_map(|(_, bucket)| Some((bucket.as_str(), self.perks.bucket(bucket)?.chance)))
            .filter(|(_, chance)| *chance > 0.0)
            .collect();
        while perks.len() < target && !free.is_empty() {
            let total: f64 = free.iter().map(|(_, chance)| chance).sum();
            let mut drawn = free.len() - 1;
            let mut left = total;
            for (index, (_, chance)) in free.iter().enumerate() {
                if chance_hit(chance / left, below) {
                    drawn = index;
                    break;
                }
                left -= chance;
            }
            let (bucket, _) = free.remove(drawn);
            if let Some(perk_id) =
                self.perks
                    .pick(bucket, &row.item_id, &row.classes, &perks, below)
            {
                perks.push(perk_id);
            }
        }
        PerkTable::ordered(perks)
    }

    /// A new `item_id` at the gear score `wanted` (held to its range) with the `forced` perks.
    pub fn roll(
        &self,
        item_id: &str,
        wanted: u32,
        forced: &[String],
        below: &mut impl FnMut(u32) -> u32,
    ) -> RolledItem {
        let gear_score = self.clamp_gear_score(item_id, wanted);
        RolledItem {
            gear_score,
            perks: self.roll_perks(item_id, gear_score, forced, below),
        }
    }
}

/// `classes` in one order, to compare two items' kinds.
fn sorted_classes(classes: &[String]) -> Vec<&str> {
    let mut sorted: Vec<&str> = classes.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perks::{Perk, PerkBucketRow, PerkSheet, PerkSource};

    const SWORD: &str = "1hSwordT4";

    fn perk(perk_id: &str, labels: &str) -> Perk {
        Perk {
            perk_id: perk_id.to_owned(),
            perk_type: "Generated".to_owned(),
            item_classes: parts(Some("Sword")),
            excluded_classes: Vec::new(),
            labels: parts(Some(labels)),
            display_name: None,
        }
    }

    fn rolls(can_have_perks: bool) -> ItemRolls {
        let table = PerkTable::build(PerkSource {
            sheets: vec![PerkSheet {
                unique_id: "ItemPerks".to_owned(),
                perks: vec![
                    perk("PerkID_Fixed", "Fixed"),
                    perk("PerkID_A", "A"),
                    perk("PerkID_B", "B"),
                    perk("PerkID_C", "C"),
                    perk("PerkID_Forced", "Forced"),
                ],
            }],
            buckets: vec![
                PerkBucketRow {
                    id: "Bucket".to_owned(),
                    chance: Some(1.0),
                    entries: ["PerkID_A", "PerkID_B", "PerkID_C"]
                        .map(str::to_owned)
                        .to_vec(),
                    ..PerkBucketRow::default()
                },
                PerkBucketRow {
                    id: "Never".to_owned(),
                    chance: Some(0.0),
                    entries: vec!["PerkID_C".to_owned()],
                    ..PerkBucketRow::default()
                },
            ],
            ..PerkSource::default()
        });
        let bucket = |id: &str| id.to_owned();
        ItemRolls::build(
            [ItemRollRow {
                item_id: SWORD.to_owned(),
                classes: parts(Some("EquippableMainHand+Sword")),
                tier: 4,
                min_gear_score: 400,
                max_gear_score: 500,
                fixed: vec!["PerkID_Fixed".to_owned()],
                buckets: [
                    String::new(),
                    bucket("Bucket"),
                    bucket("Bucket"),
                    bucket("Never"),
                    bucket("Bucket"),
                ],
                can_have_perks,
                ..ItemRollRow::default()
            }],
            table,
        )
    }

    #[test]
    fn the_best_of_a_kind_has_the_highest_gear_score_of_its_classes_without_a_fixed_one() {
        let row = |item_id: &str, classes: &str, max_gear_score, fixed: Option<u32>| ItemRollRow {
            item_id: item_id.to_owned(),
            classes: parts(Some(classes)),
            max_gear_score,
            gear_score_override: fixed,
            ..ItemRollRow::default()
        };
        // The kit sword and the retail tier 5 longswords (`Infix_*_1hLongsword`, 726 to 800).
        let rolls = ItemRolls::build(
            [
                row("1hSwordT2_FTUE", "EquippableMainHand+Weapon+Melee+Sword", 60, None),
                row("Infix_Tenacity_1hLongsword", "Sword+Weapon+Melee+EquippableMainHand", 800, None),
                row("Infix_Enrage_1hLongsword", "EquippableMainHand+Weapon+Melee+Sword", 800, None),
                row("Omega_Longsword_Blank_800", "EquippableMainHand+Weapon+Melee+Sword", 800, Some(800)),
                row("2hGreatSwordT5", "EquippableTwoHand+Weapon+Melee+GreatSword", 900, None),
            ],
            PerkTable::default(),
        );
        assert_eq!(
            rolls
                .best_of_kind("1hSwordT2_FTUE")
                .map(|best| best.item_id.as_str()),
            Some("Infix_Enrage_1hLongsword"),
            "same classes in any order, highest gear score, lowest id on a tie, no fixed score"
        );
        assert!(rolls.best_of_kind("no_such_item").is_none());
    }

    #[test]
    fn the_gear_score_is_held_under_the_item_maximum_not_over_its_minimum() {
        let rolls = rolls(true);
        assert_eq!(
            rolls.clamp_gear_score(SWORD, 300),
            300,
            "below the minimum stays"
        );
        assert_eq!(rolls.clamp_gear_score(SWORD, 450), 450);
        assert_eq!(rolls.clamp_gear_score(SWORD, 900), 500);
        assert_eq!(rolls.clamp_gear_score("Unknown", 900), 900);
    }

    #[test]
    fn a_roll_fills_the_perk_count_of_the_tier_and_gear_score() {
        let rolls = rolls(true);
        let mut zero = |_: u32| 0_u32;
        // Tier 4 at 420: 3 perks; the fixed one and two from the buckets.
        let rolled = rolls.roll(SWORD, 420, &[], &mut zero);
        assert_eq!(rolled.gear_score, 420);
        assert_eq!(rolled.perks.len(), 3);
        assert!(rolled.perks.contains(&"PerkID_Fixed".to_owned()));
        assert_eq!(rolled.perks, PerkTable::ordered(rolled.perks.clone()));
        // At 450: 4 perks, with a forced one.
        let forced = rolls.roll(SWORD, 450, &["PerkID_Forced".to_owned()], &mut zero);
        assert_eq!(forced.perks.len(), 4);
        assert!(forced.perks.contains(&"PerkID_Forced".to_owned()));
    }

    #[test]
    fn an_item_rolls_with_perks_or_a_gear_score_range() {
        let rolls = rolls(false);
        let sword = rolls.item(SWORD).expect("sword");
        assert!(sword.rolls(), "a gear score range");
        let fixed = ItemRollRow {
            gear_score_override: Some(100),
            ..sword.clone()
        };
        assert!(!fixed.rolls());
        assert!(
            ItemRollRow {
                can_have_perks: true,
                ..fixed
            }
            .rolls()
        );
    }

    #[test]
    fn an_item_without_perks_keeps_its_fixed_ones() {
        let rolls = rolls(false);
        let rolled = rolls.roll(SWORD, 450, &[], &mut |_: u32| 0_u32);
        assert_eq!(rolled.perks, ["PerkID_Fixed"]);
        assert_eq!(rolls.fixed_perks(SWORD), ["PerkID_Fixed"]);
    }
}
