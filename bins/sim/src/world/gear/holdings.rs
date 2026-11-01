// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's items beyond wearing them and carrying them in the bag (run-15 items study): the
//! instance id and counter of each new stack, the owned-items keys, the storage chests, the
//! container shown in the second container, the saved loadouts, and the moves between them.
//!
//! Retail open world, `storage_open_deposit_withdraw_transfer` and `item_actions`: a storage
//! chest the player uses and a bag of dropped items it opens show in its second container
//! (component 76); a deposit (2801) empties the bag slot and fills the first free storage slot,
//! a take (2942) the other way, each with a new counter and, for an item without durability, a
//! new instance id ([`Gear::stored_move`]); the close leaves one empty slot and no transfer. Retail newchar 2026-09-18: a worn item keeps its owned-items key, a new one takes the
//! lowest free key, and a loadout names its items by those keys.

use std::collections::{BTreeMap, BTreeSet};

use nw_bus::{ItemLookState, ItemState, PlayerInventory, SavedLoadout, StoredItem};
use nw_content::item_rolls::RolledItem;
use nw_content::starter::{KitItem, StarterKits};
use nw_messages::item_calls::{LOCATION_BAG, LOCATIONS_BY_KEY, loadout_status};
use nw_messages::items::{SYNC_LOGIN, SYNC_MOVED};
use nw_messages::paperdoll::Loadout;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{GearExtras, GearItem, ItemLook, OpenContainer};
use uuid::Uuid;

use super::{Gear, MAX_STACK_QUANTITY, Stack, fits, instance_id, stack};

/// Most slots of a container: the bag and a storage list a fixed array of 500 (paperdoll-
/// inventory note, section 7; retail bags of 190 to 406 slots grew one slot per new stack).
pub(in crate::world) const MAX_CONTAINER_SLOTS: usize = 500;
/// The sync reason of the items of a storage the player stands at (retail 6 at the open).
const SYNC_STORAGE_NEAR: u8 = 6;
/// The sync reason of the items of a storage seen from afar (retail 47 after 1512).
const SYNC_STORAGE_AFAR: u8 = 47;
/// The sync reason of the items of an opened bag of dropped items (retail 18).
const SYNC_DROPPED_BAG: u8 = 18;
/// The slots of a closed second container: one empty slot (retail at each close).
const CLOSED_SLOTS: usize = 1;
/// Separates an item id from the serial of its instance-id seed.
const SERIAL_SEPARATOR: char = '#';
/// The name a new loadout takes before the player renames it: this and its index (retail
/// `@lo_name 5`).
const LOADOUT_NAME_PREFIX: &str = "@lo_name ";
/// Why an item call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::world) enum ItemRefusal {
    /// The named stack is not there.
    NoSuchItem,
    /// Its owner locked it.
    Locked,
    /// No container is open, or the open one takes no items.
    NoContainer,
    /// The container has no free slot.
    Full,
    /// The item does not go into that paperdoll slot.
    WrongSlot,
}

/// A stack the player dropped (1838, 826, 3938): its item, quantity and, for gear, its own
/// gear score and perks, which the bag of dropped items keeps for the take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::world) struct DroppedStack {
    /// The item id.
    pub(in crate::world) item_id: String,
    /// The quantity dropped.
    pub(in crate::world) quantity: u32,
    /// Its rolls; `None` for an item without durability or perks (a resource keeps stacking).
    pub(in crate::world) rolled: Option<RolledItem>,
}

/// A saved loadout.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedSet {
    name: String,
    /// (paperdoll slot, instance id) of each item, one per slot.
    items: Vec<(u32, [u8; RAW_ID_LEN])>,
    /// The attribute points saved with it.
    attributes: Vec<(u32, u32)>,
}

/// What the second container shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Opened {
    /// A storage chest, and whether the player stands at it.
    Storage {
        storage: [u8; RAW_ID_LEN],
        near: bool,
    },
    /// A bag of dropped items and what it holds.
    Dropped {
        bag: [u8; RAW_ID_LEN],
        items: Vec<Option<Stack>>,
    },
    /// Nothing any more: the player closed what it opened.
    Closed,
}

/// The holdings of one player besides its worn items and its bag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Holdings {
    /// The counter of the next new stack.
    next_counter: u32,
    /// Each storage chest's slots.
    storages: BTreeMap<[u8; RAW_ID_LEN], Vec<Option<Stack>>>,
    /// The saved loadouts: index 1 first.
    loadouts: Vec<SavedSet>,
    /// Whether the loadout panel is open.
    panel_open: bool,
    /// What the second container shows; `None` before anything was opened.
    opened: Option<Opened>,
}

/// Puts `stack` into the first free slot of `slots`, a new slot when none is free and the
/// container may grow; the slot, or `None` when it is full.
fn put_in(slots: &mut Vec<Option<Stack>>, stack: Stack) -> Option<usize> {
    let free = slots.iter().position(Option::is_none).or_else(|| {
        (slots.len() < MAX_CONTAINER_SLOTS).then(|| {
            slots.push(None);
            slots.len() - 1
        })
    })?;
    slots[free] = Some(stack);
    Some(free)
}

/// Whether `slots` can take one more stack: a free slot, or room to grow.
fn has_room(slots: &[Option<Stack>]) -> bool {
    slots.iter().any(Option::is_none) || slots.len() < MAX_CONTAINER_SLOTS
}

/// Takes `quantity` (0 or more than the stack: all of it, retail 1838 and 2801) of the stack of
/// instance `instance` out of `slots`: the stack taken; the rest stays as it was.
fn take_out(
    slots: &mut [Option<Stack>],
    instance: [u8; RAW_ID_LEN],
    quantity: u32,
) -> Result<Stack, ItemRefusal> {
    let entry = slots
        .iter_mut()
        .find(|entry| {
            entry
                .as_ref()
                .is_some_and(|stack| stack.item.instance == instance)
        })
        .ok_or(ItemRefusal::NoSuchItem)?;
    let held = entry.as_ref().map_or(0, |stack| stack.item.quantity);
    if quantity == 0 || quantity >= held {
        return entry.take().ok_or(ItemRefusal::NoSuchItem);
    }
    let left = entry.as_mut().ok_or(ItemRefusal::NoSuchItem)?;
    left.item.quantity -= quantity;
    let mut taken = left.clone();
    taken.item.quantity = quantity;
    Ok(taken)
}

impl Stack {
    /// What it keeps over a save and a handoff.
    pub(super) fn state(&self) -> ItemState {
        ItemState {
            instance: Some(self.item.instance),
            locked: self.item.locked,
            gear_score: Some(self.item.gear_score),
            durability: Some(self.item.durability),
            perks: Some(self.perks.clone()),
            bound: Some(self.item.bound),
            look: ItemLookState {
                dye: self.item.look.dye,
                appearance: self.item.look.appearance,
                skin_dye: self.item.look.skin_dye,
            },
        }
    }

    /// Its gear score and perks when it is gear (it has durability or perks); `None` for an
    /// item that stacks with any other of its kind.
    pub(super) fn rolled(&self) -> Option<RolledItem> {
        (self.item.max_durability > 0 || !self.perks.is_empty()).then(|| RolledItem {
            gear_score: self.item.gear_score,
            perks: self.perks.clone(),
        })
    }

