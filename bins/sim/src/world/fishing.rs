// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fishing (run 15 fishing study; retail `open-world/Fishing`, `gather_node_20260912` and the
//! feed-the-fire side quest): the server's fishing state machine, replicated as the own fishing
//! state ([`FishingComponentState`]), driven by the client's keys and its acknowledgements.
//!
//! The fishing key takes the pole of paperdoll slot 37 out (`StartedEquippingFishing`, then
//! `FishingEquipped` [`EQUIP_MS`] later) and puts it away (`FishingEnded` and `FishingUnequipped`
//! [`UNEQUIP_MS`] later). With the pole out, the bait key opens the bait window
//! (`ApplyingBait`); its 2309 applies a bait (answered by 557) and the pole is out again. The
//! attack key charges a cast while it is held (`CastStart`; the cast bar fills in 2 s, then falls
//! back) and casts at its release (`CastEnd`): the bobber flies and lands (2444, then
//! `CastHitWater` and `FishingStarted`, or `Failed_CastHitLand` on land). A fish nibbles after
//! the water's (or the hotspot's) nibble time, bites after the bite time, and the bite window
//! stays open 1 s; the attack key in it sets the hook (3172 with the catch, `HookHit`), a press
//! during the nibble misses (`Failed_HookMissedTooEarly`), no press misses too
//! (`Failed_HookMissedTooLate`), and a press while waiting ends the cast. After the hook the line
//! rests (`ReelingRecover`); holding the attack key reels (`ReelingActive`). Reeling brings the
//! fish in at the pole's speed and strains the line, resting lets the fish swim out and the line
//! recover; the fish fights or tires in 2 s blocks after its behavior's pattern. A line at the top
//! tension breaks (`Failed_LineBreak`); a fish brought all the way in is caught
//! (`Succeeded_FishCaught`): the item, the fishing experience and the catch event's rewards.
//! After a catch or a failure the cast ends (`FishingEnded`, `FishingEquipped`).
//!
//! The bait goes on the hook at the cast's end (Observed: the bag lost one at `CastEnd`, twice)
//! and comes back after the cast unless its `FishingData` loss chance takes it.

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::fishing::{
    Behavior, DayPhase, FISHING_DATA, FISHING_TRADESKILL, FishStateMultipliers, Pole, Range,
    RollModifiers, WaitRanges, WaterType,
};
use nw_content::loot::RollBonus;
use nw_content::rewards::Reward;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::client_input::InputRecord;
use nw_messages::fishing::{
    FISHING_COMPONENT, FISHING_FACET, FishCatch, FishingClientMsg, FishingComponentState,
    FishingServerMsg, FishingState, RollModifier, StateTransition,
};
use nw_messages::items::FIRST_VERSION;
use nw_messages::{BundleMember, FacetHeader, MemberState, OBSERVED_DAY_PHASE, ServerMessage};
use nw_sim_core::character::yaw_axes;
use nw_sim_core::{Ground, Position};

use super::attack::counter_is_newer;
use super::quest_objects::grant;
use super::tradeskill::progression_gain;
use super::{Body, Player, SimWorld, send_now, unix_ns};
use crate::objectives::{Event, FishCaught, FishingStep};

/// The paperdoll slot of the fishing pole (`PaperdollSlotTypes` `FISHING_SLOT`, `FUN_145cac380`).
pub(super) const FISHING_SLOT: u32 = 37;
/// How long the pole takes to come out (retail 534 ms from `StartedEquippingFishing` to
/// `FishingEquipped`, `open-world/Fishing`; 532 to 549 ms in three sessions).
const EQUIP_MS: u64 = 534;
/// How long the pole takes to go away (retail 684 ms from the key to `FishingEnded` and
/// `FishingUnequipped`; 668 to 684 ms in three sessions).
const UNEQUIP_MS: u64 = 684;
/// The cast bar: 50 per second of the held key, full at 100, then back down (`FUN_1430f5a70`).
const CAST_BAR_PER_S: f64 = 50.0;
const CAST_BAR_FULL: f64 = 100.0;
/// The bobber leaves the rod's tip, this far ahead of the player (the client measures the reel
/// distance from the player plus 2 m along its facing, `FUN_1430f0f40`).
const ROD_TIP_M: f32 = 2.0;
/// The bobber's flight: retail landed it `65 ms * distance - 480 ms` after `CastEnd` within
/// 60 ms in 8 casts of two sessions (Inferred fit; the client's flight is physics).
const FLIGHT_MS_PER_M: f64 = 65.0;
const FLIGHT_OFFSET_MS: f64 = 480.0;
/// The reel path back to the player: steps of 0.5 m, at most 500, and a cast distance above 100 m
/// is held at 98 m (`CalculateActualReelInDistance`, `FUN_1430e2880`).
const REEL_STEP_M: f32 = 0.5;
const REEL_MAX_STEPS: u32 = 500;
const REEL_INVALID_ABOVE_M: f32 = 100.0;
const REEL_CLAMP_M: f32 = 98.0;
/// How long `HookHit` lasts before the line rests (retail 568 to 617 ms; 585 ms in
/// `open-world/Fishing`).
const HOOK_HIT_MS: u64 = 585;
/// When a catch ends on its own (retail 6147 ms and 6164 ms after `Succeeded_FishCaught`; a press
/// ends it earlier, 612 to 1916 ms after it in the feed-the-fire quest).
const CAUGHT_END_MS: u64 = 6_147;
/// When a missed hook ends (retail 1386 ms after `Failed_HookMissedTooLate`; a line break and an
/// escape end alike, Inferred: neither was captured).
const MISS_END_MS: u64 = 1_386;
/// When a cast on land ends (retail 567 and 801 ms after `Failed_CastHitLand`; their mean).
const CAST_ON_LAND_END_MS: u64 = 684;
/// The top of the line tension (the client clamps it at 100, `FUN_1430f5420`): the line breaks.
const MAX_TENSION: f64 = 100.0;
/// The scale of the roll modifiers on the wire and on the loot rolls (Inferred: retail sent
/// 19500 for a T5 pole's 1.25 plus a bait's 0.7, and the hotspot tables roll to 400000).
const ROLL_SCALE: f64 = 10_000.0;
/// The loot table of a catch without a hotspot or a bait table (`javelindata_loottables_fishing`
/// `FishingLoot`; the remote config key `javelin.fishing.fishing-loot-table` that may name
/// another is not in the captured remote configs).
const FISHING_LOOT_TABLE: &str = "FishingLoot";
/// The roll bonus conditions of the fishing loot tables and their `FishingLootRollModifierType`.
const FISH_SIZE: (&str, u8) = ("FishSize", 0);
const FISH_RARITY: (&str, u8) = ("FishRarity", 1);
const SUMMER_FISH_RARITY: (&str, u8) = ("SummerFishRarity", 2);
const LOOT_TABLE_DIVERTED: (&str, u8) = ("LootTableDiverted", 3);
/// An item reward that rolls a loot table.
const TABLE_REWARD_PREFIX: &str = "[LTID]";
/// Milliseconds per second.
const MS_PER_S: f64 = 1_000.0;
/// One firing of a game event.
const ONCE: u32 = 1;
/// One bait goes on the hook.
const ONE_BAIT: u32 = 1;
/// The roll of a quest fish's chance, in percent.
const PERCENT: u32 = 100;

/// The keys of one input record that fishing reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FishingKeys {
    /// The fishing key was pressed.
    pub(super) toggle: bool,
    /// The bait window key was pressed.
    pub(super) bait_window: bool,
    /// The attack key was pressed.
    pub(super) press: bool,
    /// The attack key is held.
    pub(super) held: bool,
    /// The attack key was let go.
    pub(super) release: bool,
}

