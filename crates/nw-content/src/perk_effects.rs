// SPDX-License-Identifier: AGPL-3.0-or-later
//! The status effects that worn perks hold (`PerkData.Affix` -> `AffixStatData.StatusEffect`):
//! 138 perks of build 25810808, the tool, armour, amulet and bag tradeskill perks among them
//! (`PerkID_Tools_Efficiency_Mining` -> `Status_Perk_Tools_Efficiency_Mining`).
//!
//! **Observed** (client `FUN_143675f70` reads `ScalingPerGearScore` with `FUN_14367c270`): the
//! cell is a comma list of steps, each `<gearScore>:<scale>`, the first without a gear score
//! (`0.00457,625:0.008`). **Observed** (retail `general-other-01`, the own local effects map at
//! 1792964570474): `Status_Perk_Bags_HarestingWeight` held with potency 2.5078 (a half float)
//! while the player wore a `BagAT4` of gear score 430 (scale `0.00457,625:0.008`):
//! 1 + 0.00457 x (430 - 100) = 2.508. So the potency is 1 plus, from gear score 100 up to the
//! item's, each step's scale per gear score point (Inferred from that one entry: the other two
//! bags, 525 and 580, give 2.94 and 3.19). A perk without the cell has potency 1
//! (`Status_Perk_Weapon_OnCritEmpower`, potency 1 in the same map).
//!
//! A perk with an `EquipAbility` turns that ability on while it is worn ([`PerkEffects::ability_of`]:
//! the ability lists test it with `IfAbilityActive`). **Unknown**: `ScalingPerGearScoreAttributes`.

use std::collections::HashMap;

use crate::Tables;

/// The gear score from which the scale counts (module notes).
const SCALE_FROM_GEAR_SCORE: u32 = 100;
/// The potency of a perk at the scale's start.
const BASE_POTENCY: f64 = 1.0;
/// Separates the steps of a `ScalingPerGearScore` cell.
const STEP_SEPARATOR: char = ',';
/// Separates a step's gear score and scale.
const GEAR_SCORE_SEPARATOR: char = ':';

/// The effect one perk holds while its item is worn.
#[derive(Debug, Clone, PartialEq)]
pub struct PerkEffect {
    /// `AffixStatData.StatusEffect`: the effect's `StatusID`.
    pub status_effect: String,
    /// `ScalingPerGearScore`: (gear score from which it applies, scale per point), ascending.
    steps: Vec<(u32, f64)>,
}

impl PerkEffect {
    /// The effect `status_effect` with the steps of `ScalingPerGearScore` cell `scaling`; a cell
    /// that does not read gives no steps.
    pub fn new(status_effect: &str, scaling: Option<&str>) -> Self {
        Self {
            status_effect: status_effect.to_owned(),
            steps: scaling.map(steps_of).unwrap_or_default(),
        }
    }

    /// The potency on an item of gear score `gear_score` (module notes).
    pub fn potency(&self, gear_score: u32) -> f64 {
        let mut potency = BASE_POTENCY;
        for (index, (from, scale)) in self.steps.iter().enumerate() {
            let start = (*from).max(SCALE_FROM_GEAR_SCORE);
            let end = self
                .steps
                .get(index + 1)
                .map_or(gear_score, |(next, _)| (*next).min(gear_score));
            if end > start {
                potency += scale * f64::from(end - start);
            }
        }
        potency
    }
}

/// The steps of a `ScalingPerGearScore` cell, ascending; none when a step does not read.
fn steps_of(cell: &str) -> Vec<(u32, f64)> {
    let steps: Option<Vec<(u32, f64)>> = cell
        .split(STEP_SEPARATOR)
        .map(str::trim)
        .filter(|step| !step.is_empty())
        .map(|step| match step.split_once(GEAR_SCORE_SEPARATOR) {
            Some((from, scale)) => Some((from.trim().parse().ok()?, scale.trim().parse().ok()?)),
            None => Some((0, step.parse().ok()?)),
        })
        .collect();
    let mut steps = steps.unwrap_or_default();
    steps.sort_by_key(|(from, _)| *from);
    steps
}

/// The effect of every perk that holds one, and the ability of every perk that turns one on, by
/// lowercase `PerkID`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerkEffects {
    by_perk: HashMap<String, PerkEffect>,
    abilities: HashMap<String, String>,
}

