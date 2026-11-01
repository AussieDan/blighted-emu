// SPDX-License-Identifier: AGPL-3.0-or-later
//! Weapon abilities (abilities design note and ability-use-wire note, 2026-10-31): the server runs
//! the ability list of the weapon in the hand as the client does. A press of ability key 1, 2 or 3
//! (input 402, 115, 139) names the ability mapped to that key; the list's actions start while
//! their activation holds and stop when their deactivation holds, tested against the player
//! ([`CageWorld`]). The action that tests the pressed ability starts a chain; its follow-ups (the
//! actions that test what the chain runs) belong to the same chain, and each action starts at
//! most once per chain. A running action starts its cooldowns and applies its status effects
//! when it starts, and casts each of its spells once, when the cast's branch conditions hold;
//! a cast spends its spell's mana (retail: the cost is the `SpellData` `ManaCost`).
//!
//! Observed (general-other-01): the cooldown change (2932 `cdmapN`, N the pressed key) and the
//! mana spend go out in one bundle; mana comes back at 3.259 per second in steps every 50 ms,
//! with no pause after a spend. Inferred: the cooldown starts when its action starts (retail
//! sends it at the cast, 160 to 935 ms after the press).

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::attribute_bonuses::AbilityStats;
use nw_content::player_abilities::{AbilityList, SpellProjectile, SpellRow};
use nw_content::player_attacks::stance_tags;
use nw_content::status_effects::StatusEffectRules;
#[cfg(test)]
use nw_formats::mannequin::Condition;
use nw_formats::mannequin::{CageWorld, CastKind};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::{AbilityCooldown, InputRecord, LookDir};
use nw_sim_core::Position;

use super::gear::Gear;
use super::hit_shape::PLAYER_CAPSULE;
use super::projectile::{AimLine, Shot, ShotHit, first_on_line, flight_ms};
use super::spell_areas::SpellArea;
use super::{Player, SimWorld};
use crate::combat::{AttackOutcome, AttackRefusal};
use crate::mobs::Capsule;

/// Mana back per second at Focus 250 (Observed, general-other-01 and target-lock-test).
const RETAIL_MANA_REGEN_PER_S: f32 = 3.259;
/// The `ManaRate` share that Focus 250 holds: `Foc_Bonus_100_1` (0.2).
const RETAIL_MANA_RATE_SHARE: f32 = 0.2;
/// Mana back per second without a `ManaRate` share: the retail rate less its share (Inferred:
/// `ManaBaseRegenRate` is 2, the rule that gives the rest is Unknown).
pub(super) const MANA_REGEN_PER_S: f32 = RETAIL_MANA_REGEN_PER_S / (1.0 + RETAIL_MANA_RATE_SHARE);
/// `VitalsBaseData` row `Player`: `ManaBaseMax`.
pub(super) const MANA_MAX: f32 = 100.0;
/// Retail sends the mana every 50 ms while it changes (Observed).
const MANA_STEP_MS: u64 = 50;
/// How long a pressed key names its ability: `IfActiveAbilityMoveName BackTicks="20"`, 20 client
/// frames of about 34 ms (input note: 34 ms median; Inferred).
const PRESS_WINDOW_MS: u64 = 680;
/// Milliseconds per second.
const MS_PER_S: f32 = 1_000.0;
/// Microseconds per millisecond.
const US_PER_MS: u64 = 1_000;
/// Tests the server takes as passing for a standing player with a weapon in the hand, by element
/// name (Inferred: the client's character fits its controller, the weapon its requirements, and
/// the client checks its own aim point, which it does not send).
const PASSING_TESTS: [&str; 3] = [
    "IfCanResizeCharacterController",
    "IfHasWeaponPrerequisites",
    "ActionConditionSpellSpawnLocationState",
];

/// Status effects an ability puts on a player are of this potency (the data's own).
const OWN_EFFECT_POTENCY: f64 = 1.0;
/// `TargetTypes` that take enemies, and those that take friends.
pub(super) const ENEMY_TARGETS: [&str; 2] = ["Enemy", "All"];
pub(super) const FRIEND_TARGETS: [&str; 4] = ["Friend", SELF_TARGET, "Group", "All"];
/// The `TargetTypes` of a spell on its caster only.
pub(super) const SELF_TARGET: &str = "Self";

/// What the action list reads from the player besides the run.
pub(super) trait Holder {
    /// Its Mannequin tags (the weapon stance's).
    fn tags(&self) -> Vec<&str>;
    /// It has status effect `effect`.
    fn has_effect(&self, effect: &str) -> bool;
    /// An item is in slot `slot` (`ACTIVE_MAIN_HAND_WEAPON`, ...).
    fn item_in_slot(&self, slot: &str) -> bool;
    /// Its passive ability or upgrade `ability` is on.
    fn ability_active(&self, ability: &str) -> bool;
    /// What its abilities that are on add to its mana and cooldowns.
    fn stats(&self) -> AbilityStats;
}

/// A press of an ability key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Press {
    /// The key: 0, 1 or 2.
    key: usize,
    /// CRC-32 of the lowercase id of the ability mapped to it.
    ability: u32,
    /// When it came.
    at_ms: u64,
    /// It started a chain: it still names its ability to the chain's tests for its window, but
    /// starts no other chain.
    used: bool,
}

/// An action that runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    /// Its index in the list.
    action: usize,
    /// When it started.
    started_ms: u64,
}

/// A running cooldown.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cooldown {
    /// CRC-32 of the lowercase ability id.
    ability: u32,
    /// When it ends.
    end_ms: u64,
}

/// What a tick of the run asks the world to do.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum AbilityEvent {
    /// Cast `spell` (aimed as `kind`) for action `action`.
    Cast {
        /// The action.
        action: String,
        /// The spell.
        spell: Box<SpellRow>,
        /// How it aims.
        kind: CastKind,
    },
    /// Apply the status effects `effects` to the player.
    Effects(Vec<String>),
}

/// A player's mana.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Mana {
    /// Now.
    pub current: f32,
    /// Its max.
    pub max: f32,
    /// The value the client has; `None` before the first send.
    sent: Option<f32>,
    /// The time of its last regen step.
    regen_ms: Option<u64>,
    /// Mana back per second.
    regen_per_s: f32,
}

impl Default for Mana {
    fn default() -> Self {
        Self {
            current: MANA_MAX,
            max: MANA_MAX,
            sent: None,
            regen_ms: None,
            regen_per_s: MANA_REGEN_PER_S,
        }
    }
}

impl Mana {
    /// Sets the max and the regen to the shares of `stats` (`MaxMana`, `ManaRate`) of the base;
    /// the mana above a lower max is gone.
    fn set_stats(&mut self, stats: AbilityStats) {
        // Shares of a few tenths.
        #[allow(clippy::cast_possible_truncation)]
        let (max_share, rate_share) = (stats.max_mana as f32, stats.mana_rate as f32);
        self.max = MANA_MAX * (1.0 + max_share);
        self.regen_per_s = MANA_REGEN_PER_S * (1.0 + rate_share);
        self.current = self.current.min(self.max);
    }

    /// Gives back mana for the steps of [`MANA_STEP_MS`] since the last one, up to the max.
    fn regen(&mut self, now_ms: u64) {
        let from = *self.regen_ms.get_or_insert(now_ms);
        let steps = now_ms.saturating_sub(from) / MANA_STEP_MS;
        if steps == 0 {
            return;
        }
        self.regen_ms = Some(from + steps * MANA_STEP_MS);
        // A count of 50 ms steps is small.
        #[allow(clippy::cast_precision_loss)]
        let back = self.regen_per_s * (steps * MANA_STEP_MS) as f32 / MS_PER_S;
        self.current = (self.current + back).min(self.max);
    }

