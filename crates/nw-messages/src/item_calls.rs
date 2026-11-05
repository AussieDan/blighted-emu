// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's item calls besides equip, unequip and swap ([`crate::facet`]): use, drop,
//! storage moves, locks and loadouts (run-15 items study), gems and charms, moves by UI item
//! class and storage searches (run-15 items2 study), and the server's loadout status.
//!
//! Every body starts with a [`FacetHeader`]. Layouts are the Azoth catalog's where it
//! has one (2917, 1838, 826, 2801, 2942, 2549) and the retail open-world bodies' otherwise
//! (`Hiveloadouts`, `gearsets`, `storage_open_deposit_withdraw_transfer`, `Drop_Item_Lootbag`,
//! `drop_equipped_item`, `item_actions`). The player-trade calls (2801, 2942, 1461, 2950) name
//! the player's own entity as their container in every retail body: the container they act on
//! is the one the player has open.

use crate::facet::FacetHeader;
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer};

/// Type index of `ContainerComponentServerFacet_ServerUseItem`.
pub const USE_ITEM_TYPE_INDEX: u64 = 2917;
/// Type index of `ItemDropComponentServerFacet_DropInventoryItems`.
pub const DROP_INVENTORY_ITEMS_TYPE_INDEX: u64 = 1838;
/// Type index of `ItemDropComponentServerFacet_DropPaperdollItem`.
pub const DROP_PAPERDOLL_ITEM_TYPE_INDEX: u64 = 826;
/// Type index of `PlayerTradeComponentServerFacet_GiveItem`.
pub const GIVE_ITEM_TYPE_INDEX: u64 = 2801;
/// Type index of `PlayerTradeComponentServerFacet_TakeItem`.
pub const TAKE_ITEM_TYPE_INDEX: u64 = 2942;
/// Type index of `PlayerTradeComponentServerFacet_EquipItem`.
pub const TRADE_EQUIP_ITEM_TYPE_INDEX: u64 = 1461;
/// Type index of `PlayerTradeComponentServerFacet_UnequipItem`.
pub const TRADE_UNEQUIP_ITEM_TYPE_INDEX: u64 = 2950;
/// Type index of `GlobalStorageComponentServerFacet_UpdateStorageSession`.
pub const UPDATE_STORAGE_SESSION_TYPE_INDEX: u64 = 1512;
/// Type index of `PaperdollComponentServerFacet_RequestSetIsLoadoutPanelOpen`.
pub const LOADOUT_PANEL_OPEN_TYPE_INDEX: u64 = 5578;
/// Type index of `PaperdollComponentServerFacet_RequestAddItemToLoadouts`.
pub const ADD_ITEM_TO_LOADOUTS_TYPE_INDEX: u64 = 5579;
/// Type index of `PaperdollComponentServerFacet_RequestSwitchToLoadout`.
pub const SWITCH_TO_LOADOUT_TYPE_INDEX: u64 = 5442;
/// Type index of `PaperdollComponentServerFacet_RequestSavePaperdollToLoadout`.
pub const SAVE_PAPERDOLL_TO_LOADOUT_TYPE_INDEX: u64 = 5542;
/// Type index of `PaperdollComponentServerFacet_RequestRemoveAllItemsFromLoadout`.
pub const REMOVE_ALL_ITEMS_FROM_LOADOUT_TYPE_INDEX: u64 = 5427;
/// Type index of `PaperdollComponentServerFacet_RequestRenameLoadout`.
pub const RENAME_LOADOUT_TYPE_INDEX: u64 = 5487;
/// Type index of `PaperdollComponentServerFacet_RequestRemoveItemFromAllLoadouts`.
pub const REMOVE_ITEM_FROM_ALL_LOADOUTS_TYPE_INDEX: u64 = 5443;
/// Type index of `PaperdollComponentServerFacet_RequestRemoveItemFromLoadout`.
pub const REMOVE_ITEM_FROM_LOADOUT_TYPE_INDEX: u64 = 5430;
/// Type index of `ItemRepairComponentServerFacet_RequestSetLockState`.
pub const SET_LOCK_STATE_TYPE_INDEX: u64 = 2549;
/// Type index of `PaperdollComponentClientFacet_SendLoadoutStatusNotification`.
pub const LOADOUT_STATUS_TYPE_INDEX: u64 = 5497;
/// Type index of `ItemRepairComponentServerFacet_RequestSlotItemWithCharm`.
pub const SLOT_ITEM_WITH_CHARM_TYPE_INDEX: u64 = 6983;
/// Type index of `ItemRepairComponentServerFacet_ClearCharmRequests`.
pub const CLEAR_CHARM_REQUESTS_TYPE_INDEX: u64 = 7038;
/// Type index of `ItemDropComponentServerFacet_DropInventoryItemsByClass`.
pub const DROP_INVENTORY_ITEMS_BY_CLASS_TYPE_INDEX: u64 = 3938;
/// Type index of `PlayerTradeComponentServerFacet_GiveItemsByClass`.
pub const GIVE_ITEMS_BY_CLASS_TYPE_INDEX: u64 = 2946;
/// Type index of `PlayerTradeComponentServerFacet_TakeItemsByClass`.
pub const TAKE_ITEMS_BY_CLASS_TYPE_INDEX: u64 = 2945;
/// Type index of `GlobalStorageComponentServerFacet_RequestSearchStorage`.
pub const REQUEST_SEARCH_STORAGE_TYPE_INDEX: u64 = 7044;
/// Type index of `GlobalStorageComponentServerFacet_RequestReplicateStorageSearchResults`.
pub const REPLICATE_SEARCH_RESULTS_TYPE_INDEX: u64 = 7039;
/// The location of a 6983 target item: its value is the paperdoll slot (retail MSQ15: every
/// target, slot 24 the main hand).
pub const LOCATION_PAPERDOLL_SLOT: u32 = 2;
/// Most bytes of a loadout name this server reads (the paperdoll's loadout name limit).
const MAX_NAME_BYTES: u64 = 0x2_ffff;
/// Most entries of a list in an item call: a container's slots.
const MAX_CALL_ENTRIES: u64 = 500;

