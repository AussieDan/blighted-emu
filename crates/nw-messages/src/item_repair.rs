// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's repair and gear score upgrade calls (`ItemRepairComponentServerFacet`, run-15
//! items2 study). The server answers none of them with a message of its own (no
//! `ItemRepairComponentClientFacet` type exists): the item record, the coins and the
//! categorical progression points change in the next bundle.
//!
//! Layouts: 3008 from 11 retail bodies in 6 ledgers (open-world
//! `upgrade_artifact_at_gypsum_kiln`: header, item reference, `u16` gear score from, `u16` to);
//! 3003, 3007 and 2079 are in no capture: their shapes are the Azoth catalog's (3003:
//! the 28-byte item reference of 2549 and a flag byte; 3007: the header alone; 2079: one
//! `u64`) and the flag of 3003 is the client check `FUN_142eb1190`'s "use a repair kit"
//! (Inferred).

use crate::facet::FacetHeader;
use crate::item_calls::ItemRef;
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `ItemRepairComponentServerFacet_InitiateRepair`.
pub const INITIATE_REPAIR_TYPE_INDEX: u64 = 3003;
/// Type index of `ItemRepairComponentServerFacet_RepairAllEquipment`.
pub const REPAIR_ALL_EQUIPMENT_TYPE_INDEX: u64 = 3007;
/// Type index of `ItemRepairComponentServerFacet_RepairAllItemClass`.
pub const REPAIR_ALL_ITEM_CLASS_TYPE_INDEX: u64 = 2079;
/// Type index of `ItemRepairComponentServerFacet_RequestUpgradeItemGearScore`.
pub const UPGRADE_GEAR_SCORE_TYPE_INDEX: u64 = 3008;

/// A repair or upgrade call of the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRepairMsg {
    /// 3003: repair one item.
    InitiateRepair {
        /// The header.
        header: FacetHeader,
        /// The item.
        item: ItemRef,
        /// Repair it with a repair kit of its tier instead of parts and coins.
        use_kit: bool,
    },
    /// 3007: repair every worn item.
    RepairAllEquipment {
        /// The header.
        header: FacetHeader,
    },
    /// 2079: repair every item of a UI item class (`nw_content::ui_item_class`).
    RepairAllItemClass {
        /// The header.
        header: FacetHeader,
        /// The UI item class number.
        item_class: u64,
    },
    /// 3008: raise an item's gear score.
    UpgradeGearScore {
        /// The header.
        header: FacetHeader,
        /// The item: location 1 a bag slot, 2 an owned-items key.
        item: ItemRef,
        /// The gear score it has.
        from: u16,
        /// The gear score asked for.
        to: u16,
    },
}

