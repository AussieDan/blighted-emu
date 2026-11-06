// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transmog and dye calls (run-15 items2 study): the client's skin calls (5692 enable, 5693
//! disable), its transmog screen flags (5302, 5978, 6105, 6463, 6486) and its paperdoll dye
//! (2224); the server's answers 5985 (skin enabled or disabled) and 6147 (a skin was dyed).
//!
//! Retail open world `Hive_Transmog`, `HiveChangeSkins`, `Hive_Dye`, `3v3`: every body starts
//! with a [`FacetHeader`]; integers are big-endian. A skin is named by the CRC-32 of its
//! lowercase appearance id (all 24 skins of the 5692 bodies matched an appearance id). The
//! server's 5985 and 6147 go to the player's transmog component ([`TRANSMOG`] on the child
//! [`TRANSMOG_CHILD`]).

use crate::facet::FacetHeader;
use crate::items::DYE_BYTES;
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `TransmogComponentServerFacet_EnableItemSkin`.
pub const ENABLE_ITEM_SKIN_TYPE_INDEX: u64 = 5692;
/// Type index of `TransmogComponentServerFacet_DisableItemSkin`.
pub const DISABLE_ITEM_SKIN_TYPE_INDEX: u64 = 5693;
/// Type index of `PaperdollComponentServerFacet_SetPreviewActive`.
pub const SET_PREVIEW_ACTIVE_TYPE_INDEX: u64 = 5302;
/// Type index of `PaperdollComponentServerFacet_SetHidingSkinOnSlot`.
pub const SET_HIDING_SKIN_ON_SLOT_TYPE_INDEX: u64 = 5978;
/// Type index of `PlayerComponentServerFacet_RequestSetIsTransmogScreenOpen`.
pub const TRANSMOG_SCREEN_OPEN_TYPE_INDEX: u64 = 6105;
/// Type index of `PlayerComponentServerFacet_RequestSetIsTransmogStationScreenOpen`.
pub const TRANSMOG_STATION_SCREEN_OPEN_TYPE_INDEX: u64 = 6463;
/// Type index of `PlayerComponentServerFacet_RequestSetIsArmorDyeingOpen`.
pub const ARMOR_DYEING_OPEN_TYPE_INDEX: u64 = 6486;
/// Type index of `CraftingComponentServerFacet_InitiatePaperdollDye`.
pub const PAPERDOLL_DYE_TYPE_INDEX: u64 = 2224;
/// Type index of `TransmogComponentClientFacet_OnEnableSkinSuccess`.
pub const ENABLE_SKIN_SUCCESS_TYPE_INDEX: u64 = 5985;
/// Type index of `TransmogComponentClientFacet_OnSkinDye`.
pub const SKIN_DYE_TYPE_INDEX: u64 = 6147;
/// Type index of `TransmogComponentClientFacet_OnUnlockTransmogSkin`.
pub const UNLOCK_TRANSMOG_SKIN_TYPE_INDEX: u64 = 5700;
/// The byte of a retail 5700 whose unlock worked.
pub const SKIN_UNLOCKED: u8 = 1;

/// The transmog component of the player, on its child [`TRANSMOG_CHILD`]: solved from the
/// transmog targets of 12 retail characters (the requester of their 5792 bodies and their
/// 5692/5985 headers; target = (entity XOR child) + component; the top bit and bit 0 of each
/// are open, and every choice gives the same targets).
pub const TRANSMOG: u64 = 0x9343_9459_9ef4_4469;
/// The XOR of the player prefab's main entity and the child that holds its transmog component.
pub const TRANSMOG_CHILD: u64 = 0x5aee_4c33_3d33_cbea;
/// Most entries of a dye request (one per paperdoll slot of the dye screen; retail 6).
const MAX_DYE_ENTRIES: u8 = 32;
const DYE_FIELD: &str = "paperdoll dye";

/// The transmog header of the player of entity number `player`.
pub fn transmog_header(player: u64) -> FacetHeader {
    FacetHeader::of_child(TRANSMOG, TRANSMOG_CHILD, player)
}

/// One slot of a dye request (2224).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DyeEntry {
    /// The paperdoll slot (0 to 4 armor, 27 off hand in retail bodies).
    pub slot: u32,
    /// The `DyeColorData` index of each channel (R, G, B, A); 0 is no dye.
    pub dye: [u8; DYE_BYTES],
    /// The skin the slot's item shows, when it shows one.
    pub skin: Option<u32>,
}

