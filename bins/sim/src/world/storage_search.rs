// SPDX-License-Identifier: AGPL-3.0-or-later
//! The storage search items (7048) that go with each change of a player's items.
//!
//! Retail sends one 7048 with no search and no items before the bundle of a change of the
//! player's items (tutorial-quest note, section 10). Newchar-retail-2026-09-18 (106 sends):
//! 5 around the first card, whose gear is five worn starter items; one within 30 ms after each
//! equip (11 of 11 `ServerEquipItem`) and each quest turn-in (6 of 6
//! `RequestCompleteObjective`); 2 after the take-all of a loot bag (2939) and 3 after the end of
//! a chest's use (3746). Inferred: one per item whose stacks changed. Which items the two loot
//! cases changed is not in the decoded members, so whether their counts follow that rule is
//! Unknown. The storage chests count apart: retail `storage_open_deposit_withdraw_transfer`
//! sent 2 after each of its 7 deposits (2801) and 5 takes (2942) of one stack, the item's
//! change in the bag and in the storage. Retail also sent 1 to 3 after each of its 7 storage
//! views from afar (1512), which move no item, and 2 at the chest's open; what those count is
//! Unknown and this server sends none for them.
//!
//! **Searches (run-15 items2 study, retail MSQ16).** A new search (7044, `refresh` 0) is
//! answered with 7046: every item of the player's storage chests that an ingredient of a
//! recipe of the searched station takes (an `Item` ingredient by id, a `Category_Only` one by
//! the item's `IngredientCategories`; 14 of 14 retail results fit) with its total over all
//! storages. A refresh comes in bursts of up to 20 in one tick and retail answered each burst
//! once (260 requests, 14 answers). A 7039 asks for the last results again (best guess: retail
//! sent 7047, then 7046 after the client's 7039 in 3 of 7 searches; this server always answers
//! 7044 at once, as retail did in the other 4). When the storages' totals of listed items change,
//! a 7048 with the search id lists those results' indexes and new totals and the count of the
//! results (retail 4 to 10 s after the change; this server sends it with the change).
//!
//! **The last search goes with every item change (retail crafting ledgers).** From a player's
//! first search on, each item-change 7048 carries its last search id and that search's result
//! count, also after the station closed: `Refining` 15.962 s (search of 11.583 s, 21 results),
//! `refining_all_stations_single_batch_territorylvlup` 159.490 s (3, 33 s after the close) and
//! 224.075 s (a salvage), `Hive_Dye` 233.387 s; before the first search, none (the same ledgers
//! at login). A change of the search's stored totals goes in the same message, not one more:
//! `Craft-Gypsum-Orbs` 54.233 s and `Refining` 98.805 s sent one 7048 that listed the changed
//! result. Which of several 7048s lists them is Unknown (no retail sample); this server lists
//! them in the first. **A craft counts the items it made** ([`ItemChanges::absorb`]): one 7048
//! per craft in 44 of 44 retail crafts of one output kind (seven ledgers), though each changed
//! the ingredient too; `Refining` 77.255 s sent 2 for a craft that also gave a bonus item of
//! another kind.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::{CarriedItem, ItemPlace};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::item_calls::SEARCH_FILTER_HOUSING;
use nw_messages::storage_search::SEARCH_READY;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    SearchedItem, ServerMessage, StorageItemSimple, StorageSearchItemsMsg, StorageSearchReadyMsg,
    StorageSearchResultsMsg, global_storage_header,
};

use super::{Player, SimWorld, send_now};

/// The station of a search that names none (the housing and dye screens).
const NO_STATION: u32 = 0;

/// The stacks of one item: each place and quantity, in order.
type Stacks = Vec<(ItemPlace, u32)>;
/// A stored stack: its item id, storage chest, slot there and quantity.
pub(super) type StoredStack = (String, [u8; RAW_ID_LEN], usize, u32);
/// The stored stacks of one item: each storage chest, slot and quantity, in order.
type StoredStacks = Vec<([u8; RAW_ID_LEN], usize, u32)>;

/// The items a player held at its last change, and its last storage search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ItemChanges {
    held: BTreeMap<String, Stacks>,
    /// The stored stacks; `None` before the first call.
    stored: Option<BTreeMap<String, StoredStacks>>,
    /// The player's last storage search; `None` before its first.
    search: Option<StorageSearch>,
}