/// The paperdoll component of the player entity, on its child [`PAPERDOLL_CHILD`]: solved from
/// the 1461, 3895, 5497 and 5578 headers of 9 retail characters (target = (entity XOR child) +
/// component; the top bit and bit 0 of each are open, and every choice gives the same targets).
pub const PAPERDOLL: u64 = 0x2468_2af2_564e_7814;
/// The XOR of the player prefab's main entity and the child that holds its paperdoll.
pub const PAPERDOLL_CHILD: u64 = 0xb390_f4d0_0323_708d;

/// The location of an item reference to a bag slot (retail 2549 on bag slot 402, 5579 on bag
/// slots 36, 48 and 51).
pub const LOCATION_BAG: u32 = 1;
/// Locations of an item reference by owned-items key (retail 5579 locations 2 and 4: each slot
/// field was the key of an item the loadouts named).
pub const LOCATIONS_BY_KEY: [u32; 2] = [2, 4];

/// `eLoadoutStatusCode` values (5497), in the order of the client's reflected enum
/// (`FUN_1468edfc0`), as retail sent them (open-world `Hiveloadouts`, `gearsets`).
pub mod loadout_status {
    /// `ChangeSuccess`: a switch (5442) is done (retail 0 after each switch).
    pub const CHANGE_SUCCESS: u32 = 0;
    /// `ChangeFailure_Index`: no such loadout.
    pub const CHANGE_FAILURE_INDEX: u32 = 1;
    /// `ChangeFailure_Generic`: an item of the loadout could not be equipped.
    pub const CHANGE_FAILURE_GENERIC: u32 = 3;
    /// `UnequipUpdate_Linked`: the one item of a loadout left it (retail 7 for a loadout of one
    /// item emptied by 5427).
    pub const UNEQUIP_UPDATE_LINKED: u32 = 7;
    /// `UnequipAllUpdate_Linked`: the items of a loadout left it (retail 8 for loadouts of 14
    /// and 15 items).
    pub const UNEQUIP_ALL_UPDATE_LINKED: u32 = 8;
    /// `EquipUpdate_Success`: an item went into a loadout (retail 11 after each 5579).
    pub const EQUIP_UPDATE_SUCCESS: u32 = 11;
    /// `EquipUpdate_Fail`: it did not.
    pub const EQUIP_UPDATE_FAIL: u32 = 12;
}

