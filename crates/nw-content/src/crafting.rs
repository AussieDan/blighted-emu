// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crafting recipes (beach-quests note, section 4): a recipe makes `OutputQty` of its item from up
//! to seven ingredients at one of its stations. An ingredient of type `Category_Only` is any item
//! whose `IngredientCategories` hold the category (the Light Ration takes one
//! `FoodIngredientT1`: the captured craft used `PorkT1`); every other ingredient names an item.
//!
//! A station type is a name and a tier (`blacksmith2`, `camp1`): a station makes the recipes of
//! its name up to its tier (Observed: retail crafted `weaving2`, `tanning2`, `smelting2`,
//! `carpentry2` and `masonry2` recipes at the T5 refining stations and `blacksmith2` recipes at a
//! T4 forge, ledgers `refining_all_stations_single_batch_territorylvlup` and `Crafting_Forge`). A
//! recipe's tradeskill, recipe level, game event and fee are its [`RecipeDetails`].

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;
use crate::recipe_cooldowns::RecipeCooldown;

/// The ingredient type of a category.
const CATEGORY_ONLY: &str = "Category_Only";
/// Separates the steps of a `GearScoreBonus` or `GearScoreReduction` cell (`5,10,15,20,25`).
const STEP_SEPARATOR: char = ',';
/// Separates the categories of an `IngredientCategories` cell.
const CATEGORY_SEPARATOR: char = ',';

/// What one ingredient of a recipe takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngredientKind {
    /// This item.
    Item(String),
    /// Any item of this category.
    Category(String),
}

/// One ingredient of a recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingredient {
    /// What it takes.
    pub kind: IngredientKind,
    /// How many per craft.
    pub quantity: u32,
}

/// One recipe of `CraftingRecipeData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// `RecipeID`.
    pub id: String,
    /// `ItemID`: what it makes.
    pub item: String,
    /// `OutputQty`.
    pub quantity: u32,
    /// The ingredients, in column order.
    pub ingredients: Vec<Ingredient>,
    /// `StationType1` to `StationType4`.
    pub stations: Vec<String>,
}

impl Recipe {
    /// Whether station type `station` (any case) makes it: a station type of the same name with
    /// a tier at least the recipe's, or the same type for a name without a tier.
    pub fn made_at(&self, station: &str) -> bool {
        let (station_name, station_tier) = name_and_tier(station);
        self.stations.iter().any(|known| {
            let (name, tier) = name_and_tier(known);
            name.eq_ignore_ascii_case(station_name)
                && match (tier, station_tier) {
                    (Some(tier), Some(station_tier)) => station_tier >= tier,
                    (None, None) => true,
                    _ => false,
                }
        })
    }
}

/// A cell with text, trimmed; `None` for an empty one.
fn filled(cell: &Option<String>) -> Option<String> {
    cell.as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A station type's name and its tier: `blacksmith2` is `blacksmith` tier 2; a type that does
/// not end in digits (`camp1_standing`) has no tier.
fn name_and_tier(station: &str) -> (&str, Option<u32>) {
    let name = station.trim_end_matches(|letter: char| letter.is_ascii_digit());
    let tier = station[name.len()..].parse().ok();
    match tier {
        Some(tier) if !name.is_empty() => (name, Some(tier)),
        _ => (station, None),
    }
}

/// What a recipe gives and costs besides its items (`CraftingRecipeData`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeDetails {
    /// `Tradeskill`: the tradeskill whose rank the recipe needs.
    pub tradeskill: Option<String>,
    /// `RecipeLevel`: the `RecipeLevel` the tradeskill's rank must reach.
    pub recipe_level: u32,
    /// `GameEventID`: the game event of each craft (its tradeskill experience).
    pub game_event: Option<String>,
    /// `CraftingFee`: the most coins one craft costs (Observed: retail took less, the fee its
    /// client showed after the station's tax: 69 for one craft of a 184-coin recipe, 18 for 40
    /// crafts of a 1-coin recipe).
    pub fee: u32,
    /// `BaseGearScore`: the gear score a craft starts from; `None`: the item's own.
    pub base_gear_score: Option<u32>,
    /// `BaseTier`: the tier its ingredients are measured against, and the tier of its rank
    /// gear score bonus.
    pub base_tier: u32,
    /// `GearScoreBonus`: what a `Category_Only` ingredient of 1, 2, ... tiers above
    /// [`Self::base_tier`] adds.
    pub gear_score_bonus: Vec<u32>,
    /// `GearScoreReduction`: what one of 1, 2, ... tiers below takes.
    pub gear_score_reduction: Vec<u32>,
    /// `DisallowBonusesToGS`: the craft gets its base gear score only.
    pub disallow_bonuses: bool,
    /// `MaxPerkItemsAllowed`: the most perk items one craft takes; `None`: as many as the item
    /// has perk slots.
    pub max_perk_items: Option<u32>,
    /// `RequiredAchievementID`: the achievement the player must have to craft it (511 rows of
    /// build 25810808: season and expedition recipes).
    pub required_achievement: Option<String>,
    /// `HiddenByAchievementID`: once the player has it, the recipe is gone (141 rows).
    pub hidden_by: Option<String>,
    /// `UnlockedAchievementID`: a craft sets it (73 rows), and with
    /// `UnlockedAchievementBlocksRecrafting` no craft follows once it is set (5 rows).
    pub unlocks: Option<String>,
    /// `UnlockedAchievementBlocksRecrafting`.
    pub unlock_blocks_recraft: bool,
    /// `FirstCraftAchievementId`: the first craft sets it (5,116 rows).
    pub first_craft_achievement: Option<String>,
    /// `CooldownSeconds` and `CooldownQuantity` ([`crate::recipe_cooldowns`]).
    pub cooldown: Option<RecipeCooldown>,
}

