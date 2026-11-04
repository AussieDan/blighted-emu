// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MB::PaperdollComponentReplicatedState` (type 3183, player component 62): the equipped items
//! (paperdoll-inventory note, section 3).
//!
//! Slot 0 has no children. Slot 1 (the owner's) has 12 children: durability and item lists of
//! the visible and the other paperdoll slots, the equip load bonus, flags, loaded ammo, the
//! loadouts and hidden skins. Slot 2 (public) has 6: bit maps of sheathed, attached and active
//! paperdoll slots, the visible slot ids, their item visuals, and a loadout swap count. The
//! visible lists are parallel: entry `i` of each is the same item.

use crate::items::{DYE_BYTES, ItemRecord, read_number};
use crate::list_delta::{ReplicatedList, child_change, value_change};
use crate::slots::{children_mask, read_children, read_slot_byte, write_children};
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `MB::PaperdollComponentReplicatedState`.
pub const PAPERDOLL_TYPE_INDEX: u64 = 3183;

/// Paperdoll slot ids (`Javelin::PaperdollSlotTypes`, section 5).
pub mod slot {
    /// `HEAD`.
    pub const HEAD: u32 = 0;
    /// `CHEST`.
    pub const CHEST: u32 = 1;
    /// `HANDS`.
    pub const HANDS: u32 = 2;
    /// `LEGS`.
    pub const LEGS: u32 = 3;
    /// `FEET`.
    pub const FEET: u32 = 4;
    /// `GATHERABLE_HAND`.
    pub const GATHERABLE_HAND: u32 = 9;
    /// `QUICK_SLOT_1` (paperdoll note, the slot table).
    pub const QUICK_SLOT_1: u32 = 14;
    /// `QUICK_SLOT_1` to `QUICK_SLOT_4`, the slots of the quick slot keys 1 to 4
    /// ([`crate::QUICK_SLOT_KEYS`]). Observed: retail equipped the mount food of
    /// `9401_tutorialfood-Hungry-as-a-horse` to slot 16 and used it at a press of quick slot key
    /// 3; Inferred: the slots follow in order (the client names `QUICK_SLOT_1` to `QUICK_SLOT_6`).
    pub const QUICK_SLOTS: [u32; 4] = [QUICK_SLOT_1, 15, 16, 17];
    /// `MAIN_HAND_OPTION_1`, the main hand.
    pub const MAIN_HAND_OPTION_1: u32 = 24;
    /// `MAIN_HAND_OPTION_2`, the second weapon.
    pub const MAIN_HAND_OPTION_2: u32 = 25;
    /// `MAIN_HAND_OPTION_3`.
    pub const MAIN_HAND_OPTION_3: u32 = 26;
    /// `OFF_HAND_OPTION_1`, the off hand.
    pub const OFF_HAND_OPTION_1: u32 = 27;
    /// `OFF_HAND_OPTION_3`.
    pub const OFF_HAND_OPTION_3: u32 = 29;
}

/// Slot byte bit of slot 1 (owner).
const OWNER_SLOT_BIT: u8 = 0b010;
/// Slot byte bit of slot 2 (public).
const PUBLIC_SLOT_BIT: u8 = 0b100;
/// Slots of the class.
const PAPERDOLL_SLOTS: u8 = 3;
/// Children of slot 1.
const OWNER_CHILDREN: u8 = 12;
/// Children of slot 2.
const PUBLIC_CHILDREN: u8 = 6;
/// Bytes of a paperdoll slot bit map: 50 slot ids in 56 bits.
pub const SLOT_BITS_BYTES: usize = 7;
/// Item visual flags bit 0: a 16-bit number follows.
const VISUAL_EXTRA: u8 = 0x01;
/// Item visual flags bit 1: a dye follows.
const VISUAL_DYE: u8 = 0x02;
/// Most bytes of a loadout name (`FUN_14087a3d0` limit).
const MAX_NAME_BYTES: u64 = 0x2_ffff;
const FIELD: &str = "paperdoll";

