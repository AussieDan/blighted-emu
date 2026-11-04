// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules of dropping and using items: the use and drop rules of each
//! `MasterItemDefinitions` row (its stack size is in [`crate::starter::KitItem`]), and what each
//! consumable does (`ConsumableItemDefinitions` and
//! the `StatusEffectData` rows it adds).
//!
//! A consumable is named by its item id, or by the `ItemStatsRef` of its item (retail potions
//! have no `ItemStatsRef`: their consumable id is the item id). Each status effect it adds
//! changes the vitals once (`BaseDuration` -1 and no `TickRate`) or once per `TickRate` seconds
//! for its duration: the consumable's `DurationOverrides` when set, else the effect's
//! `BaseDuration`. A change is `Health` plus `HealthModifierPercent` of the maximum health, and
//! `Stamina`, each times the consumable's `StatusEffectMod`.

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;

/// Separates the status effects of an `AddStatusEffects` cell.
const EFFECT_SEPARATOR: char = '+';
/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;
/// The `BaseDuration` of an effect that has no duration of its own (retail `PotionHeal`).
const NO_DURATION: f64 = -1.0;
/// The potency of a consumable without a `StatusEffectMod`.
const PLAIN_POTENCY: f64 = 1.0;

/// How an item may be used and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRule {
    /// `ConsumeOnUse`: a use takes one of the stack.
    pub consume_on_use: bool,
    /// `NotDroppable`: the player may not drop it.
    pub not_droppable: bool,
    /// `Nonremovable`: the player may not drop or destroy it.
    pub nonremovable: bool,
}

/// One change of the vitals that a used consumable makes.
#[derive(Debug, Clone, PartialEq)]
pub struct VitalsEffect {
    /// The `StatusID` that makes it.
    pub status_id: String,
    /// The flat health it gives (`Health`, times the potency).
    pub health: f64,
    /// The share of the maximum health it gives (`HealthModifierPercent`, times the potency).
    pub health_share: f64,
    /// The stamina it gives (`Stamina`, times the potency).
    pub stamina: f64,
    /// How the change repeats; `None`: once, at the use.
    pub ticks: Option<Ticks>,
    /// `RemoveOnDeath`: a death ends it.
    pub remove_on_death: bool,
    /// Whether it has a visual script (`FxScriptOn`): the client plays it on the 608 that names
    /// the effect (retail `PotionHeal`).
    pub has_fx: bool,
}

/// The repeats of a vitals change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticks {
    /// `DelayInitialTick`: the first change comes this long after the use.
    pub first_ms: u64,
    /// `TickRate`: the time between two changes.
    pub every_ms: u64,
    /// The effect's duration: no change comes later than this after the use.
    pub duration_ms: u64,
}

/// What using a consumable does.
#[derive(Debug, Clone, PartialEq)]
pub struct Consumable {
    /// `ConsumableID`.
    pub id: String,
    /// `CooldownId` and `CooldownDuration` (seconds), when it has a cooldown: consumables of one
    /// id share it.
    pub cooldown: Option<(String, u32)>,
    /// The vitals changes of the status effects it adds.
    pub effects: Vec<VitalsEffect>,
    /// `DisableUseInCombat`.
    pub disabled_in_combat: bool,
    /// The CRC-32 of the lowercase id of every status effect it adds (`AddStatusEffects`), for
    /// the status effect state of its user (`StatusEffectsComponentReplicatedState`).
    pub status_effects: Vec<u32>,
    /// `StatusEffectMod`: the potency of the effects it adds.
    pub potency: f64,
    /// `DurationOverrides`: how long the effects it adds last, when set.
    pub duration_override_ms: Option<u64>,
    /// `MannequinTag`: the item use its user plays (`Eat` for food, `Potion`); the use takes
    /// effect at that use's moment of the user's stance.
    pub use_tag: Option<String>,
    /// `IsMountConsumable`: a mount food, used while its user rides.
    pub mount_only: bool,
    /// `RequiredInactiveLootLimitID`: the loot limit that must not be at its limit for a use
    /// (the mount foods' `Mount_FoodDailyXP_Limit`, 3 a day).
    pub required_inactive_limit: Option<String>,
    /// `RemoveStatusEffectCategories`, lowercase: a use first ends the held effects of these
    /// categories (a new mount food ends the one before, `MountFoodUtility`).
    pub remove_categories: Vec<String>,
}

