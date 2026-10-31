// SPDX-License-Identifier: AGPL-3.0-or-later
//! The status effects of the players and the mobs (run-15 combat study): buffs, debuffs, damage
//! and heals over time, stacks, durations, immunities and the crowd-control effects, from the
//! `StatusEffectData` rules ([`StatusEffectRules`]), and the owner's
//! `StatusEffectsComponentReplicatedState` (4236, own component 44).
//!
//! Effects come from attack rows (`DamageData.StatusEffect`: a mob's hit on a player, a player's
//! hit on a mob), from used consumables (`AddStatusEffects`; their health and stamina changes
//! stay with [`super::item_calls`]), from other effects (`OnTickStatusEffect`,
//! `OnEndStatusEffect`), from the equip load (`EquipLoad_*` effects, retail newchar spawn) and
//! from being out of combat (`Status_OutOfCombat_PassiveRegen`).
//!
//! Retail sends the owner's state on each change only (no periodic resend), in the own group;
//! this server sends the whole map at each change (a full list is what the client reads when
//! it has no base, and it reads one at any time; the retail writer sends one when the delta
//! would be empty or too long).

use bevy_ecs::entity::Entity;
use nw_content::item_rules::Consumable;
use nw_content::perk_effects::PerkEffects;
use nw_content::status_effects::{RowKey, StatusEffectRule, StatusEffectRules};
use nw_messages::{
    BundleMember, DamageDealtMsg, DamageItem, EffectMap, HIT_TAG, LocalEffect, MemberState,
    NEUTRAL_SCALE, OnDamageMsg, OnHealedMsg, STATUS_EFFECTS_COMPONENT, ServerMessage,
    StatusEffectsState,
};
use nw_sim_core::EquipLoad;

use super::attack::Vitals;
use super::gear::unix_ns;
use super::{EQUIP_LOAD_SLOTS, Player, SimWorld, send_now};
use crate::combat::AttackOutcome;

/// The effect of a hurt player out of combat (`BaseDuration` 180, `Health` 40 a second; retail
/// removed it at the first hit taken). When it starts is a best guess from the retail captures:
/// out of combat and below the max health (the newchar tutorial had none in its first 12
/// minutes, open-world sessions had it after their fights).
const OUT_OF_COMBAT_REGEN: &str = "Status_OutOfCombat_PassiveRegen";
/// The equip-load effects: the prefix of each, then the load's suffix (retail newchar spawn:
/// `EquipLoad_CCDurReduction_Heavy` and `EquipLoad_CritDmgReduction_Heavy` on a slow load; an
/// open-world light load had the two `_Light` ones).
const EQUIP_LOAD_EFFECTS: [&str; 2] = ["EquipLoad_CCDurReduction_", "EquipLoad_CritDmgReduction_"];
const LIGHT_LOAD: &str = "Light";
const MEDIUM_LOAD: &str = "Medium";
const HEAVY_LOAD: &str = "Heavy";
/// The tick rate of a predicted lightweight value (Observed: 30 a second, median error 0.5 tick
/// over 1,242 pairs of retail values).
const TICKS_PER_SECOND: u64 = 30;
/// The tick of the own spawn (retail: `65507` = tick 1 less 30 at the newchar spawn).
const SPAWN_TICK: u64 = 1;
/// How far a predicted lightweight effect without an end reads before its add, in ticks.
const NO_END_LEAD_TICKS: u64 = 30;
/// Predicted lightweight values are 16-bit ticks (retail never above 65535).
const TICK_WRAP: u64 = 1 << 16;
/// Milliseconds per second.
const MS_PER_SECOND: u64 = 1_000;
/// The version of the state a player gets before the first change.
const FIRST_VERSION: u64 = 1;
/// The potency of an effect that nothing scales.
const PLAIN_POTENCY: f64 = 1.0;

/// What added an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// A used consumable: its health and stamina changes are [`super::item_calls`]'.
    Consumable,
    /// A hit, another effect or a rule of the world.
    World,
}

/// One application of an effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Application {
    /// The entity number of what applied it (the holder itself for its own effects).
    pub(super) source: u64,
    /// What added it.
    pub(super) origin: Origin,
    /// The potency of one stack (`StatusEffectMod`; 1 for a hit).
    pub(super) potency: f64,
    /// A duration that replaces the effect's own (`DurationOverrides`).
    pub(super) duration_ms: Option<u64>,
    /// The hit that applied it, for `HealthModifierDamageBased`.
    pub(super) hit_amount: f64,
}

impl Application {
    /// An application by `source` of plain potency, without a hit.
    const fn plain(source: u64) -> Self {
        Self {
            source,
            origin: Origin::World,
            potency: PLAIN_POTENCY,
            duration_ms: None,
            hit_amount: 0.0,
        }
    }
}

/// One effect a holder has.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    crc: u32,
    key: u16,
    source: u64,
    origin: Origin,
    /// When it was added (server ms).
    added_ms: u64,
    /// When it ends; `None`: until something removes it.
    ends_ms: Option<u64>,
    /// The potency of one stack.
    potency: f64,
    stacks: u8,
    applications: u8,
    /// When its next vitals change comes.
    next_tick_ms: Option<u64>,
    hit_amount: f64,
    hits_taken: u32,
}

/// One vitals change of a holder's effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct VitalsTick {
    /// What applied the effect.
    pub(super) source: u64,
    /// The health it gives (below 0: it takes).
    pub(super) health: f64,
    /// The stamina it gives.
    pub(super) stamina: f64,
    /// The `IntID` of its damage type.
    pub(super) damage_type: Option<u8>,
}

/// The clock of the owner's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EffectClock {
    /// The server time of the own spawn (the start of the own clock).
    pub(super) spawned_ms: Option<u64>,
}

impl EffectClock {
    /// The 30 Hz own tick at server time `at_ms`, below 2^16.
    fn tick(self, at_ms: u64) -> u64 {
        let since_ms = self
            .spawned_ms
            .map_or(0, |spawned| at_ms.saturating_sub(spawned));
        (SPAWN_TICK + since_ms * TICKS_PER_SECOND / MS_PER_SECOND) % TICK_WRAP
    }
}

/// The status effects of one holder.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct StatusEffects {
    held: Vec<Held>,
    /// The effects changed since the last state.
    changed: bool,
    /// The holder was dead at the last tick.
    dead: bool,
    /// The equip load whose effects it holds.
    equip_load: Option<EquipLoad>,
    /// The effects its worn perks hold (effect, potency), by effect.
    perk_effects: Vec<(u32, f64)>,
    /// The newest state, and whether its client has not got it yet.
    state: Option<StatusEffectsState>,
    state_due: bool,
    /// The maps of the newest state were not empty: an emptied map is sent once more, empty.
    local_shown: bool,
    lightweight_shown: bool,
}