    /// The stack as dropped: its item, quantity and rolls.
    fn dropped(&self) -> DroppedStack {
        DroppedStack {
            item_id: self.item_id.clone(),
            quantity: self.item.quantity,
            rolled: self.rolled(),
        }
    }

    /// Sets its perks (ids in slot order) and their wire indexes.
    pub(super) fn set_perks(&mut self, kits: &StarterKits, perks: Vec<String>) {
        self.item.perks = kits.perk_slots(&perks);
        self.perks = perks;
    }
}

impl Gear {
    /// A new stack of `quantity` of `item`: the next item counter, and the instance id derived
    /// from the character and the item id, or from them and the first serial no stack holds.
    pub(super) fn new_stack(&mut self, item: &KitItem, quantity: u32) -> Stack {
        let mut new = stack(self.character, item, quantity);
        new.item.instance = self.free_instance(&item.item_id, None);
        new.item.counter = self.holdings.next_counter;
        self.holdings.next_counter = self.holdings.next_counter.saturating_add(1);
        new
    }

    /// A new stack of `quantity` of `item` with the saved `state`: its instance id, gear score,
    /// durability (at most full), perks and bind state when it kept them, its lock and its look.
    pub(super) fn restored_stack(
        &mut self,
        kits: &StarterKits,
        item: &KitItem,
        quantity: u32,
        state: &ItemState,
    ) -> Stack {
        let mut restored = self.new_stack(item, quantity);
        if let Some(instance) = state.instance {
            restored.item.instance = instance;
        }
        restored.item.locked = state.locked;
        if let Some(gear_score) = state.gear_score {
            restored.item.gear_score = gear_score;
        }
        if let Some(durability) = state.durability {
            restored.item.durability = durability.min(restored.item.max_durability);
        }
        if let Some(perks) = &state.perks {
            restored.set_perks(kits, perks.clone());
        }
        if let Some(bound) = state.bound {
            restored.item.bound = bound;
        }
        restored.item.look = ItemLook {
            dye: state.look.dye,
            appearance: state.look.appearance,
            skin_dye: state.look.skin_dye,
        };
        restored
    }

    /// `moved`, a stack taken out of a container, with an instance id of its own: a part split
    /// off a stack that stays gets a new instance id and counter (one id names one stack).
    pub(super) fn own_instance(&mut self, mut moved: Stack) -> Stack {
        let instance = moved.item.instance;
        if self.stacks().any(|left| left.item.instance == instance) {
            moved.item.instance = self.free_instance(&moved.item_id, None);
            moved.item.counter = self.holdings.next_counter;
            moved.key = None;
            self.holdings.next_counter = self.holdings.next_counter.saturating_add(1);
        }
        moved
    }

    /// `moved`, a stack that goes between the bag and a storage chest (2801, 2942), as it
    /// arrives: always the next item counter, and a new instance id unless it is an item with
    /// durability whose id no other stack holds.
    ///
    /// Retail `storage_open_deposit_withdraw_transfer` (12 moves): each resource deposited
    /// (19.6, 22.9, 39.7, 98.1 s) and each stack taken (28.3, 29.8, 94.7 s) arrived with a new
    /// instance id and a new counter; the gear deposited at 25.5 s (durability 750) kept its
    /// instance id, lock and perks, with a new counter (bag 100027, storage 90914). Retail
    /// counts the counters of the bag and of the storages apart (100071 and up, 90912 and up);
    /// where each count starts is Unknown, so this server takes them from its one counter.
    /// Inferred: a stack with durability taken out of a storage keeps its id as a deposited one
    /// did (no retail take of gear).
    fn stored_move(&mut self, mut moved: Stack) -> Stack {
        let old = moved.item.instance;
        let shared = self.stacks().any(|left| left.item.instance == old);
        if moved.item.max_durability == 0 || shared {
            moved.item.instance = self.free_instance(&moved.item_id, Some(&old));
            moved.key = None;
        }
        moved.item.counter = self.holdings.next_counter;
        self.holdings.next_counter = self.holdings.next_counter.saturating_add(1);
        moved
    }

    /// Every stack: worn, in the bag, in the storages and in an opened bag of dropped items.
    fn stacks(&self) -> impl Iterator<Item = &Stack> {
        let worn = self.worn.iter().filter_map(|(_, stack)| stack.as_ref());
        let stored = self.holdings.storages.values().flatten().flatten();
        let dropped: &[Option<Stack>] = match &self.holdings.opened {
            Some(Opened::Dropped { items, .. }) => items,
            _ => &[],
        };
        worn.chain(self.bag.iter().flatten())
            .chain(stored)
            .chain(dropped.iter().flatten())
    }

    /// An instance id for a new stack of `item_id` that no stack holds and that is not
    /// `besides` (the id a moved stack leaves behind).
    fn free_instance(&self, item_id: &str, besides: Option<&[u8; RAW_ID_LEN]>) -> [u8; RAW_ID_LEN] {
        let taken = |instance: &[u8; RAW_ID_LEN]| {
            besides == Some(instance) || self.stacks().any(|stack| stack.item.instance == *instance)
        };
        let first = instance_id(self.character, item_id);
        if !taken(&first) {
            return first;
        }
        (1_u64..)
            .map(|serial| {
                *Uuid::new_v5(
                    &self.character,
                    format!("{item_id}{SERIAL_SEPARATOR}{serial}").as_bytes(),
                )
                .as_bytes()
            })
            .find(|candidate| !taken(candidate))
            .unwrap_or(first)
    }

    /// The lowest owned-items key no stack holds.
    pub(super) fn lowest_free_key(&self) -> u16 {
        let used: BTreeSet<u16> = self.stacks().filter_map(|stack| stack.key).collect();
        (0_u16..)
            .find(|key| !used.contains(key))
            .unwrap_or(u16::MAX)
    }

    /// The instance ids the saved loadouts name.
    fn named(&self) -> BTreeSet<[u8; RAW_ID_LEN]> {
        self.holdings
            .loadouts
            .iter()
            .flat_map(|set| set.items.iter().map(|(_, instance)| *instance))
            .collect()
    }

    /// Frees the key of every stack that is neither worn nor named by a loadout, then gives a
    /// key to every named stack without one (retail: a new key goes to the lowest free key once
    /// the item put on took its own).
    pub(super) fn settle_keys(&mut self) {
        let named = self.named();
        let unworn = self
            .bag
            .iter_mut()
            .chain(self.holdings.storages.values_mut().flatten())
            .flatten();
        for stack in unworn {
            if !named.contains(&stack.item.instance) {
                stack.key = None;
            }
        }
        for instance in named {
            let key = self.lowest_free_key();
            let keyless = self
                .bag
                .iter_mut()
                .chain(self.holdings.storages.values_mut().flatten())
                .flatten()
                .find(|stack| stack.item.instance == instance && stack.key.is_none());
            if let Some(stack) = keyless {
                stack.key = Some(key);
            }
        }
    }

