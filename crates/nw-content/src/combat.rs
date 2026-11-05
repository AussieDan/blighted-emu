// SPDX-License-Identifier: AGPL-3.0-or-later
//! The combat rows a `sim` keeps (M6-A spec section 3). [`CombatSource`] is the plain content of
//! the datasheets that combat reads; [`CombatData`] is what a `sim` looks up during a tick.
//!
//! A mob type is found by the crc32 of its lower-case vitals id, as the world file stores it: in
//! `VitalsBaseData` first, then `VitalsData` (no mods: 1.0), then `VitalsLevelVariantData`, whose
//! row names the base type it varies. A type's mods are its own times the category mods of its
//! creature type (`VitalsModifierData`, for example `Solo+`: health 2, damage 1.2).

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;
use thiserror::Error;

use crate::Tables;
use crate::gameplay_config::ITEM_CLASS_SEPARATOR;
use crate::player_abilities::{AbilityList, PlayerAbilities};
use crate::player_attacks::{PlayerAttacks, Stance, stance_key};
use crate::player_sequences::PlayerSequences;

/// Separates the status effects of an `AddStatusEffects` cell.
const EFFECT_SEPARATOR: char = '+';
/// Health and damage mod of a type without them.
const PLAIN_MOD: f64 = 1.0;

/// The player row of `VitalsBaseData` (M6-A spec section 9).
pub const DEFAULT_PLAYER_VITALS_ID: &str = "Player";
/// The player sword of `WeaponItemDefinitions`.
pub const DEFAULT_WEAPON_ID: &str = "SwordT1";
/// The `RangedAttackProfile` of a weapon that fires spread shots.
const SPREAD_SHOT_PROFILE: &str = "Blunderbuss";
/// The player's damage table: the unique id of the datasheet that the player slice names
/// (`slices/player.dynamicslice` refers to `datatables/javelindata_damagetable`, whose unique id
/// is `DamageTable`; build 25810808). A damage row id is unique within one table only.
pub const DEFAULT_PLAYER_DAMAGE_TABLE: &str = "DamageTable";
/// The separator of a damage row's `Affixes` cell.
const AFFIX_SEPARATOR: char = ',';
/// A block stability is a percentage.
const PERCENT: f64 = 100.0;
/// The `BalanceCategory` of the open-world PvP balance rows that adjust a weapon class
/// (`javelindata_pvpbalance_openworld`).
const PVP_BALANCE_ITEM_CLASS: &str = "ItemClass";

/// The config rows a `sim` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombatRows {
    /// The player row of `VitalsBaseData`.
    pub player_vitals_id: String,
    /// The player weapon row of `WeaponItemDefinitions`.
    pub weapon_id: String,
    /// The unique id of the player's damage table, where its attack rows are.
    pub damage_table: String,
}

impl Default for CombatRows {
    fn default() -> Self {
        Self {
            player_vitals_id: DEFAULT_PLAYER_VITALS_ID.to_owned(),
            weapon_id: DEFAULT_WEAPON_ID.to_owned(),
            damage_table: DEFAULT_PLAYER_DAMAGE_TABLE.to_owned(),
        }
    }
}

/// The `VitalsModifierData` mods of one creature type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CategoryMods {
    /// Health multiplier.
    pub health: f64,
    /// Damage multiplier.
    pub damage: f64,
}

/// A mob type.
#[derive(Debug, Clone, PartialEq)]
pub struct MobType {
    /// Multiplies the level's base health (`HealthMod`).
    pub health_mod: f64,
    /// Multiplies that health again: the creature type's `CategoryHealthMod` (each product
    /// loses its fraction, as on the retail cards).
    pub category_health_mod: f64,
    /// Multiplies the level's base damage (with the creature type's category mod).
    pub damage_mod: f64,
    /// The creature type, for the vision offset.
    pub creature_type: Option<String>,
}

impl MobType {
    /// A type without mods.
    pub const PLAIN: Self = Self {
        health_mod: PLAIN_MOD,
        category_health_mod: PLAIN_MOD,
        damage_mod: PLAIN_MOD,
        creature_type: None,
    };
}

/// One level of `VitalsLevelData`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelRow {
    /// Base max health.
    pub base_max_health: f64,
    /// Base damage of one hit.
    pub base_damage: f64,
    /// Physical armor rating of a mob of this level.
    pub physical_armor_rating: f64,
    /// Elemental armor rating of a mob of this level.
    pub elemental_armor_rating: f64,
    /// `GearScore`: the gear score of a mob of this level, against which a player's armor
    /// mitigates its hits.
    pub gear_score: u32,
}

/// The armor of an armor item (`ArmorItemDefinitions`): its share of a full set's rating and the
/// scale of each kind (the set's physical and elemental scale factors).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArmorRow {
    /// `ArmorRatingScaleFactor`: the item's share of a full set (the five heavy starter pieces:
    /// head 0.15, chest 0.35, hands 0.15, legs 0.2, feet 0.15).
    pub rating_share: f64,
    /// `PhysicalArmorSetScaleFactor`.
    pub physical_scale: f64,
    /// `ElementalArmorSetScaleFactor`.
    pub elemental_scale: f64,
}

/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;

/// The player weapon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponRow {
    /// Base damage.
    pub base_damage: f64,
    /// Crit chance, 0 to 1.
    pub crit_chance: f64,
    /// Crit damage multiplier.
    pub crit_multiplier: f64,
    /// It fires spread shots (a blunderbuss): its attacks come as 2719 requests.
    pub spread_shot: bool,
    /// The shots it holds when loaded (`MaxLoadedAmmo`; the blunderbuss 2, melee weapons 0).
    pub max_loaded_ammo: u8,
    /// How long it takes to load again (`AutoReloadAmmoSeconds`; the blunderbuss 1.46 s).
    pub reload_ms: u64,
    /// The key of its stance: its `MannequinTag` ([`crate::player_attacks::stance_key`]).
    pub stance: u32,
    /// `BlockStaminaDamage`: the stamina its hits take from a blocker, before the attack's mod.
    pub block_stamina_damage: f64,
    /// `BlockStability`: the share (percent) of a blocked hit's stamina it takes off.
    pub block_stability: f64,
    /// How it blocks a projectile; `None` without `CanBlockRanged`: a projectile passes its
    /// block.
    pub ranged_block: Option<RangedBlock>,
}

/// How a weapon with `CanBlockRanged` blocks a projectile (build 25810808: shields 0 and 1,
/// swords 0.5 and 0.5, great axes 0.3 and 0.7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangedBlock {
    /// `RangedBlockHealthDamageScaling`: the share of the hit's health that passes the block
    /// (Inferred: 0 without the cell, as on the shields that have one).
    pub health_scale: f64,
    /// `RangedBlockStaminaDamageScaling`: the share of the block's stamina the hit takes
    /// (Inferred: 1 without the cell).
    pub stamina_scale: f64,
}

