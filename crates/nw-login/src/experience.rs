// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[experience]` of `config/edge.toml`: this server's experience multipliers. The server writes
//! them into the remote config it gives the client (`javelin.xp-multipliers`,
//! [`crate::remote_config`]), and the sims read them from there, so the game and the server
//! use the same values.
//!
//! The default is normal experience, 1 for every gain. Observed: every retail remote config
//! capture (2,553 copies, 2026-09) has a double experience event (`character` 2, each trade skill
//! and weapon mastery 2); some add 3 for territory standing and Umbral Crystals, others leave
//! those out. Inferred: without an event, retail has no multiplier, and the client stores 1 for a
//! progression without an entry (`FUN_143cad9b0`).

use std::collections::BTreeMap;

use serde::Deserialize;

/// Normal experience: a gain as the game data gives it.
pub const NORMAL_EXPERIENCE: f64 = 1.0;

/// The `CategoricalProgressionId` of Umbral Crystals.
pub const UMBRAL_CRYSTALS_ID: &str = "UmbralCurrency";

/// `[experience.trade_skills]`: setting name and `CategoricalProgressionId`.
pub const TRADE_SKILLS: [(&str, &str); 19] = [
    ("weaponsmithing", "Weaponsmithing"),
    ("armoring", "Armoring"),
    ("engineering", "Engineering"),
    ("jewelcrafting", "Jewelcrafting"),
    ("arcana", "Arcana"),
    ("cooking", "Cooking"),
    ("furnishing", "Furnishing"),
    ("smelting", "Smelting"),
    ("woodworking", "Woodworking"),
    ("leatherworking", "Leatherworking"),
    ("weaving", "Weaving"),
    ("stonecutting", "Stonecutting"),
    ("mining", "Mining"),
    ("logging", "Logging"),
    ("harvesting", "Harvesting"),
    ("skinning", "Skinning"),
    ("fishing", "Fishing"),
    ("musician", "Musician"),
    ("riding", "Riding"),
];

/// `[experience.weapon_masteries]`: setting name and `CategoricalProgressionId`.
pub const WEAPON_MASTERIES: [(&str, &str); 16] = [
    ("sword_and_shield", "SwordAbilityTable"),
    ("rapier", "RapierAbilityTable"),
    ("hatchet", "HatchetAbilityTable"),
    ("flail", "FlailAbilityTable"),
    ("dagger", "DaggerAbilityTable"),
    ("spear", "SpearAbilityTable"),
    ("great_axe", "GreatAxeAbilityTable"),
    ("war_hammer", "WarHammerAbilityTable"),
    ("greatsword", "GreatSwordAbilityTable"),
    ("bow", "BowAbilityTable"),
    ("musket", "MusketAbilityTable"),
    ("blunderbuss", "BlunderbussAbilityTable"),
    ("fire_staff", "FireMagicAbilityTable"),
    ("life_staff", "LifeMagicAbilityTable"),
    ("ice_gauntlet", "IceMagicAbilityTable"),
    ("void_gauntlet", "VoidGauntletAbilityTable"),
];

/// `[experience.territory_standing]`: setting name and `CategoricalProgressionId` (the territory
/// rows of `javelindata_categoricalprogression`; their English names).
pub const TERRITORY_STANDING: [(&str, &str); 14] = [
    ("brightwood", "2"),
    ("everfall", "4"),
    ("reekwater", "5"),
    ("windsward", "6"),
    ("ebonscale_reach", "8"),
    ("elysian_wilds", "9"),
    ("cutlass_keys", "10"),
    ("mourningdale", "11"),
    ("monarchs_bluffs", "12"),
    ("weavers_fen", "13"),
    ("edengrove", "14"),
    ("restless_shore", "15"),
    ("brimstone_sands", "16"),
    ("nighthaven", "17"),
];

/// `[experience]`. A setting left out is [`NORMAL_EXPERIENCE`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceConfig {
    /// Character experience: kills, quests and every other gain of character levels.
    #[serde(default = "normal_experience")]
    pub character: f64,
    /// Umbral Crystals.
    #[serde(default = "normal_experience")]
    pub umbral_crystals: f64,
    /// By the names of [`TRADE_SKILLS`].
    #[serde(default)]
    pub trade_skills: BTreeMap<String, f64>,
    /// By the names of [`WEAPON_MASTERIES`].
    #[serde(default)]
    pub weapon_masteries: BTreeMap<String, f64>,
    /// By the names of [`TERRITORY_STANDING`].
    #[serde(default)]
    pub territory_standing: BTreeMap<String, f64>,
}

const fn normal_experience() -> f64 {
    NORMAL_EXPERIENCE
}