impl StatusEffects {
    fn index(&self, crc: u32) -> Option<usize> {
        self.held.iter().position(|held| held.crc == crc)
    }

    /// Whether it holds effect `crc`.
    pub(super) fn holds(&self, crc: u32) -> bool {
        self.index(crc).is_some()
    }

    /// The rules of the effects it holds.
    fn rules<'a>(
        &'a self,
        rules: &'a StatusEffectRules,
    ) -> impl Iterator<Item = &'a StatusEffectRule> {
        self.held.iter().filter_map(move |held| rules.get(held.crc))
    }

    /// The duration `ms` of `rule` after the `EffectDurationMods` of the held effects for its
    /// categories, at most its `DurationMax`.
    fn scaled_duration(&self, rules: &StatusEffectRules, rule: &StatusEffectRule, ms: u64) -> u64 {
        let factor: f64 = self
            .rules(rules)
            .flat_map(|held| &held.duration_mods)
            .filter(|(category, _)| rule.has_category(category))
            .map(|(_, value)| 1.0 + value)
            .product();
        // A duration of hours at most.
        let scaled = (ms as f64 * factor.max(0.0)).round() as u64;
        rule.duration_max_ms.map_or(scaled, |max| scaled.min(max))
    }

    /// The potency of `held` (of `rule`): one stack's times the stacks, at most `PotencyMax`,
    /// times one plus the `EffectPotencyMods` of the other held effects for its categories.
    fn potency(&self, rules: &StatusEffectRules, held: &Held, rule: &StatusEffectRule) -> f64 {
        let stacked = held.potency * f64::from(held.stacks);
        let stacked = rule.potency_max.map_or(stacked, |max| stacked.min(max));
        let factor: f64 = self
            .held
            .iter()
            .filter(|other| other.crc != held.crc)
            .filter_map(|other| rules.get(other.crc))
            .flat_map(|other| &other.potency_mods)
            .filter(|(category, _)| rule.has_category(category))
            .map(|(_, value)| 1.0 + value)
            .product();
        stacked * factor
    }

    /// Adds effect `crc` by `application` at server time `now_ms`: a new effect, or one more
    /// stack and application of a held one. `false`, changing nothing, for an unknown effect, a
    /// dead holder the effect does not allow, or an effect a held one blocks (its
    /// `AntirequisiteEffectCategories`: the holder's immunity). Its application ends the held
    /// effects it removes (`RemoveStatusEffects`, `RemoveStatusEffectCategories`).
    pub(super) fn add(
        &mut self,
        rules: &StatusEffectRules,
        crc: u32,
        application: Application,
        now_ms: u64,
    ) -> bool {
        let Some(rule) = rules.get(crc) else {
            return false;
        };
        if self.dead && !rule.allow_add_if_dead {
            return false;
        }
        let blocked = self.rules(rules).any(|held| {
            held.categories
                .iter()
                .any(|category| rule.blocked_by.contains(category))
        });
        if blocked {
            return false;
        }
        self.held.retain(|held| {
            held.crc == crc
                || !rules.get(held.crc).is_some_and(|held_rule| {
                    rule.remove_effects.contains(&held_rule.crc)
                        || held_rule
                            .categories
                            .iter()
                            .any(|category| rule.remove_categories.contains(category))
                })
        });
        let ends_ms = application
            .duration_ms
            .or(rule.duration_ms)
            .map(|ms| now_ms.saturating_add(self.scaled_duration(rules, rule, ms)));
        match self.index(crc) {
            Some(index) => {
                let held = &mut self.held[index];
                held.stacks = held.stacks.saturating_add(1).min(rule.stack_max);
                // The wire count is one byte.
                held.applications = held.applications.wrapping_add(1);
                if rule.refresh_duration {
                    held.ends_ms = ends_ms;
                }
                held.source = application.source;
                held.potency = application.potency;
                held.hit_amount = application.hit_amount;
            }
            None => self.held.push(Held {
                crc,
                key: rule.key,
                source: application.source,
                origin: application.origin,
                added_ms: now_ms,
                ends_ms,
                potency: application.potency,
                stacks: 1,
                applications: 1,
                next_tick_ms: rule
                    .tick_ms
                    .map(|_| now_ms.saturating_add(rule.first_tick_ms)),
                hit_amount: application.hit_amount,
                hits_taken: 0,
            }),
        }
        self.changed = true;
        true
    }

    /// When the held effect `crc` ends (server ms); `None` when it is not held or has no end.
    pub(super) fn ends_ms(&self, crc: u32) -> Option<u64> {
        self.held
            .iter()
            .find(|held| held.crc == crc)
            .and_then(|held| held.ends_ms)
    }

    /// Adds the effect named `id` (a `StatusID`) of potency `potency`, applied by `source`, at
    /// server time `now_ms` ([`Self::add`]); `false` for an id the data does not have.
    pub(super) fn add_by_id(
        &mut self,
        rules: &StatusEffectRules,
        id: &str,
        potency: f64,
        source: u64,
        now_ms: u64,
    ) -> bool {
        let Some(crc) = rules.by_id(id).map(|rule| rule.crc) else {
            return false;
        };
        let application = Application {
            potency,
            ..Application::plain(source)
        };
        self.add(rules, crc, application, now_ms)
    }

    /// Ends effect `crc`.
    pub(super) fn remove(&mut self, crc: u32) {
        let before = self.held.len();
        self.held.retain(|held| held.crc != crc);
        self.changed |= self.held.len() != before;
    }

    /// Ends the held effects whose rule `ends` picks.
    fn remove_where(
        &mut self,
        rules: &StatusEffectRules,
        ends: impl Fn(&StatusEffectRule) -> bool,
    ) {
        let before = self.held.len();
        self.held
            .retain(|held| !rules.get(held.crc).is_some_and(&ends));
        self.changed |= self.held.len() != before;
    }

    /// The holder took a hit: the effects that end after a number of hits taken count it
    /// (`StopOnHitCount` with `OnHitTaken`; retail ended `Status_OutOfCombat_PassiveRegen` at the
    /// first hit).
    pub(super) fn hit_taken(&mut self, rules: &StatusEffectRules) {
        let before = self.held.len();
        self.held.retain_mut(|held| {
            let Some(stop) = rules.get(held.crc).and_then(|rule| rule.stop_on_hits_taken) else {
                return true;
            };
            held.hits_taken += 1;
            held.hits_taken < stop
        });
        self.changed |= self.held.len() != before;
    }

    /// The holder is `dead` now: a death ends the effects with `RemoveOnDeath`, a new life those
    /// with `RemoveOnRespawn`.
    pub(super) fn set_dead(&mut self, rules: &StatusEffectRules, dead: bool) {
        if dead == self.dead {
            return;
        }
        self.dead = dead;
        if dead {
            self.remove_where(rules, |rule| rule.remove_on_death);
        } else {
            self.remove_where(rules, |rule| rule.remove_on_respawn);
        }
    }

    /// The vitals changes due at server time `now_ms` for a holder of `max_health`, the effects
    /// that ended gone; the effects their changes and ends add (`OnTickStatusEffect`,
    /// `OnEndStatusEffect`) are added. A consumable's effects change no vitals here.
    pub(super) fn tick(
        &mut self,
        rules: &StatusEffectRules,
        now_ms: u64,
        max_health: f64,
    ) -> Vec<VitalsTick> {
        let mut changes = Vec::new();
        let mut follow_ups: Vec<(u32, u64)> = Vec::new();
        for index in 0..self.held.len() {
            let held = &self.held[index];
            let Some(rule) = rules.get(held.crc) else {
                continue;
            };
            let (Some(every_ms), Some(mut next_ms)) = (rule.tick_ms, held.next_tick_ms) else {
                continue;
            };
            let potency = self.potency(rules, held, rule);
            let ends_ms = held.ends_ms;
            let (source, origin, hit_amount) = (held.source, held.origin, held.hit_amount);
            while next_ms <= now_ms && ends_ms.is_none_or(|ends| next_ms <= ends) {
                if origin == Origin::World && rule.changes_vitals() {
                    changes.push(VitalsTick {
                        source,
                        health: (rule.health
                            + rule.health_share * max_health
                            + rule.hit_share * hit_amount)
                            * potency,
                        stamina: rule.stamina * potency,
                        damage_type: rule.damage_type,
                    });
                }
                follow_ups.extend(rule.on_tick.iter().map(|crc| (*crc, source)));
                next_ms = next_ms.saturating_add(every_ms);
            }
            self.held[index].next_tick_ms = Some(next_ms);
        }
        let mut ended = Vec::new();
        self.held.retain(|held| {
            let over = held.ends_ms.is_some_and(|ends| ends <= now_ms);
            if over {
                ended.push((held.crc, held.source));
            }
            !over
        });
        if !ended.is_empty() {
            self.changed = true;
        }
        for (crc, source) in ended {
            if let Some(rule) = rules.get(crc) {
                follow_ups.extend(rule.on_end.iter().map(|follow| (*follow, source)));
            }
        }
        for (crc, source) in follow_ups {
            self.add(rules, crc, Application::plain(source), now_ms);
        }
        changes
    }

    /// The share of a hit of damage type `tag` the held effects take off (`Abs*`), 0 to 1.
    pub(super) fn absorption(&self, rules: &StatusEffectRules, tag: u8) -> f64 {
        self.sum_by_type(rules, tag, |rule| &rule.absorption)
            .clamp(0.0, 1.0)
    }

    /// The multiple the held effects give the holder's hits of damage type `tag` (`Dmg*`).
    pub(super) fn damage_scale(&self, rules: &StatusEffectRules, tag: u8) -> f64 {
        (1.0 + self.sum_by_type(rules, tag, |rule| &rule.damage_mods)).max(0.0)
    }

    /// The gathering efficiency the held effects add to gathers of tradeskill `tradeskill`
    /// (`EFF_*`: food and the music buffs, `MusicBuff_GatheringSpeed_*` 0.75).
    pub(super) fn gather_efficiency(&self, rules: &StatusEffectRules, tradeskill: &str) -> f64 {
        self.sum_of(rules, |rule| {
            rule.gather_efficiency
                .iter()
                .filter(|(skill, _)| skill.eq_ignore_ascii_case(tradeskill))
                .map(|(_, value)| *value)
                .sum()
        })
    }

    /// The gear score the held effects add to the least and the most gear score of crafts of
    /// tradeskill `tradeskill` (`MGS_*`, `MaxGS_*`: the `FoodArmorer*` meals 5 to 15).
    pub(super) fn craft_gear_score(
        &self,
        rules: &StatusEffectRules,
        tradeskill: &str,
    ) -> (f64, f64) {
        let column = |most: bool| {
            self.sum_of(rules, |rule| {
                rule.craft_gear_score
                    .iter()
                    .filter(|(skill, _, _)| skill.eq_ignore_ascii_case(tradeskill))
                    // Gear scores of a few tens.
                    .map(|(_, least, top)| (if most { *top } else { *least }) as f64)
                    .sum()
            })
        };
        (column(false), column(true))
    }

    /// The sum over the held effects of `value`, each times its potency.
    fn sum_of(&self, rules: &StatusEffectRules, value: impl Fn(&StatusEffectRule) -> f64) -> f64 {
        self.held
            .iter()
            .filter_map(|held| Some((held, rules.get(held.crc)?)))
            .map(|(held, rule)| value(rule) * self.potency(rules, held, rule))
            .sum()
    }

    /// The sum over the held effects of their `values` for damage type `tag`, each times its
    /// potency.
    fn sum_by_type(
        &self,
        rules: &StatusEffectRules,
        tag: u8,
        values: impl Fn(&StatusEffectRule) -> &Vec<(u8, f64)>,
    ) -> f64 {
        self.held
            .iter()
            .filter_map(|held| Some((held, rules.get(held.crc)?)))
            .map(|(held, rule)| {
                let potency = self.potency(rules, held, rule);
                values(rule)
                    .iter()
                    .filter(|(own, _)| *own == tag)
                    .map(|(_, value)| value * potency)
                    .sum::<f64>()
            })
            .sum()
    }

    /// Keeps the held equip-load effects those of `load` (`EQUIP_LOAD_EFFECTS` with the load's
    /// suffix; an overburdened load has none), at server time `now_ms`; `holder` adds them.
    pub(super) fn set_equip_load(
        &mut self,
        rules: &StatusEffectRules,
        load: EquipLoad,
        holder: u64,
        now_ms: u64,
    ) {
        if self.equip_load == Some(load) {
            return;
        }
        if let Some(old) = self.equip_load {
            for crc in equip_load_effects(rules, old) {
                self.remove(crc);
            }
        }
        self.equip_load = Some(load);
        for crc in equip_load_effects(rules, load) {
            self.add(rules, crc, Application::plain(holder), now_ms);
        }
    }

    /// Keeps the held perk effects those of `wanted` (effect, potency, by effect), added by
    /// `holder` at server time `now_ms`: an effect whose potency changed goes and comes back,
    /// and one that something else ended (a death) comes back.
    pub(super) fn set_perk_effects(
        &mut self,
        rules: &StatusEffectRules,
        wanted: Vec<(u32, f64)>,
        holder: u64,
        now_ms: u64,
    ) {
        if self.perk_effects == wanted && wanted.iter().all(|(crc, _)| self.holds(*crc)) {
            return;
        }
        let old = std::mem::take(&mut self.perk_effects);
        for held in old.iter().filter(|held| !wanted.contains(held)) {
            self.remove(held.0);
        }
        let missing: Vec<(u32, f64)> = wanted
            .iter()
            .filter(|effect| !old.contains(effect) || !self.holds(effect.0))
            .copied()
            .collect();
        for (crc, potency) in missing {
            let application = Application {
                potency,
                ..Application::plain(holder)
            };
            self.add(rules, crc, application, now_ms);
        }
        self.perk_effects = wanted;
    }

    /// The owner's state of the held effects at own clock `clock`, with `version`: the local and the lightweight maps by key, a map that is empty only when it
    /// was not before (to clear it), the local map at least when both are empty (retail never
    /// sends the one-byte empty state).
    fn owner_state(
        &mut self,
        rules: &StatusEffectRules,
        version: u64,
        clock: EffectClock,
    ) -> StatusEffectsState {
        let mut local = Vec::new();
        let mut lightweight = Vec::new();
        for held in &self.held {
            let Some(rule) = rules.get(held.crc) else {
                continue;
            };
            if rule.lightweight {
                let value = match (rule.client_predicted, held.ends_ms) {
                    (true, Some(ends)) => clock.tick(ends),
                    (true, None) => {
                        (clock.tick(held.added_ms) + TICK_WRAP - NO_END_LEAD_TICKS) % TICK_WRAP
                    }
                    (false, Some(ends)) => unix_ns(ends),
                    (false, None) => 0,
                };
                lightweight.push((held.key, value));
            } else {
                // A potency of a few dozen at most.
                let potency = self.potency(rules, held, rule) as f32;
                local.push((
                    held.key,
                    LocalEffect {
                        end_unix_ns: held.ends_ms.map_or(0, unix_ns),
                        potency,
                        stacks: held.stacks,
                        applications: held.applications,
                    },
                ));
            }
        }
        local.sort_by_key(|(key, _)| *key);
        lightweight.sort_by_key(|(key, _)| *key);
        let send_lightweight = !lightweight.is_empty() || self.lightweight_shown;
        let send_local = !local.is_empty() || self.local_shown || !send_lightweight;
        self.local_shown = !local.is_empty();
        self.lightweight_shown = !lightweight.is_empty();
        StatusEffectsState {
            local: send_local.then(|| EffectMap::full(version, local)),
            lightweight: send_lightweight.then(|| EffectMap::full(version, lightweight)),
            remote: None,
        }
    }

    /// Makes the owner's state anew when its effects changed, with the next version of
    /// `version`, at own clock `clock`.
    pub(super) fn refresh_state(
        &mut self,
        rules: &StatusEffectRules,
        version: &mut u64,
        clock: EffectClock,
    ) {
        if !std::mem::take(&mut self.changed) && self.state.is_some() {
            return;
        }
        *version += 1;
        let state = self.owner_state(rules, *version, clock);
        self.state = Some(state);
        self.state_due = true;
    }

    /// The member of the owner's state for its next bundle: the newest state after a change,
    /// and with the first card (a first state of [`FIRST_VERSION`] when no state was made yet).
    pub(super) fn take_member(&mut self, first: bool) -> Option<BundleMember> {
        let due = std::mem::take(&mut self.state_due);
        if !due && !first {
            return None;
        }
        let state = self.state.clone().unwrap_or_else(|| StatusEffectsState {
            local: Some(EffectMap::full(FIRST_VERSION, Vec::new())),
            ..StatusEffectsState::default()
        });
        Some(BundleMember {
            component: STATUS_EFFECTS_COMPONENT,
            state: MemberState::StatusEffects(state),
        })
    }
}

