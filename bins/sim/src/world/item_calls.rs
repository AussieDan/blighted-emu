// SPDX-License-Identifier: AGPL-3.0-or-later
//! The item calls of a player (run-15 items study): the use of a consumable from the bag
//! (2917), drops into a bag of dropped items (1838, 826), storage chests (interaction 26, 2801,
//! 2942, 1461, 2950, 1512, 387), locks (2549) and loadouts (5578, 5579, 5442, 5542, 5427, 5487,
//! 5443, 5430; status 5497).
//!
//! Retail open world: a use takes one of the bag stack and answers with the cooldowns
//! (`using_azoth_potion_2`, 2026-10-02 session: the bag slot change and the cooldown list 35 to
//! 50 ms after the call); a drop leaves a bag of the dropped items at the player that its loot
//! tracker lists as dropped items (type 5), whose items its client asks for (387) and shows in
//! the second container when the player opens it with interaction 26 (`@inv_DroppedItems`),
//! and that its Take All (2939) empties (`item_actions`); a storage chest opens with
//! interaction 26 too, named by the chest's own name.

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::item_rolls::RolledItem;
use nw_content::item_rules::VitalsEffect;
use nw_content::starter::{KitItem, StarterKits};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::gear::record;
use nw_messages::items::SYNC_OWNED;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    AdjustFxScriptsMsg, ConsumableCooldown, InteractionRequest, ItemCallMsg, LoadoutStatusMsg,
    ServerMessage, StoredItems,
};

use super::attack::Vitals;
use super::gathering::{GATHER_ACCEPTED, GATHER_BUSY};
use super::gear::{DroppedStack, ItemRefusal};
use super::loot_bag::{bag_namespace, storage_emptied, storage_member};
use super::stamina::Stamina;
use super::statics::interactor_target;
use super::storage_search::SearchFor;
use super::{Body, Player, SimWorld, gather_answer, send_now};
use crate::objectives::Event;

/// The interaction kind of a storage chest and of a bag of dropped items (`OpenStorage`).
pub(super) const OPEN_STORAGE_KIND: u8 = 26;
/// The name the accept of an opened bag of dropped items carries (retail 3743 at the open).
const DROPPED_ITEMS_NAME: &[u8] = b"@inv_DroppedItems";
/// The name of a refusal (retail sends a name only with an accept).
const NO_NAME: &[u8] = &[];
/// Microseconds per millisecond, the unit of a cooldown's end.
const US_PER_MS: u64 = 1_000;
/// Milliseconds per second.
const MS_PER_SECOND: u64 = 1_000;
/// The quantity of a move that takes the whole stack (retail 1838 and 2801: 0).
const ALL_OF_THE_STACK: u32 = 0;

/// Where a used item comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UsedFrom {
    /// This bag slot (the item call `UseItem`).
    Bag(u32),
    /// This paperdoll slot (a quick slot key in the input).
    Worn(u32),
}

/// A vitals change of a used consumable that repeats; its times are server ms.
#[derive(Debug, Clone, PartialEq)]
struct Running {
    effect: VitalsEffect,
    /// When the next change comes.
    next_ms: u64,
    /// When the effect ends.
    ends_ms: u64,
    /// The time between two changes.
    every_ms: u64,
}

/// The repeating vitals changes of the consumables a player used.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ItemEffects {
    running: Vec<Running>,
}

impl ItemEffects {
    /// Starts the repeats of `effect`, used at `now_ms`.
    fn start(&mut self, effect: &VitalsEffect, now_ms: u64) {
        if let Some(ticks) = effect.ticks {
            self.resume(
                effect,
                now_ms.saturating_add(ticks.first_ms),
                now_ms.saturating_add(ticks.duration_ms),
            );
        }
    }

    /// Runs the repeats of `effect` with the next change at `next_ms` and the last at `ends_ms`
    /// at most; an effect without repeats runs none.
    pub(super) fn resume(&mut self, effect: &VitalsEffect, next_ms: u64, ends_ms: u64) {
        let Some(ticks) = effect.ticks else {
            return;
        };
        self.running
            .retain(|running| running.effect.status_id != effect.status_id);
        self.running.push(Running {
            effect: effect.clone(),
            next_ms,
            ends_ms,
            every_ms: ticks.every_ms.max(1),
        });
    }

    /// The next change and the end (server ms) of the running repeats of the effect named
    /// `status_id`.
    pub(super) fn repeat_of(&self, status_id: &str) -> Option<(u64, u64)> {
        self.running
            .iter()
            .find(|running| running.effect.status_id.eq_ignore_ascii_case(status_id))
            .map(|running| (running.next_ms, running.ends_ms))
    }

    /// The changes due at `now_ms`; the ended effects go. A dead player loses the effects that
    /// a death ends.
    fn due(&mut self, now_ms: u64, dead: bool) -> Vec<VitalsEffect> {
        if dead {
            self.running
                .retain(|running| !running.effect.remove_on_death);
        }
        let mut due = Vec::new();
        for running in &mut self.running {
            while running.next_ms <= now_ms && running.next_ms <= running.ends_ms {
                due.push(running.effect.clone());
                running.next_ms = running.next_ms.saturating_add(running.every_ms);
            }
        }
        self.running
            .retain(|running| running.next_ms <= running.ends_ms);
        due
    }
}

/// Changes `vitals` and `stamina` by `effect` times `scale` (the multiple the user's held
/// effects give it, [`super::status_effects`]), at most to their maximum: the health it gave.
fn apply(effect: &VitalsEffect, scale: f64, vitals: &mut Vitals, stamina: &mut Stamina) -> u32 {
    if vitals.health.is_dead() {
        return 0;
    }
    let max = f64::from(vitals.health.max());
    // The heal is a few times the max health plus a flat amount of a few thousand.
    let amount = ((effect.health + effect.health_share * max) * scale)
        .max(0.0)
        .round() as u32;
    // Stamina values of a few dozen.
    stamina.gain((effect.stamina * scale) as f32);
    vitals.health.heal(amount)
}

/// The master rows of `items` with their quantities and the `rolls` of each (an item `kits`
/// does not know is left out; a missing roll is `None`).
fn rolled_kit_items<'a>(
    kits: &'a StarterKits,
    items: &[(String, u32)],
    rolls: &[Option<RolledItem>],
) -> Vec<(&'a KitItem, u32, Option<RolledItem>)> {
    items
        .iter()
        .zip(rolls.iter().cloned().chain(std::iter::repeat(None)))
        .filter_map(|((item_id, quantity), rolled)| Some((kits.item(item_id)?, *quantity, rolled)))
        .collect()
}