/// The container a player-trade call names: an entity id, its facet entity and an id of
/// unknown meaning (retail: the player's own entity in every storage and bag call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerRef {
    /// The entity id.
    pub id: [u8; RAW_ID_LEN],
    /// Its facet entity.
    pub entity: u64,
    /// An id of unknown meaning.
    pub extra: u64,
}

impl ContainerRef {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            id: reader.raw_id()?,
            entity: reader.u64()?,
            extra: reader.u64()?,
        })
    }
}

/// An item reference (28 bytes, 2549 and 5579): a location, a slot of it, the instance id and a
/// number of unknown meaning (0 in every retail body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemRef {
    /// The location: 1 for a bag slot; 2 and 4 name an owned-items key (retail 5579).
    pub location: u32,
    /// The bag slot or the key.
    pub slot: u32,
    /// The instance id; all zero when the call names the item by key alone.
    pub instance: [u8; RAW_ID_LEN],
    /// A number of unknown meaning.
    pub extra: u32,
}

impl ItemRef {
    /// Reads an item reference.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early.
    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            location: reader.u32()?,
            slot: reader.u32()?,
            instance: reader.raw_id()?,
            extra: reader.u32()?,
        })
    }
}

/// One stack of a drop (1838).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropEntry {
    /// Its bag slot.
    pub bag_slot: u32,
    /// Its instance id.
    pub instance: [u8; RAW_ID_LEN],
    /// A number of unknown meaning (`0xFFFF_FFFF` in every retail body).
    pub extra: u32,
    /// How many to drop; 0 or more than the stack is the whole stack.
    pub quantity: u32,
}

