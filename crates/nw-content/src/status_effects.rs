// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules of every status effect (`StatusEffectData`, all 36 tables of build 25810808, 4,517
//! rows) and the effects that attack rows put on their target (`DamageData.StatusEffect`).
//!
//! The wire names an effect by its key: the position of the CRC-32 of its lowercase id in the
//! ascending list of the CRC-32 of every row of every `StatusEffectData` table, repeated ids
//! kept, a repeated id at its first position (run-15 combat study, Observed on seven retail
//! captures: `Sanctuary` is in two tables at 1316 and 1317 and always sent as 1316; with the
//! repeats left out every key above 1316 names an unrelated effect). The key is a big-endian
//! `u16` on the wire (`ntohs` in the client's reader).
//!
//! An effect lasts its `BaseDuration` (none: -1 or 0), longer or shorter by the
//! `EffectDurationMods` of the effects the target has whose categories it shares, at most its
//! `DurationMax`. A new application of an effect the target has adds a stack up to `StackMax`
//! (retail `Status_Passive_Lifestaff_Healing_HealingBuffonBasic`: stacks 1, 2, 3, 3, 3 and the
//! application count 1 to 6) and moves its end when `RefreshDuration` is not false. Its potency
//! is one stack's potency times the stacks, at most `PotencyMax`, times one plus the
//! `EffectPotencyMods` of the target's other effects for its categories (retail `FoodRecoveryT5`
//! potency 9 rose to 27 while `Status_OutOfCombat_PassiveRegen`, `PassiveFoodRegen=2`, ran).

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;
use crate::generated::StatusEffectData;

/// Separates the entries of a list cell (`EffectCategories`, `AddStatusEffects`, mods).
const LIST_SEPARATOR: char = '+';
/// Separates the effects of a `DamageData.StatusEffect` cell (a repeated id adds a stack).
const HIT_EFFECT_SEPARATOR: char = ',';
/// Separates a category from its value in a mod cell (`Root=-0.2`).
const MOD_SEPARATOR: char = '=';
/// The `HitCondition` of an effect that counts the hits its holder takes.
const HIT_TAKEN_CONDITION: &str = "OnHitTaken";
/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;
/// The stack limit of an effect whose `StackMax` is empty (567 rows): one stack.
const DEFAULT_STACK_MAX: u8 = 1;

/// The damage types that have an `Abs*` and a `Dmg*` column.
const DAMAGE_TYPE_COUNT: usize = 12;
/// The `TypeID` of the damage type of each `Abs*` and `Dmg*` column, in column order.
const DAMAGE_TYPE_COLUMNS: [&str; DAMAGE_TYPE_COUNT] = [
    "Acid",
    "Arcane",
    "Corruption",
    "Fire",
    "Ice",
    "Lightning",
    "Nature",
    "Siege",
    "Slash",
    "Standard",
    "Strike",
    "Thrust",
];

/// The gathering tradeskill of each `EFF_*` column, in column order.
const GATHER_EFFICIENCY_COLUMNS: [&str; 4] = ["Harvesting", "Logging", "Mining", "Skinning"];
/// The crafting tradeskill of each `MGS_*` and `MaxGS_*` column pair, in column order.
const CRAFT_GEAR_SCORE_COLUMNS: [&str; 5] = [
    "Arcana",
    "Armoring",
    "Engineering",
    "Jewelcrafting",
    "Weaponsmithing",
];