    /// The mana to send when it changed since the last send.
    pub fn take_change(&mut self) -> Option<f32> {
        (self.sent != Some(self.current)).then(|| {
            self.sent = Some(self.current);
            self.current
        })
    }
}

/// The ability state of one player.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct AbilityRun {
    /// The newest input frame read.
    last_frame: Option<u16>,
    /// The newest ability key press.
    press: Option<Press>,
    /// The CAGE names of the inputs the newest input message holds, in any of its new frames.
    inputs_on: Vec<String>,
    /// The running actions.
    running: Vec<Running>,
    /// The chain each action started in last, by action index.
    started_in: Vec<Option<u64>>,
    /// The current chain.
    chain: u64,
    /// The spells the current chain cast. A chain casts each spell once: its list may cast one
    /// spell in two aim forms and in two actions (Sacred Ground: `CastSpellTargeting` while the
    /// staff readies, `CastSpellRaycast` once it is ready, and its loop action), and retail
    /// spends the mana and spawns the spell once per press (ability-use-wire note).
    chain_cast: Vec<String>,
    /// The running cooldowns.
    cooldowns: Vec<Cooldown>,
    /// The cooldowns started since the last send, with the key of their press.
    cooldown_changes: Vec<(usize, AbilityCooldown)>,
    /// The mana.
    pub mana: Mana,
}

/// The tests of a list, for the run of one player and the action `current` that tests.
struct RunWorld<'a> {
    run: &'a AbilityRun,
    list: &'a AbilityList,
    holder: &'a dyn Holder,
    tags: Vec<&'a str>,
    current: Option<usize>,
    now_ms: u64,
}

impl RunWorld<'_> {
    /// How long action `index` runs, when it runs.
    fn elapsed(&self, index: usize) -> Option<u64> {
        self.run
            .running
            .iter()
            .find(|running| running.action == index)
            .map(|running| self.now_ms.saturating_sub(running.started_ms))
    }

    /// The length of action `index`'s fragment, ms.
    fn length(&self, index: usize) -> u64 {
        self.list.actions.get(index).map_or(0, |action| {
            self.list
                .fragment(&action.fragment)
                .map_or(0, |fragment| fragment.length_ms)
        })
    }

    /// The running actions that play fragment `fragment` (any case), with how long they run.
    fn playing(&self, fragment: Option<&str>) -> Vec<(usize, u64)> {
        let wanted = |index: usize| match fragment {
            None => Some(index) == self.current,
            Some(name) => self
                .list
                .actions
                .get(index)
                .is_some_and(|action| action.fragment.eq_ignore_ascii_case(name)),
        };
        self.run
            .running
            .iter()
            .filter(|running| wanted(running.action))
            .map(|running| {
                (
                    running.action,
                    self.now_ms.saturating_sub(running.started_ms),
                )
            })
            .collect()
    }
}

impl CageWorld for RunWorld<'_> {
    fn tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|own| own.eq_ignore_ascii_case(tag))
    }

    fn marker(&self, marker: &str) -> bool {
        self.run.running.iter().any(|running| {
            let Some(action) = self.list.actions.get(running.action) else {
                return false;
            };
            self.list
                .fragment(&action.fragment)
                .is_some_and(|fragment| {
                    fragment
                        .markers_at(self.now_ms.saturating_sub(running.started_ms))
                        .contains(&marker)
                })
        })
    }

    fn input(&self, input: &str) -> bool {
        self.run
            .inputs_on
            .iter()
            .any(|own| own.eq_ignore_ascii_case(input))
    }

    fn action_status(&self, action: &str, installed: bool) -> bool {
        installed
            && self
                .list
                .actions
                .iter()
                .position(|own| own.name == action)
                .is_some_and(|index| self.elapsed(index).is_some())
    }

    fn fragment_playing(&self, fragment: Option<&str>) -> bool {
        self.playing(fragment)
            .into_iter()
            .any(|(index, elapsed)| elapsed < self.length(index))
    }

    fn fragment_done(&self, fragment: Option<&str>, exit_offset_ms: u32) -> bool {
        self.playing(fragment)
            .into_iter()
            .any(|(index, elapsed)| elapsed + u64::from(exit_offset_ms) >= self.length(index))
    }

    fn ability_move(&self, ability: &str) -> bool {
        self.run.press.is_some_and(|press| {
            press.ability == lowercase_crc32(ability)
                && self.now_ms.saturating_sub(press.at_ms) <= PRESS_WINDOW_MS
        })
    }

    fn cooldown(&self, ability: &str) -> bool {
        let crc = lowercase_crc32(ability);
        self.run
            .cooldowns
            .iter()
            .any(|cooldown| cooldown.ability == crc && cooldown.end_ms > self.now_ms)
    }

    fn mana_cost(&self, cost: &str) -> bool {
        self.run.mana.current >= self.list.mana_cost(cost).unwrap_or_default()
    }

    fn status_effect(&self, effect: &str) -> bool {
        self.holder.has_effect(effect)
    }

    fn item_in_slot(&self, slot: &str) -> bool {
        self.holder.item_in_slot(slot)
    }

    fn ability_active(&self, ability: &str) -> bool {
        self.holder.ability_active(ability)
    }

    fn other(&self, test: &str) -> bool {
        PASSING_TESTS.contains(&test)
    }
}

impl AbilityRun {
    /// Reads the presses of the ability keys in `records` (newest first, older frames repeated:
    /// each frame id once, oldest first); the newest names the ability that `mapped` maps to its
    /// key (CRC-32 of its lowercase id, 0 for an empty slot). The CAGE inputs on are those that
    /// `inputs_of` names in the new frames.
    pub fn press(
        &mut self,
        records: &[InputRecord],
        mapped: impl Fn(usize) -> Option<u32>,
        inputs_of: impl Fn(&InputRecord) -> Vec<String>,
        now_ms: u64,
    ) {
        self.inputs_on.clear();
        for record in records.iter().rev() {
            let frame = record.u16_after_mask;
            if self
                .last_frame
                .is_some_and(|last| !super::attack::counter_is_newer(frame, last))
            {
                continue;
            }
            self.last_frame = Some(frame);
            for input in inputs_of(record) {
                if !self.inputs_on.contains(&input) {
                    self.inputs_on.push(input);
                }
            }
            let Some(key) = record.ability_key() else {
                continue;
            };
            if let Some(ability) = mapped(key).filter(|ability| *ability != 0) {
                self.press = Some(Press {
                    key,
                    ability,
                    at_ms: now_ms,
                    used: false,
                });
            }
        }
    }