impl ItemRepairMsg {
    /// Decodes `body` when `type_index` is a repair or upgrade call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            INITIATE_REPAIR_TYPE_INDEX => read(|header, reader| {
                Ok(Self::InitiateRepair {
                    header,
                    item: ItemRef::read(reader)?,
                    use_kit: reader.boolean()?,
                })
            }),
            REPAIR_ALL_EQUIPMENT_TYPE_INDEX => {
                read(|header, _| Ok(Self::RepairAllEquipment { header }))
            }
            REPAIR_ALL_ITEM_CLASS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RepairAllItemClass {
                    header,
                    item_class: reader.u64()?,
                })
            }),
            UPGRADE_GEAR_SCORE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::UpgradeGearScore {
                    header,
                    item: ItemRef::read(reader)?,
                    from: reader.u16()?,
                    to: reader.u16()?,
                })
            }),
            _ => return None,
        })
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::InitiateRepair { .. } => INITIATE_REPAIR_TYPE_INDEX,
            Self::RepairAllEquipment { .. } => REPAIR_ALL_EQUIPMENT_TYPE_INDEX,
            Self::RepairAllItemClass { .. } => REPAIR_ALL_ITEM_CLASS_TYPE_INDEX,
            Self::UpgradeGearScore { .. } => UPGRADE_GEAR_SCORE_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::InitiateRepair {
                header,
                item,
                use_kit,
            } => {
                header.write(&mut writer);
                item.write(&mut writer);
                writer.boolean(*use_kit);
            }
            Self::RepairAllEquipment { header } => header.write(&mut writer),
            Self::RepairAllItemClass { header, item_class } => {
                header.write(&mut writer);
                writer.u64(*item_class);
            }
            Self::UpgradeGearScore {
                header,
                item,
                from,
                to,
            } => {
                header.write(&mut writer);
                item.write(&mut writer);
                writer.u16(*from).u16(*to);
            }
        }
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gear::tests::hex;
    use crate::item_calls::LOCATION_BAG;
    use crate::wire::RAW_ID_LEN;

    /// The header of a retail item-repair call (`item_actions` 2549).
    const HEADER: &str = "9bcbf835d6df0039f7da088ca2d16c7c";
    /// An item reference to bag slot 32 with instance id 0x11.. (retail shape).
    const BAG_REF: &str = concat!(
        "00000001",
        "00000020",
        "11111111111111111111111111111111",
        "00000000"
    );
    /// Retail 3008 tail: gear score 700 to 701.
    const UPGRADE_TAIL: &str = "02bc02bd";

    fn body(parts: &[&str]) -> Vec<u8> {
        hex(&parts.concat())
    }

    #[test]
    fn the_upgrade_reads_the_retail_shape() {
        let bytes = body(&[HEADER, BAG_REF, UPGRADE_TAIL]);
        // Header 16, item reference 28, two u16: the retail 48 bytes.
        assert_eq!(bytes.len(), 48);
        let message = ItemRepairMsg::decode(UPGRADE_GEAR_SCORE_TYPE_INDEX, &bytes)
            .expect("a repair call")
            .expect("reads");
        let ItemRepairMsg::UpgradeGearScore { item, from, to, .. } = message else {
            panic!("an upgrade");
        };
        assert_eq!((item.location, item.slot), (LOCATION_BAG, 32));
        assert_eq!(item.instance, [0x11; RAW_ID_LEN]);
        assert_eq!((from, to), (700, 701));
        assert_eq!(message.type_index(), UPGRADE_GEAR_SCORE_TYPE_INDEX);
        assert_eq!(message.encode(), bytes);
    }

    #[test]
    fn the_repairs_read_their_catalog_shapes() {
        let repair = body(&[HEADER, BAG_REF, "01"]);
        assert!(matches!(
            ItemRepairMsg::decode(INITIATE_REPAIR_TYPE_INDEX, &repair),
            Some(Ok(ItemRepairMsg::InitiateRepair { use_kit: true, .. }))
        ));
        let all = body(&[HEADER]);
        assert!(matches!(
            ItemRepairMsg::decode(REPAIR_ALL_EQUIPMENT_TYPE_INDEX, &all),
            Some(Ok(ItemRepairMsg::RepairAllEquipment { .. }))
        ));
        let class = body(&[HEADER, "0000000000000047"]);
        assert!(matches!(
            ItemRepairMsg::decode(REPAIR_ALL_ITEM_CLASS_TYPE_INDEX, &class),
            Some(Ok(ItemRepairMsg::RepairAllItemClass { item_class: 71, .. }))
        ));
        let long = body(&[HEADER, "00"]);
        assert!(matches!(
            ItemRepairMsg::decode(REPAIR_ALL_EQUIPMENT_TYPE_INDEX, &long),
            Some(Err(_))
        ));
        assert!(ItemRepairMsg::decode(1, &all).is_none());
    }

    #[test]
    fn every_repair_call_writes_what_it_reads() {
        let header = FacetHeader {
            target: 0x9bcb_f835_d6df_0039,
            entity: 0xf7da_088c_a2d1_6c7c,
        };
        let item = ItemRef {
            location: LOCATION_BAG,
            slot: 32,
            instance: [0x11; RAW_ID_LEN],
            extra: 5,
        };
        let calls = [
            ItemRepairMsg::InitiateRepair {
                header,
                item,
                use_kit: true,
            },
            ItemRepairMsg::RepairAllEquipment { header },
            ItemRepairMsg::RepairAllItemClass {
                header,
                item_class: 71,
            },
            ItemRepairMsg::UpgradeGearScore {
                header,
                item,
                from: 700,
                to: 701,
            },
        ];
        for call in calls {
            assert_eq!(
                ItemRepairMsg::decode(call.type_index(), &call.encode()),
                Some(Ok(call))
            );
        }
    }
}
