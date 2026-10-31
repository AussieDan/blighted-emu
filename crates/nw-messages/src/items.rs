// SPDX-License-Identifier: AGPL-3.0-or-later
//! The item record and the replicated list form that the paperdoll (type 3183), the containers
//! (type 1755) and the owned items (type 5437) share (paperdoll-inventory note, sections 1, 2
//! and 4).
//!
//! An item starts with one packed big-endian `u64`:
//! `(((quantity * 64 + syncReason) * 502 + containerSlot) * 2^20 + itemIndex) * 8001 + durability`,
//! with container slot 501 for "in no container". Eight variable-length numbers follow (max
//! durability, a per-item counter, gear score, five perk slots), then four dye bytes, the 16-byte
//! instance id, a flags byte, and the transmog id and dye (retail servers send the transmog bytes
//! on every item).
//!
//! A list is sent whole ("full form": a count of 0, a version, then the elements) or as changes
//! ("delta form"). This server sends and reads the full form; a delta form is
//! [`MessageError::Unsized`], so a bundle walk over a capture stops there.

use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// Perk slots of an item.
pub const PERK_SLOTS: usize = 5;
/// Bytes of a dye set.
pub const DYE_BYTES: usize = 4;
/// Sync reason of an item in a full state at login (paperdoll and containers).
pub const SYNC_LOGIN: u8 = 4;
/// Sync reason of an item listed in a lootable container (retail beach bag fill).
pub const SYNC_CONTAINER: u8 = 3;
/// Sync reason of an item that moved mid-session: into a bag (retail potions) or out of a
/// taken container (the beach bag's empty records).
pub const SYNC_MOVED: u8 = 6;
/// Sync reason of the empty record a removed worn item leaves in its visible slot (retail
/// 2026-09-18: the tutorial sword taken after the ship's sinking).
pub const SYNC_REMOVED: u8 = 5;
/// Sync reason of an owned item at login (type 5437).
pub const SYNC_OWNED: u8 = 17;
/// Sync reason of a bag stack that a craft used (retail `12-2_skinningknife`, 1790514751716 ms:
/// the flint and the wood of the skinning knife; `12-3_boarfeast`: the meat of the rations).
pub const SYNC_CRAFT_USED: u8 = 2;
/// Sync reason of a bag stack that a craft made (retail, same bundles: the skinning knife, the
/// 2 light rations). With another reason the live client said "Crafting attempt failed" after
/// each good craft and cancelled it (2026-10-29 play test).
pub const SYNC_CRAFTED: u8 = 3;
/// Sync reason of the bag stack a salvage took from, or of its emptied slot (retail
/// `MSQ6-lv28` 888.480 s, `Playthrough_Part_03` 218.774 s, `20261008-145727-200` 699.612 s: 20 of
/// 20 salvages).
pub const SYNC_SALVAGED: u8 = 24;
/// Sync reason of the bag stacks a salvage of an item without durability gave: a loot
/// container's (retail `refining_all_stations_single_batch_territorylvlup` 224.081 s and
/// 226.932 s, the reward containers crafted at 94.596 s and 99.997 s; `Playthrough_Part_03`
/// 218.774 s, a `ConcealedVaultLVL30`; `20261008-145727-200` 1625.280 s and 1635.714 s).
pub const SYNC_SALVAGE_OPENED: u8 = 26;
/// Sync reason of the bag stacks a salvage of an item with durability gave (retail
/// `20261008-145727-200` 1622.944 s: a gear item of durability 1500 and gear score 350).
pub const SYNC_SALVAGE_GEAR: u8 = 33;
/// Item flags bit 0: bound to the character.
pub const ITEM_BOUND: u8 = 0x01;
/// Item flags bit 1: locked by its owner (retail 2549 set it and cleared it: flags 1 to 3 and
/// back, open-world `item_actions`).
pub const ITEM_LOCKED: u8 = 0x02;
/// The version that retail first (full) states carry.
pub const FIRST_VERSION: u64 = 3;

