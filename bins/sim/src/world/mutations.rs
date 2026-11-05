// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mutated expeditions in the sim (run 15 dungeon study, `local-data/scratch-work/run15/
//! dungeon2/REPORT-mutations.md`; rules in [`nw_content::mutations`]):
//!
//! - Every player's client gets the week's mutations once it spawned, and again when the week
//!   turns: `GameModeMutationSchedulerReplicatedState` (2201) as component 6 of entity slot 2
//!   (Observed in 549 retail ledgers; [`schedule_group`]).
//! - A 5037's byte is the difficulty the player asks for (best guess: 0 in every capture, all of
//!   which were normal runs; [`difficulty_of_request`]). An activation of a mutated difficulty
//!   is refused with `ScheduledNotAvailable` when the game mode is not mutated this week,
//!   `MutationLevelReq` below the difficulty's `MatchmakingMinLevel`, and
//!   `MutationDifficultyReq` above the rank the player has for the week's slot of the game mode
//!   ([`SimWorld::mutation_refusal`]).
//! - The instance sim takes the mutation from the first player's stay
//!   ([`SimWorld::adopt_mutation`]) and mutates every live mob: health and damage factors, and
//!   the status effects ([`SimWorld::mutate_live_mobs`]); the status effects and the players'
//!   curse afflictions wait in a queue for the status effect runtime
//!   ([`SimWorld::take_mutation_effects`]).
//! - A mob kill rolls the mutation's tags and its injected creature tables
//!   ([`SimWorld::mutation_kill_loot`]); a completed run grants the difficulty's completion event
//!   and raises the players' rank ([`SimWorld::complete_mutated_instance`]).

use std::collections::HashMap;
use std::sync::Arc;

use nw_bus::InstanceStay;
use nw_content::instances::InstanceMode;
use nw_content::mutations::{AppliedEffect, MutationCatalog, MutationContext, MutationWeek};
use nw_messages::items::{FIRST_VERSION, FullList};
use nw_messages::{
    ActivateResponse, BundleGroup, MessageError, MutationSchedulerState, MutationSetEntry,
};
use nw_sim_core::Health;

use super::quest_objects::grant;
use super::{Player, SimWorld, unix_ns};
use nw_content::rewards::Reward;

/// The 5037 byte of a normal run (Observed: every retail 5037).
const NORMAL_RUN: u8 = 0;
/// Nanoseconds in a second.
const NS_PER_SECOND: u64 = 1_000_000_000;
/// The rank of a mutated slot a player has without one (best guess: difficulty 1 is open to
/// everyone of the level).
const FIRST_RANK: u8 = 1;
/// The potency of a curse's global affliction on a player (an affliction has none in its row).
const AFFLICTION_POTENCY: f64 = 1.0;
/// A completion event's item, once.
const ONE: u32 = 1;

/// The difficulty a 5037 asks for: `None` for a normal run.
pub(super) const fn difficulty_of_request(byte: u8) -> Option<u8> {
    if byte == NORMAL_RUN { None } else { Some(byte) }
}

/// What a player's client has of the weekly mutations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ScheduleView {
    /// The start of the week it has, Unix seconds.
    sent_week: Option<u64>,
    /// The version of its set list.
    version: Option<u64>,
}

/// The mutation of the instance an instance sim runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InstanceMutation {
    /// The week's set and the difficulty.
    pub(crate) context: MutationContext,
    /// The set's slot among the sets of the week of the activation: the rank a completion
    /// raises, also when the run ends in the next week.
    slot: Option<usize>,
    /// The scaled maximum health of each mutated mob, by entity: a mob whose maximum differs
    /// (it respawned) is mutated again.
    mutated: HashMap<u64, u32>,
    /// Status effects for the status effect runtime: (entity number, effect).
    effects: Vec<(u64, AppliedEffect)>,
}

impl InstanceMutation {
    /// The mutation `context` of slot `slot` of its week, no mob mutated yet.
    pub(crate) fn new(context: MutationContext, slot: Option<usize>) -> Self {
        Self {
            context,
            slot,
            mutated: HashMap::new(),
            effects: Vec::new(),
        }
    }
}