impl SimWorld {
    /// Answers item call `call` of the player of `connection` at `now_ms`.
    pub fn item_call(&mut self, connection: &ConnectionRef, call: ItemCallMsg, now_ms: u64) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        match call {
            ItemCallMsg::UseItem { bag_slot, .. } => {
                self.use_item(entity, number, bag_slot, now_ms)
            }
            ItemCallMsg::DropInventoryItems { entries, .. } => {
                let dropped = self.with_gear(entity, |gear, _| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            gear.drop_from_bag(entry.bag_slot, entry.instance, entry.quantity)
                                .inspect_err(|refusal| tracing::debug!(?refusal, "drop refused"))
                                .ok()
                        })
                        .collect::<Vec<_>>()
                });
                self.drop_items(entity, number, dropped.unwrap_or_default(), now_ms);
            }
            ItemCallMsg::DropPaperdollItem { slot, quantity, .. } => {
                let time_ns = super::unix_ns(self.server_ms);
                let dropped = self.with_gear(entity, |gear, _| {
                    gear.drop_worn(slot, quantity, time_ns)
                        .inspect_err(|refusal| tracing::debug!(?refusal, "drop refused"))
                        .ok()
                });
                self.drop_items(
                    entity,
                    number,
                    dropped.flatten().into_iter().collect(),
                    now_ms,
                );
            }
            ItemCallMsg::GiveItem {
                instance, quantity, ..
            } => {
                self.refused(entity, "deposit", |gear| gear.deposit(instance, quantity));
            }
            ItemCallMsg::TakeItem {
                instance, quantity, ..
            } => {
                self.take_item(entity, number, instance, quantity, now_ms);
            }
            ItemCallMsg::TradeEquipItem {
                slot,
                instance,
                quantity,
                ..
            } => {
                self.refused(entity, "equip from storage", |gear| {
                    gear.equip_from_storage(slot, instance, quantity)
                        .map(|_| ())
                });
            }
            ItemCallMsg::TradeUnequipItem { slot, instance, .. } => {
                self.refused(entity, "unequip to storage", |gear| {
                    gear.unequip_to_storage(slot, instance)
                });
            }
            ItemCallMsg::UpdateStorageSession { storage, .. } => {
                self.with_gear(entity, |gear, _| gear.open_storage(storage, false));
            }
            ItemCallMsg::SetLockState { item, locked, .. } => {
                self.refused(entity, "lock", |gear| {
                    gear.set_locked(item.location, item.slot, item.instance, locked)
                });
            }
            ItemCallMsg::LoadoutPanelOpen { open, .. } => {
                self.with_gear(entity, |gear, _| gear.set_loadout_panel(open));
            }
            ItemCallMsg::AddItemToLoadouts {
                item,
                slot,
                loadouts,
                ..
            } => {
                let statuses = self.with_gear(entity, |gear, _| {
                    gear.add_to_loadouts(item.location, item.slot, item.instance, slot, &loadouts)
                });
                for (loadout, status) in statuses.unwrap_or_default() {
                    self.loadout_status(entity, number, status, loadout);
                }
            }
            ItemCallMsg::SwitchToLoadout { loadout, .. } => {
                let content = Arc::clone(&self.content);
                let status = self.with_gear(entity, |gear, _| {
                    gear.switch_loadout(loadout, |item_id| content.is_item_disabled(item_id))
                });
                if let Some(status) = status {
                    self.loadout_status(entity, number, status, loadout);
                }
            }
            ItemCallMsg::SavePaperdollToLoadout { loadout, .. } => {
                let content = Arc::clone(&self.content);
                self.with_gear(entity, |gear, player| {
                    let attributes = player.attributes.values(&content.progression.points);
                    gear.save_to_loadout(loadout, attributes);
                });
            }
            ItemCallMsg::RemoveAllItemsFromLoadout { loadout, .. } => {
                let status = self.with_gear(entity, |gear, _| gear.clear_loadout(loadout));
                if let Some(status) = status.flatten() {
                    self.loadout_status(entity, number, status, loadout);
                }
            }
            ItemCallMsg::RenameLoadout { loadout, name, .. } => {
                let name = String::from_utf8_lossy(&name).into_owned();
                self.with_gear(entity, |gear, _| gear.rename_loadout(loadout, name));
            }
            ItemCallMsg::RemoveItemFromAllLoadouts { key, .. } => {
                self.with_gear(entity, |gear, _| gear.remove_from_loadouts(key));
            }
            ItemCallMsg::RemoveItemFromLoadout {
                loadout, key, slot, ..
            } => {
                self.with_gear(entity, |gear, _| {
                    gear.remove_from_loadout(loadout, key, slot)
                });
            }
            ItemCallMsg::SlotItemWithCharm {
                target,
                charm,
                perk_index,
                ..
            } => self.slot_charm(entity, number, target, charm, perk_index),
            // Retail answered 7038 with nothing; a 6983 is done at once, so no request is left
            // pending for it to drop.
            ItemCallMsg::ClearCharmRequests { .. } => {}
            ItemCallMsg::DropItemsByClass { class, .. } => {
                let content = Arc::clone(&self.content);
                let dropped = self.with_gear(entity, |gear, _| {
                    gear.bag_stacks()
                        .into_iter()
                        .filter(|(_, item_id, _, locked)| {
                            !locked && content.is_of_ui_class(item_id, class)
                        })
                        .filter_map(|(bag_slot, _, instance, _)| {
                            gear.drop_from_bag(bag_slot, instance, ALL_OF_THE_STACK)
                                .inspect_err(|refusal| tracing::debug!(?refusal, "drop refused"))
                                .ok()
                        })
                        .collect::<Vec<_>>()
                });
                self.drop_items(entity, number, dropped.unwrap_or_default(), now_ms);
            }
            ItemCallMsg::GiveItemsByClass { class, .. } => {
                let content = Arc::clone(&self.content);
                self.with_gear(entity, |gear, _| {
                    for (_, item_id, instance, locked) in gear.bag_stacks() {
                        if locked || !content.is_of_ui_class(&item_id, class) {
                            continue;
                        }
                        if let Err(refusal) = gear.deposit(instance, ALL_OF_THE_STACK) {
                            tracing::debug!(?refusal, "deposit by class refused");
                        }
                    }
                });
            }
            ItemCallMsg::TakeItemsByClass { class, .. } => {
                let content = Arc::clone(&self.content);
                let wanted: Vec<[u8; RAW_ID_LEN]> = self
                    .world
                    .get::<Player>(entity)
                    .and_then(|player| player.gear.as_ref())
                    .map(|gear| {
                        gear.open_stacks()
                            .into_iter()
                            .filter(|(item_id, _)| content.is_of_ui_class(item_id, class))
                            .map(|(_, instance)| instance)
                            .collect()
                    })
                    .unwrap_or_default();
                for instance in wanted {
                    self.take_item(entity, number, instance, ALL_OF_THE_STACK, now_ms);
                }
            }
            ItemCallMsg::SearchStorage {
                search,
                filter,
                station,
                refresh,
                ..
            } => self.search_storage(
                entity,
                number,
                search,
                SearchFor::of(filter, station),
                refresh,
                now_ms,
            ),
            ItemCallMsg::ReplicateSearchResults { .. } => self.replicate_search(entity, number),
        }
    }

    /// Runs `act` on the gear of player `entity` (and the player); `None` without gear.
    fn with_gear<T>(
        &mut self,
        entity: Entity,
        act: impl FnOnce(&mut super::gear::Gear, &Player) -> T,
    ) -> Option<T> {
        let mut player = self.world.get_mut::<Player>(entity)?;
        let player = &mut *player;
        let mut gear = player.gear.take()?;
        let result = act(&mut gear, player);
        player.gear = Some(gear);
        Some(result)
    }

    /// Runs the gear move `act` of player `entity`; a refusal is logged as `what`.
    fn refused(
        &mut self,
        entity: Entity,
        what: &str,
        act: impl FnOnce(&mut super::gear::Gear) -> Result<(), ItemRefusal>,
    ) {
        if let Some(Err(refusal)) = self.with_gear(entity, |gear, _| act(gear)) {
            tracing::debug!(?refusal, what, "item move refused");
        }
    }

    /// Sends loadout status `status` of loadout `loadout` to player `number` (`entity`).
    fn loadout_status(&mut self, entity: Entity, number: u64, status: u32, loadout: u8) {
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let message = ServerMessage::LoadoutStatus(LoadoutStatusMsg::to(number, status, loadout));
        send_now(&mut self.combat_messages, &player.connection, &[message]);
    }

    /// Player `number` (`entity`) uses the item of bag slot `bag_slot` at `now_ms`
    /// ([`SimWorld::consume`]).
    fn use_item(&mut self, entity: Entity, number: u64, bag_slot: u32, now_ms: u64) {
        let Some(item_id) = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref()?.bag_item(bag_slot))
            .map(|(item_id, _)| item_id.to_owned())
        else {
            tracing::debug!(player = number, bag_slot, "use of an empty bag slot");
            return;
        };
        self.consume(entity, number, &item_id, UsedFrom::Bag(bag_slot), now_ms);
    }

    /// Player `number` (`entity`) uses its item `item_id` from `from` at `now_ms`: a
    /// consumable whose cooldown does not run, which its class does not disable and that the
    /// fight allows, takes one of the stack (`ConsumeOnUse`), starts its cooldown, changes the
    /// vitals by its status effects and plays the visual script of each that has one.
    pub(super) fn consume(
        &mut self,
        entity: Entity,
        number: u64,
        item_id: &str,
        from: UsedFrom,
        now_ms: u64,
    ) {
        let content = Arc::clone(&self.content);
        let item_id = item_id.to_owned();
        if content.is_item_disabled(&item_id) {
            tracing::debug!(player = number, item = %item_id, "use: the item's class is disabled");
            return;
        }
        let Some(consumable) = content.item_rules.consumable(&item_id) else {
            tracing::debug!(player = number, item = %item_id, "use of an item that is no consumable");
            return;
        };
        let consumes = content
            .item_rules
            .item(&item_id)
            .is_some_and(|rule| rule.consume_on_use);
        let server_ms = self.server_ms;
        let now_us = server_ms.saturating_mul(US_PER_MS);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        if consumable.disabled_in_combat && player.combat.in_combat() {
            tracing::debug!(player = number, item = %item_id, "use: not in combat");
            return;
        }
        let cooldown = consumable
            .cooldown
            .as_ref()
            .map(|(cooldown_id, seconds)| (cooldown_id.as_str(), *seconds));
        if !start_cooldown(&mut player.cooldowns, &consumable.id, cooldown, now_us) {
            tracing::debug!(player = number, item = %item_id, "use: the cooldown runs");
            return;
        }
        let taken = !consumes
            || player.gear.as_mut().is_some_and(|gear| match from {
                UsedFrom::Bag(bag_slot) => gear.take_from_slot(bag_slot, 1),
                UsedFrom::Worn(slot_id) => gear.use_one(slot_id).is_some(),
            });
        if !taken {
            return;
        }
        player.cooldowns_changed |= consumable.cooldown.is_some();
        for effect in &consumable.effects {
            player.item_effects.start(effect, server_ms);
        }
        let connection = player.connection.clone();
        let instant: Vec<&VitalsEffect> = consumable
            .effects
            .iter()
            .filter(|effect| effect.ticks.is_none())
            .collect();
        self.change_vitals(entity, &instant);
        // Every effect it adds goes in the player's status effects (4236): attributes,
        // absorption, food and potion buffs, immunities ([`super::status_effects`]).
        self.add_consumable_effects(entity, consumable);
        let scripts: Vec<ServerMessage> = consumable
            .effects
            .iter()
            .filter(|effect| effect.has_fx)
            .map(|effect| {
                ServerMessage::AdjustFxScripts(AdjustFxScriptsMsg::on(
                    number,
                    lowercase_crc32(&effect.status_id),
                ))
            })
            .collect();
        send_now(&mut self.combat_messages, &connection, &scripts);
        self.quest_event(number, Event::ItemUsed(&item_id), now_ms);
        tracing::debug!(player = number, item = %item_id, "used");
    }

    /// Changes the vitals of player `entity` by `effects`.
    fn change_vitals(&mut self, entity: Entity, effects: &[&VitalsEffect]) {
        if effects.is_empty() {
            return;
        }
        let rules = &self.content.status_effects;
        let Some((mut stamina, scales)) = self.world.get::<Player>(entity).map(|player| {
            let scales: Vec<f64> = effects
                .iter()
                .map(|effect| {
                    player
                        .status_effects
                        .potency_scale(rules, &effect.status_id)
                })
                .collect();
            (player.stamina, scales)
        }) else {
            return;
        };
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            for (effect, scale) in effects.iter().zip(scales) {
                apply(effect, scale, &mut vitals, &mut stamina);
            }
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.stamina = stamina;
        }
    }

    /// The repeating vitals changes of used consumables due at server time `now_ms` (their
    /// repeats run on the server clock, as the status effects they come with), for every player,
    /// each times the multiple the player's held effects give it.
    pub(super) fn tick_item_effects(&mut self, now_ms: u64) {
        let rules = &self.content.status_effects;
        let mut query = self.world.query::<(&mut Player, &mut Vitals)>();
        for (mut player, mut vitals) in query.iter_mut(&mut self.world) {
            let player = &mut *player;
            for effect in player.item_effects.due(now_ms, vitals.health.is_dead()) {
                let scale = player
                    .status_effects
                    .potency_scale(rules, &effect.status_id);
                apply(&effect, scale, &mut vitals, &mut player.stamina);
            }
        }
    }

    /// Player `number` (`entity`) dropped `dropped` at `now_ms`: a bag of them, with their
    /// rolls, lies at its feet, and its loot tracker lists the bag as dropped items. Nothing
    /// dropped drops no bag.
    fn drop_items(&mut self, entity: Entity, number: u64, dropped: Vec<DroppedStack>, now_ms: u64) {
        if dropped.is_empty() {
            return;
        }
        let Some(at) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return;
        };
        let namespace = bag_namespace(self.me.generation);
        let (items, rolls) = dropped
            .into_iter()
            .map(|stack| ((stack.item_id, stack.quantity), stack.rolled))
            .unzip();
        let bag = self
            .bags
            .drop_rolled_bag(namespace, at, number, items, rolls, now_ms);
        let server_ms = self.server_ms;
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.list_version += 1;
            let version = player.list_version;
            let tracked = player.loot.track_dropped(bag, at, server_ms, version);
            player.extra_members.push(tracked);
        }
    }

    /// Player `number` (`entity`) uses a storage chest or opens a bag of its dropped items with
    /// interaction `request`, named `name` by its client: the accept (3743), and its items in
    /// the second container. A bag of another player is refused (status 2, as a busy gather).
    pub(super) fn open_storage(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) {
        let content = Arc::clone(&self.content);
        let bag = self.bags.takeable(&request.target, number);
        let is_bag = self.bags.holds_bag(&request.target);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        let Some(gear) = player.gear.as_mut() else {
            return;
        };
        let answer = match bag {
            Some(bag) => {
                let items = rolled_kit_items(&content.kits, &bag.items, &bag.rolls);
                gear.open_dropped(&content.kits, bag.id, &items);
                gather_answer(number, request, GATHER_ACCEPTED, DROPPED_ITEMS_NAME)
            }
            None if is_bag => gather_answer(number, request, GATHER_BUSY, NO_NAME),
            None => {
                gear.open_storage(request.target, true);
                gather_answer(number, request, GATHER_ACCEPTED, name)
            }
        };
        send_now(&mut self.combat_messages, &player.connection, &[answer]);
    }

    /// The player stopped using what its second container shows (2473).
    pub(super) fn close_container(&mut self, entity: Entity) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
        {
            gear.close_container();
        }
    }

    /// Player `number` (`entity`) takes every item of the bag of dropped items it has open
    /// (2939) at `now_ms`: the items into its bag, the tracker's entry looted, its global
    /// storage emptied, the view emptied. Whether it had one open.
    pub(super) fn take_all_open(&mut self, entity: Entity, number: u64, now_ms: u64) -> bool {
        let Some(bag) = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref()?.open_bag())
        else {
            return false;
        };
        let taken = self
            .bags
            .take_rolled(&bag, number, now_ms)
            .unwrap_or_default();
        let (items, rolls): (Vec<(String, u32)>, Vec<Option<RolledItem>>) =
            taken.into_iter().unzip();
        self.take_from_bag(entity, number, bag, &items, &rolls);
        true
    }

    /// Player `number` (`entity`) takes `quantity` of the stack of instance `instance` of what
    /// its second container shows (2942) at `now_ms`: from a storage chest into the bag, or from
    /// a bag of its dropped items.
    fn take_item(
        &mut self,
        entity: Entity,
        number: u64,
        instance: [u8; RAW_ID_LEN],
        quantity: u32,
        now_ms: u64,
    ) {
        let open = self.world.get::<Player>(entity).and_then(|player| {
            let gear = player.gear.as_ref()?;
            Some((gear.open_bag(), gear.dropped_index(instance)))
        });
        match open {
            Some((Some(bag), Some(index))) => {
                let (items, rolls): (Vec<(String, u32)>, Vec<Option<RolledItem>>) = self
                    .bags
                    .take_part(&bag, number, index, quantity, now_ms)
                    .into_iter()
                    .unzip();
                self.take_from_bag(entity, number, bag, &items, &rolls);
            }
            _ => self.refused(entity, "withdraw", |gear| gear.withdraw(instance, quantity)),
        }
    }

    /// `items` of bag `bag` of dropped items, with the `rolls` of each, go into the bag of
    /// player `number` (`entity`); the view shows what is left, and a bag left empty leaves the
    /// tracker as looted, with the global storage emptied (retail at the take).
    fn take_from_bag(
        &mut self,
        entity: Entity,
        number: u64,
        bag: [u8; RAW_ID_LEN],
        items: &[(String, u32)],
        rolls: &[Option<RolledItem>],
    ) {
        let content = Arc::clone(&self.content);
        let emptied = !self.bags.lying(&bag);
        let left = self.bags.takeable(&bag, number);
        let server_ms = self.server_ms;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        let kit_items = rolled_kit_items(&content.kits, items, rolls);
        if let Some(gear) = player.gear.as_mut() {
            if !gear.take_dropped(&content.kits, &kit_items) {
                tracing::info!(?items, "dropped items lost: a full bag");
            }
            match left {
                Some(left) => {
                    let shown = rolled_kit_items(&content.kits, &left.items, &left.rolls);
                    gear.open_dropped(&content.kits, bag, &shown);
                }
                None => {
                    gear.close_container();
                }
            }
        }
        if emptied {
            player.list_version += 1;
            let version = player.list_version;
            let taken = player.loot.taken(&bag, server_ms, version);
            player.extra_members.extend([
                interactor_target(nw_messages::facet_of_raw_id(&bag)),
                taken,
                storage_emptied(version),
            ]);
        }
    }

    /// The client of player `entity` asks for the items of storage `storage` (387): a storage
    /// chest of the player lists them in its global storage (retail: every item, sync 17, in no
    /// container). Whether `storage` is one of its storage chests.
    pub(super) fn show_stored_items(&mut self, entity: Entity, storage: &[u8; RAW_ID_LEN]) -> bool {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return false;
        };
        let Some(items) = player
            .gear
            .as_ref()
            .and_then(|gear| gear.stored_items(storage))
        else {
            return false;
        };
        player.list_version += 1;
        let version = player.list_version;
        let records = items
            .iter()
            .map(|item| record(item, SYNC_OWNED, None))
            .collect();
        player.extra_members.push(storage_member(
            vec![StoredItems {
                storage: *storage,
                items: records,
            }],
            version,
        ));
        true
    }
}