impl RecipeDetails {
    /// Whether a player who has the achievements `has` may craft it: it has the required one,
    /// not the hiding one, and not the one a craft unlocks when that blocks a recraft.
    pub fn open_to(&self, has: impl Fn(&str) -> bool) -> bool {
        self.required_achievement.as_deref().is_none_or(&has)
            && !self.hidden_by.as_deref().is_some_and(&has)
            && !(self.unlock_blocks_recraft && self.unlocks.as_deref().is_some_and(&has))
    }

    /// The achievements a craft sets: the one it unlocks and the first craft's.
    pub fn craft_achievements(&self) -> impl Iterator<Item = &str> {
        self.unlocks
            .as_deref()
            .into_iter()
            .chain(self.first_craft_achievement.as_deref())
    }
}

/// What one picked ingredient adds to a craft's gear score range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngredientGearScore {
    /// `IngredientGearScoreBaseBonus`: added to both ends.
    pub base_bonus: u32,
    /// `IngredientGearScoreMaxBonus`: added to the top.
    pub max_bonus: u32,
    /// For a `Category_Only` ingredient: its item's `Tier`.
    pub category_tier: Option<u32>,
}

/// The allowed gear score range of a craft (best guess of the client's
/// `CraftingRequestBus.GetGearScoreBonus`, whose body is not in the decompile; run-15 items2
/// R3): the base gear score, plus the rank bonus of the recipe tier, plus each ingredient's
/// base bonus (both ends) and max bonus (top), plus `GearScoreBonus[d-1]` for a `Category_Only`
/// ingredient `d` tiers above the base tier or minus `GearScoreReduction[d-1]` for one below;
/// without bonuses when the recipe disallows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CraftGearScore {
    /// The lowest.
    pub min: u32,
    /// The highest.
    pub max: u32,
}

impl RecipeDetails {
    /// The gear score range of a craft from `base` (its `BaseGearScore`, else the item's own)
    /// with the rank bonus `rank_bonus` (lowest, highest) and the picked `ingredients`.
    pub fn gear_score_range(
        &self,
        base: u32,
        rank_bonus: (u32, u32),
        ingredients: &[IngredientGearScore],
    ) -> CraftGearScore {
        let base = self.base_gear_score.unwrap_or(base);
        if self.disallow_bonuses {
            return CraftGearScore {
                min: base,
                max: base,
            };
        }
        let step = |steps: &[u32], distance: u32| {
            usize::try_from(distance)
                .ok()
                .and_then(|distance| distance.checked_sub(1))
                .and_then(|index| steps.get(index).or_else(|| steps.last()))
                .copied()
                .unwrap_or_default()
        };
        let (mut gain, mut loss, mut top) = (0_u32, 0_u32, 0_u32);
        for ingredient in ingredients {
            gain = gain.saturating_add(ingredient.base_bonus);
            top = top.saturating_add(ingredient.max_bonus);
            match ingredient.category_tier {
                Some(tier) if tier > self.base_tier => {
                    gain = gain.saturating_add(step(&self.gear_score_bonus, tier - self.base_tier));
                }
                Some(tier) if tier < self.base_tier => {
                    loss = loss
                        .saturating_add(step(&self.gear_score_reduction, self.base_tier - tier));
                }
                _ => {}
            }
        }
        let min = base
            .saturating_add(rank_bonus.0)
            .saturating_add(gain)
            .saturating_sub(loss);
        let max = base
            .saturating_add(rank_bonus.1)
            .saturating_add(gain)
            .saturating_add(top)
            .saturating_sub(loss)
            .max(min);
        CraftGearScore { min, max }
    }
}

