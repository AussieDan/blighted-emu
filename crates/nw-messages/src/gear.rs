// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gear states of a player (paperdoll-inventory note, sections 2, 9 and 12): the paperdoll
//! (3183), the bag and the second container (1755, components 61 and 76) and the owned items
//! (5437), as full states built from the worn items and the bag. The shapes follow the retail
//! first states of a new character ("Soul Survivor" capture). A later change of the items sends
//! the children that changed, a list as its changed elements ([`PlayerGear::changes`]; retail
//! newchar 2026-09-18: every equip, take and use). A change of the hands alone sends only the
//! hand children that changed ([`PlayerGear::hands_change`]).

use crate::containers::{ContainerState, ItemManagementState, OwnedItem, OwnedItems};
use crate::items::{
    DYE_BYTES, FIRST_VERSION, FullList, ITEM_BOUND, ITEM_LOCKED, ItemRecord, PERK_SLOTS,
    SYNC_LOGIN, SYNC_OWNED, SYNC_REMOVED,
};
use crate::list_delta::ReplicatedList;
use crate::paperdoll::{
    ItemVisual, LoadedAmmo, Loadout, PaperdollOwner, PaperdollPublic, PaperdollState, SlotBits,
    slot,
};
use crate::wire::RAW_ID_LEN;

/// Bag slots of a new character: the retail tutorial bag held 5 empty slots at entry.
pub const STARTER_BAG_SLOTS: usize = 5;
/// The name of the first loadout, as retail sends it.
const FIRST_LOADOUT_NAME: &[u8] = b"@lo_name 0";
/// Sync reason of an item moved into a bag slot (paperdoll note, section 10.3).
const SYNC_MOVED_IN: u8 = 6;

/// How an item looks beyond its item index (the item serializer's `m_dyeSettings`,
/// `m_appearanceId` and `m_skinDyeSettings`; run-15 items2 study).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemLook {
    /// The `DyeColorData` index of each dye channel (R, G, B, A); 0 is no dye.
    pub dye: [u8; DYE_BYTES],
    /// The skin applied over it: the CRC-32 of the lowercase appearance id; 0 is none.
    pub appearance: u32,
    /// The applied skin's dye.
    pub skin_dye: [u8; DYE_BYTES],
}

impl ItemLook {
    /// How other players see an item of `item_index` with this look: the dye only when one is
    /// set (the visual's dye flag), the skin and its dye.
    pub fn visual(self, item_index: u32) -> ItemVisual {
        ItemVisual {
            dye: (self.dye != [0; DYE_BYTES]).then_some(self.dye),
            transmog: self.appearance,
            transmog_dye: self.skin_dye,
            ..ItemVisual::of(item_index)
        }
    }
}

/// One item of a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GearItem {
    /// The client item index.
    pub item_index: u32,
    /// Stack size.
    pub quantity: u32,
    /// Gear score.
    pub gear_score: u32,
    /// Current durability.
    pub durability: u32,
    /// Full durability (`MasterItemDefinitions` `Durability`; retail sends it on every record).
    pub max_durability: u32,
    /// Its perks as wire perk indexes (0 is an empty slot), filled from slot 0 with no gaps.
    pub perks: [u32; PERK_SLOTS],
    /// Its dye and applied skin.
    pub look: ItemLook,
    /// The character's item counter when the item came (retail: rises with each new item of
    /// the character and stays with the item).
    pub counter: u32,
    /// Whether the item is bound to the character.
    pub bound: bool,
    /// Whether its owner locked it ([`ITEM_LOCKED`]).
    pub locked: bool,
    /// The item instance id.
    pub instance: [u8; RAW_ID_LEN],
}

/// One worn item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EquippedItem {
    /// The paperdoll slot id ([`slot`]).
    pub slot: u32,
    /// Its owned-items key, which the loadouts name. Retail keeps a key while the item is worn
    /// and gives a new item the lowest free key (newchar 2026-09-18: the greatsword took the
    /// removed sword's key 4, the next items 6, 7 and 8, a chest put on while key 0 was worn
    /// key 11 and the next boots the freed key 0).
    pub key: u16,
    /// The item; `None`: a visible slot whose item was removed. Retail keeps such a slot in the
    /// visible lists with an empty record and visual 0, out of the owned items and the loadout,
    /// until an item fills it again (retail 2026-09-18: the tutorial sword; a client given a
    /// shorter list kept the sword in the hand).
    pub item: Option<GearItem>,
}

/// One worn item as other players see it (the public paperdoll): its slot, its client item
/// index (0 for a visible slot whose item was removed) and its look. Its stack, gear score,
/// durability, bind flag and instance stay its owner's (Observed: every retail remote paperdoll
/// of the duel and the 3v3 arena sets slot 2 alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownItem {
    /// The paperdoll slot id ([`slot`]).
    pub slot: u32,
    /// The client item index; 0: the slot's item was removed.
    pub item_index: u32,
    /// Its dye and applied skin.
    pub look: ItemLook,
}

impl ShownItem {
    /// How `worn` shows to other players.
    pub fn of(worn: &EquippedItem) -> Self {
        Self {
            slot: worn.slot,
            item_index: worn.item.map_or(0, |item| item.item_index),
            look: worn.item.map(|item| item.look).unwrap_or_default(),
        }
    }
}