impl FishingKeys {
    /// The keys of `record`.
    pub(super) fn of(record: &InputRecord) -> Self {
        Self {
            toggle: record.fishing_toggle_pressed(),
            bait_window: record.bait_window_pressed(),
            press: record.attack_pressed(),
            held: record.attack_held(),
            release: record.attack_released(),
        }
    }
}

/// Where a cast landed and what the wait there is.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cast {
    /// The bobber, m (z the water surface, or the ground for a cast on land).
    pub(super) bobber: [f32; 3],
    /// The water there; `None` for land.
    pub(super) water: Option<WaterType>,
    /// The hotspot the bobber is in: its id.
    pub(super) hotspot: Option<String>,
    /// The CRC-32 of the hotspot's id for 2444; 0 for none.
    pub(super) hotspot_crc: u32,
    /// The reel distance, m.
    pub(super) reel_m: f32,
    /// The wait for the nibble, and for the bite after it.
    pub(super) nibble_ms: u64,
    pub(super) bite_ms: u64,
    /// The flight time.
    pub(super) flight_ms: u64,
}

/// A hooked fish and its fight.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Fight {
    /// The item caught.
    pub(super) item: String,
    /// Its `FishingXp`.
    pub(super) fishing_xp: u32,
    /// Its `RewardGameEventId`.
    pub(super) reward_event: Option<String>,
    /// Its behavior.
    pub(super) behavior: Behavior,
    /// Its swim-away speed, m/s, and its line tension multiplier.
    pub(super) swim_away_mps: f64,
    pub(super) tension_multiplier: f64,
    /// The pole's reel time, tension gain and recovery.
    pub(super) reel_time_s: f64,
    pub(super) tension_gain: f64,
    pub(super) tension_recover: f64,
    /// The full reel distance and what is left, m.
    pub(super) reel_m: f64,
    pub(super) left_m: f64,
    /// The line tension, 0 to [`MAX_TENSION`].
    pub(super) tension: f64,
    /// When the hook was set.
    pub(super) hooked_ms: u64,
    /// The fish's length and weight.
    pub(super) length: f32,
    pub(super) weight: f32,
    /// It bit at a hotspot.
    pub(super) at_hotspot: bool,
}

impl Fight {
    /// The multipliers of the fish's state at `now_ms`: fighting or tired by its behavior's
    /// block (`Fish Behavior Time Block Duration Seconds`).
    fn multipliers(&self, now_ms: u64) -> FishStateMultipliers {
        let block_ms = (FISHING_DATA.behavior_block_s * MS_PER_S) as u64;
        let block = now_ms.saturating_sub(self.hooked_ms) / block_ms.max(1);
        if self.behavior.fighting(block) {
            FISHING_DATA.fighting
        } else {
            FISHING_DATA.tired
        }
    }

    /// `dt_s` seconds of reeling (`active`) or resting up to `now_ms` (`FUN_1430f5420`,
    /// `FUN_1430e8230`): reeling brings the fish in at the reel distance over the pole's reel
    /// time and strains the line, resting lets the fish swim out and the line recover.
    fn step(&mut self, active: bool, dt_s: f64, now_ms: u64) {
        let multipliers = self.multipliers(now_ms);
        if active {
            let speed = self.reel_m / self.reel_time_s.max(f64::EPSILON) * multipliers.reel;
            self.left_m = (self.left_m - speed * dt_s).max(0.0);
            self.tension = (self.tension
                + self.tension_gain * self.tension_multiplier * multipliers.tension * dt_s)
                .min(MAX_TENSION);
        } else {
            self.left_m =
                (self.left_m + self.swim_away_mps * multipliers.swim * dt_s).min(self.reel_m);
            self.tension = (self.tension - self.tension_recover * dt_s).max(0.0);
        }
    }
}

/// What the cast does now.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// No pole in the hands.
    Unequipped,
    /// The pole comes out at `at_ms`.
    Equipping { at_ms: u64 },
    /// The pole is in the hands.
    Equipped,
    /// The bait window is open.
    ApplyingBait,
    /// The cast charges since `since_ms`.
    Charging { since_ms: u64 },
    /// The bobber flies until `at_ms`.
    Flying { at_ms: u64, cast: Cast },
    /// Waiting for the nibble at `at_ms`.
    Waiting { at_ms: u64, cast: Cast },
    /// A fish nibbles; it bites at `at_ms`.
    Nibbling { at_ms: u64, cast: Cast },
    /// A fish bites until `until_ms`.
    Biting { until_ms: u64, cast: Cast },
    /// The hook is set; the line rests at `at_ms`.
    Hooked { at_ms: u64, fight: Box<Fight> },
    /// The fish is on the line; `active` while the player reels; `last_ms` the last step.
    Reeling {
        fight: Box<Fight>,
        active: bool,
        last_ms: u64,
    },
    /// The cast ends at `at_ms`: the pole stays out (or goes away when `unequip`); a press ends
    /// it earlier when `press_ends`.
    Ending {
        at_ms: u64,
        unequip: bool,
        press_ends: bool,
    },
}

/// What the world must do for a step of the machine.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum FishingAction {
    /// Put the drawn weapon away: the pole comes out.
    PutWeaponAway,
    /// Work out the cast of a key held `hold_ms` and call [`PlayerFishing::cast`].
    Cast { hold_ms: u64 },
    /// The bobber landed: send 2444.
    Landed { bobber: [f32; 3], hotspot: u32 },
    /// Roll the catch of the bite of `cast` and call [`PlayerFishing::hook`].
    Hook { cast: Box<Cast> },
    /// The fish is caught.
    Caught(Box<Fight>),
    /// The cast is over: its bait `item` comes back unless the loss chance takes it.
    BaitBack { item: String, loss_chance: f64 },
    /// Fishing took a step that the player's fishing tasks count.
    QuestStep(FishingStep),
}

/// The replicated step list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Steps {
    /// The next step's sequence.
    next: u16,
    /// The steps the client has not acknowledged.
    unacknowledged: Vec<StateTransition>,
    /// The version of the newest list sent.
    version: u64,
    /// Whether a step was added since the last bundle.
    changed: bool,
}

impl Default for Steps {
    fn default() -> Self {
        Self {
            next: 0,
            unacknowledged: Vec::new(),
            version: FIRST_VERSION,
            changed: false,
        }
    }
}

/// One player's fishing.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlayerFishing {
    phase: Phase,
    steps: Steps,
    /// The counter of the newest input record read.
    last_counter: Option<u16>,
    /// The attack key is held.
    held: bool,
    /// The bait applied: its item id.
    bait: Option<String>,
    /// The bait on the hook of the running cast: its item id.
    hooked_bait: Option<String>,
}

impl Default for PlayerFishing {
    fn default() -> Self {
        Self {
            phase: Phase::Unequipped,
            steps: Steps::default(),
            last_counter: None,
            held: false,
            bait: None,
            hooked_bait: None,
        }
    }
}

impl PlayerFishing {
    /// The fishing of a player whose client expects step `next_sequence` next (a handoff's;
    /// 0 for a join).
    pub(super) fn continuing(next_sequence: u16) -> Self {
        Self {
            steps: Steps {
                next: next_sequence,
                ..Steps::default()
            },
            ..Self::default()
        }
    }

    /// The sequence of the next step.
    pub(super) const fn next_sequence(&self) -> u16 {
        self.steps.next
    }

    /// Whether the pole is out or coming out: the attack key fishes.
    pub(super) const fn pole_out(&self) -> bool {
        !matches!(self.phase, Phase::Unequipped)
    }

    /// The bait applied, when one is.
    pub(super) fn bait(&self) -> Option<&str> {
        self.bait.as_deref()
    }