/// The health share of a ranged block without `RangedBlockHealthDamageScaling`.
const RANGED_BLOCK_HEALTH: f64 = 0.0;
/// The stamina share of a ranged block without `RangedBlockStaminaDamageScaling`.
const RANGED_BLOCK_STAMINA: f64 = 1.0;

/// A consumable that heals: a potion.
#[derive(Debug, Clone, PartialEq)]
pub struct HealingConsumable {
    /// `ConsumableItemDefinitions.CooldownDuration`, in seconds.
    pub cooldown_s: u32,
    /// `CooldownId`: consumables of one id share the cooldown.
    pub cooldown_id: String,
    /// The heal of its status effect (`StatusEffectData.Health`).
    pub health: f64,
    /// Its share of max health (`HealthModifierPercent`).
    pub health_share: f64,
    /// Its `MannequinTag`: the item use it plays (`Potion`); empty for none.
    pub use_tag: String,
}

/// One row of `ConsumableItemDefinitions` that has a cooldown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumableRow {
    /// `ConsumableID`.
    pub id: String,
    /// `CooldownDuration`, seconds.
    pub cooldown_s: u32,
    /// `CooldownId`.
    pub cooldown_id: String,
    /// `AddStatusEffects`.
    pub status_effects: String,
    /// `MannequinTag`; empty for none.
    pub use_tag: String,
}

/// The extra damage of an attack's affix (`AffixStatData`): its share of the hit and the
/// `DamageTypeData` `IntID` of its type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffixDamage {
    /// `DamagePercentage`, 0 to 1.
    pub share: f64,
    /// The `IntID` of its `DamageType`.
    pub damage_type: u8,
}

/// The `Category` of a `DamageTypeData` row: which armor takes a hit of the type off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DamageCategory {
    /// `True` (`True`, `Falling`, `Brimstone`): no armor.
    True,
    /// `Physical` (`Standard`, `Slash`, `Thrust`, `Strike`, `PhysFire`, `Siege`).
    #[default]
    Physical,
    /// `Elemental` (`Arcane`, `Fire`, `Lightning`, `Corruption`, `Ice`, `Nature`, `Acid`).
    Elemental,
}

impl DamageCategory {
    /// The category of a `Category` cell.
    fn of(cell: &str) -> Self {
        match cell {
            "True" => Self::True,
            "Elemental" => Self::Elemental,
            _ => Self::Physical,
        }
    }
}

/// One attack of `DamageData`.
#[derive(Debug, Clone, PartialEq)]
pub struct AttackRow {
    /// Its row id.
    pub damage_id: String,
    /// Damage coefficient.
    pub coefficient: f64,
    /// Whether it can crit (an empty cell allows it).
    pub can_crit: bool,
    /// Threat it adds besides its damage.
    pub threat: u64,
    /// The `IntID` of its `DamageType` in `DamageTypeData`, when the row names a known one.
    pub damage_type: Option<u8>,
    /// The unique id of its table.
    pub table: String,
    /// `BlockStaminaDmgMod`: multiplies the attacker weapon's `BlockStaminaDamage` when a blocker
    /// takes the hit; 0 without a number.
    pub block_stamina_mod: f64,
    /// Whether a block stops it: not `Unblockable` and not `CanBeBlocked` false.
    pub blockable: bool,
    /// The `Category` of its damage type (physical when the row names none).
    pub category: DamageCategory,
    /// The extra damage of its first affix with one (`Affixes`, `AffixStatData`).
    pub affix: Option<AffixDamage>,
}

impl AttackRow {
    /// The stamina it takes from a blocker of `block_stability` (percent) when the attacker's
    /// weapon is `weapon` (retail: 14.6 and 25.55 from the legionnaire's and Thorpe's stabs on a
    /// warhammer of stability 27, `100 x 0.2 x 0.73` and `125 x 0.28 x 0.73`).
    pub fn block_stamina(&self, weapon: &WeaponRow, block_stability: f64) -> f64 {
        weapon.block_stamina_damage * self.block_stamina_mod * (1.0 - block_stability / PERCENT)
    }
}

/// The plain content of the combat datasheets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CombatSource {
    /// `VitalsBaseData` rows by id.
    pub base_types: Vec<(String, MobType)>,
    /// `VitalsData` ids.
    pub plain_types: Vec<String>,
    /// `VitalsLevelVariantData`: (id, base id).
    pub variants: Vec<(String, String)>,
    /// `VitalsLevelData` by level.
    pub levels: Vec<(u32, LevelRow)>,
    /// `WeaponItemDefinitions` by id.
    pub weapons: Vec<(String, WeaponRow)>,
    /// `WeaponItemDefinitions`: (id, `RangedAttackProfile`) of the weapons that have a profile.
    pub ranged_profiles: Vec<(String, String)>,
    /// `MasterItemDefinitions`: (item id, `ItemStatsRef`) of the items that have stats.
    pub item_stats: Vec<(String, String)>,
    /// `ArmorItemDefinitions` by id.
    pub armors: Vec<(String, ArmorRow)>,
    /// `ConsumableItemDefinitions`: the consumables with a cooldown.
    pub consumables: Vec<ConsumableRow>,
    /// `StatusEffectData`: (status id, health, health share) of the effects that heal.
    pub heals: Vec<(String, f64, f64)>,
    /// `DamageData` rows.
    pub attacks: Vec<AttackRow>,
    /// `AITargetingData`: out-of-combat vision offset by creature type.
    pub vision_offsets: Vec<(String, f32)>,
    /// `VitalsBaseData` initial health by id (rows whose cell is a whole number).
    pub player_health: Vec<(String, u32)>,
    /// `VitalsModifierData` by category (creature type).
    pub categories: Vec<(String, CategoryMods)>,
    /// `MasterItemDefinitions`: (item id, `ItemClass` cell) of the items that have classes.
    pub item_classes: Vec<(String, String)>,
    /// The open-world PvP balance: (item class, `WeaponBaseDamageAdjustment`) of each weapon
    /// class row.
    pub pvp_weapon_adjustments: Vec<(String, f64)>,
}