/// A set of paperdoll slots: bit `8 * byte + bit` is slot id `8 * byte + bit`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotBits(pub [u8; SLOT_BITS_BYTES]);

impl SlotBits {
    /// The set of `slots`; ids beyond the map are left out.
    pub fn of(slots: impl IntoIterator<Item = u32>) -> Self {
        let mut bits = [0; SLOT_BITS_BYTES];
        for slot in slots {
            let (byte, bit) = (slot / u8::BITS, slot % u8::BITS);
            if let Some(byte) = bits.get_mut(byte as usize) {
                *byte |= 1 << bit;
            }
        }
        Self(bits)
    }
}

/// Ammo loaded in a main-hand option (`FUN_146951510`, `LoadedAmmo`; client-gates note: retail
/// 2 at rest on a blunderbuss, 1 after one shot, 0 after the second).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadedAmmo {
    /// The shots loaded.
    pub count: u8,
    /// When the option was last put away: ns since 1970-01-01.
    pub time_ns: u64,
}

/// One loadout (`FUN_14669df20`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loadout {
    /// The name bytes (retail: `@lo_name 0`).
    pub name: Vec<u8>,
    /// (paperdoll slot id, owned-items key) of each item.
    pub slots: Vec<(u32, u16)>,
    /// A flag of unknown meaning.
    pub flag: bool,
    /// Pairs of unknown meaning.
    pub extra: Vec<(u32, u32)>,
}

impl Loadout {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let length = reader.wide_prefixed()?;
        if length > MAX_NAME_BYTES {
            return Err(MessageError::CountTooLarge {
                count: length,
                limit: MAX_NAME_BYTES,
            });
        }
        // At most `MAX_NAME_BYTES`.
        let name = reader.bytes(length as usize)?.to_vec();
        let slots = read_pairs(reader, |reader| Ok((reader.u32()?, reader.u16()?)))?;
        let flag = reader.boolean()?;
        let extra = read_pairs(reader, |reader| Ok((reader.u32()?, reader.u32()?)))?;
        Ok(Self {
            name,
            slots,
            flag,
            extra,
        })
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        // A `usize` length fits 64 bits.
        writer
            .wide_prefixed(self.name.len() as u64)
            .bytes(&self.name);
        writer.wide_prefixed(self.slots.len() as u64);
        for (slot, key) in &self.slots {
            writer.u32(*slot).u16(*key);
        }
        writer.boolean(self.flag);
        writer.wide_prefixed(self.extra.len() as u64);
        for (first, second) in &self.extra {
            writer.u32(*first).u32(*second);
        }
        Ok(())
    }
}

/// A count, then that many elements of `read_element`.
fn read_pairs<T>(
    reader: &mut Reader<'_>,
    mut read_element: impl FnMut(&mut Reader<'_>) -> Result<T, MessageError>,
) -> Result<Vec<T>, MessageError> {
    let count = reader.wide_prefixed()?;
    // Each element takes at least one byte.
    if count > reader.remaining() as u64 {
        return Err(MessageError::LengthMismatch { field: FIELD });
    }
    (0..count).map(|_| read_element(reader)).collect()
}

/// The look of one visible item (`FUN_145ce9070`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemVisual {
    /// The client item index.
    pub item_index: u32,
    /// A 16-bit number of unknown meaning (flags bit 0).
    pub extra: Option<u32>,
    /// A dye (flags bit 1).
    pub dye: Option<[u8; DYE_BYTES]>,
    /// Transmog id.
    pub transmog: u32,
    /// Transmog dye.
    pub transmog_dye: [u8; DYE_BYTES],
}

