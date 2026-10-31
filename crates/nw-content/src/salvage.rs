// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the salvage of an item gives (salvage note in
//! `local-data/scratch-work/run15/craft/REPORT.md`): the client's `InitiateSalvage` (1733) and
//! `SalvageItemsInInventory` (6188) name bag items, and the server takes each and grants:
//!
//! - its `SalvageGameEventID`'s reward (`SalvageCoinGrantT2`: 25 coins);
//! - its `SalvageAchievement`: a schematic (`Schematic_ShatterslashT4`) sets the achievement
//!   (`ShatterslashT4`) that its recipe's `RequiredAchievementID` asks for, so the salvage
//!   teaches the recipe;
//! - when `SalvageResources` is 1, the items of its `RepairRecipe`: a `[LTID]X` cell rolls loot
//!   table `X` with the item's tier, rarity and gear score as levels
//!   ([`crate::loot::LootTables::roll_salvage`]); a plain cell names a "Salvage Values" recipe
//!   (`SalvageSmall_MetalT2`: 5 `IngotT2`) and gives a fifth of each ingredient.
//!
//! Observed (retail ledgers `Playthrough_Part_03` and `MSQ6-lv28`, 92 salvages): the loot
//! containers (`ConcealedVaultLVL30`, `CRX_RewardContainerT3_*`) open through the same call and
//! the same columns; the plain recipes gave 1 `IngotT2` for `SalvageSmall_MetalT2` (5 ingots, 4
//! salvages), 2 `ClothT2` for `SalvageLarge_ClothT2` (10 cloth) and 1 `LeatherT2` for
//! `SalvageSmall_LeatherT2` (5 leather, 2 salvages); an item without `SalvageResources`
//! (`SoldierArchetype_1hRshield`) gave no items.

use std::collections::HashMap;

use crate::Tables;
use crate::loot::SalvageLevels;

/// The prefix of a `RepairRecipe` cell that names a loot table.
const TABLE_PREFIX: &str = "[LTID]";
/// The `SalvageResources` value of an item whose salvage gives items.
const GIVES_RESOURCES: i64 = 1;
/// The share of a "Salvage Values" recipe's ingredients a salvage gives back: one in five
/// (Observed: 5 gave 1 and 10 gave 2 in all 7 retail salvages of such items).
pub const RECIPE_RETURN_DIVISOR: u32 = 5;
/// The ingredient type of a "Salvage Values" recipe ingredient that names an item.
const ITEM_INGREDIENT: &str = "Item";

/// Where a salvage's items come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SalvageSource {
    /// A loot table, rolled with the item's values.
    Table(String),
    /// A "Salvage Values" recipe's ingredients (item id, quantity), of which a fifth comes
    /// back.
    Recipe(Vec<(String, u32)>),
}

/// What the salvage of one item type gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalvageRule {
    /// `Tier`.
    pub tier: u32,
    /// `ForceRarity`: the rarity of an item that has one; 0 (common) otherwise.
    pub rarity: u32,
    /// Its items' source; `None` without `SalvageResources` or a `RepairRecipe`.
    pub source: Option<SalvageSource>,
    /// `SalvageGameEventID`.
    pub event: Option<String>,
    /// `SalvageAchievement`.
    pub achievement: Option<String>,
}

impl SalvageRule {
    /// The loot levels of one of these items with gear score `gear_score`.
    pub fn levels(&self, gear_score: u32) -> SalvageLevels {
        SalvageLevels {
            tier: self.tier,
            rarity: self.rarity,
            gear_score,
        }
    }