/// The equip-load effects of `load` that `rules` has.
fn equip_load_effects(rules: &StatusEffectRules, load: EquipLoad) -> Vec<u32> {
    let suffix = match load {
        EquipLoad::Fast => LIGHT_LOAD,
        EquipLoad::Normal => MEDIUM_LOAD,
        EquipLoad::Slow => HEAVY_LOAD,
        EquipLoad::Overburdened => return Vec::new(),
    };
    EQUIP_LOAD_EFFECTS
        .iter()
        .filter_map(|prefix| rules.by_id(&format!("{prefix}{suffix}")))
        .map(|rule| rule.crc)
        .collect()
}

impl SimWorld {
    /// Player `entity` used `consumable` at server time `now_ms`: it holds the effects the
    /// consumable adds, of its potency and duration override.
    pub(super) fn add_consumable_effects(&mut self, entity: Entity, consumable: &Consumable) {
        let now_ms = self.server_ms;
        let rules = &self.content.status_effects;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let application = Application {
            source: player.number,
            origin: Origin::Consumable,
            potency: consumable.potency,
            duration_ms: consumable.duration_override_ms,
            hit_amount: 0.0,
        };
        let number = player.number;
        for crc in &consumable.status_effects {
            let added = player.status_effects.add(rules, *crc, application, now_ms);
            let ends_ms = player.status_effects.ends_ms(*crc);
            tracing::debug!(
                player = number,
                consumable = %consumable.id,
                effect = rules.get(*crc).map_or("", |rule| rule.id.as_str()),
                added,
                now_ms,
                ?ends_ms,
                "consumable effect"
            );
        }
    }