    /// One tick of the list `list` at `now_ms` (`server_ms`: the same time on the server clock,
    /// ms since 2000-01-01): the mana comes back, actions stop and start, and the running ones
    /// cast; gives what the world must do.
    pub fn tick(
        &mut self,
        list: &AbilityList,
        holder: &dyn Holder,
        now_ms: u64,
        server_ms: u64,
    ) -> Vec<AbilityEvent> {
        let stats = holder.stats();
        self.mana.set_stats(stats);
        self.mana.regen(now_ms);
        self.started_in.resize(list.actions.len(), None);
        let tags = holder.tags();
        let stops: Vec<usize> = self
            .running
            .iter()
            .map(|running| running.action)
            .filter(|index| {
                let world = self.world(list, holder, &tags, Some(*index), now_ms);
                match list.actions[*index].deactivation.as_ref() {
                    Some(condition) => condition.holds_in(&world),
                    None => world.fragment_done(None, 0),
                }
            })
            .collect();
        self.running
            .retain(|running| !stops.contains(&running.action));
        let mut events = Vec::new();
        for index in 0..list.actions.len() {
            let action = &list.actions[index];
            let Some(activation) = action.activation.as_ref() else {
                continue;
            };
            let starts_chain = action
                .ability
                .as_ref()
                .is_some_and(|ability| ability.move_name.is_some());
            let in_chain = starts_chain || action.ability.is_some() || !action.casts.is_empty();
            if !in_chain || self.running.iter().any(|running| running.action == index) {
                continue;
            }
            if starts_chain && self.press.is_none_or(|press| press.used) {
                continue;
            }
            let chain = if starts_chain {
                self.chain + 1
            } else {
                self.chain
            };
            if self.started_in[index] == Some(chain) || (!starts_chain && self.chain == 0) {
                continue;
            }
            if !activation.holds_in(&self.world(list, holder, &tags, Some(index), now_ms)) {
                continue;
            }
            if starts_chain {
                self.chain = chain;
                self.chain_cast.clear();
            }
            self.started_in[index] = Some(chain);
            self.running.push(Running {
                action: index,
                started_ms: now_ms,
            });
            if let Some(ability) = &action.ability {
                for id in &ability.starts_cooldowns {
                    self.start_cooldown(list, id, stats, (now_ms, server_ms));
                }
                if !ability.applies_on_enter.is_empty() {
                    events.push(AbilityEvent::Effects(ability.applies_on_enter.clone()));
                }
            }
            if starts_chain && let Some(press) = self.press.as_mut() {
                press.used = true;
            }
        }
        let mut casts = Vec::new();
        for running in &self.running {
            let action = &list.actions[running.action];
            let world = self.world(list, holder, &tags, Some(running.action), now_ms);
            for (cast_index, cast) in action.casts.iter().enumerate() {
                let spell = &cast.spell.id;
                let cast_before = self.chain_cast.contains(spell)
                    || casts.iter().any(|(own, index): &(usize, usize)| {
                        list.actions[*own].casts[*index].spell.id == *spell
                    });
                if !cast_before && cast.condition.holds_in(&world) {
                    casts.push((running.action, cast_index));
                }
            }
        }
        for (action_index, cast_index) in casts {
            let cast = &list.actions[action_index].casts[cast_index];
            self.chain_cast.push(cast.spell.id.clone());
            let cost = cast.spell.mana_cost.unwrap_or_default();
            if self.mana.current < cost {
                continue;
            }
            self.mana.current -= cost;
            events.push(AbilityEvent::Cast {
                action: list.actions[action_index].name.clone(),
                spell: Box::new(cast.spell.clone()),
                kind: cast.kind,
            });
        }
        events
    }

    fn world<'a>(
        &'a self,
        list: &'a AbilityList,
        holder: &'a dyn Holder,
        tags: &[&'a str],
        current: Option<usize>,
        now_ms: u64,
    ) -> RunWorld<'a> {
        let mut all_tags = tags.to_vec();
        for running in &self.running {
            all_tags.extend(
                list.actions[running.action]
                    .sets_tags
                    .iter()
                    .map(String::as_str),
            );
        }
        RunWorld {
            run: self,
            list,
            holder,
            tags: all_tags,
            current,
            now_ms,
        }
    }

    /// Starts the cooldown of ability `id` (its `Cooldowns_Player` time times 1 plus the
    /// `CooldownTimer` share of `stats`) for the key of the newest press, at `now_ms` and
    /// `server_ms`.
    fn start_cooldown(
        &mut self,
        list: &AbilityList,
        id: &str,
        stats: AbilityStats,
        (now_ms, server_ms): (u64, u64),
    ) {
        let Some(table_ms) = list.cooldown_ms(id) else {
            return;
        };
        // A cooldown of some seconds; a share of a few tenths.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let time_ms = (table_ms as f64 * (1.0 + stats.cooldown).max(0.0)).round() as u64;
        let ability = lowercase_crc32(id);
        self.cooldowns
            .retain(|cooldown| cooldown.ability != ability && cooldown.end_ms > now_ms);
        self.cooldowns.push(Cooldown {
            ability,
            end_ms: now_ms + time_ms,
        });
        let key = self.press.map_or(0, |press| press.key);
        let start_us = server_ms.saturating_mul(US_PER_MS);
        self.cooldown_changes.push((
            key,
            AbilityCooldown {
                ability,
                end_us: start_us + time_ms * US_PER_MS,
                start_us,
            },
        ));
    }

    /// The cooldowns started since the last call, with the key (0, 1 or 2) of their press.
    pub fn take_cooldown_changes(&mut self) -> Vec<(usize, AbilityCooldown)> {
        std::mem::take(&mut self.cooldown_changes)
    }

    /// Whether `condition` holds for the player now (for the world's own checks).
    #[cfg(test)]
    fn holds(
        &self,
        list: &AbilityList,
        holder: &dyn Holder,
        condition: &Condition,
        now_ms: u64,
    ) -> bool {
        let tags = holder.tags();
        condition.holds_in(&self.world(list, holder, &tags, None, now_ms))
    }
}

/// The ability that a key whose mapped abilities are `mapped` (CRC-32 of lowercase ids) starts
/// in the lists `lists`: the first that one of their actions tests (`IfActiveAbilityMoveName`).
fn ability_of_key(lists: &[&AbilityList], mut mapped: impl Iterator<Item = u32>) -> Option<u32> {
    mapped.find(|crc| {
        lists
            .iter()
            .flat_map(|list| &list.actions)
            .filter_map(|action| action.ability.as_ref()?.move_name.as_deref())
            .any(|name| lowercase_crc32(name) == *crc)
    })
}

/// A player as its ability lists read it: the tags of the weapon stance in its hand, its status
/// effects, the abilities that are on (the progression points it spent, the `EquipAbility` of
/// the perks it wears, the threshold abilities of its attribute values) and their stats.
struct PlayerHolder<'a> {
    tags: Vec<String>,
    effects: &'a super::status_effects::StatusEffects,
    rules: &'a StatusEffectRules,
    weapon: bool,
    abilities_on: Vec<&'a str>,
    stats: AbilityStats,
}

impl Holder for PlayerHolder<'_> {
    fn tags(&self) -> Vec<&str> {
        self.tags.iter().map(String::as_str).collect()
    }

    fn has_effect(&self, effect: &str) -> bool {
        self.rules
            .by_id(effect)
            .is_some_and(|rule| self.effects.holds(rule.crc))
    }

    fn item_in_slot(&self, _slot: &str) -> bool {
        self.weapon
    }

    fn ability_active(&self, ability: &str) -> bool {
        self.abilities_on
            .iter()
            .any(|own| own.eq_ignore_ascii_case(ability))
    }

    fn stats(&self) -> AbilityStats {
        self.stats
    }
}

/// A player that casts a spell.
pub(super) struct Caster {
    pub(super) entity: Entity,
    pub(super) connection: ConnectionRef,
    pub(super) number: u64,
    from: Position,
    view: Option<LookDir>,
    lock_target: Option<u64>,
}

/// The status effects of a friendly projectile spell on their way to the player it met.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct EffectFlight {
    target: Entity,
    effects: Vec<String>,
    /// The number of the caster.
    source: u64,
    /// The radius around the player in which the other players take them too.
    splash_m: f32,
    /// When they get there.
    land_ms: u64,
}

impl SimWorld {
    /// Reads the ability key presses in the input `records` of player `entity` at `now_ms`: a key
    /// names the mapped ability that the lists of the weapon in its hand start.
    pub(super) fn ability_presses(&mut self, entity: Entity, records: &[InputRecord], now_ms: u64) {
        let data = Arc::clone(&self.combat.data);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        let Some((item, _)) = player.gear.as_ref().and_then(Gear::main_weapon) else {
            return;
        };
        let lists = data.ability_lists_of_item(item);
        let mapped: Vec<Option<u32>> = (0..nw_messages::MAX_MAPPED_SLOTS)
            .map(|key| ability_of_key(&lists, player.masteries.mapped_to(key)))
            .collect();
        player.abilities.press(
            records,
            |key| mapped.get(key).copied().flatten(),
            |record| data.ability_inputs_on(|number| record.input_on(number)),
            now_ms,
        );
    }