    /// What the gear states show besides the worn items, the bag and the hands.
    pub(super) fn extras(&self) -> GearExtras {
        let key_of = |instance: &[u8; RAW_ID_LEN]| {
            self.stacks()
                .find(|stack| stack.item.instance == *instance)
                .and_then(|stack| stack.key)
        };
        let listed = self
            .bag
            .iter()
            .chain(self.holdings.storages.values().flatten())
            .flatten()
            .filter_map(|stack| Some((stack.key?, stack.item)))
            .collect();
        let loadouts = self
            .holdings
            .loadouts
            .iter()
            .map(|set| Loadout {
                name: set.name.as_bytes().to_vec(),
                slots: set
                    .items
                    .iter()
                    .filter_map(|(slot_id, instance)| Some((*slot_id, key_of(instance)?)))
                    .collect(),
                flag: false,
                extra: set.attributes.clone(),
            })
            .collect();
        GearExtras {
            listed,
            loadouts,
            loadout_panel_open: self.holdings.panel_open,
            open_container: self.open_container(),
            bag_reasons: self
                .bag
                .iter()
                .enumerate()
                .map(|(index, stack)| {
                    stack.as_ref().map_or_else(
                        || self.emptied.get(&index).copied().unwrap_or(SYNC_LOGIN),
                        |stack| stack.reason,
                    )
                })
                .collect(),
        }
    }

    /// The second container's view.
    fn open_container(&self) -> Option<OpenContainer> {
        let view =
            |stacks: &[Option<Stack>], sync_reason, slots_listed, empty_reason| OpenContainer {
                items: stacks
                    .iter()
                    .map(|stack| stack.as_ref().map(|stack| stack.item))
                    .collect(),
                sync_reason,
                can_transfer: true,
                was_emptied: stacks.iter().all(Option::is_none),
                slots_listed,
                empty_reason,
            };
        Some(match self.holdings.opened.as_ref()? {
            Opened::Storage { storage, near } => {
                let slots = self
                    .holdings
                    .storages
                    .get(storage)
                    .map_or(&[][..], Vec::as_slice);
                // The view's reason goes with its empty slots too; only a storage the player
                // stands at lists the slots ([`OpenContainer::slots_listed`]).
                let reason = if *near {
                    SYNC_STORAGE_NEAR
                } else {
                    SYNC_STORAGE_AFAR
                };
                view(slots, reason, *near, reason)
            }
            Opened::Dropped { items, .. } => {
                view(items.as_slice(), SYNC_DROPPED_BAG, true, SYNC_LOGIN)
            }
            Opened::Closed => OpenContainer {
                items: vec![None; CLOSED_SLOTS],
                sync_reason: SYNC_DROPPED_BAG,
                can_transfer: false,
                was_emptied: false,
                slots_listed: true,
                empty_reason: SYNC_LOGIN,
            },
        })
    }

    /// Puts `stack` into the first free bag slot, a new one when none is free; whether the bag
    /// had room.
    pub(super) fn put_in_bag(&mut self, stack: Stack) -> bool {
        self.put_in_bag_as(stack, SYNC_MOVED)
    }

    /// [`Gear::put_in_bag`] for a change of sync reason `reason`.
    pub(super) fn put_in_bag_as(&mut self, mut stack: Stack, reason: u8) -> bool {
        stack.reason = reason;
        self.fill_bag(stack)
    }

    /// Puts `stack` into the first free bag slot, a new one when none is free, and the slot
    /// forgets the reason it was emptied with; whether the bag had room.
    fn fill_bag(&mut self, stack: Stack) -> bool {
        put_in(&mut self.bag, stack)
            .inspect(|index| {
                self.emptied.remove(index);
            })
            .is_some()
    }

    /// Puts `stack` into bag slot `index`, which forgets the reason it was emptied with.
    pub(super) fn fill_bag_slot(&mut self, index: usize, stack: Stack) {
        self.bag[index] = Some(stack);
        self.emptied.remove(&index);
    }

    /// Whether the bag can take one more stack.
    pub(super) fn bag_has_room(&self) -> bool {
        has_room(&self.bag)
    }

    /// Marks the items changed: the client and the next save get them.
    fn touched(&mut self) {
        self.changed = true;
        self.unsaved = true;
    }

    /// The storage chests and saved loadouts, for a save and a handoff.
    pub(in crate::world) fn inventory(&self) -> PlayerInventory {
        PlayerInventory {
            stored: self
                .holdings
                .storages
                .iter()
                .flat_map(|(storage, slots)| {
                    (0_u16..).zip(slots).filter_map(|(slot, stack)| {
                        let stack = stack.as_ref()?;
                        Some(StoredItem {
                            storage: *storage,
                            slot,
                            item_id: stack.item_id.clone(),
                            quantity: stack.item.quantity,
                            state: stack.state(),
                        })
                    })
                })
                .collect(),
            loadouts: (1_u8..)
                .zip(&self.holdings.loadouts)
                .map(|(index, set)| SavedLoadout {
                    index,
                    name: set.name.clone(),
                    items: set.items.clone(),
                    attributes: set.attributes.clone(),
                })
                .collect(),
        }
    }

    /// Takes the storage chests and loadouts of `inventory` (a join or a handoff): an item `kits`
    /// does not know is left out; a loadout index past the list adds the loadouts before it.
    pub(in crate::world) fn restore_inventory(
        &mut self,
        kits: &StarterKits,
        inventory: &PlayerInventory,
    ) {
        for stored in &inventory.stored {
            let Some(item) = kits.item(&stored.item_id) else {
                continue;
            };
            let stack = self.restored_stack(kits, item, stored.quantity, &stored.state);
            let slots = self.holdings.storages.entry(stored.storage).or_default();
            let slot = usize::from(stored.slot);
            if slot >= MAX_CONTAINER_SLOTS {
                continue;
            }
            if slots.len() <= slot {
                slots.resize(slot + 1, None);
            }
            if slots[slot].is_none() {
                slots[slot] = Some(stack);
            } else {
                put_in(slots, stack);
            }
        }
        for saved in &inventory.loadouts {
            let set = self.loadout_mut(saved.index);
            if let Some(set) = set {
                *set = SavedSet {
                    name: saved.name.clone(),
                    items: saved.items.clone(),
                    attributes: saved.attributes.clone(),
                };
            }
        }
        self.settle_keys();
    }

    /// The saved loadout of `index` (1 or more), added with its default name, and the ones
    /// before it, when it is new; `None` for index 0, the worn items.
    fn loadout_mut(&mut self, index: u8) -> Option<&mut SavedSet> {
        let position = usize::from(index.checked_sub(1)?);
        while self.holdings.loadouts.len() <= position {
            let next = self.holdings.loadouts.len() + 1;
            self.holdings.loadouts.push(SavedSet {
                name: format!("{LOADOUT_NAME_PREFIX}{next}"),
                items: Vec::new(),
                attributes: Vec::new(),
            });
        }
        self.holdings.loadouts.get_mut(position)
    }

