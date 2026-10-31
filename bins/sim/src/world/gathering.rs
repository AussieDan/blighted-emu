// SPDX-License-Identifier: AGPL-3.0-or-later
//! The open world's gather nodes (gather note, 2026-09-18): flint, bushes and the corpses to
//! skin. The client has the nodes in its level data and names one in its gather request, so a
//! node is known here from its first gather. Retail cards a node only once it left its default
//! state: after a gather it sends the node's card with its state Depleted and its interaction off,
//! and when the node grows back (60 to 90 s later for flint) its state Healthy and its interaction
//! on again.
//!
//! A gather with a tool runs for the gatherable's `BaseGatherTime` over the gather efficiency:
//! the tool's (`MinGatherEFF` to `MaxGatherEFF` over its gear score range) plus the gathering
//! efficiency of the tradeskill's rank (Inferred: added; retail skinned with the T1 knife, range
//! 2 to 2, at Skinning rank 0 with efficiency 2.0 in 7.58 s for 15 s of work). A gather by hand
//! runs at efficiency 1. A gatherable may ask for a tradeskill rank or an item in the bag
//! ([`nw_content::gathering::GatherRule`]); without them the request is refused. The gather's
//! game event gives its tradeskill experience ([`SimWorld::grant_event`]).

use std::collections::HashMap;

use bevy_ecs::entity::Entity;
use nw_content::chest_lockouts::Lockout;
use nw_content::corpse_times::{DEFAULT_CORPSE_MS, SKINNED_CORPSE_MS, corpse_ms};
use nw_content::gathering::Gatherable;
use nw_content::loot::{LootContext, loot_level};
use nw_content::tradeskill_xp::NEUTRAL_MODIFIER;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::items::SYNC_MOVED;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BundleGroup, BundleMember, GatherableControllerState, GdeMetadataReplicatedState,
    InteractState, InteractionEndedMsg, InteractionRequest, MemberState, ServerMessage,
    entity_of_raw_id,
};
use nw_sim_core::Position;

use super::loot_bag::LimitsSent;
use super::slots::Slot;
use super::statics::{GatherRun, chest_gather_ended, gather_started};
use super::tradeskill::EventScale;
use super::{Body, Player, SimWorld, gather_answer, gather_interrupted, send_now};
use crate::mobs::spawn_index;
use crate::objectives::Event;
use crate::server::SERVER_EPOCH_UNIX_MS;

/// First slot key of a gather node: its index is added. Keys are never sent; they stay above
/// every player and mob number and below the tutorial's static entities.
pub(super) const GATHERABLE_ENTITY_BASE: u64 = 1 << 39;
/// The components of a node's group: its card, its gatherable controller (class 12) and its
/// interact state (class 2930).
const CARD_COMPONENT: u64 = 0;
const CONTROLLER_COMPONENT: u64 = 3;
const INTERACT_COMPONENT: u64 = 4;
/// `gatherableState` values (client enum): Healthy 4, Depleted 8.
const HEALTHY: u8 = 4;
const DEPLETED: u8 = 8;

/// The status of a refused gather request (gather note: retail 2 for a node another player
/// gathers; Inferred the same for a depleted node).
pub(super) const GATHER_BUSY: u8 = 2;
/// The status of an accepted gather request.
pub(super) const GATHER_ACCEPTED: u8 = 1;
/// The name an open-world gather's accept carries (retail: empty).
const NO_NAME: &[u8] = &[];
/// The server tick a hand gather ends on (retail flint 0.25 s ended 300 to 307 ms after the
/// accept: the gather time rounded up to the tick, plus one).
const GATHER_TICK_MS: u64 = 33;
/// The efficiency of a gather by hand (retail flint, bushes and chests: 1.0), and of a tool
/// gather whose tool has no efficiency row, before the tradeskill's.
const HAND_EFFICIENCY: f32 = 1.0;
/// The gatherers of an open-world node (retail 1).
const ONE_GATHERER: u8 = 1;
/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;
/// The paperdoll slot of each tradeskill's tool (beach-quests note: retail equips of the
/// skinning knife, the pick and the hatchet; the client's `PaperdollSlotTypes` order puts
/// `CHOPPING_SLOT` at 33, `CUTTING_SLOT`, the sickle's, at 34, `DRESSING_SLOT` at 35 and
/// `MINING_SLOT` at 36, before `FISHING_SLOT` 37). A gatherable that needs its tool is gathered
/// only with an item in that slot.
const TOOL_SLOTS: [(&str, u32); 4] = [
    ("Skinning", 35),
    ("Mining", 36),
    ("Logging", 33),
    ("Harvesting", 34),
];
/// The paperdoll slot of the tool of `gatherable`'s tradeskill; `None` for a tradeskill
/// without a tool slot.
pub(super) fn tool_slot(gatherable: &Gatherable) -> Option<u32> {
    TOOL_SLOTS
        .iter()
        .find(|(tradeskill, _)| gatherable.tradeskill.as_deref() == Some(*tradeskill))
        .map(|(_, slot)| *slot)
}

/// The roll of a quest drop's chance, in percent.
const PERCENT: u32 = 100;
/// The firings of a gatherable's game event per gather.
const ONE_GATHER: u32 = 1;
/// The gatherable of a water gather (`GatherableData`: `BaseGatherTime` 3, loot table `Water`).
const WATER_GATHERABLE: &str = "Water";

/// Why a gather request is refused besides a busy node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatherRefusal {
    /// The gatherable needs its tradeskill's tool and none is worn.
    NoTool,
    /// The tradeskill's rank is below the gatherable's `RequiredTradeskillLevel`.
    Rank,
    /// The bag lacks the gatherable's `ItemRestrictionId`.
    Item,
    /// The player opened this loot container and its lockout has not reset.
    Opened,
}

/// A gather that runs.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OpenGather {
    number: u64,
    request: InteractionRequest,
    gatherable: Gatherable,
    /// The loot table it rolls: its crate variant's, else its `FinalLootTable`.
    table: Option<String>,
    /// The mob skinned, for a corpse.
    mob: Option<u64>,
    /// A water gather (kind 15): no node to deplete and no own gather state.
    water: bool,
    end_ms: u64,
}