    /// Runs the ability lists of the weapon in each player's hand at `now_ms`: the status effects
    /// an ability applies go on its player; its casts go to [`SimWorld::cast_ability_spell`].
    /// Gives the hits of the spells; lands the status effects of friendly projectiles due and
    /// runs the spell areas ([`super::spell_areas`]).
    pub(super) fn tick_abilities(
        &mut self,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let data = Arc::clone(&self.combat.data);
        let content = Arc::clone(&self.content);
        let rules = &content.status_effects;
        let server_ms = self.server_ms;
        let mut casts: Vec<(Entity, String, Box<SpellRow>, CastKind)> = Vec::new();
        let mut query = self.world.query::<(Entity, &mut Player)>();
        for (entity, mut player) in query.iter_mut(&mut self.world) {
            let player = &mut *player;
            let held = player.gear.as_ref().and_then(Gear::main_weapon);
            let Some((item, _)) = held else {
                continue;
            };
            let lists = data.ability_lists_of_item(item);
            if lists.is_empty() {
                continue;
            }
            let tags = data
                .weapon_of_item(item)
                .and_then(|weapon| data.stance(weapon.stance))
                .map(|stance| stance_tags(&stance.tags))
                .unwrap_or_default();
            let mut abilities_on: Vec<&str> = player.masteries.spent_points().collect();
            abilities_on.extend(player.gear.iter().flat_map(|gear| {
                gear.worn_perks()
                    .into_iter()
                    .filter_map(|(perk, _)| content.perk_effects.ability_of(perk))
            }));
            let attributes = &player.attributes;
            abilities_on.extend(
                content.attribute_bonuses.abilities(|attribute| {
                    attributes.value(&content.progression.points, attribute)
                }),
            );
            let stats = content
                .attribute_bonuses
                .stats_of(abilities_on.iter().copied());
            let holder = PlayerHolder {
                tags,
                effects: &player.status_effects,
                rules,
                weapon: true,
                abilities_on,
                stats,
            };
            let mut events = Vec::new();
            for list in lists {
                events.extend(player.abilities.tick(list, &holder, now_ms, server_ms));
            }
            let number = player.number;
            for event in events {
                match event {
                    AbilityEvent::Effects(effects) => {
                        for effect in effects {
                            player.status_effects.add_by_id(
                                rules,
                                &effect,
                                OWN_EFFECT_POTENCY,
                                number,
                                server_ms,
                            );
                        }
                    }
                    AbilityEvent::Cast {
                        action,
                        spell,
                        kind,
                    } => casts.push((entity, action, spell, kind)),
                }
            }
        }
        let mut results = Vec::new();
        for (entity, action, spell, kind) in casts {
            results.extend(self.cast_ability_spell(entity, &action, &spell, kind, now_ms));
        }
        self.effect_flights_due(now_ms);
        results.extend(self.tick_spell_areas(now_ms));
        results
    }

    /// Casts `spell` of ability action `action` for player `entity` at `now_ms`: a spell with a
    /// projectile flies ([`SimWorld::fire_ability_spell`]), any other opens its area at once
    /// ([`SimWorld::ability_spell_area`]).
    pub(super) fn cast_ability_spell(
        &mut self,
        entity: Entity,
        action: &str,
        spell: &SpellRow,
        kind: CastKind,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let Some(caster) = self.caster(entity) else {
            return Vec::new();
        };
        tracing::debug!(
            player = caster.number,
            action,
            spell = %spell.id,
            ?kind,
            projectile = spell.projectile.is_some(),
            "ability spell cast"
        );
        match spell.projectile {
            Some(projectile) => self.fire_ability_spell(&caster, spell, projectile, now_ms),
            None => {
                self.ability_spell_area(&caster, spell, now_ms);
                Vec::new()
            }
        }
    }

    /// Player `entity` as the caster of a spell.
    fn caster(&self, entity: Entity) -> Option<Caster> {
        let player = self.world.get::<Player>(entity)?;
        let body = self.world.get::<super::Body>(entity)?;
        Some(Caster {
            entity,
            connection: player.connection.clone(),
            number: player.number,
            from: body.motion.position(),
            view: player.view,
            lock_target: self
                .world
                .get::<super::Vitals>(entity)
                .and_then(|vitals| vitals.lock_target()),
        })
    }

    /// Fires the `projectile` of `spell` from `caster` along its view at `now_ms` (toward its
    /// locked mob, as a basic shot). The first mob on its line takes the damage row and the
    /// status effects when its targets take enemies; the first other player on it the status
    /// effects when they take friends; both splash over the spell's radius (Inferred: a flying
    /// spell's `Radius` is its impact's). Its secondary spell opens where it ends: on what it
    /// met first, else at its range or the end of its flight (Inferred: no ground under the
    /// line is known).
    fn fire_ability_spell(
        &mut self,
        caster: &Caster,
        spell: &SpellRow,
        projectile: SpellProjectile,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let places = self.target_places();
        let facing = caster.view.map(|view| view.yaw);
        let Some(yaw) = Self::aim_yaw(caster.lock_target, facing, caster.from, &places) else {
            return vec![Err(AttackRefusal::NoAim)];
        };
        let aim = AimLine {
            yaw,
            pitch: caster.view.map_or(0.0, |view| view.pitch),
        };
        let splash_m = spell.radius_m.unwrap_or_default();
        let shot = projectile
            .flight
            .map_or(Shot::hit_scan(projectile.range_m), |flight| {
                Shot::projectile(projectile.range_m, projectile.radius_m, flight)
            })
            .with_splash(splash_m);
        let targets = spell.target_types.as_deref().unwrap_or_default();
        let enemy_row = spell
            .damage_row
            .as_deref()
            .filter(|_| ENEMY_TARGETS.contains(&targets));
        let met_mob = enemy_row.and_then(|_| first_on_line(&shot, caster.from, aim, &places));
        let met_friend = (!spell.status_effects.is_empty() && FRIEND_TARGETS.contains(&targets))
            .then(|| {
                let friends = self.friend_places(caster.entity);
                first_on_line(&shot, caster.from, aim, &friends)
            })
            .flatten();
        let mut results = Vec::new();
        match (enemy_row, met_mob) {
            (Some(row), Some((mob, flight_s))) => {
                let effects = (!spell.status_effects.is_empty())
                    .then_some((spell.status_effects.as_slice(), caster.number));
                let hit = ShotHit {
                    damage_id: row,
                    effects,
                };
                self.launch(&caster.connection, (shot, hit), mob, flight_s, now_ms);
            }
            (Some(_), None) => results.push(Ok(AttackOutcome::Missed)),
            (None, _) => {}
        }
        if let Some((met, flight_s)) = met_friend {
            self.effect_flights.push(EffectFlight {
                target: Entity::from_bits(met),
                effects: spell.status_effects.clone(),
                source: caster.number,
                splash_m,
                land_ms: now_ms.saturating_add(flight_ms(flight_s)),
            });
        }
        if let Some(secondary) = &spell.secondary {
            let (at, flight_s) = self.shot_end(caster, &shot, aim, met_mob, met_friend);
            let area = SpellArea::new(
                caster,
                secondary,
                at,
                now_ms.saturating_add(flight_ms(flight_s)),
            );
            self.open_spell_area(area);
        }
        results
    }