/// The rules of one status effect.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusEffectRule {
    /// `StatusID`.
    pub id: String,
    /// The CRC-32 of the lowercase id.
    pub crc: u32,
    /// The wire key ([`StatusEffectRules::key`]).
    pub key: u16,
    /// How long it lasts; `None`: until something removes it.
    pub duration_ms: Option<u64>,
    /// `DurationMax`: the longest it may last after the mods.
    pub duration_max_ms: Option<u64>,
    /// `TickRate`: the time between two of its vitals changes; `None`: it does not tick.
    pub tick_ms: Option<u64>,
    /// `DelayInitialTick`: the first change comes this long after the application.
    pub first_tick_ms: u64,
    /// `StackMax`, at least 1.
    pub stack_max: u8,
    /// A new application moves its end (`RefreshDuration` not false).
    pub refresh_duration: bool,
    /// `PotencyMax`: the most its stacks give.
    pub potency_max: Option<f64>,
    /// `EffectCategories`, lowercase.
    pub categories: Vec<String>,
    /// `RemoveStatusEffectCategories`: its application ends the effects of these categories.
    pub remove_categories: Vec<String>,
    /// `RemoveStatusEffects`: its application ends these effects (CRC-32 of the lowercase ids).
    pub remove_effects: Vec<u32>,
    /// `AntirequisiteEffectCategories`: it does not apply while the holder has an effect of one
    /// of these categories.
    pub blocked_by: Vec<String>,
    /// `Health`: health per change.
    pub health: f64,
    /// `HealthModifierPercent`: the share of the max health per change.
    pub health_share: f64,
    /// `HealthModifierDamageBased`: the share of the applying hit per change.
    pub hit_share: f64,
    /// `Stamina`: stamina per change.
    pub stamina: f64,
    /// The `DamageTypeData` `IntID` of its `DamageType`.
    pub damage_type: Option<u8>,
    /// `RemoveOnDeath`.
    pub remove_on_death: bool,
    /// `RemoveOnRespawn`.
    pub remove_on_respawn: bool,
    /// `AllowAddIfDead`.
    pub allow_add_if_dead: bool,
    /// `UseLightweightReplication`: it goes in the lightweight map of the 4236 state.
    pub lightweight: bool,
    /// `IsClientPredicted`: its lightweight value is a 30 Hz tick, not a time.
    pub client_predicted: bool,
    /// `StopOnHitCount` with the `OnHitTaken` condition: it ends after this many hits taken.
    pub stop_on_hits_taken: Option<u32>,
    /// `OnEndStatusEffect`: added when it ends (CRC-32 of the lowercase ids).
    pub on_end: Vec<u32>,
    /// `OnTickStatusEffect`: added at each change (CRC-32 of the lowercase ids).
    pub on_tick: Vec<u32>,
    /// `EffectPotencyMods`: (lowercase category, mod) for the holder's other effects.
    pub potency_mods: Vec<(String, f64)>,
    /// `EffectDurationMods`: (lowercase category, mod) for the effects the holder gets.
    pub duration_mods: Vec<(String, f64)>,
    /// The `Abs*` columns: (damage type `IntID`, share of the hit taken off).
    pub absorption: Vec<(u8, f64)>,
    /// The `Dmg*` columns: (damage type `IntID`, share added to the holder's hits).
    pub damage_mods: Vec<(u8, f64)>,
    /// The `EFF_*` columns: (gathering tradeskill, gathering efficiency added).
    pub gather_efficiency: Vec<(String, f64)>,
    /// The `MGS_*` and `MaxGS_*` columns: (crafting tradeskill, gear score added to the least
    /// and to the most gear score of its crafts).
    pub craft_gear_score: Vec<(String, i64, i64)>,
}

impl StatusEffectRule {
    /// Whether one of its categories is `category` (lowercase).
    pub fn has_category(&self, category: &str) -> bool {
        self.categories.iter().any(|own| own == category)
    }

    /// Whether it changes the vitals at its changes.
    pub fn changes_vitals(&self) -> bool {
        self.health != 0.0
            || self.health_share != 0.0
            || self.hit_share != 0.0
            || self.stamina != 0.0
    }
}