/// An item call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemCallMsg {
    /// 2917: use the item of a bag slot (a potion, food).
    UseItem {
        /// The header.
        header: FacetHeader,
        /// The bag slot.
        bag_slot: u32,
    },
    /// 1838: drop stacks of the bag.
    DropInventoryItems {
        /// The header.
        header: FacetHeader,
        /// The stacks.
        entries: Vec<DropEntry>,
    },
    /// 826: drop the item of a paperdoll slot.
    DropPaperdollItem {
        /// The header.
        header: FacetHeader,
        /// The paperdoll slot.
        slot: u32,
        /// How many; 0 or more than the stack is the whole stack.
        quantity: u32,
    },
    /// 2801: put a bag stack into the open container.
    GiveItem {
        /// The header.
        header: FacetHeader,
        /// The container named.
        container: ContainerRef,
        /// The stack's instance id.
        instance: [u8; RAW_ID_LEN],
        /// How many; 0 or more than the stack is the whole stack.
        quantity: u32,
    },
    /// 2942: take a stack of the open container into the bag.
    TakeItem {
        /// The header.
        header: FacetHeader,
        /// The container named.
        container: ContainerRef,
        /// The stack's instance id.
        instance: [u8; RAW_ID_LEN],
        /// How many; 0 or more than the stack is the whole stack.
        quantity: u32,
    },
    /// 1461: put a stack of the open container on.
    TradeEquipItem {
        /// The header.
        header: FacetHeader,
        /// The container named.
        container: ContainerRef,
        /// The paperdoll slot.
        slot: u32,
        /// The stack's instance id.
        instance: [u8; RAW_ID_LEN],
        /// How many.
        quantity: u32,
        /// A byte of unknown meaning (0 in every retail body).
        flag: u8,
    },
    /// 2950: take a worn item off into the open container.
    TradeUnequipItem {
        /// The header.
        header: FacetHeader,
        /// The container named.
        container: ContainerRef,
        /// The paperdoll slot.
        slot: u32,
        /// The item's instance id.
        instance: [u8; RAW_ID_LEN],
        /// How many.
        quantity: u32,
    },
    /// 1512: show another storage of the player in the storage window.
    UpdateStorageSession {
        /// The header.
        header: FacetHeader,
        /// The storage's entity id.
        storage: [u8; RAW_ID_LEN],
    },
    /// 5578: the loadout panel opened or closed.
    LoadoutPanelOpen {
        /// The header.
        header: FacetHeader,
        /// Whether it is open.
        open: bool,
    },
    /// 5579: put an item into paperdoll slot `slot` of loadouts.
    AddItemToLoadouts {
        /// The header.
        header: FacetHeader,
        /// The item.
        item: ItemRef,
        /// A number of unknown meaning (1 in every retail body; Inferred: the quantity).
        quantity: u32,
        /// The paperdoll slot.
        slot: u32,
        /// The loadout indexes.
        loadouts: Vec<u8>,
    },
    /// 5442: put a loadout's items on.
    SwitchToLoadout {
        /// The header.
        header: FacetHeader,
        /// The loadout index.
        loadout: u8,
    },
    /// 5542: save the worn items as a loadout.
    SavePaperdollToLoadout {
        /// The header.
        header: FacetHeader,
        /// The loadout index.
        loadout: u8,
    },
    /// 5427: empty a loadout.
    RemoveAllItemsFromLoadout {
        /// The header.
        header: FacetHeader,
        /// The loadout index.
        loadout: u8,
        /// A byte of unknown meaning (0 in every retail body).
        flag: u8,
    },
    /// 5487: rename a loadout.
    RenameLoadout {
        /// The header.
        header: FacetHeader,
        /// The loadout index.
        loadout: u8,
        /// The new name.
        name: Vec<u8>,
    },
    /// 5443: take an item out of every loadout.
    RemoveItemFromAllLoadouts {
        /// The header.
        header: FacetHeader,
        /// The item's owned-items key.
        key: u16,
    },
    /// 5430: take an item out of one loadout.
    RemoveItemFromLoadout {
        /// The header.
        header: FacetHeader,
        /// The loadout index.
        loadout: u8,
        /// The item's owned-items key.
        key: u16,
        /// The paperdoll slot.
        slot: u32,
    },
    /// 2549: lock or unlock an item.
    SetLockState {
        /// The header.
        header: FacetHeader,
        /// The item.
        item: ItemRef,
        /// Whether it is locked.
        locked: bool,
    },
    /// 6983: put a gem or a charm of the bag into a perk slot of an item (retail MSQ15, MSQ19,
    /// OPR_TEST3: 17 bodies).
    SlotItemWithCharm {
        /// The header.
        header: FacetHeader,
        /// The item that takes it: location [`LOCATION_PAPERDOLL_SLOT`] and its paperdoll slot,
        /// or [`LOCATION_BAG`] and its bag slot.
        target: ItemRef,
        /// The gem or charm: [`LOCATION_BAG`] and its bag slot.
        charm: ItemRef,
        /// The index in the item's perks of the slot it fills.
        perk_index: u32,
        /// A number of unknown meaning (1 in all 17 retail bodies).
        extra: u32,
    },
    /// 7038: the charm window dropped its pending requests (header only; no reply in any
    /// capture).
    ClearCharmRequests {
        /// The header.
        header: FacetHeader,
    },
    /// 3938: drop every bag stack of a UI item class into one bag of dropped items.
    DropItemsByClass {
        /// The header.
        header: FacetHeader,
        /// The UI item class number (`nw_content::ui_item_class`).
        class: u64,
        /// A flag of unknown meaning (1 in all 10 retail bodies).
        flag: bool,
    },
    /// 2946: put every bag stack of a UI item class into the open container.
    GiveItemsByClass {
        /// The header.
        header: FacetHeader,
        /// The container named (the player's own entity in every retail body).
        container: ContainerRef,
        /// The UI item class number.
        class: u64,
    },
    /// 2945: take every stack of a UI item class of the open container into the bag.
    TakeItemsByClass {
        /// The header.
        header: FacetHeader,
        /// The container named.
        container: ContainerRef,
        /// The UI item class number.
        class: u64,
    },
    /// 7044: search the player's storages for the items a station takes (retail: 37 sessions,
    /// 31 bytes after the header).
    SearchStorage {
        /// The header (the player's `GlobalStorageComponent`).
        header: FacetHeader,
        /// The search id; the client keeps one for a whole station session.
        search: [u8; RAW_ID_LEN],
        /// A byte of unknown meaning (0 in every retail body).
        lead: u8,
        /// A filter: 0 for a station search, [`SEARCH_FILTER_HOUSING`] for housing items.
        filter: u32,
        /// A number of unknown meaning (0 in every retail body).
        extra: u32,
        /// CRC-32 of the station type's lowercase name without its tier digits; 0 for none.
        station: u32,
        /// A byte of unknown meaning (0 in every retail body).
        tail: u8,
        /// A refresh of the same search (the client sends bursts of up to 20 in one tick).
        refresh: bool,
    },
    /// 7039: send the results of the last search again (header only).
    ReplicateSearchResults {
        /// The header.
        header: FacetHeader,
    },
}