/// Starts the cooldown `cooldown` (`CooldownId`, seconds) of consumable `consumable_id` in
/// `cooldowns` at `now_us` (µs since 2000-01-01), the ended ones gone; `false`, changing
/// nothing, while one of its cooldown id runs (xp-potion-exit note, section 2.3: the entry names
/// the consumable, the end and the cooldown id). A consumable without a cooldown always starts.
pub(super) fn start_cooldown(
    cooldowns: &mut Vec<ConsumableCooldown>,
    consumable_id: &str,
    cooldown: Option<(&str, u32)>,
    now_us: u64,
) -> bool {
    let Some((cooldown_id, seconds)) = cooldown else {
        return true;
    };
    let group = lowercase_crc32(cooldown_id);
    cooldowns.retain(|cooldown| cooldown.end_us > now_us);
    if cooldowns.iter().any(|cooldown| cooldown.group == group) {
        return false;
    }
    let cooldown_us = u64::from(seconds)
        .saturating_mul(MS_PER_SECOND)
        .saturating_mul(US_PER_MS);
    cooldowns.push(ConsumableCooldown {
        consumable: lowercase_crc32(consumable_id),
        end_us: now_us.saturating_add(cooldown_us),
        group,
    });
    true
}

#[cfg(test)]
mod tests {
    use nw_content::item_rules::Ticks;