    /// The bait on the hook of the running cast, when one is.
    pub(super) fn hooked_bait(&self) -> Option<&str> {
        self.hooked_bait.as_deref()
    }

    fn enter(&mut self, state: FishingState) {
        self.steps.unacknowledged.push(StateTransition {
            sequence: self.steps.next,
            state,
        });
        self.steps.next = self.steps.next.wrapping_add(1);
        self.steps.changed = true;
    }

    /// The client has every step before `next_sequence`: they leave the list.
    pub(super) fn acknowledge(&mut self, next_sequence: u16) {
        self.steps
            .unacknowledged
            .retain(|step| next_sequence.wrapping_sub(step.sequence) as i16 <= 0);
    }

    /// The own fishing state for the next bundle: the whole list with the first card, the list
    /// after a step, else none.
    pub(super) fn take_own_member(&mut self, first: bool) -> Option<BundleMember> {
        if !first && !self.steps.changed {
            return None;
        }
        if self.steps.changed {
            self.steps.version += 1;
        }
        self.steps.changed = false;
        Some(BundleMember {
            component: FISHING_COMPONENT,
            state: MemberState::Fishing(FishingComponentState::listing(
                self.steps.version,
                self.steps.unacknowledged.clone(),
            )),
        })
    }

    /// Reads the keys of input record counter `counter` at `now_ms` when it is newer than the
    /// records read before; `pole` is whether a pole is worn. Gives what the world must do.
    pub(super) fn keys(
        &mut self,
        counter: u16,
        keys: FishingKeys,
        pole: bool,
        now_ms: u64,
    ) -> Vec<FishingAction> {
        if self
            .last_counter
            .is_some_and(|last| !counter_is_newer(counter, last))
        {
            return Vec::new();
        }
        self.last_counter = Some(counter);
        let mut actions = Vec::new();
        if keys.toggle {
            self.toggle(pole, now_ms, &mut actions);
        } else if keys.bait_window {
            self.bait_window();
        } else if keys.press {
            self.press(now_ms, &mut actions);
        } else if keys.release
            && let Phase::Charging { since_ms } = self.phase
        {
            actions.push(FishingAction::Cast {
                hold_ms: now_ms.saturating_sub(since_ms),
            });
        }
        self.held = keys.press || (keys.held && !keys.release);
        actions
    }

    fn toggle(&mut self, pole: bool, now_ms: u64, actions: &mut Vec<FishingAction>) {
        match self.phase {
            Phase::Unequipped if pole => {
                self.enter(FishingState::StartedEquippingFishing);
                self.phase = Phase::Equipping {
                    at_ms: now_ms.saturating_add(EQUIP_MS),
                };
                actions.push(FishingAction::PutWeaponAway);
            }
            Phase::Unequipped | Phase::Ending { unequip: true, .. } => {}
            // The pole goes away; a running cast ends and its bait comes back.
            _ => {
                self.back_out(actions, 0.0);
                self.phase = Phase::Ending {
                    at_ms: now_ms.saturating_add(UNEQUIP_MS),
                    unequip: true,
                    press_ends: false,
                };
            }
        }
    }

    fn bait_window(&mut self) {
        match self.phase {
            Phase::Equipped => {
                self.enter(FishingState::ApplyingBait);
                self.phase = Phase::ApplyingBait;
            }
            Phase::ApplyingBait => {
                self.enter(FishingState::FishingEquipped);
                self.phase = Phase::Equipped;
            }
            _ => {}
        }
    }

    /// The bait window closes on bait `item` (2309); `None` for a bait the player does not carry,
    /// which leaves the applied bait as it was.
    pub(super) fn apply_bait(&mut self, item: Option<String>) {
        if item.is_some() {
            self.bait = item;
        }
        if self.phase == Phase::ApplyingBait {
            self.enter(FishingState::FishingEquipped);
            self.phase = Phase::Equipped;
        }
    }

    fn press(&mut self, now_ms: u64, actions: &mut Vec<FishingAction>) {
        match &self.phase {
            Phase::Equipped => {
                self.enter(FishingState::CastStart);
                self.phase = Phase::Charging { since_ms: now_ms };
            }
            // A press while waiting ends the cast without a failure (retail gather_node:
            // `FishingEnded` and `FishingEquipped` 67 ms after the press, after 18.7 s).
            Phase::Waiting { .. } => {
                self.back_out(actions, 0.0);
                self.end();
            }
            Phase::Nibbling { .. } => {
                self.fail(
                    FishingState::FailedHookMissedTooEarly,
                    FISHING_DATA.bait_loss_hook_miss,
                    MISS_END_MS,
                    now_ms,
                    actions,
                );
            }
            Phase::Biting { cast, .. } => actions.push(FishingAction::Hook {
                cast: Box::new(cast.clone()),
            }),
            Phase::Ending {
                press_ends: true, ..
            } => self.end(),
            _ => {}
        }
    }

    /// The bait of the cast comes back unless `loss_chance` takes it.
    fn back_out(&mut self, actions: &mut Vec<FishingAction>, loss_chance: f64) {
        if let Some(item) = self.hooked_bait.take() {
            actions.push(FishingAction::BaitBack { item, loss_chance });
        }
    }

    fn fail(
        &mut self,
        state: FishingState,
        loss_chance: f64,
        end_after_ms: u64,
        now_ms: u64,
        actions: &mut Vec<FishingAction>,
    ) {
        self.enter(state);
        self.back_out(actions, loss_chance);
        self.phase = Phase::Ending {
            at_ms: now_ms.saturating_add(end_after_ms),
            unequip: false,
            press_ends: false,
        };
    }

    /// The cast ends: the pole stays out.
    fn end(&mut self) {
        self.enter(FishingState::FishingEnded);
        self.enter(FishingState::FishingEquipped);
        self.phase = Phase::Equipped;
    }

    /// The cast of the key let go lands as `cast` (`None`: refused, the pole stays out), with
    /// bait `bait` on the hook.
    pub(super) fn cast(&mut self, cast: Option<Cast>, bait: Option<String>, now_ms: u64) {
        if !matches!(self.phase, Phase::Charging { .. }) {
            return;
        }
        let Some(cast) = cast else {
            self.end();
            return;
        };
        self.enter(FishingState::CastEnd);
        self.hooked_bait = bait;
        self.phase = Phase::Flying {
            at_ms: now_ms.saturating_add(cast.flight_ms),
            cast,
        };
    }

    /// The hook is set on `fight` at `now_ms` (`None`: nothing on the hook, the fish got away).
    pub(super) fn hook(&mut self, fight: Option<Fight>, now_ms: u64) -> Vec<FishingAction> {
        let mut actions = Vec::new();
        if !matches!(self.phase, Phase::Biting { .. }) {
            return actions;
        }
        match fight {
            Some(fight) => {
                self.enter(FishingState::HookHit);
                actions.push(FishingAction::QuestStep(FishingStep::Hook));
                self.phase = Phase::Hooked {
                    at_ms: now_ms.saturating_add(HOOK_HIT_MS),
                    fight: Box::new(fight),
                };
            }
            None => self.fail(
                FishingState::FailedFishSwamAway,
                FISHING_DATA.bait_loss_reeling_distance,
                MISS_END_MS,
                now_ms,
                &mut actions,
            ),
        }
        actions
    }

    /// The pole is gone from its slot: it is put away at once.
    pub(super) fn pole_gone(&mut self) -> Vec<FishingAction> {
        let mut actions = Vec::new();
        if self.pole_out() {
            self.back_out(&mut actions, 0.0);
            self.enter(FishingState::FishingEnded);
            self.enter(FishingState::FishingUnequipped);
            self.phase = Phase::Unequipped;
        }
        actions
    }