impl ItemVisual {
    /// The plain look of item `item_index`.
    pub const fn of(item_index: u32) -> Self {
        Self {
            item_index,
            extra: None,
            dye: None,
            transmog: 0,
            transmog_dye: [0; DYE_BYTES],
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let item_index = read_number(reader)?;
        let flags = reader.u8()?;
        if flags & !(VISUAL_EXTRA | VISUAL_DYE) != 0 {
            return Err(MessageError::UnknownBits {
                field: FIELD,
                bits: flags,
            });
        }
        let extra = (flags & VISUAL_EXTRA != 0)
            .then(|| read_number(reader))
            .transpose()?;
        let dye = (flags & VISUAL_DYE != 0)
            .then(|| reader.array())
            .transpose()?;
        Ok(Self {
            item_index,
            extra,
            dye,
            transmog: reader.u32()?,
            transmog_dye: reader.array()?,
        })
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let flags = if self.extra.is_some() {
            VISUAL_EXTRA
        } else {
            0
        } | if self.dye.is_some() { VISUAL_DYE } else { 0 };
        writer.wide_prefixed(u64::from(self.item_index)).u8(flags);
        if let Some(extra) = self.extra {
            writer.wide_prefixed(u64::from(extra));
        }
        if let Some(dye) = self.dye {
            writer.bytes(&dye);
        }
        writer.u32(self.transmog).bytes(&self.transmog_dye);
        Ok(())
    }
}

/// Slot 1, the owner's children; `None` is a child that is not sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaperdollOwner {
    /// Child 0: durability of each visible item.
    pub visible_durability: Option<ReplicatedList<u32>>,
    /// Child 1: the visible items.
    pub visible_items: Option<ReplicatedList<ItemRecord>>,
    /// Child 2: the items of the other paperdoll slots.
    pub hidden_items: Option<ReplicatedList<ItemRecord>>,
    /// Child 3: the paperdoll slot ids of the other items.
    pub hidden_slots: Option<ReplicatedList<u32>>,
    /// Child 4: durability of each other item.
    pub hidden_durability: Option<ReplicatedList<u32>>,
    /// Child 5: `bonusEquipLoad`.
    pub bonus_equip_load: Option<u16>,
    /// Child 6: `isLocalPlayer`.
    pub is_local_player: Option<bool>,
    /// Child 7: `isLoadoutPanelOpen`.
    pub is_loadout_panel_open: Option<bool>,
    /// Child 8: ammo of main-hand option 1.
    pub main_hand_ammo_1: Option<LoadedAmmo>,
    /// Child 9: ammo of main-hand option 2.
    pub main_hand_ammo_2: Option<LoadedAmmo>,
    /// Child 10: the loadouts.
    pub loadouts: Option<ReplicatedList<Loadout>>,
    /// Child 11: `hideSkins`, (skin id, hidden).
    pub hide_skins: Option<ReplicatedList<(u32, bool)>>,
}

impl PaperdollOwner {
    /// The children that changed from `before` (`None`: never sent) to `now` at `version`.
    pub(crate) fn change(before: Option<&Self>, now: &Self, version: u64) -> Self {
        Self {
            visible_durability: child_change(
                before.and_then(|before| before.visible_durability.as_ref()),
                now.visible_durability.as_ref(),
                version,
            ),
            visible_items: child_change(
                before.and_then(|before| before.visible_items.as_ref()),
                now.visible_items.as_ref(),
                version,
            ),
            hidden_items: child_change(
                before.and_then(|before| before.hidden_items.as_ref()),
                now.hidden_items.as_ref(),
                version,
            ),
            hidden_slots: child_change(
                before.and_then(|before| before.hidden_slots.as_ref()),
                now.hidden_slots.as_ref(),
                version,
            ),
            hidden_durability: child_change(
                before.and_then(|before| before.hidden_durability.as_ref()),
                now.hidden_durability.as_ref(),
                version,
            ),
            bonus_equip_load: value_change(
                before.and_then(|before| before.bonus_equip_load.as_ref()),
                now.bonus_equip_load.as_ref(),
            ),
            is_local_player: value_change(
                before.and_then(|before| before.is_local_player.as_ref()),
                now.is_local_player.as_ref(),
            ),
            is_loadout_panel_open: value_change(
                before.and_then(|before| before.is_loadout_panel_open.as_ref()),
                now.is_loadout_panel_open.as_ref(),
            ),
            main_hand_ammo_1: value_change(
                before.and_then(|before| before.main_hand_ammo_1.as_ref()),
                now.main_hand_ammo_1.as_ref(),
            ),
            main_hand_ammo_2: value_change(
                before.and_then(|before| before.main_hand_ammo_2.as_ref()),
                now.main_hand_ammo_2.as_ref(),
            ),
            loadouts: child_change(
                before.and_then(|before| before.loadouts.as_ref()),
                now.loadouts.as_ref(),
                version,
            ),
            hide_skins: child_change(
                before.and_then(|before| before.hide_skins.as_ref()),
                now.hide_skins.as_ref(),
                version,
            ),
        }
    }

