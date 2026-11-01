// SPDX-License-Identifier: AGPL-3.0-or-later
//! `player-abilities`: builds the player ability table (`player-abilities-<build>.json`, read by
//! `nw_content::player_abilities`) from the game paks and the content bundle (abilities design
//! note, 2026-10-31).
//!
//! Each ability list of the player grid with `WeaponRequirements` goes with the first weapon
//! stance (by `MannequinTag` cell) whose items have one of those item classes. The table keeps
//! every action of the list, each fragment they play for that stance (its length, its
//! `CAGE-Named` markers and its hit, as a basic attack's, [`crate::player_attacks`]), the
//! `Cooldowns_Player` times and `ManaCosts_Player` costs the actions name, and the `SpellData`
//! row of each cast.

#[cfg(test)]
use std::collections::BTreeSet;
use std::collections::{BTreeMap, HashMap};

use nw_content::generated::{SpellData, Tables};
use nw_content::player_abilities::{
    AbilityCast, AbilityList, FragmentMarker, FragmentRow, FragmentTransition, ListAction,
    PlayerAbilities, SpellProjectile, SpellRow,
};
use nw_content::player_attacks::stance_tags;
use nw_formats::input_profile::input_actions;
use nw_formats::mannequin::{ActionList, Condition, Fragment, GridList};

use crate::melee_shapes::ms;
use crate::player_attacks::{
    AnimationSources, BundleRows, NAMED_CLIP, NAMED_CLIP_NAME, fragment_attack, weapon_body,
};

/// Milliseconds per second.
const MS_PER_S: u64 = 1_000;
/// The separator of a spell row's status effects (`A,B`).
const EFFECT_SEPARATOR: char = ',';
/// The client's input profile: the actions of every input number.
const INPUT_PROFILE: &str = "libs/config/defaultprofile.xml";
/// A text cell that is on (`ClearOnExit`).
const CELL_TRUE: &str = "1";
/// The clip that moves a running action on to another, and its parameters.
const TRANSITION_CLIP: &str = "CAGE-TransitionInstant";
const TRANSITION_ACTION: &str = "NewAction";
const TRANSITION_FRAGMENT: &str = "NewFragment";
const TRANSITION_INPUT: &str = "Input";
const TRANSITION_CONDITION: &str = "Condition";
/// The input profile's input that is always on: a transition that names it needs no input.
const ALWAYS_ON_INPUT: &str = "Test_AlwaysTrue";

/// What the build made and left out.
#[derive(Debug, Default)]
pub struct PlayerAbilitiesReport {
    /// Ability lists written.
    pub lists: usize,
    /// Actions written.
    pub actions: usize,
    /// Input action names written.
    pub inputs: usize,
    /// Each list or part left out, and why.
    pub skipped: Vec<(String, String)>,
}

/// The rows of the data tables an ability list reads.
struct AbilityRows<'a> {
    /// `Cooldowns_Player` time, ms, by lowercase ability id.
    cooldowns: HashMap<String, (String, u64)>,
    /// `ManaCosts_Player` cost by lowercase cost id.
    mana: HashMap<String, (String, f32)>,
    /// `SpellData` rows by lowercase spell id.
    spells: HashMap<String, &'a SpellData>,
}