/// One `StatusEffectData` row, as the consumables need it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectRow {
    /// `StatusID`.
    pub status_id: String,
    /// `BaseDuration`, seconds; -1 for none.
    pub base_duration_s: Option<f64>,
    /// `TickRate`, seconds.
    pub tick_s: Option<f64>,
    /// `DelayInitialTick`, seconds.
    pub delay_s: Option<f64>,
    /// `Health`.
    pub health: Option<f64>,
    /// `HealthModifierPercent`.
    pub health_share: Option<f64>,
    /// `Stamina`.
    pub stamina: Option<f64>,
    /// `RemoveOnDeath` is set.
    pub remove_on_death: bool,
    /// `FxScriptOn` names a script.
    pub has_fx: bool,
}

/// One `ConsumableItemDefinitions` row, as the consumables need it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumableRow {
    /// `ConsumableID`.
    pub id: String,
    /// `AddStatusEffects`.
    pub add_status_effects: Option<String>,
    /// `StatusEffectMod`.
    pub potency: Option<f64>,
    /// `DurationOverrides`, seconds; 0 for none.
    pub duration_override_s: Option<f64>,
    /// `CooldownId`.
    pub cooldown_id: Option<String>,
    /// `CooldownDuration`, seconds.
    pub cooldown_s: Option<u32>,
    /// `DisableUseInCombat`.
    pub disabled_in_combat: bool,
    /// `MannequinTag`: the item use its user plays (`Eat`, `Potion`).
    pub use_tag: Option<String>,
    /// `IsMountConsumable`.
    pub mount_only: bool,
    /// `RequiredInactiveLootLimitID`.
    pub required_inactive_limit: Option<String>,
    /// `RemoveStatusEffectCategories`.
    pub remove_categories: Option<String>,
}

/// One `MasterItemDefinitions` row, as the rules need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRuleRow {
    /// `ItemID`.
    pub item_id: String,
    /// `ItemStatsRef`.
    pub stats_ref: Option<String>,
    /// `ConsumeOnUse` is set.
    pub consume_on_use: bool,
    /// `NotDroppable` is set.
    pub not_droppable: bool,
    /// `Nonremovable` is set.
    pub nonremovable: bool,
}

/// The plain content of the item rule sheets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemRuleSource {
    /// The master item rows.
    pub items: Vec<ItemRuleRow>,
    /// The consumable rows.
    pub consumables: Vec<ConsumableRow>,
    /// The status effect rows.
    pub effects: Vec<EffectRow>,
}

impl ItemRuleSource {
    /// The rule content of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let set = |value: Option<i64>| value.is_some_and(|value| value != 0);
        Self {
            items: tables
                .master_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| ItemRuleRow {
                    item_id: row.item_id.clone(),
                    stats_ref: row.item_stats_ref.clone(),
                    consume_on_use: set(row.consume_on_use),
                    not_droppable: row.not_droppable.unwrap_or(false),
                    nonremovable: set(row.nonremovable),
                })
                .collect(),
            consumables: tables
                .consumable_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| ConsumableRow {
                    id: row.consumable_id.clone(),
                    add_status_effects: row.add_status_effects.clone(),
                    // Small whole numbers.
                    potency: row.status_effect_mod.map(|potency| potency as f64),
                    duration_override_s: row.duration_overrides.map(|seconds| seconds as f64),
                    cooldown_id: row.cooldown_id.clone(),
                    cooldown_s: row
                        .cooldown_duration
                        .and_then(|seconds| u32::try_from(seconds).ok()),
                    disabled_in_combat: row.disable_use_in_combat.unwrap_or(false),
                    use_tag: row.mannequin_tag.clone().filter(|tag| !tag.is_empty()),
                    mount_only: row.is_mount_consumable.unwrap_or(false),
                    required_inactive_limit: row
                        .required_inactive_loot_limit_id
                        .clone()
                        .filter(|id| !id.is_empty()),
                    remove_categories: row.remove_status_effect_categories.clone(),
                })
                .collect(),
            effects: tables
                .status_effect_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| EffectRow {
                    status_id: row.status_id.clone(),
                    base_duration_s: row.base_duration,
                    tick_s: row.tick_rate,
                    delay_s: row.delay_initial_tick,
                    // Health values of a few thousand.
                    health: row.health.map(|health| health as f64),
                    health_share: row.health_modifier_percent,
                    stamina: row.stamina,
                    remove_on_death: set(row.remove_on_death),
                    has_fx: row.fx_script_on.as_deref().is_some_and(|fx| !fx.is_empty()),
                })
                .collect(),
        }
    }
}