    /// Takes `quantity` (0 or more than the stack: all of it) of the stack of instance
    /// `instance` in bag slot `bag_slot` out of the bag to drop it (1838): what was dropped. A
    /// locked stack stays.
    pub(in crate::world) fn drop_from_bag(
        &mut self,
        bag_slot: u32,
        instance: [u8; RAW_ID_LEN],
        quantity: u32,
    ) -> Result<DroppedStack, ItemRefusal> {
        let index = usize::try_from(bag_slot).map_err(|_| ItemRefusal::NoSuchItem)?;
        let stack = self
            .bag
            .get(index)
            .and_then(Option::as_ref)
            .filter(|stack| stack.item.instance == instance)
            .ok_or(ItemRefusal::NoSuchItem)?;
        if stack.item.locked {
            return Err(ItemRefusal::Locked);
        }
        let dropped = take_out(&mut self.bag[index..=index], instance, quantity)?;
        self.touched();
        Ok(dropped.dropped())
    }

    /// Takes `quantity` (0 or more than the stack: all of it) of the item worn in paperdoll slot
    /// `slot_id` off to drop it (826) at time `time_ns`: what was dropped. A visible slot
    /// stays, emptied, in its place, as for a removed item.
    pub(in crate::world) fn drop_worn(
        &mut self,
        slot_id: u32,
        quantity: u32,
        time_ns: u64,
    ) -> Result<DroppedStack, ItemRefusal> {
        let index = self
            .worn
            .iter()
            .position(|(worn, stack)| *worn == slot_id && stack.is_some())
            .ok_or(ItemRefusal::NoSuchItem)?;
        let stack = self.worn[index].1.as_mut().ok_or(ItemRefusal::NoSuchItem)?;
        if stack.item.locked {
            return Err(ItemRefusal::Locked);
        }
        let mut dropped = stack.dropped();
        let held = stack.item.quantity;
        if quantity > 0 && quantity < held {
            stack.item.quantity -= quantity;
            dropped.quantity = quantity;
            self.touched();
            return Ok(dropped);
        }
        if let Some(sheathed) = self.empty_worn(index) {
            self.stamp(sheathed, time_ns);
        }
        self.touched();
        Ok(dropped)
    }

    /// The player uses storage chest `storage`: its items show in the second container, near
    /// (interaction 26) or seen from afar (1512).
    pub(in crate::world) fn open_storage(&mut self, storage: [u8; RAW_ID_LEN], near: bool) {
        self.holdings.storages.entry(storage).or_default();
        self.holdings.opened = Some(Opened::Storage { storage, near });
        self.changed = true;
    }

    /// The storage the player uses, when it uses one.
    pub(in crate::world) fn open_storage_id(&self) -> Option<[u8; RAW_ID_LEN]> {
        match self.holdings.opened {
            Some(Opened::Storage { storage, .. }) => Some(storage),
            _ => None,
        }
    }

    /// The player opened bag `bag` of dropped items, which holds `items` with their rolls: they
    /// show in the second container, each with its rolls and the instance id its global storage
    /// record has (the bag's id and its place, as `loot_bag::storage_of`).
    pub(in crate::world) fn open_dropped(
        &mut self,
        kits: &StarterKits,
        bag: [u8; RAW_ID_LEN],
        items: &[(&KitItem, u32, Option<RolledItem>)],
    ) {
        let namespace = Uuid::from_bytes(bag);
        let stacks = (0_u64..)
            .zip(items)
            .map(|(index, (item, quantity, rolled))| {
                let mut shown = stack(self.character, item, *quantity);
                shown.item.instance = *Uuid::new_v5(&namespace, &index.to_be_bytes()).as_bytes();
                if let Some(rolled) = rolled {
                    shown.item.gear_score = rolled.gear_score;
                    shown.set_perks(kits, rolled.perks.clone());
                }
                Some(shown)
            })
            .collect();
        self.holdings.opened = Some(Opened::Dropped { bag, items: stacks });
        self.changed = true;
    }

    /// The bag of dropped items the player has open, when it has one.
    pub(in crate::world) fn open_bag(&self) -> Option<[u8; RAW_ID_LEN]> {
        match self.holdings.opened {
            Some(Opened::Dropped { bag, .. }) => Some(bag),
            _ => None,
        }
    }

    /// The place in the opened bag of dropped items of the stack of instance `instance`.
    pub(in crate::world) fn dropped_index(&self, instance: [u8; RAW_ID_LEN]) -> Option<usize> {
        let Some(Opened::Dropped { items, .. }) = &self.holdings.opened else {
            return None;
        };
        items
            .iter()
            .flatten()
            .position(|stack| stack.item.instance == instance)
    }

    /// The items of storage chest `storage`; `None` for a storage the player never used.
    pub(in crate::world) fn stored_items(
        &self,
        storage: &[u8; RAW_ID_LEN],
    ) -> Option<Vec<GearItem>> {
        let slots = self.holdings.storages.get(storage)?;
        Some(slots.iter().flatten().map(|stack| stack.item).collect())
    }

    /// The bag's stacks in slot order: each one's bag slot, item id, instance id and lock.
    pub(in crate::world) fn bag_stacks(&self) -> Vec<(u32, String, [u8; RAW_ID_LEN], bool)> {
        (0_u32..)
            .zip(&self.bag)
            .filter_map(|(bag_slot, stack)| {
                let stack = stack.as_ref()?;
                Some((
                    bag_slot,
                    stack.item_id.clone(),
                    stack.item.instance,
                    stack.item.locked,
                ))
            })
            .collect()
    }

    /// The stacks the second container shows (a storage chest or a bag of dropped items): each
    /// one's item id and instance id; empty when nothing is open.
    pub(in crate::world) fn open_stacks(&self) -> Vec<(String, [u8; RAW_ID_LEN])> {
        let shown: &[Option<Stack>] = match &self.holdings.opened {
            Some(Opened::Storage { storage, .. }) => self
                .holdings
                .storages
                .get(storage)
                .map_or(&[][..], Vec::as_slice),
            Some(Opened::Dropped { items, .. }) => items,
            Some(Opened::Closed) | None => &[],
        };
        shown
            .iter()
            .flatten()
            .map(|stack| (stack.item_id.clone(), stack.item.instance))
            .collect()
    }

    /// Each stored stack: its item id, its storage chest, its slot there and its quantity.
    pub(in crate::world) fn stored_stacks(&self) -> Vec<(String, [u8; RAW_ID_LEN], usize, u32)> {
        self.holdings
            .storages
            .iter()
            .flat_map(|(storage, slots)| {
                slots.iter().enumerate().filter_map(|(slot, stack)| {
                    let stack = stack.as_ref()?;
                    Some((stack.item_id.clone(), *storage, slot, stack.item.quantity))
                })
            })
            .collect()
    }

    /// The total quantity of each item over every storage chest of the player, by item id.
    pub(in crate::world) fn stored_totals(&self) -> BTreeMap<String, u32> {
        let mut totals = BTreeMap::new();
        for stack in self.holdings.storages.values().flatten().flatten() {
            let total: &mut u32 = totals.entry(stack.item_id.clone()).or_default();
            *total = total.saturating_add(stack.item.quantity);
        }
        totals
    }