    fn present(&self) -> [bool; OWNER_CHILDREN as usize] {
        [
            self.visible_durability.is_some(),
            self.visible_items.is_some(),
            self.hidden_items.is_some(),
            self.hidden_slots.is_some(),
            self.hidden_durability.is_some(),
            self.bonus_equip_load.is_some(),
            self.is_local_player.is_some(),
            self.is_loadout_panel_open.is_some(),
            self.main_hand_ammo_1.is_some(),
            self.main_hand_ammo_2.is_some(),
            self.loadouts.is_some(),
            self.hide_skins.is_some(),
        ]
    }

    fn read_child(&mut self, reader: &mut Reader<'_>, child: u8) -> Result<(), MessageError> {
        match child {
            0 => self.visible_durability = Some(read_list(reader, |reader| reader.u32())?),
            1 => self.visible_items = Some(read_list(reader, ItemRecord::read)?),
            2 => self.hidden_items = Some(read_list(reader, ItemRecord::read)?),
            3 => self.hidden_slots = Some(read_list(reader, |reader| reader.u32())?),
            4 => self.hidden_durability = Some(read_list(reader, |reader| reader.u32())?),
            5 => self.bonus_equip_load = Some(reader.u16()?),
            6 => self.is_local_player = Some(reader.boolean()?),
            7 => self.is_loadout_panel_open = Some(reader.boolean()?),
            8 => self.main_hand_ammo_1 = Some(read_ammo(reader)?),
            9 => self.main_hand_ammo_2 = Some(read_ammo(reader)?),
            10 => self.loadouts = Some(read_list(reader, Loadout::read)?),
            _ => {
                self.hide_skins = Some(read_list(reader, |reader| {
                    Ok((reader.u32()?, reader.boolean()?))
                })?);
            }
        }
        Ok(())
    }

    fn write_child(&self, writer: &mut Writer, child: u8) -> Result<(), MessageError> {
        let write_u32 = |writer: &mut Writer, value: &u32| {
            writer.u32(*value);
            Ok(())
        };
        let write_item = |writer: &mut Writer, item: &ItemRecord| item.write(writer);
        match child {
            0 => write_list(writer, self.visible_durability.as_ref(), write_u32),
            1 => write_list(writer, self.visible_items.as_ref(), write_item),
            2 => write_list(writer, self.hidden_items.as_ref(), write_item),
            3 => write_list(writer, self.hidden_slots.as_ref(), write_u32),
            4 => write_list(writer, self.hidden_durability.as_ref(), write_u32),
            5 => {
                self.bonus_equip_load.map(|load| writer.u16(load));
                Ok(())
            }
            6 => {
                self.is_local_player.map(|local| writer.boolean(local));
                Ok(())
            }
            7 => {
                self.is_loadout_panel_open.map(|open| writer.boolean(open));
                Ok(())
            }
            8 => {
                write_ammo(writer, self.main_hand_ammo_1);
                Ok(())
            }
            9 => {
                write_ammo(writer, self.main_hand_ammo_2);
                Ok(())
            }
            10 => write_list(writer, self.loadouts.as_ref(), |writer, loadout| {
                loadout.write(writer)
            }),
            _ => write_list(
                writer,
                self.hide_skins.as_ref(),
                |writer, (skin, hidden)| {
                    writer.u32(*skin).boolean(*hidden);
                    Ok(())
                },
            ),
        }
    }
}