/// A transmog or dye call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransmogClientMsg {
    /// 5692: show skin `skin` over the item of paperdoll slot `slot`.
    EnableItemSkin {
        /// The header (the transmog component).
        header: FacetHeader,
        /// The paperdoll slot (0 to 4 armor, 24 main hand, 33 to 38 tools in retail bodies).
        slot: u32,
        /// The skin: CRC-32 of the lowercase appearance id.
        skin: u32,
    },
    /// 5693: show the item of paperdoll slot `slot` without its skin.
    DisableItemSkin {
        /// The header.
        header: FacetHeader,
        /// The paperdoll slot.
        slot: u32,
    },
    /// 5302: the transmog preview is on or off.
    SetPreviewActive {
        /// The header (the paperdoll component).
        header: FacetHeader,
        /// Whether the preview is on.
        active: bool,
    },
    /// 5978: hide or show the skin of paperdoll slot `slot`.
    SetHidingSkinOnSlot {
        /// The header.
        header: FacetHeader,
        /// The paperdoll slot.
        slot: u32,
        /// Whether the skin is hidden.
        hide: bool,
    },
    /// 6105: the transmog screen opened or closed.
    TransmogScreenOpen {
        /// The header (the player component).
        header: FacetHeader,
        /// Whether it is open.
        open: bool,
    },
    /// 6463: the transmog station screen opened or closed.
    TransmogStationScreenOpen {
        /// The header.
        header: FacetHeader,
        /// Whether it is open.
        open: bool,
    },
    /// 6486: the armor dyeing screen opened or closed.
    ArmorDyeingOpen {
        /// The header.
        header: FacetHeader,
        /// Whether it is open.
        open: bool,
    },
    /// 2224: dye the worn items.
    PaperdollDye {
        /// The header (the crafting component).
        header: FacetHeader,
        /// One entry per slot of the dye screen.
        entries: Vec<DyeEntry>,
        /// A number of unknown meaning (0 in every retail body).
        extra: u32,
        /// The bag slots of the dye items the request uses.
        bag_slots: Vec<u32>,
    },
}

impl TransmogClientMsg {
    /// Decodes `body` when `type_index` is a transmog or dye call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            ENABLE_ITEM_SKIN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::EnableItemSkin {
                    header,
                    slot: reader.u32()?,
                    skin: reader.u32()?,
                })
            }),
            DISABLE_ITEM_SKIN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DisableItemSkin {
                    header,
                    slot: reader.u32()?,
                })
            }),
            SET_PREVIEW_ACTIVE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetPreviewActive {
                    header,
                    active: reader.boolean()?,
                })
            }),
            SET_HIDING_SKIN_ON_SLOT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetHidingSkinOnSlot {
                    header,
                    slot: reader.u32()?,
                    hide: reader.boolean()?,
                })
            }),
            TRANSMOG_SCREEN_OPEN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TransmogScreenOpen {
                    header,
                    open: reader.boolean()?,
                })
            }),
            TRANSMOG_STATION_SCREEN_OPEN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TransmogStationScreenOpen {
                    header,
                    open: reader.boolean()?,
                })
            }),
            ARMOR_DYEING_OPEN_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ArmorDyeingOpen {
                    header,
                    open: reader.boolean()?,
                })
            }),
            PAPERDOLL_DYE_TYPE_INDEX => read(|header, reader| {
                let count = reader.u8()?;
                if count > MAX_DYE_ENTRIES {
                    return Err(MessageError::CountTooLarge {
                        count: u64::from(count),
                        limit: u64::from(MAX_DYE_ENTRIES),
                    });
                }
                let entries = (0..count)
                    .map(|_| {
                        let slot = reader.u32()?;
                        let dye = reader.array()?;
                        let skin = if reader.boolean()? {
                            Some(reader.u32()?)
                        } else {
                            None
                        };
                        Ok(DyeEntry { slot, dye, skin })
                    })
                    .collect::<Result<_, MessageError>>()?;
                let extra = reader.u32()?;
                let slots = reader.u8()?;
                let bag_slots = (0..slots)
                    .map(|_| reader.u32())
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::PaperdollDye {
                    header,
                    entries,
                    extra,
                    bag_slots,
                })
            }),
            _ => return None,
        })
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::EnableItemSkin { .. } => ENABLE_ITEM_SKIN_TYPE_INDEX,
            Self::DisableItemSkin { .. } => DISABLE_ITEM_SKIN_TYPE_INDEX,
            Self::SetPreviewActive { .. } => SET_PREVIEW_ACTIVE_TYPE_INDEX,
            Self::SetHidingSkinOnSlot { .. } => SET_HIDING_SKIN_ON_SLOT_TYPE_INDEX,
            Self::TransmogScreenOpen { .. } => TRANSMOG_SCREEN_OPEN_TYPE_INDEX,
            Self::TransmogStationScreenOpen { .. } => TRANSMOG_STATION_SCREEN_OPEN_TYPE_INDEX,
            Self::ArmorDyeingOpen { .. } => ARMOR_DYEING_OPEN_TYPE_INDEX,
            Self::PaperdollDye { .. } => PAPERDOLL_DYE_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] when a dye request has more entries than the
    /// reader takes or more than `u8::MAX` bag slots.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        match self {
            Self::EnableItemSkin { header, slot, skin } => {
                header.write(&mut writer);
                writer.u32(*slot).u32(*skin);
            }
            Self::DisableItemSkin { header, slot } => {
                header.write(&mut writer);
                writer.u32(*slot);
            }
            Self::SetPreviewActive {
                header,
                active: flag,
            }
            | Self::TransmogScreenOpen { header, open: flag }
            | Self::TransmogStationScreenOpen { header, open: flag }
            | Self::ArmorDyeingOpen { header, open: flag } => {
                header.write(&mut writer);
                writer.boolean(*flag);
            }
            Self::SetHidingSkinOnSlot { header, slot, hide } => {
                header.write(&mut writer);
                writer.u32(*slot).boolean(*hide);
            }
            Self::PaperdollDye {
                header,
                entries,
                extra,
                bag_slots,
            } => {
                if entries.len() > usize::from(MAX_DYE_ENTRIES) {
                    return Err(MessageError::CountTooLarge {
                        count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
                        limit: u64::from(MAX_DYE_ENTRIES),
                    });
                }
                header.write(&mut writer);
                writer.u8_count(entries.len())?;
                for entry in entries {
                    writer.u32(entry.slot).bytes(&entry.dye);
                    match entry.skin {
                        Some(skin) => writer.boolean(true).u32(skin),
                        None => writer.boolean(false),
                    };
                }
                writer.u32(*extra).u8_count(bag_slots.len())?;
                for bag_slot in bag_slots {
                    writer.u32(*bag_slot);
                }
            }
        }
        Ok(writer.finish())
    }
}