    /// Moves the machine on to `now_ms`.
    pub(super) fn tick(&mut self, now_ms: u64) -> Vec<FishingAction> {
        let mut actions = Vec::new();
        match std::mem::replace(&mut self.phase, Phase::Unequipped) {
            Phase::Equipping { at_ms } if now_ms >= at_ms => {
                self.enter(FishingState::FishingEquipped);
                self.phase = Phase::Equipped;
                actions.push(FishingAction::QuestStep(FishingStep::Equip));
            }
            Phase::Flying { at_ms, cast } if now_ms >= at_ms => {
                actions.push(FishingAction::Landed {
                    bobber: cast.bobber,
                    hotspot: cast.hotspot_crc,
                });
                if cast.water.is_some() {
                    self.enter(FishingState::CastHitWater);
                    self.enter(FishingState::FishingStarted);
                    actions.push(FishingAction::QuestStep(FishingStep::Cast));
                    self.phase = Phase::Waiting {
                        at_ms: now_ms.saturating_add(cast.nibble_ms),
                        cast,
                    };
                } else {
                    self.fail(
                        FishingState::FailedCastHitLand,
                        FISHING_DATA.bait_loss_cast_hit_land,
                        CAST_ON_LAND_END_MS,
                        now_ms,
                        &mut actions,
                    );
                }
            }
            Phase::Waiting { at_ms, cast } if now_ms >= at_ms => {
                self.enter(FishingState::FishNibbleWindowOpen);
                self.phase = Phase::Nibbling {
                    at_ms: now_ms.saturating_add(cast.bite_ms),
                    cast,
                };
            }
            Phase::Nibbling { at_ms, cast } if now_ms >= at_ms => {
                self.enter(FishingState::FishBiteWindowOpen);
                self.phase = Phase::Biting {
                    until_ms: now_ms.saturating_add((FISHING_DATA.bite_window_s * MS_PER_S) as u64),
                    cast,
                };
            }
            Phase::Biting { until_ms, .. } if now_ms > until_ms => self.fail(
                FishingState::FailedHookMissedTooLate,
                FISHING_DATA.bait_loss_hook_miss,
                MISS_END_MS,
                now_ms,
                &mut actions,
            ),
            Phase::Hooked { at_ms, fight } if now_ms >= at_ms => {
                self.enter(FishingState::ReelingRecover);
                self.phase = Phase::Reeling {
                    fight,
                    active: false,
                    last_ms: now_ms,
                };
            }
            Phase::Reeling {
                fight,
                active,
                last_ms,
            } => self.reel(fight, active, last_ms, now_ms, &mut actions),
            Phase::Ending { at_ms, unequip, .. } if now_ms >= at_ms => {
                if unequip {
                    self.enter(FishingState::FishingEnded);
                    self.enter(FishingState::FishingUnequipped);
                    self.phase = Phase::Unequipped;
                } else {
                    self.end();
                }
            }
            phase => self.phase = phase,
        }
        actions
    }

    /// One step of the fight from `last_ms` to `now_ms`: the line breaks at the top tension, the
    /// fish is caught when nothing is left to reel, and the line rests or reels as the key says.
    fn reel(
        &mut self,
        mut fight: Box<Fight>,
        active: bool,
        last_ms: u64,
        now_ms: u64,
        actions: &mut Vec<FishingAction>,
    ) {
        fight.step(
            active,
            now_ms.saturating_sub(last_ms) as f64 / MS_PER_S,
            now_ms,
        );
        if fight.tension >= MAX_TENSION {
            self.fail(
                FishingState::FailedLineBreak,
                FISHING_DATA.bait_loss_reeling_tension,
                MISS_END_MS,
                now_ms,
                actions,
            );
        } else if fight.left_m <= 0.0 {
            self.enter(FishingState::SucceededFishCaught);
            self.back_out(actions, FISHING_DATA.bait_loss_fish_caught);
            actions.push(FishingAction::Caught(fight));
            self.phase = Phase::Ending {
                at_ms: now_ms.saturating_add(CAUGHT_END_MS),
                unequip: false,
                press_ends: true,
            };
        } else {
            if active != self.held {
                if self.held {
                    self.enter(FishingState::ReelingActive);
                    actions.push(FishingAction::QuestStep(FishingStep::Reel));
                } else {
                    self.enter(FishingState::ReelingRecover);
                }
            }
            self.phase = Phase::Reeling {
                fight,
                active: self.held,
                last_ms: now_ms,
            };
        }
    }
}

/// The cast bar's fill after holding the key `hold_ms`, 0 to 1.
fn cast_fill(hold_ms: u64) -> f64 {
    let bar = CAST_BAR_PER_S * hold_ms as f64 / MS_PER_S;
    let bar = if bar > CAST_BAR_FULL {
        2.0 * CAST_BAR_FULL - bar
    } else {
        bar
    };
    bar.clamp(0.0, CAST_BAR_FULL) / CAST_BAR_FULL
}

/// The cast distance of a pole whose range is `range` with `bonus` m more at full fill, after
/// holding the key `hold_ms`.
fn cast_distance(range: Range, bonus: f64, hold_ms: u64) -> f64 {
    Range {
        low: range.low,
        high: range.high + bonus,
    }
    .at(cast_fill(hold_ms))
}

/// The bobber's flight time over `distance_m`.
fn flight_ms(distance_m: f32) -> u64 {
    (FLIGHT_MS_PER_M * f64::from(distance_m) - FLIGHT_OFFSET_MS).max(0.0) as u64
}

/// The water type at `x`, `y` for a body at height `z` (the bobber: the surface), from
/// `ground`'s water flags, its surface and the terrain under it (`FUN_1430e8590`).
fn water_at(ground: &dyn Ground, x: f32, y: f32, z: f32, sea_level_m: f32) -> Option<WaterType> {
    let flags = ground.water_flags(x, y)?;
    let surface = ground.water_surface(x, y, sea_level_m)?;
    let floor = ground.height_at(x, y)?;
    if z < floor || z > surface {
        return None;
    }
    WaterType::of(flags, (surface - floor).min(surface - z))
}

/// The reel distance of a bobber at `bobber` cast from the rod's tip `tip`: the distance
/// between them, held at the first dry place on the way back (`CalculateActualReelInDistance`).
fn reel_distance(ground: &dyn Ground, tip: [f32; 2], bobber: [f32; 2], sea_level_m: f32) -> f32 {
    let (dx, dy) = (tip[0] - bobber[0], tip[1] - bobber[1]);
    let total = dx.hypot(dy);
    if total <= f32::EPSILON {
        return 0.0;
    }
    let (ux, uy) = (dx / total, dy / total);
    let total = if total > REEL_INVALID_ABOVE_M {
        REEL_CLAMP_M
    } else {
        total
    };
    (1..=REEL_MAX_STEPS)
        .map(|step| step as f32 * REEL_STEP_M)
        .take_while(|walked| *walked < total)
        .find(|walked| {
            ground
                .water_surface(
                    bobber[0] + ux * walked,
                    bobber[1] + uy * walked,
                    sea_level_m,
                )
                .is_none()
        })
        .unwrap_or(total)
}