/// One `StatusEffectData` row, as the rules need it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectSourceRow {
    /// `StatusID`.
    pub status_id: String,
    /// `BaseDuration`, seconds.
    pub base_duration_s: Option<f64>,
    /// `DurationMax`, seconds.
    pub duration_max_s: Option<f64>,
    /// `TickRate`, seconds.
    pub tick_s: Option<f64>,
    /// `DelayInitialTick`, seconds.
    pub delay_s: Option<f64>,
    /// `StackMax`.
    pub stack_max: Option<i64>,
    /// `RefreshDuration`.
    pub refresh_duration: Option<bool>,
    /// `PotencyMax`.
    pub potency_max: Option<f64>,
    /// `EffectCategories`.
    pub categories: Option<String>,
    /// `RemoveStatusEffectCategories`.
    pub remove_categories: Option<String>,
    /// `RemoveStatusEffects`.
    pub remove_effects: Option<String>,
    /// `AntirequisiteEffectCategories`.
    pub antirequisite_categories: Option<String>,
    /// `Health`.
    pub health: Option<f64>,
    /// `HealthModifierPercent`.
    pub health_share: Option<f64>,
    /// `HealthModifierDamageBased`.
    pub hit_share: Option<f64>,
    /// `Stamina`.
    pub stamina: Option<f64>,
    /// `DamageType`.
    pub damage_type: Option<String>,
    /// `RemoveOnDeath` is set.
    pub remove_on_death: bool,
    /// `RemoveOnRespawn` is set.
    pub remove_on_respawn: bool,
    /// `AllowAddIfDead`.
    pub allow_add_if_dead: bool,
    /// `UseLightweightReplication`.
    pub lightweight: bool,
    /// `IsClientPredicted`.
    pub client_predicted: bool,
    /// `StopOnHitCount`.
    pub stop_on_hit_count: Option<i64>,
    /// `HitCondition`.
    pub hit_condition: Option<String>,
    /// `OnEndStatusEffect`.
    pub on_end: Option<String>,
    /// `OnTickStatusEffect`.
    pub on_tick: Option<String>,
    /// `EffectPotencyMods`.
    pub potency_mods: Option<String>,
    /// `EffectDurationMods`.
    pub duration_mods: Option<String>,
    /// The `Abs*` columns: (damage type `TypeID`, value).
    pub absorption: Vec<(String, f64)>,
    /// The `Dmg*` columns: (damage type `TypeID`, value).
    pub damage_mods: Vec<(String, f64)>,
    /// The `EFF_*` columns: (gathering tradeskill, value).
    pub gather_efficiency: Vec<(String, f64)>,
    /// The `MGS_*` and `MaxGS_*` columns: (crafting tradeskill, least, most).
    pub craft_gear_score: Vec<(String, i64, i64)>,
}

