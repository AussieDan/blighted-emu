// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shops' offers (economy note `local-data/scratch-work/run15/economy/REPORT.md`, section
//! 7): every row of `ItemCurrencyConversionData` (`javelindata_itemcurrencyconversion`) that can
//! be bought, by the CRC-32 of its lowercase `ConversionID`, the id the client's purchase (3741)
//! names.
//!
//! A row costs coins (`BuyCurrencyCost`, hundredths, as the wallet keeps them), items of the bag
//! (`BuyCurrencyItemName` times `BuyCurrencyItemCost`), and points of categorical progressions
//! (`BuyCategoricalProgressionCost` of `CategoricalProgressionId`, `BuyProgression2Cost` of
//! `BuyProgression2Id`). It may need a rank (`RequiredCategoricalProgressionRank` of
//! `RankCheckCategoricalProgressionId`) and wait between two purchases (`BuyCooldownSeconds`).
//! It gives items (`ItemID` times `ItemQty`) or an achievement (`AchievementId`: the stables'
//! mounts). A row with an `EntitlementId` needs a paid entitlement, which this server never
//! grants. `BuyProgression3Id` and `BuyProgression3Cost` are empty in every row of the client
//! data, and no row is `Sold`.

use std::collections::HashMap;

use nw_content::Tables;
use nw_content::generated::ItemCurrencyConversionData;
use nw_formats::objectstream::lowercase_crc32;

/// Milliseconds per second: cooldowns are whole seconds.
const MS_PER_SECOND: u64 = 1_000;

/// A cost in points of one categorical progression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressionCost {
    /// `CategoricalProgressionId`.
    pub progression: String,
    /// Points per purchase.
    pub points: u32,
}

/// One row a shop sells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopOffer {
    /// `ConversionID`.
    pub conversion_id: String,
    /// Coins per purchase, hundredths.
    pub coins: u64,
    /// An item of the bag and its quantity per purchase.
    pub item_cost: Option<(String, u32)>,
    /// Points of categorical progressions per purchase.
    pub progression_costs: Vec<ProgressionCost>,
    /// The progression and its least rank (zero-based) that the buyer needs.
    pub required_rank: Option<(String, u16)>,
    /// Time between two purchases of one character.
    pub cooldown_ms: Option<u64>,
    /// The item and its quantity per purchase.
    pub item: Option<(String, u32)>,
    /// The achievement it gives.
    pub achievement: Option<String>,
    /// Whether it needs a paid entitlement.
    pub needs_entitlement: bool,
}

impl ShopOffer {
    /// The offer of `row`; `None` for a row that cannot be bought or whose numbers are out of
    /// range.
    fn of(row: &ItemCurrencyConversionData) -> Option<Self> {
        if row.bought == 0 {
            return None;
        }
        let points = |value: Option<i64>| value.and_then(|value| u32::try_from(value).ok());
        let progression_costs = [
            (
                row.categorical_progression_id.as_ref(),
                row.buy_categorical_progression_cost,
            ),
            (row.buy_progression2_id.as_ref(), row.buy_progression2_cost),
        ]
        .into_iter()
        .filter_map(|(progression, cost)| {
            let amount = points(cost).filter(|amount| *amount > 0)?;
            Some(ProgressionCost {
                progression: progression?.clone(),
                points: amount,
            })
        })
        .collect();
        let item_cost = match (
            &row.buy_currency_item_name,
            points(row.buy_currency_item_cost),
        ) {
            (Some(item), Some(quantity)) if quantity > 0 => Some((item.clone(), quantity)),
            _ => None,
        };
        let item = match (&row.item_id, points(row.item_qty)) {
            (Some(item), Some(quantity)) if quantity > 0 => Some((item.clone(), quantity)),
            _ => None,
        };
        let required_rank = row
            .rank_check_categorical_progression_id
            .clone()
            .zip(
                row.required_categorical_progression_rank
                    .and_then(|rank| u16::try_from(rank).ok()),
            )
            .filter(|(_, rank)| *rank > 0);
        Some(Self {
            conversion_id: row.conversion_id.clone(),
            coins: row
                .buy_currency_cost
                .map_or(Some(0), |coins| u64::try_from(coins).ok())?,
            item_cost,
            progression_costs,
            required_rank,
            cooldown_ms: row
                .buy_cooldown_seconds
                .and_then(|seconds| u64::try_from(seconds).ok())
                .filter(|seconds| *seconds > 0)
                .map(|seconds| seconds.saturating_mul(MS_PER_SECOND)),
            item,
            achievement: row.achievement_id.clone().filter(|id| !id.is_empty()),
            needs_entitlement: row.entitlement_id.as_ref().is_some_and(|id| !id.is_empty()),
        })
    }
}

/// The offers of every shop, by the CRC-32 of their lowercase `ConversionID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShopCatalog {
    offers: HashMap<u32, ShopOffer>,
}