/// The 7044 filter of the housing and dye screens (Observed in 4 sessions with station 0;
/// best guess: all 48 results of those searches were `HousingItem` ids).
pub const SEARCH_FILTER_HOUSING: u32 = 0x000a_bfc6;

/// A count of `limit` or fewer list entries.
fn entries(reader: &mut Reader<'_>, limit: u64) -> Result<u64, MessageError> {
    let count = reader.wide_prefixed()?;
    if count > limit {
        return Err(MessageError::CountTooLarge { count, limit });
    }
    Ok(count)
}

impl ItemCallMsg {
    /// Decodes `body` when `type_index` is an item call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            USE_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::UseItem {
                    header,
                    bag_slot: reader.u32()?,
                })
            }),
            DROP_INVENTORY_ITEMS_TYPE_INDEX => read(|header, reader| {
                let count = entries(reader, MAX_CALL_ENTRIES)?;
                let entries = (0..count)
                    .map(|_| {
                        Ok(DropEntry {
                            bag_slot: reader.u32()?,
                            instance: reader.raw_id()?,
                            extra: reader.u32()?,
                            quantity: reader.u32()?,
                        })
                    })
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::DropInventoryItems { header, entries })
            }),
            DROP_PAPERDOLL_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DropPaperdollItem {
                    header,
                    slot: reader.u32()?,
                    quantity: reader.u32()?,
                })
            }),
            GIVE_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::GiveItem {
                    header,
                    container: ContainerRef::read(reader)?,
                    instance: reader.raw_id()?,
                    quantity: reader.u32()?,
                })
            }),
            TAKE_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TakeItem {
                    header,
                    container: ContainerRef::read(reader)?,
                    instance: reader.raw_id()?,
                    quantity: reader.u32()?,
                })
            }),
            TRADE_EQUIP_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TradeEquipItem {
                    header,
                    container: ContainerRef::read(reader)?,
                    slot: reader.u32()?,
                    instance: reader.raw_id()?,
                    quantity: reader.u32()?,
                    flag: reader.u8()?,
                })
            }),
            TRADE_UNEQUIP_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TradeUnequipItem {
                    header,
                    container: ContainerRef::read(reader)?,
                    slot: reader.u32()?,
                    instance: reader.raw_id()?,
                    quantity: reader.u32()?,
                })
            }),
            UPDATE_STORAGE_SESSION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::UpdateStorageSession {
                    header,
                    storage: reader.raw_id()?,
                })
            }),
            LOADOUT_PANEL_OPEN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::LoadoutPanelOpen {
                    header,
                    open: reader.boolean()?,
                })
            }),
            ADD_ITEM_TO_LOADOUTS_TYPE_INDEX => read(|header, reader| {
                // Retail Hiveloadouts: the item, a quantity, one slot byte, then one byte per
                // loadout index (owned key 11, quantity 1, slot 1, loadout 5).
                let item = ItemRef::read(reader)?;
                let quantity = reader.u32()?;
                let slot = u32::from(reader.u8()?);
                let mut loadouts = Vec::new();
                while !reader.is_empty() {
                    loadouts.push(reader.u8()?);
                }
                Ok(Self::AddItemToLoadouts {
                    header,
                    item,
                    quantity,
                    slot,
                    loadouts,
                })
            }),
            SWITCH_TO_LOADOUT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SwitchToLoadout {
                    header,
                    loadout: reader.u8()?,
                })
            }),
            SAVE_PAPERDOLL_TO_LOADOUT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SavePaperdollToLoadout {
                    header,
                    loadout: reader.u8()?,
                })
            }),
            REMOVE_ALL_ITEMS_FROM_LOADOUT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RemoveAllItemsFromLoadout {
                    header,
                    loadout: reader.u8()?,
                    flag: reader.u8()?,
                })
            }),
            RENAME_LOADOUT_TYPE_INDEX => read(|header, reader| {
                let loadout = reader.u8()?;
                let length = entries(reader, MAX_NAME_BYTES)?;
                // At most `MAX_NAME_BYTES`.
                let name = reader.bytes(length as usize)?.to_vec();
                Ok(Self::RenameLoadout {
                    header,
                    loadout,
                    name,
                })
            }),
            REMOVE_ITEM_FROM_ALL_LOADOUTS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RemoveItemFromAllLoadouts {
                    header,
                    key: reader.u16()?,
                })
            }),
            REMOVE_ITEM_FROM_LOADOUT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RemoveItemFromLoadout {
                    header,
                    loadout: reader.u8()?,
                    key: reader.u16()?,
                    slot: reader.u32()?,
                })
            }),
            SET_LOCK_STATE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetLockState {
                    header,
                    item: ItemRef::read(reader)?,
                    locked: reader.boolean()?,
                })
            }),
            SLOT_ITEM_WITH_CHARM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SlotItemWithCharm {
                    header,
                    target: ItemRef::read(reader)?,
                    charm: ItemRef::read(reader)?,
                    perk_index: reader.u32()?,
                    extra: reader.u32()?,
                })
            }),
            CLEAR_CHARM_REQUESTS_TYPE_INDEX => {
                read(|header, _| Ok(Self::ClearCharmRequests { header }))
            }
            DROP_INVENTORY_ITEMS_BY_CLASS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DropItemsByClass {
                    header,
                    class: reader.u64()?,
                    flag: reader.boolean()?,
                })
            }),
            GIVE_ITEMS_BY_CLASS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::GiveItemsByClass {
                    header,
                    container: ContainerRef::read(reader)?,
                    class: reader.u64()?,
                })
            }),
            TAKE_ITEMS_BY_CLASS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TakeItemsByClass {
                    header,
                    container: ContainerRef::read(reader)?,
                    class: reader.u64()?,
                })
            }),
            REQUEST_SEARCH_STORAGE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SearchStorage {
                    header,
                    search: reader.raw_id()?,
                    lead: reader.u8()?,
                    filter: reader.u32()?,
                    extra: reader.u32()?,
                    station: reader.u32()?,
                    tail: reader.u8()?,
                    refresh: reader.boolean()?,
                })
            }),
            REPLICATE_SEARCH_RESULTS_TYPE_INDEX => {
                read(|header, _| Ok(Self::ReplicateSearchResults { header }))
            }
            _ => return None,
        })
    }
}