    /// The items a "Salvage Values" recipe source gives back; none for a table source.
    pub fn returned(&self) -> Vec<(String, u32)> {
        match &self.source {
            Some(SalvageSource::Recipe(ingredients)) => ingredients
                .iter()
                .map(|(item, quantity)| (item.clone(), quantity / RECIPE_RETURN_DIVISOR))
                .filter(|(_, quantity)| *quantity > 0)
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// One master item row as salvage reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalvageRow {
    /// `ItemID`.
    pub item: String,
    /// `IsSalvageable`.
    pub salvageable: bool,
    /// `SalvageResources`.
    pub resources: Option<i64>,
    /// `RepairRecipe`.
    pub repair_recipe: Option<String>,
    /// `SalvageGameEventID`.
    pub event: Option<String>,
    /// `SalvageAchievement`.
    pub achievement: Option<String>,
    /// `Tier`.
    pub tier: Option<i64>,
    /// `ForceRarity`.
    pub rarity: Option<i64>,
}

/// The salvage rule of every salvageable item, by lowercase item id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Salvage {
    by_item: HashMap<String, SalvageRule>,
}

/// A table number as a count: negative or absent is 0.
fn count(cell: Option<i64>) -> u32 {
    cell.and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default()
}

impl Salvage {
    /// The rules of `tables`: the master item rows with `IsSalvageable`, and the "Salvage
    /// Values" recipes of `CraftingRecipeData` that their plain `RepairRecipe` cells name.
    pub fn from_tables(tables: &Tables) -> Self {
        let recipes = tables
            .crafting_recipe_data
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| {
                let cells = [
                    (&row.ingredient1, &row.type1, row.qty1),
                    (&row.ingredient2, &row.type2, row.qty2),
                    (&row.ingredient3, &row.type3, row.qty3),
                    (&row.ingredient4, &row.type4, row.qty4),
                    (&row.ingredient5, &row.type5, row.qty5),
                    (&row.ingredient6, &row.type6, row.qty6),
                    (&row.ingredient7, &row.type7, row.qty7),
                ];
                let ingredients = cells
                    .into_iter()
                    .filter(|(_, kind, _)| kind.as_deref() == Some(ITEM_INGREDIENT))
                    .filter_map(|(item, _, quantity)| {
                        Some((
                            item.clone().filter(|item| !item.is_empty())?,
                            count(quantity),
                        ))
                    })
                    .collect();
                (row.recipe_id.clone(), ingredients)
            });
        let rows = tables
            .master_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| SalvageRow {
                item: row.item_id.clone(),
                salvageable: row.is_salvageable.unwrap_or_default(),
                resources: row.salvage_resources,
                repair_recipe: row.repair_recipe.clone(),
                event: row.salvage_game_event_id.clone(),
                achievement: row.salvage_achievement.clone(),
                tier: row.tier,
                rarity: row.force_rarity,
            });
        Self::build(rows, recipes)
    }

    /// The rules of `rows` with the "Salvage Values" `recipes` (recipe id, item ingredients);
    /// a row that is not salvageable is left out, and the first row of an item wins.
    pub fn build(
        rows: impl IntoIterator<Item = SalvageRow>,
        recipes: impl IntoIterator<Item = (String, Vec<(String, u32)>)>,
    ) -> Self {
        let recipes: HashMap<String, Vec<(String, u32)>> = recipes
            .into_iter()
            .map(|(id, ingredients)| (id.to_lowercase(), ingredients))
            .collect();
        let mut by_item = HashMap::new();
        for row in rows.into_iter().filter(|row| row.salvageable) {
            let source = row
                .repair_recipe
                .as_deref()
                .map(str::trim)
                .filter(|_| row.resources == Some(GIVES_RESOURCES))
                .filter(|cell| !cell.is_empty())
                .and_then(|cell| match cell.strip_prefix(TABLE_PREFIX) {
                    Some(table) => Some(SalvageSource::Table(table.to_owned())),
                    None => recipes
                        .get(&cell.to_lowercase())
                        .map(|ingredients| SalvageSource::Recipe(ingredients.clone())),
                });
            by_item
                .entry(row.item.to_lowercase())
                .or_insert(SalvageRule {
                    tier: count(row.tier),
                    rarity: count(row.rarity),
                    source,
                    event: row.event.filter(|event| !event.is_empty()),
                    achievement: row
                        .achievement
                        .filter(|achievement| !achievement.is_empty()),
                });
        }
        Self { by_item }
    }

    /// The salvage rule of item `item` (any case); `None` for an item that cannot be salvaged.
    pub fn of(&self, item: &str) -> Option<&SalvageRule> {
        self.by_item.get(&item.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Master rows of build 25810808: a drop armour piece with a loot table, a quest chest piece
    /// with a "Salvage Values" recipe, a starter shield without resources, a loot container,
    /// a resource that cannot be salvaged, and a weapon schematic.
    fn salvage() -> Salvage {
        let row =
            |item: &str, resources: Option<i64>, recipe: &str, event: &str, tier: i64| SalvageRow {
                item: item.to_owned(),
                salvageable: true,
                resources,
                repair_recipe: Some(recipe.to_owned()),
                event: Some(event.to_owned()),
                achievement: None,
                tier: Some(tier),
                rarity: None,
            };
        Salvage::build(
            [
                row(
                    "FemHeavyFeet_DropT2",
                    Some(1),
                    "[LTID]MasterSalvageArmorSmall",
                    "SalvageCoinGrantT2",
                    2,
                ),
                row(
                    "SoldierArchetypeChest_Upgrade",
                    Some(1),
                    "SalvageSmall_MetalT2",
                    "",
                    1,
                ),
                row(
                    "SoldierArchetype_1hRshield",
                    None,
                    "SalvageSmall_MetalT2",
                    "",
                    1,
                ),
                SalvageRow {
                    rarity: Some(1),
                    ..row(
                        "ConcealedVaultLVL30",
                        Some(1),
                        "[LTID]ConcealedVault",
                        "EventBagofJuniperBerriesLVL30",
                        3,
                    )
                },
                SalvageRow {
                    salvageable: false,
                    ..row("IngotT2", None, "", "", 2)
                },
                SalvageRow {
                    achievement: Some("ShatterslashT4".to_owned()),
                    ..row("Schematic_ShatterslashT4", None, "", "", 4)
                },
            ],
            [
                (
                    "SalvageSmall_MetalT2".to_owned(),
                    vec![("IngotT2".to_owned(), 5)],
                ),
                (
                    "SalvageLarge_ClothT2".to_owned(),
                    vec![("ClothT2".to_owned(), 10)],
                ),
            ],
        )
    }

    #[test]
    fn a_drop_rolls_its_table_with_its_values() {
        let salvage = salvage();
        let feet = salvage.of("femheavyfeet_dropt2").expect("feet");
        assert_eq!(
            feet.source,
            Some(SalvageSource::Table("MasterSalvageArmorSmall".to_owned()))
        );
        assert_eq!(feet.event.as_deref(), Some("SalvageCoinGrantT2"));
        assert_eq!(
            feet.levels(155),
            SalvageLevels {
                tier: 2,
                rarity: 0,
                gear_score: 155
            }
        );
        assert!(feet.returned().is_empty(), "a table gives no recipe items");
        let vault = salvage.of("ConcealedVaultLVL30").expect("vault");
        assert_eq!(vault.rarity, 1);
        assert_eq!(
            vault.source,
            Some(SalvageSource::Table("ConcealedVault".to_owned()))
        );
    }

    #[test]
    fn a_plain_recipe_gives_back_a_fifth_of_its_ingredients() {
        let salvage = salvage();
        let chest = salvage.of("SoldierArchetypeChest_Upgrade").expect("chest");
        assert_eq!(chest.returned(), [("IngotT2".to_owned(), 1)]);
        assert_eq!(chest.event, None, "an empty event cell is no event");
        let shield = salvage.of("SoldierArchetype_1hRshield").expect("shield");
        assert_eq!(shield.source, None, "no SalvageResources");
        assert!(shield.returned().is_empty());
        assert!(salvage.of("IngotT2").is_none(), "not salvageable");
        let cloth = SalvageRule {
            source: Some(SalvageSource::Recipe(vec![
                ("ClothT2".to_owned(), 10),
                ("FiberT1".to_owned(), 4),
            ])),
            ..SalvageRule::default()
        };
        assert_eq!(
            cloth.returned(),
            [("ClothT2".to_owned(), 2)],
            "less than five gives none back"
        );
    }

    #[test]
    fn a_schematic_salvage_teaches_the_achievement_of_its_recipe() {
        let salvage = salvage();
        let schematic = salvage.of("schematic_shatterslasht4").expect("schematic");
        assert_eq!(schematic.achievement.as_deref(), Some("ShatterslashT4"));
        assert_eq!(schematic.source, None, "a schematic gives no items");
        let chest = salvage.of("SoldierArchetypeChest_Upgrade").expect("chest");
        assert_eq!(chest.achievement, None);
    }
}