/// Values of the sync reason: 6 bits.
const SYNC_REASONS: u64 = 64;
/// Container slots: 0 to 500, and 501 for "in no container".
const CONTAINER_SLOTS: u64 = 502;
/// The container slot value of an item that is in no container.
const NO_CONTAINER: u64 = CONTAINER_SLOTS - 1;
/// Bits of the item index.
const ITEM_INDEX_BITS: u32 = 20;
/// Durability values: 0 to 8000.
const DURABILITY_VALUES: u64 = 8001;
/// A list's first number: 0 is the full form.
pub(crate) const FULL_FORM: u64 = 0;
/// The version flag byte: a version follows.
pub(crate) const VERSION_PRESENT: u8 = 1;
/// Most elements of a full list (`FUN_1440e8f30` limit).
const MAX_FULL_ELEMENTS: u64 = 0x200_0000;
const FIELD: &str = "replicated list";

/// One item (`FUN_145ce8c40` / `FUN_145c734a0`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemRecord {
    /// Stack size; 0 is an empty container slot.
    pub quantity: u32,
    /// Why the item is sent (6 bits): [`SYNC_LOGIN`], [`SYNC_OWNED`], ...
    pub sync_reason: u8,
    /// The container slot, or `None` for an item in no container.
    pub container_slot: Option<u16>,
    /// The client item index (20 bits); 0 is no item.
    pub item_index: u32,
    /// Current durability, 0 to 8000.
    pub durability: u32,
    /// Full durability.
    pub max_durability: u32,
    /// A number that grows per new item of the character (meaning unknown).
    pub counter: u32,
    /// Gear score.
    pub gear_score: u32,
    /// Perk table indexes.
    pub perks: [u32; PERK_SLOTS],
    /// Dye.
    pub dye: [u8; DYE_BYTES],
    /// The item instance id.
    pub instance: [u8; RAW_ID_LEN],
    /// Flags: [`ITEM_BOUND`], ...
    pub flags: u8,
    /// Transmog id.
    pub transmog: u32,
    /// Transmog dye.
    pub transmog_dye: [u8; DYE_BYTES],
}

impl ItemRecord {
    /// An empty container slot as retail sends it.
    pub const EMPTY_SLOT: Self = Self {
        quantity: 0,
        sync_reason: SYNC_LOGIN,
        container_slot: None,
        item_index: 0,
        durability: 0,
        max_durability: 0,
        counter: 0,
        gear_score: 0,
        perks: [0; PERK_SLOTS],
        dye: [0; DYE_BYTES],
        instance: [0; RAW_ID_LEN],
        flags: 0,
        transmog: 0,
        transmog_dye: [0; DYE_BYTES],
    };

    /// Reads one item.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the item ends early or a number is too large.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let packed = reader.u64()?;
        let durability = packed % DURABILITY_VALUES;
        let rest = packed / DURABILITY_VALUES;
        let item_index = rest & ((1 << ITEM_INDEX_BITS) - 1);
        let rest = rest >> ITEM_INDEX_BITS;
        let slot = rest % CONTAINER_SLOTS;
        let rest = rest / CONTAINER_SLOTS;
        let mut item = Self {
            quantity: narrow(rest / SYNC_REASONS)?,
            // Below 64.
            sync_reason: (rest % SYNC_REASONS) as u8,
            // Below 501.
            container_slot: (slot != NO_CONTAINER).then_some(slot as u16),
            // 20 bits and below 8001.
            item_index: item_index as u32,
            durability: durability as u32,
            max_durability: read_number(reader)?,
            counter: read_number(reader)?,
            gear_score: read_number(reader)?,
            ..Self::EMPTY_SLOT
        };
        for perk in &mut item.perks {
            *perk = read_number(reader)?;
        }
        item.dye = reader.array()?;
        item.instance = reader.raw_id()?;
        item.flags = reader.u8()?;
        item.transmog = reader.u32()?;
        item.transmog_dye = reader.array()?;
        Ok(item)
    }

    /// Writes the item.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] when a packed field does not fit its range.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        writer.u64(self.packed()?);
        for number in [self.max_durability, self.counter, self.gear_score]
            .into_iter()
            .chain(self.perks)
        {
            writer.wide_prefixed(u64::from(number));
        }
        writer
            .bytes(&self.dye)
            .bytes(&self.instance)
            .u8(self.flags)
            .u32(self.transmog)
            .bytes(&self.transmog_dye);
        Ok(())
    }

    fn packed(&self) -> Result<u64, MessageError> {
        let slot = self.container_slot.map_or(NO_CONTAINER, u64::from);
        within(slot, NO_CONTAINER)?;
        within(u64::from(self.sync_reason), SYNC_REASONS - 1)?;
        within(u64::from(self.item_index), (1 << ITEM_INDEX_BITS) - 1)?;
        within(u64::from(self.durability), DURABILITY_VALUES - 1)?;
        let overflow = || MessageError::CountTooLarge {
            count: u64::from(self.quantity),
            limit: (u64::MAX / (SYNC_REASONS * CONTAINER_SLOTS * DURABILITY_VALUES))
                >> ITEM_INDEX_BITS,
        };
        u64::from(self.quantity)
            .checked_mul(SYNC_REASONS)
            .map(|value| value + u64::from(self.sync_reason))
            .and_then(|value| value.checked_mul(CONTAINER_SLOTS))
            .map(|value| value + slot)
            .and_then(|value| value.checked_mul(1 << ITEM_INDEX_BITS))
            .map(|value| value + u64::from(self.item_index))
            .and_then(|value| value.checked_mul(DURABILITY_VALUES))
            .and_then(|value| value.checked_add(u64::from(self.durability)))
            .ok_or_else(overflow)
    }
}