fn read_ammo(reader: &mut Reader<'_>) -> Result<LoadedAmmo, MessageError> {
    Ok(LoadedAmmo {
        count: reader.u8()?,
        time_ns: reader.u64()?,
    })
}

fn write_ammo(writer: &mut Writer, ammo: Option<LoadedAmmo>) {
    if let Some(ammo) = ammo {
        writer.u8(ammo.count).u64(ammo.time_ns);
    }
}

/// Reads a list child in either form.
fn read_list<T>(
    reader: &mut Reader<'_>,
    read_element: impl FnMut(&mut Reader<'_>) -> Result<T, MessageError>,
) -> Result<ReplicatedList<T>, MessageError> {
    ReplicatedList::read(reader, FIELD, read_element)
}

/// Writes `list` when it is sent.
fn write_list<T>(
    writer: &mut Writer,
    list: Option<&ReplicatedList<T>>,
    write_element: impl FnMut(&mut Writer, &T) -> Result<(), MessageError>,
) -> Result<(), MessageError> {
    list.map_or(Ok(()), |list| list.write(writer, write_element))
}

/// Slot 2, the public children; `None` is a child that is not sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaperdollPublic {
    /// Child 0: `sheatheMap`.
    pub sheathed: Option<SlotBits>,
    /// Child 1: `itemSlotsAttachmentStatus`.
    pub attached: Option<SlotBits>,
    /// Child 2: `activeMap`.
    pub active: Option<SlotBits>,
    /// Child 3: the paperdoll slot ids of the visible items.
    pub visible_slots: Option<ReplicatedList<u32>>,
    /// Child 4: the looks of the visible items.
    pub visuals: Option<ReplicatedList<ItemVisual>>,
    /// Child 5: `loadoutSwapIncrement`.
    pub loadout_swaps: Option<u8>,
}

impl PaperdollPublic {
    /// The children that changed from `before` (`None`: never sent) to `now` at `version`.
    pub(crate) fn change(before: Option<&Self>, now: &Self, version: u64) -> Self {
        Self {
            sheathed: value_change(
                before.and_then(|before| before.sheathed.as_ref()),
                now.sheathed.as_ref(),
            ),
            attached: value_change(
                before.and_then(|before| before.attached.as_ref()),
                now.attached.as_ref(),
            ),
            active: value_change(
                before.and_then(|before| before.active.as_ref()),
                now.active.as_ref(),
            ),
            visible_slots: child_change(
                before.and_then(|before| before.visible_slots.as_ref()),
                now.visible_slots.as_ref(),
                version,
            ),
            visuals: child_change(
                before.and_then(|before| before.visuals.as_ref()),
                now.visuals.as_ref(),
                version,
            ),
            loadout_swaps: value_change(
                before.and_then(|before| before.loadout_swaps.as_ref()),
                now.loadout_swaps.as_ref(),
            ),
        }
    }

    fn present(&self) -> [bool; PUBLIC_CHILDREN as usize] {
        [
            self.sheathed.is_some(),
            self.attached.is_some(),
            self.active.is_some(),
            self.visible_slots.is_some(),
            self.visuals.is_some(),
            self.loadout_swaps.is_some(),
        ]
    }