    /// The player stops using what its second container shows (2473); whether it showed
    /// something.
    pub(in crate::world) fn close_container(&mut self) -> bool {
        let was_open = matches!(
            self.holdings.opened,
            Some(Opened::Storage { .. } | Opened::Dropped { .. })
        );
        if was_open {
            self.holdings.opened = Some(Opened::Closed);
            self.changed = true;
        }
        was_open
    }

    /// The open storage chest's slots.
    fn open_slots(&mut self) -> Result<&mut Vec<Option<Stack>>, ItemRefusal> {
        let storage = self.open_storage_id().ok_or(ItemRefusal::NoContainer)?;
        Ok(self.holdings.storages.entry(storage).or_default())
    }

    /// Moves `quantity` (0 or more than the stack: all of it) of the bag stack of instance
    /// `instance` into the open storage chest (2801). A locked stack stays.
    pub(in crate::world) fn deposit(
        &mut self,
        instance: [u8; RAW_ID_LEN],
        quantity: u32,
    ) -> Result<(), ItemRefusal> {
        self.open_slots()?;
        let locked = self
            .bag
            .iter()
            .flatten()
            .find(|stack| stack.item.instance == instance)
            .ok_or(ItemRefusal::NoSuchItem)?
            .item
            .locked;
        if locked {
            return Err(ItemRefusal::Locked);
        }
        if !has_room(self.open_slots()?) {
            return Err(ItemRefusal::Full);
        }
        let moved = take_out(&mut self.bag, instance, quantity)?;
        let moved = self.stored_move(moved);
        put_in(self.open_slots()?, moved);
        self.touched();
        Ok(())
    }

    /// Moves `quantity` (0 or more than the stack: all of it) of the stack of instance
    /// `instance` from the open storage chest into the bag (2942).
    pub(in crate::world) fn withdraw(
        &mut self,
        instance: [u8; RAW_ID_LEN],
        quantity: u32,
    ) -> Result<(), ItemRefusal> {
        if !has_room(&self.bag) {
            return Err(ItemRefusal::Full);
        }
        let moved = take_out(self.open_slots()?, instance, quantity)?;
        let moved = self.stored_move(moved);
        self.fill_bag(moved);
        self.touched();
        Ok(())
    }

    /// Puts `quantity` of `item` into storage chest `storage` in new stacks of at most the
    /// item's stack size, each with sync reason `reason`: the units of a filled buy order, which
    /// retail gave to the storage of the order's settlement (`buy-sell-TP` 328.900 s and
    /// `Hivesellandbuyorder` 401.71 s: a 7048 with the 1117 of the order and no bag change, 2 of
    /// 2). Whether they join a stored stack of the item is Unknown; a deposit makes its own
    /// stack (`storage_open_deposit_withdraw_transfer`), and so does this. Nothing is stored
    /// when they do not all fit; whether it fit.
    pub(in crate::world) fn store_new(
        &mut self,
        storage: [u8; RAW_ID_LEN],
        item: &KitItem,
        quantity: u32,
        reason: u8,
    ) -> bool {
        if quantity == 0 {
            return false;
        }
        let limit = item
            .max_stack
            .unwrap_or(MAX_STACK_QUANTITY)
            .clamp(1, MAX_STACK_QUANTITY);
        let needed = quantity.div_ceil(limit);
        let slots = self.holdings.storages.entry(storage).or_default();
        let free = slots.iter().filter(|slot| slot.is_none()).count()
            + MAX_CONTAINER_SLOTS.saturating_sub(slots.len());
        if usize::try_from(needed).map_or(true, |needed| needed > free) {
            return false;
        }
        let mut left = quantity;
        while left > 0 {
            let part = left.min(limit);
            let mut new = self.new_stack(item, part);
            new.reason = reason;
            put_in(self.holdings.storages.entry(storage).or_default(), new);
            left -= part;
        }
        self.touched();
        true
    }

    /// Puts `quantity` of the open storage's stack of instance `instance` on in paperdoll slot
    /// `slot_id` (1461); an item worn there goes into the storage. The id of the equipped item.
    pub(in crate::world) fn equip_from_storage(
        &mut self,
        slot_id: u32,
        instance: [u8; RAW_ID_LEN],
        quantity: u32,
    ) -> Result<String, ItemRefusal> {
        let slots = self.open_slots()?;
        let kind = slots
            .iter()
            .flatten()
            .find(|stack| stack.item.instance == instance)
            .ok_or(ItemRefusal::NoSuchItem)?
            .kind;
        if !fits(kind, slot_id) {
            return Err(ItemRefusal::WrongSlot);
        }
        if !has_room(slots) {
            return Err(ItemRefusal::Full);
        }
        let taken = take_out(slots, instance, quantity.max(1))?;
        let taken = self.own_instance(taken);
        let item_id = taken.item_id.clone();
        if let Some(off) = self.take_off(slot_id) {
            put_in(self.open_slots()?, off);
        }
        self.put_on(slot_id, taken);
        self.touched();
        Ok(item_id)
    }

    /// Takes the item of instance `instance` worn in paperdoll slot `slot_id` off into the open
    /// storage chest (2950).
    pub(in crate::world) fn unequip_to_storage(
        &mut self,
        slot_id: u32,
        instance: [u8; RAW_ID_LEN],
    ) -> Result<(), ItemRefusal> {
        if !has_room(self.open_slots()?) {
            return Err(ItemRefusal::Full);
        }
        let worn = self.worn.iter().any(|(worn, stack)| {
            *worn == slot_id
                && stack
                    .as_ref()
                    .is_some_and(|stack| stack.item.instance == instance)
        });
        if !worn {
            return Err(ItemRefusal::NoSuchItem);
        }
        let off = self.take_off(slot_id).ok_or(ItemRefusal::NoSuchItem)?;
        put_in(self.open_slots()?, off);
        self.touched();
        Ok(())
    }

    /// Takes the stack worn in paperdoll slot `slot_id` off: a visible slot stays, emptied, in
    /// its place (retail newchar 2026-09-18: the chest taken off was replaced at its visible
    /// index), a hidden one leaves the list.
    pub(super) fn take_off(&mut self, slot_id: u32) -> Option<Stack> {
        let index = self
            .worn
            .iter()
            .position(|(worn, stack)| *worn == slot_id && stack.is_some())?;
        let off = self.worn[index].1.clone();
        let _ = self.empty_worn(index);
        off
    }

    /// The items `taken` of the opened bag of dropped items go into the bag, each with its
    /// rolls, and the view shows it empty; whether all of them fit.
    pub(in crate::world) fn take_dropped(
        &mut self,
        kits: &StarterKits,
        taken: &[(&KitItem, u32, Option<RolledItem>)],
    ) -> bool {
        let mut all = true;
        for (item, quantity, rolled) in taken {
            all &= match rolled {
                Some(rolled) => self.add_rolled(kits, item, *quantity, rolled, SYNC_MOVED),
                None => self.add(item, *quantity),
            };
        }
        if let Some(Opened::Dropped { items, .. }) = &mut self.holdings.opened {
            items.iter_mut().for_each(|entry| *entry = None);
        }
        self.changed = true;
        all
    }