/// How many items differ between `before` and `after`: an item counts once when it came, went
/// or its stacks changed.
fn changed_items<V: PartialEq>(before: &BTreeMap<String, V>, after: &BTreeMap<String, V>) -> usize {
    after
        .iter()
        .filter(|(item, stacks)| before.get(*item) != Some(*stacks))
        .count()
        + before
            .keys()
            .filter(|item| !after.contains_key(*item))
            .count()
}

impl ItemChanges {
    /// How many items changed from the last call to `carried` and `stored`: an item counts once
    /// for its carried stacks and once for its stored stacks when they came, went, moved or
    /// changed quantity. The first call counts every carried item and no stored one (Inferred:
    /// the retail new character's 5 at its first card were its 5 worn items, with no storage).
    pub(super) fn changes(&mut self, carried: &[CarriedItem], stored: &[StoredStack]) -> usize {
        let held = stacks_by_item(carried);
        let kept = stored_by_item(stored);
        let changed = changed_items(&self.held, &held)
            + self
                .stored
                .as_ref()
                .map_or(0, |before| changed_items(before, &kept));
        self.held = held;
        self.stored = Some(kept);
        changed
    }

    /// The 7048s of a change of the items of the player of entity number `number` to `carried`
    /// and `stored`, whose stored totals by item id are `totals`: one per changed item
    /// ([`ItemChanges::changes`]), each with the last search's id and result count; the first
    /// lists the search's results whose totals changed (module notes).
    pub(super) fn messages(
        &mut self,
        number: u64,
        carried: &[CarriedItem],
        stored: &[StoredStack],
        totals: &BTreeMap<String, u32>,
    ) -> Vec<ServerMessage> {
        let count = self.changes(carried, stored);
        if count == 0 {
            return Vec::new();
        }
        let blank = StorageSearchItemsMsg::item_change(number);
        let Some(search) = self.search.as_mut() else {
            return vec![ServerMessage::StorageSearchItems(blank); count];
        };
        let mut items = search.changes(totals);
        let total = u32::try_from(search.results.len()).unwrap_or(u32::MAX);
        (0..count)
            .map(|_| {
                ServerMessage::StorageSearchItems(StorageSearchItemsMsg {
                    search: search.search,
                    items: std::mem::take(&mut items),
                    total,
                    ..blank.clone()
                })
            })
            .collect()
    }

    /// Takes the stacks of the items `item_ids` in `carried` and `stored` as known: a change of
    /// them so far counts for no 7048 (a craft's used ingredients, module notes).
    pub(super) fn absorb(
        &mut self,
        item_ids: &[&str],
        carried: &[CarriedItem],
        stored: &[StoredStack],
    ) {
        let mut held = stacks_by_item(carried);
        let mut kept = stored_by_item(stored);
        for item_id in item_ids {
            match held.remove(*item_id) {
                Some(stacks) => self.held.insert((*item_id).to_owned(), stacks),
                None => self.held.remove(*item_id),
            };
            if let Some(known) = self.stored.as_mut() {
                match kept.remove(*item_id) {
                    Some(stacks) => known.insert((*item_id).to_owned(), stacks),
                    None => known.remove(*item_id),
                };
            }
        }
    }
}

/// The stored stacks of `stored` by item id, each item's stacks in storage and slot order.
fn stored_by_item(stored: &[StoredStack]) -> BTreeMap<String, StoredStacks> {
    let mut kept: BTreeMap<String, StoredStacks> = BTreeMap::new();
    for (item_id, storage, slot, quantity) in stored {
        kept.entry(item_id.clone())
            .or_default()
            .push((*storage, *slot, *quantity));
    }
    for stacks in kept.values_mut() {
        stacks.sort_unstable();
    }
    kept
}

/// The stacks of `carried` by item id, each item's stacks in place order.
fn stacks_by_item(carried: &[CarriedItem]) -> BTreeMap<String, Stacks> {
    let mut held: BTreeMap<String, Stacks> = BTreeMap::new();
    for item in carried {
        held.entry(item.item_id.clone())
            .or_default()
            .push((item.place, item.quantity));
    }
    for stacks in held.values_mut() {
        stacks.sort_unstable_by_key(|(place, quantity)| (place_order(*place), *quantity));
    }
    held
}