    /// Mob `mob` hit player `entity` for `amount` with the attack row `row`: the player took a
    /// hit, and holds the effects the row puts on its target.
    pub(super) fn mob_hit_effects(
        &mut self,
        entity: Entity,
        mob: u64,
        row: Option<RowKey>,
        amount: u32,
    ) {
        let now_ms = self.server_ms;
        let rules = &self.content.status_effects;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let application = Application {
            hit_amount: f64::from(amount),
            ..Application::plain(mob)
        };
        for crc in row.map_or(&[][..], |row| rules.on_hit(row)) {
            player.status_effects.add(rules, *crc, application, now_ms);
        }
    }

    /// Player `entity` took a hit: its effects that end at a hit count it.
    pub(super) fn player_hit_taken(&mut self, entity: Entity) {
        let rules = &self.content.status_effects;
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.status_effects.hit_taken(rules);
        }
    }

    /// Player `attacker` hit mob `mob` for `amount` with the attack row `row`: the mob took a
    /// hit, and holds the effects the row puts on its target.
    pub(super) fn player_hit_effects(&mut self, mob: u64, attacker: u64, row: RowKey, amount: u32) {
        let now_ms = self.server_ms;
        let rules = &self.content.status_effects;
        let effects = self.mob_effects.entry(mob).or_default();
        effects.hit_taken(rules);
        let application = Application {
            hit_amount: f64::from(amount),
            ..Application::plain(attacker)
        };
        for crc in rules.on_hit(row) {
            effects.add(rules, *crc, application, now_ms);
        }
    }

    /// The `amount` of a hit of damage type `tag` on player `entity` that its effects let
    /// through.
    pub(super) fn player_absorbs(&self, entity: Entity, tag: u8, amount: u32) -> u32 {
        let rules = &self.content.status_effects;
        self.world.get::<Player>(entity).map_or(amount, |player| {
            let through = 1.0 - player.status_effects.absorption(rules, tag);
            // A hit of a few thousand points.
            (f64::from(amount) * through).round() as u32
        })
    }

    /// The multiple the effects of player `entity` give its hits of damage type `tag`, times the
    /// share mob `mob`'s effects let through.
    pub(super) fn player_hit_scale(&self, entity: Entity, mob: u64, tag: u8) -> f64 {
        let rules = &self.content.status_effects;
        let own = self
            .world
            .get::<Player>(entity)
            .map_or(PLAIN_POTENCY, |player| {
                player.status_effects.damage_scale(rules, tag)
            });
        let through = self.mob_effects.get(&mob).map_or(PLAIN_POTENCY, |effects| {
            1.0 - effects.absorption(rules, tag)
        });
        own * through
    }

    /// The multiple the effects of mob `mob` give its hits of damage type `tag`.
    pub(super) fn mob_hit_scale(&self, mob: u64, tag: u8) -> f64 {
        let rules = &self.content.status_effects;
        self.mob_effects
            .get(&mob)
            .map_or(PLAIN_POTENCY, |effects| effects.damage_scale(rules, tag))
    }

    /// Puts the effects the instance's mutation queued ([`SimWorld::take_mutation_effects`]) on
    /// their holders, players and mobs; the mutation applies them, so each is its own source.
    fn add_mutation_effects(&mut self) {
        let queued = self.take_mutation_effects();
        if queued.is_empty() {
            return;
        }
        let now_ms = self.server_ms;
        let content = std::sync::Arc::clone(&self.content);
        let rules = &content.status_effects;
        for (number, effect) in queued {
            let added = match self.player_by_number(number) {
                Some(entity) => self
                    .world
                    .get_mut::<Player>(entity)
                    .is_some_and(|mut player| {
                        player.status_effects.add_by_id(
                            rules,
                            &effect.status_id,
                            effect.potency,
                            number,
                            now_ms,
                        )
                    }),
                None => self.mob_effects.entry(number).or_default().add_by_id(
                    rules,
                    &effect.status_id,
                    effect.potency,
                    number,
                    now_ms,
                ),
            };
            if !added {
                tracing::warn!(
                    number,
                    effect = %effect.status_id,
                    "mutation effect not added"
                );
            }
        }
    }

    /// The status effects of every player and mob at `now_ms`: deaths and new lives, the
    /// out-of-combat effect, the equip-load effects, the changes over time and the ends, then
    /// the owners' states.
    pub(super) fn tick_status_effects(&mut self, now_ms: u64) {
        self.add_mutation_effects();
        let server_ms = self.server_ms;
        let content = std::sync::Arc::clone(&self.content);
        let rules = &content.status_effects;
        let regen = rules.by_id(OUT_OF_COMBAT_REGEN).map(|rule| rule.crc);
        let mut due: Vec<(Entity, u64, Vec<VitalsTick>)> = Vec::new();
        let mut query = self.world.query::<(Entity, &mut Player, &Vitals)>();
        for (entity, mut player, vitals) in query.iter_mut(&mut self.world) {
            let player = &mut *player;
            let dead = vitals.health.is_dead();
            player.status_effects.set_dead(rules, dead);
            if let Some(regen) = regen {
                let in_combat = player.combat.in_combat();
                let hurt = vitals.health.current() < vitals.health.max();
                let holds = player.status_effects.holds(regen);
                if !dead && !in_combat && hurt && !holds {
                    let number = player.number;
                    player
                        .status_effects
                        .add(rules, regen, Application::plain(number), server_ms);
                } else if in_combat && holds {
                    player.status_effects.remove(regen);
                }
            }
            let load = player.gear.as_ref().and_then(|gear| {
                content.equip_load(
                    EQUIP_LOAD_SLOTS
                        .iter()
                        .filter_map(|slot| gear.worn_item(*slot)),
                )
            });
            if let Some(load) = load {
                let number = player.number;
                player
                    .status_effects
                    .set_equip_load(rules, load, number, server_ms);
            }
            let perks = player.gear.as_ref().map_or_else(Vec::new, |gear| {
                perk_effects(&content.perk_effects, rules, &gear.worn_perks())
            });
            let number = player.number;
            player
                .status_effects
                .set_perk_effects(rules, perks, number, server_ms);
            let max_health = f64::from(vitals.health.max());
            let ticks = player.status_effects.tick(rules, server_ms, max_health);
            if !ticks.is_empty() {
                due.push((entity, player.number, ticks));
            }
            let clock = EffectClock {
                spawned_ms: player.own_spawned_ms,
            };
            player
                .status_effects
                .refresh_state(rules, &mut player.list_version, clock);
        }
        for (entity, number, ticks) in due {
            for tick in ticks {
                self.player_vitals_tick(entity, number, tick, now_ms);
            }
        }
        self.tick_mob_effects(now_ms);
    }

    /// Applies the change `tick` of an effect of player `number` (`entity`) at `now_ms`: a heal
    /// and a stamina gain at once; damage as an over-time 3601 (retail flags `08 00`), or as a
    /// hit when it kills, which runs the death.
    fn player_vitals_tick(&mut self, entity: Entity, number: u64, tick: VitalsTick, now_ms: u64) {
        let source_at = self.mobs.get(tick.source).map_or([0.0; 3], |mob| {
            [mob.position.x, mob.position.y, mob.position.z]
        });
        let Some(connection) = self.world.get_mut::<Player>(entity).map(|mut player| {
            // Stamina values of a few dozen.
            player.stamina.gain(tick.stamina as f32);
            player.connection.clone()
        }) else {
            return;
        };
        // Health values of a few thousand.
        let amount = tick.health.abs().round() as u32;
        // The max health after a damage that does not kill; `None` for a kill.
        let survived = {
            let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) else {
                return;
            };
            if vitals.health.is_dead() || amount == 0 {
                return;
            }
            if tick.health > 0.0 {
                Err(vitals.health.heal(amount))
            } else {
                Ok((amount < vitals.health.current()).then(|| {
                    vitals.health.take(amount);
                    vitals.health.max()
                }))
            }
        };
        let Ok(survived) = survived else {
            let healed = survived.unwrap_err();
            if healed > 0 {
                send_now(
                    &mut self.combat_messages,
                    &connection,
                    &[ServerMessage::OnHealed(OnHealedMsg::of(
                        number,
                        number,
                        healed as f32,
                    ))],
                );
            }
            return;
        };
        let Some(max_health) = survived else {
            self.apply_hit(number, tick.source, amount, 0, now_ms);
            return;
        };
        // Health of a few thousand: f32 holds it.
        let max_health = max_health as f32;
        let item = DamageItem {
            tag: tick.damage_type.unwrap_or(HIT_TAG),
            amount: amount as f32,
            scale: NEUTRAL_SCALE,
        };
        let message = OnDamageMsg::over_time(number, tick.source, source_at, item, max_health);
        send_now(
            &mut self.combat_messages,
            &connection,
            &[ServerMessage::OnDamage(message)],
        );
    }

    /// The effects of the mobs at `now_ms`: dead and gone mobs lose theirs; damage over time
    /// lands as hits of the player that applied it, who gets a 2071 with the status-effect flag;
    /// a heal heals the mob.
    fn tick_mob_effects(&mut self, now_ms: u64) {
        let server_ms = self.server_ms;
        let content = std::sync::Arc::clone(&self.content);
        let rules = &content.status_effects;
        let mobs = &self.mobs;
        self.mob_effects.retain(|mob, effects| {
            let alive = mobs.get(*mob).is_some_and(|mob| !mob.health.is_dead());
            alive && !effects.held.is_empty()
        });
        let mut due: Vec<(u64, VitalsTick)> = Vec::new();
        for (mob, effects) in &mut self.mob_effects {
            let max_health = self
                .mobs
                .get(*mob)
                .map_or(0.0, |mob| f64::from(mob.health.max()));
            for tick in effects.tick(rules, server_ms, max_health) {
                due.push((*mob, tick));
            }
        }
        for (mob, tick) in due {
            // Health values of a few thousand.
            let amount = tick.health.abs().round() as u32;
            if amount == 0 {
                continue;
            }
            if tick.health > 0.0 {
                if let Some(mob) = self.mobs.get_mut(mob) {
                    mob.health.heal(amount);
                }
                continue;
            }
            let outcome = self.apply_hit(mob, tick.source, amount, 0, now_ms);
            let killed = matches!(outcome, Some(AttackOutcome::Killed { .. }));
            let Some(attacker) = self.player_by_number(tick.source) else {
                continue;
            };
            let Some(connection) = self
                .world
                .get::<Player>(attacker)
                .map(|player| player.connection.clone())
            else {
                continue;
            };
            let item = DamageItem {
                tag: tick.damage_type.unwrap_or(HIT_TAG),
                // A hit of a few thousand points.
                amount: amount as f32,
                scale: NEUTRAL_SCALE,
            };
            send_now(
                &mut self.combat_messages,
                &connection,
                &[ServerMessage::DamageDealt(DamageDealtMsg::over_time(
                    tick.source,
                    mob,
                    item,
                    killed,
                ))],
            );
        }
    }
}