/// The loot roll bonuses and the 3172 roll modifiers of `modifiers`.
fn roll_bonuses(modifiers: RollModifiers) -> ([RollBonus<'static>; 4], Vec<RollModifier>) {
    let scaled = |value: f64| (value * ROLL_SCALE).round().clamp(0.0, f64::from(u32::MAX)) as u32;
    let entries = [
        (FISH_SIZE, scaled(modifiers.size)),
        (FISH_RARITY, scaled(modifiers.rarity)),
        (SUMMER_FISH_RARITY, scaled(modifiers.summer_rarity)),
        (LOOT_TABLE_DIVERTED, 0),
    ];
    (
        entries.map(|((name, _), value)| RollBonus { name, value }),
        entries
            .iter()
            .map(|((_, kind), value)| RollModifier {
                kind: *kind,
                value: *value,
            })
            .collect(),
    )
}

/// What the hook brought in, before the fight.
struct HookedCatch {
    message: FishCatch,
    fight: Fight,
}

impl SimWorld {
    /// Reads the fishing keys of the new records of `records` (newest first) of the client of
    /// `connection` at `now_ms`.
    pub fn fishing_input(
        &mut self,
        connection: &ConnectionRef,
        records: &[InputRecord],
        now_ms: u64,
    ) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let pole = self.worn_pole(entity).is_some();
        for record in records.iter().rev() {
            let actions = match self.world.get_mut::<Player>(entity) {
                Some(mut player) => player.fishing.keys(
                    record.u16_after_mask,
                    FishingKeys::of(record),
                    pole,
                    now_ms,
                ),
                None => return,
            };
            self.fishing_actions(entity, actions, now_ms);
        }
    }

    /// Whether the attack key of the client of `connection` fishes: its pole is out.
    pub fn fishing_holds_attack(&self, connection: &ConnectionRef) -> bool {
        self.by_connection
            .get(connection)
            .and_then(|entity| self.world.get::<Player>(*entity))
            .is_some_and(|player| player.fishing.pole_out())
    }

    /// A fishing call of the client of `connection`: an acknowledgement, or a bait to apply (557
    /// answers a bait the player carries).
    pub fn fishing_call(&mut self, connection: &ConnectionRef, call: FishingClientMsg) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let content = Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        if call.header() != FacetHeader::of(FISHING_FACET, player.number) {
            tracing::debug!(
                player = player.number,
                ?call,
                "fishing call of another target"
            );
            return;
        }
        match call {
            FishingClientMsg::StateReceived { next_sequence, .. } => {
                player.fishing.acknowledge(next_sequence);
            }
            FishingClientMsg::ApplyBait { header, bait } => {
                let carried = content
                    .fishing
                    .bait(bait)
                    .map(|row| row.id.clone())
                    .filter(|id| {
                        player
                            .gear
                            .as_ref()
                            .is_some_and(|gear| gear.bag_count(id) > 0)
                    });
                if carried.is_none() {
                    tracing::debug!(player = player.number, bait, "bait not carried");
                }
                let answer = carried.is_some().then_some(ServerMessage::Fishing(
                    FishingServerMsg::BaitData { header, bait },
                ));
                player.fishing.apply_bait(carried);
                if let Some(answer) = answer {
                    send_now(&mut self.combat_messages, &player.connection, &[answer]);
                }
            }
        }
    }

    /// Moves every player's fishing on to `now_ms`.
    pub(super) fn tick_fishing(&mut self, now_ms: u64) {
        let entities: Vec<Entity> = self.by_connection.values().copied().collect();
        for entity in entities {
            let pole = self.worn_pole(entity).is_some();
            let actions = match self.world.get_mut::<Player>(entity) {
                Some(mut player) => {
                    let mut actions = if pole {
                        Vec::new()
                    } else {
                        player.fishing.pole_gone()
                    };
                    actions.extend(player.fishing.tick(now_ms));
                    actions
                }
                None => continue,
            };
            self.fishing_actions(entity, actions, now_ms);
        }
    }

    /// The pole worn by player `entity`.
    fn worn_pole(&self, entity: Entity) -> Option<&Pole> {
        let item = self
            .world
            .get::<Player>(entity)?
            .gear
            .as_ref()?
            .worn_item(FISHING_SLOT)?;
        self.content.fishing.pole(item)
    }

    /// Carries out `actions` for player `entity` at `now_ms`.
    fn fishing_actions(&mut self, entity: Entity, actions: Vec<FishingAction>, now_ms: u64) {
        for action in actions {
            match action {
                FishingAction::PutWeaponAway => {
                    let time_ns = unix_ns(self.server_ms);
                    if let Some(mut player) = self.world.get_mut::<Player>(entity)
                        && let Some(gear) = player.gear.as_mut()
                    {
                        gear.put_away(time_ns, None);
                    }
                }
                FishingAction::Cast { hold_ms } => self.cast_line(entity, hold_ms, now_ms),
                FishingAction::Landed { bobber, hotspot } => {
                    self.send_fishing(entity, |header| FishingServerMsg::BobberPosition {
                        header,
                        position: bobber,
                        hotspot,
                    });
                }
                FishingAction::Hook { cast } => self.set_hook(entity, &cast, now_ms),
                FishingAction::Caught(fight) => self.caught(entity, &fight, now_ms),
                FishingAction::BaitBack { item, loss_chance } => {
                    self.bait_back(entity, &item, loss_chance);
                }
                FishingAction::QuestStep(step) => {
                    if let Some(number) =
                        self.world.get::<Player>(entity).map(|player| player.number)
                    {
                        self.quest_event(number, Event::Fishing(step), now_ms);
                    }
                }
            }
        }
    }

    /// Sends the fishing call `message` (made with player `entity`'s header) to its client.
    fn send_fishing(
        &mut self,
        entity: Entity,
        message: impl FnOnce(FacetHeader) -> FishingServerMsg,
    ) {
        if let Some(player) = self.world.get::<Player>(entity) {
            let call = message(FacetHeader::of(FISHING_FACET, player.number));
            send_now(
                &mut self.combat_messages,
                &player.connection,
                &[ServerMessage::Fishing(call)],
            );
        }
    }

    /// Player `entity` lets the key go after `hold_ms`: the cast flies from its place along its
    /// view, as far as the pole and its fishing rank cast with that fill. A cast from deep
    /// water is refused (`CastStartAction`: "can't cast from here"). The applied bait goes on
    /// the hook when it works in the water hit.
    fn cast_line(&mut self, entity: Entity, hold_ms: u64, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let sea_level_m = self.settings.ocean_level_m;
        let (rank, _) = self.tradeskill_perks(entity, FISHING_TRADESKILL);
        let place = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position());
        let yaw = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.view)
            .map_or(0.0, |view| view.yaw);
        let pole = self.worn_pole(entity).cloned();
        let ground = Arc::clone(&self.ground);
        let landing = place.zip(pole).and_then(|(place, pole)| {
            if water_at(&*ground, place.x, place.y, place.z, sea_level_m)
                .is_some_and(WaterType::is_deep)
            {
                tracing::debug!(?place, "cast from deep water");
                return None;
            }
            let (forward, _) = yaw_axes(yaw);
            let distance = cast_distance(
                pole.cast_distance,
                content.fishing.cast_bonus(rank),
                hold_ms,
            ) as f32;
            let tip = [
                place.x + forward[0] * ROD_TIP_M,
                place.y + forward[1] * ROD_TIP_M,
            ];
            let (x, y) = (
                tip[0] + forward[0] * distance,
                tip[1] + forward[1] * distance,
            );
            let z = ground
                .water_surface(x, y, sea_level_m)
                .or_else(|| ground.height_at(x, y))
                .unwrap_or(place.z);
            let water = water_at(&*ground, x, y, z, sea_level_m);
            let hotspot = water.and_then(|_| content.fishing.hotspot_at(x, y, rank).cloned());
            Some((place, tip, [x, y, z], water, hotspot))
        });
        let Some((place, tip, bobber, water, hotspot)) = landing else {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.fishing.cast(None, None, now_ms);
            }
            return;
        };
        let bait = self.bait_on_hook(entity, water);
        let waits = hotspot
            .as_ref()
            .map(|hotspot| hotspot.waits)
            .or_else(|| water.and_then(|water| content.fishing.waits(water)));
        let nibble_multiplier = bait
            .as_deref()
            .and_then(|item| content.fishing.bait_of_item(item))
            .map_or(1.0, |row| row.nibble_multiplier);
        let (nibble_ms, bite_ms) = waits.map_or((0, 0), |waits: WaitRanges| {
            let nibble_s = waits.nibble_s.at(self.dice.fraction()) * nibble_multiplier;
            let bite_s = waits.bite_s.at(self.dice.fraction());
            ((nibble_s * MS_PER_S) as u64, (bite_s * MS_PER_S) as u64)
        });
        let distance = Position {
            x: bobber[0],
            y: bobber[1],
            z: bobber[2],
        }
        .horizontal_distance(place);
        let cast = Cast {
            bobber,
            water,
            hotspot_crc: hotspot.as_ref().map_or(0, |hotspot| hotspot.crc),
            hotspot: hotspot.map(|hotspot| hotspot.id),
            reel_m: reel_distance(&*ground, tip, [bobber[0], bobber[1]], sea_level_m),
            nibble_ms,
            bite_ms,
            flight_ms: flight_ms(distance),
        };
        tracing::debug!(?cast, hold_ms, "cast");
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.fishing.cast(Some(cast), bait, now_ms);
        }
    }

    /// Puts one of player `entity`'s applied bait on the hook for water `water` at the day
    /// phase: a bait that works there and that the player carries. Gives its item id.
    fn bait_on_hook(&mut self, entity: Entity, water: Option<WaterType>) -> Option<String> {
        let content = Arc::clone(&self.content);
        let phase = DayPhase::of(OBSERVED_DAY_PHASE)?;
        let mut player = self.world.get_mut::<Player>(entity)?;
        let player = &mut *player;
        let item = player.fishing.bait()?.to_owned();
        let works = content
            .fishing
            .bait_of_item(&item)
            .zip(water)
            .is_some_and(|(bait, water)| bait.works(water, phase));
        (works && player.gear.as_mut()?.take(&item, ONE_BAIT)).then_some(item)
    }

    /// The cast of player `entity` ends: its bait `item` comes back unless `loss_chance` takes it.
    fn bait_back(&mut self, entity: Entity, item: &str, loss_chance: f64) {
        if self.dice.chance(loss_chance) {
            return;
        }
        let content = Arc::clone(&self.content);
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some((gear, kit)) = player.gear.as_mut().zip(content.kits.item(item))
        {
            gear.add(kit, ONE_BAIT);
        }
    }

    /// Player `entity` sets the hook in the bite of `cast` at `now_ms`: the catch is rolled
    /// (its 3172 goes out) and the fight starts, with the hook's game event; nothing on the hook
    /// lets the fish get away.
    fn set_hook(&mut self, entity: Entity, cast: &Cast, now_ms: u64) {
        let hooked = self.roll_catch(entity, cast, now_ms);
        let number = self.world.get::<Player>(entity).map(|player| player.number);
        let actions = match hooked {
            Some(HookedCatch { message, fight }) => {
                self.send_fishing(entity, |header| FishingServerMsg::CatchData {
                    header,
                    catch: message,
                });
                if let Some(number) = number {
                    self.fishing_event(number, FISHING_DATA.hook_event, now_ms);
                }
                self.world
                    .get_mut::<Player>(entity)
                    .map(|mut player| player.fishing.hook(Some(fight), now_ms))
            }
            None => {
                tracing::debug!(?cast, "nothing on the hook");
                self.world
                    .get_mut::<Player>(entity)
                    .map(|mut player| player.fishing.hook(None, now_ms))
            }
        };
        self.fishing_actions(entity, actions.unwrap_or_default(), now_ms);
    }

    /// Rolls what player `entity`'s hook brings in from the water of `cast`: a quest fish of the
    /// player's waiting catch tasks at its chance ([`crate::objectives::ObjectiveRun::fish_drops`]),
    /// else the bait's loot table when it names one, else the hotspot's, else
    /// [`FISHING_LOOT_TABLE`], with the roll bonuses of the pole, the bait and the hotspot,
    /// holding the water's tag, the hotspot's loot tag and the loot tags of the player's
    /// territories. The first catchable item rolled is the catch; its behavior, length and
    /// weight are picked evenly from its rows.
    fn roll_catch(&mut self, entity: Entity, cast: &Cast, now_ms: u64) -> Option<HookedCatch> {
        let content = Arc::clone(&self.content);
        let rules = &content.fishing;
        let water = cast.water?;
        let pole = self.worn_pole(entity)?.clone();
        let (number, territories, bait) = {
            let player = self.world.get::<Player>(entity)?;
            (
                player.number,
                player.territories.clone(),
                player
                    .fishing
                    .hooked_bait()
                    .and_then(|item| rules.bait_of_item(item)),
            )
        };
        let hotspot = cast.hotspot.as_deref().and_then(|id| rules.hotspot(id));
        let modifiers = [
            Some(pole.modifiers),
            bait.map(|bait| bait.modifiers),
            hotspot.map(|hotspot| hotspot.modifiers),
        ]
        .into_iter()
        .flatten()
        .fold(RollModifiers::default(), RollModifiers::plus);
        let table = bait
            .and_then(|bait| bait.loot_table.clone())
            .or_else(|| hotspot.map(|hotspot| hotspot.loot_table.clone()))
            .unwrap_or_else(|| FISHING_LOOT_TABLE.to_owned());
        let mut tags = rules.territory_tags(&territories);
        tags.push(water.tag().to_owned());
        tags.extend(hotspot.map(|hotspot| hotspot.loot_tag.clone()));
        let (bonuses, wire_modifiers) = roll_bonuses(modifiers);
        let zones = super::player_territories(&content, &territories);
        let quest_fish: Vec<(String, u32)> = self
            .quests
            .get(&number)
            .into_iter()
            .flatten()
            .flat_map(|quest| quest.run.fish_drops(water.tag(), &zones))
            .collect();
        let bitten = quest_fish
            .into_iter()
            .find(|(item, percent)| {
                rules.catchable(item).is_some() && self.dice.below(PERCENT) < *percent
            })
            .map(|(item, _)| item);
        let (item, catchable) = match bitten {
            Some(item) => {
                let catchable = rules.catchable(&item)?;
                (item, catchable)
            }
            None => {
                let reached = self.reached_loot_limits(number);
                let context = nw_content::loot::LootContext {
                    tags: &tags,
                    ..self.loot_context(number, None, &reached)
                };
                let dice = &mut self.dice;
                let rolled =
                    content
                        .loot
                        .roll_with_bonuses(&table, context, &bonuses, &mut |below| {
                            dice.below(below)
                        });
                let items = self.limit_loot(number, rolled);
                items
                    .iter()
                    .find_map(|(item, _)| Some((item.clone(), rules.catchable(item)?)))?
            }
        };
        let behaviors: Vec<&Behavior> = catchable
            .behaviors
            .iter()
            .filter_map(|id| rules.behavior(id))
            .collect();
        let pick = usize::try_from(self.dice.below(u32::try_from(behaviors.len()).ok()?)).ok()?;
        let behavior = (*behaviors.get(pick)?).clone();
        let length = catchable.length.at(self.dice.fraction()) as f32;
        let weight = catchable.weight.at(self.dice.fraction()) as f32;
        let reel_m = f64::from(cast.reel_m);
        Some(HookedCatch {
            message: FishCatch {
                item: lowercase_crc32(&item),
                behavior: behavior.crc,
                behavior_bits: behavior.bits,
                length,
                weight,
                modifiers: wire_modifiers,
            },
            fight: Fight {
                item,
                fishing_xp: catchable.fishing_xp,
                reward_event: catchable.reward_event.clone(),
                behavior,
                swim_away_mps: catchable.swim_away_mps,
                tension_multiplier: catchable.tension_multiplier,
                reel_time_s: pole.reel_time_s,
                tension_gain: pole.tension_gain,
                tension_recover: pole.tension_recover,
                reel_m,
                left_m: reel_m,
                tension: 0.0,
                hooked_ms: now_ms,
                length,
                weight,
                at_hotspot: hotspot.is_some(),
            },
        })
    }

    /// Player `entity` caught the fish of `fight` at `now_ms`: the item goes into the bag, its
    /// `FishingXp` times the remote config's `Fishing` multiplier into the `Fishing` tradeskill
    /// (Observed: retail raised it by exactly twice `FishingXp` at all 6 catches of the
    /// feed-the-fire quest, under multiplier 2), and the catch event and the catchable's reward
    /// event fire.
    fn caught(&mut self, entity: Entity, fight: &Fight, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let number = player.number;
        if let Some(mastery) = content.progression.tradeskill(FISHING_TRADESKILL) {
            let gain = progression_gain(
                f64::from(fight.fishing_xp),
                ONCE,
                content.gameplay.xp.progression(FISHING_TRADESKILL),
            );
            let experience = player.masteries.experience(mastery).saturating_add(gain);
            player.masteries.set(mastery, experience);
        }
        tracing::debug!(player = number, item = %fight.item, "fish caught");
        self.give_loot(number, vec![(fight.item.clone(), ONCE)], now_ms);
        self.quest_event(
            number,
            Event::FishCaught(FishCaught {
                item: &fight.item,
                length: fight.length,
                weight: fight.weight,
                hotspot: fight.at_hotspot,
            }),
            now_ms,
        );
        self.fishing_event(number, FISHING_DATA.catch_event, now_ms);
        if let Some(event) = &fight.reward_event {
            self.fishing_event(number, event, now_ms);
        }
    }

    /// Game event `event` of fishing happens for player `number` at `now_ms`: its quest task
    /// counts it, and it gives its reward (experience, coins, its item or the roll of its item's
    /// loot table). The hook's event also raises its categorical progression
    /// ([`Self::grant_event`]; Observed: `FishHooked` gave `Fishing` 20 = 10 under multiplier
    /// 2); the catch's progression is the catchable's `FishingXp` ([`Self::caught`]:
    /// `FishCaught`'s own `Fishing` 1 did not show on retail).
    fn fishing_event(&mut self, number: u64, event: &str, now_ms: u64) {
        let content = Arc::clone(&self.content);
        if event.eq_ignore_ascii_case(FISHING_DATA.hook_event) {
            self.grant_event(number, event, ONCE, now_ms);
        }
        if let Some(reward) = content.rewards.get(event) {
            if let Some(mut player) = self
                .player_by_number(number)
                .and_then(|entity| self.world.get_mut::<Player>(entity))
            {
                grant(
                    &mut player,
                    &Reward {
                        item: None,
                        ..reward.clone()
                    },
                    &content.gameplay,
                );
            }
            if let Some((item, quantity)) = &reward.item {
                let items = match item.strip_prefix(TABLE_REWARD_PREFIX) {
                    Some(table) => {
                        let reached = self.reached_loot_limits(number);
                        let context = self.loot_context(number, None, &reached);
                        let dice = &mut self.dice;
                        let rolled = content
                            .loot
                            .roll(table, context, &mut |below| dice.below(below));
                        self.limit_loot(number, rolled)
                    }
                    None => vec![(item.clone(), *quantity)],
                };
                self.give_loot(number, items, now_ms);
            }
        }
        self.quest_event(number, Event::GameEvent(event), now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    /// A pole's fight values (T5: reel 8 s, gain 12.5/s, recovery 47/s) on a medium salmon
    /// (swim 0.15 m/s, tension x1.2) of the `SporadicEasy` behavior, 20 m out.
    fn fight(bits: u8) -> Fight {
        Fight {
            item: "FishingSalmonMediumT1".to_owned(),
            fishing_xp: 110,
            reward_event: None,
            behavior: Behavior {
                id: "SporadicEasy".to_owned(),
                crc: 0xd26d_ad61,
                bits,
            },
            swim_away_mps: 0.15,
            tension_multiplier: 1.2,
            reel_time_s: 8.0,
            tension_gain: 12.5,
            tension_recover: 47.0,
            reel_m: 20.0,
            left_m: 20.0,
            tension: 0.0,
            hooked_ms: NOW,
            length: 12.0,
            weight: 3.0,
            at_hotspot: false,
        }
    }

    fn cast(water: Option<WaterType>) -> Cast {
        Cast {
            bobber: [10.0, 20.0, 40.0],
            water,
            hotspot: None,
            hotspot_crc: 0,
            reel_m: 20.0,
            nibble_ms: 10_000,
            bite_ms: 1_200,
            flight_ms: 1_500,
        }
    }

    fn states(fishing: &PlayerFishing) -> Vec<FishingState> {
        fishing
            .steps
            .unacknowledged
            .iter()
            .map(|step| step.state)
            .collect()
    }

    /// A machine with its pole out at `NOW` and its list acknowledged.
    fn equipped() -> (PlayerFishing, u16) {
        let mut fishing = PlayerFishing::default();
        let keys = FishingKeys {
            toggle: true,
            ..FishingKeys::default()
        };
        assert_eq!(
            fishing.keys(1, keys, true, NOW),
            [FishingAction::PutWeaponAway]
        );
        assert_eq!(
            fishing.tick(NOW + EQUIP_MS),
            [FishingAction::QuestStep(FishingStep::Equip)]
        );
        assert_eq!(
            states(&fishing),
            [
                FishingState::StartedEquippingFishing,
                FishingState::FishingEquipped
            ]
        );
        fishing.acknowledge(fishing.steps.next);
        (fishing, 2)
    }

    #[test]
    fn the_pole_comes_out_only_when_worn_and_goes_away_with_the_key() {
        let mut fishing = PlayerFishing::default();
        let toggle = FishingKeys {
            toggle: true,
            ..FishingKeys::default()
        };
        assert!(fishing.keys(1, toggle, false, NOW).is_empty(), "no pole");
        assert!(!fishing.pole_out());
        let (mut fishing, counter) = equipped();
        assert!(fishing.pole_out());
        assert!(fishing.keys(counter, toggle, true, NOW).is_empty());
        assert!(
            states(&fishing).is_empty(),
            "the put-away waits for its time"
        );
        fishing.tick(NOW + EQUIP_MS + UNEQUIP_MS);
        assert_eq!(
            states(&fishing),
            [FishingState::FishingEnded, FishingState::FishingUnequipped]
        );
        assert!(!fishing.pole_out());
        assert!(
            fishing.keys(counter, toggle, true, NOW).is_empty() && !fishing.pole_out(),
            "a record read before does nothing"
        );
    }

    #[test]
    fn a_whole_catch_follows_the_retail_steps() {
        let (mut fishing, mut counter) = equipped();
        let mut key = |fishing: &mut PlayerFishing, keys: FishingKeys, at: u64| {
            counter += 1;
            fishing.keys(counter, keys, true, at)
        };
        let press = FishingKeys {
            press: true,
            ..FishingKeys::default()
        };
        let held = FishingKeys {
            held: true,
            ..FishingKeys::default()
        };
        let release = FishingKeys {
            release: true,
            ..FishingKeys::default()
        };
        assert!(key(&mut fishing, press, NOW).is_empty());
        assert_eq!(
            key(&mut fishing, release, NOW + 2_000),
            [FishingAction::Cast { hold_ms: 2_000 }]
        );
        fishing.cast(
            Some(cast(Some(WaterType::FreshShallow))),
            Some("BaitFreshFireflyT3".to_owned()),
            NOW + 2_000,
        );
        let landed = fishing.tick(NOW + 3_500);
        assert_eq!(
            landed,
            [
                FishingAction::Landed {
                    bobber: [10.0, 20.0, 40.0],
                    hotspot: 0
                },
                FishingAction::QuestStep(FishingStep::Cast)
            ]
        );
        fishing.tick(NOW + 13_500);
        fishing.tick(NOW + 14_700);
        assert!(matches!(
            key(&mut fishing, press, NOW + 15_000).as_slice(),
            [FishingAction::Hook { .. }]
        ));
        // A small fish's tension multiplier (0.8): 8 s of reeling stays below the top.
        let small = Fight {
            tension_multiplier: 0.8,
            ..fight(0)
        };
        assert_eq!(
            fishing.hook(Some(small), NOW + 15_000),
            [FishingAction::QuestStep(FishingStep::Hook)]
        );
        fishing.tick(NOW + 15_000 + HOOK_HIT_MS);
        key(&mut fishing, held, NOW + 15_700);
        let mut at = NOW + 15_700;
        at += 33;
        assert_eq!(
            fishing.tick(at),
            [FishingAction::QuestStep(FishingStep::Reel)],
            "the reel starts"
        );
        let mut caught = Vec::new();
        while caught.is_empty() && at < NOW + 60_000 {
            at += 33;
            caught = fishing.tick(at);
        }
        assert_eq!(
            states(&fishing),
            [
                FishingState::CastStart,
                FishingState::CastEnd,
                FishingState::CastHitWater,
                FishingState::FishingStarted,
                FishingState::FishNibbleWindowOpen,
                FishingState::FishBiteWindowOpen,
                FishingState::HookHit,
                FishingState::ReelingRecover,
                FishingState::ReelingActive,
                FishingState::SucceededFishCaught,
            ]
        );
        // A lazy fish comes in after 8 s of reeling (retail ifrit: 8032 ms).
        assert!((at - (NOW + 15_700)).abs_diff(8_000) <= 66, "{}", at - NOW);
        assert!(matches!(
            caught.as_slice(),
            [
                FishingAction::BaitBack { loss_chance, .. },
                FishingAction::Caught(_)
            ] if (*loss_chance - FISHING_DATA.bait_loss_fish_caught).abs() < f64::EPSILON
        ));
        fishing.tick(at + CAUGHT_END_MS);
        assert_eq!(
            states(&fishing)[10..],
            [FishingState::FishingEnded, FishingState::FishingEquipped]
        );
    }

    #[test]
    fn misses_and_casts_on_land_fail_and_give_the_bait_a_chance_back() {
        let (mut fishing, counter) = equipped();
        let press = FishingKeys {
            press: true,
            ..FishingKeys::default()
        };
        let release = FishingKeys {
            release: true,
            ..FishingKeys::default()
        };
        fishing.keys(counter + 1, press, true, NOW);
        fishing.keys(counter + 2, release, true, NOW + 500);
        fishing.cast(Some(cast(None)), Some("Bait".to_owned()), NOW + 500);
        let actions = fishing.tick(NOW + 2_000);
        assert!(matches!(
            actions.last(),
            Some(FishingAction::BaitBack { loss_chance, .. }) if loss_chance.abs() < f64::EPSILON
        ));
        assert_eq!(
            states(&fishing).last(),
            Some(&FishingState::FailedCastHitLand)
        );
        fishing.tick(NOW + 2_000 + CAST_ON_LAND_END_MS);
        assert_eq!(
            states(&fishing).last(),
            Some(&FishingState::FishingEquipped)
        );
        // A cast whose bite window closes misses too late.
        fishing.keys(counter + 3, press, true, NOW + 3_000);
        fishing.keys(counter + 4, release, true, NOW + 4_000);
        fishing.cast(Some(cast(Some(WaterType::SaltDeep))), None, NOW + 4_000);
        fishing.tick(NOW + 5_500);
        fishing.tick(NOW + 15_500);
        fishing.tick(NOW + 16_700);
        fishing.tick(NOW + 17_701);
        assert_eq!(
            states(&fishing).last(),
            Some(&FishingState::FailedHookMissedTooLate)
        );
    }

    #[test]
    fn reeling_strains_the_line_and_a_fighting_fish_breaks_it() {
        let mut steady = fight(0);
        steady.step(true, 1.0, NOW);
        assert!((steady.left_m - 17.5).abs() < 1e-9, "20 m over 8 s");
        assert!((steady.tension - 15.0).abs() < 1e-9, "12.5 x 1.2");
        steady.step(false, 1.0, NOW);
        assert!((steady.tension - 0.0).abs() < 1e-9);
        assert!((steady.left_m - 17.65).abs() < 1e-9);
        let mut fighting = fight(u8::MAX);
        fighting.step(true, 1.0, NOW);
        assert!((fighting.left_m - 18.125).abs() < 1e-9, "0.75 of the speed");
        assert!((fighting.tension - 37.5).abs() < 1e-9, "2.5 times the gain");
        assert!(!fight(0b10).behavior.fighting(0) && fight(0b10).behavior.fighting(1));
        let (mut fishing, counter) = equipped();
        fishing.phase = Phase::Reeling {
            fight: Box::new(fight(u8::MAX)),
            active: true,
            last_ms: NOW,
        };
        fishing.keys(
            counter + 1,
            FishingKeys {
                held: true,
                ..FishingKeys::default()
            },
            true,
            NOW,
        );
        fishing.tick(NOW + 3_000);
        assert_eq!(
            states(&fishing).last(),
            Some(&FishingState::FailedLineBreak)
        );
    }

    #[test]
    fn the_cast_bar_fills_in_two_seconds_and_falls_back() {
        let range = Range {
            low: 5.0,
            high: 20.0,
        };
        assert!((cast_distance(range, 0.0, 0) - 5.0).abs() < 1e-9);
        assert!((cast_distance(range, 0.6, 2_000) - 20.6).abs() < 1e-9);
        assert!((cast_distance(range, 0.0, 3_000) - 12.5).abs() < 1e-9);
        assert!((cast_distance(range, 0.0, 9_000) - 5.0).abs() < 1e-9);
        assert_eq!(flight_ms(30.0), 1_470);
        assert_eq!(flight_ms(1.0), 0);
    }

    #[test]
    fn acknowledged_steps_leave_the_list_and_a_change_sends_it() {
        let mut fishing = PlayerFishing::default();
        let first = fishing.take_own_member(true).expect("first card");
        assert_eq!(
            first.state,
            MemberState::Fishing(FishingComponentState::listing(FIRST_VERSION, Vec::new()))
        );
        assert!(fishing.take_own_member(false).is_none());
        fishing.enter(FishingState::StartedEquippingFishing);
        fishing.enter(FishingState::FishingEquipped);
        fishing.acknowledge(1);
        let member = fishing.take_own_member(false).expect("a change");
        assert_eq!(
            member.state,
            MemberState::Fishing(FishingComponentState::listing(
                FIRST_VERSION + 1,
                vec![StateTransition {
                    sequence: 1,
                    state: FishingState::FishingEquipped
                }]
            ))
        );
        assert_eq!(member.component, FISHING_COMPONENT);
    }

    #[test]
    fn the_roll_bonuses_scale_the_modifiers_as_retail_sent_them() {
        let (bonuses, modifiers) = roll_bonuses(RollModifiers {
            rarity: 1.25 + 0.7,
            size: 0.0,
            summer_rarity: 0.0,
        });
        assert_eq!(bonuses[1].name, "FishRarity");
        assert_eq!(bonuses[1].value, 19_500, "retail gather_node");
        assert_eq!(
            modifiers
                .iter()
                .map(|modifier| modifier.kind)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }
}