/// `value` times `factor`, rounded, at least 1.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the u32 range first"
)]
fn scaled(value: u32, factor: f64) -> u32 {
    (f64::from(value) * factor)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

/// The scheduler state of `week` at list version `version`.
fn schedule_state(week: &MutationWeek, version: u64) -> MutationSchedulerState {
    MutationSchedulerState {
        sets: Some(FullList {
            version: Some(version),
            elements: week
                .sets
                .iter()
                .map(|set| {
                    let [game_mode, elemental, promotion, curse] = set.crcs();
                    MutationSetEntry {
                        game_mode,
                        elemental,
                        promotion,
                        curse,
                    }
                })
                .collect(),
        }),
        cadence_start_ns: Some(week.start_ns()),
    }
}

/// The slot-2 group of the week at `unix_s`, when `view` does not have that week.
///
/// # Errors
///
/// Returns [`MessageError`] when the state cannot be encoded.
pub(super) fn schedule_group(
    mutations: &MutationCatalog,
    unix_s: u64,
    view: &mut ScheduleView,
) -> Result<Option<BundleGroup>, MessageError> {
    if mutations.modes().is_empty() {
        return Ok(None);
    }
    let week = mutations.week_at(unix_s);
    if week.sets.is_empty() || view.sent_week == Some(week.start_unix_s) {
        return Ok(None);
    }
    let version = view.version.map_or(FIRST_VERSION, |version| version + 1);
    let group = schedule_state(&week, version).group()?;
    view.sent_week = Some(week.start_unix_s);
    view.version = Some(version);
    Ok(Some(group))
}

impl SimWorld {
    /// Unix seconds now.
    pub(super) fn unix_s(&self) -> u64 {
        unix_ns(self.server_ms) / NS_PER_SECOND
    }

    /// Why player `player` may not activate `mode` at mutation difficulty `difficulty`; `None`
    /// when it may.
    pub(super) fn mutation_refusal(
        &self,
        player: &Player,
        mode: &InstanceMode,
        difficulty: u8,
        shown_level: u32,
    ) -> Option<ActivateResponse> {
        let mutations = &self.content.mutations;
        let week = mutations.week_at(self.unix_s());
        let (Some(slot), Some(rules)) = (
            week.sets
                .iter()
                .position(|set| set.game_mode.eq_ignore_ascii_case(&mode.id)),
            mutations.difficulty(difficulty),
        ) else {
            return Some(ActivateResponse::ScheduledNotAvailable);
        };
        if shown_level < rules.min_level {
            return Some(ActivateResponse::MutationLevelReq);
        }
        let rank = player
            .arena
            .rank(slot)
            .unwrap_or(FIRST_RANK)
            .max(FIRST_RANK);
        (difficulty > rank).then_some(ActivateResponse::MutationDifficultyReq)
    }

    /// The instance mutation, when this sim runs a mutated instance.
    pub fn instance_mutation(&self) -> Option<&MutationContext> {
        self.instance_run
            .as_ref()?
            .mutation()
            .map(|mutation| &mutation.context)
    }

    /// The instance sim takes the mutation of `stay` (the first player's that arrives with one)
    /// and mutates every live mob; the arriving player `number` gets the curse's affliction.
    pub(super) fn adopt_mutation(&mut self, stay: &InstanceStay, number: u64) {
        let Some(difficulty) = stay.mutation_difficulty else {
            return;
        };
        let activated_s = stay.activated_ns / NS_PER_SECOND;
        let content = Arc::clone(&self.content);
        let Some(run) = self.instance_run.as_mut() else {
            return;
        };
        if run.mutation().is_none() {
            let Some(context) = content
                .mutations
                .context(&stay.game_mode, activated_s, difficulty)
            else {
                tracing::warn!(game_mode = %stay.game_mode, difficulty, "no mutation this week");
                return;
            };
            tracing::info!(
                game_mode = %stay.game_mode,
                difficulty,
                elemental = %context.set.elemental,
                promotion = %context.set.promotion,
                curse = %context.set.curse,
                "mutated instance"
            );
            let slot = content
                .mutations
                .week_at(activated_s)
                .sets
                .iter()
                .position(|set| set.game_mode.eq_ignore_ascii_case(&context.set.game_mode));
            run.set_mutation(InstanceMutation::new(context, slot));
        }
        if let Some(mutation) = run.mutation_mut()
            && let Some(affliction) = content.mutations.player_affliction(&mutation.context)
        {
            mutation.effects.push((
                number,
                AppliedEffect {
                    status_id: affliction.to_owned(),
                    potency: AFFLICTION_POTENCY,
                },
            ));
        }
        self.mutate_live_mobs();
    }

    /// Mutates every live mob of the mutated instance that is not mutated yet (a new spawn, or
    /// a respawn whose health was reset). The encounter runtime calls it after its spawns; the
    /// instance beat calls it too.
    pub fn mutate_live_mobs(&mut self) {
        if self.instance_mutation().is_none() {
            return;
        }
        for entity in self.mobs.live_entities() {
            self.mutate_mob(entity);
        }
    }

    /// Mutates mob `entity` when the instance is mutated and the mob is not mutated yet: its
    /// maximum health and damage scale by its creature type, and its status effects wait in
    /// the queue.
    pub fn mutate_mob(&mut self, entity: u64) {
        let content = Arc::clone(&self.content);
        let Some(context) = self.instance_mutation().cloned() else {
            return;
        };
        let Some(max) = self.mobs.get(entity).map(|mob| mob.health.max()) else {
            return;
        };
        let already = self
            .instance_run
            .as_ref()
            .and_then(|run| run.mutation())
            .and_then(|mutation| mutation.mutated.get(&entity).copied());
        if already == Some(max) {
            return;
        }
        let creature = self
            .mobs
            .vitals_of(entity)
            .and_then(|vitals| content.kill_rewards.creature_type(vitals));
        let dice = &mut self.dice;
        let change = content
            .mutations
            .mob_mutation(&context, creature, &mut |below| dice.below(below));
        let scaled_max = scaled(max, change.health_factor);
        if let Some(mob) = self.mobs.get_mut(entity) {
            mob.health = Health::full(scaled_max);
            mob.attack.damage_mod *= change.damage_factor;
        }
        if let Some(mutation) = self
            .instance_run
            .as_mut()
            .and_then(|run| run.mutation_mut())
        {
            mutation.mutated.insert(entity, scaled_max);
            mutation
                .effects
                .extend(change.effects.into_iter().map(|effect| (entity, effect)));
        }
    }

    /// The status effects and afflictions the mutation put on mobs and players since the last
    /// call, for the status effect runtime: (entity number, effect).
    pub fn take_mutation_effects(&mut self) -> Vec<(u64, AppliedEffect)> {
        self.instance_run
            .as_mut()
            .and_then(|run| run.mutation_mut())
            .map(|mutation| std::mem::take(&mut mutation.effects))
            .unwrap_or_default()
    }

    /// The mutation's loot tags, appended to `tags` (a mob's own `LootTags`); `tags` alone
    /// outside a mutated instance.
    pub(super) fn mutation_loot_tags(&self, tags: &[String]) -> Vec<String> {
        let mut all = tags.to_vec();
        if let Some(context) = self.instance_mutation() {
            all.extend(self.content.mutations.loot(context).tags);
        }
        all
    }

    /// The items of the mutation's injected creature tables for a kill by player `killer` of
    /// mob `mob`, rolled with the mutation's tags; none outside a mutated instance.
    pub(super) fn mutation_kill_loot(&mut self, killer: u64, mob: u64) -> Vec<(String, u32)> {
        let Some(context) = self.instance_mutation().cloned() else {
            return Vec::new();
        };
        let content = Arc::clone(&self.content);
        let loot = content.mutations.loot(&context);
        let reached = self.reached_loot_limits(killer);
        let context = self.loot_context(killer, Some((mob, &loot.tags)), &reached);
        let dice = &mut self.dice;
        let rolls: Vec<_> = loot
            .creature_tables
            .iter()
            .map(|table| {
                content
                    .loot
                    .roll(table, context, &mut |below| dice.below(below))
            })
            .collect();
        rolls
            .into_iter()
            .flat_map(|rolled| self.limit_loot(killer, rolled))
            .collect()
    }

    /// The mutated run is complete (the encounter runtime calls it at the last boss): every
    /// player in the instance gets the difficulty's completion event (its coins, experience and
    /// item) and the next difficulty of the slot of the week it was activated in.
    pub fn complete_mutated_instance(&mut self, now_ms: u64) {
        let Some((context, slot)) = self
            .instance_run
            .as_ref()
            .and_then(|run| run.mutation())
            .map(|mutation| (mutation.context.clone(), mutation.slot))
        else {
            return;
        };
        let content = Arc::clone(&self.content);
        let mutations = &content.mutations;
        let reward = mutations
            .completion_event(&context)
            .and_then(|event| {
                let dice = &mut self.dice;
                content
                    .rewards
                    .rolled(event, &mut |below| dice.below(below))
            })
            .unwrap_or_default();
        let next_rank = mutations
            .highest_difficulty()
            .map_or(context.difficulty, |highest| {
                context.difficulty.saturating_add(1).min(highest)
            });
        let numbers: Vec<u64> = self
            .by_connection
            .values()
            .filter_map(|entity| self.world.get::<Player>(*entity))
            .map(|player| player.number)
            .collect();
        for number in numbers {
            let Some(entity) = self.player_by_number(number) else {
                continue;
            };
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                grant(
                    &mut player,
                    &Reward {
                        item: None,
                        ..reward.clone()
                    },
                    &content.gameplay,
                );
                if let Some(slot) = slot {
                    player.arena.raise_rank(slot, next_rank);
                }
            }
            self.push_arena_change(entity);
            if let Some((item, _)) = &reward.item {
                self.give_loot(number, vec![(item.clone(), ONE)], now_ms);
            }
        }
        tracing::info!(game_mode = %context.set.game_mode, difficulty = context.difficulty, "mutated run complete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nw_content::mutations::MutationSet;

    fn week() -> MutationWeek {
        MutationWeek {
            start_unix_s: 1_792_479_600,
            sets: vec![MutationSet {
                game_mode: "DungeonCutlassKeys00".to_owned(),
                elemental: "Ice".to_owned(),
                promotion: "Barbaric".to_owned(),
                curse: "Fiendish".to_owned(),
            }],
        }
    }

    #[test]
    fn the_state_is_the_retail_entry_of_the_week() {
        let state = schedule_state(&week(), FIRST_VERSION);
        let sets = state.sets.expect("sets");
        assert_eq!(sets.version, Some(FIRST_VERSION));
        assert_eq!(
            sets.elements,
            [MutationSetEntry {
                game_mode: 0x2348_25a2,
                elemental: 0xcb4f_840e,
                promotion: 0xcb5e_7b4d,
                curse: 0xa78a_7b02,
            }]
        );
        assert_eq!(state.cadence_start_ns, Some(1_792_479_600_000_000_000));
    }

    #[test]
    fn a_zero_byte_is_a_normal_run_and_health_scales_rounded() {
        assert_eq!(difficulty_of_request(0), None);
        assert_eq!(difficulty_of_request(3), Some(3));
        assert_eq!(scaled(1_000, 3.2), 3_200);
        assert_eq!(scaled(3, 1.5), 5);
        assert_eq!(scaled(0, 2.0), 1);
    }

    #[test]
    fn a_world_without_mutable_expeditions_sends_no_schedule() {
        let world = crate::world::tests::world_with_spawns(Vec::new());
        let mut view = ScheduleView::default();
        assert_eq!(
            schedule_group(&world.content.mutations, world.unix_s(), &mut view),
            Ok(None)
        );
        assert_eq!(view, ScheduleView::default());
    }
}