impl<'a> AbilityRows<'a> {
    fn of(tables: &'a Tables) -> Self {
        Self {
            cooldowns: tables
                .cooldown_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    (
                        row.ability_id.to_lowercase(),
                        (
                            row.ability_id.clone(),
                            u64::try_from(row.time).unwrap_or_default() * MS_PER_S,
                        ),
                    )
                })
                .collect(),
            mana: tables
                .mana_data
                .iter()
                .flat_map(|table| &table.rows)
                // A mana cost is a small number.
                .map(|row| {
                    (
                        row.cost_id.to_lowercase(),
                        (row.cost_id.clone(), row.mana_cost as f32),
                    )
                })
                .collect(),
            spells: tables
                .spell_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| (row.spell_id.to_lowercase(), row))
                .collect(),
        }
    }

    fn mana_of(&self, cost: &str) -> Option<f32> {
        self.mana.get(&cost.to_lowercase()).map(|(_, cost)| *cost)
    }

    /// The spell row `id`, with its projectile from `sources` and its secondary spell (when
    /// `with_secondary`); a ranged spell without its ranged attack or flight, and a secondary
    /// spell without a row, go into `skipped`.
    fn spell(
        &self,
        id: &str,
        sources: &AnimationSources,
        skipped: &mut Vec<(String, String)>,
        with_secondary: bool,
    ) -> Option<SpellRow> {
        let row = self.spells.get(&id.to_lowercase())?;
        let secondary = row
            .secondary_spell_id
            .as_deref()
            .filter(|secondary| with_secondary && !secondary.is_empty())
            .and_then(|secondary| {
                let spell = self.spell(secondary, sources, skipped, false);
                if spell.is_none() {
                    skipped.push((
                        row.spell_id.clone(),
                        format!("secondary spell {secondary}: no row"),
                    ));
                }
                spell
            });
        let projectile = match (&row.ranged_attack_profile, &row.ranged_attack_name) {
            (Some(profile), Some(name)) => {
                let projectile = spell_projectile(sources, profile, name, row);
                if projectile.is_none() {
                    skipped.push((
                        row.spell_id.clone(),
                        format!("no ranged attack {profile}/{name} or no flight"),
                    ));
                }
                projectile
            }
            _ => None,
        };
        Some(SpellRow {
            id: row.spell_id.clone(),
            types: row.spell_types.clone(),
            shape: row.shape_types.clone(),
            // Lengths of metres.
            radius_m: row.radius.map(|radius| radius as f32),
            cast_distance_m: row.cast_distance.map(|distance| distance as f32),
            duration_ms: row.duration,
            damage_row: row.damage_table_row.clone(),
            status_effects: row
                .status_effects
                .iter()
                .flat_map(|effects| effects.split(EFFECT_SEPARATOR))
                .map(str::trim)
                .filter(|effect| !effect.is_empty())
                .map(str::to_owned)
                .collect(),
            target_types: row.target_types.clone(),
            mana_cost: row.mana_cost.as_deref().and_then(|cost| self.mana_of(cost)),
            projectile,
            clear_on_exit: row.clear_on_exit.as_deref() == Some(CELL_TRUE),
            clear_on_end: row.clear_on_end == Some(true),
            secondary: secondary.map(Box::new),
        })
    }
}

/// The projectile of the spell `row`: its ranged attack `profile` / `name` and its prefab's
/// flight (none for a hit scan).
fn spell_projectile(
    sources: &AnimationSources,
    profile: &str,
    name: &str,
    row: &SpellData,
) -> Option<SpellProjectile> {
    let (range_m, radius_m, hit_scan) =
        *sources.ranged.get(&(profile.to_owned(), name.to_owned()))?;
    let flight = if hit_scan {
        None
    } else {
        let prefab = row.spell_prefab_path.as_deref()?.to_lowercase();
        Some(*sources.flights.get(&prefab)?)
    };
    Some(SpellProjectile {
        range_m,
        radius_m,
        flight,
    })
}

/// The weapon stance of the item classes `classes` (any case): the first whose `MannequinTag`
/// cell names one of them (`1H_Melee,Sword` for `Sword`), else the first whose items have one
/// (`2HAxe` items are `2H_Melee,GreatAxe`). The tag comes first because tools carry weapon
/// classes too (the fleshing knife items are of class `Sword`, Observed in the bundle).
fn stance_of<'a>(rows: &'a BundleRows, classes: &[String]) -> Option<&'a String> {
    let wanted = |class: &str| {
        classes
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case(class))
    };
    rows.weapon_classes
        .keys()
        .find(|cell| stance_tags(cell).iter().any(|tag| wanted(tag)))
        .or_else(|| {
            rows.weapon_classes
                .iter()
                .find(|(_, own)| own.iter().any(|class| wanted(class)))
                .map(|(cell, _)| cell)
        })
}