    /// The living players but `caster`, with their bodies, as a shot meets them (by entity bits).
    fn friend_places(&mut self, caster: Entity) -> Vec<(u64, Position, Capsule)> {
        let mut query = self.world.query::<(Entity, &super::Body, &super::Vitals)>();
        query
            .iter(&self.world)
            .filter(|(other, _, vitals)| *other != caster && !vitals.health.is_dead())
            .map(|(other, body, _)| (other.to_bits(), body.motion.position(), PLAYER_CAPSULE))
            .collect()
    }

    /// Where `shot` of `caster` along `aim` ends, and after how long (seconds): at the mob or
    /// player it meets first, else at its end ([`Shot::end`]).
    fn shot_end(
        &mut self,
        caster: &Caster,
        shot: &Shot,
        aim: AimLine,
        met_mob: Option<(u64, f32)>,
        met_friend: Option<(u64, f32)>,
    ) -> (Position, f32) {
        let mob = met_mob.and_then(|(mob, flight_s)| {
            let at = self
                .mob_states()
                .find(|(own, _, _)| *own == mob)
                .map(|(_, at, _)| at)?;
            Some((at, flight_s))
        });
        let friend = met_friend.and_then(|(bits, flight_s)| {
            let at = self
                .world
                .get::<super::Body>(Entity::from_bits(bits))?
                .motion
                .position();
            Some((at, flight_s))
        });
        let first = match (mob, friend) {
            (Some(mob), Some(friend)) => Some(if friend.1 < mob.1 { friend } else { mob }),
            (one, other) => one.or(other),
        };
        first.unwrap_or_else(|| {
            let (distance_m, time_s) = shot.end();
            (ahead_of(caster.from, aim.yaw, distance_m), time_s)
        })
    }

    /// Opens the area `spell` of `caster` at `now_ms`: at the caster, or the point its
    /// `CastDistance` ahead along the caster's look (Inferred: the client aims a raycast or
    /// targeting cast where it looks, and sends no point) ([`super::spell_areas`]).
    fn ability_spell_area(&mut self, caster: &Caster, spell: &SpellRow, now_ms: u64) {
        let yaw = caster.view.map_or(0.0, |view| view.yaw);
        let ahead = spell.cast_distance_m.unwrap_or_default().max(0.0);
        let area = SpellArea::new(caster, spell, ahead_of(caster.from, yaw, ahead), now_ms);
        self.open_spell_area(area);
    }

    /// The living players within `radius` of `centre`.
    pub(super) fn players_within(&mut self, centre: Position, radius: f32) -> Vec<Entity> {
        let mut query = self.world.query::<(Entity, &super::Body, &super::Vitals)>();
        query
            .iter(&self.world)
            .filter(|(_, body, vitals)| {
                !vitals.health.is_dead()
                    && body.motion.position().horizontal_distance(centre) <= radius
            })
            .map(|(entity, _, _)| entity)
            .collect()
    }

    /// Puts the spell `effects` of player `source` on player `entity`; with `keep_held`, an
    /// effect it holds stays as it is.
    pub(super) fn spell_effects_on_player(
        &mut self,
        entity: Entity,
        effects: &[String],
        source: u64,
        keep_held: bool,
    ) {
        let content = Arc::clone(&self.content);
        let rules = &content.status_effects;
        let server_ms = self.server_ms;
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            for effect in effects {
                if keep_held && holds_effect(&player.status_effects, rules, effect) {
                    continue;
                }
                player.status_effects.add_by_id(
                    rules,
                    effect,
                    OWN_EFFECT_POTENCY,
                    source,
                    server_ms,
                );
            }
        }
    }

    /// Puts the spell `effects` of player `source` on mob `mob` of this sim (a mob of another
    /// sim holds its effects there: Unknown how retail moves them, so they stay off); with
    /// `keep_held`, an effect it holds stays as it is.
    pub(super) fn spell_effects_on_mob(
        &mut self,
        mob: u64,
        effects: &[String],
        source: u64,
        keep_held: bool,
    ) {
        if effects.is_empty() || self.mobs.get(mob).is_none() {
            return;
        }
        let content = Arc::clone(&self.content);
        let rules = &content.status_effects;
        let server_ms = self.server_ms;
        let held = self.mob_effects.entry(mob).or_default();
        for effect in effects {
            if keep_held && holds_effect(held, rules, effect) {
                continue;
            }
            held.add_by_id(rules, effect, OWN_EFFECT_POTENCY, source, server_ms);
        }
    }

    /// Takes the spell `effects` off player `entity`.
    pub(super) fn clear_spell_effects_on_player(&mut self, entity: Entity, effects: &[String]) {
        let content = Arc::clone(&self.content);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            for crc in effect_crcs(&content.status_effects, effects) {
                player.status_effects.remove(crc);
            }
        }
    }

    /// Takes the spell `effects` off mob `mob`.
    pub(super) fn clear_spell_effects_on_mob(&mut self, mob: u64, effects: &[String]) {
        let content = Arc::clone(&self.content);
        if let Some(held) = self.mob_effects.get_mut(&mob) {
            for crc in effect_crcs(&content.status_effects, effects) {
                held.remove(crc);
            }
        }
    }

    /// Lands the status effects of the friendly projectiles that got to their player at
    /// `now_ms`: on it and the living players within the splash of it.
    fn effect_flights_due(&mut self, now_ms: u64) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.effect_flights)
            .into_iter()
            .partition(|flight| now_ms >= flight.land_ms);
        self.effect_flights = later;
        for flight in due {
            let Some(at) = self
                .world
                .get::<super::Body>(flight.target)
                .map(|body| body.motion.position())
            else {
                continue;
            };
            let mut takers = self.players_within(at, flight.splash_m);
            if !takers.contains(&flight.target) {
                takers.push(flight.target);
            }
            for taker in takers {
                self.spell_effects_on_player(taker, &flight.effects, flight.source, false);
            }
        }
    }
}

/// The point `distance_m` ahead of `from` along `yaw`, at its height.
fn ahead_of(from: Position, yaw: f32, distance_m: f32) -> Position {
    Position {
        x: from.x - yaw.sin() * distance_m,
        y: from.y + yaw.cos() * distance_m,
        z: from.z,
    }
}

/// Whether `held` holds the status effect `effect` (any id `rules` knows).
fn holds_effect(
    held: &super::status_effects::StatusEffects,
    rules: &StatusEffectRules,
    effect: &str,
) -> bool {
    rules.by_id(effect).is_some_and(|rule| held.holds(rule.crc))
}