impl EffectSourceRow {
    /// The rule content of the datasheet row `row`.
    fn of(row: &StatusEffectData) -> Self {
        let set = |value: Option<i64>| value.is_some_and(|value| value != 0);
        let absorption = [
            row.abs_acid,
            row.abs_arcane,
            row.abs_corruption,
            row.abs_fire,
            row.abs_ice,
            row.abs_lightning,
            row.abs_nature,
            row.abs_siege,
            row.abs_slash,
            row.abs_standard,
            row.abs_strike,
            row.abs_thrust,
        ];
        let damage = [
            row.dmg_acid,
            row.dmg_arcane,
            row.dmg_corruption,
            row.dmg_fire,
            row.dmg_ice,
            row.dmg_lightning,
            row.dmg_nature,
            row.dmg_siege,
            row.dmg_slash,
            row.dmg_standard,
            row.dmg_strike,
            row.dmg_thrust,
        ];
        let by_type = |values: [Option<f64>; DAMAGE_TYPE_COUNT]| -> Vec<(String, f64)> {
            DAMAGE_TYPE_COLUMNS
                .iter()
                .zip(values)
                .filter_map(|(name, value)| {
                    value
                        .filter(|value| *value != 0.0)
                        .map(|value| ((*name).to_owned(), value))
                })
                .collect()
        };
        Self {
            status_id: row.status_id.clone(),
            base_duration_s: row.base_duration,
            duration_max_s: row.duration_max,
            tick_s: row.tick_rate,
            delay_s: row.delay_initial_tick,
            stack_max: row.stack_max,
            refresh_duration: row.refresh_duration,
            potency_max: row.potency_max,
            categories: row.effect_categories.clone(),
            remove_categories: row.remove_status_effect_categories.clone(),
            remove_effects: row.remove_status_effects.clone(),
            antirequisite_categories: row.antirequisite_effect_categories.clone(),
            // Health values of a few thousand.
            health: row.health.map(|health| health as f64),
            health_share: row.health_modifier_percent,
            hit_share: row.health_modifier_damage_based,
            stamina: row.stamina,
            damage_type: row.damage_type.clone(),
            remove_on_death: set(row.remove_on_death),
            remove_on_respawn: set(row.remove_on_respawn),
            allow_add_if_dead: row.allow_add_if_dead.unwrap_or(false),
            lightweight: row.use_lightweight_replication.unwrap_or(false),
            client_predicted: row.is_client_predicted.unwrap_or(false),
            stop_on_hit_count: row.stop_on_hit_count,
            hit_condition: row.hit_condition.clone(),
            on_end: row.on_end_status_effect.clone(),
            on_tick: row.on_tick_status_effect.clone(),
            potency_mods: row.effect_potency_mods.clone(),
            duration_mods: row.effect_duration_mods.clone(),
            absorption: by_type(absorption),
            damage_mods: by_type(damage),
            gather_efficiency: GATHER_EFFICIENCY_COLUMNS
                .iter()
                .zip([
                    row.eff_harvesting,
                    row.eff_logging,
                    row.eff_mining,
                    row.eff_skinning,
                ])
                .filter_map(|(skill, value)| {
                    value
                        .filter(|value| *value != 0.0)
                        .map(|value| ((*skill).to_owned(), value))
                })
                .collect(),
            craft_gear_score: CRAFT_GEAR_SCORE_COLUMNS
                .iter()
                .zip([
                    (row.mgs_arcana, row.max_gs_arcana),
                    (row.mgs_armoring, row.max_gs_armoring),
                    (row.mgs_engineering, row.max_gs_engineering),
                    (row.mgs_jewelcrafting, row.max_gs_jewelcrafting),
                    (row.mgs_weaponsmithing, row.max_gs_weaponsmithing),
                ])
                .filter(|(_, (least, most))| least.is_some() || most.is_some())
                .map(|(skill, (least, most))| {
                    (
                        (*skill).to_owned(),
                        least.unwrap_or_default(),
                        most.unwrap_or_default(),
                    )
                })
                .collect(),
        }
    }
}

/// One attack row that puts status effects on its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitEffectRow {
    /// The unique id of its damage table.
    pub table: String,
    /// `DamageID`.
    pub damage_id: String,
    /// `StatusEffect`: the effect ids, `,`-separated; a repeated id adds a stack.
    pub status_effects: String,
}

/// The plain content of the status effect sheets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusEffectSource {
    /// Every `StatusEffectData` row of every table, in table order.
    pub effects: Vec<EffectSourceRow>,
    /// The `DamageTypeData` rows: (`TypeID`, `IntID`).
    pub damage_types: Vec<(String, u8)>,
    /// The attack rows that put effects on their target.
    pub hit_effects: Vec<HitEffectRow>,
}

impl StatusEffectSource {
    /// The status effect content of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self {
            effects: tables
                .status_effect_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(EffectSourceRow::of)
                .collect(),
            damage_types: tables
                .damage_type_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| Some((row.type_id.clone(), u8::try_from(row.int_id).ok()?)))
                .collect(),
            hit_effects: tables
                .damage_data
                .iter()
                .flat_map(|table| table.rows.iter().map(move |row| (table, row)))
                .filter_map(|(table, row)| {
                    Some(HitEffectRow {
                        table: table.unique_id.clone(),
                        damage_id: row.damage_id.clone(),
                        status_effects: row
                            .status_effect
                            .clone()
                            .filter(|cell| !cell.trim().is_empty())?,
                    })
                })
                .collect(),
        }
    }
}