/// The `CAGE-Named` markers of `fragments`, by start; a marker without an end lasts to `end_ms`.
fn markers_of(fragments: &[&Fragment], end_ms: u64) -> Vec<FragmentMarker> {
    let mut markers: Vec<FragmentMarker> = fragments
        .iter()
        .flat_map(|fragment| fragment.clips_of(NAMED_CLIP))
        .filter_map(|clip| {
            Some(FragmentMarker {
                name: clip.param(NAMED_CLIP_NAME)?.to_owned(),
                start_ms: ms(clip.start_s),
                end_ms: clip.end_s.map_or(end_ms, ms),
            })
        })
        .collect();
    markers.sort_by_key(|marker| marker.start_ms);
    markers
}

/// The `CAGE-TransitionInstant` clips of `fragments` that name an action of `list` (`NewAction`,
/// else the action that plays its `NewFragment`: the ice gauntlet's Ice Storm intro goes on to
/// `Ability_IceMagic_IceStorm_Aim`, the fragment of `IceMagic_IceStorm_AimSubState`), by start; a
/// clip without an end lasts to `end_ms`. A clip's named condition is the one of `list`, else of
/// another of the action lists `all` (the bow's aim fragment tests the archery list's); a name
/// that none has stays a test that never holds.
fn transitions_of(
    fragments: &[&Fragment],
    end_ms: u64,
    list: &ActionList,
    all: &[ActionList],
) -> Vec<FragmentTransition> {
    let mut transitions: Vec<FragmentTransition> = fragments
        .iter()
        .flat_map(|fragment| fragment.clips_of(TRANSITION_CLIP))
        .filter_map(|clip| {
            let named = |key| clip.param(key).filter(|value| !value.is_empty());
            let action = named(TRANSITION_ACTION).map(str::to_owned).or_else(|| {
                let fragment = named(TRANSITION_FRAGMENT)?;
                list.actions
                    .iter()
                    .find(|action| action.fragment.eq_ignore_ascii_case(fragment))
                    .map(|action| action.name.clone())
            })?;
            Some(FragmentTransition {
                action,
                input: clip
                    .param(TRANSITION_INPUT)
                    .filter(|input| !input.is_empty() && *input != ALWAYS_ON_INPUT)
                    .map(str::to_owned),
                condition: named(TRANSITION_CONDITION).map(|name| {
                    list.resolved_condition(name)
                        .or_else(|| all.iter().find_map(|other| other.resolved_condition(name)))
                        .unwrap_or_else(|| Condition::External(name.to_owned()))
                }),
                start_ms: ms(clip.start_s),
                end_ms: clip.end_s.map_or(end_ms, ms),
            })
        })
        .collect();
    transitions.sort_by_key(|transition| transition.start_ms);
    transitions
}

/// The cooldown and mana cost ids `condition` tests, into `cooldowns` and `costs`.
fn tested_ids(condition: &Condition, cooldowns: &mut Vec<String>, costs: &mut Vec<String>) {
    match condition {
        Condition::All(parts) | Condition::Any(parts) => {
            for part in parts {
                tested_ids(part, cooldowns, costs);
            }
        }
        Condition::Not(part) => tested_ids(part, cooldowns, costs),
        Condition::Cooldown(id) => cooldowns.push(id.clone()),
        Condition::ManaCost { cost, .. } => costs.push(cost.clone()),
        _ => {}
    }
}

