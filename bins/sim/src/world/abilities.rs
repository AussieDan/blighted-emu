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

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::attribute_bonuses::AbilityStats;
use nw_content::player_abilities::{AbilityList, ListAction, SpellProjectile, SpellRow};
use nw_content::player_attacks::stance_tags;
use nw_content::status_effects::StatusEffectRules;
use nw_formats::mannequin::{CageWorld, CastKind, Condition, ManaCompare, TagCase, TagChange};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::paperdoll::slot;
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
/// One client frame, the unit of an `IfActiveAbilityMoveName BackTicks` count (input note: 34 ms
/// median; Inferred): `BackTicks="20"` names the press for 680 ms, a test without the count only
/// in the tick of the press.
const CLIENT_FRAME_MS: u64 = 34;
/// Milliseconds per second.
const MS_PER_S: f32 = 1_000.0;
/// Microseconds per millisecond.
const US_PER_MS: u64 = 1_000;
/// Tests the server takes as passing for a standing player with a weapon in the hand, by element
/// name (Inferred: the client's character fits its controller, the weapon its requirements and
/// its slot (the abilities run only while a weapon is in the hand), and the client checks its own
/// aim point, which it does not send).
const PASSING_TESTS: [&str; 4] = [
    "IfCanResizeCharacterController",
    "IfHasWeaponPrerequisites",
    "IfItemInSlotEquippable",
    "ActionConditionSpellSpawnLocationState",
];

/// The client settings that `IfStackConfigVar` tests and that are on: each one's client default
/// is on (the creation site of `NewWorld.exe` build 25810808 passes 1) and the retail remote
/// configuration of 2026-09-18 does not set it. Weapons need no ammo
/// (`javelin.enable-ammoless-weapons`). A setting not listed is off.
const CLIENT_SETTINGS_ON: [&str; 4] = [
    "javelin.enable-ammoless-weapons",
    "javelin.enable-inventory-while-running",
    "javelin.enable-running-on-roads",
    "javelin.musical-performances.enabled",
];
/// The slot alias of the weapon in the hand.
const ACTIVE_MAIN_HAND: &str = "ACTIVE_MAIN_HAND_WEAPON";
/// The slot alias of the off-hand item worn with it (Observed: 19 `IfItemInSlot` tests of the
/// player ability lists name it).
const ACTIVE_OFF_HAND: &str = "ACTIVE_OFF_HAND_WEAPON";
/// The slot aliases of the main-hand options, with their paperdoll slots.
const MAIN_HAND_OPTIONS: [(&str, u32); 3] = [
    ("MAIN_HAND_OPTION_1", slot::MAIN_HAND_OPTION_1),
    ("MAIN_HAND_OPTION_2", slot::MAIN_HAND_OPTION_2),
    ("MAIN_HAND_OPTION_3", slot::MAIN_HAND_OPTION_3),
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
    /// The Mannequin tags of the item in slot `slot`: those of its weapon stance. By default
    /// the weapon in the hand has the holder's tags and no other slot has any.
    fn tags_in_slot(&self, slot: &str) -> Vec<&str> {
        if slot == ACTIVE_MAIN_HAND {
            self.tags()
        } else {
            Vec::new()
        }
    }
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
    /// It started a chain: it still names its ability to the chain's tests within the frames
    /// each test counts back (`BackTicks`), but starts no other chain.
    used: bool,
}

/// An action that runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    /// Its list ([`list_key`]): a weapon has up to three lists (the spear's own and the shared
    /// `Ranged+Magic+Spear+Hatchet` one), each with its own action indices.
    list: usize,
    /// Its index in the list.
    action: usize,
    /// When it started.
    started_ms: u64,
    /// Each of its tag activities: off, or on with the case it started with (`None`: no case).
    tag_activities: Vec<Option<Option<TagCase>>>,
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
    /// The Mannequin tags its actions set and did not unset.
    tags_on: Vec<String>,
    /// The chain each action started in last, by list ([`list_key`]) and action index.
    started_in: HashMap<(usize, usize), u64>,
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

/// The tests of a list, for the run of one player and the action `current` that tests. The lists
/// of the weapon run as one: a test of a fragment, a marker or an action sees the actions of
/// every list (the bow's ability list aims with the archery list's `Bow_Aim_UpperBody`).
struct RunWorld<'a> {
    run: &'a AbilityRun,
    lists: &'a [&'a AbilityList],
    list: &'a AbilityList,
    holder: &'a dyn Holder,
    tags: Vec<&'a str>,
    current: Option<usize>,
    now_ms: u64,
}