    use super::*;

    const NOW_US: u64 = 1_000_000;
    const COOLDOWN_S: u32 = 25;

    const POTION: &str = "PotionHealthT1";
    const COOLDOWN: Option<(&str, u32)> = Some(("SurvivalPotion", COOLDOWN_S));

    fn regen(remove_on_death: bool) -> VitalsEffect {
        VitalsEffect {
            status_id: "SerumRegen".to_owned(),
            health: 400.0,
            health_share: 0.011,
            stamina: 3.0,
            ticks: Some(Ticks {
                first_ms: 0,
                every_ms: 1_000,
                duration_ms: 3_000,
            }),
            remove_on_death,
            has_fx: true,
        }
    }

    #[test]
    fn a_cooldown_starts_once_and_ends_at_its_duration() {
        let mut cooldowns = Vec::new();
        assert!(start_cooldown(&mut cooldowns, POTION, COOLDOWN, NOW_US));
        assert_eq!(
            cooldowns,
            [ConsumableCooldown {
                consumable: lowercase_crc32("PotionHealthT1"),
                end_us: NOW_US + u64::from(COOLDOWN_S) * 1_000_000,
                group: lowercase_crc32("SurvivalPotion"),
            }]
        );
        assert!(
            !start_cooldown(&mut cooldowns, POTION, COOLDOWN, NOW_US + 1),
            "it runs"
        );
        let ended = NOW_US + u64::from(COOLDOWN_S) * 1_000_000 + 1;
        assert!(start_cooldown(&mut cooldowns, POTION, COOLDOWN, ended));
        assert_eq!(cooldowns.len(), 1, "the ended one went");
        assert!(
            start_cooldown(&mut cooldowns, POTION, None, ended),
            "no cooldown"
        );
    }