/// The ability list of the grid list `list`, whose action list is `action_list`.
fn list_of(
    sources: &AnimationSources,
    rows: &BundleRows,
    data: &AbilityRows<'_>,
    list: &GridList,
    action_list: &ActionList,
    skipped: &mut Vec<(String, String)>,
) -> AbilityList {
    let actions = &action_list.actions;
    let label = list.weapon_classes.join("+");
    let cell = stance_of(rows, &list.weapon_classes);
    if cell.is_none() {
        skipped.push((label, "no weapon stance has these item classes".to_owned()));
    }
    let weapon = cell.and_then(|cell| rows.weapons.get(cell));
    let owned_tags = cell.map(|cell| stance_tags(cell)).unwrap_or_default();
    let tags: Vec<&str> = owned_tags.iter().map(String::as_str).collect();
    let body = weapon.map(|weapon| weapon_body(&sources.bodies, weapon));
    let mut fragments: Vec<FragmentRow> = Vec::new();
    for action in actions.iter().filter(|action| !action.fragment.is_empty()) {
        if fragments
            .iter()
            .any(|fragment| fragment.name.eq_ignore_ascii_case(&action.fragment))
        {
            continue;
        }
        let player = sources.player_fragment_any_case(&action.fragment, &tags);
        let attach = weapon
            .and_then(|weapon| weapon.attach.as_ref())
            .and_then(|path| sources.attach.get(path))
            .and_then(|adb| {
                let entry = adb
                    .fragments
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(&action.fragment))?;
                adb.best(&entry.name, &tags)
            });
        let found: Vec<&Fragment> = attach.into_iter().chain(player).collect();
        if found.is_empty() {
            continue;
        }
        let hit = match (weapon, &body) {
            (Some(weapon), Some(body)) => fragment_attack(
                sources,
                rows,
                weapon,
                body.as_ref(),
                &tags,
                &action.name,
                &action.fragment,
            )
            .ok(),
            _ => None,
        };
        let length_ms = hit.as_ref().map_or_else(
            || {
                player
                    .and_then(|fragment| sources.bodies.fragment_length_s(fragment))
                    .map_or(0, ms)
            },
            |hit| hit.length_ms,
        );
        fragments.push(FragmentRow {
            name: action.fragment.clone(),
            length_ms,
            markers: markers_of(&found, length_ms),
            transitions: transitions_of(&found, length_ms, action_list, &sources.actions),
            hit,
        });
    }
    let mut cooldown_ids = Vec::new();
    let mut cost_ids = Vec::new();
    let list_actions: Vec<ListAction> = actions
        .iter()
        .map(|action| {
            for condition in action.activation.iter().chain(&action.deactivation) {
                tested_ids(condition, &mut cooldown_ids, &mut cost_ids);
            }
            for cast in &action.casts {
                tested_ids(&cast.condition, &mut cooldown_ids, &mut cost_ids);
            }
            if let Some(ability) = &action.ability {
                cooldown_ids.extend(ability.starts_cooldowns.iter().cloned());
                cooldown_ids.extend(ability.needs_ready.iter().cloned());
                cost_ids.extend(ability.mana_cost.iter().cloned());
            }
            ListAction {
                name: action.name.clone(),
                fragment: action.fragment.clone(),
                activation: action.activation.clone(),
                deactivation: action.deactivation.clone(),
                sets_tags: action.sets_tags.clone(),
                ability: action.ability.clone(),
                casts: action
                    .casts
                    .iter()
                    .filter_map(|cast| {
                        let spell = data.spell(&cast.spell, sources, skipped, true);
                        if spell.is_none() {
                            skipped.push((
                                action.name.clone(),
                                format!("spell {}: no row", cast.spell),
                            ));
                        }
                        Some(AbilityCast {
                            condition: cast.condition.clone(),
                            kind: cast.kind,
                            spell: spell?,
                        })
                    })
                    .collect(),
            }
        })
        .collect();
    let cooldowns_ms: BTreeMap<String, u64> = cooldown_ids
        .iter()
        .filter_map(|id| data.cooldowns.get(&id.to_lowercase()).cloned())
        .collect();
    for id in &cooldown_ids {
        if !cooldowns_ms.keys().any(|own| own.eq_ignore_ascii_case(id)) {
            skipped.push((id.clone(), "no cooldown row".to_owned()));
        }
    }
    let mana_costs: BTreeMap<String, f32> = cost_ids
        .iter()
        .filter_map(|id| data.mana.get(&id.to_lowercase()).cloned())
        .collect();
    AbilityList {
        weapon_classes: list.weapon_classes.clone(),
        actions: list_actions,
        fragments,
        cooldowns_ms,
        mana_costs,
    }
}

