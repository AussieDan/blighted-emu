// SPDX-License-Identifier: AGPL-3.0-or-later
//! Items that come with values of their own (run-15 items2 study): a crafted gear item at the
//! gear score the player picked, a dropped gear item with its rolled perks, and a stack another
//! player traded or sold, with its durability, bind state and look. Each such item is one new
//! stack: two items of different values never share a stack.

use std::collections::BTreeMap;

use nw_bus::ItemState;
use nw_content::starter::{KitItem, StarterKits};

use super::holdings::MAX_CONTAINER_SLOTS;
use super::{Gear, MAX_STACK_QUANTITY};

/// One entry to put into the bag: its item, quantity and whether it keeps values of its own (a
/// stack of its own).
pub(in crate::world) type BagEntry<'a> = (&'a KitItem, u32, bool);

impl Gear {
    /// Puts one new stack of `quantity` of `item` with its own `values` (its instance id unless
    /// another stack holds it, gear score, durability, perks, bind state and look) into the bag;
    /// whether it fit.
    pub(in crate::world) fn add_with_values(
        &mut self,
        kits: &StarterKits,
        item: &KitItem,
        quantity: u32,
        values: &ItemState,
        reason: u8,
    ) -> bool {
        if quantity == 0 {
            return false;
        }
        let restored = self.restored_stack(kits, item, quantity.min(MAX_STACK_QUANTITY), values);
        let stack = self.own_instance(restored);
        let added = self.put_in_bag_as(stack, reason);
        if added {
            self.changed = true;
            self.unsaved = true;
        }
        added
    }

    /// Whether the bag takes every one of `entries` at once: a plain entry fills the room left
    /// in the stacks of its item (up to its `MaxStackSize`) and then new stacks; an entry with
    /// values of its own takes a new stack. New stacks take the free slots, then new slots up
    /// to [`MAX_CONTAINER_SLOTS`].
    pub(in crate::world) fn has_room_for(&self, entries: &[BagEntry<'_>]) -> bool {
        let limit_of = |item: &KitItem| {
            item.max_stack
                .unwrap_or(MAX_STACK_QUANTITY)
                .clamp(1, MAX_STACK_QUANTITY)
        };
        let mut spare: BTreeMap<&str, u32> = BTreeMap::new();
        let mut new_stacks = 0_usize;
        for (item, quantity, own) in entries {
            if *quantity == 0 {
                continue;
            }
            if *own {
                new_stacks += 1;
                continue;
            }
            let limit = limit_of(item);
            let room = spare.entry(item.item_id.as_str()).or_insert_with(|| {
                self.bag
                    .iter()
                    .flatten()
                    .filter(|stack| stack.item_id == item.item_id)
                    .map(|stack| limit.saturating_sub(stack.item.quantity))
                    .fold(0, u32::saturating_add)
            });
            let filled = (*room).min(*quantity);
            *room -= filled;
            let left = quantity - filled;
            let stacks = left.div_ceil(limit);
            new_stacks += usize::try_from(stacks).unwrap_or(usize::MAX);
            *room = stacks.saturating_mul(limit) - left;
        }
        let free = self.bag.iter().filter(|entry| entry.is_none()).count()
            + MAX_CONTAINER_SLOTS.saturating_sub(self.bag.len());
        new_stacks <= free
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{BACKSTORY, CHARACTER, FLINT, kits};
    use super::*;
    use nw_bus::ItemLookState;
    use nw_messages::items::SYNC_MOVED;

    const OWN_GEAR_SCORE: u32 = 425;
    const OWN_DURABILITY: u32 = 3;
    const PERK: &str = "PerkID_Weapon_DmgCrit";

    #[test]
    fn an_item_with_own_values_is_a_stack_of_its_own_with_them() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let flint = kits.item(FLINT).expect("flint").clone();
        let values = ItemState {
            instance: Some([7; nw_messages::wire::RAW_ID_LEN]),
            gear_score: Some(OWN_GEAR_SCORE),
            durability: Some(OWN_DURABILITY),
            perks: Some(vec![PERK.to_owned()]),
            bound: Some(true),
            look: ItemLookState {
                dye: [1, 2, 3, 4],
                ..ItemLookState::default()
            },
            ..ItemState::default()
        };
        assert!(gear.add_with_values(&kits, &flint, 1, &values, SYNC_MOVED));
        assert!(gear.add_with_values(&kits, &flint, 1, &values, SYNC_MOVED));
        assert!(!gear.add_with_values(&kits, &flint, 0, &values, SYNC_MOVED));
        let stacks: Vec<_> = gear
            .bag
            .iter()
            .flatten()
            .filter(|stack| stack.item.gear_score == OWN_GEAR_SCORE)
            .collect();
        assert_eq!(stacks.len(), 2, "items with own values do not stack");
        assert!(stacks.iter().all(|stack| stack.perks == [PERK]
            && stack.item.durability == OWN_DURABILITY.min(stack.item.max_durability)
            && stack.item.bound
            && stack.item.look.dye == [1, 2, 3, 4]));
        assert_eq!(stacks[0].item.instance, [7; nw_messages::wire::RAW_ID_LEN]);
        assert_ne!(
            stacks[0].item.instance, stacks[1].item.instance,
            "one id names one stack"
        );
        assert_eq!(gear.take_unsaved().map(|items| items.len() > 1), Some(true));
    }

    #[test]
    fn the_room_check_counts_stack_room_and_new_slots() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let flint = kits.item(FLINT).expect("flint").clone();
        assert!(gear.has_room_for(&[(&flint, 1, false), (&flint, 1, true)]));
        while gear.bag.len() < MAX_CONTAINER_SLOTS || gear.bag.iter().any(Option::is_none) {
            assert!(gear.add_with_values(&kits, &flint, 1, &ItemState::default(), SYNC_MOVED));
        }
        assert!(!gear.has_room_for(&[(&flint, 1, true)]), "no slot is left");
        assert!(gear.has_room_for(&[]), "nothing always fits");
    }
}