/// The effects of the worn `perks` (perk id, its item's gear score) under `perk_effects`, by
/// effect: one entry per effect, its potency the sum over the perks that hold it (Inferred: two
/// worn items with one perk hold it once, twice as strong).
fn perk_effects(
    perk_effects: &PerkEffects,
    rules: &StatusEffectRules,
    perks: &[(&str, u32)],
) -> Vec<(u32, f64)> {
    let mut held: Vec<(u32, f64)> = Vec::new();
    for (perk, gear_score) in perks {
        let Some(effect) = perk_effects.of(perk) else {
            continue;
        };
        let Some(rule) = rules.by_id(&effect.status_effect) else {
            continue;
        };
        let potency = effect.potency(*gear_score);
        match held.iter_mut().find(|(crc, _)| *crc == rule.crc) {
            Some((_, sum)) => *sum += potency,
            None => held.push((rule.crc, potency)),
        }
    }
    held.sort_by_key(|(crc, _)| *crc);
    held
}

#[cfg(test)]
mod tests {
    use nw_content::status_effects::{EffectSourceRow, StatusEffectSource};

    use super::*;

    const BLEED: &str = "AI_Bleed_9";
    const REGEN: &str = OUT_OF_COMBAT_REGEN;
    const FOOD: &str = "FoodRecoveryT5";
    const STUN: &str = "AI_Stun_2sec";
    const IMMUNE: &str = "Status_Immune_Stun";
    const BUFF: &str = "Status_Passive_Lifestaff_Healing_HealingBuffonBasic";
    const SPRINT: &str = "Sprint_Delay_Timer";
    const LOAD: &str = "EquipLoad_CCDurReduction_Heavy";
    const MOB: u64 = (1 << 34) + 7;
    const PLAYER: u64 = 3;
    const START_MS: u64 = 1_000_000;
    const MAX_HEALTH: f64 = 2_000.0;
    const HIT: f64 = 300.0;