    #[test]
    fn a_repeating_effect_changes_the_vitals_each_tick_for_its_duration() {
        const START: u64 = 10_000;
        let mut effects = ItemEffects::default();
        effects.start(&regen(true), START);
        assert_eq!(effects.due(START, false).len(), 1, "the first tick at once");
        assert!(effects.due(START + 999, false).is_empty());
        assert_eq!(
            effects.due(START + 2_000, false).len(),
            2,
            "two ticks caught up"
        );
        assert_eq!(
            effects.due(START + 3_000, false).len(),
            1,
            "the last one at the end"
        );
        assert!(effects.running.is_empty(), "it ended");
        effects.start(&regen(true), START);
        assert!(effects.due(START, true).is_empty(), "a death ends it");
        effects.start(&regen(false), START);
        assert_eq!(
            effects.due(START, true).len(),
            1,
            "food recovery outlives a death"
        );
    }

    mod in_world {
        use std::sync::Arc;

        use nw_content::crafting::{Ingredient, IngredientKind, Recipe, Recipes};
        use nw_content::item_rolls::{ItemRollRow, ItemRolls};
        use nw_content::item_rules::{
            ConsumableRow, EffectRow, ItemRuleRow, ItemRuleSource, ItemRules,
        };
        use nw_content::perks::PerkTable;
        use nw_formats::objectstream::lowercase_crc32;
        use nw_messages::wire::RAW_ID_LEN;
        use nw_messages::{
            FacetHeader, InteractionRequest, ItemCallMsg, ServerMessage, StorageItemSimple,
            StorageSearchItemsMsg, StorageSearchResultsMsg,
        };

        use crate::player_content::PlayerContent;
        use crate::world::attack::Vitals;
        use crate::world::gear::Gear;
        use crate::world::gear::tests::{BACKSTORY, CHARACTER, FLINT, POTION, kits};
        use crate::world::item_calls::UsedFrom;
        use crate::world::tests::{
            NOW, SERVER_TIME, connection, join, messages_to, tick, world_with_spawns,
        };
        use crate::world::{Player, SimWorld, item_calls::OPEN_STORAGE_KIND};
        use nw_messages::item_calls::LOCATION_BAG;