/// An attack row by the CRC-32 of its lowercase table and row ids: a small key that hit records
/// can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowKey {
    /// CRC-32 of the lowercase unique id of its damage table.
    pub table: u32,
    /// CRC-32 of the lowercase `DamageID`.
    pub row: u32,
}

impl RowKey {
    /// The key of row `damage_id` of table `table` (any case).
    pub fn of(table: &str, damage_id: &str) -> Self {
        Self {
            table: lowercase_crc32(table),
            row: lowercase_crc32(damage_id),
        }
    }
}

/// The rules of every status effect.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusEffectRules {
    /// Each effect, by the CRC-32 of its lowercase id (the first row of a repeated id).
    by_crc: HashMap<u32, StatusEffectRule>,
    /// The effects each attack row puts on its target.
    on_hit: HashMap<RowKey, Vec<u32>>,
}

impl StatusEffectRules {
    /// The rules of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(StatusEffectSource::from_tables(tables))
    }

    /// The rules of `source`.
    pub fn build(source: StatusEffectSource) -> Self {
        let mut sorted: Vec<u32> = source
            .effects
            .iter()
            .map(|row| lowercase_crc32(&row.status_id))
            .collect();
        sorted.sort_unstable();
        let damage_types: HashMap<String, u8> = source
            .damage_types
            .iter()
            .map(|(name, id)| (name.to_lowercase(), *id))
            .collect();
        let mut by_crc = HashMap::new();
        for row in &source.effects {
            let crc = lowercase_crc32(&row.status_id);
            if by_crc.contains_key(&crc) {
                continue;
            }
            // The first position of the CRC among the 4,517 sorted rows: below 2^16.
            let Ok(key) = u16::try_from(sorted.partition_point(|other| *other < crc)) else {
                continue;
            };
            by_crc.insert(crc, rule(row, crc, key, &damage_types));
        }
        let on_hit = source
            .hit_effects
            .iter()
            .map(|row| {
                (
                    RowKey::of(&row.table, &row.damage_id),
                    row.status_effects
                        .split(HIT_EFFECT_SEPARATOR)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(lowercase_crc32)
                        .collect(),
                )
            })
            .collect();
        Self { by_crc, on_hit }
    }

    /// The effect of CRC-32 `crc` (of its lowercase id).
    pub fn get(&self, crc: u32) -> Option<&StatusEffectRule> {
        self.by_crc.get(&crc)
    }

    /// The effect of id `id` (any case).
    pub fn by_id(&self, id: &str) -> Option<&StatusEffectRule> {
        self.get(lowercase_crc32(id))
    }

    /// The effects the attack row `row` puts on its target, one entry per stack.
    pub fn on_hit(&self, row: RowKey) -> &[u32] {
        self.on_hit.get(&row).map_or(&[], Vec::as_slice)
    }

    /// The number of effects.
    pub fn len(&self) -> usize {
        self.by_crc.len()
    }

    /// Whether there is no effect.
    pub fn is_empty(&self) -> bool {
        self.by_crc.is_empty()
    }
}