/// A transmog answer of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmogServerMsg {
    /// 5985: skin `skin` (0 after a disable) shows over the item of paperdoll slot `slot`.
    EnableSkinSuccess {
        /// The header (the transmog component).
        header: FacetHeader,
        /// The skin; 0 after a disable.
        skin: u32,
        /// The paperdoll slot.
        slot: u32,
    },
    /// 6147: skin `skin` was dyed.
    SkinDye {
        /// The header (the transmog component).
        header: FacetHeader,
        /// The skin.
        skin: u32,
        /// Whether the dye took (retail: true in all 9 bodies).
        ok: bool,
    },
    /// 5700: a transmog skin was unlocked (the byte is [`SKIN_UNLOCKED`] in retail).
    UnlockTransmogSkin {
        /// The header (the transmog component).
        header: FacetHeader,
        /// Whether the unlock worked.
        unlocked: u8,
    },
}

impl TransmogServerMsg {
    /// Whether `type_index` is a transmog answer of the server.
    pub const fn is_type(type_index: u64) -> bool {
        matches!(
            type_index,
            ENABLE_SKIN_SUCCESS_TYPE_INDEX | SKIN_DYE_TYPE_INDEX | UNLOCK_TRANSMOG_SKIN_TYPE_INDEX
        )
    }