        const HEAL: f64 = 300.0;
        const HURT: u32 = 500;
        const COOLDOWN_S: u32 = 25;
        const POTIONS: u32 = 3;
        const FLINTS: u32 = 4;
        const INTERACTION: u64 = 77;
        /// A storage chest's entity id (retail form: 8 bytes, then zeros).
        const STORAGE: [u8; RAW_ID_LEN] = [
            0x6b, 0xbf, 0x90, 0xec, 0x53, 0xd9, 0xf3, 0x5f, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        const STORAGE_NAME: &[u8] = b"@StorageT2_Outpost";

        fn rules() -> ItemRules {
            ItemRules::build(ItemRuleSource {
                items: vec![ItemRuleRow {
                    item_id: POTION.to_owned(),
                    stats_ref: None,
                    consume_on_use: true,
                    not_droppable: false,
                    nonremovable: false,
                }],
                consumables: vec![ConsumableRow {
                    id: POTION.to_owned(),
                    add_status_effects: Some("PotionHeal".to_owned()),
                    potency: Some(1.0),
                    duration_override_s: Some(0.0),
                    cooldown_id: Some("SurvivalPotion".to_owned()),
                    cooldown_s: Some(COOLDOWN_S),
                    disabled_in_combat: false,
                    use_tag: None,
                }],
                effects: vec![EffectRow {
                    status_id: "PotionHeal".to_owned(),
                    base_duration_s: Some(-1.0),
                    tick_s: Some(0.0),
                    delay_s: None,
                    health: Some(HEAL),
                    health_share: Some(0.0),
                    stamina: None,
                    remove_on_death: true,
                    has_fx: true,
                }],
            })
        }

        /// A world whose player 1 wears the test kit and carries potions and flint.
        fn world() -> SimWorld {
            world_of(PlayerContent::default())
        }

        /// [`world`] with `content` besides the test kit and the item rules.
        fn world_of(content: PlayerContent) -> SimWorld {
            let mut world = world_with_spawns(Vec::new());
            world.content = Arc::new(PlayerContent {
                kits: kits(),
                item_rules: rules(),
                ..content
            });
            join(&mut world, 1, 0.0);
            let kits = kits();
            let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
            assert!(gear.add(kits.item(POTION).expect("potion"), POTIONS));
            assert!(gear.add(kits.item(FLINT).expect("flint"), FLINTS));
            let entity = world.by_connection[&connection(1)];
            world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
            world
        }

        fn gear_of(world: &SimWorld) -> &Gear {
            let entity = world.by_connection[&connection(1)];
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .expect("gear")
        }

        fn header() -> FacetHeader {
            FacetHeader {
                target: 0,
                entity: 0,
            }
        }

        /// Retail `general-other-01`: a `FoodRecoveryT2` heal was 187.03 at 502.111 s and 561.10
        /// at 1212.654 s, while `Status_OutOfCombat_PassiveRegen` (`PassiveFoodRegen=2`) ran.
        #[test]
        fn a_food_heal_is_three_times_as_large_while_the_out_of_combat_regen_runs() {
            use nw_content::item_rules::{Ticks, VitalsEffect};
            use nw_content::status_effects::{
                EffectSourceRow, StatusEffectRules, StatusEffectSource,
            };

            const FOOD: &str = "FoodRecoveryT2";
            const REGEN: &str = "Status_OutOfCombat_PassiveRegen";
            const FOOD_SHARE: f64 = 0.011;
            const FOOD_EVERY_MS: u64 = 2_000;
            const FOOD_DURATION_MS: u64 = 1_800_000;
            /// The food's potency while the regen runs: 1 + `PassiveFoodRegen=2`.
            const WITH_REGEN: f64 = 3.0;
            const REGEN_POTENCY: f64 = 1.0;
            let status_effects = StatusEffectRules::build(StatusEffectSource {
                effects: vec![
                    EffectSourceRow {
                        status_id: FOOD.to_owned(),
                        categories: Some("Recovery+PassiveFoodRegen+FoodUtility+Food".to_owned()),
                        ..EffectSourceRow::default()
                    },
                    EffectSourceRow {
                        status_id: REGEN.to_owned(),
                        potency_mods: Some("PassiveFoodRegen=2".to_owned()),
                        ..EffectSourceRow::default()
                    },
                ],
                ..StatusEffectSource::default()
            });
            let mut world = world_of(PlayerContent {
                status_effects,
                ..PlayerContent::default()
            });
            let entity = world.by_connection[&connection(1)];
            let food = VitalsEffect {
                status_id: FOOD.to_owned(),
                health: 0.0,
                health_share: FOOD_SHARE,
                stamina: 0.0,
                ticks: Some(Ticks {
                    first_ms: 0,
                    every_ms: FOOD_EVERY_MS,
                    duration_ms: FOOD_DURATION_MS,
                }),
                remove_on_death: false,
                has_fx: false,
            };
            let max = {
                let mut vitals = world.world.get_mut::<Vitals>(entity).expect("vitals");
                vitals.health.take(HURT);
                vitals.health.max()
            };
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .item_effects
                .start(&food, NOW);
            let health = |world: &SimWorld| {
                world
                    .world
                    .get::<Vitals>(entity)
                    .expect("vitals")
                    .health
                    .current()
            };
            let before = health(&world);
            world.tick_item_effects(NOW);
            let alone = health(&world) - before;
            let rules = Arc::clone(&world.content);
            assert!(
                world
                    .world
                    .get_mut::<Player>(entity)
                    .expect("player")
                    .status_effects
                    .add_by_id(&rules.status_effects, REGEN, REGEN_POTENCY, 1, NOW)
            );
            let before = health(&world);
            world.tick_item_effects(NOW + FOOD_EVERY_MS);
            let with_regen = health(&world) - before;
            let share = FOOD_SHARE * f64::from(max);
            assert_eq!(
                (f64::from(alone), f64::from(with_regen)),
                (share.round(), (share * WITH_REGEN).round())
            );
        }

        /// Retail `target-lock-test`: a `FoodRecoveryT2` was held with 303.5 s left 1.5 s after
        /// the login. A used food's effect and its heals go with the progression and come back
        /// at the next spawn with the time they had left.
        #[test]
        fn a_food_effect_and_its_heals_come_back_at_the_next_spawn() {
            use nw_content::status_effects::{
                EffectSourceRow, StatusEffectRules, StatusEffectSource,
            };

            use crate::world::gear::unix_ns;
            use crate::world::tests::admit_at;

            const FOOD: &str = "FoodRecoveryT2";
            const DURATION_S: f64 = 1_800.0;
            const EVERY_S: f64 = 2.0;
            const FOOD_SHARE: f64 = 0.011;
            const USED_MS: u64 = 846_118_000_000;
            const DURATION_MS: u64 = 1_800_000;
            /// The time it ran before the save: retail had 303.5 s left.
            const RAN_MS: u64 = DURATION_MS - 303_500;
            let content = PlayerContent {
                item_rules: ItemRules::build(ItemRuleSource {
                    items: Vec::new(),
                    consumables: vec![ConsumableRow {
                        id: FOOD.to_owned(),
                        add_status_effects: Some(FOOD.to_owned()),
                        potency: Some(1.0),
                        duration_override_s: Some(0.0),
                        cooldown_id: None,
                        cooldown_s: None,
                        disabled_in_combat: false,
                        use_tag: Some("Eat".to_owned()),
                    }],
                    effects: vec![EffectRow {
                        status_id: FOOD.to_owned(),
                        base_duration_s: Some(DURATION_S),
                        tick_s: Some(EVERY_S),
                        delay_s: Some(EVERY_S),
                        health: None,
                        health_share: Some(FOOD_SHARE),
                        stamina: None,
                        remove_on_death: false,
                        has_fx: false,
                    }],
                }),
                status_effects: StatusEffectRules::build(StatusEffectSource {
                    effects: vec![EffectSourceRow {
                        status_id: FOOD.to_owned(),
                        base_duration_s: Some(DURATION_S),
                        categories: Some("Recovery+Food".to_owned()),
                        ..EffectSourceRow::default()
                    }],
                    ..StatusEffectSource::default()
                }),
                ..PlayerContent::default()
            };
            let content = Arc::new(content);
            let crc = lowercase_crc32(FOOD);
            let mut before = world_with_spawns(Vec::new());
            before.content = Arc::clone(&content);
            join(&mut before, 1, 0.0);
            let entity = before.by_connection[&connection(1)];
            before.server_ms = USED_MS;
            before.consume(entity, 1, FOOD, UsedFrom::Bag(0), NOW);
            before.server_ms = USED_MS + RAN_MS;
            let kept = before
                .world
                .get::<Player>(entity)
                .expect("player")
                .progression()
                .status_effects;
            assert_eq!(
                kept,
                vec![nw_bus::PlayerStatusEffect {
                    consumable_id: FOOD.to_owned(),
                    status_id: FOOD.to_owned(),
                    ends_ns: unix_ns(USED_MS + DURATION_MS),
                    stacks: 1,
                    runs_offline: false,
                    repeat: Some(nw_bus::EffectRepeat {
                        next_ns: unix_ns(USED_MS + 2_000),
                        ends_ns: unix_ns(USED_MS + DURATION_MS),
                    }),
                }],
                "an unended food and its next heal"
            );

            let mut after = world_with_spawns(Vec::new());
            after.content = Arc::clone(&content);
            after.server_ms = USED_MS + RAN_MS;
            let mut admit = admit_at(1, 0.0, true);
            admit.progression.status_effects = kept.clone();
            after.admit(&admit, NOW, SERVER_TIME).expect("admit");
            let entity = after.by_connection[&connection(1)];
            let player = after.world.get::<Player>(entity).expect("player");
            assert_eq!(
                player.status_effects.ends_ms(crc),
                Some(USED_MS + DURATION_MS),
                "held again with its end"
            );
            assert_eq!(
                player.item_effects.repeat_of(FOOD),
                Some((USED_MS + RAN_MS, USED_MS + DURATION_MS)),
                "its heals go on from the spawn"
            );
            assert_eq!(player.progression().status_effects.len(), 1, "kept again");

            let mut ended = world_with_spawns(Vec::new());
            ended.content = Arc::clone(&content);
            ended.server_ms = USED_MS + DURATION_MS;
            admit.progression.status_effects = kept;
            ended.admit(&admit, NOW, SERVER_TIME).expect("admit");
            let entity = ended.by_connection[&connection(1)];
            let player = ended.world.get::<Player>(entity).expect("player");
            assert!(
                !player.status_effects.holds(crc),
                "an ended food stays gone"
            );
            assert_eq!(player.item_effects.repeat_of(FOOD), None);
        }

        #[test]
        fn a_potion_from_the_bag_heals_once_per_cooldown() {
            let mut world = world();
            let entity = world.by_connection[&connection(1)];
            world
                .world
                .get_mut::<Vitals>(entity)
                .expect("vitals")
                .health
                .take(HURT);
            let hurt = world
                .world
                .get::<Vitals>(entity)
                .expect("vitals")
                .health
                .current();
            let call = || ItemCallMsg::UseItem {
                header: header(),
                bag_slot: 0,
            };
            world.item_call(&connection(1), call(), NOW);
            assert_eq!(gear_of(&world).bag_count(POTION), POTIONS - 1);
            let healed = world
                .world
                .get::<Vitals>(entity)
                .expect("vitals")
                .health
                .current();
            assert_eq!(f64::from(healed), f64::from(hurt) + HEAL);
            let messages = tick(&mut world, NOW).messages;
            assert!(
                messages_to(&messages, 1)
                    .iter()
                    .any(|message| matches!(message, ServerMessage::AdjustFxScripts(_))),
                "the heal's visual script"
            );
            world.item_call(&connection(1), call(), NOW + 1);
            assert_eq!(
                gear_of(&world).bag_count(POTION),
                POTIONS - 1,
                "the cooldown runs"
            );
        }

        #[test]
        fn a_dropped_stack_lies_in_a_bag_its_owner_opens_and_takes_back() {
            let mut world = world();
            let flint = gear_of(&world)
                .bag_item(1)
                .map(|(_, item)| item.instance)
                .expect("flint");
            world.item_call(
                &connection(1),
                ItemCallMsg::DropInventoryItems {
                    header: header(),
                    entries: vec![nw_messages::DropEntry {
                        bag_slot: 1,
                        instance: flint,
                        extra: u32::MAX,
                        quantity: 0,
                    }],
                },
                NOW,
            );
            assert_eq!(gear_of(&world).bag_count(FLINT), 0);
            assert_eq!(world.bags.counts(), (1, 1), "one bag, the player's");
            let bag = world
                .world
                .get::<Player>(world.by_connection[&connection(1)])
                .and_then(|player| player.loot.dropped_bag())
                .expect("tracked");
            world.interact(
                &connection(1),
                InteractionRequest {
                    target: bag,
                    target_entity: nw_messages::facet_of_raw_id(&bag),
                    interaction: INTERACTION,
                    kind: OPEN_STORAGE_KIND,
                    gatherable: None,
                },
                b"",
                NOW,
            );
            let answers: Vec<Vec<u8>> = messages_to(&tick(&mut world, NOW).messages, 1)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::InteractionValidated(answer) => Some(answer.name),
                    _ => None,
                })
                .collect();
            assert_eq!(answers, [b"@inv_DroppedItems".to_vec()]);
            assert_eq!(gear_of(&world).open_bag(), Some(bag));
            let entity = world.by_connection[&connection(1)];
            assert!(world.take_all_open(entity, 1, NOW));
            assert_eq!(gear_of(&world).bag_count(FLINT), FLINTS);
            assert_eq!(gear_of(&world).open_bag(), None, "closed once emptied");
        }