/// Whether paperdoll slot `slot_id` shows on the character: the armor and the weapon options.
/// Other slots (amulet, ring, quick slots, ...) are the paperdoll's hidden items (retail: the
/// amulet in slot 5 and potions in slot 14).
pub const fn is_visible_slot(slot_id: u32) -> bool {
    slot_id <= slot::FEET
        || slot::MAIN_HAND_OPTION_1 <= slot_id && slot_id <= slot::OFF_HAND_OPTION_3
}

/// The gear states of one player.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerGear {
    /// Type 3183, component 62.
    pub paperdoll: PaperdollState,
    /// Type 1755, component 61.
    pub bag: ContainerState,
    /// Type 1755, component 76.
    pub second_container: ContainerState,
    /// Type 5437, component 2.
    pub owned: ItemManagementState,
}

/// The full list of `elements` at `version`.
const fn list<T>(version: u64, elements: Vec<T>) -> ReplicatedList<T> {
    ReplicatedList::Full(FullList {
        version: Some(version),
        elements,
    })
}

/// The record of `item`, sent for `sync_reason`.
pub fn record(item: &GearItem, sync_reason: u8, container_slot: Option<u16>) -> ItemRecord {
    let flag = |set: bool, bit: u8| if set { bit } else { 0 };
    ItemRecord {
        quantity: item.quantity,
        sync_reason,
        container_slot,
        item_index: item.item_index,
        durability: item.durability,
        max_durability: item.max_durability,
        counter: item.counter,
        gear_score: item.gear_score,
        perks: item.perks,
        dye: item.look.dye,
        instance: item.instance,
        flags: flag(item.bound, ITEM_BOUND) | flag(item.locked, ITEM_LOCKED),
        transmog: item.look.appearance,
        transmog_dye: item.look.skin_dye,
    }
}

/// The records of the second container `open` (one per slot, empty ones included): each item
/// sent for its sync reason, with its slot unless the view is seen from afar, and each empty
/// slot as [`OpenContainer::empty_reason`] says.
fn container_records(open: &OpenContainer) -> Vec<ItemRecord> {
    (0_u16..)
        .zip(&open.items)
        .map(|(container_slot, item)| match item {
            Some(item) => record(
                item,
                open.sync_reason,
                open.slots_listed.then_some(container_slot),
            ),
            None => ItemRecord {
                sync_reason: open.empty_reason,
                ..ItemRecord::EMPTY_SLOT
            },
        })
        .collect()
}

/// The records of the bag `bag`, each with its slot's reason of `reasons` (else moved in).
fn bag_records(bag: &[Option<GearItem>], reasons: &[u8]) -> Vec<ItemRecord> {
    (0_u16..)
        .zip(bag)
        .map(|(container_slot, item)| {
            let reason = reasons.get(usize::from(container_slot)).copied();
            match item {
                Some(item) => record(item, reason.unwrap_or(SYNC_MOVED_IN), Some(container_slot)),
                None => ItemRecord {
                    sync_reason: reason.unwrap_or(SYNC_LOGIN),
                    ..ItemRecord::EMPTY_SLOT
                },
            }
        })
        .collect()
}

/// A container shown in the player's second container: a storage chest it uses, or a bag of
/// dropped items it opened (retail open-world `storage_open_deposit_withdraw_transfer` and
/// `item_actions`: the chest's items with transfer allowed, the bag's items).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenContainer {
    /// One entry per container slot.
    pub items: Vec<Option<GearItem>>,
    /// The sync reason its records carry: retail 6 for a storage the player stands at, 47 for
    /// a storage seen from afar (`UpdateStorageSession`, 1512), 18 for a bag of dropped items.
    pub sync_reason: u8,
    /// Whether items may go in and out (`Can Transfer items`): retail true while open, false
    /// once closed.
    pub can_transfer: bool,
    /// `Container Was Emptied`: retail true for an opened storage without items, false for a
    /// closed one (open-world `storage_open_deposit_withdraw_transfer`, `item_actions`).
    pub was_emptied: bool,
    /// Whether each item's record carries its container slot. Retail
    /// `storage_open_deposit_withdraw_transfer`: yes for a storage the player stands at (19.6 s,
    /// slot 0 after the first deposit), no for a storage seen from afar (92.5 s, after 1512).
    pub slots_listed: bool,
    /// The sync reason of an empty slot's record. Retail, same capture: 6 for a storage the
    /// player stands at (28.4 s, the slot a take emptied), 47 for a storage seen from afar
    /// (88.3 s, the two empty slots of an empty storage).
    pub empty_reason: u8,
}

/// What the gear states show besides the worn items, the bag and the hands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GearExtras {
    /// Items that are not worn but that a saved loadout names: their owned-items key and the
    /// item (retail lists them in the owned items with the worn ones).
    pub listed: Vec<(u16, GearItem)>,
    /// The saved loadouts after the first: loadout 0 is the worn items.
    pub loadouts: Vec<Loadout>,
    /// Whether the loadout panel is open (5578).
    pub loadout_panel_open: bool,
    /// The container in the second container, when one was opened.
    pub open_container: Option<OpenContainer>,
    /// The sync reason of each bag slot's record: the reason of its last change (a craft's
    /// [`crate::items::SYNC_CRAFT_USED`] or [`crate::items::SYNC_CRAFTED`]), for an empty slot
    /// the reason it was emptied with (retail: a craft's 2, a salvage's 24); a slot past its end
    /// is an item moved in, or an empty slot of [`SYNC_LOGIN`].
    pub bag_reasons: Vec<u8>,
    /// The paperdoll slots whose skin the player hid or showed again (5978), in slot order:
    /// (slot, hidden). Retail `Hive_Transmog`: a hide of slot 2 sent the entry `2, 1`, the
    /// show again `2, 0`.
    pub hidden_skins: Vec<(u32, bool)>,
}