/// A sort key of `place`: worn slots, then bag slots.
fn place_order(place: ItemPlace) -> (u8, u32) {
    match place {
        ItemPlace::Worn(slot) => (0, slot),
        ItemPlace::Bag(slot) => (1, u32::from(slot)),
    }
}

/// What a storage search looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchFor {
    /// The ingredients of the recipes of the station whose lowercase name without tier digits
    /// has this CRC-32.
    Station(u32),
    /// Housing items: every stored item that is furniture of the housing tables (Inferred from
    /// the filter's name: no retail capture has a housing search).
    Housing,
    /// A filter whose meaning is Unknown: no results.
    Unknown,
}

impl SearchFor {
    /// What a 7044 with `filter` and `station` looks for.
    pub(super) const fn of(filter: u32, station: u32) -> Self {
        if station != NO_STATION {
            Self::Station(station)
        } else if filter == SEARCH_FILTER_HOUSING {
            Self::Housing
        } else {
            Self::Unknown
        }
    }
}

/// A player's last storage search: its id and the results last sent (item id and total).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct StorageSearch {
    search: [u8; RAW_ID_LEN],
    results: Vec<(String, u32)>,
    /// When the last answer went: a refresh in the same tick is not answered again.
    answered_ms: Option<u64>,
}

impl StorageSearch {
    /// The results as sent.
    fn items(&self) -> Vec<StorageItemSimple> {
        self.results
            .iter()
            .map(|(item_id, total)| StorageItemSimple {
                item: lowercase_crc32(item_id),
                quantity: *total,
            })
            .collect()
    }

    /// The results whose totals differ in `totals` (by item id; a missing item has none): each
    /// one's index and new total, which the search keeps.
    fn changes(&mut self, totals: &BTreeMap<String, u32>) -> Vec<SearchedItem> {
        let mut changed = Vec::new();
        for (index, (item_id, total)) in (0_u32..).zip(&mut self.results) {
            let now = totals.get(item_id).copied().unwrap_or_default();
            if now != *total {
                *total = now;
                changed.push(SearchedItem {
                    index,
                    item: StorageItemSimple {
                        item: lowercase_crc32(item_id),
                        quantity: now,
                    },
                });
            }
        }
        changed
    }
}

/// The results of a search: the items of `totals` (item id, total over the storages) that
/// `wanted` takes, in item id order, with a total above 0.
fn search_results(
    totals: &BTreeMap<String, u32>,
    wanted: impl Fn(&str) -> bool,
) -> Vec<(String, u32)> {
    totals
        .iter()
        .filter(|(item_id, total)| **total > 0 && wanted(item_id))
        .map(|(item_id, total)| (item_id.clone(), *total))
        .collect()
}

impl SimWorld {
    /// Player `number` (`entity`) searches its storages (7044) at `now_ms` for `what` with
    /// search id `search`; a refresh answered in this tick already is not answered again.
    pub(super) fn search_storage(
        &mut self,
        entity: Entity,
        number: u64,
        search: [u8; RAW_ID_LEN],
        what: SearchFor,
        refresh: bool,
        now_ms: u64,
    ) {
        let content = Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let answered = player
            .item_changes
            .search
            .as_ref()
            .is_some_and(|last| last.search == search && last.answered_ms == Some(now_ms));
        if refresh && answered {
            return;
        }
        let connection = player.connection.clone();
        let totals = player
            .gear
            .as_ref()
            .map(super::gear::Gear::stored_totals)
            .unwrap_or_default();
        let results = match what {
            SearchFor::Station(station) => {
                let ingredients: Vec<_> = content.recipes.ingredients_at(station).collect();
                search_results(&totals, |item_id| {
                    ingredients
                        .iter()
                        .any(|ingredient| content.recipes.fits(ingredient, item_id))
                })
            }
            SearchFor::Housing => search_results(&totals, |item_id| {
                content
                    .kits
                    .item(item_id)
                    .is_some_and(|item| content.travel.furniture(item.item_index).is_some())
            }),
            SearchFor::Unknown => Vec::new(),
        };
        let state = StorageSearch {
            search,
            results,
            answered_ms: Some(now_ms),
        };
        let answer = ServerMessage::StorageSearchResults(StorageSearchResultsMsg {
            header: global_storage_header(number),
            search,
            items: state.items(),
        });
        player.item_changes.search = Some(state);
        send_now(&mut self.combat_messages, &connection, &[answer]);
    }