/// The rules of every item and consumable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemRules {
    /// The rule of each item, by lowercase item id.
    items: HashMap<String, ItemRule>,
    /// The `ItemStatsRef` of each item that has one, by lowercase item id, lowercase.
    stats_refs: HashMap<String, String>,
    /// Each consumable, by lowercase consumable id.
    consumables: HashMap<String, Consumable>,
}

impl ItemRules {
    /// The rules of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(ItemRuleSource::from_tables(tables))
    }

    /// The rules of `source`.
    pub fn build(source: ItemRuleSource) -> Self {
        let effects: HashMap<String, &EffectRow> = source
            .effects
            .iter()
            .map(|row| (row.status_id.to_lowercase(), row))
            .collect();
        let consumables = source
            .consumables
            .iter()
            .map(|row| {
                let potency = row.potency.unwrap_or(PLAIN_POTENCY);
                let duration_override = row.duration_override_s.filter(|seconds| *seconds > 0.0);
                let effects = row
                    .add_status_effects
                    .as_deref()
                    .unwrap_or_default()
                    .split(EFFECT_SEPARATOR)
                    .filter_map(|id| effects.get(&id.trim().to_lowercase()))
                    .filter_map(|effect| vitals_effect(effect, potency, duration_override))
                    .collect();
                let consumable = Consumable {
                    id: row.id.clone(),
                    cooldown: row
                        .cooldown_id
                        .clone()
                        .zip(row.cooldown_s)
                        .filter(|(_, seconds)| *seconds > 0),
                    effects,
                    disabled_in_combat: row.disabled_in_combat,
                    status_effects: row
                        .add_status_effects
                        .as_deref()
                        .unwrap_or_default()
                        .split(EFFECT_SEPARATOR)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(lowercase_crc32)
                        .collect(),
                    potency,
                    duration_override_ms: duration_override
                        .map(|seconds| (seconds * MS_PER_SECOND).round() as u64),
                    use_tag: row.use_tag.clone(),
                    mount_only: row.mount_only,
                    required_inactive_limit: row.required_inactive_limit.clone(),
                    remove_categories: crate::status_effects::names(
                        row.remove_categories.as_deref(),
                    ),
                };
                (row.id.to_lowercase(), consumable)
            })
            .collect();
        Self {
            items: source
                .items
                .iter()
                .map(|row| {
                    (
                        row.item_id.to_lowercase(),
                        ItemRule {
                            consume_on_use: row.consume_on_use,
                            not_droppable: row.not_droppable,
                            nonremovable: row.nonremovable,
                        },
                    )
                })
                .collect(),
            stats_refs: source
                .items
                .iter()
                .filter_map(|row| {
                    Some((
                        row.item_id.to_lowercase(),
                        row.stats_ref.as_ref()?.to_lowercase(),
                    ))
                })
                .collect(),
            consumables,
        }
    }

    /// The rule of item `item_id` (any case).
    pub fn item(&self, item_id: &str) -> Option<&ItemRule> {
        self.items.get(&item_id.to_lowercase())
    }

    /// The consumable of item `item_id` (any case): the consumable of its id, else of its
    /// `ItemStatsRef`; `None` for an item that is no consumable.
    pub fn consumable(&self, item_id: &str) -> Option<&Consumable> {
        let id = item_id.to_lowercase();
        self.consumables.get(&id).or_else(|| {
            self.stats_refs
                .get(&id)
                .and_then(|stats| self.consumables.get(stats))
        })
    }
}