impl CombatSource {
    /// The combat content of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let base_rows = || tables.vitals_base_data.iter().flat_map(|table| &table.rows);
        let damage_types: HashMap<&str, u8> = tables
            .damage_type_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| Some((row.type_id.as_str(), u8::try_from(row.int_id).ok()?)))
            .collect();
        let categories: HashMap<&str, DamageCategory> = tables
            .damage_type_data
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| (row.type_id.as_str(), DamageCategory::of(&row.category)))
            .collect();
        let affixes: HashMap<&str, AffixDamage> = tables
            .affix_stat_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                Some((
                    row.status_id.as_str(),
                    AffixDamage {
                        share: row.damage_percentage?,
                        damage_type: *damage_types.get(row.damage_type.as_deref()?)?,
                    },
                ))
            })
            .collect();
        Self {
            base_types: base_rows()
                .map(|row| {
                    (
                        row.vitals_id.clone(),
                        MobType {
                            health_mod: row.health_mod.unwrap_or(PLAIN_MOD),
                            category_health_mod: PLAIN_MOD,
                            damage_mod: row.damage_mod.unwrap_or(PLAIN_MOD),
                            creature_type: row.creature_type.clone(),
                        },
                    )
                })
                .collect(),
            plain_types: tables
                .vitals_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| row.vitals_id.clone())
                .collect(),
            variants: tables
                .vitals_level_variant_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| (row.vitals_id.clone(), row.base_vitals_id.clone()))
                .collect(),
            levels: tables
                .vitals_level_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let level = u32::try_from(row.level).ok()?;
                    Some((
                        level,
                        LevelRow {
                            base_max_health: row.base_max_health,
                            base_damage: row.base_damage,
                            physical_armor_rating: row.physical_armor_rating,
                            elemental_armor_rating: row.elemental_armor_rating,
                            gear_score: u32::try_from(row.gear_score).unwrap_or(0),
                        },
                    ))
                })
                .collect(),
            weapons: tables
                .weapon_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    (
                        row.weapon_id.clone(),
                        WeaponRow {
                            base_damage: row.base_damage.map_or(0.0, |damage| damage as f64),
                            crit_chance: row.crit_chance.unwrap_or(0.0),
                            crit_multiplier: row.crit_damage_multiplier.unwrap_or(PLAIN_MOD),
                            spread_shot: row.ranged_attack_profile.as_deref()
                                == Some(SPREAD_SHOT_PROFILE),
                            max_loaded_ammo: row
                                .max_loaded_ammo
                                .and_then(|ammo| u8::try_from(ammo).ok())
                                .unwrap_or(0),
                            // A reload of a few seconds.
                            reload_ms: row
                                .auto_reload_ammo_seconds
                                .map_or(0, |seconds| (seconds * MS_PER_SECOND).round() as u64),
                            stance: stance_key(row.mannequin_tag.as_deref().unwrap_or_default()),
                            block_stamina_damage: row
                                .block_stamina_damage
                                .map_or(0.0, |damage| damage as f64),
                            block_stability: row
                                .block_stability
                                .map_or(0.0, |stability| stability as f64),
                            ranged_block: row.can_block_ranged.unwrap_or_default().then(|| {
                                RangedBlock {
                                    health_scale: row
                                        .ranged_block_health_damage_scaling
                                        .unwrap_or(RANGED_BLOCK_HEALTH),
                                    stamina_scale: row
                                        .ranged_block_stamina_damage_scaling
                                        .unwrap_or(RANGED_BLOCK_STAMINA),
                                }
                            }),
                        },
                    )
                })
                .collect(),
            ranged_profiles: tables
                .weapon_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| Some((row.weapon_id.clone(), row.ranged_attack_profile.clone()?)))
                .collect(),
            consumables: tables
                .consumable_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    Some(ConsumableRow {
                        id: row.consumable_id.clone(),
                        cooldown_s: u32::try_from(row.cooldown_duration?).ok()?,
                        cooldown_id: row.cooldown_id.clone()?,
                        status_effects: row.add_status_effects.clone()?,
                        use_tag: row.mannequin_tag.clone().unwrap_or_default(),
                    })
                })
                .collect(),
            heals: tables
                .status_effect_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let health = row.health?;
                    (health > 0).then(|| {
                        (
                            row.status_id.clone(),
                            health as f64,
                            row.health_modifier_percent.unwrap_or(0.0),
                        )
                    })
                })
                .collect(),
            armors: tables
                .armor_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    (
                        row.weapon_id.clone(),
                        ArmorRow {
                            rating_share: row.armor_rating_scale_factor.unwrap_or(0.0),
                            physical_scale: row.physical_armor_set_scale_factor.unwrap_or(0.0),
                            elemental_scale: row.elemental_armor_set_scale_factor.unwrap_or(0.0),
                        },
                    )
                })
                .collect(),
            item_stats: tables
                .master_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| Some((row.item_id.clone(), row.item_stats_ref.clone()?)))
                .collect(),
            attacks: tables
                .damage_data
                .iter()
                .flat_map(|table| table.rows.iter().map(move |row| (table, row)))
                .map(|(table, row)| AttackRow {
                    damage_id: row.damage_id.clone(),
                    coefficient: row.dmg_coef.unwrap_or(PLAIN_MOD),
                    can_crit: row.can_crit.unwrap_or(true),
                    threat: row
                        .add_threat
                        .and_then(|threat| u64::try_from(threat).ok())
                        .unwrap_or(0),
                    damage_type: row
                        .damage_type
                        .as_deref()
                        .and_then(|name| damage_types.get(name).copied()),
                    table: table.unique_id.clone(),
                    block_stamina_mod: row
                        .block_stamina_dmg_mod
                        .as_deref()
                        .and_then(|cell| cell.trim().parse().ok())
                        .unwrap_or(0.0),
                    // 145 rows are `Unblockable` (boss wipes, howls, spells) and 5 not
                    // `CanBeBlocked` (the admiral's cannon storm).
                    blockable: row.unblockable != Some(true) && row.can_be_blocked != Some(false),
                    category: row
                        .damage_type
                        .as_deref()
                        .and_then(|name| categories.get(name).copied())
                        .unwrap_or_default(),
                    affix: row.affixes.as_deref().and_then(|cell| {
                        cell.split(AFFIX_SEPARATOR)
                            .find_map(|affix| affixes.get(affix.trim()).copied())
                    }),
                })
                .collect(),
            vision_offsets: tables
                .ai_targeting_data
                .iter()
                .flat_map(|table| &table.rows)
                // A vision offset is a few metres; f32 holds it exactly enough.
                .map(|row| {
                    (
                        row.creature_type.clone(),
                        row.out_of_combat_vision_distance_offset as f32,
                    )
                })
                .collect(),
            categories: tables
                .vitals_modifier_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    (
                        row.category_id.clone(),
                        CategoryMods {
                            health: row.category_health_mod,
                            damage: row.category_damage_mod,
                        },
                    )
                })
                .collect(),
            player_health: base_rows()
                .filter_map(|row| {
                    Some((
                        row.vitals_id.clone(),
                        row.health_initial.trim().parse().ok()?,
                    ))
                })
                .collect(),
            item_classes: tables
                .master_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| Some((row.item_id.clone(), row.item_class.clone()?)))
                .collect(),
            pvp_weapon_adjustments: tables
                .open_world_balance_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter(|row| row.balance_category == PVP_BALANCE_ITEM_CLASS)
                .filter_map(|row| {
                    Some((
                        row.balance_target.clone(),
                        row.weapon_base_damage_adjustment?,
                    ))
                })
                .collect(),
        }
    }
}