/// The CRCs of the status effects `effects` that `rules` knows.
fn effect_crcs(rules: &StatusEffectRules, effects: &[String]) -> Vec<u32> {
    effects
        .iter()
        .filter_map(|effect| rules.by_id(effect).map(|rule| rule.crc))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nw_content::player_abilities::{AbilityCast, FragmentMarker, FragmentRow, ListAction};
    use nw_content::player_attacks::Flight;
    use nw_content::status_effects::{EffectSourceRow, StatusEffectSource};
    use nw_formats::mannequin::AbilityUse;
    use nw_messages::InputSlots;

    use super::*;
    use crate::mobs::mob_entity;
    use crate::world::tests::{CELL_M, NOW, join, table, test_spawn, tick, world_with_spawns};

    const SMITE: &str = "Ability_Life_Staff_Smite";
    /// The upgrade the test staff holder has.
    const UPGRADE: &str = "Upgrade_LifeStaff_Smite";
    const FOLLOW_UP: &str = "Smite_Cast";
    const MARKER: &str = "Magic_IsReadying_Smite";
    const COOLDOWN_MS: u64 = 4_000;
    const COST: f32 = 16.0;
    const FRAGMENT_MS: u64 = 1_000;
    const MARKER_START_MS: u64 = 200;
    const SERVER_MS: u64 = 846_279_677_606;
    /// The input value and bit of ability key 1 (402 = 6 x 64 + 18).
    const KEY_1: (usize, u32) = (6, 18);
    /// Yaw toward -x.
    const FACING_WEST: f32 = std::f32::consts::FRAC_PI_2;
    const MOB_X: f32 = CELL_M / 2.0;
    const CASTER_X: f32 = MOB_X + 10.0;
    /// A row of the test combat data.
    const DAMAGE_ROW: &str = "1H_Sword_Attack1";
    const BOLT_RANGE_M: f32 = 40.0;
    const BOLT_RADIUS_M: f32 = 0.15;
    const BOLT_SPEED_MPS: f32 = 30.0;
    const BOLT_LIFETIME_S: f32 = 2.0;
    /// The second mob stands this far east of the first, nearer the caster.
    const SPLASH_GAP_M: f32 = 1.5;
    /// To the near mob (10 - 1.5 m) at 30 m/s, and a little.
    const BOLT_FLIGHT_MS: u64 = 300;
    const SPLASH_M: f32 = 3.0;
    const FAR_M: f32 = 6.0;
    /// After the hit interval of the first bolt.
    const LATER_MS: u64 = 5_000;
    const TICK_MS: u64 = 33;
    const FOCUS_MAX_MANA_SHARE: f64 = 0.1;
    const FOCUS_COOLDOWN_SHARE: f64 = -0.03;
    const FOCUS_MANA_MAX: f32 = 110.0;
    /// [`COOLDOWN_MS`] x 0.97.
    const FOCUS_COOLDOWN_MS: u64 = 3_880;
    const GROUND: &str = "Status_Ability_Lifestaff_SacredGround";
    const GROUND_EFFECT_S: f64 = 30.0;
    /// Sacred Ground's `Duration`.
    const GROUND_MS: i64 = 12_000;
    /// To the mob (10 m) at 30 m/s, and a little.
    const BOLT_TO_FAR_MS: u64 = 340;

    struct Staff;

    impl Holder for Staff {
        fn tags(&self) -> Vec<&str> {
            vec!["MagicStaff", "Heal"]
        }

        fn has_effect(&self, _: &str) -> bool {
            false
        }

        fn item_in_slot(&self, _: &str) -> bool {
            true
        }

        fn ability_active(&self, ability: &str) -> bool {
            ability == UPGRADE
        }

        fn stats(&self) -> AbilityStats {
            AbilityStats::default()
        }
    }

    fn spell() -> SpellRow {
        SpellRow {
            id: "LifeStaff_Smite".to_owned(),
            types: None,
            shape: Some("Sphere".to_owned()),
            radius_m: Some(3.0),
            cast_distance_m: Some(20.0),
            duration_ms: Some(500),
            damage_row: None,
            status_effects: Vec::new(),
            target_types: Some("Enemy".to_owned()),
            mana_cost: Some(COST),
            projectile: None,
            clear_on_exit: false,
            clear_on_end: false,
            secondary: None,
        }
    }

    /// Smite: a start action on its key (mana, no cooldown, the staff's tag) that starts the
    /// cooldown, and a follow-up while the start's fragment plays that casts at its marker.
    fn list() -> AbilityList {
        let start = ListAction {
            name: SMITE.to_owned(),
            fragment: SMITE.to_owned(),
            activation: Some(Condition::All(vec![
                Condition::AbilityMove(SMITE.to_owned()),
                Condition::ManaCost("LifeStaff_Smite".to_owned()),
                Condition::Not(Box::new(Condition::Cooldown(SMITE.to_owned()))),
                Condition::Tag("Heal".to_owned()),
            ])),
            deactivation: None,
            sets_tags: vec!["AttackAction".to_owned()],
            ability: Some(AbilityUse {
                move_name: Some(SMITE.to_owned()),
                mana_cost: Some("LifeStaff_Smite".to_owned()),
                needs_ready: vec![SMITE.to_owned()],
                performs: vec![SMITE.to_owned()],
                starts_cooldowns: vec![SMITE.to_owned()],
                applies_on_enter: vec!["Smite_Self".to_owned()],
            }),
            casts: Vec::new(),
        };
        let follow_up = ListAction {
            name: FOLLOW_UP.to_owned(),
            fragment: String::new(),
            activation: Some(Condition::All(vec![
                Condition::FragmentPlaying(Some(SMITE.to_owned())),
                Condition::Tag("AttackAction".to_owned()),
            ])),
            deactivation: Some(Condition::Not(Box::new(Condition::FragmentPlaying(Some(
                SMITE.to_owned(),
            ))))),
            sets_tags: Vec::new(),
            ability: None,
            casts: vec![AbilityCast {
                condition: Condition::Marker(MARKER.to_owned()),
                kind: CastKind::Raycast,
                spell: spell(),
            }],
        };
        AbilityList {
            weapon_classes: vec!["LifeStaff".to_owned()],
            actions: vec![start, follow_up],
            fragments: vec![FragmentRow {
                name: SMITE.to_owned(),
                length_ms: FRAGMENT_MS,
                markers: vec![FragmentMarker {
                    name: MARKER.to_owned(),
                    start_ms: MARKER_START_MS,
                    end_ms: FRAGMENT_MS,
                }],
                hit: None,
            }],
            cooldowns_ms: BTreeMap::from([(SMITE.to_owned(), COOLDOWN_MS)]),
            mana_costs: BTreeMap::from([("LifeStaff_Smite".to_owned(), COST)]),
        }
    }

    pub(super) fn key_1(frame: u16) -> InputRecord {
        let mut values = vec![0_u64; KEY_1.0 + 1];
        values[KEY_1.0] = 1 << KEY_1.1;
        InputRecord {
            u16_after_mask: frame,
            slots: InputSlots {
                values,
                ..InputSlots::default()
            },
            ..InputRecord::default()
        }
    }

    pub(super) fn no_inputs(_: &InputRecord) -> Vec<String> {
        Vec::new()
    }

    fn mapped(key: usize) -> Option<u32> {
        (key == 0).then(|| lowercase_crc32(SMITE))
    }

    #[test]
    fn a_key_press_runs_the_ability_chain_once_with_its_cooldown_mana_and_cast() {
        let list = list();
        let mut run = AbilityRun::default();
        assert!(run.tick(&list, &Staff, 0, SERVER_MS).is_empty(), "no press");
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        let start = run.tick(&list, &Staff, 0, SERVER_MS);
        assert_eq!(
            start,
            [AbilityEvent::Effects(vec!["Smite_Self".to_owned()])]
        );
        let changes = run.take_cooldown_changes();
        assert_eq!(changes.len(), 1);
        let (key, cooldown) = changes[0];
        assert_eq!((key, cooldown.ability), (0, lowercase_crc32(SMITE)));
        assert_eq!(cooldown.end_us - cooldown.start_us, COOLDOWN_MS * US_PER_MS);
        // The follow-up starts on the start's fragment and tag, and casts at the marker.
        assert!(
            run.tick(&list, &Staff, 100, SERVER_MS).is_empty(),
            "before the marker"
        );
        let cast = run.tick(&list, &Staff, MARKER_START_MS, SERVER_MS);
        assert!(
            matches!(&cast[..], [AbilityEvent::Cast { spell, kind: CastKind::Raycast, .. }] if spell.id == "LifeStaff_Smite"),
            "{cast:?}"
        );
        assert!(
            (run.mana.current - (MANA_MAX - COST)).abs() < 1.0,
            "spent, a little back"
        );
        assert!(
            run.tick(&list, &Staff, MARKER_START_MS + 50, SERVER_MS)
                .is_empty(),
            "one cast per run"
        );
        // A second press during the cooldown starts nothing.
        run.press(&[key_1(11)], mapped, no_inputs, 2_000);
        assert!(run.tick(&list, &Staff, 2_000, SERVER_MS).is_empty());
        assert!(run.take_cooldown_changes().is_empty());
        // After the cooldown it runs again.
        run.press(&[key_1(12)], mapped, no_inputs, 5_000);
        assert_eq!(run.tick(&list, &Staff, 5_000, SERVER_MS).len(), 1);
    }

    /// The test staff holder with the shares of Focus 250 (`Foc_Bonus_25_1`, `Foc_Bonus_100_1`).
    struct FocusedStaff;

    impl Holder for FocusedStaff {
        fn tags(&self) -> Vec<&str> {
            Staff.tags()
        }

        fn has_effect(&self, effect: &str) -> bool {
            Staff.has_effect(effect)
        }

        fn item_in_slot(&self, slot: &str) -> bool {
            Staff.item_in_slot(slot)
        }

        fn ability_active(&self, ability: &str) -> bool {
            Staff.ability_active(ability)
        }

        fn stats(&self) -> AbilityStats {
            AbilityStats {
                max_mana: FOCUS_MAX_MANA_SHARE,
                mana_rate: f64::from(RETAIL_MANA_RATE_SHARE),
                cooldown: FOCUS_COOLDOWN_SHARE,
            }
        }
    }

    #[test]
    fn focus_shares_raise_the_mana_max_and_regen_and_shorten_the_cooldown() {
        let list = list();
        let mut run = AbilityRun::default();
        run.tick(&list, &FocusedStaff, 0, SERVER_MS);
        assert!((run.mana.max - FOCUS_MANA_MAX).abs() < 1e-3, "retail 110");
        run.mana.current = 0.0;
        run.tick(&list, &FocusedStaff, 1_000, SERVER_MS);
        assert!(
            (run.mana.current - RETAIL_MANA_REGEN_PER_S).abs() < 1e-3,
            "the retail rate at Focus 250"
        );
        run.mana.current = run.mana.max;
        run.press(&[key_1(10)], mapped, no_inputs, 2_000);
        run.tick(&list, &FocusedStaff, 2_000, SERVER_MS);
        let changes = run.take_cooldown_changes();
        let (_, cooldown) = changes[0];
        assert_eq!(
            cooldown.end_us - cooldown.start_us,
            FOCUS_COOLDOWN_MS * US_PER_MS,
            "4 s less 3 %"
        );
    }

    #[test]
    fn a_repeated_frame_is_one_press_and_an_empty_slot_none() {
        let mut run = AbilityRun::default();
        run.press(&[key_1(10), key_1(10)], |_| Some(0), no_inputs, 0);
        assert_eq!(run.press, None, "an empty slot names no ability");
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        assert_eq!(run.press, None, "frame 10 was read");
        run.press(&[key_1(11)], mapped, no_inputs, 0);
        assert!(run.press.is_some());
    }

    #[test]
    fn mana_comes_back_in_steps_up_to_its_max() {
        let mut mana = Mana {
            current: 50.0,
            ..Mana::default()
        };
        mana.regen(0);
        mana.regen(1_000);
        assert!((mana.current - (50.0 + MANA_REGEN_PER_S)).abs() < 1e-3);
        assert_eq!(mana.take_change(), Some(mana.current));
        assert_eq!(mana.take_change(), None, "sent");
        mana.regen(100_000);
        assert!((mana.current - MANA_MAX).abs() < f32::EPSILON);
    }

    /// A world with mobs at `xs` (the test spawns' line) and player 1 at [`CASTER_X`] looking
    /// west at them, after a tick that wakes them.
    fn spell_fight(xs: &[f32]) -> (SimWorld, Entity) {
        let mut world = world_with_spawns(xs.iter().map(|x| test_spawn(*x)).collect());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, CASTER_X);
        let entity = world.player_by_number(1).expect("player");
        world.world.get_mut::<Player>(entity).expect("player").view = Some(LookDir {
            yaw: FACING_WEST,
            pitch: 0.0,
        });
        tick(&mut world, NOW);
        (world, entity)
    }

    /// A bolt of [`BOLT_SPEED_MPS`] that hurts enemies by the sword row, splashing `splash_m`.
    fn bolt(splash_m: Option<f32>) -> SpellRow {
        SpellRow {
            id: "Spell_Test_Bolt".to_owned(),
            shape: None,
            radius_m: splash_m,
            cast_distance_m: None,
            damage_row: Some(DAMAGE_ROW.to_owned()),
            projectile: Some(SpellProjectile {
                range_m: BOLT_RANGE_M,
                radius_m: BOLT_RADIUS_M,
                flight: Some(Flight {
                    base_speed_mps: BOLT_SPEED_MPS,
                    extra_speed_mps: 0.0,
                    time_for_max_speed_s: 0.0,
                    gravity_mps2: 0.0,
                    lifetime_s: BOLT_LIFETIME_S,
                }),
            }),
            ..spell()
        }
    }

    /// The mobs hit in `results`.
    fn hit_mobs(results: Vec<Result<AttackOutcome, AttackRefusal>>) -> Vec<u64> {
        let mut mobs: Vec<u64> = results
            .into_iter()
            .filter_map(|result| match result {
                Ok(AttackOutcome::Hit { target, .. }) => Some(target),
                _ => None,
            })
            .collect();
        mobs.sort_unstable();
        mobs
    }

    #[test]
    fn a_projectile_spell_flies_to_the_first_mob_on_its_line_and_splashes() {
        let near = MOB_X + SPLASH_GAP_M;
        let (mut world, caster) = spell_fight(&[MOB_X, near]);
        let fired = world.cast_ability_spell(caster, "Test", &bolt(None), CastKind::Plain, NOW);
        assert!(fired.is_empty(), "on its way: {fired:?}");
        // 10 m less the near mob's gap at 30 m/s.
        let flight_ms = BOLT_FLIGHT_MS;
        assert!(
            hit_mobs(world.player_swings_due(NOW + flight_ms / 2)).is_empty(),
            "still flying"
        );
        assert_eq!(
            hit_mobs(world.player_swings_due(NOW + flight_ms)),
            vec![mob_entity(1)],
            "the nearer mob only"
        );
        let splash = bolt(Some(SPLASH_M));
        world.cast_ability_spell(caster, "Test", &splash, CastKind::Plain, NOW + LATER_MS);
        assert_eq!(
            hit_mobs(world.player_swings_due(NOW + LATER_MS + flight_ms)),
            vec![mob_entity(0), mob_entity(1)],
            "the splash takes the one behind"
        );
    }

    #[test]
    fn an_area_spell_hits_the_mobs_in_its_radius_ahead() {
        let (mut world, caster) = spell_fight(&[MOB_X, MOB_X - FAR_M]);
        let area = SpellRow {
            radius_m: Some(SPLASH_M),
            cast_distance_m: Some(CASTER_X - MOB_X),
            damage_row: Some(DAMAGE_ROW.to_owned()),
            ..spell()
        };
        let cast = world.cast_ability_spell(caster, "Test", &area, CastKind::Raycast, NOW);
        assert!(cast.is_empty(), "the area opens at the next pass");
        assert_eq!(
            hit_mobs(world.tick_spell_areas(NOW)),
            vec![mob_entity(0)],
            "not the far one"
        );
        assert!(
            hit_mobs(world.tick_spell_areas(NOW + TICK_MS)).is_empty(),
            "its row hits a mob once"
        );
    }

    /// The test world with the one status effect [`GROUND`] (30 s).
    fn with_ground_effect(world: &mut SimWorld) {
        Arc::make_mut(&mut world.content).status_effects =
            StatusEffectRules::build(StatusEffectSource {
                effects: vec![EffectSourceRow {
                    status_id: GROUND.to_owned(),
                    base_duration_s: Some(GROUND_EFFECT_S),
                    ..EffectSourceRow::default()
                }],
                ..StatusEffectSource::default()
            });
    }

    fn holds_ground(world: &mut SimWorld, number: u64) -> bool {
        let entity = world.player_by_number(number).expect("player");
        let player = world.world.get::<Player>(entity).expect("player");
        holds_effect(
            &player.status_effects,
            &world.content.status_effects,
            GROUND,
        )
    }

    #[test]
    fn a_lasting_area_holds_its_effects_on_the_players_in_it_until_they_leave_or_it_ends() {
        let (mut world, caster) = spell_fight(&[]);
        with_ground_effect(&mut world);
        join(&mut world, 2, CASTER_X + 1.0);
        let ground = SpellRow {
            radius_m: Some(SPLASH_M),
            cast_distance_m: Some(0.0),
            duration_ms: Some(GROUND_MS),
            status_effects: vec![GROUND.to_owned()],
            target_types: Some("Friend".to_owned()),
            clear_on_exit: true,
            clear_on_end: true,
            ..spell()
        };
        world.cast_ability_spell(caster, "Test", &ground, CastKind::Raycast, NOW);
        world.tick_spell_areas(NOW);
        assert!(holds_ground(&mut world, 1) && holds_ground(&mut world, 2));
        let away = nw_bus::Point {
            x: CASTER_X + FAR_M,
            y: 10.0,
            z: 50.0,
        };
        world.teleport_player(2, away, NOW + TICK_MS);
        world.tick_spell_areas(NOW + TICK_MS);
        assert!(!holds_ground(&mut world, 2), "it left");
        assert!(holds_ground(&mut world, 1), "still in it");
        world.tick_spell_areas(NOW + GROUND_MS as u64);
        assert!(!holds_ground(&mut world, 1), "the area ended");
        assert!(world.spell_areas.is_empty());
    }

    #[test]
    fn a_projectile_opens_its_secondary_spell_where_it_hits() {
        let (mut world, caster) = spell_fight(&[MOB_X]);
        with_ground_effect(&mut world);
        let vortex = SpellRow {
            radius_m: Some(SPLASH_M),
            damage_row: None,
            status_effects: vec![GROUND.to_owned()],
            ..spell()
        };
        let well = SpellRow {
            secondary: Some(Box::new(vortex)),
            ..bolt(None)
        };
        world.cast_ability_spell(caster, "Test", &well, CastKind::Plain, NOW);
        world.tick_spell_areas(NOW);
        assert!(world.mob_effects.is_empty(), "not there yet");
        let landed = NOW + BOLT_TO_FAR_MS;
        world.player_swings_due(landed);
        world.tick_spell_areas(landed);
        let held = world.mob_effects.get(&mob_entity(0)).expect("effects");
        assert!(holds_effect(held, &world.content.status_effects, GROUND));
    }

    #[test]
    fn the_run_answers_the_tests_of_the_list() {
        let list = list();
        let mut run = AbilityRun::default();
        let staff = Staff;
        let ready = Condition::Not(Box::new(Condition::Cooldown(SMITE.to_owned())));
        assert!(run.holds(&list, &staff, &ready, 0));
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        run.tick(&list, &staff, 0, SERVER_MS);
        assert!(!run.holds(&list, &staff, &ready, 1), "on cooldown");
        let resize = Condition::Other("IfCanResizeCharacterController".to_owned());
        assert!(run.holds(&list, &staff, &resize, 0), "a passing guard");
        let installed = Condition::ActionStatus {
            action: SMITE.to_owned(),
            installed: true,
        };
        assert!(run.holds(&list, &staff, &installed, 1));
        let upgraded = Condition::AbilityActive(UPGRADE.to_owned());
        assert!(run.holds(&list, &staff, &upgraded, 0), "the upgrade is on");
        let other = Condition::AbilityActive("Upgrade_LifeStaff_Other".to_owned());
        assert!(!run.holds(&list, &staff, &other, 0));
        let shoot = Condition::Input("attack_shoot".to_owned());
        assert!(!run.holds(&list, &staff, &shoot, 0), "not pressed");
        run.press(&[key_1(11)], mapped, |_| vec!["attack_shoot".to_owned()], 0);
        assert!(run.holds(&list, &staff, &shoot, 0), "named by the record");
        run.press(&[key_1(12)], mapped, no_inputs, 0);
        assert!(
            !run.holds(&list, &staff, &shoot, 0),
            "the next message lets it go"
        );
    }
}