/// The ability table of `sources` and the data `tables`, with what it left out.
pub fn build_abilities(
    sources: &AnimationSources,
    rows: &BundleRows,
    tables: &Tables,
) -> (PlayerAbilities, PlayerAbilitiesReport) {
    let data = AbilityRows::of(tables);
    let mut report = PlayerAbilitiesReport::default();
    let lists: Vec<AbilityList> = sources
        .grid
        .iter()
        .filter(|list| !list.weapon_classes.is_empty())
        .filter_map(|list| {
            let path = list.pak_path();
            let Some(index) = sources.action_paths.iter().position(|own| *own == path) else {
                report
                    .skipped
                    .push((path, "the action list is not in the paks".to_owned()));
                return None;
            };
            Some(list_of(
                sources,
                rows,
                &data,
                list,
                &sources.actions[index],
                &mut report.skipped,
            ))
        })
        .collect();
    report.lists = lists.len();
    report.actions = lists.iter().map(|list| list.actions.len()).sum();
    let inputs = input_names(sources, &mut report.skipped);
    report.inputs = inputs.len();
    report.skipped.sort();
    report.skipped.dedup();
    (PlayerAbilities { lists, inputs }, report)
}

/// The client's input action names in input-number order, from the input profile of
/// `sources` ([`nw_formats::input_profile`]); none, with the reason in `skipped`, when it is
/// missing or does not read.
fn input_names(sources: &AnimationSources, skipped: &mut Vec<(String, String)>) -> Vec<String> {
    let Some(profile) = sources.input_files.get(INPUT_PROFILE) else {
        skipped.push((INPUT_PROFILE.to_owned(), "not in the paks".to_owned()));
        return Vec::new();
    };
    input_actions(profile, &|path| sources.input_files.get(path).cloned()).unwrap_or_else(|why| {
        skipped.push((INPUT_PROFILE.to_owned(), why.to_string()));
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use nw_content::player_attacks::Flight;

    use super::*;

    const PROFILE: &str = "FireBall";
    const SIEGE: &str = "Siege";
    const BEAM: &str = "Beam";
    const PREFAB: &str = "Items/Weapons/FireSiege_Projectile_Scorched_Earth";
    const RANGE_M: f32 = 30.0;
    const RADIUS_M: f32 = 0.3;
    const FLIGHT: Flight = Flight {
        base_speed_mps: 25.0,
        extra_speed_mps: 0.0,
        time_for_max_speed_s: 0.0,
        gravity_mps2: 0.0,
        lifetime_s: 2.0,
    };

    fn sources() -> AnimationSources {
        AnimationSources {
            player: Vec::new(),
            attach: HashMap::new(),
            actions: Vec::new(),
            action_paths: Vec::new(),
            grid: Vec::new(),
            sheathe: None,
            ranged: HashMap::from([
                (
                    (PROFILE.to_owned(), SIEGE.to_owned()),
                    (RANGE_M, RADIUS_M, false),
                ),
                ((PROFILE.to_owned(), BEAM.to_owned()), (RANGE_M, 0.0, true)),
            ]),
            flights: HashMap::from([(PREFAB.to_lowercase(), FLIGHT)]),
            input_files: HashMap::new(),
            bodies: crate::melee_shapes::test_support::bodies(),
        }
    }

    fn row(name: &str, prefab: &str) -> SpellData {
        serde_json::from_value(serde_json::json!({
            "spell_id": "Spell_Ability_Firestaff_Fireball",
            "ranged_attack_profile": PROFILE,
            "ranged_attack_name": name,
            "spell_prefab_path": prefab,
        }))
        .expect("row")
    }

    #[test]
    fn a_ranged_spell_flies_as_its_prefab_and_a_hit_scan_does_not() {
        let sources = sources();
        assert_eq!(
            spell_projectile(&sources, PROFILE, SIEGE, &row(SIEGE, PREFAB)),
            Some(SpellProjectile {
                range_m: RANGE_M,
                radius_m: RADIUS_M,
                flight: Some(FLIGHT),
            }),
        );
        let beam = spell_projectile(&sources, PROFILE, BEAM, &row(BEAM, "Items/None"));
        assert_eq!(beam.map(|beam| beam.flight), Some(None), "a hit scan");
        assert_eq!(
            spell_projectile(&sources, PROFILE, SIEGE, &row(SIEGE, "Items/None")),
            None,
            "no flight"
        );
        assert_eq!(
            spell_projectile(&sources, PROFILE, "Unknown", &row("Unknown", PREFAB)),
            None,
            "no ranged attack"
        );
    }

    #[test]
    fn a_spell_keeps_its_clear_rules_and_its_secondary_one_level_deep() {
        const WELL: &str = "Spell_GreatAxe_GravityWell_Projectile";
        const VORTEX: &str = "Spell_GreatAxe_GravityWell_Vortex";
        const LOST: &str = "Spell_Lost";
        let well: SpellData = serde_json::from_value(serde_json::json!({
            "spell_id": WELL,
            "secondary_spell_id": VORTEX,
        }))
        .expect("row");
        let vortex: SpellData = serde_json::from_value(serde_json::json!({
            "spell_id": VORTEX,
            "clear_on_exit": "1",
            "clear_on_end": true,
            "secondary_spell_id": WELL,
        }))
        .expect("row");
        let lost: SpellData = serde_json::from_value(serde_json::json!({
            "spell_id": LOST,
            "secondary_spell_id": "Spell_Nowhere",
        }))
        .expect("row");
        let data = AbilityRows {
            cooldowns: HashMap::new(),
            mana: HashMap::new(),
            spells: HashMap::from([
                (WELL.to_lowercase(), &well),
                (VORTEX.to_lowercase(), &vortex),
                (LOST.to_lowercase(), &lost),
            ]),
        };
        let mut skipped = Vec::new();
        let spell = data
            .spell(WELL, &sources(), &mut skipped, true)
            .expect("spell");
        let secondary = spell.secondary.expect("secondary");
        assert_eq!(secondary.id, VORTEX);
        assert!(secondary.clear_on_exit && secondary.clear_on_end);
        assert!(secondary.secondary.is_none(), "one level");
        assert!(!spell.clear_on_exit && !spell.clear_on_end);
        assert!(skipped.is_empty());
        let lost = data
            .spell(LOST, &sources(), &mut skipped, true)
            .expect("spell");
        assert!(lost.secondary.is_none());
        assert_eq!(skipped.len(), 1, "{skipped:?}");
    }

    #[test]
    fn a_stance_whose_tags_name_the_class_wins_over_a_tool_of_that_class() {
        let classes = |list: &[&str]| -> BTreeSet<String> {
            list.iter().map(|class| (*class).to_owned()).collect()
        };
        let rows = BundleRows::with_weapon_classes(BTreeMap::from([
            ("1H_FleshingKnife".to_owned(), classes(&["Sword", "Weapon"])),
            ("1H_Melee,Sword".to_owned(), classes(&["Sword", "Weapon"])),
            (
                "2H_Melee,GreatAxe".to_owned(),
                classes(&["2HAxe", "Weapon"]),
            ),
        ]));
        let of = |class: &str| stance_of(&rows, &[class.to_owned()]).cloned();
        assert_eq!(of("Sword").as_deref(), Some("1H_Melee,Sword"));
        assert_eq!(
            of("2HAxe").as_deref(),
            Some("2H_Melee,GreatAxe"),
            "by item class"
        );
        assert_eq!(of("Dagger"), None);
    }
}