/// A config row that the datasheets do not have.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CombatDataError {
    /// No `VitalsBaseData` row with a whole initial health.
    #[error("no player vitals row `{0}`")]
    MissingPlayer(String),
    /// No `WeaponItemDefinitions` row.
    #[error("no weapon row `{0}`")]
    MissingWeapon(String),
}

/// What a `sim` looks up during a tick.
#[derive(Debug, Clone, PartialEq)]
pub struct CombatData {
    mob_types: HashMap<u32, MobType>,
    levels: HashMap<u32, LevelRow>,
    weapon: WeaponRow,
    /// The weapon row of each item that is a weapon, by lower-case item id.
    item_weapons: HashMap<String, WeaponRow>,
    /// The armor row of each item that is armor, by lower-case item id.
    item_armors: HashMap<String, ArmorRow>,
    /// The ranged attack profile of each weapon item that has one, by lower-case item id.
    item_ranged_profiles: HashMap<String, String>,
    /// The healing consumable of each item that is one, by lower-case item id.
    item_heals: HashMap<String, HealingConsumable>,
    /// The open-world PvP damage factor of each weapon item whose class the balance table
    /// adjusts (1 + `WeaponBaseDamageAdjustment`), by lower-case item id.
    item_pvp_damage: HashMap<String, f64>,
    /// Every attack row, by lower-case table unique id and row id.
    table_attacks: HashMap<(String, String), AttackRow>,
    /// The player's damage table (lower-case unique id).
    player_table: String,
    /// The basic attacks of each weapon stance.
    player_attacks: PlayerAttacks,
    /// The ability lists of the player grid.
    player_abilities: PlayerAbilities,
    /// The item classes of each weapon item (`ItemClass`, split), by lower-case item id.
    item_weapon_classes: HashMap<String, Vec<String>>,
    /// The slayer sequences the server starts for each weapon stance.
    player_sequences: PlayerSequences,
    vision_offsets: HashMap<String, f32>,
    player_base_health: u32,
}

impl CombatData {
    /// The combat data of `tables` for the config `rows`.
    ///
    /// # Errors
    ///
    /// Returns [`CombatDataError`] when a config row is missing.
    pub fn from_tables(tables: &Tables, rows: &CombatRows) -> Result<Self, CombatDataError> {
        Self::build(CombatSource::from_tables(tables), rows)
    }

    /// The combat data of `source` for the config `rows`.
    ///
    /// # Errors
    ///
    /// Returns [`CombatDataError`] when a config row is missing.
    pub fn build(source: CombatSource, rows: &CombatRows) -> Result<Self, CombatDataError> {
        let player_base_health = source
            .player_health
            .iter()
            .find(|(id, _)| *id == rows.player_vitals_id)
            .map(|(_, health)| *health)
            .ok_or_else(|| CombatDataError::MissingPlayer(rows.player_vitals_id.clone()))?;
        let weapon = source
            .weapons
            .iter()
            .find(|(id, _)| *id == rows.weapon_id)
            .map(|(_, weapon)| *weapon)
            .ok_or_else(|| CombatDataError::MissingWeapon(rows.weapon_id.clone()))?;
        let table_attacks: HashMap<(String, String), AttackRow> = source
            .attacks
            .iter()
            .map(|row| {
                (
                    (row.table.to_lowercase(), row.damage_id.clone()),
                    row.clone(),
                )
            })
            .collect();
        let player_table = rows.damage_table.to_lowercase();
        let weapons: HashMap<String, WeaponRow> = source
            .weapons
            .iter()
            .map(|(id, weapon)| (id.to_lowercase(), *weapon))
            .collect();
        let pvp_adjustments: HashMap<String, f64> = source
            .pvp_weapon_adjustments
            .iter()
            .map(|(class, adjustment)| (class.to_lowercase(), *adjustment))
            .collect();
        let item_pvp_damage = source
            .item_classes
            .iter()
            .filter_map(|(item, classes)| {
                let adjustment = classes
                    .split(ITEM_CLASS_SEPARATOR)
                    .find_map(|class| pvp_adjustments.get(&class.trim().to_lowercase()))?;
                Some((item.to_lowercase(), PLAIN_MOD + adjustment))
            })
            .collect();
        let item_weapons: HashMap<String, WeaponRow> = source
            .item_stats
            .iter()
            .filter_map(|(item, stats)| {
                Some((item.to_lowercase(), *weapons.get(&stats.to_lowercase())?))
            })
            .collect();
        let armors: HashMap<String, ArmorRow> = source
            .armors
            .iter()
            .map(|(id, armor)| (id.to_lowercase(), *armor))
            .collect();
        let item_armors = source
            .item_stats
            .iter()
            .filter_map(|(item, stats)| {
                Some((item.to_lowercase(), *armors.get(&stats.to_lowercase())?))
            })
            .collect();
        let item_weapon_classes = source
            .item_classes
            .iter()
            .filter(|(item, _)| item_weapons.contains_key(&item.to_lowercase()))
            .map(|(item, classes)| {
                (
                    item.to_lowercase(),
                    classes
                        .split(ITEM_CLASS_SEPARATOR)
                        .map(str::trim)
                        .filter(|class| !class.is_empty())
                        .map(str::to_owned)
                        .collect(),
                )
            })
            .collect();
        let profiles: HashMap<String, &String> = source
            .ranged_profiles
            .iter()
            .map(|(id, profile)| (id.to_lowercase(), profile))
            .collect();
        let item_ranged_profiles = source
            .item_stats
            .iter()
            .filter_map(|(item, stats)| {
                Some((
                    item.to_lowercase(),
                    (*profiles.get(&stats.to_lowercase())?).clone(),
                ))
            })
            .collect();
        let heals: HashMap<String, (f64, f64)> = source
            .heals
            .iter()
            .map(|(id, health, share)| (id.to_lowercase(), (*health, *share)))
            .collect();
        let consumables: HashMap<String, HealingConsumable> = source
            .consumables
            .iter()
            .filter_map(|row| {
                let (health, health_share) = row
                    .status_effects
                    .split(EFFECT_SEPARATOR)
                    .find_map(|effect| heals.get(&effect.trim().to_lowercase()))?;
                Some((
                    row.id.to_lowercase(),
                    HealingConsumable {
                        cooldown_s: row.cooldown_s,
                        cooldown_id: row.cooldown_id.clone(),
                        health: *health,
                        health_share: *health_share,
                        use_tag: row.use_tag.clone(),
                    },
                ))
            })
            .collect();
        // A consumable's id is its item id: every retail health potion has no `ItemStatsRef`
        // (live 2026-09-16: `PotionHealthT1` never healed). An item that names a consumable by
        // its stats is kept too; its own consumable row wins.
        let item_heals = source
            .item_stats
            .iter()
            .filter_map(|(item, stats)| {
                Some((
                    item.to_lowercase(),
                    consumables.get(&stats.to_lowercase())?.clone(),
                ))
            })
            .chain(consumables.clone())
            .collect();
        let categories: HashMap<String, CategoryMods> = source.categories.into_iter().collect();
        let base: HashMap<u32, MobType> = source
            .base_types
            .into_iter()
            .map(|(id, mut kind)| {
                if let Some(mods) = kind
                    .creature_type
                    .as_ref()
                    .and_then(|kind| categories.get(kind))
                {
                    kind.category_health_mod = mods.health;
                    kind.damage_mod *= mods.damage;
                }
                (lowercase_crc32(&id), kind)
            })
            .collect();
        let mut mob_types = base.clone();
        for id in source.plain_types {
            mob_types
                .entry(lowercase_crc32(&id))
                .or_insert(MobType::PLAIN);
        }
        for (id, base_id) in source.variants {
            let kind = base
                .get(&lowercase_crc32(&base_id))
                .cloned()
                .unwrap_or(MobType::PLAIN);
            mob_types.entry(lowercase_crc32(&id)).or_insert(kind);
        }
        Ok(Self {
            mob_types,
            levels: source.levels.into_iter().collect(),
            weapon,
            item_weapons,
            item_armors,
            item_ranged_profiles,
            item_heals,
            item_pvp_damage,
            table_attacks,
            player_table,
            player_attacks: PlayerAttacks::default(),
            player_abilities: PlayerAbilities::default(),
            item_weapon_classes,
            player_sequences: PlayerSequences::default(),
            vision_offsets: source.vision_offsets.into_iter().collect(),
            player_base_health,
        })
    }