/// `value` is at most `limit`.
fn within(value: u64, limit: u64) -> Result<(), MessageError> {
    if value > limit {
        return Err(MessageError::CountTooLarge {
            count: value,
            limit,
        });
    }
    Ok(())
}

fn narrow(value: u64) -> Result<u32, MessageError> {
    u32::try_from(value).map_err(|_| MessageError::CountTooLarge {
        count: value,
        limit: u64::from(u32::MAX),
    })
}

/// One variable-length number of at most 32 bits (`FUN_14087b5c0`; the same bytes as the wide
/// prefix code below 2^32).
pub(crate) fn read_number(reader: &mut Reader<'_>) -> Result<u32, MessageError> {
    narrow(reader.wide_prefixed()?)
}

/// A list in its full form: the version and the elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullList<T> {
    /// The version; `None` sends the flag byte 0.
    pub version: Option<u64>,
    /// The elements.
    pub elements: Vec<T>,
}

impl<T> FullList<T> {
    /// A first state's list: [`FIRST_VERSION`] and `elements`.
    pub const fn first(elements: Vec<T>) -> Self {
        Self {
            version: Some(FIRST_VERSION),
            elements,
        }
    }

    /// Reads a full list whose elements `read_element` reads.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Unsized`] for a delta form, or the error of an element.
    pub fn read(
        reader: &mut Reader<'_>,
        mut read_element: impl FnMut(&mut Reader<'_>) -> Result<T, MessageError>,
    ) -> Result<Self, MessageError> {
        let form = reader.wide_prefixed()?;
        if form != FULL_FORM {
            return Err(MessageError::Unsized {
                field: FIELD,
                // Only its being a delta matters to the walk.
                byte: u8::try_from(form).unwrap_or(u8::MAX),
            });
        }
        let version = match reader.u8()? {
            0 => None,
            VERSION_PRESENT => Some(reader.wide_prefixed()?),
            value => return Err(MessageError::NotBoolean { value }),
        };
        let count = reader.wide_prefixed()?;
        if count > MAX_FULL_ELEMENTS {
            return Err(MessageError::CountTooLarge {
                count,
                limit: MAX_FULL_ELEMENTS,
            });
        }
        let elements = (0..count)
            .map(|_| read_element(reader))
            .collect::<Result<_, _>>()?;
        Ok(Self { version, elements })
    }

    /// Writes the list with `write_element`.
    ///
    /// # Errors
    ///
    /// Returns the error of an element.
    pub fn write(
        &self,
        writer: &mut Writer,
        mut write_element: impl FnMut(&mut Writer, &T) -> Result<(), MessageError>,
    ) -> Result<(), MessageError> {
        writer.wide_prefixed(FULL_FORM);
        match self.version {
            Some(version) => writer.u8(VERSION_PRESENT).wide_prefixed(version),
            None => writer.u8(0),
        };
        // A `usize` count fits 64 bits.
        writer.wide_prefixed(self.elements.len() as u64);
        self.elements
            .iter()
            .try_for_each(|element| write_element(writer, element))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retail starter chest piece: quantity 1, sync reason 4, no container, index 40226,
    /// durability 375 (paperdoll note, section 10.1).
    const RETAIL_CHEST_PACKED: [u8; 8] = [0x00, 0x01, 0x08, 0x4a, 0xeb, 0xff, 0x05, 0x19];
    /// A retail empty bag slot (section 4).
    const RETAIL_EMPTY_PACKED: [u8; 8] = [0x00, 0x00, 0x13, 0x25, 0x00, 0xd0, 0x00, 0x00];
    /// The record a removed worn item left in its visible slot (retail 2026-09-18, 149365).
    const RETAIL_REMOVED_PACKED: [u8; 8] = [0x00, 0x00, 0x16, 0xf9, 0x98, 0x30, 0x00, 0x00];
    /// Two potions moved into bag slot 1 (section 10.3).
    const RETAIL_POTIONS_PACKED: [u8; 8] = [0x00, 0x02, 0x01, 0x49, 0x32, 0xa3, 0xc7, 0x10];
    const INSTANCE: [u8; RAW_ID_LEN] = [7; RAW_ID_LEN];

    fn chest() -> ItemRecord {
        ItemRecord {
            quantity: 1,
            container_slot: None,
            item_index: 40_226,
            durability: 375,
            max_durability: 375,
            gear_score: 75,
            instance: INSTANCE,
            flags: ITEM_BOUND,
            ..ItemRecord::EMPTY_SLOT
        }
    }

    fn encode(item: &ItemRecord) -> Vec<u8> {
        let mut writer = Writer::new();
        item.write(&mut writer).expect("write");
        writer.finish()
    }

    #[test]
    fn the_packed_number_is_the_retail_one() {
        assert_eq!(encode(&chest())[..8], RETAIL_CHEST_PACKED);
        assert_eq!(encode(&ItemRecord::EMPTY_SLOT)[..8], RETAIL_EMPTY_PACKED);
        let removed = ItemRecord {
            sync_reason: SYNC_REMOVED,
            ..ItemRecord::EMPTY_SLOT
        };
        assert_eq!(encode(&removed)[..8], RETAIL_REMOVED_PACKED);
        let potions = ItemRecord {
            quantity: 2,
            sync_reason: 6,
            container_slot: Some(1),
            item_index: 4880,
            ..ItemRecord::EMPTY_SLOT
        };
        assert_eq!(encode(&potions)[..8], RETAIL_POTIONS_PACKED);
    }

    #[test]
    fn an_item_round_trips_with_its_retail_length() {
        // 8 packed, 2 + 1 + 1 + 5 numbers, 4 dye, 16 id, 1 flags, 4 + 4 transmog.
        const CHEST_BYTES: usize = 46;
        let bytes = encode(&chest());
        assert_eq!(bytes.len(), CHEST_BYTES);
        let mut reader = Reader::new(&bytes);
        assert_eq!(ItemRecord::read(&mut reader), Ok(chest()));
        assert!(reader.is_empty());
    }

    #[test]
    fn a_field_out_of_range_is_refused() {
        let item = ItemRecord {
            durability: 8001,
            ..chest()
        };
        assert!(item.write(&mut Writer::new()).is_err());
        let item = ItemRecord {
            item_index: 1 << ITEM_INDEX_BITS,
            ..chest()
        };
        assert!(item.write(&mut Writer::new()).is_err());
    }

    #[test]
    fn a_full_list_round_trips_and_a_delta_is_unsized() {
        let list = FullList::first(vec![375_u32, 1000]);
        let mut writer = Writer::new();
        list.write(&mut writer, |writer, value| {
            writer.u32(*value);
            Ok(())
        })
        .expect("write");
        let bytes = writer.finish();
        assert_eq!(bytes[..4], [0x00, 0x01, 0x03, 0x02]);
        let mut reader = Reader::new(&bytes);
        assert_eq!(FullList::read(&mut reader, |reader| reader.u32()), Ok(list));
        let delta = [0x01_u8, 0x01, 0x01];
        assert!(matches!(
            FullList::read(&mut Reader::new(&delta), |reader| reader.u32()),
            Err(MessageError::Unsized { .. })
        ));
    }
}