impl SimWorld {
    /// Player `number` (`entity`) asks at `now_ms` to gather `request` in the open world: a node
    /// of the level (flint, a bush), a loot crate or a dead mob to skin, named by its gatherable
    /// and `name` (a crate's variant name, empty for a node). A known gatherable that nobody
    /// else gathers is accepted (3743, the running gather in the own states) and ends after its
    /// gather time ([`SimWorld::gathers_due`]); a busy one is refused (3743 status 2).
    /// [`SimWorld::interact`] passes each interaction once.
    pub(super) fn gather_open_world(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
        now_ms: u64,
    ) {
        let Some(gatherable) = request
            .gatherable
            .and_then(|crc| self.content.gatherables.get(crc))
            .cloned()
        else {
            tracing::debug!(player = number, ?request, "gather of an unknown gatherable");
            return;
        };
        let Some(position) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return;
        };
        let mob = entity_of_raw_id(&request.target).filter(|mob| spawn_index(*mob).is_some());
        let table = self.gather_table(&gatherable, request, name);
        let opened = gatherable.loot_container
            && self
                .world
                .get::<Player>(entity)
                .is_some_and(|player| player.loot.opened(&request.target));
        let allowed = if opened {
            Err(GatherRefusal::Opened)
        } else if self.holds_tool(entity, &gatherable) {
            self.meets_gather_rule(entity, &gatherable, request.gatherable)
        } else {
            Err(GatherRefusal::NoTool)
        };
        if let Err(refusal) = allowed {
            tracing::debug!(player = number, gatherable = %gatherable.id, ?refusal, "gather refused");
            if let Some(player) = self.world.get::<Player>(entity) {
                let refusal = gather_answer(number, request, GATHER_BUSY, NO_NAME);
                send_now(&mut self.combat_messages, &player.connection, &[refusal]);
            }
            return;
        }
        let tool_efficiency =
            (!gatherable.by_hand).then(|| self.gather_efficiency(entity, &gatherable));
        let started = match mob {
            Some(mob) => self.start_skinning(mob, number),
            None => self
                .nodes
                .start(request.target, position, number)
                .map_err(|busy| tracing::debug!(player = number, ?busy, "gather refused")),
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if started.is_err() {
            let refusal = gather_answer(number, request, GATHER_BUSY, NO_NAME);
            send_now(&mut self.combat_messages, &player.connection, &[refusal]);
            return;
        }
        #[allow(clippy::cast_precision_loss, reason = "a gather time of seconds")]
        let amount = gatherable.gather_ms as f32 / MS_PER_SECOND;
        let (duration_ms, efficiency) = match tool_efficiency {
            Some(efficiency) => {
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a gather time of seconds over a positive efficiency"
                )]
                let tool_ms = (gatherable.gather_ms as f32 / efficiency) as u64;
                (tool_ms, efficiency)
            }
            None => (
                gatherable.gather_ms.div_ceil(GATHER_TICK_MS) * GATHER_TICK_MS + GATHER_TICK_MS,
                HAND_EFFICIENCY,
            ),
        };
        let run = GatherRun {
            amount,
            efficiency,
            gatherers: ONE_GATHERER,
        };
        let accept = gather_answer(number, request, GATHER_ACCEPTED, NO_NAME);
        send_now(&mut self.combat_messages, &player.connection, &[accept]);
        let started = gather_started(request.target_entity, player.gathers, run);
        player.extra_members.extend(started);
        tracing::debug!(player = number, gatherable = %gatherable.id, duration_ms, "gather");
        self.open_gathers.push(OpenGather {
            number,
            request,
            gatherable,
            table,
            mob,
            water: false,
            end_ms: now_ms.saturating_add(duration_ms),
        });
    }

    /// Player `number` gathers water at `now_ms` (interaction kind 15; craft2 report, retail
    /// `Hive_Dye`, 2 gathers): the request names no gatherable and retail answers no 3743; the
    /// gather of the `Water` gatherable ends after its `BaseGatherTime` (retail 3,144 ms for 3 s)
    /// with the interaction's end (3746) and its loot table's water (`WaterT1` 30). There is no
    /// node: two retail gathers 3.7 s apart both gave water.
    pub(super) fn gather_water(&mut self, number: u64, request: InteractionRequest, now_ms: u64) {
        let Some(gatherable) = self
            .content
            .gatherables
            .get(lowercase_crc32(WATER_GATHERABLE))
            .cloned()
        else {
            tracing::debug!(player = number, "water gather without a water gatherable");
            return;
        };
        let duration_ms =
            gatherable.gather_ms.div_ceil(GATHER_TICK_MS) * GATHER_TICK_MS + GATHER_TICK_MS;
        tracing::debug!(player = number, duration_ms, "water gather");
        self.open_gathers.push(OpenGather {
            number,
            request,
            table: gatherable.loot_table.clone(),
            gatherable,
            mob: None,
            water: true,
            end_ms: now_ms.saturating_add(duration_ms),
        });
    }

    /// The loot table of a gather of `gatherable` that `request` names `name`: a loot crate's
    /// is its variant's ([`nw_content::gathering::CrateLoot`]), any other gatherable's its
    /// `FinalLootTable`. A crate whose variant is Unknown rolls its `FinalLootTable` (`Empty`).
    fn gather_table(
        &self,
        gatherable: &Gatherable,
        request: InteractionRequest,
        name: &[u8],
    ) -> Option<String> {
        let variant = gatherable
            .loot_container
            .then(|| {
                let name = std::str::from_utf8(name).ok()?;
                let crc = request.gatherable?;
                self.content.crates.table(request.target_entity, crc, name)
            })
            .flatten();
        if gatherable.loot_container && variant.is_none() {
            tracing::info!(gatherable = %gatherable.id, ?request, "crate of an unknown variant");
        }
        variant
            .map(str::to_owned)
            .or_else(|| gatherable.loot_table.clone())
    }

    /// What player `number` rolls loot with: its level, and for a mob `mob` (killed or skinned)
    /// the mob's level as the enemy and content level (Inferred: the content of a kill is the
    /// creature; nwdb.info lists creature loot by `{!clvl}` ranges around the creature's level)
    /// and its loot tags. A gather has no enemy, and its content and POI levels are Unknown.
    /// `reached_limits` are the player's loot limits at their limit
    /// ([`SimWorld::reached_loot_limits`]).
    pub(super) fn loot_context<'a>(
        &mut self,
        number: u64,
        mob: Option<(u64, &'a [String])>,
        reached_limits: &'a [String],
    ) -> LootContext<'a> {
        let player = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Player>(entity))
            .and_then(|player| {
                self.content
                    .player_loot_levels
                    .of(u64::from(player.experience))
            });
        let enemy = mob
            .and_then(|(mob, _)| self.mobs.level(mob))
            .map(loot_level);
        LootContext {
            enemy,
            content: enemy,
            player,
            poi: None,
            tags: mob.map_or(&[], |(_, tags)| tags),
            reached_limits,
        }
    }

    /// Whether player `entity` may gather `gatherable`: one that needs its tradeskill's tool
    /// (`Restriction` ToolRequired) only with an item in the tool's slot (Inferred: retail
    /// answered the skinning knife's absence with the client's "No equipped Knife" prompt, live
    /// 2026-09-18). A tradeskill without a known slot needs no tool.
    fn holds_tool(&self, entity: Entity, gatherable: &Gatherable) -> bool {
        let has_slot = TOOL_SLOTS
            .iter()
            .any(|(tradeskill, _)| gatherable.tradeskill.as_deref() == Some(*tradeskill));
        !(gatherable.tool_required && has_slot) || self.worn_tool(entity, gatherable).is_some()
    }

    /// Whether player `entity` meets the gather rule of `gatherable` (named by `crc` in the
    /// request): its tradeskill's rank at the rule's rank, and the rule's item in the bag
    /// (Unknown whether the gather takes the item; it stays in the bag here).
    fn meets_gather_rule(
        &self,
        entity: Entity,
        gatherable: &Gatherable,
        crc: Option<u32>,
    ) -> Result<(), GatherRefusal> {
        let rule = crc
            .map(|crc| self.content.gather_rules.of(crc))
            .unwrap_or_default();
        let rank = gatherable
            .tradeskill
            .as_deref()
            .map_or(0, |tradeskill| self.tradeskill_perks(entity, tradeskill).0);
        if u32::from(rank) < rule.required_rank {
            return Err(GatherRefusal::Rank);
        }
        let holds_item = rule.required_item.as_ref().is_none_or(|(item, quantity)| {
            self.world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .is_some_and(|gear| gear.bag_count(item) >= *quantity)
        });
        if holds_item {
            Ok(())
        } else {
            Err(GatherRefusal::Item)
        }
    }

    /// The gather efficiency of player `entity` with a tool on `gatherable`: its tradeskill's
    /// tool at its gear score (else [`HAND_EFFICIENCY`]), plus the gathering efficiency of the
    /// tradeskill's rank and of the player's held status effects (`EFF_*`).
    fn gather_efficiency(&self, entity: Entity, gatherable: &Gatherable) -> f32 {
        let tool = self.worn_tool(entity, gatherable).and_then(|item| {
            let efficiency = self.content.tools.of(item)?;
            let gear_score = self.content.kits.item(item).map_or(0, |kit| kit.gear_score);
            Some(efficiency.at(gear_score))
        });
        let rank = gatherable.tradeskill.as_deref().map_or(0.0, |tradeskill| {
            let effects = self.world.get::<Player>(entity).map_or(0.0, |player| {
                player
                    .status_effects
                    .gather_efficiency(&self.content.status_effects, tradeskill)
            });
            self.tradeskill_perks(entity, tradeskill)
                .1
                .gathering_efficiency
                + effects
        });
        #[allow(
            clippy::cast_possible_truncation,
            reason = "efficiencies of a few units fit an f32"
        )]
        let efficiency = (tool.unwrap_or(f64::from(HAND_EFFICIENCY)) + rank) as f32;
        efficiency.max(HAND_EFFICIENCY)
    }

    /// The item player `entity` wears in the tool slot of `gatherable`'s tradeskill.
    fn worn_tool(&self, entity: Entity, gatherable: &Gatherable) -> Option<&str> {
        let slot = tool_slot(gatherable)?;
        self.world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .and_then(|gear| gear.worn_item(slot))
    }

    /// Player `number` starts to skin `mob`: only a dead mob that nobody skinned or skins.
    fn start_skinning(&self, mob: u64, number: u64) -> Result<(), ()> {
        let dead = self.mobs.health(mob) == Some(0);
        let taken = self.skinned.contains_key(&mob)
            || self
                .open_gathers
                .iter()
                .any(|gather| gather.mob == Some(mob) && gather.number != number);
        if dead && !taken {
            Ok(())
        } else {
            tracing::debug!(player = number, mob, dead, taken, "skinning refused");
            Err(())
        }
    }

    /// The gathers whose time came at `now_ms` end: the interaction ends (3746), the own gather
    /// state is done, the loot of the gatherable (less what the player's loot limits hold back)
    /// and its quest drops go into the bag and to the
    /// open-world quest, its game event goes to the quest (a quest object), and the node is
    /// depleted until its respawn (a corpse is skinned). The depleted nodes whose time came grow
    /// back.
    pub(super) fn gathers_due(&mut self, now_ms: u64) {
        self.nodes.tick(now_ms);
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open_gathers)
            .into_iter()
            .partition(|gather| now_ms >= gather.end_ms);
        self.open_gathers = later;
        for gather in due {
            self.end_gather(&gather, now_ms);
        }
    }

    /// The corpses whose time came at `now_ms` go (`nw_content::corpse_times`): a
    /// skinned one [`SKINNED_CORPSE_MS`] after its skinning, one that can be skinned stays until
    /// then, any other after its creature's corpse time.
    pub(super) fn bury_corpses(&mut self, now_ms: u64) {
        let content = std::sync::Arc::clone(&self.content);
        let due: Vec<u64> = self
            .mobs
            .corpses()
            .filter(|&(mob, died_ms)| {
                let gone_at_ms = if let Some(&skinned_ms) = self.skinned.get(&mob) {
                    Some(skinned_ms + SKINNED_CORPSE_MS)
                } else {
                    let creature = self
                        .mobs
                        .vitals_of(mob)
                        .and_then(|vitals| content.mob_looks.look(vitals, self.mobs.asset(mob)))
                        .and_then(|look| content.creature_animations.get(&look.asset.id));
                    match creature {
                        Some(creature) if creature.skinnable() => None,
                        Some(creature) => Some(died_ms + corpse_ms(&creature.slice)),
                        None => Some(died_ms + DEFAULT_CORPSE_MS),
                    }
                };
                gone_at_ms.is_some_and(|gone_at_ms| now_ms >= gone_at_ms)
            })
            .map(|(mob, _)| mob)
            .collect();
        for mob in due {
            self.mobs.bury(mob);
        }
    }

    fn end_gather(&mut self, gather: &OpenGather, now_ms: u64) {
        let content = std::sync::Arc::clone(&self.content);
        let tags = gather
            .mob
            .and_then(|mob| self.mobs.vitals_of(mob))
            .and_then(|vitals| content.mob_loot.of(vitals))
            .map_or(&[][..], |drop| drop.tags.as_slice());
        let reached = self.reached_loot_limits(gather.number);
        let levels = self.loot_context(gather.number, gather.mob.map(|mob| (mob, tags)), &reached);
        let dice = &mut self.dice;
        let rolled = gather
            .table
            .as_deref()
            .map(|table| {
                content
                    .loot
                    .roll(table, levels, &mut |below| dice.below(below))
            })
            .unwrap_or_default();
        let hints = rolled.gear_scores.clone();
        let mut loot = self.limit_loot(gather.number, rolled);
        self.send_loot_limits(gather.number, LimitsSent::Changed);
        loot.extend(self.quest_drops(gather.number, gather.mob, &gather.gatherable));
        match gather.mob {
            Some(mob) => {
                self.skinned.insert(mob, now_ms);
            }
            None if gather.water => {}
            // A container stays for everyone else: only its opener's tracker locks it.
            None if self.lockout_of(gather).is_some() => self.nodes.release(&gather.request.target),
            None => {
                let (shortest, longest) = gather.gatherable.respawn_ms;
                let spread = u32::try_from(longest.saturating_sub(shortest)).unwrap_or(u32::MAX);
                let respawn_at_ms = now_ms
                    .saturating_add(shortest)
                    .saturating_add(u64::from(self.dice.below(spread.saturating_add(1))));
                self.nodes.deplete(&gather.request.target, respawn_at_ms);
            }
        }
        // A container's lockout ends at the next reset of its kind, from now.
        let opened = self.lockout_of(gather).and_then(|lockout| {
            let opened_unix_ms = self.server_ms.saturating_add(SERVER_EPOCH_UNIX_MS);
            let reset_ms = lockout
                .ends_at(opened_unix_ms)
                .saturating_sub(SERVER_EPOCH_UNIX_MS);
            Some((reset_ms, self.nodes.position_of(&gather.request.target)?))
        });
        let Some(entity) = self.player_by_number(gather.number) else {
            return;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let end = ServerMessage::InteractionEnded(InteractionEndedMsg {
            header: nw_messages::FacetHeader::of(
                nw_messages::facet::component::INTERACTOR,
                gather.number,
            ),
        });
        send_now(&mut self.combat_messages, &player.connection, &[end]);
        if !gather.water {
            let ended = chest_gather_ended(gather.request.target_entity, player.gathers);
            player.extra_members.push(ended);
            player.gathers = player.gathers.saturating_add(1);
        }
        player.ended_gather = Some(gather.request);
        if let Some((reset_ms, at)) = opened {
            player.list_version += 1;
            let version = player.list_version;
            let tracked = player.loot.track_opened(
                gather.request.target,
                at,
                self.server_ms,
                reset_ms,
                true,
                version,
            );
            player.extra_members.push(tracked);
        }
        let (items, rolls) = self.roll_drops(gather.number, gather.mob, loot, &hints);
        let gathered = items.clone();
        self.give_drops(
            gather.number,
            items.into_iter().zip(rolls).collect(),
            now_ms,
        );
        for (item, quantity) in &gathered {
            self.quest_event(
                gather.number,
                Event::Gathered {
                    item,
                    quantity: *quantity,
                },
                now_ms,
            );
        }
        if let Some(crc) = gather.request.gatherable {
            for (cyclic, state) in self.cyclic_gathers(entity, crc) {
                self.quest_event(
                    gather.number,
                    Event::CyclicGathered {
                        cyclic: &cyclic,
                        state,
                    },
                    now_ms,
                );
            }
        }
        self.wear_tool(entity, &gather.gatherable);
        if let Some(event) = &gather.gatherable.game_event {
            let scale = self.gather_scale(gather);
            self.grant_scaled_event(gather.number, event, ONE_GATHER, scale);
            self.quest_event(gather.number, Event::GameEvent(event), now_ms);
        }
        if let Some(tradeskill) = &gather.gatherable.tradeskill {
            self.meta_event(
                entity,
                super::meta_achievements::MetaEvent::Gathered { tradeskill },
            );
        }
    }

    /// The cyclic gatherables (variant id) and their states that player `entity` gathers with a
    /// gather of the gatherable whose lowercase id has the CRC-32 `crc`: the state the
    /// gatherable shows, else the player's turns of it so far (which this gather adds to).
    fn cyclic_gathers(&mut self, entity: Entity, crc: u32) -> Vec<(String, u32)> {
        let shown = self.content.cyclic_gatherables.of(crc).to_vec();
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return Vec::new();
        };
        shown
            .into_iter()
            .map(|shown| {
                let state = shown.state.unwrap_or_else(|| {
                    let turns = player
                        .cyclic_turns
                        .entry(shown.cyclic.to_lowercase())
                        .or_default();
                    let state = *turns % shown.states;
                    *turns = turns.wrapping_add(1);
                    state
                });
                (shown.cyclic, state)
            })
            .collect()
    }

    /// The lockout of the loot container `gather` opens; `None` for a gather of another kind
    /// ([`nw_content::chest_lockouts`]).
    fn lockout_of(&self, gather: &OpenGather) -> Option<Lockout> {
        if !gather.gatherable.loot_container || gather.mob.is_some() || gather.water {
            return None;
        }
        gather
            .request
            .gatherable
            .and_then(|crc| self.content.chest_lockouts.of(crc))
    }

    /// How the game event of `gather` scales: a skinned corpse by the `GatherableLevelModifier`
    /// of the mob's level in the tradeskill's rank table, a node not at all (craft2 report:
    /// retail boars of level 2 and 3 gave 198 and 208 for `Gather_SkinningT2` 95).
    fn gather_scale(&self, gather: &OpenGather) -> EventScale {
        let modifier = gather
            .mob
            .and_then(|mob| self.mobs.level(mob))
            .zip(gather.gatherable.tradeskill.as_deref())
            .map_or(NEUTRAL_MODIFIER, |(level, tradeskill)| {
                self.content
                    .tradeskill_curves
                    .level_modifier(tradeskill, level)
            });
        EventScale::gather(modifier)
    }

    /// The items the quests of player `number` drop for its gather of `gatherable`: the corpse
    /// of `mob` it skinned, a quest object, or a container. One of each waiting task's item whose
    /// enemy the mob is, or whose gatherable the object is, or that a container in the player's
    /// place drops, at the task's chance.
    fn quest_drops(
        &mut self,
        number: u64,
        mob: Option<u64>,
        gatherable: &Gatherable,
    ) -> Vec<(String, u32)> {
        let quests = self.quests_of_player(number);
        if quests.is_empty() {
            return Vec::new();
        }
        let drops: Vec<(String, u32)> = match mob {
            Some(mob) => {
                let Some((vitals_id, categories)) = self
                    .mobs
                    .vitals_of(mob)
                    .and_then(|vitals| self.content.kill_types.of(vitals))
                else {
                    return Vec::new();
                };
                let spawn_categories = self
                    .content
                    .kill_types
                    .spawn_categories(self.mobs.category_of(mob));
                quests
                    .iter()
                    .flat_map(|quest| {
                        quest
                            .run
                            .quest_drops(vitals_id, categories, &spawn_categories)
                    })
                    .collect()
            }
            // A quest object is most often a loot container too (346 of the 381 steps that drop
            // from a gatherable, `1225_Truffle` of Truffle Fever among them): it gives its own
            // drops and the drops of any container in a mission's place.
            None => quests
                .iter()
                .flat_map(|quest| {
                    let mut drops = quest.run.gather_drops(&gatherable.id);
                    if gatherable.loot_container {
                        drops.extend(quest.run.chest_drops());
                    }
                    drops
                })
                .collect(),
        };
        drops
            .into_iter()
            .filter(|(_, percent)| self.dice.below(PERCENT) < *percent)
            .map(|(item, _)| (item, 1))
            .collect()
    }

    /// Puts rolled `loot` into the bag of player `number`; each item that fits is an event of
    /// its quest. An item with no master row, or that does not fit, is lost with a log line.
    pub(super) fn give_loot(&mut self, number: u64, loot: Vec<(String, u32)>, now_ms: u64) {
        self.give_loot_as(number, loot, SYNC_MOVED, now_ms);
    }

    /// [`SimWorld::give_loot`] for a bag change of sync reason `reason` (a craft's output).
    pub(super) fn give_loot_as(
        &mut self,
        number: u64,
        loot: Vec<(String, u32)>,
        reason: u8,
        now_ms: u64,
    ) {
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let mut gained = Vec::new();
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            for (item_id, quantity) in loot {
                let Some(item) = self.content.kits.item(&item_id) else {
                    tracing::debug!(item = %item_id, "loot item without a master row");
                    continue;
                };
                let added = player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.add_as(item, quantity, reason));
                if added {
                    tracing::debug!(player = number, item = %item_id, quantity, "looted");
                    gained.push((item_id, quantity));
                } else {
                    tracing::info!(player = number, item = %item_id, quantity, "loot lost: no bag or a full bag");
                }
            }
        }
        for (item, quantity) in &gained {
            self.quest_event(
                number,
                Event::ItemGained {
                    item,
                    quantity: *quantity,
                },
                now_ms,
            );
        }
    }

    /// The client of player `entity` ends interaction `interaction` at `now_ms`: a gather that
    /// still runs stops without a harvest, unless its end time has come (then it ends with its
    /// harvest) (the own gather state done, as retail sends it after a cancel);
    /// after a gather's own end the client's cancel is its acknowledgement and changes nothing.
    /// Both get the gatherable's 2724, as retail sends it (loot note: after 98 of 126 retail
    /// cancels of a running gather and 1,561 of 3,042 after the end; why not after the others
    /// is Unknown).
    pub(super) fn cancel_gather(&mut self, entity: Entity, interaction: u64, now_ms: u64) {
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        let mut running = self.open_gathers.iter().position(|gather| {
            gather.number == number && gather.request.interaction == interaction
        });
        // A cancel at or after the gather's end time comes as the client's own end, before the
        // tick that ends it here: the gather ends, it is not stopped, and the cancel is its
        // acknowledgement (live 2026-10-29: the watchtower crate's 2,541 ms gather got its
        // cancel at 2,571 ms and gave nothing).
        if let Some(at) = running
            && now_ms >= self.open_gathers[at].end_ms
        {
            let gather = self.open_gathers.remove(at);
            self.end_gather(&gather, now_ms);
            running = None;
        }
        let request = match running {
            Some(at) => {
                let gather = self.open_gathers.remove(at);
                if gather.mob.is_none() {
                    self.nodes.release(&gather.request.target);
                }
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    let stopped = chest_gather_ended(gather.request.target_entity, player.gathers);
                    player.extra_members.push(stopped);
                    player.gathers = player.gathers.saturating_add(1);
                }
                gather.request
            }
            None => {
                let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                    return;
                };
                match player.ended_gather {
                    Some(ended) if ended.interaction == interaction => {
                        player.ended_gather = None;
                        ended
                    }
                    _ => return,
                }
            }
        };
        if let Some(player) = self.world.get::<Player>(entity) {
            let interrupted = gather_interrupted(request);
            send_now(
                &mut self.combat_messages,
                &player.connection,
                &[interrupted],
            );
        }
    }
}