impl Default for ExperienceConfig {
    fn default() -> Self {
        Self {
            character: NORMAL_EXPERIENCE,
            umbral_crystals: NORMAL_EXPERIENCE,
            trade_skills: BTreeMap::new(),
            weapon_masteries: BTreeMap::new(),
            territory_standing: BTreeMap::new(),
        }
    }
}

/// A setting of `[experience]` that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("experience.{setting}: {reason}")]
pub struct ExperienceError {
    /// The setting, for example `trade_skills.riding`.
    pub setting: String,
    /// What is wrong.
    pub reason: &'static str,
}

/// Setting names and the `CategoricalProgressionId` each one sets.
type NamedProgressions = &'static [(&'static str, &'static str)];

impl ExperienceConfig {
    /// The groups of named settings: the table name, its names and its values.
    fn groups(&self) -> [(&'static str, NamedProgressions, &BTreeMap<String, f64>); 3] {
        [
            ("trade_skills", &TRADE_SKILLS, &self.trade_skills),
            (
                "weapon_masteries",
                &WEAPON_MASTERIES,
                &self.weapon_masteries,
            ),
            (
                "territory_standing",
                &TERRITORY_STANDING,
                &self.territory_standing,
            ),
        ]
    }

    /// Every value is a finite number of 0 or more, and every name is one the table knows.
    pub fn validate(&self) -> Result<(), ExperienceError> {
        let check = |setting: String, value: f64| {
            if value.is_finite() && value >= 0.0 {
                Ok(())
            } else {
                Err(ExperienceError {
                    setting,
                    reason: "must be a number of 0 or more",
                })
            }
        };
        check("character".to_owned(), self.character)?;
        check("umbral_crystals".to_owned(), self.umbral_crystals)?;
        for (table, names, values) in self.groups() {
            for (name, value) in values {
                let setting = format!("{table}.{name}");
                if !names.iter().any(|(known, _)| known == name) {
                    return Err(ExperienceError {
                        setting,
                        reason: "is no setting of this table",
                    });
                }
                check(setting, *value)?;
            }
        }
        Ok(())
    }

    /// The multiplier of each progression, by `CategoricalProgressionId`: every progression of
    /// the tables, [`NORMAL_EXPERIENCE`] where the file leaves it out.
    pub fn progressions(&self) -> Vec<(&'static str, f64)> {
        let mut progressions = vec![(UMBRAL_CRYSTALS_ID, self.umbral_crystals)];
        for (_, names, values) in self.groups() {
            progressions.extend(
                names.iter().map(|(name, id)| {
                    (*id, values.get(*name).copied().unwrap_or(NORMAL_EXPERIENCE))
                }),
            );
        }
        progressions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Double experience.
    const DOUBLE: f64 = 2.0;

    fn with(
        table: fn(&mut ExperienceConfig) -> &mut BTreeMap<String, f64>,
        name: &str,
        value: f64,
    ) -> ExperienceConfig {
        let mut experience = ExperienceConfig::default();
        table(&mut experience).insert(name.to_owned(), value);
        experience
    }

    #[test]
    fn a_left_out_setting_is_normal_experience() {
        let mut experience = with(|config| &mut config.trade_skills, "riding", DOUBLE);
        experience.character = DOUBLE;
        assert_eq!(experience.validate(), Ok(()));
        let progressions = experience.progressions();
        assert_eq!(
            progressions.len(),
            1 + TRADE_SKILLS.len() + WEAPON_MASTERIES.len() + TERRITORY_STANDING.len()
        );
        for (id, value) in progressions {
            let expected = if id == "Riding" {
                DOUBLE
            } else {
                NORMAL_EXPERIENCE
            };
            assert!((value - expected).abs() < f64::EPSILON, "{id}");
        }
    }

    #[test]
    fn an_unknown_name_or_a_bad_value_is_refused() {
        let refused = |experience: ExperienceConfig, setting: &str| {
            assert_eq!(
                experience.validate().map_err(|error| error.setting),
                Err(setting.to_owned())
            );
        };
        refused(
            with(|config| &mut config.trade_skills, "ridin", DOUBLE),
            "trade_skills.ridin",
        );
        refused(
            with(
                |config| &mut config.territory_standing,
                "brightwood",
                -DOUBLE,
            ),
            "territory_standing.brightwood",
        );
        refused(
            with(|config| &mut config.weapon_masteries, "bow", f64::NAN),
            "weapon_masteries.bow",
        );
        refused(
            ExperienceConfig {
                character: f64::INFINITY,
                ..ExperienceConfig::default()
            },
            "character",
        );
        refused(
            ExperienceConfig {
                umbral_crystals: -DOUBLE,
                ..ExperienceConfig::default()
            },
            "umbral_crystals",
        );
    }

    #[test]
    fn each_table_names_each_progression_once() {
        let progressions = ExperienceConfig::default().progressions();
        let mut ids: Vec<&str> = progressions.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), progressions.len());
    }
}