/// 5497: the result of a loadout call: an `eLoadoutStatusCode` and the loadout index (retail
/// 11 after each 5579, 0 after each 5442, 7 and 8 after a 5427).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadoutStatusMsg {
    /// The header (the player's paperdoll, [`PAPERDOLL`]).
    pub header: FacetHeader,
    /// The status code.
    pub status: u32,
    /// The loadout index.
    pub loadout: u16,
}

impl LoadoutStatusMsg {
    /// Status `status` of loadout `loadout` for the player of entity number `player`.
    pub fn to(player: u64, status: u32, loadout: u8) -> Self {
        Self {
            header: FacetHeader::of_child(PAPERDOLL, PAPERDOLL_CHILD, player),
            status,
            loadout: u16::from(loadout),
        }
    }
}

impl BodyCodec for LoadoutStatusMsg {
    const TYPE_INDEX: u64 = LOADOUT_STATUS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.u32(self.status).u16(self.loadout).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            status: reader.u32()?,
            loadout: reader.u16()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gear::tests::hex;

    /// Retail open-world `item_actions`: a drop of one item of bag slot 365.
    const RETAIL_DROP: &str = concat!(
        "db742d107f23c591f7da088ca2d16c7c010000016dd1100e13ed10428d9604fbacc2a1e347ffffffff",
        "00000001"
    );
    /// Retail `storage_open_deposit_withdraw_transfer`: a deposit of 87 (0x57).
    const RETAIL_GIVE: &str = concat!(
        "121ae8fbb8b588ceb3ac442d2430908c8c9030242d44acb3463b76a400cfe344b3ac442d2430908cb0aac9",
        "01d32a4f508402aac46c8845f18c2763732c2aa4ee00000057"
    );
    /// Retail `Hiveloadouts`: owned key 11 into chest slot 1 of loadout 5.
    const RETAIL_ADD_TO_LOADOUT: &str = concat!(
        "75470b159dd87c55e34e14f344a974cc000000040000000b0000000000000000000000000000000000000001",
        "000000010105"
    );
    /// Retail `Hiveloadouts`: loadout 5 renamed.
    const RETAIL_RENAME: &str = "75470b159dd87c55e34e14f344a974cc050d48697665736861646f774d4d4f";
    /// Retail `item_actions`: the lock of bag slot 402.
    const RETAIL_LOCK: &str = concat!(
        "9bcbf835d6df0039f7da088ca2d16c7c0000000100000192289f09d416584e8a91fa53898c28be320000",
        "000001"
    );
    /// Retail `Hiveloadouts`: status 11 of loadout 5 to the player whose facet entity is
    /// `e34e14f344a974cc`.
    const RETAIL_STATUS: &str = "75470b159dd87c55e34e14f344a974cc0000000b0005";
    const PLAYER_FACET: u64 = 0xe34e_14f3_44a9_74cc;
    const LOADOUT: u8 = 5;