/// One gather node that left its default state.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    gde_ref: [u8; RAW_ID_LEN],
    /// Where its first gatherer stood (Inferred: a node is within reach of its gatherer).
    position: Position,
    /// The player who gathers it now.
    gatherer: Option<u64>,
    /// When it grows back, while it is depleted.
    respawn_at_ms: Option<u64>,
    /// Counts its state changes, so each player gets each one once.
    version: u32,
}

impl Node {
    const fn depleted(&self) -> bool {
        self.respawn_at_ms.is_some()
    }

    /// Its card, controller and interact members (`with_card`), or its last two alone.
    fn members(&self, with_card: bool) -> Vec<BundleMember> {
        let (state, enabled) = if self.depleted() {
            (DEPLETED, false)
        } else {
            (HEALTHY, true)
        };
        let card = with_card.then_some(BundleMember {
            component: CARD_COMPONENT,
            state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
                asset: None,
                gde_ref: Some(self.gde_ref),
            }),
        });
        card.into_iter()
            .chain([
                BundleMember {
                    component: CONTROLLER_COMPONENT,
                    state: MemberState::GatherableController(GatherableControllerState {
                        gatherable_state: Some(state),
                    }),
                },
                BundleMember {
                    component: INTERACT_COMPONENT,
                    state: MemberState::Interact(InteractState {
                        enabled: Some(enabled),
                        has_interactors: None,
                    }),
                },
            ])
            .collect()
    }
}