    /// Player `number` (`entity`) asks for the results of its last search again (7039):
    /// the ready note (7047) goes first, then the results (7046), as retail sent them in
    /// 3 of 7 searches.
    pub(super) fn replicate_search(&mut self, entity: Entity, number: u64) {
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let Some(last) = player.item_changes.search.as_ref() else {
            return;
        };
        let ready = ServerMessage::StorageSearchReady(StorageSearchReadyMsg {
            header: global_storage_header(number),
            search: last.search,
            ready: SEARCH_READY,
        });
        let answer = ServerMessage::StorageSearchResults(StorageSearchResultsMsg {
            header: global_storage_header(number),
            search: last.search,
            items: last.items(),
        });
        let connection = player.connection.clone();
        send_now(&mut self.combat_messages, &connection, &[ready, answer]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWORD: &str = "1hSwordT2_FTUE";
    const CHEST: &str = "SwordsmanArchetypeChest";
    const POTION: &str = "PotionHealthT1";
    const MAIN_HAND: u32 = 24;
    const CHEST_SLOT: u32 = 1;

    fn item(item_id: &str, quantity: u32, place: ItemPlace) -> CarriedItem {
        CarriedItem {
            item_id: item_id.to_owned(),
            quantity,
            place,
            state: nw_bus::ItemState::default(),
        }
    }

    #[test]
    fn a_search_lists_the_wanted_stored_items_and_their_changes() {
        const INGOT: &str = "IngotT2";
        const ORE: &str = "OreT1";
        const SEARCH: [u8; RAW_ID_LEN] = [7; RAW_ID_LEN];
        let totals = BTreeMap::from([
            (INGOT.to_owned(), 12),
            (ORE.to_owned(), 40),
            ("FlintT1".to_owned(), 3),
        ]);
        let results = search_results(&totals, |item| item != "FlintT1");
        assert_eq!(results, [(INGOT.to_owned(), 12), (ORE.to_owned(), 40)]);
        let mut search = StorageSearch {
            search: SEARCH,
            results,
            answered_ms: None,
        };
        assert_eq!(
            search.items()[1],
            StorageItemSimple {
                item: lowercase_crc32(ORE),
                quantity: 40,
            }
        );
        assert!(search.changes(&totals).is_empty());
        let mut after = totals;
        after.insert(ORE.to_owned(), 39);
        after.remove(INGOT);
        let changes = search.changes(&after);
        assert_eq!(
            changes
                .iter()
                .map(|change| (change.index, change.item.quantity))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 39)]
        );
        assert!(search.changes(&after).is_empty(), "kept");
        assert_eq!(
            SearchFor::of(0, 0x2294_152c),
            SearchFor::Station(0x2294_152c)
        );
        assert_eq!(
            SearchFor::of(SEARCH_FILTER_HOUSING, NO_STATION),
            SearchFor::Housing
        );
        assert_eq!(SearchFor::of(1, NO_STATION), SearchFor::Unknown);
    }

    #[test]
    fn the_first_call_counts_every_item() {
        let mut changes = ItemChanges::default();
        let worn = [
            item(SWORD, 1, ItemPlace::Worn(MAIN_HAND)),
            item(CHEST, 1, ItemPlace::Worn(CHEST_SLOT)),
        ];
        assert_eq!(changes.changes(&worn, &[]), 2);
        assert_eq!(changes.changes(&worn, &[]), 0, "nothing changed");
    }

    #[test]
    fn a_move_a_gain_a_quantity_and_a_removal_count_once_each() {
        let mut changes = ItemChanges::default();
        changes.changes(
            &[
                item(SWORD, 1, ItemPlace::Bag(0)),
                item(POTION, 1, ItemPlace::Bag(1)),
            ],
            &[],
        );
        assert_eq!(
            changes.changes(
                &[
                    item(SWORD, 1, ItemPlace::Worn(MAIN_HAND)),
                    item(POTION, 1, ItemPlace::Bag(1)),
                ],
                &[]
            ),
            1,
            "an equip moves one item"
        );
        assert_eq!(
            changes.changes(
                &[
                    item(SWORD, 1, ItemPlace::Worn(MAIN_HAND)),
                    item(POTION, 2, ItemPlace::Bag(1)),
                    item(CHEST, 1, ItemPlace::Bag(2)),
                ],
                &[]
            ),
            2,
            "a new quantity and a new item"
        );
        assert_eq!(
            changes.changes(
                &[
                    item(POTION, 2, ItemPlace::Bag(1)),
                    item(CHEST, 1, ItemPlace::Bag(2)),
                ],
                &[]
            ),
            1,
            "a removal"
        );
    }