impl ShopCatalog {
    /// The offers of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::of(
            tables
                .item_currency_conversion_data
                .iter()
                .flat_map(|table| &table.rows),
        )
    }

    /// The offers of `rows`.
    pub fn of<'a>(rows: impl IntoIterator<Item = &'a ItemCurrencyConversionData>) -> Self {
        Self {
            offers: rows
                .into_iter()
                .filter_map(ShopOffer::of)
                .map(|offer| (lowercase_crc32(&offer.conversion_id), offer))
                .collect(),
        }
    }

    /// How many offers there are.
    pub fn len(&self) -> usize {
        self.offers.len()
    }

    /// Whether there is no offer (content without the table).
    pub fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }

    /// Whether an offer gives item `item` (any case).
    pub fn sells(&self, item: &str) -> bool {
        self.offers.values().any(|offer| {
            offer
                .item
                .as_ref()
                .is_some_and(|(given, _)| given.eq_ignore_ascii_case(item))
        })
    }

    /// The offer whose lowercase `ConversionID` has the CRC-32 `crc`.
    pub fn by_crc(&self, crc: u32) -> Option<&ShopOffer> {
        self.offers.get(&crc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(conversion_id: &str) -> ItemCurrencyConversionData {
        ItemCurrencyConversionData {
            achievement_id: None,
            bought: 1,
            buy_categorical_progression_cost: None,
            buy_cooldown_seconds: None,
            buy_currency_cost: None,
            buy_currency_item_cost: None,
            buy_currency_item_name: None,
            buy_progression2_cost: None,
            buy_progression2_id: None,
            buy_progression3_cost: None,
            buy_progression3_id: None,
            categorical_progression_id: None,
            conversion_id: conversion_id.to_owned(),
            display_order: None,
            entitlement_id: None,
            in_contracts: None,
            item_id: None,
            item_qty: None,
            perk1: None,
            perk2: None,
            perk3: None,
            perk4: None,
            rank_check_categorical_progression_id: None,
            required_categorical_progression_rank: None,
            sell_azoth_cost: None,
            sell_categorical_progression_cost: None,
            sell_currency_cost: None,
            sold: 0,
        }
    }

    /// The client rows of the captured purchases: the stable's horse (`Mount-quest`), a war camp
    /// potion and an outpost rush food.
    fn retail_rows() -> Vec<ItemCurrencyConversionData> {
        vec![
            ItemCurrencyConversionData {
                achievement_id: Some("AwardAchievement_Mount_Horse_2".to_owned()),
                buy_currency_cost: Some(1_000_000),
                ..row("AwardAchievement_Mount_Horse_2")
            },
            ItemCurrencyConversionData {
                buy_categorical_progression_cost: Some(20),
                categorical_progression_id: Some("Battle_Token".to_owned()),
                rank_check_categorical_progression_id: Some("Battle_Token".to_owned()),
                item_id: Some("PotionCleanseFTW".to_owned()),
                item_qty: Some(3),
                ..row("PotionCleanseFTW")
            },
            ItemCurrencyConversionData {
                buy_currency_item_cost: Some(25),
                buy_currency_item_name: Some("OutpostRushAzothEssence".to_owned()),
                categorical_progression_id: Some("Battle_Token".to_owned()),
                item_id: Some("OR_FoodPvP".to_owned()),
                item_qty: Some(5),
                ..row("OR_FoodPvP")
            },
            ItemCurrencyConversionData {
                entitlement_id: Some("SomePaidEntitlement".to_owned()),
                required_categorical_progression_rank: Some(2),
                rank_check_categorical_progression_id: Some("Syndicate".to_owned()),
                buy_cooldown_seconds: Some(86_400),
                ..row("PaidRow")
            },
            ItemCurrencyConversionData {
                bought: 0,
                ..row("NotForSale")
            },
        ]
    }

    #[test]
    fn the_retail_rows_become_offers_by_their_crc() {
        let rows = retail_rows();
        let catalog = ShopCatalog::of(&rows);
        assert!(
            catalog.sells("potioncleanseftw"),
            "an offer's item, any case"
        );
        assert!(
            !catalog.sells("AwardAchievement_Mount_Horse_2"),
            "an achievement is no item"
        );
        assert_eq!(catalog.len(), 4);
        let horse = catalog.by_crc(0x87c0_53c5).expect("the stable's horse");
        assert_eq!(horse.coins, 1_000_000);
        assert_eq!(
            horse.achievement.as_deref(),
            Some("AwardAchievement_Mount_Horse_2")
        );
        assert!(horse.item.is_none() && horse.progression_costs.is_empty());
        let potion = catalog
            .by_crc(lowercase_crc32("PotionCleanseFTW"))
            .expect("a war camp potion");
        assert_eq!(
            potion.progression_costs,
            vec![ProgressionCost {
                progression: "Battle_Token".to_owned(),
                points: 20,
            }]
        );
        assert_eq!(potion.item, Some(("PotionCleanseFTW".to_owned(), 3)));
        assert_eq!(potion.required_rank, None, "rank 0 needs nothing");
        let food = catalog
            .by_crc(lowercase_crc32("OR_FoodPvP"))
            .expect("an outpost rush food");
        assert_eq!(
            food.item_cost,
            Some(("OutpostRushAzothEssence".to_owned(), 25))
        );
        assert!(
            food.progression_costs.is_empty(),
            "no progression cost without a number"
        );
        let paid = catalog
            .by_crc(lowercase_crc32("PaidRow"))
            .expect("a paid row");
        assert!(paid.needs_entitlement);
        assert_eq!(paid.required_rank, Some(("Syndicate".to_owned(), 2)));
        assert_eq!(paid.cooldown_ms, Some(86_400_000));
        assert!(catalog.by_crc(lowercase_crc32("NotForSale")).is_none());
    }
}