        /// UI class number of `UI_Refining`, the class of the test flint.
        const UI_REFINING: u64 = 90;

        /// [`world`] whose flint is of `UI_Refining` and an ingredient of an `engineering2`
        /// recipe.
        fn world_with_flint_rules() -> SimWorld {
            let rolls = ItemRolls::build(
                [ItemRollRow {
                    item_id: FLINT.to_owned(),
                    ui_item_class: Some("UI_Refining".to_owned()),
                    ..ItemRollRow::default()
                }],
                PerkTable::default(),
            );
            let recipes = Recipes::build(
                [Recipe {
                    id: "1hSkinningKnifeT1".to_owned(),
                    item: "1hSkinningKnifeT1".to_owned(),
                    quantity: 1,
                    ingredients: vec![Ingredient {
                        kind: IngredientKind::Item(FLINT.to_owned()),
                        quantity: 1,
                    }],
                    stations: vec!["engineering2".to_owned()],
                }],
                [],
            );
            world_of(PlayerContent {
                item_rolls: rolls,
                recipes,
                ..PlayerContent::default()
            })
        }

        /// Player 1 opens the storage chest [`STORAGE`].
        fn open_storage(world: &mut SimWorld) {
            world.interact(
                &connection(1),
                InteractionRequest {
                    target: STORAGE,
                    target_entity: nw_messages::facet_of_raw_id(&STORAGE),
                    interaction: INTERACTION,
                    kind: OPEN_STORAGE_KIND,
                    gatherable: None,
                },
                STORAGE_NAME,
                NOW,
            );
        }