/// The weapon a player holds and the ammo of its first two main-hand options (client-gates
/// note: retail answered each weapon key with these maps).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hands {
    /// The main-hand option slot drawn; `None`: every weapon is sheathed.
    pub drawn: Option<u32>,
    /// The main-hand option slot selected (`activeMap`), kept while every weapon is sheathed;
    /// `None`: the drawn one, else the first.
    pub active: Option<u32>,
    /// The drawn weapon is in the hand (retail 331 to 418 ms after the key).
    pub attached: bool,
    /// The off-hand option slot drawn with the main-hand weapon (a shield), in the hand with it
    /// (retail `general-other-01` at 1792964468728: the flail of slot 25 drawn with the shield
    /// of slot 27, sheathe map `0x35`; at 1792964469230 the attached map `0x0a`).
    pub off_hand: Option<u32>,
    /// The ammo of main-hand options 1 and 2; `None` before the server knows it.
    pub ammo: [Option<LoadedAmmo>; 2],
}

impl Hands {
    /// The sheathed map: every weapon slot but the drawn one and the off-hand drawn with it.
    fn sheathed(&self) -> SlotBits {
        SlotBits::of(
            std::iter::once(slot::GATHERABLE_HAND)
                .chain(slot::MAIN_HAND_OPTION_1..=slot::OFF_HAND_OPTION_3)
                .filter(|slot_id| Some(*slot_id) != self.drawn && Some(*slot_id) != self.off_hand),
        )
    }

    /// The attached map: the drawn weapon and the off-hand drawn with it, once in the hand.
    fn attached(&self) -> SlotBits {
        SlotBits::of(
            [self.drawn, self.off_hand]
                .into_iter()
                .flatten()
                .filter(|_| self.attached),
        )
    }

    /// The active map: the selected main-hand option (else the drawn one, else the first) and
    /// the first off-hand.
    fn active(&self) -> SlotBits {
        SlotBits::of([
            self.active
                .or(self.drawn)
                .unwrap_or(slot::MAIN_HAND_OPTION_1),
            slot::OFF_HAND_OPTION_1,
        ])
    }
}

impl PaperdollPublic {
    /// The public paperdoll of a player whose worn items show as `shown` (in that order) and who
    /// holds its weapons as `hands`, with lists of `version`: the hands' maps, then the slots and
    /// looks of the items in visible slots.
    pub fn of(shown: &[ShownItem], version: u64, hands: Hands) -> Self {
        let visible: Vec<&ShownItem> = shown
            .iter()
            .filter(|item| is_visible_slot(item.slot))
            .collect();
        Self {
            sheathed: Some(hands.sheathed()),
            attached: Some(hands.attached()),
            active: Some(hands.active()),
            visible_slots: Some(list(
                version,
                visible.iter().map(|item| item.slot).collect(),
            )),
            visuals: Some(list(
                version,
                visible
                    .iter()
                    .map(|item| item.look.visual(item.item_index))
                    .collect(),
            )),
            loadout_swaps: None,
        }
    }

    /// The change of the hands from `before` to `now`: the sheathed, attached and active maps
    /// that differ, alone. Retail sends a draw, a put-away or a swap so and never the slots and
    /// looks again (newchar 2026-09-18: the attack-press draw at 1792695769648 sent `sheathed`
    /// and `attached`, the weapon key at 1792695921824 `sheathed` and `active`, the attach at
    /// 1792695921940 `attached` alone). Live run 15 sent the whole states with the same maps at
    /// the draw of the attack press, and the client showed no weapon in the hand (Inferred: the
    /// resent item lists put the weapon back at its sheathe).
    pub fn hands_change(before: Hands, now: Hands) -> Self {
        let changed =
            |map: fn(&Hands) -> SlotBits| Some(map(&now)).filter(|bits| *bits != map(&before));
        Self {
            sheathed: changed(Hands::sheathed),
            attached: changed(Hands::attached),
            active: changed(Hands::active),
            ..Self::default()
        }
    }
}

impl PlayerGear {
    /// The change of the hands from `before` to `now` alone: the paperdoll's changed ammo of
    /// main-hand options 1 and 2 and its changed hand maps ([`PaperdollPublic::hands_change`]),
    /// no list and no container (retail newchar 2026-09-18: `a2` with `sheathed` and
    /// `attached` at the draw, `a1` with `sheathed` and `attached` at the put-away, `a2` alone
    /// at each shot and reload).
    pub fn hands_change(before: Hands, now: Hands) -> Self {
        let ammo =
            |option: usize| now.ammo[option].filter(|ammo| Some(*ammo) != before.ammo[option]);
        Self {
            paperdoll: PaperdollState {
                owner: Some(PaperdollOwner {
                    main_hand_ammo_1: ammo(0),
                    main_hand_ammo_2: ammo(1),
                    ..PaperdollOwner::default()
                }),
                public: Some(PaperdollPublic::hands_change(before, now)),
            },
            ..Self::default()
        }
    }

    /// The first states of a player who wears `equipped` (in that order) and has an empty bag.
    pub fn first(equipped: &[EquippedItem]) -> Self {
        Self::of(
            equipped,
            &[None; STARTER_BAG_SLOTS],
            FIRST_VERSION,
            Hands::default(),
        )
    }