    /// Sets the lock of the stack of instance `instance` that the item reference (`location`,
    /// `slot`) names (2549).
    pub(in crate::world) fn set_locked(
        &mut self,
        location: u32,
        slot: u32,
        instance: [u8; RAW_ID_LEN],
        locked: bool,
    ) -> Result<(), ItemRefusal> {
        let stack = self.referenced_mut(location, slot, instance)?;
        stack.item.locked = locked;
        self.touched();
        Ok(())
    }

    /// The stack an item reference names: a bag slot ([`LOCATION_BAG`]) or an owned-items key
    /// ([`LOCATIONS_BY_KEY`]); its instance id must match unless the reference carries none
    /// (retail 5579 by key sent an all-zero id).
    pub(super) fn referenced_mut(
        &mut self,
        location: u32,
        slot: u32,
        instance: [u8; RAW_ID_LEN],
    ) -> Result<&mut Stack, ItemRefusal> {
        let matches =
            |stack: &Stack| instance == [0; RAW_ID_LEN] || stack.item.instance == instance;
        let found = if location == LOCATION_BAG {
            usize::try_from(slot)
                .ok()
                .and_then(|index| self.bag.get_mut(index))
                .and_then(Option::as_mut)
        } else if LOCATIONS_BY_KEY.contains(&location) {
            let key = u16::try_from(slot).map_err(|_| ItemRefusal::NoSuchItem)?;
            self.worn
                .iter_mut()
                .filter_map(|(_, stack)| stack.as_mut())
                .chain(self.bag.iter_mut().flatten())
                .chain(self.holdings.storages.values_mut().flatten().flatten())
                .find(|stack| stack.key == Some(key))
        } else {
            None
        };
        found
            .filter(|stack| matches(stack))
            .ok_or(ItemRefusal::NoSuchItem)
    }

    /// Opens or closes the loadout panel (5578).
    pub(in crate::world) fn set_loadout_panel(&mut self, open: bool) {
        if std::mem::replace(&mut self.holdings.panel_open, open) != open {
            self.changed = true;
        }
    }

    /// Puts the stack that the item reference (`location`, `slot`, `instance`) names into
    /// paperdoll slot `slot_id` of each loadout of `loadouts` (5579): the status of each.
    pub(in crate::world) fn add_to_loadouts(
        &mut self,
        location: u32,
        slot: u32,
        instance: [u8; RAW_ID_LEN],
        slot_id: u32,
        loadouts: &[u8],
    ) -> Vec<(u8, u32)> {
        let found = self
            .referenced_mut(location, slot, instance)
            .map(|stack| (stack.item.instance, stack.kind));
        let statuses = loadouts
            .iter()
            .map(|index| {
                let status = match found {
                    Ok((instance, kind)) if fits(kind, slot_id) => match self.loadout_mut(*index) {
                        Some(set) => {
                            set.items
                                .retain(|(worn, named)| *worn != slot_id && *named != instance);
                            set.items.push((slot_id, instance));
                            loadout_status::EQUIP_UPDATE_SUCCESS
                        }
                        None => loadout_status::EQUIP_UPDATE_FAIL,
                    },
                    _ => loadout_status::EQUIP_UPDATE_FAIL,
                };
                (*index, status)
            })
            .collect();
        self.settle_keys();
        self.touched();
        statuses
    }

    /// Saves the worn items as loadout `index` (5542) with the attribute points `attributes`.
    pub(in crate::world) fn save_to_loadout(&mut self, index: u8, attributes: Vec<(u32, u32)>) {
        let items: Vec<(u32, [u8; RAW_ID_LEN])> = self
            .worn
            .iter()
            .filter_map(|(slot_id, stack)| Some((*slot_id, stack.as_ref()?.item.instance)))
            .collect();
        if let Some(set) = self.loadout_mut(index) {
            set.items = items;
            set.attributes = attributes;
            self.settle_keys();
            self.touched();
        }
    }

    /// Empties loadout `index` (5427): the status, `None` without such a loadout.
    pub(in crate::world) fn clear_loadout(&mut self, index: u8) -> Option<u32> {
        let position = usize::from(index.checked_sub(1)?);
        let set = self.holdings.loadouts.get_mut(position)?;
        let count = std::mem::take(&mut set.items).len();
        self.settle_keys();
        self.touched();
        Some(if count == 1 {
            loadout_status::UNEQUIP_UPDATE_LINKED
        } else {
            loadout_status::UNEQUIP_ALL_UPDATE_LINKED
        })
    }

    /// Renames loadout `index` (5487).
    pub(in crate::world) fn rename_loadout(&mut self, index: u8, name: String) {
        if let Some(set) = self.loadout_mut(index) {
            set.name = name;
            self.touched();
        }
    }

    /// Takes the item of owned-items key `key` out of every loadout (5443).
    pub(in crate::world) fn remove_from_loadouts(&mut self, key: u16) {
        let Some(instance) = self.instance_of_key(key) else {
            return;
        };
        for set in &mut self.holdings.loadouts {
            set.items.retain(|(_, named)| *named != instance);
        }
        self.settle_keys();
        self.touched();
    }

    /// Takes the item of owned-items key `key` in paperdoll slot `slot_id` out of loadout
    /// `index` (5430).
    pub(in crate::world) fn remove_from_loadout(&mut self, index: u8, key: u16, slot_id: u32) {
        let Some(instance) = self.instance_of_key(key) else {
            return;
        };
        let Some(position) = index.checked_sub(1).map(usize::from) else {
            return;
        };
        if let Some(set) = self.holdings.loadouts.get_mut(position) {
            set.items
                .retain(|(worn, named)| (*worn, *named) != (slot_id, instance));
            self.settle_keys();
            self.touched();
        }
    }

    /// The instance id of the stack of owned-items key `key`.
    fn instance_of_key(&self, key: u16) -> Option<[u8; RAW_ID_LEN]> {
        self.stacks()
            .find(|stack| stack.key == Some(key))
            .map(|stack| stack.item.instance)
    }