/// Why a gather request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeBusy {
    /// Another player gathers it.
    Gathered,
    /// It is depleted.
    Depleted,
}

/// The gather nodes of one sim.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Nodes {
    nodes: Vec<Node>,
    by_ref: HashMap<[u8; RAW_ID_LEN], usize>,
}

impl Nodes {
    /// Player `player` at `position` starts to gather node `gde_ref`; refused while another
    /// player gathers it or it is depleted.
    pub(super) fn start(
        &mut self,
        gde_ref: [u8; RAW_ID_LEN],
        position: Position,
        player: u64,
    ) -> Result<(), NodeBusy> {
        let index = *self.by_ref.entry(gde_ref).or_insert_with(|| {
            self.nodes.push(Node {
                gde_ref,
                position,
                gatherer: None,
                respawn_at_ms: None,
                version: 0,
            });
            self.nodes.len() - 1
        });
        let node = &mut self.nodes[index];
        if node.depleted() {
            return Err(NodeBusy::Depleted);
        }
        if node.gatherer.is_some_and(|gatherer| gatherer != player) {
            return Err(NodeBusy::Gathered);
        }
        node.gatherer = Some(player);
        Ok(())
    }

    /// The gather of node `gde_ref` ends without a harvest (a cancel).
    pub(super) fn release(&mut self, gde_ref: &[u8; RAW_ID_LEN]) {
        if let Some(node) = self
            .by_ref
            .get(gde_ref)
            .map(|index| &mut self.nodes[*index])
        {
            node.gatherer = None;
        }
    }