    fn decode(type_index: u64, text: &str) -> ItemCallMsg {
        ItemCallMsg::decode(type_index, &hex(text))
            .expect("an item call")
            .expect("reads")
    }

    #[test]
    fn the_drop_and_the_deposit_read_the_retail_bodies() {
        let ItemCallMsg::DropInventoryItems { entries, .. } =
            decode(DROP_INVENTORY_ITEMS_TYPE_INDEX, RETAIL_DROP)
        else {
            panic!("a drop");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].bag_slot, entries[0].extra, entries[0].quantity),
            (365, u32::MAX, 1)
        );
        let ItemCallMsg::GiveItem {
            container,
            quantity,
            ..
        } = decode(GIVE_ITEM_TYPE_INDEX, RETAIL_GIVE)
        else {
            panic!("a give");
        };
        assert_eq!(quantity, 87);
        assert_eq!(
            container.entity, 0xb3ac_442d_2430_908c,
            "the player's own entity"
        );
    }

    #[test]
    fn the_loadout_calls_read_the_retail_bodies() {
        let ItemCallMsg::AddItemToLoadouts {
            item,
            quantity,
            slot,
            loadouts,
            ..
        } = decode(ADD_ITEM_TO_LOADOUTS_TYPE_INDEX, RETAIL_ADD_TO_LOADOUT)
        else {
            panic!("an add");
        };
        assert_eq!(
            (item.location, item.slot, item.instance),
            (4, 11, [0; RAW_ID_LEN])
        );
        assert_eq!((quantity, slot, loadouts), (1, 1, vec![LOADOUT]));
        assert_eq!(
            decode(RENAME_LOADOUT_TYPE_INDEX, RETAIL_RENAME),
            ItemCallMsg::RenameLoadout {
                header: FacetHeader {
                    target: 0x7547_0b15_9dd8_7c55,
                    entity: PLAYER_FACET,
                },
                loadout: LOADOUT,
                name: b"HiveshadowMMO".to_vec(),
            }
        );
        let ItemCallMsg::SetLockState { item, locked, .. } =
            decode(SET_LOCK_STATE_TYPE_INDEX, RETAIL_LOCK)
        else {
            panic!("a lock");
        };
        assert_eq!(
            (item.location, item.slot, locked),
            (LOCATION_BAG, 402, true)
        );
        assert!(ItemCallMsg::decode(1, &[]).is_none());
    }

    /// Retail MSQ15: a gem from bag slot 120 into perk index 0 of the main-hand item.
    const RETAIL_SLOT_CHARM: &str = concat!(
        "40c610f1baaa2cb75ad5e1c8869e39e2",
        "00000002000000180000000000000000000000000000000000000000",
        "00000001000000780000000000000000000000000000000000000000",
        "0000000000000001"
    );
    /// Retail MSQ15, 5 s later: the same bag slot into perk index 1 of paperdoll slot 25.
    const RETAIL_SLOT_CHARM_SECOND_PERK: &str = concat!(
        "40c610f1baaa2cb75ad5e1c8869e39e2",
        "00000002000000190000000000000000000000000000000000000000",
        "00000001000000780000000000000000000000000000000000000000",
        "0000000100000001"
    );
    /// Retail: drop every `UI_Refining` (90) stack.
    const RETAIL_DROP_BY_CLASS: &str = "c4cbfad59c1a4a47e131d651bfc7f132000000000000005a01";
    /// Retail: give every `UI_Refining` stack to the open storage.
    const RETAIL_GIVE_BY_CLASS: &str = concat!(
        "c65677512e8438ec67e7d28299ff40aaaa40ff9982d2e767ddec0f76dbebbf82",
        "67e7d28299ff40aa04725b9870f51f76000000000000005a"
    );
    /// Retail MSQ16: a new smelting search.
    const RETAIL_SEARCH: &str = concat!(
        "fd34bbfc0f73dcfe5ad5e1c8869e39e21b86c5de7622407a92ee6430968861df",
        "0000000000000000002294152c0000"
    );
    /// Retail MSQ16: the results asked for again.
    const RETAIL_REPLICATE: &str = "fd34bbfc0f73dcfe5ad5e1c8869e39e2";
    /// CRC-32 of `smelting`, the station of [`RETAIL_SEARCH`].
    const SMELTING_CRC: u32 = 0x2294_152c;
    /// The UI class number of `UI_Refining`.
    const UI_REFINING: u64 = 90;

    #[test]
    fn the_charm_and_by_class_calls_read_the_retail_bodies() {
        let ItemCallMsg::SlotItemWithCharm {
            target,
            charm,
            perk_index,
            extra,
            ..
        } = decode(SLOT_ITEM_WITH_CHARM_TYPE_INDEX, RETAIL_SLOT_CHARM)
        else {
            panic!("a charm");
        };
        assert_eq!(
            (target.location, target.slot),
            (LOCATION_PAPERDOLL_SLOT, 24)
        );
        assert_eq!((charm.location, charm.slot), (LOCATION_BAG, 120));
        assert_eq!((perk_index, extra), (0, 1));
        let ItemCallMsg::SlotItemWithCharm {
            target, perk_index, ..
        } = decode(
            SLOT_ITEM_WITH_CHARM_TYPE_INDEX,
            RETAIL_SLOT_CHARM_SECOND_PERK,
        )
        else {
            panic!("a charm");
        };
        assert_eq!((target.slot, perk_index), (25, 1));
        assert!(matches!(
            decode(CLEAR_CHARM_REQUESTS_TYPE_INDEX, RETAIL_REPLICATE),
            ItemCallMsg::ClearCharmRequests { .. }
        ));
        assert!(matches!(
            decode(
                DROP_INVENTORY_ITEMS_BY_CLASS_TYPE_INDEX,
                RETAIL_DROP_BY_CLASS
            ),
            ItemCallMsg::DropItemsByClass {
                class: UI_REFINING,
                flag: true,
                ..
            }
        ));
        let ItemCallMsg::GiveItemsByClass { class, .. } =
            decode(GIVE_ITEMS_BY_CLASS_TYPE_INDEX, RETAIL_GIVE_BY_CLASS)
        else {
            panic!("a give by class");
        };
        assert_eq!(class, UI_REFINING);
        assert!(matches!(
            decode(TAKE_ITEMS_BY_CLASS_TYPE_INDEX, RETAIL_GIVE_BY_CLASS),
            ItemCallMsg::TakeItemsByClass {
                class: UI_REFINING,
                ..
            }
        ));
    }

    #[test]
    fn the_storage_search_calls_read_the_retail_bodies() {
        let ItemCallMsg::SearchStorage {
            filter,
            station,
            refresh,
            search,
            ..
        } = decode(REQUEST_SEARCH_STORAGE_TYPE_INDEX, RETAIL_SEARCH)
        else {
            panic!("a search");
        };
        assert_eq!((filter, station, refresh), (0, SMELTING_CRC, false));
        assert_eq!(search[0], 0x1b);
        assert!(matches!(
            decode(REPLICATE_SEARCH_RESULTS_TYPE_INDEX, RETAIL_REPLICATE),
            ItemCallMsg::ReplicateSearchResults { .. }
        ));
    }

    #[test]
    fn the_loadout_status_is_the_retail_body() {
        let target = (PLAYER_FACET ^ PAPERDOLL_CHILD).wrapping_add(PAPERDOLL);
        assert_eq!(target, 0x7547_0b15_9dd8_7c55, "the solved component");
        let status = LoadoutStatusMsg {
            header: FacetHeader {
                target,
                entity: PLAYER_FACET,
            },
            status: loadout_status::EQUIP_UPDATE_SUCCESS,
            loadout: u16::from(LOADOUT),
        };
        let retail = hex(RETAIL_STATUS);
        assert_eq!(status.encode().expect("encode"), retail);
        assert_eq!(LoadoutStatusMsg::decode(&retail), Ok(status));
    }
}