/// The vitals change of status effect `row` for a consumable of `potency` whose duration
/// override is `duration_override` (seconds); `None` for an effect that changes no vital.
fn vitals_effect(
    row: &EffectRow,
    potency: f64,
    duration_override: Option<f64>,
) -> Option<VitalsEffect> {
    let health = row.health.unwrap_or(0.0) * potency;
    let health_share = row.health_share.unwrap_or(0.0) * potency;
    let stamina = row.stamina.unwrap_or(0.0) * potency;
    if health == 0.0 && health_share == 0.0 && stamina == 0.0 {
        return None;
    }
    let duration_s = duration_override
        .or(row.base_duration_s)
        .filter(|seconds| *seconds != NO_DURATION && *seconds > 0.0);
    let ms = |seconds: f64| (seconds * MS_PER_SECOND).round() as u64;
    let ticks = row
        .tick_s
        .filter(|seconds| *seconds > 0.0)
        .zip(duration_s)
        .map(|(every_s, duration_s)| Ticks {
            first_ms: ms(row.delay_s.unwrap_or(0.0).max(0.0)),
            every_ms: ms(every_s),
            duration_ms: ms(duration_s),
        });
    Some(VitalsEffect {
        status_id: row.status_id.clone(),
        health,
        health_share,
        stamina,
        ticks,
        remove_on_death: row.remove_on_death,
        has_fx: row.has_fx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retail `PotionHealthT1` row (build 25810808): `PotionHeal`, cooldown 25 s.
    const POTION: &str = "PotionHealthT1";
    const POTION_HEALTH: f64 = 400.0;
    const POTION_SHARE: f64 = 0.1;
    const POTION_COOLDOWN_S: u32 = 25;
    /// The retail `SerumRegenT5` row: `SerumRegen` ticks each second for its 25 s override.
    const SERUM: &str = "SerumRegenT5";
    const SERUM_HEALTH: f64 = 400.0;
    const SERUM_SHARE: f64 = 0.011;
    const SERUM_STAMINA: f64 = 3.0;
    const SERUM_DURATION_S: f64 = 25.0;
    const SERUM_TICK_S: f64 = 1.0;
    /// A food whose stats name its consumable.
    const FOOD: &str = "FoodSTRT52";
    const FOOD_STATS: &str = "FoodSTRT52_Stats";
    const FOOD_TICK_S: f64 = 2.0;
    const FOOD_DURATION_S: f64 = 2400.0;
    const FOOD_SHARE: f64 = 0.01;

    fn effect(status_id: &str) -> EffectRow {
        EffectRow {
            status_id: status_id.to_owned(),
            base_duration_s: Some(NO_DURATION),
            tick_s: Some(0.0),
            delay_s: None,
            health: None,
            health_share: None,
            stamina: None,
            remove_on_death: true,
            has_fx: false,
        }
    }

    fn consumable(id: &str, effects: &str) -> ConsumableRow {
        ConsumableRow {
            id: id.to_owned(),
            add_status_effects: Some(effects.to_owned()),
            potency: Some(PLAIN_POTENCY),
            duration_override_s: Some(0.0),
            cooldown_id: None,
            cooldown_s: None,
            disabled_in_combat: false,
            use_tag: None,
            mount_only: false,
            required_inactive_limit: None,
            remove_categories: None,
        }
    }

    fn rules() -> ItemRules {
        ItemRules::build(ItemRuleSource {
            items: vec![
                ItemRuleRow {
                    item_id: POTION.to_owned(),
                    stats_ref: None,
                    consume_on_use: true,
                    not_droppable: false,
                    nonremovable: false,
                },
                ItemRuleRow {
                    item_id: FOOD.to_owned(),
                    stats_ref: Some(FOOD_STATS.to_owned()),
                    consume_on_use: true,
                    not_droppable: true,
                    nonremovable: false,
                },
            ],
            consumables: vec![
                ConsumableRow {
                    cooldown_id: Some("SurvivalPotion".to_owned()),
                    cooldown_s: Some(POTION_COOLDOWN_S),
                    ..consumable(POTION, "PotionHeal")
                },
                ConsumableRow {
                    duration_override_s: Some(SERUM_DURATION_S),
                    ..consumable(SERUM, "SerumRegen")
                },
                consumable(FOOD_STATS, "FoodSTRT52+FoodRecoveryT52"),
            ],
            effects: vec![
                EffectRow {
                    health: Some(POTION_HEALTH),
                    health_share: Some(POTION_SHARE),
                    has_fx: true,
                    ..effect("PotionHeal")
                },
                EffectRow {
                    tick_s: Some(SERUM_TICK_S),
                    health: Some(SERUM_HEALTH),
                    health_share: Some(SERUM_SHARE),
                    stamina: Some(SERUM_STAMINA),
                    ..effect("SerumRegen")
                },
                EffectRow {
                    base_duration_s: Some(FOOD_DURATION_S),
                    tick_s: None,
                    ..effect("FoodSTRT52")
                },
                EffectRow {
                    base_duration_s: Some(FOOD_DURATION_S),
                    tick_s: Some(FOOD_TICK_S),
                    health_share: Some(FOOD_SHARE),
                    remove_on_death: false,
                    ..effect("FoodRecoveryT52")
                },
            ],
        })
    }

    #[test]
    fn a_potion_heals_once_and_shares_its_cooldown_id() {
        let rules = rules();
        let potion = rules.consumable("potionhealtht1").expect("potion");
        assert_eq!(
            potion.cooldown,
            Some(("SurvivalPotion".to_owned(), POTION_COOLDOWN_S))
        );
        assert_eq!(potion.effects.len(), 1);
        let heal = &potion.effects[0];
        assert_eq!(
            (heal.health, heal.health_share, heal.ticks, heal.has_fx),
            (POTION_HEALTH, POTION_SHARE, None, true)
        );
        let rule = rules.item(POTION).expect("rule");
        assert!(rule.consume_on_use && !rule.not_droppable);
    }

    #[test]
    fn a_consumable_names_every_effect_it_adds_with_its_potency_and_override() {
        let rules = rules();
        let food = rules.consumable(FOOD).expect("food");
        assert_eq!(
            food.status_effects,
            vec![
                lowercase_crc32("FoodSTRT52"),
                lowercase_crc32("FoodRecoveryT52")
            ],
            "the attribute effect too"
        );
        assert_eq!(
            (food.potency, food.duration_override_ms),
            (PLAIN_POTENCY, None)
        );
        let serum = rules.consumable(SERUM).expect("serum");
        assert_eq!(serum.duration_override_ms, Some(25_000));
    }

    #[test]
    fn a_serum_ticks_for_its_override_and_food_through_its_stats() {
        let rules = rules();
        let serum = &rules.consumable(SERUM).expect("serum").effects[0];
        assert_eq!(
            serum.ticks,
            Some(Ticks {
                first_ms: 0,
                every_ms: 1_000,
                duration_ms: 25_000,
            })
        );
        assert_eq!(serum.stamina, SERUM_STAMINA);
        let food = rules.consumable(FOOD).expect("food by its stats");
        assert_eq!(
            food.effects.len(),
            1,
            "the attribute effect changes no vital"
        );
        assert_eq!(
            food.effects[0].ticks,
            Some(Ticks {
                first_ms: 0,
                every_ms: 2_000,
                duration_ms: 2_400_000,
            })
        );
        assert!(!food.effects[0].remove_on_death);
        let rule = rules.item(FOOD).expect("rule");
        assert!(rule.not_droppable);
        assert!(rules.consumable("NoSuchItem").is_none());
    }
}