/// The rule of `row`, the effect of CRC-32 `crc` and wire key `key`.
fn rule(
    row: &EffectSourceRow,
    crc: u32,
    key: u16,
    damage_types: &HashMap<String, u8>,
) -> StatusEffectRule {
    let ms = |seconds: f64| (seconds * MS_PER_SECOND).round() as u64;
    let positive_ms = |seconds: Option<f64>| seconds.filter(|seconds| *seconds > 0.0).map(ms);
    let type_of = |name: &str| damage_types.get(&name.trim().to_lowercase()).copied();
    let typed = |values: &[(String, f64)]| {
        values
            .iter()
            .filter_map(|(name, value)| Some((type_of(name)?, *value)))
            .collect()
    };
    StatusEffectRule {
        id: row.status_id.clone(),
        crc,
        key,
        duration_ms: positive_ms(row.base_duration_s),
        duration_max_ms: positive_ms(row.duration_max_s),
        tick_ms: positive_ms(row.tick_s),
        first_tick_ms: positive_ms(row.delay_s).unwrap_or(0),
        stack_max: row
            .stack_max
            .and_then(|max| u8::try_from(max).ok())
            .filter(|max| *max > 0)
            .unwrap_or(DEFAULT_STACK_MAX),
        refresh_duration: row.refresh_duration != Some(false),
        potency_max: row.potency_max.filter(|max| *max > 0.0),
        categories: names(row.categories.as_deref()),
        remove_categories: names(row.remove_categories.as_deref()),
        remove_effects: ids(row.remove_effects.as_deref()),
        blocked_by: names(row.antirequisite_categories.as_deref()),
        health: row.health.unwrap_or(0.0),
        health_share: row.health_share.unwrap_or(0.0),
        hit_share: row.hit_share.unwrap_or(0.0),
        stamina: row.stamina.unwrap_or(0.0),
        damage_type: row.damage_type.as_deref().and_then(type_of),
        remove_on_death: row.remove_on_death,
        remove_on_respawn: row.remove_on_respawn,
        allow_add_if_dead: row.allow_add_if_dead,
        lightweight: row.lightweight,
        client_predicted: row.client_predicted,
        stop_on_hits_taken: (row.hit_condition.as_deref() == Some(HIT_TAKEN_CONDITION))
            .then_some(row.stop_on_hit_count)
            .flatten()
            .and_then(|count| u32::try_from(count).ok())
            .filter(|count| *count > 0),
        on_end: ids(row.on_end.as_deref()),
        on_tick: ids(row.on_tick.as_deref()),
        potency_mods: mods(row.potency_mods.as_deref()),
        duration_mods: mods(row.duration_mods.as_deref()),
        absorption: typed(&row.absorption),
        damage_mods: typed(&row.damage_mods),
        gather_efficiency: row.gather_efficiency.clone(),
        craft_gear_score: row.craft_gear_score.clone(),
    }
}