impl RunWorld<'_> {
    /// The length of action `index`'s fragment, ms.
    fn length(&self, index: usize) -> u64 {
        self.list.actions.get(index).map_or(0, |action| {
            self.list
                .fragment(&action.fragment)
                .map_or(0, |fragment| fragment.length_ms)
        })
    }

    /// The running actions of every list, with their list and action.
    fn all_running(&self) -> impl Iterator<Item = (&Running, &AbilityList, &ListAction)> {
        self.run.running.iter().filter_map(|running| {
            let list = self
                .lists
                .iter()
                .find(|list| list_key(list) == running.list)?;
            Some((running, *list, list.actions.get(running.action)?))
        })
    }

    /// The running actions that play fragment `fragment` (any case; `None`: the testing
    /// action), each with its fragment's length and how long it runs.
    fn playing(&self, fragment: Option<&str>) -> Vec<(u64, u64)> {
        let elapsed = |running: &Running| self.now_ms.saturating_sub(running.started_ms);
        match fragment {
            None => self
                .run
                .running_in(self.list)
                .filter(|running| Some(running.action) == self.current)
                .map(|running| (self.length(running.action), elapsed(running)))
                .collect(),
            Some(name) => self
                .all_running()
                .filter(|(_, _, action)| action.fragment.eq_ignore_ascii_case(name))
                .map(|(running, list, action)| {
                    let length = list
                        .fragment(&action.fragment)
                        .map_or(0, |fragment| fragment.length_ms);
                    (length, elapsed(running))
                })
                .collect(),
        }
    }
}