    /// The 7048s of `sent`.
    fn search_items(sent: &[ServerMessage]) -> Vec<&StorageSearchItemsMsg> {
        sent.iter()
            .filter_map(|message| match message {
                ServerMessage::StorageSearchItems(items) => Some(items),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_item_change_carries_the_last_search_and_the_first_its_changed_results() {
        // Retail `refining_all_stations_single_batch_territorylvlup`: none before the first
        // search (0.472 s), the search after its station closed (159.490 s);
        // `Craft-Gypsum-Orbs` 54.233 s: the changed result in the change's own 7048.
        const NUMBER: u64 = 1;
        const INGOT: &str = "IngotT2";
        const ORE: &str = "OreT1";
        const SEARCH: [u8; RAW_ID_LEN] = [7; RAW_ID_LEN];
        const STORAGE: [u8; RAW_ID_LEN] = [3; RAW_ID_LEN];
        let mut changes = ItemChanges::default();
        let sent = changes.messages(
            NUMBER,
            &[item(POTION, 1, ItemPlace::Bag(0))],
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            search_items(&sent),
            [&StorageSearchItemsMsg::item_change(NUMBER)],
            "no search yet"
        );
        changes.search = Some(StorageSearch {
            search: SEARCH,
            results: vec![(INGOT.to_owned(), 12), (ORE.to_owned(), 40)],
            answered_ms: None,
        });
        let sent = changes.messages(
            NUMBER,
            &[item(POTION, 2, ItemPlace::Bag(0))],
            &[(ORE.to_owned(), STORAGE, 0, 39)],
            &BTreeMap::from([(ORE.to_owned(), 39)]),
        );
        let sent = search_items(&sent);
        assert_eq!(sent.len(), 2, "the potion and the stored ore");
        assert!(
            sent.iter()
                .all(|items| items.search == SEARCH && items.total == 2)
        );
        assert_eq!(
            sent[0]
                .items
                .iter()
                .map(|changed| (changed.index, changed.item.quantity))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 39)]
        );
        assert!(sent[1].items.is_empty(), "listed once");
    }

    #[test]
    fn an_absorbed_change_counts_for_no_item_change() {
        let mut changes = ItemChanges::default();
        changes.changes(&[item(POTION, 3, ItemPlace::Bag(0))], &[]);
        let after = [
            item(POTION, 1, ItemPlace::Bag(0)),
            item(SWORD, 1, ItemPlace::Bag(1)),
        ];
        changes.absorb(&[POTION], &after, &[]);
        assert_eq!(changes.changes(&after, &[]), 1, "the sword only");
    }

    #[test]
    fn a_deposit_and_a_take_count_twice_as_retail() {
        const FLINT: &str = "FlintT1";
        const STORAGE: [u8; RAW_ID_LEN] = [3; RAW_ID_LEN];
        const STORED: [u8; RAW_ID_LEN] = [4; RAW_ID_LEN];
        let mut changes = ItemChanges::default();
        let bag = [item(FLINT, 5, ItemPlace::Bag(0))];
        assert_eq!(
            changes.changes(&bag, &[(FLINT.to_owned(), STORED, 0, 7)]),
            1,
            "the first call counts no stored item"
        );
        assert_eq!(
            changes.changes(
                &[],
                &[
                    (FLINT.to_owned(), STORED, 0, 7),
                    (FLINT.to_owned(), STORAGE, 0, 5)
                ]
            ),
            2
        );
        assert_eq!(
            changes.changes(&bag, &[(FLINT.to_owned(), STORED, 0, 7)]),
            2
        );
        assert_eq!(
            changes.changes(&bag, &[(FLINT.to_owned(), STORED, 0, 7)]),
            0,
            "nothing moved"
        );
    }
}