    /// Puts on the items of loadout `index` (5442), each from the bag into its slot, the item
    /// worn there into the bag: the status. An item that `disabled` names stays in the bag.
    pub(in crate::world) fn switch_loadout(
        &mut self,
        index: u8,
        disabled: impl Fn(&str) -> bool,
    ) -> u32 {
        let Some(set) = index
            .checked_sub(1)
            .and_then(|position| self.holdings.loadouts.get(usize::from(position)))
        else {
            return loadout_status::CHANGE_FAILURE_INDEX;
        };
        let wanted = set.items.clone();
        let mut all = true;
        for (slot_id, instance) in wanted {
            let worn = self.worn.iter().any(|(worn, stack)| {
                *worn == slot_id
                    && stack
                        .as_ref()
                        .is_some_and(|stack| stack.item.instance == instance)
            });
            if worn {
                continue;
            }
            let bag_slot = self
                .bag
                .iter()
                .position(|entry| {
                    entry
                        .as_ref()
                        .is_some_and(|stack| stack.item.instance == instance)
                })
                .and_then(|index| u32::try_from(index).ok());
            let equipped = bag_slot.is_some_and(|bag_slot| {
                self.equip(bag_slot, slot_id, MAX_STACK_QUANTITY, instance, &disabled)
                    .is_ok()
            });
            all &= equipped;
        }
        if all {
            loadout_status::CHANGE_SUCCESS
        } else {
            loadout_status::CHANGE_FAILURE_GENERIC
        }
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::paperdoll::slot;
    use nw_messages::{OwnedItems, ReplicatedList};

    use super::super::tests::{
        BACKSTORY, CHARACTER, FLINT, POTION, STAFF, SWORD, kits_with, none_disabled,
    };
    use super::*;

    /// A second chest, to put on over the kit's.
    const NEW_CHEST: &str = "ChestT2";
    /// A storage chest's entity id (retail form: 8 bytes, then zeros).
    const STORAGE: [u8; RAW_ID_LEN] = [
        0x6b, 0xbf, 0x90, 0xec, 0x53, 0xd9, 0xf3, 0x5f, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    const FLINTS: u32 = 12;
    const LOADOUT: u8 = 5;

    fn kits() -> StarterKits {
        kits_with(&[(NEW_CHEST, "EquippableChest+Armor+Light")])
    }

    /// The kit worn (chest key 0, sword key 1), the staff and `FLINTS` flint in the bag, after
    /// the first states.
    fn gear() -> Gear {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.add(kits.item(STAFF).expect("staff"), 1));
        assert!(gear.add(kits.item(FLINT).expect("flint"), FLINTS));
        gear.first_states();
        gear
    }

    /// Sets the durability (and its maximum) of the bag stack of instance `instance`; its
    /// counter.
    fn set_durability(gear: &mut Gear, instance: [u8; RAW_ID_LEN], durability: u32) -> u32 {
        let stack = gear
            .bag
            .iter_mut()
            .flatten()
            .find(|stack| stack.item.instance == instance)
            .expect("in the bag");
        stack.item.max_durability = durability;
        stack.item.durability = durability;
        stack.item.counter
    }

    fn bag_instance(gear: &Gear, item_id: &str) -> [u8; RAW_ID_LEN] {
        gear.bag
            .iter()
            .flatten()
            .find(|stack| stack.item_id == item_id)
            .expect("in the bag")
            .item
            .instance
    }

    fn worn_key(gear: &Gear, slot_id: u32) -> Option<u16> {
        gear.worn
            .iter()
            .find(|(worn, stack)| *worn == slot_id && stack.is_some())
            .and_then(|(_, stack)| stack.as_ref()?.key)
    }

    #[test]
    fn a_new_item_takes_the_lowest_free_key_before_the_old_one_frees() {
        let kits = kits();
        let mut gear = gear();
        assert_eq!(
            (
                worn_key(&gear, slot::CHEST),
                worn_key(&gear, slot::MAIN_HAND_OPTION_1)
            ),
            (Some(0), Some(1))
        );
        assert!(gear.add(kits.item(NEW_CHEST).expect("chest"), 1));
        let bag_slot = gear
            .bag
            .iter()
            .position(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|stack| stack.item_id == NEW_CHEST)
            })
            .and_then(|index| u32::try_from(index).ok())
            .expect("slot");
        let instance = bag_instance(&gear, NEW_CHEST);
        assert!(
            gear.equip(bag_slot, slot::CHEST, 1, instance, none_disabled)
                .is_ok()
        );
        // Retail newchar 2026-09-18: the chest put on while key 0 was worn took key 11, the
        // next free one, and key 0 was free afterwards.
        assert_eq!(worn_key(&gear, slot::CHEST), Some(2));
        let change = gear.take_change().expect("change");
        let Some(OwnedItems::Changes(changes)) = change.owned.owned else {
            panic!("owned changes");
        };
        assert_eq!(
            changes
                .iter()
                .map(|change| (change.key, change.item.is_some()))
                .collect::<Vec<_>>(),
            [(0, false), (2, true)]
        );
        assert_eq!(gear.lowest_free_key(), 0);
        let visible = change
            .paperdoll
            .owner
            .and_then(|owner| owner.visible_items)
            .expect("visible");
        assert!(
            matches!(&visible, ReplicatedList::Changes(changes) if changes[0].index == 0),
            "the chest's visible place stays"
        );
    }

    #[test]
    fn a_storage_takes_and_gives_back_the_same_stack() {
        let mut gear = gear();
        let flint = bag_instance(&gear, FLINT);
        set_durability(&mut gear, flint, 0);
        assert_eq!(gear.deposit(flint, 0), Err(ItemRefusal::NoContainer));
        gear.open_storage(STORAGE, true);
        let view = gear.extras().open_container.expect("open");
        assert!(view.can_transfer && view.was_emptied);
        // A part of the stack goes in with an id of its own.
        assert_eq!(gear.deposit(flint, 2), Ok(()));
        let stored = gear.holdings.storages[&STORAGE][0].clone().expect("stored");
        assert_eq!(stored.item.quantity, 2);
        assert_ne!(stored.item.instance, flint);
        // The rest of it, an item without durability, takes a new id and counter too (retail).
        assert_eq!(gear.deposit(flint, 0), Ok(()));
        let rest = gear.holdings.storages[&STORAGE][1].clone().expect("stored");
        assert_eq!(rest.item.quantity, FLINTS - 2);
        assert_ne!(rest.item.instance, flint);
        assert_ne!(rest.item.counter, stored.item.counter);
        assert_eq!(gear.bag_count(FLINT), 0);
        let saved = gear.inventory();
        assert_eq!(saved.stored.len(), 2);
        assert_eq!(gear.withdraw(flint, 0), Err(ItemRefusal::NoSuchItem));
        assert_eq!(gear.withdraw(rest.item.instance, 0), Ok(()));
        assert_eq!(gear.bag_count(FLINT), FLINTS - 2);
        assert_ne!(bag_instance(&gear, FLINT), rest.item.instance);
        assert!(gear.close_container());
        let closed = gear.extras().open_container.expect("closed");
        assert_eq!((closed.items.len(), closed.can_transfer), (1, false));
        assert_eq!(
            gear.withdraw(stored.item.instance, 0),
            Err(ItemRefusal::NoContainer)
        );
        // Another sim takes the storage as it was saved.
        let kits = kits();
        let mut again = Gear::carried_in(&kits, CHARACTER, &gear.carried(), None, None);
        again.restore_inventory(&kits, &saved);
        assert_eq!(again.inventory(), saved);
    }

    #[test]
    fn stored_gear_keeps_its_id_and_a_storage_seen_from_afar_lists_no_slots() {
        const DURABILITY: u32 = 750;
        let mut gear = gear();
        let flint = bag_instance(&gear, FLINT);
        let counter = set_durability(&mut gear, flint, DURABILITY);
        gear.open_storage(STORAGE, true);
        assert_eq!(gear.deposit(flint, 0), Ok(()));
        let stored = gear.holdings.storages[&STORAGE][0].clone().expect("stored");
        assert_eq!(
            stored.item.instance, flint,
            "an item with durability keeps its id"
        );
        assert_ne!(stored.item.counter, counter);
        let near = gear.extras().open_container.expect("near");
        assert_eq!(
            (near.sync_reason, near.slots_listed, near.empty_reason),
            (SYNC_STORAGE_NEAR, true, SYNC_STORAGE_NEAR)
        );
        gear.open_storage(STORAGE, false);
        let afar = gear.extras().open_container.expect("afar");
        assert_eq!(
            (afar.sync_reason, afar.slots_listed, afar.empty_reason),
            (SYNC_STORAGE_AFAR, false, SYNC_STORAGE_AFAR)
        );
    }

    #[test]
    fn an_item_keeps_its_rolls_and_look_over_a_handoff() {
        const ROLLED_GEAR_SCORE: u32 = 590;
        const WORN_DURABILITY: u32 = 12;
        const PERK: &str = "PerkID_Weapon_DmgCrit";
        let mut gear = gear();
        let flint = bag_instance(&gear, FLINT);
        let kits = kits();
        let stack = gear
            .bag
            .iter_mut()
            .flatten()
            .find(|stack| stack.item.instance == flint)
            .expect("flint");
        stack.item.gear_score = ROLLED_GEAR_SCORE;
        stack.item.max_durability = WORN_DURABILITY * 2;
        stack.item.durability = WORN_DURABILITY;
        stack.item.bound = false;
        stack.item.look = ItemLook {
            dye: [4, 0, 184, 0],
            appearance: u32::MAX,
            skin_dye: [0, 5, 0, 0],
        };
        stack.set_perks(&kits, vec![PERK.to_owned()]);
        let carried = gear.carried();
        let state = &carried
            .iter()
            .find(|item| item.item_id == FLINT)
            .expect("carried")
            .state;
        assert_eq!(state.gear_score, Some(ROLLED_GEAR_SCORE));
        assert_eq!(state.durability, Some(WORN_DURABILITY));
        assert_eq!(state.perks.as_deref(), Some(&[PERK.to_owned()][..]));
        assert_eq!(state.bound, Some(false));
        assert_eq!(state.look.appearance, u32::MAX);
        let again = Gear::carried_in(&kits, CHARACTER, &carried, None, None);
        let restored = again
            .bag
            .iter()
            .flatten()
            .find(|stack| stack.item.instance == flint)
            .expect("restored");
        assert_eq!(restored.item.gear_score, ROLLED_GEAR_SCORE);
        assert_eq!(restored.perks, [PERK]);
        assert!(!restored.item.bound);
        assert_eq!(restored.item.look.dye, [4, 0, 184, 0]);
        assert_eq!(restored.state().look, state.look);
    }

    #[test]
    fn a_locked_stack_is_not_dropped_and_a_drop_takes_its_quantity() {
        let mut gear = gear();
        let flint = bag_instance(&gear, FLINT);
        let bag_slot = 1;
        assert_eq!(gear.set_locked(LOCATION_BAG, bag_slot, flint, true), Ok(()));
        assert_eq!(
            gear.drop_from_bag(bag_slot, flint, 1),
            Err(ItemRefusal::Locked)
        );
        assert_eq!(
            gear.set_locked(LOCATION_BAG, bag_slot, flint, false),
            Ok(())
        );
        let dropped = |item_id: &str, quantity, rolled| DroppedStack {
            item_id: item_id.to_owned(),
            quantity,
            rolled,
        };
        // The test kit gives every item a durability: each drop is gear with its rolls.
        let flint_rolls = gear
            .bag
            .iter()
            .flatten()
            .find(|stack| stack.item.instance == flint)
            .and_then(Stack::rolled);
        assert!(flint_rolls.is_some());
        assert_eq!(
            gear.drop_from_bag(bag_slot, flint, 5),
            Ok(dropped(FLINT, 5, flint_rolls.clone()))
        );
        assert_eq!(gear.bag_count(FLINT), FLINTS - 5);
        assert_eq!(
            gear.drop_from_bag(bag_slot, flint, 0),
            Ok(dropped(FLINT, FLINTS - 5, flint_rolls)),
            "0: the whole stack"
        );
        let sword = gear
            .drop_worn(slot::MAIN_HAND_OPTION_1, 0, 0)
            .expect("sword");
        assert_eq!((sword.item_id.as_str(), sword.quantity), (SWORD, 1));
        assert_eq!(gear.worn_item(slot::MAIN_HAND_OPTION_1), None);
    }

    #[test]
    fn loadouts_name_their_items_by_key_and_switch_them_on() {
        let mut gear = gear();
        let staff = bag_instance(&gear, STAFF);
        let statuses =
            gear.add_to_loadouts(LOCATION_BAG, 0, staff, slot::MAIN_HAND_OPTION_1, &[LOADOUT]);
        assert_eq!(statuses, [(LOADOUT, loadout_status::EQUIP_UPDATE_SUCCESS)]);
        let extras = gear.extras();
        assert_eq!(extras.loadouts.len(), usize::from(LOADOUT));
        assert_eq!(extras.loadouts[0].name, b"@lo_name 1");
        let staff_key = extras.listed[0].0;
        assert_eq!(
            extras.loadouts[usize::from(LOADOUT) - 1].slots,
            [(slot::MAIN_HAND_OPTION_1, staff_key)]
        );
        assert_eq!(
            gear.switch_loadout(LOADOUT, none_disabled),
            loadout_status::CHANGE_SUCCESS
        );
        assert_eq!(gear.worn_item(slot::MAIN_HAND_OPTION_1), Some(STAFF));
        assert_eq!(worn_key(&gear, slot::MAIN_HAND_OPTION_1), Some(staff_key));
        assert_eq!(
            gear.switch_loadout(LOADOUT + 1, none_disabled),
            loadout_status::CHANGE_FAILURE_INDEX
        );
        gear.rename_loadout(LOADOUT, "HiveshadowMMO".to_owned());
        gear.save_to_loadout(LOADOUT, vec![(0, 150)]);
        let saved = gear.inventory();
        let set = &saved.loadouts[usize::from(LOADOUT) - 1];
        assert_eq!(set.name, "HiveshadowMMO");
        assert_eq!(set.items.len(), 2, "the chest and the staff");
        assert_eq!(set.attributes, [(0, 150)]);
        assert_eq!(
            gear.clear_loadout(LOADOUT),
            Some(loadout_status::UNEQUIP_ALL_UPDATE_LINKED)
        );
        assert_eq!(gear.clear_loadout(0), None, "loadout 0 is the worn items");
    }

    #[test]
    fn a_bag_grows_a_slot_for_a_new_stack_and_a_split_has_its_own_id() {
        let kits = kits();
        let mut gear = gear();
        let free = gear.bag.iter().filter(|entry| entry.is_none()).count();
        let potion = kits.item(POTION).expect("potion");
        for _ in 0..free {
            let filler = gear.new_stack(potion, 1);
            assert!(gear.put_in_bag(filler));
        }
        let before = gear.bag.len();
        assert!(gear.add(kits.item(NEW_CHEST).expect("chest"), 1));
        assert_eq!(gear.bag.len(), before + 1);
        let ids: BTreeSet<[u8; RAW_ID_LEN]> =
            gear.stacks().map(|stack| stack.item.instance).collect();
        assert_eq!(ids.len(), gear.stacks().count(), "one id per stack");
    }
}