    /// The mob type of a vitals id crc32.
    pub fn mob_type(&self, vitals_crc: u32) -> Option<&MobType> {
        self.mob_types.get(&vitals_crc)
    }

    /// The weapon row of item `item_id` (its `ItemStatsRef`); `None` for an item that is no
    /// weapon. Case does not matter.
    pub fn weapon_of_item(&self, item_id: &str) -> Option<&WeaponRow> {
        self.item_weapons.get(&item_id.to_lowercase())
    }

    /// The armor row of item `item_id` (its `ItemStatsRef`); `None` for an item that is no armor.
    /// Case does not matter.
    pub fn armor_of_item(&self, item_id: &str) -> Option<&ArmorRow> {
        self.item_armors.get(&item_id.to_lowercase())
    }

    /// The ranged attack profile (`RangedAttackProfile`) of weapon item `item_id`; `None` for an
    /// item that has none. Case does not matter.
    pub fn ranged_profile_of_item(&self, item_id: &str) -> Option<&str> {
        self.item_ranged_profiles
            .get(&item_id.to_lowercase())
            .map(String::as_str)
    }

    /// The open-world PvP damage factor of weapon item `item_id`: 1 plus its class's
    /// `WeaponBaseDamageAdjustment`, 1 for a class the balance table does not adjust. Case does
    /// not matter.
    pub fn pvp_damage_factor(&self, item_id: &str) -> f64 {
        self.item_pvp_damage
            .get(&item_id.to_lowercase())
            .copied()
            .unwrap_or(PLAIN_MOD)
    }

    /// The healing consumable of item `item_id`; `None` for an item that is none. Case does not
    /// matter.
    pub fn heal_of_item(&self, item_id: &str) -> Option<&HealingConsumable> {
        self.item_heals.get(&item_id.to_lowercase())
    }

    /// The row of `level`.
    pub fn level(&self, level: u32) -> Option<&LevelRow> {
        self.levels.get(&level)
    }

    /// Every level row.
    pub fn levels(&self) -> impl Iterator<Item = (u32, &LevelRow)> {
        self.levels.iter().map(|(level, row)| (*level, row))
    }

    /// The player weapon.
    pub const fn weapon(&self) -> &WeaponRow {
        &self.weapon
    }

    /// The player attack row `damage_id` of the player's damage table.
    pub fn attack(&self, damage_id: &str) -> Option<&AttackRow> {
        self.table_attack(&self.player_table, damage_id)
    }

    /// The attack row `damage_id` of the table of unique id `table` (any case).
    pub fn table_attack(&self, table: &str, damage_id: &str) -> Option<&AttackRow> {
        self.table_attacks
            .get(&(table.to_lowercase(), damage_id.to_owned()))
    }

    /// This data with the basic attacks of the weapon stances, `attacks`.
    #[must_use]
    pub fn with_player_attacks(self, attacks: PlayerAttacks) -> Self {
        Self {
            player_attacks: attacks,
            ..self
        }
    }

    /// This data with the ability lists of the player grid, `abilities`.
    #[must_use]
    pub fn with_player_abilities(self, abilities: PlayerAbilities) -> Self {
        Self {
            player_abilities: abilities,
            ..self
        }
    }

    /// The CAGE input names of the input numbers `on` holds ([`PlayerAbilities::inputs_on`]).
    pub fn ability_inputs_on(&self, on: impl Fn(u32) -> bool) -> Vec<String> {
        self.player_abilities.inputs_on(on)
    }

    /// The ability lists that weapon item `item_id` turns on (any case), in grid order.
    pub fn ability_lists_of_item(&self, item_id: &str) -> Vec<&AbilityList> {
        self.item_weapon_classes
            .get(&item_id.to_lowercase())
            .map(|classes| {
                let classes: Vec<&str> = classes.iter().map(String::as_str).collect();
                self.player_abilities.lists_for(&classes)
            })
            .unwrap_or_default()
    }

    /// The same data with the player sequence table `sequences`.
    #[must_use]
    pub fn with_player_sequences(self, sequences: PlayerSequences) -> Self {
        Self {
            player_sequences: sequences,
            ..self
        }
    }

    /// The player sequence table.
    pub const fn player_sequences(&self) -> &PlayerSequences {
        &self.player_sequences
    }

    /// The stance of key `key` ([`WeaponRow::stance`]).
    pub fn stance(&self, key: u32) -> Option<&Stance> {
        self.player_attacks.stance(key)
    }

