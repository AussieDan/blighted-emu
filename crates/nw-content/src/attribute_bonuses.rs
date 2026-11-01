// SPDX-License-Identifier: AGPL-3.0-or-later
//! The abilities an attribute value turns on (abilities design note, 2026-10-31): each
//! `AttributeDefinition` row names, in `EquipAbilities`, the `AttributeThresholdAbilityTable`
//! abilities a value of its `Level` or more holds (Focus 100: `Foc_Bonus_100_1,Foc_Bonus_100_2`).
//! Their `AbilityData` cells change the mana and the cooldowns: `MaxMana` (Focus 100: 0.1),
//! `ManaRate` (Focus 100: 0.2) and `CooldownTimer` (Focus 25: -0.03), each a share added to 1.
//! **Observed** (retail `general-other-01`, Focus 250): a mana max of 110, the 100 of
//! `ManaBaseMax` and the 0.1 of `Foc_Bonus_100_1`.

use std::collections::HashMap;

use crate::Tables;
use crate::character_points::ATTRIBUTE_NAMES;

/// Separates the abilities of an `EquipAbilities` cell.
const ABILITY_SEPARATOR: char = ',';

/// What the held abilities add to the mana and the cooldowns, as shares of the base.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AbilityStats {
    /// `MaxMana`.
    pub max_mana: f64,
    /// `ManaRate`.
    pub mana_rate: f64,
    /// `CooldownTimer` (below 0: shorter cooldowns).
    pub cooldown: f64,
}

impl AbilityStats {
    fn plus(self, other: Self) -> Self {
        Self {
            max_mana: self.max_mana + other.max_mana,
            mana_rate: self.mana_rate + other.mana_rate,
            cooldown: self.cooldown + other.cooldown,
        }
    }
}

/// The threshold abilities of every attribute and their stats.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttributeBonuses {
    /// Attribute id, the `Level` from which they hold, and the abilities.
    thresholds: Vec<(u32, u32, Vec<String>)>,
    /// The stats of the abilities, by lowercase `AbilityID`.
    stats: HashMap<String, AbilityStats>,
}

impl AttributeBonuses {
    /// The bonuses of `tables`: the `EquipAbilities` of each attribute table's rows, and the
    /// `MaxMana`, `ManaRate` and `CooldownTimer` of every `AbilityData` row.
    pub fn from_tables(tables: &Tables) -> Self {
        let thresholds = tables.attribute_definition.iter().flat_map(|table| {
            let attribute = ATTRIBUTE_NAMES
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&table.unique_id));
            table.rows.iter().filter_map(move |row| {
                let attribute = u32::try_from(attribute?).ok()?;
                Some((
                    attribute,
                    u32::try_from(row.level).ok()?,
                    row.equip_abilities.as_deref()?.to_owned(),
                ))
            })
        });
        let stats = tables
            .ability_data
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| {
                (
                    row.ability_id.clone(),
                    AbilityStats {
                        max_mana: row.max_mana.unwrap_or_default(),
                        mana_rate: row.mana_rate.unwrap_or_default(),
                        cooldown: row.cooldown_timer.unwrap_or_default(),
                    },
                )
            })
            .filter(|(_, stats)| *stats != AbilityStats::default());
        Self::build(thresholds, stats)
    }

    /// The bonuses of the `thresholds` (attribute id, level, `EquipAbilities` cell) and the
    /// ability `stats` (id, stats).
    pub fn build(
        thresholds: impl IntoIterator<Item = (u32, u32, String)>,
        stats: impl IntoIterator<Item = (String, AbilityStats)>,
    ) -> Self {
        Self {
            thresholds: thresholds
                .into_iter()
                .map(|(attribute, level, cell)| {
                    let abilities = cell
                        .split(ABILITY_SEPARATOR)
                        .map(str::trim)
                        .filter(|ability| !ability.is_empty())
                        .map(str::to_owned)
                        .collect();
                    (attribute, level, abilities)
                })
                .collect(),
            stats: stats
                .into_iter()
                .map(|(id, stats)| (id.to_lowercase(), stats))
                .collect(),
        }
    }

    /// The abilities that the attribute values `value_of` (attribute id to its value) hold.
    pub fn abilities(&self, value_of: impl Fn(u32) -> u32) -> Vec<&str> {
        self.thresholds
            .iter()
            .filter(|(attribute, level, _)| value_of(*attribute) >= *level)
            .flat_map(|(_, _, abilities)| abilities.iter().map(String::as_str))
            .collect()
    }

    /// The summed stats of `abilities` (any case; one without stats adds nothing).
    pub fn stats_of<'a>(&self, abilities: impl IntoIterator<Item = &'a str>) -> AbilityStats {
        abilities
            .into_iter()
            .filter_map(|ability| self.stats.get(&ability.to_lowercase()))
            .fold(AbilityStats::default(), |sum, stats| sum.plus(*stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOCUS: u32 = 1;
    const STRENGTH: u32 = 2;

    /// The Focus rows 25 and 100 and their abilities (build 25810808).
    fn bonuses() -> AttributeBonuses {
        AttributeBonuses::build(
            [
                (FOCUS, 25, "Foc_Bonus_25_1,Foc_Bonus_25_2".to_owned()),
                (FOCUS, 100, "Foc_Bonus_100_1, Foc_Bonus_100_2".to_owned()),
                (STRENGTH, 25, "Str_Bonus_25_1".to_owned()),
            ],
            [
                (
                    "Foc_Bonus_25_1".to_owned(),
                    AbilityStats {
                        cooldown: -0.03,
                        ..AbilityStats::default()
                    },
                ),
                (
                    "Foc_Bonus_100_1".to_owned(),
                    AbilityStats {
                        max_mana: 0.1,
                        mana_rate: 0.2,
                        ..AbilityStats::default()
                    },
                ),
            ],
        )
    }

    #[test]
    fn an_attribute_value_holds_the_abilities_of_its_rows_and_their_stats_add_up() {
        let bonuses = bonuses();
        let focus = |value: u32| move |attribute: u32| if attribute == FOCUS { value } else { 5 };
        assert!(bonuses.abilities(focus(24)).is_empty());
        assert_eq!(
            bonuses.abilities(focus(250)),
            [
                "Foc_Bonus_25_1",
                "Foc_Bonus_25_2",
                "Foc_Bonus_100_1",
                "Foc_Bonus_100_2"
            ]
        );
        let stats = bonuses.stats_of(bonuses.abilities(focus(250)));
        assert!((stats.max_mana - 0.1).abs() < 1e-9);
        assert!((stats.mana_rate - 0.2).abs() < 1e-9);
        assert!((stats.cooldown + 0.03).abs() < 1e-9);
        assert_eq!(
            bonuses.stats_of(["foc_bonus_25_2"]),
            AbilityStats::default()
        );
    }
}