    /// The type index of the answer.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::EnableSkinSuccess { .. } => ENABLE_SKIN_SUCCESS_TYPE_INDEX,
            Self::SkinDye { .. } => SKIN_DYE_TYPE_INDEX,
            Self::UnlockTransmogSkin { .. } => UNLOCK_TRANSMOG_SKIN_TYPE_INDEX,
        }
    }

    /// Decodes the body of the answer of `type_index`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body does not match its layout or `type_index` is not a
    /// transmog answer ([`MessageError::BadTag`]).
    pub fn decode(type_index: u64, body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let message = match type_index {
            ENABLE_SKIN_SUCCESS_TYPE_INDEX => Self::EnableSkinSuccess {
                header,
                skin: reader.u32()?,
                slot: reader.u32()?,
            },
            SKIN_DYE_TYPE_INDEX => Self::SkinDye {
                header,
                skin: reader.u32()?,
                ok: reader.boolean()?,
            },
            UNLOCK_TRANSMOG_SKIN_TYPE_INDEX => Self::UnlockTransmogSkin {
                header,
                unlocked: reader.u8()?,
            },
            _ => {
                return Err(MessageError::BadTag {
                    field: DYE_FIELD,
                    tag: u8::try_from(type_index).unwrap_or(u8::MAX),
                });
            }
        };
        reader.finish()?;
        Ok(message)
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::EnableSkinSuccess { header, skin, slot } => {
                header.write(&mut writer);
                writer.u32(*skin).u32(*slot);
            }
            Self::SkinDye { header, skin, ok } => {
                header.write(&mut writer);
                writer.u32(*skin).boolean(*ok);
            }
            Self::UnlockTransmogSkin { header, unlocked } => {
                header.write(&mut writer);
                writer.u8(*unlocked);
            }
        }
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gear::tests::hex;

    /// Retail `Hive_Transmog`: skin `b0518525` on slot 0, its answer, the disable of slot 1 and
    /// its answer (the player's facet entity `e34e14f344a974cc`).
    const RETAIL_ENABLE: &str = "4ce3ed1a188f038fe34e14f344a974cc00000000b0518525";
    const RETAIL_ENABLED: &str = "4ce3ed1a188f038fe34e14f344a974ccb051852500000000";
    const RETAIL_DISABLE: &str = "4ce3ed1a188f038fe34e14f344a974cc00000001";
    const RETAIL_DISABLED: &str = "4ce3ed1a188f038fe34e14f344a974cc0000000000000001";
    const RETAIL_SKIN_DYE: &str = "4ce3ed1a188f038fe34e14f344a974ccb051852501";
    /// Retail `Hive_Dye`: six slots, five with skins, then five bag slots of dye items.
    const RETAIL_DYE: &str = concat!(
        "d73338cb6d8e32a6e34e14f344a974cc060000000006943e2001b051852500000001287e82680149ee1aec",
        "000000022830303901d4849aa3000000033e3e007e01eaba3033000000042c2c2c0601295527760000001b",
        "0000000000000000000500000052000000500000003a0000005100000053"
    );
    const PLAYER_FACET: u64 = 0xe34e_14f3_44a9_74cc;
    const RETAIL_TRANSMOG_TARGET: u64 = 0x4ce3_ed1a_188f_038f;
    const SKIN: u32 = 0xb051_8525;

    /// Decodes the call of `type_index` in the bytes of `text` and checks that it names that
    /// index and writes back those bytes.
    fn decode(type_index: u64, text: &str) -> TransmogClientMsg {
        let body = hex(text);
        let call = TransmogClientMsg::decode(type_index, &body)
            .expect("a transmog call")
            .expect("reads");
        assert_eq!(call.type_index(), type_index);
        assert_eq!(call.encode().expect("encode"), body);
        call
    }

    #[test]
    fn the_transmog_target_is_the_solved_component() {
        assert_eq!(
            (PLAYER_FACET ^ TRANSMOG_CHILD).wrapping_add(TRANSMOG),
            RETAIL_TRANSMOG_TARGET
        );
    }

    #[test]
    fn the_skin_calls_read_the_retail_bodies() {
        let TransmogClientMsg::EnableItemSkin { slot, skin, header } =
            decode(ENABLE_ITEM_SKIN_TYPE_INDEX, RETAIL_ENABLE)
        else {
            panic!("an enable");
        };
        assert_eq!(
            (slot, skin, header.target),
            (0, SKIN, RETAIL_TRANSMOG_TARGET)
        );
        assert!(matches!(
            decode(DISABLE_ITEM_SKIN_TYPE_INDEX, RETAIL_DISABLE),
            TransmogClientMsg::DisableItemSkin { slot: 1, .. }
        ));
        let flag = "4ce3ed1a188f038fe34e14f344a974cc01";
        assert!(matches!(
            decode(TRANSMOG_SCREEN_OPEN_TYPE_INDEX, flag),
            TransmogClientMsg::TransmogScreenOpen { open: true, .. }
        ));
        assert!(matches!(
            decode(
                SET_HIDING_SKIN_ON_SLOT_TYPE_INDEX,
                "4ce3ed1a188f038fe34e14f344a974cc0000000200"
            ),
            TransmogClientMsg::SetHidingSkinOnSlot {
                slot: 2,
                hide: false,
                ..
            }
        ));
        assert!(TransmogClientMsg::decode(1, &[]).is_none());
    }

    #[test]
    fn the_dye_request_reads_the_retail_body() {
        let TransmogClientMsg::PaperdollDye {
            entries,
            extra,
            bag_slots,
            ..
        } = decode(PAPERDOLL_DYE_TYPE_INDEX, RETAIL_DYE)
        else {
            panic!("a dye");
        };
        assert_eq!(entries.len(), 6);
        assert_eq!(
            entries[0],
            DyeEntry {
                slot: 0,
                dye: [0x06, 0x94, 0x3e, 0x20],
                skin: Some(SKIN),
            }
        );
        assert_eq!(
            entries[5],
            DyeEntry {
                slot: 27,
                dye: [0; DYE_BYTES],
                skin: None,
            }
        );
        assert_eq!(extra, 0);
        assert_eq!(bag_slots, [0x52, 0x50, 0x3a, 0x51, 0x53]);
    }

    #[test]
    fn every_transmog_call_writes_what_it_reads() {
        let header = FacetHeader {
            target: RETAIL_TRANSMOG_TARGET,
            entity: PLAYER_FACET,
        };
        let calls = [
            TransmogClientMsg::EnableItemSkin {
                header,
                slot: 24,
                skin: SKIN,
            },
            TransmogClientMsg::DisableItemSkin { header, slot: 3 },
            TransmogClientMsg::SetPreviewActive {
                header,
                active: true,
            },
            TransmogClientMsg::SetHidingSkinOnSlot {
                header,
                slot: 4,
                hide: true,
            },
            TransmogClientMsg::TransmogScreenOpen { header, open: true },
            TransmogClientMsg::TransmogStationScreenOpen { header, open: true },
            TransmogClientMsg::ArmorDyeingOpen { header, open: true },
            TransmogClientMsg::PaperdollDye {
                header,
                entries: vec![
                    DyeEntry {
                        slot: 1,
                        dye: [1, 2, 3, 4],
                        skin: Some(SKIN),
                    },
                    DyeEntry {
                        slot: 27,
                        dye: [5, 6, 7, 8],
                        skin: None,
                    },
                ],
                extra: 9,
                bag_slots: vec![0x52, 0x53],
            },
        ];
        for call in calls {
            let body = call.encode().expect("encode");
            assert_eq!(
                TransmogClientMsg::decode(call.type_index(), &body),
                Some(Ok(call))
            );
        }
        let too_many = TransmogClientMsg::PaperdollDye {
            header,
            entries: vec![
                DyeEntry {
                    slot: 0,
                    dye: [0; DYE_BYTES],
                    skin: None,
                };
                usize::from(MAX_DYE_ENTRIES) + 1
            ],
            extra: 0,
            bag_slots: Vec::new(),
        };
        assert!(matches!(
            too_many.encode(),
            Err(MessageError::CountTooLarge { .. })
        ));
    }

    /// Retail `rafflebones2`: a transmog skin unlock.
    const RETAIL_UNLOCK: &str = "d04d330b43c0cfa967e7d28299ff40aa01";

    #[test]
    fn the_unlock_answer_is_the_retail_body() {
        let bytes = hex(RETAIL_UNLOCK);
        let answer =
            TransmogServerMsg::decode(UNLOCK_TRANSMOG_SKIN_TYPE_INDEX, &bytes).expect("reads");
        assert!(matches!(
            answer,
            TransmogServerMsg::UnlockTransmogSkin {
                unlocked: SKIN_UNLOCKED,
                ..
            }
        ));
        assert_eq!(answer.encode(), bytes);
        assert!(TransmogServerMsg::is_type(UNLOCK_TRANSMOG_SKIN_TYPE_INDEX));
    }

    #[test]
    fn the_answers_are_the_retail_bodies() {
        let header = FacetHeader {
            target: RETAIL_TRANSMOG_TARGET,
            entity: PLAYER_FACET,
        };
        for (message, retail) in [
            (
                TransmogServerMsg::EnableSkinSuccess {
                    header,
                    skin: SKIN,
                    slot: 0,
                },
                RETAIL_ENABLED,
            ),
            (
                TransmogServerMsg::EnableSkinSuccess {
                    header,
                    skin: 0,
                    slot: 1,
                },
                RETAIL_DISABLED,
            ),
            (
                TransmogServerMsg::SkinDye {
                    header,
                    skin: SKIN,
                    ok: true,
                },
                RETAIL_SKIN_DYE,
            ),
        ] {
            let bytes = hex(retail);
            assert_eq!(message.encode(), bytes);
            assert_eq!(
                TransmogServerMsg::decode(message.type_index(), &bytes),
                Ok(message)
            );
            assert!(TransmogServerMsg::is_type(message.type_index()));
        }
    }
}