    /// Where node `gde_ref` was first gathered; `None` for a node nobody gathered.
    pub(super) fn position_of(&self, gde_ref: &[u8; RAW_ID_LEN]) -> Option<Position> {
        self.by_ref
            .get(gde_ref)
            .map(|index| self.nodes[*index].position)
    }

    /// Node `gde_ref` was harvested: depleted until `respawn_at_ms`.
    pub(super) fn deplete(&mut self, gde_ref: &[u8; RAW_ID_LEN], respawn_at_ms: u64) {
        if let Some(node) = self
            .by_ref
            .get(gde_ref)
            .map(|index| &mut self.nodes[*index])
        {
            node.gatherer = None;
            node.respawn_at_ms = Some(respawn_at_ms);
            node.version += 1;
        }
    }

    /// The depleted nodes whose time came at `now_ms` grow back.
    pub(super) fn tick(&mut self, now_ms: u64) {
        for node in &mut self.nodes {
            if node.respawn_at_ms.is_some_and(|at| now_ms >= at) {
                node.respawn_at_ms = None;
                node.version += 1;
            }
        }
    }

    /// The slot keys of the nodes shown to a player at `position`: those that left their default
    /// state, within `radius_m` (the interest radius).
    pub(super) fn numbers_near(
        &self,
        position: Position,
        radius_m: f32,
    ) -> impl Iterator<Item = u64> + '_ {
        (0_u64..)
            .zip(&self.nodes)
            .filter(move |(_, node)| {
                node.version > 0 && node.position.horizontal_distance(position) <= radius_m
            })
            .map(|(index, _)| GATHERABLE_ENTITY_BASE + index)
    }

    /// The group of the node of slot key `number` in `slot` for a player who got its version
    /// `sent`: its card and states when the slot is new, its states after a change, else none.
    /// Updates `sent`.
    pub(super) fn group(
        &self,
        number: u64,
        slot: Slot,
        sent: &mut HashMap<u64, u32>,
    ) -> Option<BundleGroup> {
        let index = usize::try_from(number.checked_sub(GATHERABLE_ENTITY_BASE)?).ok()?;
        let node = self.nodes.get(index)?;
        if !slot.new && sent.get(&number) == Some(&node.version) {
            return None;
        }
        sent.insert(number, node.version);
        Some(BundleGroup {
            interest: u64::from(slot.interest),
            members: node.members(slot.new),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_content::gathering::{
        CyclicGatherables, GatherRule, GatherRules, Gatherables, ToolEfficiencies, ToolEfficiency,
    };
    use nw_content::progression::{Progression, RankPerks, TradeskillRankRow, TradeskillSource};
    use nw_content::rewards::{EventGrant, EventGrants};
    use nw_formats::objectstream::lowercase_crc32;

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::world::gear;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    const FLINT: [u8; RAW_ID_LEN] = [0x5b; RAW_ID_LEN];
    /// The interest radius of the test views.
    const RADIUS_M: f32 = 100.0;
    const AT: Position = Position {
        x: 7305.0,
        y: 3093.0,
        z: 20.0,
    };
    const PLAYER: u64 = 7;
    const RESPAWN_AT_MS: u64 = 61_500;

    fn state_of(group: &BundleGroup) -> Option<u8> {
        group.members.iter().find_map(|member| match &member.state {
            MemberState::GatherableController(state) => state.gatherable_state,
            _ => None,
        })
    }

    #[test]
    fn a_node_is_gathered_once_until_it_grows_back() {
        let mut nodes = Nodes::default();
        assert_eq!(nodes.start(FLINT, AT, PLAYER), Ok(()));
        assert_eq!(nodes.start(FLINT, AT, PLAYER + 1), Err(NodeBusy::Gathered));
        assert_eq!(
            nodes.numbers_near(AT, RADIUS_M).count(),
            0,
            "a node in its default state is not shown"
        );
        nodes.deplete(&FLINT, RESPAWN_AT_MS);
        assert_eq!(nodes.start(FLINT, AT, PLAYER), Err(NodeBusy::Depleted));
        let number = GATHERABLE_ENTITY_BASE;
        assert_eq!(
            nodes.numbers_near(AT, RADIUS_M).collect::<Vec<_>>(),
            [number]
        );
        let mut sent = HashMap::new();
        let new = Slot {
            interest: 9,
            new: true,
        };
        let card = nodes.group(number, new, &mut sent).expect("card");
        assert_eq!(card.members.len(), 3, "card, controller, interact");
        assert_eq!(state_of(&card), Some(DEPLETED));
        let kept = Slot { new: false, ..new };
        assert_eq!(nodes.group(number, kept, &mut sent), None, "sent once");
        nodes.tick(RESPAWN_AT_MS - 1);
        assert_eq!(nodes.start(FLINT, AT, PLAYER), Err(NodeBusy::Depleted));
        nodes.tick(RESPAWN_AT_MS);
        let grown = nodes.group(number, kept, &mut sent).expect("change");
        assert_eq!(grown.members.len(), 2, "the states without the card");
        assert_eq!(state_of(&grown), Some(HEALTHY));
        assert_eq!(nodes.start(FLINT, AT, PLAYER + 1), Ok(()));
        nodes.release(&FLINT);
        assert_eq!(nodes.start(FLINT, AT, PLAYER), Ok(()), "a cancel frees it");
        let far = Position {
            x: AT.x + RADIUS_M + 1.0,
            ..AT
        };
        assert_eq!(nodes.numbers_near(far, RADIUS_M).count(), 0);
    }

    /// The player's number.
    const NUMBER: u64 = 1;
    /// A mining node, a node that asks for Mining rank 5 and a barrel that asks for a key.
    const VEIN: &str = "IronVeinSmall";
    const HIGH_VEIN: &str = "OreVeinOrichalcumSmall";
    const BARREL: &str = "0402_GruelBarrel";
    const KEY: &str = "0402_0401_BileSoakedMeat";
    const PICK: &str = "2hPickT1";
    const MINING_EVENT: &str = "Gather_MiningT1";
    /// `Gather_MiningT1`'s Mining experience (build 25810808).
    const MINING_REWARD: f64 = 64.0;
    /// The vein's `BaseGatherTime` and the pick's efficiency.
    const VEIN_MS: u64 = 8_000;
    const PICK_EFFICIENCY: f64 = 2.0;
    /// The Mining slot of the paperdoll ([`TOOL_SLOTS`]).
    const MINING_SLOT: u32 = 36;

    fn gatherable(id: &str, by_hand: bool) -> Gatherable {
        Gatherable {
            id: id.to_owned(),
            gather_ms: VEIN_MS,
            respawn_ms: (RESPAWN_AT_MS, RESPAWN_AT_MS),
            loot_table: None,
            loot_container: false,
            tradeskill: (!by_hand).then(|| "Mining".to_owned()),
            tool_required: !by_hand,
            by_hand,
            game_event: (!by_hand).then(|| MINING_EVENT.to_owned()),
        }
    }

    fn content() -> PlayerContent {
        let rank = |rank: i64, cap: f64| TradeskillRankRow {
            rank,
            cap,
            perks: RankPerks {
                gathering_efficiency: 0.0,
                recipe_level: 0,
                ..RankPerks::default()
            },
        };
        PlayerContent {
            kits: gear::tests::kits_with(&[
                (PICK, "EquippableTool+Axe+PickAxe"),
                (KEY, "Resource+Quest"),
            ]),
            gatherables: Gatherables::build([
                gatherable(VEIN, false),
                gatherable(HIGH_VEIN, false),
                gatherable(BARREL, true),
            ]),
            gather_rules: GatherRules::build([
                (
                    HIGH_VEIN.to_owned(),
                    GatherRule {
                        required_rank: 5,
                        required_item: None,
                    },
                ),
                (
                    BARREL.to_owned(),
                    GatherRule {
                        required_rank: 0,
                        required_item: Some((KEY.to_owned(), 1)),
                    },
                ),
            ]),
            tools: ToolEfficiencies::build([(
                PICK.to_owned(),
                ToolEfficiency {
                    efficiency: (PICK_EFFICIENCY, PICK_EFFICIENCY),
                    gear_score: (0, 0),
                },
            )]),
            progression: Progression::default().with_tradeskills([TradeskillSource {
                id: "Mining".to_owned(),
                max_levels: vec![200],
                ranks: vec![rank(0, 100.0), rank(1, 105.0)],
            }]),
            event_grants: EventGrants::build([(
                MINING_EVENT.to_owned(),
                EventGrant {
                    progression: Some(("Mining".to_owned(), MINING_REWARD)),
                    coins: (0, 0),
                    territory_standing: 0.0,
                },
            )]),
            ..PlayerContent::default()
        }
    }

    /// A world with the test content and one player wearing the pick.
    fn admitted() -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(content());
        let mut admit = admit_at(NUMBER, 0.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.by_connection[&connection(NUMBER)];
        let pick = world.content.kits.item(PICK).cloned().expect("pick");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player
                .gear
                .as_mut()
                .expect("gear")
                .renew(&pick, 1, MINING_SLOT, 0);
        }
        (world, entity)
    }

    fn request(interaction: u64, target: u8, gatherable: &str) -> InteractionRequest {
        InteractionRequest {
            target: [target; RAW_ID_LEN],
            target_entity: interaction,
            interaction,
            kind: nw_messages::client_input::GATHER_KIND,
            gatherable: Some(lowercase_crc32(gatherable)),
        }
    }

    fn mining(world: &SimWorld, entity: Entity) -> u32 {
        let mastery = world
            .content
            .progression
            .tradeskill("Mining")
            .expect("mining");
        world
            .world
            .get::<Player>(entity)
            .map_or(0, |player| player.masteries.experience(mastery))
    }

    #[test]
    fn a_tool_gather_runs_at_the_tools_efficiency_and_gives_its_tradeskill_experience() {
        let (mut world, entity) = admitted();
        world.gather_open_world(entity, NUMBER, request(1, 0x11, VEIN), &[], NOW);
        let [gather] = world.open_gathers.as_slice() else {
            panic!("one gather runs: {:?}", world.open_gathers);
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a gather time of seconds"
        )]
        let expected_ms = (VEIN_MS as f64 / PICK_EFFICIENCY) as u64;
        assert_eq!(
            gather.end_ms,
            NOW + expected_ms,
            "8 s of work at efficiency 2"
        );
        world.gathers_due(NOW + expected_ms);
        assert!(world.open_gathers.is_empty());
        assert_eq!(
            mining(&world, entity),
            64,
            "Gather_MiningT1 under a neutral multiplier"
        );
    }

    #[test]
    fn a_harvest_takes_the_sickle_slot_and_each_tool_its_own() {
        let of = |tradeskill: &str| {
            tool_slot(&Gatherable {
                tradeskill: Some(tradeskill.to_owned()),
                ..gatherable(VEIN, false)
            })
        };
        assert_eq!(
            [
                of("Logging"),
                of("Harvesting"),
                of("Skinning"),
                of("Mining")
            ],
            [Some(33), Some(34), Some(35), Some(36)],
            "CHOPPING, CUTTING, DRESSING and MINING_SLOT"
        );
        assert_eq!(of("Fishing"), None, "the pole has its own path");
    }

    #[test]
    fn a_cancel_at_the_gathers_end_time_ends_it_with_its_harvest() {
        // Live 2026-10-29: the watchtower crate's cancel came 30 ms after its end time, before
        // the tick that ends it, and the crate gave nothing.
        let (mut world, entity) = admitted();
        world.gather_open_world(entity, NUMBER, request(1, 0x11, VEIN), &[], NOW);
        let end_ms = world.open_gathers.first().expect("the gather runs").end_ms;
        world.cancel_gather(entity, 1, end_ms - 1);
        assert_eq!(world.open_gathers.len(), 0, "stopped before its end");
        assert_eq!(mining(&world, entity), 0, "no harvest before its end");
        world.gather_open_world(entity, NUMBER, request(2, 0x12, VEIN), &[], NOW);
        let end_ms = world.open_gathers.first().expect("the gather runs").end_ms;
        world.cancel_gather(entity, 2, end_ms);
        assert!(world.open_gathers.is_empty());
        assert!(
            mining(&world, entity) > 0,
            "the harvest of a gather whose time came"
        );
    }

    #[test]
    fn a_gather_asks_for_its_rank_and_its_item() {
        let (mut world, entity) = admitted();
        world.gather_open_world(entity, NUMBER, request(1, 0x12, HIGH_VEIN), &[], NOW);
        assert!(world.open_gathers.is_empty(), "Mining rank 0 is below 5");
        world.gather_open_world(entity, NUMBER, request(2, 0x13, BARREL), &[], NOW);
        assert!(world.open_gathers.is_empty(), "no key in the bag");
        let key = world.content.kits.item(KEY).cloned().expect("key");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            assert!(player.gear.as_mut().expect("gear").add(&key, 1));
        }
        world.gather_open_world(entity, NUMBER, request(3, 0x13, BARREL), &[], NOW);
        let [gather] = world.open_gathers.as_slice() else {
            panic!("the barrel is gathered: {:?}", world.open_gathers);
        };
        assert_eq!(
            gather.end_ms,
            NOW + VEIN_MS.div_ceil(GATHER_TICK_MS) * GATHER_TICK_MS + GATHER_TICK_MS,
            "by hand: the tick after its time"
        );
    }

    #[test]
    fn a_quest_object_that_is_a_loot_container_gives_its_quest_drop() {
        // Live play test 2026-10-29: Truffle Fever's truffle (`1225_Truffle`, `IsLootContainer`)
        // gave no `1225_0101_Truffle`. Client rows of the quest's gather step.
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook};
        const TRUFFLE: &str = "1225_Truffle";
        const QUEST_TRUFFLE: &str = "1225_0101_Truffle";
        const QUEST: &str = "1225_0101_truffles";
        let (mut world, entity) = admitted();
        let mut content = (*world.content).clone();
        content.gatherables = Gatherables::build([Gatherable {
            loot_container: true,
            ..gatherable(TRUFFLE, true)
        }]);
        content.kits = gear::tests::kits_with(&[
            (PICK, "EquippableTool+Axe+PickAxe"),
            (QUEST_TRUFFLE, "Resource+Quest"),
        ]);
        content.quests = QuestBook::build(
            [ObjectiveRow {
                id: QUEST.to_owned(),
                root: "Truffles".to_owned(),
                ..ObjectiveRow::default()
            }],
            TaskBook::new(vec![TaskRow {
                task_id: "Truffles".to_owned(),
                kind: "TaskGatherAndReturnItems".to_owned(),
                item_name: Some(QUEST_TRUFFLE.to_owned()),
                item_drop_vc: Some(TRUFFLE.to_owned()),
                item_drop_probability: Some(100),
                target_qty: Some(5),
                ..TaskRow::default()
            }]),
        );
        world.content = Arc::new(content);
        world.start_quest(NUMBER, QUEST, NOW).expect("quest");
        world.gather_open_world(entity, NUMBER, request(1, 0x16, TRUFFLE), &[], NOW);
        let end_ms = world
            .open_gathers
            .first()
            .expect("the truffle is gathered")
            .end_ms;
        world.gathers_due(end_ms);
        let truffles: u32 = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(super::super::Gear::carried)
            .unwrap_or_default()
            .iter()
            .filter(|item| item.item_id == QUEST_TRUFFLE)
            .map(|item| item.quantity)
            .sum();
        assert_eq!(truffles, 1, "the quest truffle is in the bag");
    }

    #[test]
    fn a_cyclic_gather_names_its_state_from_its_gatherable_or_the_turns() {
        // Client rows: a puzzle stone shows one gatherable in its 4 states, the staff ritual one
        // gatherable per state; retail `99c_msq_03_ancientbridge` turned a stone from state 0.
        const STONE: &str = "Cyclic_99C_MSQ_puzzle1_stone2";
        const RITUAL: &str = "Cyclic_09A_M13_ArcaneRitual";
        const RITUAL_1: &str = "09A_M13_ArcaneRitual_1";
        let (mut world, entity) = admitted();
        let mut content = (*world.content).clone();
        content.gatherables = Gatherables::build([gatherable(STONE, true)]);
        content.cyclic_gatherables = CyclicGatherables::build([
            (STONE.to_owned(), vec![Some(STONE.to_owned()); 4]),
            (
                RITUAL.to_owned(),
                vec![None, Some(RITUAL_1.to_owned()), None],
            ),
        ]);
        world.content = Arc::new(content);
        world.gather_open_world(entity, NUMBER, request(1, 0x15, STONE), &[], NOW);
        let end_ms = world.open_gathers.first().expect("the stone turns").end_ms;
        world.gathers_due(end_ms);
        let turns = |world: &SimWorld| {
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .cyclic_turns[&STONE.to_lowercase()]
        };
        assert_eq!(turns(&world), 1, "the gather end turned it");
        let states: Vec<u32> = (0..4)
            .flat_map(|_| world.cyclic_gathers(entity, lowercase_crc32(STONE)))
            .map(|(_, state)| state)
            .collect();
        assert_eq!(states, [1, 2, 3, 0], "the turns go round its 4 states");
        assert_eq!(
            world.cyclic_gathers(entity, lowercase_crc32(RITUAL_1)),
            [(RITUAL.to_owned(), 1)]
        );
    }

    #[test]
    fn a_water_gather_ends_without_a_node_or_an_own_gather_state() {
        let (mut world, entity) = admitted();
        let mut content = (*world.content).clone();
        content.gatherables = Gatherables::build([gatherable(WATER_GATHERABLE, true)]);
        world.content = Arc::new(content);
        let water = InteractionRequest {
            kind: nw_messages::client_input::WATER_GATHER_KIND,
            gatherable: None,
            ..request(1, 0x14, WATER_GATHERABLE)
        };
        world.gather_water(NUMBER, water, NOW);
        let [gather] = world.open_gathers.as_slice() else {
            panic!("one water gather runs: {:?}", world.open_gathers);
        };
        assert!(gather.water);
        let end_ms = gather.end_ms;
        assert_eq!(
            end_ms,
            NOW + VEIN_MS.div_ceil(GATHER_TICK_MS) * GATHER_TICK_MS + GATHER_TICK_MS
        );
        let gathers = world.world.get::<Player>(entity).expect("player").gathers;
        world.gathers_due(end_ms);
        assert!(world.open_gathers.is_empty());
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!(player.gathers, gathers, "no own gather state");
        assert_eq!(world.nodes.numbers_near(AT, RADIUS_M).count(), 0, "no node");
    }

    #[test]
    fn an_opened_chest_is_locked_for_its_opener_only() {
        use nw_content::chest_lockouts::{ChestLockouts, ContainerRow};
        const CHEST: &str = "SupplyCrate";
        let (mut world, entity) = admitted();
        let mut content = (*world.content).clone();
        content.gatherables = Gatherables::build([Gatherable {
            loot_container: true,
            ..gatherable(CHEST, true)
        }]);
        content.chest_lockouts = ChestLockouts::build([ContainerRow {
            id: CHEST,
            container: true,
            elite: false,
            max_respawn_s: 0.0,
        }]);
        world.content = Arc::new(content);
        world.gather_open_world(entity, NUMBER, request(1, 0x15, CHEST), &[], NOW);
        let end_ms = world
            .open_gathers
            .first()
            .expect("the chest is opened")
            .end_ms;
        world.gathers_due(end_ms);
        let target = [0x15; RAW_ID_LEN];
        let player = world.world.get::<Player>(entity).expect("player");
        assert!(player.loot.opened(&target), "the opener's tracker holds it");
        assert_eq!(
            world.nodes.start(target, AT, PLAYER),
            Ok(()),
            "the chest is not depleted for others"
        );
        world.nodes.release(&target);
        world.gather_open_world(entity, NUMBER, request(2, 0x15, CHEST), &[], end_ms);
        assert!(world.open_gathers.is_empty(), "locked for its opener");
    }
}