/// The life staff list of the ability table of `local-data/content` (run `content-tool
/// player-abilities` first).
#[cfg(all(test, feature = "local-data"))]
mod retail_tests {
    use std::path::Path;

    use nw_content::client_build::load_client_build;
    use nw_content::player_abilities::PlayerAbilities;

    use super::tests::{key_1, no_inputs};
    use super::*;

    const CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
    const SACRED_GROUND: &str = "Ability_Lifestaff_SacredGround";
    const SACRED_GROUND_SPELL: &str = "Spell_LifeStaff_SacredGround";
    /// `Cooldowns_Player` `Ability_Lifestaff_SacredGround`: 20 s.
    const SACRED_GROUND_COOLDOWN_MS: u64 = 20_000;
    const SERVER_MS: u64 = 846_279_677_606;
    const TICK_MS: u64 = 33;
    /// Longer than the start's fragment.
    const RUN_MS: u64 = 3_000;

    /// A player with a life staff (`MagicStaff,Heal`), no inputs, effects or upgrades.
    struct LifeStaff;

    impl Holder for LifeStaff {
        fn tags(&self) -> Vec<&str> {
            vec!["MagicStaff", "Heal"]
        }

        fn has_effect(&self, _: &str) -> bool {
            false
        }

        fn item_in_slot(&self, _: &str) -> bool {
            true
        }