    fn row(id: &str) -> EffectSourceRow {
        EffectSourceRow {
            status_id: id.to_owned(),
            ..EffectSourceRow::default()
        }
    }

    /// Rows of the retail tables (build 25810808), trimmed to the columns the rules read.
    fn rules() -> StatusEffectRules {
        StatusEffectRules::build(StatusEffectSource {
            effects: vec![
                EffectSourceRow {
                    base_duration_s: Some(10.0),
                    tick_s: Some(2.0),
                    stack_max: Some(1),
                    categories: Some("Debuff+Bleed+Dot".to_owned()),
                    health_share: Some(-0.015),
                    hit_share: Some(-0.05),
                    damage_type: Some("Standard".to_owned()),
                    remove_on_death: true,
                    ..row(BLEED)
                },
                EffectSourceRow {
                    base_duration_s: Some(180.0),
                    delay_s: Some(1.0),
                    tick_s: Some(1.0),
                    health: Some(40.0),
                    potency_mods: Some("PassiveFoodRegen=2".to_owned()),
                    stop_on_hit_count: Some(1),
                    hit_condition: Some("OnHitTaken".to_owned()),
                    remove_on_death: true,
                    remove_on_respawn: true,
                    ..row(REGEN)
                },
                EffectSourceRow {
                    base_duration_s: Some(1200.0),
                    tick_s: Some(2.0),
                    categories: Some("Recovery+PassiveFoodRegen+FoodUtility+Food".to_owned()),
                    health_share: Some(0.01),
                    ..row(FOOD)
                },
                EffectSourceRow {
                    base_duration_s: Some(2.0),
                    categories: Some("Debuff+Stun+CC".to_owned()),
                    antirequisite_categories: Some("StunImmunity".to_owned()),
                    remove_on_death: true,
                    ..row(STUN)
                },
                EffectSourceRow {
                    base_duration_s: Some(5.0),
                    categories: Some("StunImmunity".to_owned()),
                    remove_categories: Some("Stun".to_owned()),
                    ..row(IMMUNE)
                },
                EffectSourceRow {
                    base_duration_s: Some(10.0),
                    duration_max_s: Some(15.0),
                    stack_max: Some(3),
                    potency_max: Some(3.0),
                    refresh_duration: Some(true),
                    ..row(BUFF)
                },
                EffectSourceRow {
                    base_duration_s: Some(0.85),
                    lightweight: true,
                    client_predicted: true,
                    ..row(SPRINT)
                },
                EffectSourceRow {
                    base_duration_s: Some(-1.0),
                    duration_mods: Some("Stun=-0.2".to_owned()),
                    lightweight: true,
                    client_predicted: true,
                    ..row(LOAD)
                },
            ],
            damage_types: vec![("Standard".to_owned(), 1)],
            hit_effects: Vec::new(),
        })
    }