    /// The states of a player who wears `equipped` (in that order), has `bag` (one entry per
    /// bag slot) and holds its weapons as `hands`, with lists of `version`, and nothing else.
    pub fn of(
        equipped: &[EquippedItem],
        bag: &[Option<GearItem>],
        version: u64,
        hands: Hands,
    ) -> Self {
        Self::with_extras(equipped, bag, version, hands, &GearExtras::default())
    }

    /// The states of a player who wears `equipped` (in that order), has `bag` (one entry per
    /// bag slot), holds its weapons as `hands` and has `extras`, with lists of `version`. The
    /// first loadout pairs each worn item's slot with its key.
    pub fn with_extras(
        equipped: &[EquippedItem],
        bag: &[Option<GearItem>],
        version: u64,
        hands: Hands,
        extras: &GearExtras,
    ) -> Self {
        let (visible, hidden): (Vec<&EquippedItem>, Vec<&EquippedItem>) =
            equipped.iter().partition(|worn| is_visible_slot(worn.slot));
        let records = |items: &[&EquippedItem]| -> Vec<ItemRecord> {
            items
                .iter()
                .map(|worn| match &worn.item {
                    Some(item) => record(item, SYNC_LOGIN, None),
                    None => ItemRecord {
                        sync_reason: SYNC_REMOVED,
                        ..ItemRecord::EMPTY_SLOT
                    },
                })
                .collect()
        };
        let hidden: Vec<&EquippedItem> = hidden
            .into_iter()
            .filter(|worn| worn.item.is_some())
            .collect();
        let durability = |items: &[&EquippedItem]| -> Vec<u32> {
            items
                .iter()
                .map(|worn| worn.item.map_or(0, |item| item.durability))
                .collect()
        };
        let slots =
            |items: &[&EquippedItem]| -> Vec<u32> { items.iter().map(|worn| worn.slot).collect() };
        let worn_loadout = Loadout {
            name: FIRST_LOADOUT_NAME.to_vec(),
            slots: equipped
                .iter()
                .filter(|worn| worn.item.is_some())
                .map(|worn| (worn.slot, worn.key))
                .collect(),
            flag: false,
            extra: Vec::new(),
        };
        let paperdoll = PaperdollState {
            owner: Some(PaperdollOwner {
                visible_durability: Some(list(version, durability(&visible))),
                visible_items: Some(list(version, records(&visible))),
                hidden_items: Some(list(version, records(&hidden))),
                hidden_slots: Some(list(version, slots(&hidden))),
                hidden_durability: Some(list(version, durability(&hidden))),
                bonus_equip_load: Some(0),
                main_hand_ammo_1: hands.ammo[0],
                main_hand_ammo_2: hands.ammo[1],
                is_local_player: Some(true),
                is_loadout_panel_open: Some(extras.loadout_panel_open),
                loadouts: Some(list(
                    version,
                    std::iter::once(worn_loadout)
                        .chain(extras.loadouts.iter().cloned())
                        .collect(),
                )),
                // Retail first states carry no `hideSkins` child; it goes once a skin was hidden.
                hide_skins: (!extras.hidden_skins.is_empty())
                    .then(|| list(version, extras.hidden_skins.clone())),
            }),
            public: Some(PaperdollPublic::of(
                &equipped.iter().map(ShownItem::of).collect::<Vec<_>>(),
                version,
                hands,
            )),
        };
        let owned = equipped
            .iter()
            .filter_map(|worn| Some((worn.key, worn.item?)))
            .chain(extras.listed.iter().copied())
            .map(|(key, item)| OwnedItem {
                key,
                item: record(&item, SYNC_OWNED, None),
            })
            .collect();
        let second_container = match &extras.open_container {
            Some(open) => ContainerState {
                items: Some(list(version, container_records(open))),
                bonus_max_encumbrance: Some(0),
                can_transfer: Some(open.can_transfer),
                was_emptied: Some(open.was_emptied),
                ..ContainerState::default()
            },
            None => ContainerState {
                bonus_max_encumbrance: Some(0),
                was_emptied: Some(false),
                ..ContainerState::default()
            },
        };
        Self {
            paperdoll,
            bag: ContainerState {
                items: Some(list(version, bag_records(bag, &extras.bag_reasons))),
                bonus_max_encumbrance: Some(0),
                was_emptied: Some(bag.iter().all(Option::is_none)),
                ..ContainerState::default()
            },
            second_container,
            owned: ItemManagementState {
                owned: Some(OwnedItems::Full(FullList {
                    version: Some(version),
                    elements: owned,
                })),
            },
        }
    }