    fn read_child(&mut self, reader: &mut Reader<'_>, child: u8) -> Result<(), MessageError> {
        let bits = |reader: &mut Reader<'_>| reader.array().map(SlotBits);
        match child {
            0 => self.sheathed = Some(bits(reader)?),
            1 => self.attached = Some(bits(reader)?),
            2 => self.active = Some(bits(reader)?),
            3 => self.visible_slots = Some(read_list(reader, |reader| reader.u32())?),
            4 => self.visuals = Some(read_list(reader, ItemVisual::read)?),
            _ => self.loadout_swaps = Some(reader.u8()?),
        }
        Ok(())
    }

    fn write_child(&self, writer: &mut Writer, child: u8) -> Result<(), MessageError> {
        let bits = |writer: &mut Writer, bits: Option<SlotBits>| {
            bits.map(|SlotBits(bytes)| writer.bytes(&bytes));
            Ok(())
        };
        match child {
            0 => bits(writer, self.sheathed),
            1 => bits(writer, self.attached),
            2 => bits(writer, self.active),
            3 => write_list(writer, self.visible_slots.as_ref(), |writer, slot| {
                writer.u32(*slot);
                Ok(())
            }),
            4 => write_list(writer, self.visuals.as_ref(), |writer, visual| {
                visual.write(writer)
            }),
            _ => {
                self.loadout_swaps.map(|swaps| writer.u8(swaps));
                Ok(())
            }
        }
    }
}

/// The paperdoll state; `None` is a slot that is not sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaperdollState {
    /// Slot 1.
    pub owner: Option<PaperdollOwner>,
    /// Slot 2.
    pub public: Option<PaperdollPublic>,
}

impl PaperdollState {
    /// Whether it sends no child: no owner child and no public child.
    pub fn is_empty(&self) -> bool {
        let unset = |present: &[bool]| present.iter().all(|present| !present);
        self.owner
            .as_ref()
            .is_none_or(|owner| unset(&owner.present()))
            && self
                .public
                .as_ref()
                .is_none_or(|public| unset(&public.present()))
    }

    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, sets slot 0, or a list change removes
    /// an element.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slots = read_slot_byte(reader, FIELD, PAPERDOLL_SLOTS)?;
        if slots & !(OWNER_SLOT_BIT | PUBLIC_SLOT_BIT) != 0 {
            return Err(MessageError::Unsized {
                field: FIELD,
                byte: slots,
            });
        }
        let mut state = Self::default();
        if slots & OWNER_SLOT_BIT != 0 {
            let mut owner = PaperdollOwner::default();
            read_children(reader, FIELD, OWNER_CHILDREN, |reader, child| {
                owner.read_child(reader, child)
            })?;
            state.owner = Some(owner);
        }
        if slots & PUBLIC_SLOT_BIT != 0 {
            let mut public = PaperdollPublic::default();
            read_children(reader, FIELD, PUBLIC_CHILDREN, |reader, child| {
                public.read_child(reader, child)
            })?;
            state.public = Some(public);
        }
        Ok(state)
    }

    /// Writes the state for the bundle path. A slot without a child is not sent.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when an item does not fit its packed form.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let owner = self
            .owner
            .as_ref()
            .map(|owner| (owner, children_mask(&owner.present())))
            .filter(|(_, children)| *children != 0);
        let public = self
            .public
            .as_ref()
            .map(|public| (public, children_mask(&public.present())))
            .filter(|(_, children)| *children != 0);
        let slot_bit = |present: bool, bit: u8| if present { bit } else { 0 };
        writer
            .u8(slot_bit(owner.is_some(), OWNER_SLOT_BIT)
                | slot_bit(public.is_some(), PUBLIC_SLOT_BIT));
        if let Some((owner, children)) = owner {
            write_children(writer, children, OWNER_CHILDREN, |writer, child| {
                owner.write_child(writer, child)
            })?;
        }
        if let Some((public, children)) = public {
            write_children(writer, children, PUBLIC_CHILDREN, |writer, child| {
                public.write_child(writer, child)
            })?;
        }
        Ok(())
    }
}