impl CageWorld for RunWorld<'_> {
    fn tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|own| own.eq_ignore_ascii_case(tag))
    }

    fn marker(&self, marker: &str) -> bool {
        self.all_running().any(|(running, list, action)| {
            list.fragment(&action.fragment).is_some_and(|fragment| {
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
        installed && self.all_running().any(|(_, _, own)| own.name == action)
    }

    fn fragment_playing(&self, fragment: Option<&str>) -> bool {
        self.playing(fragment)
            .into_iter()
            .any(|(length, elapsed)| elapsed < length)
    }

    fn fragment_done(&self, fragment: Option<&str>, exit_offset_ms: u32) -> bool {
        self.playing(fragment)
            .into_iter()
            .any(|(length, elapsed)| elapsed + u64::from(exit_offset_ms) >= length)
    }

    fn ability_move(&self, ability: &str, back_ticks: u32) -> bool {
        self.run.press.is_some_and(|press| {
            press.ability == lowercase_crc32(ability)
                && self.now_ms.saturating_sub(press.at_ms)
                    <= u64::from(back_ticks) * CLIENT_FRAME_MS
        })
    }

    fn cooldown(&self, ability: &str) -> bool {
        let crc = lowercase_crc32(ability);
        self.run
            .cooldowns
            .iter()
            .any(|cooldown| cooldown.ability == crc && cooldown.end_ms > self.now_ms)
    }

    fn mana_cost(&self, cost: &str, compare: ManaCompare) -> bool {
        compare.holds(
            self.run.mana.current,
            self.list.mana_cost(cost).unwrap_or_default(),
        )
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

    fn tag_in_slot(&self, tag: &str, slot: &str) -> bool {
        self.holder
            .tags_in_slot(slot)
            .iter()
            .any(|own| own.eq_ignore_ascii_case(tag))
    }

    fn config_var(&self, variable: &str) -> bool {
        CLIENT_SETTINGS_ON.contains(&variable)
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

    /// One tick of the lists `lists` of the weapon in the hand at `now_ms` (`server_ms`: the same
    /// time on the server clock, ms since 2000-01-01): the mana comes back, and in each list
    /// actions move on, stop and start, and the running ones cast; gives what the world must do.
    pub fn tick(
        &mut self,
        lists: &[&AbilityList],
        holder: &dyn Holder,
        now_ms: u64,
        server_ms: u64,
    ) -> Vec<AbilityEvent> {
        let stats = holder.stats();
        self.mana.set_stats(stats);
        self.mana.regen(now_ms);
        let mut events = Vec::new();
        for list in lists {
            self.tick_list(lists, list, holder, (now_ms, server_ms), &mut events);
        }
        events
    }

    /// One tick of list `list` of `lists` at `now_ms` (`server_ms` on the server clock): a
    /// running action whose fragment's transition clip holds now moves on to the clip's action,
    /// in its own list or else in another list of the weapon (the bow's ready step goes on to the
    /// archery list's aim), before the stops (a clip at the fragment's end still moves it on);
    /// then actions stop and start, and the running ones cast; what the world must do goes in
    /// `events`.
    fn tick_list(
        &mut self,
        lists: &[&AbilityList],
        list: &AbilityList,
        holder: &dyn Holder,
        (now_ms, server_ms): (u64, u64),
        events: &mut Vec<AbilityEvent>,
    ) {
        let stats = holder.stats();
        let key = list_key(list);
        let tags = holder.tags();
        let transitions: Vec<(usize, &AbilityList, usize)> = self
            .running_in(list)
            .filter_map(|running| {
                let fragment = list.fragment(&list.actions[running.action].fragment)?;
                let since_ms = now_ms.saturating_sub(running.started_ms);
                let world = self.world(lists, list, holder, &tags, Some(running.action), now_ms);
                fragment
                    .transitions
                    .iter()
                    .filter(|transition| {
                        since_ms >= transition.start_ms
                            && (since_ms < transition.end_ms
                                || transition.start_ms >= transition.end_ms)
                            && transition
                                .input
                                .as_ref()
                                .is_none_or(|input| world.input(input))
                            && transition
                                .condition
                                .as_ref()
                                .is_none_or(|condition| condition.holds_in(&world))
                    })
                    .filter_map(|transition| {
                        std::iter::once(list)
                            .chain(lists.iter().copied().filter(|other| list_key(other) != key))
                            .find_map(|target| {
                                let index = target
                                    .actions
                                    .iter()
                                    .position(|action| action.name == transition.action)?;
                                Some((target, index))
                            })
                    })
                    .find(|(target, next)| {
                        !self.running_in(target).any(|other| other.action == *next)
                    })
                    .map(|(target, next)| (running.action, target, next))
            })
            .collect();
        for (from, target, to) in transitions {
            self.leave(list, &[from]);
            self.enter(target, to, self.chain, stats, (now_ms, server_ms), events);
        }
        let stops: Vec<usize> = self
            .running_in(list)
            .map(|running| running.action)
            .filter(|index| {
                let world = self.world(lists, list, holder, &tags, Some(*index), now_ms);
                match list.actions[*index].deactivation.as_ref() {
                    Some(condition) => condition.holds_in(&world),
                    None => world.fragment_done(None, 0),
                }
            })
            .collect();
        self.leave(list, &stops);
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
            if !in_chain || self.running_in(list).any(|running| running.action == index) {
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
            if self.started_in.get(&(key, index)) == Some(&chain)
                || (!starts_chain && self.chain == 0)
            {
                continue;
            }
            if !activation.holds_in(&self.world(lists, list, holder, &tags, Some(index), now_ms)) {
                continue;
            }
            let starts_cooldowns = action
                .ability
                .as_ref()
                .is_some_and(|ability| !ability.starts_cooldowns.is_empty());
            if starts_cooldowns && self.mana.current < chain_cost(list, index) {
                // Too little mana for the spells it starts: no cooldown and no spell.
                continue;
            }
            if starts_chain {
                self.chain = chain;
                self.chain_cast.clear();
            }
            self.enter(list, index, chain, stats, (now_ms, server_ms), events);
            if starts_chain && let Some(press) = self.press.as_mut() {
                press.used = true;
            }
        }
        self.update_tags(lists, list, holder, &tags, now_ms);
        let mut casts = Vec::new();
        for running in self.running_in(list) {
            let action = &list.actions[running.action];
            let world = self.world(lists, list, holder, &tags, Some(running.action), now_ms);
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
    }

    /// Starts action `index` of list `list` in chain `chain` at `now_ms` (`server_ms` on the
    /// server clock): it starts its ability's cooldowns, and its effects go in `events`.
    fn enter(
        &mut self,
        list: &AbilityList,
        index: usize,
        chain: u64,
        stats: AbilityStats,
        (now_ms, server_ms): (u64, u64),
        events: &mut Vec<AbilityEvent>,
    ) {
        let key = list_key(list);
        self.started_in.insert((key, index), chain);
        self.running.push(Running {
            list: key,
            action: index,
            started_ms: now_ms,
            tag_activities: vec![None; list.actions[index].tag_activities.len()],
        });
        if let Some(ability) = &list.actions[index].ability {
            for id in &ability.starts_cooldowns {
                self.start_cooldown(list, id, stats, (now_ms, server_ms));
            }
            if !ability.applies_on_enter.is_empty() {
                events.push(AbilityEvent::Effects(ability.applies_on_enter.clone()));
            }
        }
    }

    /// Actions `actions` of list `list` stop: their tag activities that are on end.
    fn leave(&mut self, list: &AbilityList, actions: &[usize]) {
        let key = list_key(list);
        let (left, kept): (Vec<Running>, Vec<Running>) = std::mem::take(&mut self.running)
            .into_iter()
            .partition(|running| running.list == key && actions.contains(&running.action));
        self.running = kept;
        for case in left
            .into_iter()
            .flat_map(|running| running.tag_activities)
            .flatten()
            .flatten()
        {
            self.change_tag(&case.tag, case.on_exit);
        }
    }

    /// The tag activities of the running actions of list `list` start when the conditions of
    /// their branches come to hold, with the case that holds then, and end when they stop
    /// holding.
    fn update_tags(
        &mut self,
        lists: &[&AbilityList],
        list: &AbilityList,
        holder: &dyn Holder,
        tags: &[&str],
        now_ms: u64,
    ) {
        let key = list_key(list);
        let mut changes = Vec::new();
        for (position, running) in self.running.iter().enumerate() {
            if running.list != key {
                continue;
            }
            let world = self.world(lists, list, holder, tags, Some(running.action), now_ms);
            let activities = &list.actions[running.action].tag_activities;
            for (index, (activity, state)) in
                activities.iter().zip(&running.tag_activities).enumerate()
            {
                let holds = activity.condition.holds_in(&world);
                match state {
                    None if holds => {
                        changes.push((position, index, Some(activity.case_in(&world).cloned())));
                    }
                    Some(_) if !holds => changes.push((position, index, None)),
                    _ => {}
                }
            }
        }
        for (position, index, now) in changes {
            let before = std::mem::replace(
                &mut self.running[position].tag_activities[index],
                now.clone(),
            );
            match (before, now) {
                (None, Some(Some(case))) => self.change_tag(&case.tag, case.on_enter),
                (Some(Some(case)), None) => self.change_tag(&case.tag, case.on_exit),
                _ => {}
            }
        }
    }

    /// Sets or unsets tag `tag` as `change` says.
    fn change_tag(&mut self, tag: &str, change: TagChange) {
        let on = self
            .tags_on
            .iter()
            .position(|own| own.eq_ignore_ascii_case(tag));
        match (change, on) {
            (TagChange::Set, None) => self.tags_on.push(tag.to_owned()),
            (TagChange::Unset, Some(at)) => {
                self.tags_on.remove(at);
            }
            _ => {}
        }
    }

    /// The running actions of list `list`.
    fn running_in<'a>(&'a self, list: &AbilityList) -> impl Iterator<Item = &'a Running> + use<'a> {
        let key = list_key(list);
        self.running
            .iter()
            .filter(move |running| running.list == key)
    }

    fn world<'a>(
        &'a self,
        lists: &'a [&'a AbilityList],
        list: &'a AbilityList,
        holder: &'a dyn Holder,
        tags: &[&'a str],
        current: Option<usize>,
        now_ms: u64,
    ) -> RunWorld<'a> {
        let mut all_tags = tags.to_vec();
        all_tags.extend(self.tags_on.iter().map(String::as_str));
        RunWorld {
            run: self,
            lists,
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
        condition.holds_in(&self.world(&[list], list, holder, &tags, None, now_ms))
    }
}

/// The identity of ability list `list`: its place in memory. The lists live in the content for
/// as long as the server runs, so the key of a list never changes or names another list.
fn list_key(list: &AbilityList) -> usize {
    std::ptr::from_ref(list).addr()
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
    /// The stance tags of the weapon in each main-hand option slot that holds one, by alias.
    option_tags: Vec<(&'static str, Vec<String>)>,
    effects: &'a super::status_effects::StatusEffects,
    rules: &'a StatusEffectRules,
    /// The slot aliases that hold an item ([`filled_slots`]).
    filled_slots: Vec<&'static str>,
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

    fn item_in_slot(&self, slot: &str) -> bool {
        self.filled_slots.contains(&slot)
    }

    fn tags_in_slot(&self, slot: &str) -> Vec<&str> {
        if slot == ACTIVE_MAIN_HAND {
            return self.tags();
        }
        self.option_tags
            .iter()
            .find(|(alias, _)| *alias == slot)
            .map(|(_, tags)| tags.iter().map(String::as_str).collect())
            .unwrap_or_default()
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

/// The mana action `index` of `list` needs to start: the highest of its activation's
/// `IfManaCost` cost, the spell costs of its own casts and those of the casts of the actions that
/// run while its fragment plays (`IfFragmentPlaying`: the follow-ups that cast its spells).
/// Inferred: retail's own check is the activation's `IfManaCost`, which 89 of the 111 actions
/// that start a cooldown lack; without this check such an action started its cooldown and its
/// cast was then refused for the mana.
fn chain_cost(list: &AbilityList, index: usize) -> f32 {
    let action = &list.actions[index];
    let own = action
        .ability
        .as_ref()
        .and_then(|ability| ability.mana_cost.as_deref())
        .and_then(|cost| list.mana_cost(cost));
    let follow_ups = list.actions.iter().filter(|other| {
        !action.fragment.is_empty()
            && other
                .activation
                .as_ref()
                .is_some_and(|activation| tests_fragment(activation, &action.fragment))
    });
    std::iter::once(action)
        .chain(follow_ups)
        .flat_map(|action| &action.casts)
        .filter_map(|cast| cast.spell.mana_cost)
        .chain(own)
        .fold(0.0, f32::max)
}

/// Whether `condition` tests that `fragment` plays (`IfFragmentPlaying`), outside a `Not`.
fn tests_fragment(condition: &Condition, fragment: &str) -> bool {
    match condition {
        Condition::All(parts) | Condition::Any(parts) => {
            parts.iter().any(|part| tests_fragment(part, fragment))
        }
        Condition::FragmentPlaying(Some(name)) => name.eq_ignore_ascii_case(fragment),
        _ => false,
    }
}

/// The slot aliases that `IfItemInSlot` tests (Observed in the player ability lists:
/// `MAIN_HAND_OPTION_1` and `_2` 762 times each, `ACTIVE_MAIN_HAND_WEAPON` 39,
/// `ACTIVE_OFF_HAND_WEAPON` 19) that hold an item of `gear`: the weapon in the hand, the off-hand
/// item worn with it, and each main-hand option slot with a weapon.
fn filled_slots(gear: &Gear) -> Vec<&'static str> {
    let mut filled = Vec::new();
    if gear.main_weapon().is_some() {
        filled.push(ACTIVE_MAIN_HAND);
    }
    if gear.off_hand_weapon().is_some() {
        filled.push(ACTIVE_OFF_HAND);
    }
    filled.extend(
        MAIN_HAND_OPTIONS
            .iter()
            .filter(|(_, slot_id)| gear.worn_item(*slot_id).is_some())
            .map(|(alias, _)| *alias),
    );
    filled
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
        // A silence or a stun keeps the ability keys from starting anything.
        if !self.player_control(entity).may_cast() {
            return;
        }
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
            let tags_of = |item: &str| {
                data.weapon_of_item(item)
                    .and_then(|weapon| data.stance(weapon.stance))
                    .map(|stance| stance_tags(&stance.tags))
            };
            let tags = tags_of(item).unwrap_or_default();
            let option_tags = player.gear.as_ref().map_or_else(Vec::new, |gear| {
                MAIN_HAND_OPTIONS
                    .iter()
                    .filter_map(|(alias, slot_id)| {
                        Some((*alias, tags_of(gear.worn_item(*slot_id)?)?))
                    })
                    .collect()
            });
            let mut abilities_on: Vec<&str> = player.masteries.spent_points().collect();
            abilities_on.extend(player.gear.iter().flat_map(|gear| {
                gear.worn_perks()
                    .into_iter()
                    .filter_map(|(perk, _)| content.perk_effects.ability_of(perk))
            }));
            let attributes = &player.attributes;
            let effects = &player.status_effects;
            // An attribute id is its place in `ATTRIBUTE_NAMES`; food adds its points.
            abilities_on.extend(content.attribute_bonuses.abilities(|attribute| {
                attributes
                    .value(&content.progression.points, attribute)
                    .saturating_add(effects.attribute_bonus(rules, attribute as usize))
            }));
            let stats = content
                .attribute_bonuses
                .stats_of(abilities_on.iter().copied());
            let holder = PlayerHolder {
                tags,
                option_tags,
                effects: &player.status_effects,
                rules,
                filled_slots: player.gear.as_ref().map_or_else(Vec::new, filled_slots),
                abilities_on,
                stats,
            };
            let events = player.abilities.tick(&lists, &holder, now_ms, server_ms);
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
        let takes_enemies = ENEMY_TARGETS.contains(&targets);
        let enemy_row = spell.damage_row.as_deref().filter(|_| takes_enemies);
        // A spell on enemies with status effects and no damage row puts its effects on them.
        let reaches_enemies =
            takes_enemies && (enemy_row.is_some() || !spell.status_effects.is_empty());
        let met_mob = reaches_enemies
            .then(|| first_on_line(&shot, caster.from, aim, &places))
            .flatten();
        let met_friend = (!spell.status_effects.is_empty() && FRIEND_TARGETS.contains(&targets))
            .then(|| {
                let friends = self.friend_places(caster.entity);
                first_on_line(&shot, caster.from, aim, &friends)
            })
            .flatten();
        let mut results = Vec::new();
        match (reaches_enemies, met_mob) {
            (true, Some((mob, flight_s))) => {
                let effects = (!spell.status_effects.is_empty())
                    .then_some((spell.status_effects.as_slice(), caster.number));
                let hit = ShotHit {
                    damage_id: enemy_row,
                    effects,
                };
                self.launch(&caster.connection, (shot, hit), mob, flight_s, now_ms);
            }
            (true, None) => results.push(Ok(AttackOutcome::Missed)),
            (false, _) => {}
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

    use nw_content::player_abilities::{
        AbilityCast, FragmentMarker, FragmentRow, FragmentTransition,
    };
    use nw_content::player_attacks::Flight;
    use nw_content::status_effects::{EffectSourceRow, StatusEffectSource};
    use nw_formats::mannequin::{AbilityUse, TagActivity};
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
                Condition::AbilityMove {
                    ability: SMITE.to_owned(),
                    back_ticks: 0,
                },
                Condition::ManaCost {
                    cost: "LifeStaff_Smite".to_owned(),
                    compare: ManaCompare::AtLeast,
                },
                Condition::Not(Box::new(Condition::Cooldown(SMITE.to_owned()))),
                Condition::Tag("Heal".to_owned()),
            ])),
            deactivation: None,
            tag_activities: vec![TagActivity {
                condition: Condition::True,
                cases: Vec::new(),
                default: Some(TagCase {
                    tag: "AttackAction".to_owned(),
                    on_enter: TagChange::Set,
                    on_exit: TagChange::Unset,
                }),
            }],
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
            tag_activities: Vec::new(),
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
                transitions: Vec::new(),
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

    /// The fire staff's Incinerate: the intro's fragment moves on to the loop by a transition
    /// clip, and the loop starts the cooldown. Here the loop is in the weapon's second list,
    /// whose one action is fewer than the first list's two running ones (the run once read the
    /// first list's action numbers in the second list and panicked).
    #[test]
    fn a_transition_clip_moves_the_running_action_on_into_another_list() {
        const LOOP: &str = "Ability_Life_Staff_Smite_Loop";
        const LOOP_COOLDOWN_MS: u64 = 9_000;
        const TRANSITION_MS: u64 = 500;
        let mut first = list();
        first.fragments[0].transitions = vec![FragmentTransition {
            action: LOOP.to_owned(),
            input: None,
            condition: None,
            start_ms: TRANSITION_MS,
            end_ms: FRAGMENT_MS,
        }];
        let second = AbilityList {
            weapon_classes: vec!["LifeStaff".to_owned()],
            actions: vec![ListAction {
                name: LOOP.to_owned(),
                fragment: LOOP.to_owned(),
                activation: None,
                deactivation: None,
                tag_activities: Vec::new(),
                ability: Some(AbilityUse {
                    starts_cooldowns: vec![LOOP.to_owned()],
                    ..AbilityUse::default()
                }),
                casts: Vec::new(),
            }],
            fragments: vec![FragmentRow {
                name: LOOP.to_owned(),
                length_ms: FRAGMENT_MS,
                markers: Vec::new(),
                transitions: Vec::new(),
                hit: None,
            }],
            cooldowns_ms: BTreeMap::from([(LOOP.to_owned(), LOOP_COOLDOWN_MS)]),
            mana_costs: BTreeMap::new(),
        };
        let lists = [&first, &second];
        let mut run = AbilityRun::default();
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        run.tick(&lists, &Staff, 0, SERVER_MS);
        run.tick(&lists, &Staff, 100, SERVER_MS);
        assert_eq!(run.running.len(), 2, "the start and its follow-up");
        run.take_cooldown_changes();
        run.tick(&lists, &Staff, TRANSITION_MS, SERVER_MS);
        let loop_key = list_key(&second);
        assert!(
            run.running.iter().any(|running| running.list == loop_key),
            "the loop runs in the second list"
        );
        let changes = run.take_cooldown_changes();
        assert_eq!(
            changes
                .iter()
                .map(|(_, cooldown)| (cooldown.ability, cooldown.end_us - cooldown.start_us))
                .collect::<Vec<_>>(),
            [(lowercase_crc32(LOOP), LOOP_COOLDOWN_MS * US_PER_MS)]
        );
    }

    #[test]
    fn a_key_press_runs_the_ability_chain_once_with_its_cooldown_mana_and_cast() {
        let list = list();
        let mut run = AbilityRun::default();
        assert!(
            run.tick(&[&list], &Staff, 0, SERVER_MS).is_empty(),
            "no press"
        );
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        let start = run.tick(&[&list], &Staff, 0, SERVER_MS);
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
            run.tick(&[&list], &Staff, 100, SERVER_MS).is_empty(),
            "before the marker"
        );
        let cast = run.tick(&[&list], &Staff, MARKER_START_MS, SERVER_MS);
        assert!(
            matches!(&cast[..], [AbilityEvent::Cast { spell, kind: CastKind::Raycast, .. }] if spell.id == "LifeStaff_Smite"),
            "{cast:?}"
        );
        assert!(
            (run.mana.current - (MANA_MAX - COST)).abs() < 1.0,
            "spent, a little back"
        );
        assert!(
            run.tick(&[&list], &Staff, MARKER_START_MS + 50, SERVER_MS)
                .is_empty(),
            "one cast per run"
        );
        // A second press during the cooldown starts nothing.
        run.press(&[key_1(11)], mapped, no_inputs, 2_000);
        assert!(run.tick(&[&list], &Staff, 2_000, SERVER_MS).is_empty());
        assert!(run.take_cooldown_changes().is_empty());
        // After the cooldown it runs again.
        run.press(&[key_1(12)], mapped, no_inputs, 5_000);
        assert_eq!(run.tick(&[&list], &Staff, 5_000, SERVER_MS).len(), 1);
    }

    /// An ability whose start does not test the mana itself (89 of the 111 retail starts that set
    /// a cooldown lack `IfManaCost`) does not start with too little mana for the spell its
    /// follow-up casts: no cooldown, no effect; with the mana it starts.
    #[test]
    fn too_little_mana_starts_no_cooldown_and_no_spell() {
        let mut list = list();
        let start = &mut list.actions[0];
        if let Some(Condition::All(parts)) = start.activation.as_mut() {
            parts.retain(|part| !matches!(part, Condition::ManaCost { .. }));
        }
        if let Some(ability) = start.ability.as_mut() {
            ability.mana_cost = None;
        }
        assert!(
            (chain_cost(&list, 0) - COST).abs() < f32::EPSILON,
            "the follow-up's spell"
        );
        let mut run = AbilityRun::default();
        run.mana.current = COST - 1.0;
        run.press(&[key_1(10)], mapped, no_inputs, 0);
        assert!(run.tick(&[&list], &Staff, 0, SERVER_MS).is_empty());
        assert!(run.take_cooldown_changes().is_empty(), "no cooldown");
        run.mana.current = MANA_MAX;
        run.press(&[key_1(11)], mapped, no_inputs, 100);
        assert_eq!(run.tick(&[&list], &Staff, 100, SERVER_MS).len(), 1);
        assert_eq!(run.take_cooldown_changes().len(), 1);
    }

    /// `IfItemInSlot` holds for the slots of the gear that hold an item: the weapon in the hand
    /// and the main-hand options with a weapon, not an empty option.
    #[test]
    fn the_filled_slots_are_those_of_the_worn_weapons() {
        let gear = super::super::gear::tests::gear_with_staff();
        let filled = filled_slots(&gear);
        assert!(filled.contains(&ACTIVE_MAIN_HAND));
        assert!(filled.contains(&"MAIN_HAND_OPTION_1") && filled.contains(&"MAIN_HAND_OPTION_2"));
        assert!(!filled.contains(&"MAIN_HAND_OPTION_3"), "an empty option");
        assert_eq!(
            filled.contains(&ACTIVE_OFF_HAND),
            gear.off_hand_weapon().is_some()
        );
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
        run.tick(&[&list], &FocusedStaff, 0, SERVER_MS);
        assert!((run.mana.max - FOCUS_MANA_MAX).abs() < 1e-3, "retail 110");
        run.mana.current = 0.0;
        run.tick(&[&list], &FocusedStaff, 1_000, SERVER_MS);
        assert!(
            (run.mana.current - RETAIL_MANA_REGEN_PER_S).abs() < 1e-3,
            "the retail rate at Focus 250"
        );
        run.mana.current = run.mana.max;
        run.press(&[key_1(10)], mapped, no_inputs, 2_000);
        run.tick(&[&list], &FocusedStaff, 2_000, SERVER_MS);
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

    /// A projectile spell on enemies with status effects and no damage row puts its effects on
    /// the mob it meets and hurts it not.
    #[test]
    fn an_effect_spell_without_a_damage_row_puts_its_effects_on_the_mob() {
        let (mut world, caster) = spell_fight(&[MOB_X]);
        with_ground_effect(&mut world);
        let curse = SpellRow {
            damage_row: None,
            status_effects: vec![GROUND.to_owned()],
            ..bolt(None)
        };
        let fired = world.cast_ability_spell(caster, "Test", &curse, CastKind::Plain, NOW);
        assert!(fired.is_empty(), "on its way: {fired:?}");
        assert!(
            hit_mobs(world.player_swings_due(NOW + LATER_MS)).is_empty(),
            "no hit"
        );
        let held = world.mob_effects.get(&mob_entity(0)).expect("effects");
        assert!(holds_effect(held, &world.content.status_effects, GROUND));
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
        run.tick(&[&list], &staff, 0, SERVER_MS);
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
    use std::collections::BTreeSet;
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

    /// A player with a weapon of stance `tags`, no inputs, effects or upgrades.
    /// A player with a weapon of stance `tags` and the status effects its abilities gave it.
    struct Armed {
        tags: &'static [&'static str],
        effects: Vec<String>,
    }

    impl Holder for Armed {
        fn tags(&self) -> Vec<&str> {
            self.tags.to_vec()
        }

        fn has_effect(&self, effect: &str) -> bool {
            self.effects
                .iter()
                .any(|own| own.eq_ignore_ascii_case(effect))
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

    /// Each weapon's ability list class and the stance tags of its weapons (the
    /// `MannequinTag` cells of the player attack table).
    const WEAPONS: [(&str, &[&str]); 15] = [
        ("Sword", &["1H_Melee", "Sword"]),
        ("Rapier", &["1H_Melee", "Rapier"]),
        ("Hatchet", &["1H_Melee", "Axe", "Throwable"]),
        ("Flail", &["1H_Melee", "Flail"]),
        ("Spear", &["2H_Melee", "Spear", "Throwable"]),
        ("2HAxe", &["2H_Melee", "GreatAxe"]),
        ("2hhammer", &["2H_Melee", "Warhammer"]),
        ("GreatSword", &["2H_Melee", "GreatSword"]),
        ("Bow", &["Bow"]),
        ("Musket", &["2H_Ranged", "Rifle"]),
        ("Blunderbuss", &["2H_Ranged", "Blunderbuss"]),
        ("FireStaff", &["MagicStaff", "Fire"]),
        ("LifeStaff", &["MagicStaff", "Heal"]),
        ("IceMagic", &["MagicGauntlet", "Ice"]),
        ("VoidGauntlet", &["MagicGauntlet", "VoidGauntlet"]),
    ];

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

    /// A player's steps for one ability, each in one input message, by tick: the key, a click
    /// about 0.5 s later (the spear's ready step throws on a click from 433 to 600 ms, and clears
    /// the press on its start: `ClearActiveAbilityMoveInput`), a click that fires an aimed
    /// ability (the shoot and attack inputs) about 0.7 s after the key, then the key again (a
    /// second press fires some aimed abilities) about 1.2 s after the first. Each step's message
    /// [`STEP_TICKS`] later holds no key and no input.
    const STEPS: [(u64, bool, bool); 4] = [
        (0, true, false),
        (15, false, true),
        (21, false, true),
        (36, true, false),
    ];
    /// The inputs of a click.
    const CLICK_INPUTS: [&str; 3] = ["attack_shoot", "attack_primary", "aim_shoot_release"];
    /// How many ticks after a step its key or click ends.
    const STEP_TICKS: u64 = 3;

    /// The input record of frame `frame`, with the first ability key down or not.
    fn record(frame: u16, key: bool) -> InputRecord {
        if key {
            key_1(frame)
        } else {
            InputRecord {
                u16_after_mask: frame,
                ..InputRecord::default()
            }
        }
    }

    /// The abilities whose fire step the [`STEPS`] do not make: the bow's ability shots and the
    /// two storms fire through an action that no transition of the parsed fragments leads to
    /// (`Bow_Penetrating_Shoot_A1`, `Ability_Fire_Staff_Storm_Loop`, `IceMagic_IceStorm_Cast`;
    /// Unknown: the server reads one option per fragment name, and the client may pick a
    /// fragment option by tags), and the ice pylon starts its cooldown when its placed pylon's
    /// ownership message comes (`IfOwnershipMessageReceived`; the server places no pylon).
    const HELD_AIM_OR_PLACED: [&str; 6] = [
        "Ability_Bow_PenetratingShot",
        "Ability_Bow_PoisonShot",
        "Ability_Bow_RainofArrows",
        "Ability_Firestaff_MeteorShower",
        "IceMagic_IcePylon",
        "IceMagic_IceStorm",
    ];

    /// Every active ability of every weapon (an ability with a `Cooldowns_Player` row) but the
    /// [`HELD_AIM_OR_PLACED`] ones starts from the [`STEPS`] of a player with full mana and no
    /// cooldown running: it starts its cooldown.
    #[test]
    fn every_weapon_ability_starts_from_its_key() {
        let table = table();
        let mut refused = Vec::new();
        let mut started = 0;
        for (class, tags) in WEAPONS {
            let lists = table.lists_for(&[class]);
            // The abilities a key of this weapon starts: the shared musket and blunderbuss list
            // names Stopping Power's cooldown, whose key action is in the musket's own list.
            let keyed: BTreeSet<&String> = lists
                .iter()
                .flat_map(|list| &list.actions)
                .filter_map(|action| action.ability.as_ref()?.move_name.as_ref())
                .collect();
            let abilities: BTreeSet<&String> = lists
                .iter()
                .flat_map(|list| list.cooldowns_ms.keys())
                .filter(|ability| keyed.contains(ability))
                .collect();
            assert!(!abilities.is_empty(), "{class} has abilities");
            for ability in abilities {
                let crc = lowercase_crc32(ability);
                let mapped = |key: usize| (key == 0).then_some(crc);
                let mut run = AbilityRun::default();
                let mut holder = Armed {
                    tags,
                    effects: Vec::new(),
                };
                let mut frame = 10;
                for tick in 0..=RUN_MS / TICK_MS {
                    let now = tick * TICK_MS;
                    for (at, key, click) in STEPS {
                        if tick != at && tick != at + STEP_TICKS {
                            continue;
                        }
                        let down = tick == at;
                        let inputs = |_: &InputRecord| {
                            CLICK_INPUTS
                                .iter()
                                .filter(|_| click && down)
                                .map(|input| (*input).to_owned())
                                .collect()
                        };
                        frame += 1;
                        run.press(&[record(frame, key && down)], mapped, inputs, now);
                    }
                    for event in run.tick(&lists, &holder, now, SERVER_MS) {
                        if let AbilityEvent::Effects(effects) = event {
                            holder.effects.extend(effects);
                        }
                    }
                }
                if run
                    .take_cooldown_changes()
                    .iter()
                    .any(|(_, cooldown)| cooldown.ability == crc)
                {
                    started += 1;
                } else if !HELD_AIM_OR_PLACED.contains(&ability.as_str()) {
                    refused.push(format!("{class} {ability}"));
                }
            }
        }
        assert!(
            refused.is_empty(),
            "{started} started; refused: {refused:#?}"
        );
    }

    /// `playeractions_ability_musket.actionlist`: Shooter's Stance readies, aims, and a click in
    /// the aim shoots and starts its cooldown; then the musket reloads. Before, the aim ended at
    /// once (its exit test on a key press of this frame read the press as 20 frames long) and the
    /// click reloaded (`ReloadRequired`, set by a branch only after a shot, counted as always
    /// set).
    #[test]
    fn shooters_stance_aims_and_a_click_shoots_and_starts_its_cooldown() {
        const STANCE: &str = "Ability_Musket_ShootersStance";
        /// `Cooldowns_Player` `Ability_Musket_ShootersStance`: 18 s.
        const STANCE_COOLDOWN_MS: u64 = 18_000;
        /// The key, then a click 0.7 s later (in the aim, which starts at 333 ms).
        const STANCE_STEPS: [(u64, bool, bool); 2] = [(0, true, false), (21, false, true)];
        let table = table();
        let lists = table.lists_for(&["Musket"]);
        let crc = lowercase_crc32(STANCE);
        let mapped = |key: usize| (key == 0).then_some(crc);
        let mut run = AbilityRun::default();
        let holder = Armed {
            tags: &["2H_Ranged", "Rifle"],
            effects: Vec::new(),
        };
        let mut frame = 10;
        let mut actions: Vec<String> = Vec::new();
        for tick in 0..=RUN_MS / TICK_MS {
            let now = tick * TICK_MS;
            for (at, key, click) in STANCE_STEPS {
                if tick != at && tick != at + STEP_TICKS {
                    continue;
                }
                let down = tick == at;
                let inputs = |_: &InputRecord| {
                    CLICK_INPUTS
                        .iter()
                        .filter(|_| click && down)
                        .map(|input| (*input).to_owned())
                        .collect()
                };
                frame += 1;
                run.press(&[record(frame, key && down)], mapped, inputs, now);
            }
            run.tick(&lists, &holder, now, SERVER_MS);
            for running in &run.running {
                let list = lists
                    .iter()
                    .find(|list| list_key(list) == running.list)
                    .expect("its list");
                let name = &list.actions[running.action].name;
                if actions.last() != Some(name) {
                    actions.push(name.clone());
                }
            }
        }
        assert_eq!(
            actions,
            [
                "Musket_ShootersStance_Ready",
                "Musket_ShootersStance_Aim_SubState",
                "Musket_ShootersStance_Shoot",
                "Firearm_Reload_SubState",
            ]
        );
        let changes = run.take_cooldown_changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        let (key, cooldown) = changes[0];
        assert_eq!((key, cooldown.ability), (0, crc));
        assert_eq!(
            cooldown.end_us - cooldown.start_us,
            STANCE_COOLDOWN_MS * US_PER_MS
        );
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
            for event in run.tick(&lists, &LifeStaff, now, SERVER_MS) {
                if let AbilityEvent::Cast { spell, .. } = event {
                    casts.push(spell.id.clone());
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