/// The lowercase names of a `+`-separated cell.
fn names(cell: Option<&str>) -> Vec<String> {
    cell.unwrap_or_default()
        .split(LIST_SEPARATOR)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The CRC-32 of the lowercase ids of a `+`-separated cell.
fn ids(cell: Option<&str>) -> Vec<u32> {
    names(cell).iter().map(|id| lowercase_crc32(id)).collect()
}

/// The (lowercase category, value) pairs of a mod cell (`Root=-0.2+Stun=-0.2`); an entry without
/// a number is left out.
fn mods(cell: Option<&str>) -> Vec<(String, f64)> {
    names(cell)
        .iter()
        .filter_map(|entry| {
            let (category, value) = entry.split_once(MOD_SEPARATOR)?;
            Some((category.trim().to_owned(), value.trim().parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DamageTypeData` `Slash` (newchar retail capture: light sword items tag 4).
    const SLASH: u8 = 4;
    /// `DamageTypeData` `Standard`.
    const STANDARD: u8 = 1;

    fn row(id: &str) -> EffectSourceRow {
        EffectSourceRow {
            status_id: id.to_owned(),
            ..EffectSourceRow::default()
        }
    }

    fn source() -> StatusEffectSource {
        StatusEffectSource {
            effects: vec![
                // The retail `AI_Bleed_9` row.
                EffectSourceRow {
                    base_duration_s: Some(10.0),
                    tick_s: Some(2.0),
                    stack_max: Some(1),
                    categories: Some("Debuff+Bleed+Dot".to_owned()),
                    health_share: Some(-0.015),
                    hit_share: Some(-0.05),
                    damage_type: Some("Standard".to_owned()),
                    remove_on_death: true,
                    ..row("AI_Bleed_9")
                },
                // The retail `EquipLoad_CCDurReduction_Heavy` row.
                EffectSourceRow {
                    base_duration_s: Some(-1.0),
                    duration_mods: Some("Root=-0.2+Stun=-0.2+Slow=-0.2".to_owned()),
                    lightweight: true,
                    client_predicted: true,
                    ..row("EquipLoad_CCDurReduction_Heavy")
                },
                // The retail `Status_OutOfCombat_PassiveRegen` row.
                EffectSourceRow {
                    base_duration_s: Some(180.0),
                    delay_s: Some(1.0),
                    tick_s: Some(1.0),
                    health: Some(40.0),
                    potency_mods: Some("PassiveFoodRegen=2".to_owned()),
                    stop_on_hit_count: Some(1),
                    hit_condition: Some("OnHitTaken".to_owned()),
                    remove_on_respawn: true,
                    absorption: vec![("Slash".to_owned(), 0.5)],
                    ..row("Status_OutOfCombat_PassiveRegen")
                },
                // A repeated id, as retail `Sanctuary` in two tables.
                row("Sanctuary"),
                EffectSourceRow {
                    stack_max: Some(3),
                    ..row("Sanctuary")
                },
            ],
            damage_types: vec![
                ("Slash".to_owned(), SLASH),
                ("Standard".to_owned(), STANDARD),
            ],
            hit_effects: vec![HitEffectRow {
                table: "WolfDamageTable".to_owned(),
                damage_id: "Bite".to_owned(),
                status_effects: "AI_Bleed_9, AI_Bleed_9".to_owned(),
            }],
        }
    }

    #[test]
    fn a_key_is_the_first_place_of_the_crc_among_every_row() {
        let rules = StatusEffectRules::build(source());
        let mut crcs: Vec<u32> = source()
            .effects
            .iter()
            .map(|row| lowercase_crc32(&row.status_id))
            .collect();
        crcs.sort_unstable();
        for id in ["AI_Bleed_9", "Sanctuary", "Status_OutOfCombat_PassiveRegen"] {
            let rule = rules.by_id(id).expect("rule");
            let first = crcs
                .iter()
                .position(|crc| *crc == rule.crc)
                .expect("listed");
            assert_eq!(usize::from(rule.key), first, "{id}");
        }
        assert_eq!(rules.len(), 4, "a repeated id is one effect");
        assert_eq!(
            rules.by_id("sanctuary").map(|rule| rule.stack_max),
            Some(DEFAULT_STACK_MAX),
            "the first row of a repeated id"
        );
    }

    #[test]
    fn the_rows_give_durations_ticks_mods_and_hit_effects() {
        let rules = StatusEffectRules::build(source());
        let bleed = rules.by_id("ai_bleed_9").expect("bleed");
        assert_eq!(bleed.duration_ms, Some(10_000));
        assert_eq!(bleed.tick_ms, Some(2_000));
        assert_eq!(bleed.damage_type, Some(STANDARD));
        assert!(bleed.has_category("dot") && bleed.changes_vitals());
        assert!(bleed.refresh_duration, "an empty RefreshDuration refreshes");
        let load = rules.by_id("EquipLoad_CCDurReduction_Heavy").expect("load");
        assert_eq!(load.duration_ms, None);
        assert!(load.lightweight && load.client_predicted);
        assert_eq!(
            load.duration_mods,
            vec![
                ("root".to_owned(), -0.2),
                ("stun".to_owned(), -0.2),
                ("slow".to_owned(), -0.2)
            ]
        );
        let regen = rules
            .by_id("Status_OutOfCombat_PassiveRegen")
            .expect("regen");
        assert_eq!(regen.first_tick_ms, 1_000);
        assert_eq!(regen.stop_on_hits_taken, Some(1));
        assert_eq!(
            regen.potency_mods,
            vec![("passivefoodregen".to_owned(), 2.0)]
        );
        assert_eq!(regen.absorption, vec![(SLASH, 0.5)]);
        assert_eq!(
            rules.on_hit(RowKey::of("wolfdamagetable", "BITE")),
            [bleed.crc, bleed.crc].as_slice(),
            "each repeat is one stack"
        );
        assert!(
            rules
                .on_hit(RowKey::of("WolfDamageTable", "Claw"))
                .is_empty()
        );
    }
}