    /// The change from the states `before` (as last sent whole, `None` for never) to the whole
    /// states `now`, at `version`: each child that changed, a list as its changed elements, the
    /// owned items as the keys set and removed ([`OwnedItems::change`]). Retail newchar
    /// 2026-09-18 at 1792695867310, the greatsword equipped from bag slot 2: the paperdoll's
    /// visible durability and item at index 4, the loadout and the look at index 4, the bag's
    /// slot 2 emptied, the owned key 4; nothing else.
    pub fn changes(before: Option<&Self>, now: &Self, version: u64) -> Self {
        let previous_owner = before.and_then(|gear| gear.paperdoll.owner.as_ref());
        let previous_public = before.and_then(|gear| gear.paperdoll.public.as_ref());
        let paperdoll = PaperdollState {
            owner: now
                .paperdoll
                .owner
                .as_ref()
                .map(|now| PaperdollOwner::change(previous_owner, now, version)),
            public: now
                .paperdoll
                .public
                .as_ref()
                .map(|now| PaperdollPublic::change(previous_public, now, version)),
        };
        Self {
            paperdoll,
            bag: ContainerState::change(before.map(|gear| &gear.bag), &now.bag, version),
            second_container: ContainerState::change(
                before.map(|gear| &gear.second_container),
                &now.second_container,
                version,
            ),
            owned: ItemManagementState {
                owned: now.owned.owned.as_ref().and_then(|owned| {
                    let before = before
                        .and_then(|gear| gear.owned.owned.as_ref())
                        .and_then(OwnedItems::full)
                        .map(|list| list.elements.as_slice());
                    let now = owned
                        .full()
                        .map_or(&[][..], |list| list.elements.as_slice());
                    OwnedItems::change(before, now, version)
                }),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_hands_give_the_retail_maps_of_a_drawn_second_weapon() {
        // Retail after the second weapon key: sheathe `00 02 00 3d 00 00 00`, active
        // `00 00 00 0a 00 00 00`, then attach `00 00 00 02 00 00 00`.
        let drawn = Hands {
            drawn: Some(slot::MAIN_HAND_OPTION_1 + 1),
            active: None,
            attached: false,
            off_hand: None,
            ammo: [None; 2],
        };
        assert_eq!(
            drawn.sheathed().0,
            [0x00, 0x02, 0x00, 0x3d, 0x00, 0x00, 0x00]
        );
        assert_eq!(drawn.active().0, [0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00]);
        assert_eq!(drawn.attached().0, [0; 7]);
        let held = Hands {
            attached: true,
            ..drawn
        };
        assert_eq!(
            held.attached().0,
            [0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]
        );
        // Nothing drawn: everything sheathed, the first options active.
        let stowed = Hands::default();
        assert_eq!(
            stowed.sheathed().0,
            [0x00, 0x02, 0x00, 0x3f, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            stowed.active().0,
            [0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00]
        );
        // Sheathed again after the second option: it stays the active one (retail newchar
        // 2026-09-18 at 1792696137458: every weapon sheathed, active slots 25 and 27).
        let put_away = Hands {
            active: Some(slot::MAIN_HAND_OPTION_1 + 1),
            ..stowed
        };
        assert_eq!(
            put_away.sheathed().0,
            [0x00, 0x02, 0x00, 0x3f, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            put_away.active().0,
            [0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn a_shield_drawn_with_the_weapon_gives_the_retail_maps() {
        // Retail `general-other-01` at 1792964468728: the flail of slot 25 drawn with the
        // shield of slot 27, sheathe `.. 35 ..`, active `.. 0a ..`; at 1792964469230 both in
        // the hand, attach `.. 0a ..`.
        let drawn = Hands {
            drawn: Some(slot::MAIN_HAND_OPTION_2),
            active: Some(slot::MAIN_HAND_OPTION_2),
            attached: false,
            off_hand: Some(slot::OFF_HAND_OPTION_1),
            ammo: [None; 2],
        };
        assert_eq!(
            drawn.sheathed().0,
            [0x00, 0x02, 0x00, 0x35, 0x00, 0x00, 0x00]
        );
        assert_eq!(drawn.active().0, [0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00]);
        assert_eq!(drawn.attached().0, [0; 7]);
        let held = Hands {
            attached: true,
            ..drawn
        };
        assert_eq!(
            held.attached().0,
            [0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00]
        );
    }

    /// The paperdoll bytes of `gear`.
    fn paperdoll_bytes(gear: &PlayerGear) -> Vec<u8> {
        let mut writer = Writer::new();
        gear.paperdoll.write(&mut writer).expect("paperdoll");
        writer.finish()
    }

    #[test]
    fn a_change_of_the_hands_sends_the_retail_hand_children_alone() {
        /// Retail newchar 2026-09-18 at 1792695769648, the own paperdoll 24 ms after the first
        /// attack press of the ship fight: `a2`, then `sheathed` and `attached`.
        const RETAIL_DRAW: &str = "0680040018e0ef4deace5830030002003e00000000000001000000";
        /// The same at 1792695815911, the put-away: `a1`, then `sheathed` and `attached`.
        const RETAIL_PUT_AWAY: &str = "0680020018e0ef58b0521650030002003f00000000000000000000";
        /// At 1792695921940, the swapped weapon reaching the hand: `attached` alone.
        const RETAIL_ATTACH: &str = "040200000002000000";
        const DRAW_NS: u64 = 1_792_695_769_624_369_200;
        const PUT_AWAY_NS: u64 = 1_792_695_815_887_787_600;
        let empty = |time_ns| Some(LoadedAmmo { count: 0, time_ns });
        let sheathed = Hands {
            drawn: None,
            active: Some(slot::MAIN_HAND_OPTION_1),
            attached: false,
            off_hand: None,
            ammo: [empty(0), empty(0)],
        };
        let drawn = Hands {
            drawn: Some(slot::MAIN_HAND_OPTION_1),
            attached: true,
            ammo: [empty(0), empty(DRAW_NS)],
            ..sheathed
        };
        let draw = PlayerGear::hands_change(sheathed, drawn);
        assert_eq!(paperdoll_bytes(&draw), hex(RETAIL_DRAW));
        assert_eq!(
            (draw.bag, draw.second_container, draw.owned),
            (
                ContainerState::default(),
                ContainerState::default(),
                ItemManagementState::default()
            ),
            "no container and no owned item"
        );
        let put_away = Hands {
            drawn: None,
            attached: false,
            ammo: [empty(PUT_AWAY_NS), empty(DRAW_NS)],
            ..drawn
        };
        assert_eq!(
            paperdoll_bytes(&PlayerGear::hands_change(drawn, put_away)),
            hex(RETAIL_PUT_AWAY)
        );
        let second = Hands {
            drawn: Some(slot::MAIN_HAND_OPTION_2),
            active: Some(slot::MAIN_HAND_OPTION_2),
            attached: false,
            off_hand: None,
            ammo: [None; 2],
        };
        let attach = PlayerGear::hands_change(
            second,
            Hands {
                attached: true,
                ..second
            },
        );
        assert_eq!(paperdoll_bytes(&attach), hex(RETAIL_ATTACH));
        assert!(
            PlayerGear::hands_change(drawn, drawn).paperdoll.is_empty(),
            "no change, no child"
        );
    }
    use crate::wire::{Reader, Writer};

    /// The retail first paperdoll of a new wanderer (paperdoll note, section 10.1), with the
    /// five instance ids set to zero and the unknown ammo child left out: chest, hands, legs and
    /// feet at capture indexes 40226 to 40229, the tutorial sword at 1223; compared up to the
    /// loadout list, whose pairs retail sends in another order.
    const RETAIL_PAPERDOLL_WITHOUT_IDS: &str = "\
        06ff0001030500000177000001770000017700000177000003e8000103050001084aebff0519b705004b000000000000\
        000000000000000000000000000000000000000100000000000000000001084aebff245ab705014b0000000000000000\
        00000000000000000000000000000000000100000000000000000001084aebff439bb705024b00000000000000000000\
        0000000000000000000000000000000100000000000000000001084aebff62dcb705034b000000000000000000000000\
        000000000000000000000000000100000000000000000001084ad965536fa80f043c0000000000000000000000000000\
        0000000000000000000000010000000000000000000103000001030000010300000001090000010301";
    /// The retail first bag (section 10.2) and second container (section 10.4).
    const RETAIL_SECOND_CONTAINER: [u8; 7] = [0x01, 0x14, 0, 0, 0, 0, 0];
    const ARMOR_DURABILITY: u32 = 375;
    const ARMOR_GEAR_SCORE: u32 = 75;
    const SWORD_DURABILITY: u32 = 1000;
    const SWORD_GEAR_SCORE: u32 = 60;
    const CAPTURE_CHEST_INDEX: u32 = 40_226;
    const CAPTURE_SWORD_INDEX: u32 = 1223;
    /// The retail bag (236 bytes): slot byte, children 0, 2 and 4, then 5 empty items of 45 bytes.
    const RETAIL_BAG_HEAD: [u8; 6] = [0x01, 0x15, 0x00, 0x01, 0x03, 0x05];
    const EMPTY_ITEM_BYTES: usize = 45;
    const RETAIL_BAG_TAIL: [u8; 5] = [0, 0, 0, 0, 0x01];

    fn item(item_index: u32, gear_score: u32, durability: u32) -> GearItem {
        GearItem {
            item_index,
            quantity: 1,
            gear_score,
            durability,
            max_durability: durability,
            perks: [0; PERK_SLOTS],
            look: ItemLook::default(),
            counter: 0,
            bound: true,
            locked: false,
            instance: [0; RAW_ID_LEN],
        }
    }

    /// The retail first worn items: keys and counters 0 to 4 in order.
    fn wanderer() -> Vec<EquippedItem> {
        let worn = |key: u16, slot, item: GearItem| EquippedItem {
            slot,
            key,
            item: Some(GearItem {
                counter: u32::from(key),
                ..item
            }),
        };
        let armor = |key, slot, item_index| {
            worn(
                key,
                slot,
                item(item_index, ARMOR_GEAR_SCORE, ARMOR_DURABILITY),
            )
        };
        vec![
            armor(0, slot::CHEST, CAPTURE_CHEST_INDEX),
            armor(1, slot::HANDS, CAPTURE_CHEST_INDEX + 1),
            armor(2, slot::LEGS, CAPTURE_CHEST_INDEX + 2),
            armor(3, slot::FEET, CAPTURE_CHEST_INDEX + 3),
            worn(
                4,
                slot::MAIN_HAND_OPTION_1,
                item(CAPTURE_SWORD_INDEX, SWORD_GEAR_SCORE, SWORD_DURABILITY),
            ),
        ]
    }

    /// The full elements of a list child.
    fn full<T>(list: Option<&ReplicatedList<T>>) -> &[T] {
        &list
            .and_then(ReplicatedList::full)
            .expect("a full list")
            .elements
    }

    /// The bytes of hex text.
    pub(crate) fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn encode(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut writer = Writer::new();
        write(&mut writer);
        writer.finish()
    }

    #[test]
    fn the_paperdoll_owner_slot_is_the_retail_one() {
        let gear = PlayerGear::first(&wanderer());
        let bytes = encode(|writer| gear.paperdoll.write(writer).expect("write"));
        let retail = hex(RETAIL_PAPERDOLL_WITHOUT_IDS);
        assert_eq!(bytes[..retail.len()], retail[..]);
        let mut reader = Reader::new(&bytes);
        assert_eq!(PaperdollState::read(&mut reader), Ok(gear.paperdoll));
        assert!(reader.is_empty());
    }

    #[test]
    fn a_storage_seen_from_afar_sends_no_slots_as_retail() {
        /// Retail `storage_open_deposit_withdraw_transfer` sync reasons: a storage the player
        /// stands at (6) and one seen from afar after 1512 (47).
        const NEAR: u8 = 6;
        const AFAR: u8 = 47;
        const STORED_INDEX: u32 = 5102;
        const VERSION: u64 = 1737;
        let view = |sync_reason, slots_listed| GearExtras {
            open_container: Some(OpenContainer {
                items: vec![Some(item(STORED_INDEX, 0, 0)), None],
                sync_reason,
                can_transfer: true,
                was_emptied: false,
                slots_listed,
                empty_reason: sync_reason,
            }),
            ..GearExtras::default()
        };
        let records = |extras: &GearExtras| {
            let gear = PlayerGear::with_extras(&[], &[], VERSION, Hands::default(), extras);
            full(gear.second_container.items.as_ref()).to_vec()
        };
        let near = records(&view(NEAR, true));
        assert_eq!(
            (near[0].container_slot, near[0].sync_reason),
            (Some(0), NEAR)
        );
        assert_eq!((near[1].container_slot, near[1].sync_reason), (None, NEAR));
        let afar = records(&view(AFAR, false));
        assert_eq!(
            (
                afar[0].container_slot,
                afar[0].sync_reason,
                afar[0].item_index
            ),
            (None, AFAR, STORED_INDEX)
        );
        assert_eq!(
            (
                afar[1].container_slot,
                afar[1].sync_reason,
                afar[1].quantity
            ),
            (None, AFAR, 0)
        );
    }

    #[test]
    fn the_containers_are_the_retail_ones() {
        let gear = PlayerGear::first(&wanderer());
        let bag = encode(|writer| gear.bag.write(writer).expect("bag"));
        assert_eq!(bag[..RETAIL_BAG_HEAD.len()], RETAIL_BAG_HEAD);
        assert_eq!(
            bag.len(),
            RETAIL_BAG_HEAD.len() + STARTER_BAG_SLOTS * EMPTY_ITEM_BYTES + RETAIL_BAG_TAIL.len()
        );
        assert_eq!(bag[bag.len() - RETAIL_BAG_TAIL.len()..], RETAIL_BAG_TAIL);
        assert_eq!(ContainerState::read(&mut Reader::new(&bag)), Ok(gear.bag));
        let second = encode(|writer| gear.second_container.write(writer).expect("second"));
        assert_eq!(second, RETAIL_SECOND_CONTAINER);
    }

    #[test]
    fn a_later_state_holds_the_bag_and_hidden_slots_at_its_version() {
        const VERSION: u64 = 9;
        const POTION_INDEX: u32 = 4885;
        const POTION_SLOT: u32 = 14;
        const POTIONS: u32 = 2;
        let mut equipped = wanderer();
        equipped.push(EquippedItem {
            slot: POTION_SLOT,
            key: 5,
            item: Some(GearItem {
                quantity: POTIONS,
                ..item(POTION_INDEX, 0, 0)
            }),
        });
        let bag = [
            None,
            Some(item(
                CAPTURE_CHEST_INDEX,
                ARMOR_GEAR_SCORE,
                ARMOR_DURABILITY,
            )),
        ];
        let gear = PlayerGear::of(&equipped, &bag, VERSION, Hands::default());
        let owner = gear.paperdoll.owner.as_ref().expect("owner");
        let visible = owner
            .visible_items
            .as_ref()
            .and_then(ReplicatedList::full)
            .expect("visible");
        assert_eq!(visible.version, Some(VERSION));
        assert_eq!(visible.elements.len(), wanderer().len());
        let hidden = full(owner.hidden_items.as_ref()).to_vec();
        assert_eq!(
            (hidden[0].item_index, hidden[0].quantity),
            (POTION_INDEX, POTIONS)
        );
        assert_eq!(full(owner.hidden_slots.as_ref()), [POTION_SLOT]);
        let loadout = &full(owner.loadouts.as_ref())[0];
        assert_eq!(loadout.slots.last(), Some(&(POTION_SLOT, 5)));
        let items = full(gear.bag.items.as_ref()).to_vec();
        assert_eq!(items[0], ItemRecord::EMPTY_SLOT);
        assert_eq!(
            (
                items[1].container_slot,
                items[1].sync_reason,
                items[1].item_index
            ),
            (Some(1), SYNC_MOVED_IN, CAPTURE_CHEST_INDEX)
        );
        assert_eq!(gear.bag.was_emptied, Some(false));
        let bytes = encode(|writer| gear.paperdoll.write(writer).expect("write"));
        assert_eq!(
            PaperdollState::read(&mut Reader::new(&bytes)),
            Ok(gear.paperdoll)
        );
    }

    #[test]
    fn a_removed_weapon_keeps_its_visible_slot_empty_as_retail() {
        // Retail 2026-09-18, 149365: the tutorial sword (key 4) taken after the sinking.
        let mut equipped = wanderer();
        equipped[4].item = None;
        let gear = PlayerGear::of(&equipped, &[], FIRST_VERSION, Hands::default());
        let owner = gear.paperdoll.owner.as_ref().expect("owner");
        let public = gear.paperdoll.public.as_ref().expect("public");
        let slots = full(public.visible_slots.as_ref());
        assert_eq!(slots.len(), wanderer().len(), "the list keeps slot 24");
        assert_eq!(slots[4], slot::MAIN_HAND_OPTION_1);
        assert_eq!(
            full(owner.visible_items.as_ref())[4],
            ItemRecord {
                sync_reason: SYNC_REMOVED,
                ..ItemRecord::EMPTY_SLOT
            }
        );
        assert_eq!(full(owner.visible_durability.as_ref())[4], 0);
        assert_eq!(full(public.visuals.as_ref())[4], ItemVisual::of(0));
        let loadout = &full(owner.loadouts.as_ref())[0];
        assert!(
            loadout
                .slots
                .iter()
                .all(|(slot_id, _)| *slot_id != slot::MAIN_HAND_OPTION_1),
            "the loadout has no pair for it"
        );
        let owned = &gear
            .owned
            .owned
            .as_ref()
            .and_then(OwnedItems::full)
            .expect("owned")
            .elements;
        assert_eq!(
            owned.iter().map(|owned| owned.key).collect::<Vec<_>>(),
            [0, 1, 2, 3],
            "key 4 is gone, the others stay"
        );
    }

    #[test]
    fn the_owned_items_carry_the_loadout_keys() {
        let gear = PlayerGear::first(&wanderer());
        let bytes = encode(|writer| gear.owned.write(writer).expect("owned"));
        assert_eq!(bytes[..6], [0x02, 0x01, 0x00, 0x01, 0x03, 0x05]);
        let read = ItemManagementState::read(&mut Reader::new(&bytes)).expect("read");
        let owned = read
            .owned
            .as_ref()
            .and_then(OwnedItems::full)
            .expect("owned")
            .elements
            .clone();
        assert_eq!(owned.len(), wanderer().len());
        assert!(
            owned
                .iter()
                .all(|owned| owned.item.sync_reason == SYNC_OWNED)
        );
        assert_eq!(owned[4].key, 4);
        assert_eq!(owned[4].item.item_index, CAPTURE_SWORD_INDEX);
    }

    #[test]
    fn an_equip_into_the_removed_swords_place_sends_the_retail_changes() {
        /// Retail newchar 2026-09-18 at 1792695867310: the list version of the change.
        const VERSION: u64 = 4688;
        const GREATSWORD_INDEX: u32 = 40_263;
        const GREATSWORD_DURABILITY: u32 = 700;
        const GREATSWORD_SLOT: usize = 2;
        const GREATSWORD_KEY: u16 = 4;
        /// The owner child 0 of the change: slot 1 only, child 0, index 4 at the version, 700.
        const RETAIL_DURABILITY_CHILD: &str = "0201010104019049000002bc";
        /// The public child 4 of the change: the look of index 4, item 40263, no dye.
        const RETAIL_LOOK_CHILD: &str = "041001010401";
        const RETAIL_LOOK_VALUE: &str = "9049c7ea04000000000000000000";
        let greatsword = item(GREATSWORD_INDEX, ARMOR_GEAR_SCORE, GREATSWORD_DURABILITY);
        let other = item(CAPTURE_CHEST_INDEX, ARMOR_GEAR_SCORE, ARMOR_DURABILITY);
        let mut worn = wanderer();
        worn[4].item = None;
        let mut bag = vec![Some(other), Some(other), Some(greatsword), None, None];
        let before = PlayerGear::of(&worn, &bag, FIRST_VERSION, Hands::default());
        worn[4] = EquippedItem {
            slot: slot::MAIN_HAND_OPTION_1,
            key: GREATSWORD_KEY,
            item: Some(greatsword),
        };
        bag[GREATSWORD_SLOT] = None;
        let now = PlayerGear::of(&worn, &bag, VERSION, Hands::default());
        let change = PlayerGear::changes(Some(&before), &now, VERSION);
        let owner = change.paperdoll.owner.clone().expect("owner");
        assert!(owner.hidden_items.is_none() && owner.bonus_equip_load.is_none());
        assert!(matches!(
            &owner.visible_items,
            Some(ReplicatedList::Changes(changes)) if changes.len() == 1 && changes[0].index == 4
        ));
        assert!(matches!(
            &owner.loadouts,
            Some(ReplicatedList::Changes(changes)) if changes[0].index == 0
        ));
        let durability_only = PaperdollState {
            owner: Some(PaperdollOwner {
                visible_durability: owner.visible_durability.clone(),
                ..PaperdollOwner::default()
            }),
            public: None,
        };
        assert_eq!(
            encode(|writer| durability_only.write(writer).expect("write")),
            hex(RETAIL_DURABILITY_CHILD)
        );
        let public = change.paperdoll.public.clone().expect("public");
        assert!(public.visible_slots.is_none(), "slot 24 kept its place");
        let look_only = PaperdollState {
            owner: None,
            public: Some(public),
        };
        assert_eq!(
            encode(|writer| look_only.write(writer).expect("write")),
            hex(&format!("{RETAIL_LOOK_CHILD}{RETAIL_LOOK_VALUE}"))
        );
        assert!(matches!(
            &change.bag.items,
            Some(ReplicatedList::Changes(changes))
                if changes.len() == 1
                    && changes[0].index == 2
                    && changes[0].value == ItemRecord::EMPTY_SLOT
        ));
        assert_eq!(change.bag.was_emptied, None, "the bag still holds items");
        assert_eq!(change.second_container, ContainerState::default());
        assert!(matches!(
            &change.owned.owned,
            Some(OwnedItems::Changes(changes))
                if changes.len() == 1
                    && changes[0].key == GREATSWORD_KEY
                    && changes[0].version == VERSION
        ));
        assert_eq!(
            PlayerGear::changes(Some(&now), &now, VERSION),
            PlayerGear {
                paperdoll: PaperdollState {
                    owner: Some(PaperdollOwner::default()),
                    public: Some(PaperdollPublic::default()),
                },
                ..PlayerGear::default()
            },
            "no change, no child"
        );
    }
}