        fn ability_active(&self, _: &str) -> bool {
            false
        }

        fn stats(&self) -> AbilityStats {
            AbilityStats::default()
        }
    }

    fn table() -> PlayerAbilities {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build = load_client_build(&repo.join(CLIENT_BUILD_CONFIG)).expect("client build");
        PlayerAbilities::load(&repo.join("local-data/content"), &build).expect("ability table")
    }

    #[test]
    fn the_table_numbers_the_ability_keys_as_retail() {
        let table = table();
        let names: Vec<&str> = nw_messages::ABILITY_KEYS
            .iter()
            .map(|number| table.inputs[*number as usize].as_str())
            .collect();
        assert_eq!(names, ["ability1", "ability2", "ability3"]);
    }

    #[test]
    fn a_sacred_ground_press_casts_its_spell_once_with_its_cooldown_and_mana() {
        let table = table();
        let lists = table.lists_for(&["LifeStaff"]);
        let mapped = |key: usize| (key == 0).then(|| lowercase_crc32(SACRED_GROUND));
        let mut run = AbilityRun::default();
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        let mut casts = Vec::new();
        let mut now = 0;
        while now <= RUN_MS {
            for list in &lists {
                for event in run.tick(list, &LifeStaff, now, SERVER_MS) {
                    if let AbilityEvent::Cast { spell, .. } = event {
                        casts.push(spell.id.clone());
                    }
                }
            }
            now += TICK_MS;
        }
        assert_eq!(casts, [SACRED_GROUND_SPELL], "one cast, no upgrade");
        let changes = run.take_cooldown_changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        let (key, cooldown) = changes[0];
        assert_eq!(key, 0);
        assert_eq!(cooldown.ability, lowercase_crc32(SACRED_GROUND));
        assert_eq!(
            cooldown.end_us - cooldown.start_us,
            SACRED_GROUND_COOLDOWN_MS * US_PER_MS
        );
        assert!(run.mana.current < MANA_MAX, "the cast spent mana");
    }
}