/// The steps of a `GearScoreBonus` or `GearScoreReduction` cell; a step that is no number is
/// left out.
fn steps_of(cell: Option<&str>) -> Vec<u32> {
    cell.unwrap_or_default()
        .split(STEP_SEPARATOR)
        .filter_map(|step| step.trim().parse().ok())
        .collect()
}

/// Every recipe by the CRC-32 of its lowercase id, and the ingredient categories of every item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipes {
    by_crc: HashMap<u32, Recipe>,
    /// The categories of each item, by lowercase item id.
    categories: HashMap<String, Vec<String>>,
    /// The details of each recipe, by the CRC-32 of its lowercase id.
    details: HashMap<u32, RecipeDetails>,
}

/// A quantity cell as a count of at least 1.
fn count_of(cell: Option<i64>) -> u32 {
    cell.and_then(|quantity| u32::try_from(quantity).ok())
        .unwrap_or(1)
        .max(1)
}

impl Recipes {
    /// The recipes and item categories of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let recipes = tables
            .crafting_recipe_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                let cells = [
                    (&row.ingredient1, row.qty1, &row.type1),
                    (&row.ingredient2, row.qty2, &row.type2),
                    (&row.ingredient3, row.qty3, &row.type3),
                    (&row.ingredient4, row.qty4, &row.type4),
                    (&row.ingredient5, row.qty5, &row.type5),
                    (&row.ingredient6, row.qty6, &row.type6),
                    (&row.ingredient7, row.qty7, &row.type7),
                ];
                let ingredients = cells
                    .into_iter()
                    .filter_map(|(name, quantity, kind)| {
                        let name = name.clone().filter(|name| !name.is_empty())?;
                        let kind = if kind.as_deref() == Some(CATEGORY_ONLY) {
                            IngredientKind::Category(name)
                        } else {
                            IngredientKind::Item(name)
                        };
                        Some(Ingredient {
                            kind,
                            quantity: count_of(quantity),
                        })
                    })
                    .collect();
                Some(Recipe {
                    id: row.recipe_id.clone(),
                    item: row.item_id.clone().filter(|item| !item.is_empty())?,
                    quantity: count_of(row.output_qty),
                    ingredients,
                    stations: [
                        &row.station_type1,
                        &row.station_type2,
                        &row.station_type3,
                        &row.station_type4,
                    ]
                    .into_iter()
                    .filter_map(Clone::clone)
                    .filter(|station| !station.is_empty())
                    .collect(),
                })
            });
        let categories = tables
            .master_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                let cell = row.ingredient_categories.as_deref()?;
                Some((row.item_id.clone(), cell.to_owned()))
            });
        let details = tables
            .crafting_recipe_data
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| {
                let count = |cell: Option<i64>| {
                    cell.and_then(|value| u32::try_from(value).ok())
                        .unwrap_or_default()
                };
                (
                    row.recipe_id.clone(),
                    RecipeDetails {
                        tradeskill: row.tradeskill.clone().filter(|skill| !skill.is_empty()),
                        recipe_level: count(row.recipe_level),
                        game_event: row.game_event_id.clone().filter(|event| !event.is_empty()),
                        fee: count(row.crafting_fee),
                        base_gear_score: row
                            .base_gear_score
                            .and_then(|value| u32::try_from(value).ok()),
                        base_tier: count(row.base_tier),
                        gear_score_bonus: steps_of(row.gear_score_bonus.as_deref()),
                        gear_score_reduction: steps_of(row.gear_score_reduction.as_deref()),
                        disallow_bonuses: row.disallow_bonuses_to_gs.unwrap_or_default(),
                        max_perk_items: row
                            .max_perk_items_allowed
                            .and_then(|value| u32::try_from(value).ok()),
                        required_achievement: filled(&row.required_achievement_id),
                        hidden_by: filled(&row.hidden_by_achievement_id),
                        unlocks: filled(&row.unlocked_achievement_id),
                        unlock_blocks_recraft: row
                            .unlocked_achievement_blocks_recrafting
                            .unwrap_or_default(),
                        first_craft_achievement: filled(&row.first_craft_achievement_id),
                        cooldown: RecipeCooldown::from_cells(
                            row.cooldown_seconds,
                            row.cooldown_quantity,
                        ),
                    },
                )
            });
        Self::build(recipes, categories).with_details(details)
    }

    /// These recipes with the `details` of their ids (recipe id, details); the first row of an
    /// id wins.
    #[must_use]
    pub fn with_details(
        mut self,
        details: impl IntoIterator<Item = (String, RecipeDetails)>,
    ) -> Self {
        for (id, detail) in details {
            self.details.entry(lowercase_crc32(&id)).or_insert(detail);
        }
        self
    }

    /// The details of the recipe whose lowercase id has the CRC-32 `crc`; none for a recipe
    /// without a row of them.
    pub fn details(&self, crc: u32) -> RecipeDetails {
        self.details.get(&crc).cloned().unwrap_or_default()
    }

    /// The book of `recipes`, with each item's `IngredientCategories` cell in `categories` (item
    /// id, cell).
    pub fn build(
        recipes: impl IntoIterator<Item = Recipe>,
        categories: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            by_crc: recipes
                .into_iter()
                .map(|recipe| (lowercase_crc32(&recipe.id), recipe))
                .collect(),
            categories: categories
                .into_iter()
                .map(|(item, cell)| {
                    let list = cell
                        .split(CATEGORY_SEPARATOR)
                        .map(str::trim)
                        .filter(|category| !category.is_empty())
                        .map(str::to_owned)
                        .collect();
                    (item.to_lowercase(), list)
                })
                .collect(),
            details: HashMap::new(),
        }
    }

    /// The recipe whose lowercase id has the CRC-32 `crc`.
    pub fn get(&self, crc: u32) -> Option<&Recipe> {
        self.by_crc.get(&crc)
    }

    /// The ingredients of every recipe made at a station whose type name, without its tier
    /// digits, has the lowercase CRC-32 `station` (the 7044 storage search: retail `smelting`
    /// for the `smelting1..5` stations).
    pub fn ingredients_at(&self, station: u32) -> impl Iterator<Item = &Ingredient> {
        self.by_crc
            .values()
            .filter(move |recipe| {
                recipe
                    .stations
                    .iter()
                    .any(|known| lowercase_crc32(name_and_tier(known).0) == station)
            })
            .flat_map(|recipe| &recipe.ingredients)
    }

    /// Whether item `item` (any case) is what `ingredient` takes.
    pub fn fits(&self, ingredient: &Ingredient, item: &str) -> bool {
        match &ingredient.kind {
            IngredientKind::Item(wanted) => wanted.eq_ignore_ascii_case(item),
            IngredientKind::Category(category) => self
                .categories
                .get(&item.to_lowercase())
                .is_some_and(|list| {
                    list.iter()
                        .any(|known| known.eq_ignore_ascii_case(category))
                }),
        }
    }
}