impl PerkEffects {
    /// The effects of `tables`: each perk with an `Affix` whose `AffixStatData` row names a
    /// `StatusEffect`; and each perk's `EquipAbility`.
    pub fn from_tables(tables: &Tables) -> Self {
        let abilities = tables
            .perk_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                let ability = row.equip_ability.as_deref()?.trim();
                (!ability.is_empty()).then(|| (row.perk_id.to_lowercase(), ability.to_owned()))
            })
            .collect();
        let effects: HashMap<String, &str> = tables
            .affix_stat_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                let effect = row
                    .status_effect
                    .as_deref()
                    .filter(|cell| !cell.is_empty())?;
                Some((row.status_id.to_lowercase(), effect))
            })
            .collect();
        Self::build(
            tables
                .perk_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let affix = row.affix.as_deref()?.to_lowercase();
                    let effect = effects.get(&affix)?;
                    Some((
                        row.perk_id.clone(),
                        PerkEffect::new(effect, row.scaling_per_gear_score.as_deref()),
                    ))
                }),
        )
        .with_abilities(abilities)
    }

    /// These effects, with the `EquipAbility` of each perk (lowercase id to ability id).
    pub fn with_abilities(self, abilities: HashMap<String, String>) -> Self {
        Self { abilities, ..self }
    }

    /// The ability perk `perk` (any case) turns on while it is worn.
    pub fn ability_of(&self, perk: &str) -> Option<&str> {
        self.abilities.get(&perk.to_lowercase()).map(String::as_str)
    }

    /// The effects of `perks` (perk id, effect); the first of an id wins.
    pub fn build(perks: impl IntoIterator<Item = (String, PerkEffect)>) -> Self {
        let mut by_perk = HashMap::new();
        for (perk, effect) in perks {
            by_perk.entry(perk.to_lowercase()).or_insert(effect);
        }
        Self {
            by_perk,
            abilities: HashMap::new(),
        }
    }

    /// The effect of perk `perk` (any case).
    pub fn of(&self, perk: &str) -> Option<&PerkEffect> {
        self.by_perk.get(&perk.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PerkID_Bags_HarestingWeight`'s cell (build 25810808; the data spells it so).
    const BAG_SCALING: &str = "0.00457,625:0.008";
    /// The retail potency, as the half float read it.
    const RETAIL_POTENCY: f64 = 2.507_812_5;
    /// The half float's step near 2.5.
    const HALF_STEP: f64 = 0.001_953_125;

    #[test]
    fn the_potency_of_the_retail_bag_fits_its_gear_score() {
        let bag = PerkEffect::new("Status_Perk_Bags_HarestingWeight", Some(BAG_SCALING));
        assert!((bag.potency(430) - RETAIL_POTENCY).abs() < HALF_STEP);
        assert!((bag.potency(625) - (1.0 + 0.004_57 * 525.0)).abs() < 1e-9);
        assert!(
            (bag.potency(700) - (1.0 + 0.004_57 * 525.0 + 0.008 * 75.0)).abs() < 1e-9,
            "the second step from 625"
        );
        assert!((bag.potency(50) - 1.0).abs() < 1e-12, "below 100: none");
    }

    #[test]
    fn a_perk_without_scaling_or_with_a_bad_cell_has_potency_one() {
        let plain = PerkEffect::new("Status_Perk_Weapon_OnCritEmpower", None);
        assert!((plain.potency(600) - 1.0).abs() < 1e-12);
        let bad = PerkEffect::new("Status_Perk_Lucky", Some("x,625:0"));
        assert!((bad.potency(600) - 1.0).abs() < 1e-12);
        let stops = PerkEffect::new("Status_Perk_Lucky", Some("0.00909090909090909,625:0"));
        assert!(
            (stops.potency(700) - stops.potency(625)).abs() < 1e-12,
            "a scale of 0 from 625"
        );
    }

    #[test]
    fn perks_are_found_by_their_id_in_any_case() {
        let effects = PerkEffects::build([(
            "PerkID_Tools_Efficiency_Mining".to_owned(),
            PerkEffect::new("Status_Perk_Tools_Efficiency_Mining", Some("7.27E-3")),
        )]);
        let effect = effects.of("perkid_tools_efficiency_mining").expect("perk");
        assert_eq!(effect.status_effect, "Status_Perk_Tools_Efficiency_Mining");
        assert!(effects.of("PerkID_Common_Luck").is_none());
    }

    #[test]
    fn a_perk_names_the_ability_it_turns_on_in_any_case() {
        const ABILITY: &str = "GlobalPerk_Ability_Sword_WhirlingBlade";
        let effects = PerkEffects::build([]).with_abilities(HashMap::from([(
            "perkid_ability_sword_whirlingblade".to_owned(),
            ABILITY.to_owned(),
        )]));
        assert_eq!(
            effects.ability_of("PerkID_Ability_Sword_WhirlingBlade"),
            Some(ABILITY)
        );
        assert_eq!(effects.ability_of("PerkID_Common_Luck"), None);
    }
}