    fn crc(rules: &StatusEffectRules, id: &str) -> u32 {
        rules.by_id(id).expect("rule").crc
    }

    fn hit_by(source: u64) -> Application {
        Application {
            hit_amount: HIT,
            ..Application::plain(source)
        }
    }

    #[test]
    fn held_tradeskill_effects_add_gather_speed_and_craft_gear_score() {
        // Retail rows `MusicBuff_GatheringSpeed_T4_Amazing` (`EFF_Mining` 0.75, also Logging,
        // Harvesting, Skinning) and `FoodArmorerT5` (`MGS_Armoring` and `MaxGS_Armoring` 15).
        const SPEED: &str = "MusicBuff_GatheringSpeed_T4_Amazing";
        const MEAL: &str = "FoodArmorerT5";
        let rules = StatusEffectRules::build(StatusEffectSource {
            effects: vec![
                EffectSourceRow {
                    base_duration_s: Some(5400.0),
                    gather_efficiency: vec![("Mining".to_owned(), 0.75)],
                    ..row(SPEED)
                },
                EffectSourceRow {
                    base_duration_s: Some(2400.0),
                    craft_gear_score: vec![("Armoring".to_owned(), 15, 15)],
                    ..row(MEAL)
                },
            ],
            ..StatusEffectSource::default()
        });
        let mut effects = StatusEffects::default();
        assert_eq!(effects.gather_efficiency(&rules, "Mining"), 0.0);
        effects.add(
            &rules,
            crc(&rules, SPEED),
            Application::plain(PLAYER),
            START_MS,
        );
        effects.add(
            &rules,
            crc(&rules, MEAL),
            Application::plain(PLAYER),
            START_MS,
        );
        assert!((effects.gather_efficiency(&rules, "mining") - 0.75).abs() < f64::EPSILON);
        assert_eq!(effects.gather_efficiency(&rules, "Logging"), 0.0);
        assert_eq!(effects.craft_gear_score(&rules, "Armoring"), (15.0, 15.0));
        assert_eq!(effects.craft_gear_score(&rules, "Arcana"), (0.0, 0.0));
    }

    #[test]
    fn worn_perks_hold_their_effects_scaled_by_gear_score_and_keep_them_over_a_death() {
        use nw_content::perk_effects::{PerkEffect, PerkEffects};
        // Build 25810808: `PerkID_Tools_Efficiency_Mining` holds
        // `Status_Perk_Tools_Efficiency_Mining` (an `EFF_Mining`), scale 7.27E-3 per gear score.
        const PERK: &str = "PerkID_Tools_Efficiency_Mining";
        const EFFECT: &str = "Status_Perk_Tools_Efficiency_Mining";
        const EFFICIENCY: f64 = 0.1;
        const SCALE: &str = "7.2727272727272727E-3";
        const GEAR_SCORE: u32 = 210;
        let rules = StatusEffectRules::build(StatusEffectSource {
            effects: vec![EffectSourceRow {
                gather_efficiency: vec![("Mining".to_owned(), EFFICIENCY)],
                remove_on_death: true,
                ..row(EFFECT)
            }],
            ..StatusEffectSource::default()
        });
        let perks = PerkEffects::build([(PERK.to_owned(), PerkEffect::new(EFFECT, Some(SCALE)))]);
        let scale: f64 = SCALE.parse().expect("scale");
        let potency = 1.0 + scale * f64::from(GEAR_SCORE - 100);
        let wanted = perk_effects(&perks, &rules, &[(PERK, GEAR_SCORE), ("PerkID_Other", 600)]);
        assert_eq!(wanted.len(), 1, "a perk without an effect holds none");
        assert!((wanted[0].1 - potency).abs() < 1e-9);
        let mut effects = StatusEffects::default();
        effects.set_perk_effects(&rules, wanted.clone(), PLAYER, START_MS);
        let gathers = |effects: &StatusEffects| effects.gather_efficiency(&rules, "Mining");
        assert!((gathers(&effects) - EFFICIENCY * potency).abs() < 1e-9);
        effects.set_dead(&rules, true);
        effects.set_dead(&rules, false);
        effects.set_perk_effects(&rules, wanted, PLAYER, START_MS);
        assert!(
            (gathers(&effects) - EFFICIENCY * potency).abs() < 1e-9,
            "back after the death"
        );
        effects.set_perk_effects(&rules, Vec::new(), PLAYER, START_MS);
        assert_eq!(gathers(&effects), 0.0, "the tool came off");
    }