    /// The stance of a character with no weapon.
    pub fn unarmed_stance(&self) -> Option<&Stance> {
        self.stance(stance_key(""))
    }

    /// The out-of-combat vision offset of `creature_type`; 0 without a row.
    pub fn vision_offset(&self, creature_type: Option<&str>) -> f32 {
        creature_type
            .and_then(|kind| self.vision_offsets.get(kind))
            .copied()
            .unwrap_or(0.0)
    }

    /// The player's base health: its max at level 0 without Constitution.
    pub const fn player_base_health(&self) -> u32 {
        self.player_base_health
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Table;
    use nw_formats::datasheet::{Column, ColumnKind, Datasheet};

    /// A datasheet of `type_name` with `columns` and `rows`.
    fn sheet(type_name: &str, columns: &[(&str, ColumnKind)], rows: &[&[&str]]) -> Datasheet {
        Datasheet {
            type_name: type_name.to_owned(),
            unique_id: format!("{type_name}_1"),
            columns: columns
                .iter()
                .map(|(name, kind)| Column {
                    name: (*name).to_owned(),
                    kind: *kind,
                })
                .collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|value| (*value).to_owned()).collect())
                .collect(),
        }
    }

    #[test]
    fn an_attack_has_the_int_id_of_its_damage_type() {
        use ColumnKind::{Number, String as Text};
        // Retail `DamageTypeData` (build 25810808): Slash 4, Thrust 5.
        let types = sheet(
            "DamageTypeData",
            &[
                ("TypeID", Text),
                ("IntID", Number),
                ("Category", Text),
                ("DamageBonusAgainst", Text),
                ("DisplayName", Text),
                ("AfflictionFrac", Number),
                ("DamageBonusFrac", Number),
                ("DefenseBonusFrac", Number),
                ("MitigationBonusFrac", Number),
            ],
            &[
                &[
                    "Slash", "4", "Physical", "None", "Slash", "0", "0", "0", "0",
                ],
                &[
                    "Thrust", "5", "Physical", "None", "Thrust", "0", "0", "0", "0",
                ],
            ],
        );
        // Retail rows of `javelindata_damagetable_damned_commander_ftue`: the stab's phase-two row
        // has a quarter fire, its phase-one row none.
        let attacks = sheet(
            "DamageData",
            &[
                ("DamageID", Text),
                ("DamageType", Text),
                ("BlockStaminaDmgMod", Text),
                ("Affixes", Text),
            ],
            &[
                &["1H_Sword_Attack1", "Slash", "0.4", ""],
                &[
                    "1H_Sword_Attack2",
                    "Thrust",
                    "0.28000000000000003",
                    "AI_FireDMG_25",
                ],
                &["Unknown_Type", "Mystery", "", "No_Such_Affix"],
                &["No_Type", "", "x", ""],
            ],
        );
        let affixes = sheet(
            "AffixStatData",
            &[
                ("StatusID", Text),
                ("DamagePercentage", Number),
                ("DamageType", Text),
            ],
            &[&["AI_FireDMG_25", "0.25", "Thrust"]],
        );
        let tables = Tables {
            damage_type_data: vec![Table::fill("types", &types).expect("types")],
            damage_data: vec![Table::fill("attacks", &attacks).expect("attacks")],
            affix_stat_data: vec![Table::fill("affixes", &affixes).expect("affixes")],
            ..Tables::default()
        };
        let source = CombatSource::from_tables(&tables);
        let types: Vec<(&str, Option<u8>)> = source
            .attacks
            .iter()
            .map(|row| (row.damage_id.as_str(), row.damage_type))
            .collect();
        assert_eq!(
            types,
            [
                ("1H_Sword_Attack1", Some(4)),
                ("1H_Sword_Attack2", Some(5)),
                ("Unknown_Type", None),
                ("No_Type", None),
            ]
        );
        let stab = &source.attacks[1];
        assert_eq!(stab.table, "DamageData_1", "the sheet's unique id");
        assert!((stab.block_stamina_mod - 0.28).abs() < 1e-9);
        assert_eq!(
            stab.affix,
            Some(AffixDamage {
                share: 0.25,
                damage_type: 5
            })
        );
        assert_eq!(source.attacks[0].affix, None);
        assert_eq!(source.attacks[2].affix, None, "an affix without a row");
        assert_eq!(source.attacks[3].block_stamina_mod, 0.0, "not a number");
    }

    #[test]
    fn a_blocked_hit_takes_the_retail_stamina() {
        // Retail tutorial, warhammer of `BlockStability` 27: the legionnaire's
        // `Legionnaire_LightStab` (0.2) with `Legion_1HSpear` (100) took 14.6, Thorpe's
        // `Attack_Stab_02` (0.28) with his sword (125) 25.55.
        let warhammer = 27.0;
        let row = |mod_: f64| AttackRow {
            block_stamina_mod: mod_,
            blockable: true,
            category: DamageCategory::Physical,
            ..source().attacks[0].clone()
        };
        let weapon = |damage: f64| WeaponRow {
            block_stamina_damage: damage,
            ..sword_row()
        };
        assert!((row(0.2).block_stamina(&weapon(100.0), warhammer) - 14.6).abs() < 1e-9);
        assert!((row(0.28).block_stamina(&weapon(125.0), warhammer) - 25.55).abs() < 1e-9);
    }

    const PLAYER_HEALTH: u32 = 856;
    const LEVEL: u32 = 20;
    const LEVEL_HEALTH: f64 = 1872.2;
    const LEVEL_DAMAGE: f64 = 686.1;
    const LEVEL_ARMOR: f64 = 109.336;
    const SWORD_ITEM: &str = "1hSwordT1";
    /// The swordsman's starter chest and its stats row (build 25810808).
    const CHEST_ITEM: &str = "SwordsmanArchetypeChest";
    const CHEST_STATS: &str = "HeavyChest_Balanced";
    const CHEST: ArmorRow = ArmorRow {
        rating_share: 0.35,
        physical_scale: 0.874_996_5,
        elemental_scale: 0.874_996_5,
    };
    /// A sword's open-world PvP adjustment in the test table.
    const SWORD_PVP_ADJUSTMENT: f64 = -0.1;
    /// The bow's (retail -0.253); no test item is a bow.
    const BOW_PVP_ADJUSTMENT: f64 = -0.253;
    const POTION_COOLDOWN_S: u32 = 25;
    const POTION_HEAL: f64 = 400.0;
    const POTION_SHARE: f64 = 0.1;
    const BOAR_HEALTH_MOD: f64 = 0.5;
    const BOAR_DAMAGE_MOD: f64 = 0.8;
    const SWORD_BASE: f64 = 64.0;
    const LIGHT: &str = "1H_Sword_Attack1";
    const HEAVY: &str = "1H_Sword_Attack2";
    const PELLET: &str = "Blunderbuss_Primary_Attack";
    const PLAYER_TABLE: &str = "DamageTable";
    const VISION_OFFSET: f32 = 5.0;

    fn rows() -> CombatRows {
        CombatRows {
            player_vitals_id: "Player".to_owned(),
            weapon_id: "SwordT1".to_owned(),
            damage_table: PLAYER_TABLE.to_owned(),
        }
    }

    fn boar() -> MobType {
        MobType {
            health_mod: BOAR_HEALTH_MOD,
            category_health_mod: 1.0,
            damage_mod: BOAR_DAMAGE_MOD,
            creature_type: Some("Beast".to_owned()),
        }
    }

    /// The occultist's starter ice gauntlet, its stats row and the row's `MannequinTag` (retail
    /// rows, build 25810808).
    const ICE_GAUNTLET_ITEM: &str = "OccultistArchetype_1hGauntletIce";
    const ICE_GAUNTLET_STATS: &str = "1hElementalGauntlet_IceT2";
    const ICE_GAUNTLET_TAGS: &str = "MagicGauntlet,Ice";
    /// The ice gauntlet's `RangedAttackProfile`.
    const ICE_PROFILE: &str = "IceMagic";

    fn sword_row() -> WeaponRow {
        WeaponRow {
            base_damage: SWORD_BASE,
            crit_chance: 0.03,
            crit_multiplier: 1.3,
            spread_shot: false,
            max_loaded_ammo: 0,
            reload_ms: 0,
            stance: stance_key("1H_Melee,Sword"),
            block_stamina_damage: 0.0,
            block_stability: 0.0,
            ranged_block: None,
        }
    }

    fn source() -> CombatSource {
        CombatSource {
            base_types: vec![("Boar_20".to_owned(), boar())],
            plain_types: vec!["Grunt".to_owned(), "Boar_20".to_owned()],
            variants: vec![("Boar_20_Elite".to_owned(), "Boar_20".to_owned())],
            levels: vec![(
                LEVEL,
                LevelRow {
                    base_max_health: LEVEL_HEALTH,
                    base_damage: LEVEL_DAMAGE,
                    physical_armor_rating: LEVEL_ARMOR,
                    elemental_armor_rating: LEVEL_ARMOR,
                    gear_score: 0,
                },
            )],
            weapons: vec![
                ("SwordT1".to_owned(), sword_row()),
                (
                    ICE_GAUNTLET_STATS.to_owned(),
                    WeaponRow {
                        stance: stance_key(ICE_GAUNTLET_TAGS),
                        ..sword_row()
                    },
                ),
            ],
            ranged_profiles: vec![(ICE_GAUNTLET_STATS.to_owned(), ICE_PROFILE.to_owned())],
            consumables: vec![ConsumableRow {
                id: "PotionHealthT1".to_owned(),
                cooldown_s: POTION_COOLDOWN_S,
                cooldown_id: "SurvivalPotion".to_owned(),
                status_effects: "PotionHeal".to_owned(),
                use_tag: "Potion".to_owned(),
            }],
            heals: vec![("PotionHeal".to_owned(), POTION_HEAL, POTION_SHARE)],
            // As in the retail tables: the potion has no stats row.
            item_classes: vec![(
                SWORD_ITEM.to_owned(),
                "EquippableMainHand+Sword+Melee".to_owned(),
            )],
            pvp_weapon_adjustments: vec![
                ("Sword".to_owned(), SWORD_PVP_ADJUSTMENT),
                ("Bow".to_owned(), BOW_PVP_ADJUSTMENT),
            ],
            item_stats: vec![
                (SWORD_ITEM.to_owned(), "SwordT1".to_owned()),
                ("Shield".to_owned(), "NoWeaponRow".to_owned()),
                (ICE_GAUNTLET_ITEM.to_owned(), ICE_GAUNTLET_STATS.to_owned()),
                (CHEST_ITEM.to_owned(), CHEST_STATS.to_owned()),
            ],
            armors: vec![(CHEST_STATS.to_owned(), CHEST)],
            attacks: [(LIGHT, 1.0, 0), (HEAVY, 1.2, 1), (PELLET, 0.19, 0)]
                .map(|(id, coefficient, threat)| AttackRow {
                    damage_id: id.to_owned(),
                    coefficient,
                    can_crit: true,
                    threat,
                    damage_type: None,
                    table: PLAYER_TABLE.to_owned(),
                    block_stamina_mod: 0.0,
                    blockable: true,
                    category: DamageCategory::Physical,
                    affix: None,
                })
                .to_vec(),
            vision_offsets: vec![("Beast".to_owned(), VISION_OFFSET)],
            player_health: vec![("Player".to_owned(), PLAYER_HEALTH)],
            categories: Vec::new(),
        }
    }

    fn data() -> CombatData {
        CombatData::build(source(), &rows()).expect("data")
    }

    #[test]
    fn a_weapon_item_has_the_row_of_its_stats() {
        let data = data();
        assert_eq!(
            data.weapon_of_item("1HSWORDT1")
                .map(|weapon| weapon.base_damage),
            Some(SWORD_BASE)
        );
        assert_eq!(data.weapon_of_item("Shield"), None, "no weapon row");
        assert_eq!(
            data.heal_of_item("potionhealtht1"),
            Some(&HealingConsumable {
                cooldown_s: POTION_COOLDOWN_S,
                cooldown_id: "SurvivalPotion".to_owned(),
                health: POTION_HEAL,
                health_share: POTION_SHARE,
                use_tag: "Potion".to_owned(),
            })
        );
        assert_eq!(
            data.level(LEVEL).map(|level| level.physical_armor_rating),
            Some(LEVEL_ARMOR)
        );
    }

    #[test]
    fn an_attack_is_found_in_its_own_table() {
        let mut source = source();
        // Retail: `1H_Sword_Attack1` is in the player table (1.0) and in NPC tables (0.15).
        source.attacks.push(AttackRow {
            coefficient: 0.15,
            table: "DamnedDamageTable_Damned_FTUE".to_owned(),
            ..source.attacks[0].clone()
        });
        let data = CombatData::build(source, &rows()).expect("data");
        assert_eq!(data.attack(LIGHT).map(|row| row.coefficient), Some(1.0));
        assert_eq!(
            data.table_attack("damneddamagetable_damned_ftue", LIGHT)
                .map(|row| row.coefficient),
            Some(0.15)
        );
        assert_eq!(data.table_attack("NoTable", LIGHT), None);
    }

    #[test]
    fn a_weapon_finds_its_stance() {
        let stance = Stance {
            tags: "1H_Melee,Sword".to_owned(),
            light: None,
            light_from_ms: 0,
            heavy_from_ms: None,
            heavy: None,
            charged_at_ms: None,
            charged: None,
            dodge_invulnerable_ms: Some([0, 500]),
            sheathe_ms: None,
            uses: Vec::new(),
            combo: Vec::new(),
            variants: Vec::new(),
            aimed: None,
            aimed_charged: None,
        };
        let unarmed = Stance {
            tags: String::new(),
            ..stance.clone()
        };
        let data = data().with_player_attacks(PlayerAttacks::of_stances(vec![stance, unarmed]));
        let sword = data.weapon_of_item(SWORD_ITEM).expect("sword");
        assert_eq!(data.armor_of_item("swordsmanarchetypechest"), Some(&CHEST));
        assert_eq!(data.armor_of_item(SWORD_ITEM), None, "a weapon is no armor");
        assert_eq!(
            data.stance(sword.stance).map(|stance| stance.tags.as_str()),
            Some("1H_Melee,Sword")
        );
        assert_eq!(
            data.unarmed_stance().map(|stance| stance.tags.as_str()),
            Some("")
        );
        // The starter ice gauntlet's stats id names no weapon; its row's tags do.
        assert_eq!(
            data.weapon_of_item(ICE_GAUNTLET_ITEM)
                .map(|weapon| weapon.stance),
            Some(stance_key(ICE_GAUNTLET_TAGS))
        );
    }

    #[test]
    fn mob_types_come_from_vitals_base_first() {
        let data = data();
        assert_eq!(data.mob_type(lowercase_crc32("boar_20")), Some(&boar()));
        assert_eq!(data.mob_type(lowercase_crc32("BOAR_20")), Some(&boar()));
    }

    #[test]
    fn a_creature_type_multiplies_the_mods() {
        let mut source = source();
        source.categories = vec![(
            "Beast".to_owned(),
            CategoryMods {
                health: 2.0,
                damage: 1.5,
            },
        )];
        let data = CombatData::build(source, &rows()).expect("data");
        let boar = data.mob_type(lowercase_crc32("Boar_20")).expect("boar");
        assert_eq!(boar.health_mod, BOAR_HEALTH_MOD);
        assert_eq!(boar.category_health_mod, 2.0);
        assert_eq!(boar.damage_mod, BOAR_DAMAGE_MOD * 1.5);
        let elite = data
            .mob_type(lowercase_crc32("Boar_20_Elite"))
            .expect("variant");
        assert_eq!(elite.category_health_mod, 2.0, "a variant too");
    }

    #[test]
    fn a_type_only_in_vitals_data_gets_mods_of_one() {
        let grunt = data()
            .mob_type(lowercase_crc32("Grunt"))
            .cloned()
            .expect("grunt");
        assert_eq!(grunt, MobType::PLAIN);
    }

    #[test]
    fn a_variant_takes_the_type_of_its_base() {
        assert_eq!(
            data().mob_type(lowercase_crc32("Boar_20_Elite")),
            Some(&boar())
        );
        assert_eq!(data().mob_type(lowercase_crc32("Nobody")), None);
    }

    #[test]
    fn levels_are_read_by_level() {
        let data = data();
        assert_eq!(
            data.level(LEVEL).map(|row| row.base_damage),
            Some(LEVEL_DAMAGE)
        );
        assert_eq!(data.level(LEVEL + 1), None);
    }

    #[test]
    fn missing_config_rows_are_errors() {
        let mut no_player = rows();
        no_player.player_vitals_id = "Ghost".to_owned();
        assert!(matches!(
            CombatData::build(source(), &no_player),
            Err(CombatDataError::MissingPlayer(_))
        ));
        let mut no_weapon = rows();
        no_weapon.weapon_id = "Spoon".to_owned();
        assert!(matches!(
            CombatData::build(source(), &no_weapon),
            Err(CombatDataError::MissingWeapon(_))
        ));
        assert!(matches!(
            CombatData::from_tables(&Tables::default(), &rows()),
            Err(CombatDataError::MissingPlayer(_))
        ));
    }

    #[test]
    fn the_player_health_is_the_initial_of_its_row() {
        assert_eq!(data().player_base_health(), PLAYER_HEALTH);
    }

    #[test]
    fn the_weapon_and_its_attacks_are_kept() {
        let data = data();
        assert_eq!(data.weapon().base_damage, SWORD_BASE);
        assert_eq!(data.attack(HEAVY).map(|row| row.threat), Some(1));
        assert_eq!(data.attack("Other"), None);
    }

    #[test]
    fn an_unknown_creature_type_has_no_vision_offset() {
        let data = data();
        assert_eq!(data.vision_offset(Some("Beast")), VISION_OFFSET);
        assert_eq!(data.vision_offset(Some("Dragon")), 0.0);
        assert_eq!(data.vision_offset(None), 0.0);
    }

    #[test]
    fn a_weapon_item_turns_on_the_ability_lists_of_its_classes() {
        use crate::player_abilities::AbilityList;
        let list = |class: &str| AbilityList {
            weapon_classes: vec![class.to_owned()],
            actions: Vec::new(),
            fragments: Vec::new(),
            cooldowns_ms: std::collections::BTreeMap::new(),
            mana_costs: std::collections::BTreeMap::new(),
        };
        let data = CombatData::build(source(), &rows())
            .expect("data")
            .with_player_abilities(PlayerAbilities {
                lists: vec![list("LifeStaff"), list("Sword")],
                inputs: Vec::new(),
            });
        let lists = data.ability_lists_of_item("1hswordt1");
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].weapon_classes, ["Sword"]);
        assert!(data.ability_lists_of_item("NoSuchItem").is_empty());
    }

    #[test]
    fn a_weapon_class_in_the_pvp_balance_scales_its_pvp_damage() {
        let data = CombatData::build(source(), &rows()).expect("data");
        assert!((data.pvp_damage_factor("1HSWORDT1") - (1.0 + SWORD_PVP_ADJUSTMENT)).abs() < 1e-9);
        assert!((data.pvp_damage_factor(ICE_GAUNTLET_ITEM) - 1.0).abs() < 1e-9);
        assert!((data.pvp_damage_factor("NoSuchItem") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_weapon_item_has_the_ranged_profile_of_its_stats_row() {
        let data = CombatData::build(source(), &rows()).expect("data");
        assert_eq!(
            data.ranged_profile_of_item(&ICE_GAUNTLET_ITEM.to_uppercase()),
            Some(ICE_PROFILE)
        );
        assert_eq!(data.ranged_profile_of_item("1hSwordT1"), None, "no profile");
        assert_eq!(data.ranged_profile_of_item("NoSuchItem"), None);
    }
}