        fn own_container() -> nw_messages::ContainerRef {
            nw_messages::ContainerRef {
                id: [0; RAW_ID_LEN],
                entity: 0,
                extra: 0,
            }
        }

        #[test]
        fn a_class_goes_into_the_storage_and_comes_back() {
            let mut world = world_with_flint_rules();
            open_storage(&mut world);
            world.item_call(
                &connection(1),
                ItemCallMsg::GiveItemsByClass {
                    header: header(),
                    container: own_container(),
                    class: UI_REFINING,
                },
                NOW,
            );
            assert_eq!(gear_of(&world).bag_count(FLINT), 0);
            assert_eq!(
                gear_of(&world).bag_count(POTION),
                POTIONS,
                "another class stays"
            );
            assert_eq!(gear_of(&world).stored_totals().get(FLINT), Some(&FLINTS));
            world.item_call(
                &connection(1),
                ItemCallMsg::TakeItemsByClass {
                    header: header(),
                    container: own_container(),
                    class: UI_REFINING,
                },
                NOW,
            );
            assert_eq!(gear_of(&world).bag_count(FLINT), FLINTS);
            assert!(gear_of(&world).stored_totals().is_empty());
        }

        #[test]
        fn a_class_drops_into_one_bag() {
            let mut world = world_with_flint_rules();
            world.item_call(
                &connection(1),
                ItemCallMsg::DropItemsByClass {
                    header: header(),
                    class: UI_REFINING,
                    flag: true,
                },
                NOW,
            );
            assert_eq!(gear_of(&world).bag_count(FLINT), 0);
            assert_eq!(gear_of(&world).bag_count(POTION), POTIONS);
            assert_eq!(world.bags.counts(), (1, 1), "one bag, the player's");
        }

        #[test]
        fn a_search_lists_the_stored_ingredients_and_their_changes() {
            const SEARCH: [u8; RAW_ID_LEN] = [9; RAW_ID_LEN];
            let mut world = world_with_flint_rules();
            open_storage(&mut world);
            let flint = gear_of(&world)
                .bag_item(1)
                .map(|(_, item)| item.instance)
                .expect("flint");
            world.item_call(
                &connection(1),
                ItemCallMsg::GiveItem {
                    header: header(),
                    container: own_container(),
                    instance: flint,
                    quantity: 0,
                },
                NOW,
            );
            let search = |refresh| ItemCallMsg::SearchStorage {
                header: header(),
                search: SEARCH,
                lead: 0,
                filter: 0,
                extra: 0,
                station: lowercase_crc32("engineering"),
                tail: 0,
                refresh,
            };
            world.item_call(&connection(1), search(false), NOW);
            world.item_call(&connection(1), search(true), NOW);
            world.item_call(
                &connection(1),
                ItemCallMsg::ReplicateSearchResults { header: header() },
                NOW,
            );
            world.item_call(
                &connection(1),
                ItemCallMsg::TakeItem {
                    header: header(),
                    container: own_container(),
                    instance: flint,
                    quantity: 1,
                },
                NOW,
            );
            let messages = messages_to(&tick(&mut world, NOW).messages, 1);
            let results: Vec<&StorageSearchResultsMsg> = messages
                .iter()
                .filter_map(|message| match message {
                    ServerMessage::StorageSearchResults(results) => Some(results),
                    _ => None,
                })
                .collect();
            assert_eq!(
                results.len(),
                2,
                "the search and the replicate; the refresh waits"
            );
            assert_eq!(
                results[0].items,
                [StorageItemSimple {
                    item: lowercase_crc32(FLINT),
                    quantity: FLINTS,
                }]
            );
            let changes: Vec<&StorageSearchItemsMsg> = messages
                .iter()
                .filter_map(|message| match message {
                    ServerMessage::StorageSearchItems(items) => Some(items),
                    _ => None,
                })
                .collect();
            assert!(
                changes.iter().all(|items| items.search == SEARCH),
                "every item change carries the last search"
            );
            let changed: Vec<_> = changes
                .iter()
                .filter(|items| !items.items.is_empty())
                .collect();
            assert_eq!(changed.len(), 1);
            assert_eq!(changed[0].total, 1);
            assert_eq!(changed[0].items[0].item.quantity, FLINTS - 1);
        }

        #[test]
        fn a_storage_chest_keeps_what_goes_in_and_locks_hold() {
            let mut world = world();
            let flint = gear_of(&world)
                .bag_item(1)
                .map(|(_, item)| item.instance)
                .expect("flint");
            world.item_call(
                &connection(1),
                ItemCallMsg::SetLockState {
                    header: header(),
                    item: nw_messages::ItemRef {
                        location: LOCATION_BAG,
                        slot: 1,
                        instance: flint,
                        extra: 0,
                    },
                    locked: true,
                },
                NOW,
            );
            world.interact(
                &connection(1),
                InteractionRequest {
                    target: STORAGE,
                    target_entity: nw_messages::facet_of_raw_id(&STORAGE),
                    interaction: INTERACTION,
                    kind: OPEN_STORAGE_KIND,
                    gatherable: None,
                },
                STORAGE_NAME,
                NOW,
            );
            assert_eq!(gear_of(&world).open_storage_id(), Some(STORAGE));
            let give = |quantity| ItemCallMsg::GiveItem {
                header: header(),
                container: nw_messages::ContainerRef {
                    id: [0; RAW_ID_LEN],
                    entity: 0,
                    extra: 0,
                },
                instance: flint,
                quantity,
            };
            world.item_call(&connection(1), give(0), NOW);
            assert_eq!(
                gear_of(&world).bag_count(FLINT),
                FLINTS,
                "a locked stack stays"
            );
            world.item_call(
                &connection(1),
                ItemCallMsg::SetLockState {
                    header: header(),
                    item: nw_messages::ItemRef {
                        location: LOCATION_BAG,
                        slot: 1,
                        instance: flint,
                        extra: 0,
                    },
                    locked: false,
                },
                NOW,
            );
            world.item_call(&connection(1), give(0), NOW);
            assert_eq!(gear_of(&world).bag_count(FLINT), 0);
            let saved = gear_of(&world).inventory();
            assert_eq!(saved.stored.len(), 1);
            assert_eq!(
                (saved.stored[0].storage, saved.stored[0].quantity),
                (STORAGE, FLINTS)
            );
            let entity = world.by_connection[&connection(1)];
            assert!(world.show_stored_items(entity, &STORAGE));
            assert!(
                !world.show_stored_items(entity, &[1; RAW_ID_LEN]),
                "no such storage"
            );
        }
    }
}