    #[test]
    fn a_bleed_ticks_its_share_of_the_max_health_and_the_hit_until_it_ends() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        assert!(effects.add(&rules, crc(&rules, BLEED), hit_by(MOB), START_MS));
        let first = effects.tick(&rules, START_MS, MAX_HEALTH);
        let expected = -0.015 * MAX_HEALTH - 0.05 * HIT;
        assert_eq!(
            first,
            vec![VitalsTick {
                source: MOB,
                health: expected,
                stamina: 0.0,
                damage_type: Some(1),
            }],
            "no DelayInitialTick: the first change at the application"
        );
        let all = effects.tick(&rules, START_MS + 10_000, MAX_HEALTH);
        assert_eq!(all.len(), 5, "every 2 s up to its end at 10 s");
        assert!(!effects.holds(crc(&rules, BLEED)), "it ended");
    }

    #[test]
    fn an_immunity_blocks_and_removes_the_stun() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        let stun = crc(&rules, STUN);
        assert!(effects.add(&rules, stun, hit_by(MOB), START_MS));
        assert!(effects.add(
            &rules,
            crc(&rules, IMMUNE),
            Application::plain(PLAYER),
            START_MS
        ));
        assert!(!effects.holds(stun), "RemoveStatusEffectCategories Stun");
        assert!(
            !effects.add(&rules, stun, hit_by(MOB), START_MS),
            "AntirequisiteEffectCategories"
        );
    }

    #[test]
    fn stacks_rise_to_the_max_and_applications_go_on() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        let buff = crc(&rules, BUFF);
        for _ in 0..6 {
            effects.add(&rules, buff, Application::plain(PLAYER), START_MS);
        }
        let held = &effects.held[0];
        assert_eq!((held.stacks, held.applications), (3, 6));
        let rule = rules.get(buff).expect("rule");
        assert_eq!(effects.potency(&rules, held, rule), 3.0);
    }

    #[test]
    fn the_out_of_combat_effect_triples_the_food_and_ends_at_a_hit() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        let food = crc(&rules, FOOD);
        effects.add(&rules, food, Application::plain(PLAYER), START_MS);
        let alone = effects.potency(&rules, &effects.held[0], rules.get(food).expect("food"));
        effects.add(
            &rules,
            crc(&rules, REGEN),
            Application::plain(PLAYER),
            START_MS,
        );
        let with_regen = effects.potency(&rules, &effects.held[0], rules.get(food).expect("food"));
        assert_eq!((alone, with_regen), (1.0, 3.0), "PassiveFoodRegen=2");
        effects.hit_taken(&rules);
        assert!(!effects.holds(crc(&rules, REGEN)));
        assert!(effects.holds(food));
    }

    #[test]
    fn a_consumable_effect_changes_no_vitals_here() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        let application = Application {
            origin: Origin::Consumable,
            ..Application::plain(PLAYER)
        };
        effects.add(&rules, crc(&rules, FOOD), application, START_MS);
        assert!(
            effects
                .tick(&rules, START_MS + 10_000, MAX_HEALTH)
                .is_empty()
        );
    }

    #[test]
    fn a_death_ends_the_effects_that_end_at_death_and_a_new_life_those_of_respawn() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        effects.add(&rules, crc(&rules, BLEED), hit_by(MOB), START_MS);
        effects.add(
            &rules,
            crc(&rules, FOOD),
            Application::plain(PLAYER),
            START_MS,
        );
        effects.set_dead(&rules, true);
        assert!(!effects.holds(crc(&rules, BLEED)));
        assert!(effects.holds(crc(&rules, FOOD)), "no RemoveOnDeath");
        assert!(
            !effects.add(&rules, crc(&rules, BLEED), hit_by(MOB), START_MS),
            "dead"
        );
    }

    #[test]
    fn the_equip_load_shortens_a_stun_by_its_duration_mod() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        effects.add(
            &rules,
            crc(&rules, LOAD),
            Application::plain(PLAYER),
            START_MS,
        );
        effects.add(&rules, crc(&rules, STUN), hit_by(MOB), START_MS);
        let stun = &effects.held[effects.index(crc(&rules, STUN)).expect("stun")];
        assert_eq!(stun.ends_ms, Some(START_MS + 1_600), "2 s x (1 - 0.2)");
    }

    #[test]
    fn the_owner_state_puts_predicted_timers_in_ticks_and_others_in_unix_ns() {
        let rules = rules();
        let mut effects = StatusEffects::default();
        let clock = EffectClock {
            spawned_ms: Some(START_MS),
        };
        effects.add(
            &rules,
            crc(&rules, LOAD),
            Application::plain(PLAYER),
            START_MS,
        );
        let mut version = 0;
        effects.refresh_state(&rules, &mut version, clock);
        let member = effects.take_member(false).expect("due");
        let MemberState::StatusEffects(state) = member.state else {
            panic!("status effects member");
        };
        assert_eq!(
            state.local, None,
            "only the lightweight map, as retail at the spawn"
        );
        assert_eq!(
            state.lightweight,
            Some(EffectMap::full(
                1,
                vec![(rules.by_id(LOAD).expect("load").key, 65_507)]
            )),
            "retail: tick 1 less 30"
        );
        effects.add(
            &rules,
            crc(&rules, SPRINT),
            Application::plain(PLAYER),
            START_MS + 1_000,
        );
        effects.add(&rules, crc(&rules, BLEED), hit_by(MOB), START_MS + 1_000);
        effects.refresh_state(&rules, &mut version, clock);
        let MemberState::StatusEffects(state) = effects.take_member(false).expect("due").state
        else {
            panic!("status effects member");
        };
        let Some(EffectMap::Full(lightweight)) = state.lightweight else {
            panic!("lightweight map");
        };
        let sprint_key = rules.by_id(SPRINT).expect("sprint").key;
        assert!(
            lightweight.elements.contains(&(
                sprint_key,
                SPAWN_TICK + 1_850 * TICKS_PER_SECOND / MS_PER_SECOND
            )),
            "the end tick"
        );
        let Some(EffectMap::Full(local)) = state.local else {
            panic!("local map");
        };
        assert_eq!(local.elements.len(), 1);
        assert_eq!(local.elements[0].1.end_unix_ns, unix_ns(START_MS + 11_000));
        assert!(effects.take_member(false).is_none(), "sent once");
        assert!(
            effects.take_member(true).is_some(),
            "again with the first card"
        );
    }

    #[test]
    fn an_effect_by_its_id_is_held_and_an_unknown_id_is_not() {
        const MUTATION_POTENCY: f64 = 2.0;
        const SOURCE: u64 = 7;
        let rules = rules();
        let mut effects = StatusEffects::default();
        assert!(effects.add_by_id(&rules, BLEED, MUTATION_POTENCY, SOURCE, 0));
        assert!(effects.holds(crc(&rules, BLEED)));
        assert!(!effects.add_by_id(&rules, "NotAnEffect", MUTATION_POTENCY, SOURCE, 0));
    }
}