/// The resources a structure's blueprint costs (`BlueprintItemDefinitions`
/// `StructureCost_Ingredient1..4` and `StructureCost_Qty1..4`; Observed: `CampT1` costs 5
/// `WoodT1` and 1 `FlintT1`, the resources the retail campfire group lists as committed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructureCosts {
    by_blueprint: HashMap<String, Vec<(String, u32)>>,
}

impl StructureCosts {
    /// The costs of every blueprint of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .blueprint_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    let cells = [
                        (&row.structure_cost_ingredient1, row.structure_cost_qty1),
                        (&row.structure_cost_ingredient2, row.structure_cost_qty2),
                        (&row.structure_cost_ingredient3, row.structure_cost_qty3),
                        // The table types `StructureCost_Qty4` as text.
                        (
                            &row.structure_cost_ingredient4,
                            row.structure_cost_qty4
                                .as_deref()
                                .and_then(|quantity| quantity.parse().ok()),
                        ),
                    ];
                    let costs = cells
                        .into_iter()
                        .filter_map(|(item, quantity)| {
                            let item = item.clone().filter(|item| !item.is_empty())?;
                            Some((item, count_of(quantity)))
                        })
                        .collect();
                    (row.blueprint_id.clone(), costs)
                }),
        )
    }

    /// The costs of `blueprints`: (blueprint id, (item id, quantity) in column order).
    pub fn build(blueprints: impl IntoIterator<Item = (String, Vec<(String, u32)>)>) -> Self {
        Self {
            by_blueprint: blueprints
                .into_iter()
                .map(|(id, costs)| (id.to_lowercase(), costs))
                .collect(),
        }
    }

    /// The costs of blueprint `id` (any case); none for an unknown one.
    pub fn of(&self, id: &str) -> &[(String, u32)] {
        self.by_blueprint
            .get(&id.to_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_recipe_asks_for_its_achievements_and_a_blocking_unlock_ends_its_recrafts() {
        use super::RecipeDetails;
        // Rows of build 25810808: a season recipe with `RequiredAchievementID`, a one-time
        // recipe whose `UnlockedAchievementID` blocks a recraft, and one its achievement hides.
        let required = RecipeDetails {
            required_achievement: Some("Season_Unlock".to_owned()),
            first_craft_achievement: Some("FirstCraft_X".to_owned()),
            ..RecipeDetails::default()
        };
        assert!(!required.open_to(|_| false));
        assert!(required.open_to(|id| id == "Season_Unlock"));
        assert_eq!(
            required.craft_achievements().collect::<Vec<_>>(),
            ["FirstCraft_X"]
        );
        let once = RecipeDetails {
            unlocks: Some("House_Made".to_owned()),
            unlock_blocks_recraft: true,
            ..RecipeDetails::default()
        };
        assert!(once.open_to(|_| false));
        assert!(!once.open_to(|id| id == "House_Made"), "made once");
        let hidden = RecipeDetails {
            hidden_by: Some("Upgrade_Done".to_owned()),
            ..RecipeDetails::default()
        };
        assert!(!hidden.open_to(|id| id == "Upgrade_Done"));
        assert!(RecipeDetails::default().open_to(|_| false));
    }

    use super::*;

    /// A T4 armor recipe of base gear score 400 and base tier 4.
    fn armor_recipe() -> RecipeDetails {
        RecipeDetails {
            base_gear_score: Some(400),
            base_tier: 4,
            gear_score_bonus: steps_of(Some("5,10,15,20,25")),
            gear_score_reduction: steps_of(Some("5, 10,x")),
            ..RecipeDetails::default()
        }
    }

    #[test]
    fn a_craft_range_adds_rank_and_ingredient_bonuses() {
        let recipe = armor_recipe();
        assert_eq!(recipe.gear_score_reduction, [5, 10]);
        let ingot = IngredientGearScore {
            base_bonus: 15,
            max_bonus: 5,
            category_tier: None,
        };
        let higher = IngredientGearScore {
            category_tier: Some(5),
            ..IngredientGearScore::default()
        };
        let lower = IngredientGearScore {
            category_tier: Some(2),
            ..IngredientGearScore::default()
        };
        assert_eq!(
            recipe.gear_score_range(0, (20, 75), &[ingot, higher]),
            CraftGearScore { min: 440, max: 500 }
        );
        assert_eq!(
            recipe.gear_score_range(0, (0, 0), &[lower]),
            CraftGearScore { min: 390, max: 390 }
        );
        let fixed = RecipeDetails {
            disallow_bonuses: true,
            ..armor_recipe()
        };
        assert_eq!(
            fixed.gear_score_range(0, (20, 75), &[ingot]),
            CraftGearScore { min: 400, max: 400 }
        );
        let own = RecipeDetails::default();
        assert_eq!(own.gear_score_range(100, (0, 5), &[]).max, 105);
    }

    #[test]
    fn a_blueprints_costs_are_found_in_any_case() {
        let costs = StructureCosts::build([(
            "CampT1".to_owned(),
            vec![("WoodT1".to_owned(), 5), ("FlintT1".to_owned(), 1)],
        )]);
        assert_eq!(
            costs.of("campt1"),
            [("WoodT1".to_owned(), 5), ("FlintT1".to_owned(), 1)]
        );
        assert!(costs.of("").is_empty());
    }

    /// The retail rows of the skinning knife and the Light Ration (build 25810808), with the
    /// categories of pork and wood.
    fn recipes() -> Recipes {
        let item = |name: &str| Ingredient {
            kind: IngredientKind::Item(name.to_owned()),
            quantity: 1,
        };
        Recipes::build(
            [
                Recipe {
                    id: "1hSkinningKnifeT1".to_owned(),
                    item: "1hSkinningKnifeT1".to_owned(),
                    quantity: 1,
                    ingredients: vec![item("FlintT1"), item("WoodT1")],
                    stations: vec!["engineering2".to_owned(), "camp1".to_owned()],
                },
                Recipe {
                    id: "Procedural_HealthRecoveryT1".to_owned(),
                    item: "FoodHealthRecoveryT1".to_owned(),
                    quantity: 1,
                    ingredients: vec![Ingredient {
                        kind: IngredientKind::Category("FoodIngredientT1".to_owned()),
                        quantity: 1,
                    }],
                    stations: vec!["cooking2".to_owned(), "camp1".to_owned()],
                },
            ],
            [
                (
                    "PorkT1".to_owned(),
                    "MeatT1,Meats,FoodIngredient,FoodIngredientT1".to_owned(),
                ),
                ("WoodT1".to_owned(), "Wood".to_owned()),
            ],
        )
    }

    #[test]
    fn a_recipe_is_found_by_the_crc_of_its_id_and_takes_its_ingredients() {
        // Retail 2997 recipe CRCs (beach-quests note).
        const KNIFE: u32 = 0xa546_f7c5;
        const RATION: u32 = 0x968f_ae8b;
        let recipes = recipes();
        let knife = recipes.get(KNIFE).expect("knife");
        assert_eq!(knife.item, "1hSkinningKnifeT1");
        assert!(knife.made_at("Camp1"));
        assert!(!knife.made_at("cooking2"));
        assert!(recipes.fits(&knife.ingredients[0], "flintt1"));
        assert!(!recipes.fits(&knife.ingredients[0], "WoodT1"));
        let ration = recipes.get(RATION).expect("ration");
        assert_eq!(ration.item, "FoodHealthRecoveryT1");
        assert!(recipes.fits(&ration.ingredients[0], "PorkT1"), "a category");
        assert!(!recipes.fits(&ration.ingredients[0], "WoodT1"));
        assert_eq!(count_of(None), 1);
        assert_eq!(count_of(Some(0)), 1);
        assert_eq!(count_of(Some(3)), 3);
    }

    #[test]
    fn a_station_search_finds_the_ingredients_of_its_recipes() {
        let recipes = recipes();
        let cooking: Vec<&Ingredient> =
            recipes.ingredients_at(lowercase_crc32("cooking")).collect();
        assert_eq!(cooking.len(), 1);
        assert!(recipes.fits(cooking[0], "PorkT1"));
        let camp: Vec<&Ingredient> = recipes.ingredients_at(lowercase_crc32("camp")).collect();
        assert_eq!(camp.len(), 3, "the knife's two and the ration's one");
        assert_eq!(
            recipes.ingredients_at(lowercase_crc32("cooking2")).count(),
            0
        );
    }

    #[test]
    fn a_station_makes_the_recipes_of_its_name_up_to_its_tier() {
        let recipes = recipes();
        let knife = recipes
            .get(lowercase_crc32("1hSkinningKnifeT1"))
            .expect("knife");
        assert!(knife.made_at("engineering2"));
        assert!(knife.made_at("Engineering5"), "a higher tier station");
        assert!(!knife.made_at("engineering1"), "a lower tier station");
        assert!(knife.made_at("camp3"));
        assert!(!knife.made_at("camp1_standing"), "another name");
        assert!(!knife.made_at("engineering"), "no tier");
        assert_eq!(name_and_tier("blacksmith5"), ("blacksmith", Some(5)));
        assert_eq!(name_and_tier("camp1_standing"), ("camp1_standing", None));
        assert_eq!(name_and_tier("12"), ("12", None));
    }

    #[test]
    fn a_recipes_details_are_found_by_the_crc_of_its_id() {
        let ration = "Procedural_HealthRecoveryT1";
        let recipes = recipes().with_details([
            (
                ration.to_owned(),
                RecipeDetails {
                    tradeskill: Some("Cooking".to_owned()),
                    recipe_level: 0,
                    game_event: Some("Craft_CookingT1".to_owned()),
                    fee: 0,
                    ..RecipeDetails::default()
                },
            ),
            (ration.to_owned(), RecipeDetails::default()),
        ]);
        let details = recipes.details(lowercase_crc32(ration));
        assert_eq!(
            details.game_event.as_deref(),
            Some("Craft_CookingT1"),
            "the first row wins"
        );
        assert_eq!(details.tradeskill.as_deref(), Some("Cooking"));
        assert_eq!(recipes.details(0), RecipeDetails::default());
    }
}
