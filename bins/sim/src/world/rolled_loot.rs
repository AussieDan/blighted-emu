// SPDX-License-Identifier: AGPL-3.0-or-later
//! Items with rolls of their own (run-15 items2 study): a dropped gear item rolls its gear
//! score from its loot entry, the player's level and the killed mob's level
//! ([`nw_content::drop_rolls`]) and its perks from its perk buckets
//! ([`nw_content::item_rolls`]); a crafted one comes with its gear score. Each rolled item is a
//! stack of its own in the bag. Every item a player gets goes through [`SimWorld::give_drops`].

use std::collections::BTreeMap;
use std::sync::Arc;

use nw_bus::ItemState;
use nw_content::item_rolls::RolledItem;
use nw_content::loot::DropGearScore;

use super::gear::MovedStack;
use super::loot_bag::TakenItem;
use super::{Player, SimWorld};
use crate::objectives::Event;

/// One item stack given and its own values (`None`: an item of its data values).
pub(super) type RolledDrop = TakenItem;

/// A drop's items and the own values of each entry: (item id, quantity) pairs and their values.
pub(super) type DropEntries = (Vec<(String, u32)>, Vec<Option<ItemState>>);

impl SimWorld {
    /// The rolls of the `items` player `number` gets (from a kill of `mob`, when one), with the
    /// gear score `hints` of the loot roll by item id: an item with rolls of its own becomes one
    /// entry per item, each with its gear score and perks; another stays one entry without
    /// rolls.
    pub(super) fn roll_drops(
        &mut self,
        number: u64,
        mob: Option<u64>,
        items: Vec<(String, u32)>,
        hints: &BTreeMap<String, DropGearScore>,
    ) -> DropEntries {
        let content = Arc::clone(&self.content);
        let player_level = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Player>(entity))
            .map(|player| content.progression.level_of(player.experience));
        let mob_level = mob.and_then(|mob| self.mobs.level(mob));
        let plain = DropGearScore::default();
        let dice = &mut self.dice;
        let mut below = |n: u32| dice.below(n);
        let mut entries = Vec::new();
        let mut rolls = Vec::new();
        for (item_id, quantity) in items {
            let Some(row) = content.item_rolls.item(&item_id).filter(|row| row.rolls()) else {
                entries.push((item_id, quantity));
                rolls.push(None);
                continue;
            };
            let hint = hints.get(&item_id).unwrap_or(&plain);
            for _ in 0..quantity {
                let wanted =
                    content
                        .drop_rolls
                        .gear_score(hint, row, player_level, mob_level, &mut below);
                let rolled = content
                    .item_rolls
                    .roll(&item_id, wanted, &hint.perks, &mut below);
                entries.push((item_id.clone(), 1));
                rolls.push(Some(rolled_values(rolled)));
            }
        }
        (entries, rolls)
    }

    /// Player `number` gets `items`, each with its own values (`None`: a plain item), in a bag
    /// change of sync reason `reason`, at `now_ms`: each into its bag, and its quest counts the
    /// items that fit. What does not fit (a full bag) is not lost: it drops in a bag of dropped
    /// items at the player's feet, which it takes when it has room (Inferred: what retail does
    /// with a full bag is Unknown). Whether every item went into the bag (an item without a
    /// master row is never given).
    pub(super) fn give_drops(
        &mut self,
        number: u64,
        items: Vec<RolledDrop>,
        reason: u8,
        now_ms: u64,
    ) -> bool {
        let Some(entity) = self.player_by_number(number) else {
            return false;
        };
        let content = Arc::clone(&self.content);
        let mut gained: Vec<(String, u32)> = Vec::new();
        let mut left: Vec<MovedStack> = Vec::new();
        let mut unknown = false;
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            for ((item_id, quantity), values) in items {
                let Some(item) = content.kits.item(&item_id) else {
                    tracing::warn!(item = %item_id, "an item without a master row is not given");
                    unknown = true;
                    continue;
                };
                let added = player.gear.as_mut().is_some_and(|gear| {
                    gear.put_moved(&content.kits, item, quantity, values.as_ref(), reason)
                });
                if added {
                    match gained.iter_mut().find(|(gained, _)| *gained == item_id) {
                        Some((_, total)) => *total = total.saturating_add(quantity),
                        None => gained.push((item_id, quantity)),
                    }
                } else {
                    left.push(MovedStack {
                        item_id,
                        quantity,
                        values,
                    });
                }
            }
        }
        let all = left.is_empty() && !unknown;
        if !all {
            tracing::info!(
                player = number,
                items = left.len(),
                "a full bag: items dropped at the player's feet"
            );
            self.drop_items(entity, number, left, now_ms);
        }
        for (item, quantity) in &gained {
            tracing::debug!(player = number, item = %item, quantity, "items gained");
            self.quest_event(
                number,
                Event::ItemGained {
                    item,
                    quantity: *quantity,
                },
                now_ms,
            );
        }
        all
    }
}

/// The own values of a newly rolled item: its gear score and perks; its other values are the
/// item's data values.
pub(super) fn rolled_values(rolled: RolledItem) -> ItemState {
    ItemState {
        gear_score: Some(rolled.gear_score),
        perks: Some(rolled.perks),
        ..ItemState::default()
    }
}
