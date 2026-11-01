// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ActionListComponentServerFacet_ReceiveInputFromClient` (type 1589): the client's input
//! (replication note section 3, movement message note sections 1 to 4, both checked on every
//! captured body). Unmarshal `FUN_144867c70`, head `FUN_142a2e6b0`, record `FUN_142a25d90`.
//!
//! A record starts with a 24-bit mask. Mask bits 0 to 8 gate fixed fields. The later rows depend
//! on the record's prefix integer (mask bit 0) and on a gate byte (row 4); a record that does not
//! set them keeps the previous record's values. Most rows are gated by a mask bit at a moving
//! cursor. Parts whose meaning is Unknown are kept as raw bytes, so a body encodes back to the
//! same bytes; `encode` refuses a record whose fixed fields do not match its mask.

use crate::position::{height_from_raw, height_to_raw};
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer, entity_of_raw_id};

/// File slot of `ReceiveInputFromClient`.
pub const RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX: u64 = 1589;
/// Height of a raw `z` of 0 in an input position, in metres.
pub const INPUT_Z_MIN: f32 = -100.0;
/// Height of a raw `z` of `u16::MAX` in an input position, in metres.
pub const INPUT_Z_MAX: f32 = 1000.0;
/// Bytes of the record mask.
const MASK_LEN: usize = 3;
/// The mask bits with a fixed field (0 to 8).
const FIXED_BITS: u32 = 9;
/// Head step 15: most pairs.
const MAX_HEAD_PAIRS: u64 = 16;
/// Most elements in one element list (`0x2000000`).
const MAX_ELEMENTS: u64 = 0x200_0000;
/// Longest string in the bit 8 body (`FUN_14087a3d0`).
const MAX_STRING: u64 = 0x2_FFFF;
/// The tag of the group after the position that ends the group at once.
pub const AFTER_POSITION_END: u8 = 3;
/// Bytes of the group after the position, after its tag: a `u8`, two `f32`, four `u16`.
pub const AFTER_POSITION_LEN: usize = 17;
/// Raw bytes before the `u64` of the row 3 id.
const ROW_3_RAW_LEN: usize = 16;
/// Bytes of the row 5 block (`FUN_1415acf00`).
const ROW_5_BLOCK_LEN: usize = 32;
/// Bytes of row 6.
const ROW_6_LEN: usize = 8;
/// Scaled bytes after the slots (`FUN_142a15ae0`, four calls).
const SLOT_BYTES: usize = 4;
/// Bytes of one element of the three element lists (`FUN_142a1a310`, `FUN_142a1a0d0`,
/// `FUN_142a19e60`).
pub const ELEMENT_LENS: [usize; 3] = [104, 192, 96];
/// Binary16 values of row 11.
const ROW_11_VALUES: usize = 3;
/// The cursor of the rows after bit 8.
const FIRST_CURSOR: u32 = 9;
/// The cursor after row 2.
const ROW_2_CURSOR: u32 = 10;
/// Prefix bits that turn rows on.
const PREFIX_ROW_1: u32 = 0;
const PREFIX_ROW_2: u32 = 11;
const PREFIX_ROW_3: u32 = 8;
const PREFIX_ROW_6: u32 = 14;
const PREFIX_ROW_10: u32 = 15;
const PREFIX_ROW_12: u32 = 16;
const PREFIX_ROW_13: u32 = 18;
const PREFIX_ROW_16: u32 = 0;
/// Mask bits of the fixed fields.
const BIT_0: u32 = 0;
const BIT_1: u32 = 1;
const BIT_2: u32 = 2;
const BIT_3: u32 = 3;
const BIT_4: u32 = 4;
const BIT_5: u32 = 5;
const BIT_6: u32 = 6;
const BIT_7: u32 = 7;
const BIT_8: u32 = 8;

/// A position in an input record: big-endian `f32` world x and y, and a `u16` height from
/// [`INPUT_Z_MIN`] to [`INPUT_Z_MAX`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputPosition {
    /// East, in metres.
    pub x: f32,
    /// North, in metres.
    pub y: f32,
    /// Height, from [`INPUT_Z_MIN`] (0) to [`INPUT_Z_MAX`] (`u16::MAX`).
    pub z_raw: u16,
}

impl InputPosition {
    /// A position from metres; the height is clamped to the encoded range. `None` when a value
    /// is not finite.
    pub fn from_metres(x: f32, y: f32, z: f32) -> Option<Self> {
        if !(x.is_finite() && y.is_finite() && z.is_finite()) {
            return None;
        }
        Some(Self {
            x,
            y,
            z_raw: height_to_raw(z, INPUT_Z_MIN, INPUT_Z_MAX),
        })
    }

    /// The height in metres.
    pub fn z(&self) -> f32 {
        height_from_raw(self.z_raw, INPUT_Z_MIN, INPUT_Z_MAX)
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            x: reader.f32()?,
            y: reader.f32()?,
            z_raw: reader.u16()?,
        })
    }

    pub(crate) fn write(&self, writer: &mut Writer) {
        writer.f32(self.x).f32(self.y).u16(self.z_raw);
    }
}

/// Head step 9 (`FUN_1461ac530`): a flag, a prefix-coded integer when the flag is 1, a `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeadStep9 {
    /// The integer after a flag of 1.
    pub prefixed: Option<u64>,
    /// The big-endian `u64` at the end.
    pub u64_at_end: u64,
}

/// The fixed head (`FUN_142a2e6b0`), steps numbered as in the replication note.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputHead {
    /// Steps 1 to 4.
    pub bytes_1_to_4: [u8; 4],
    /// Step 5, a wide prefix-coded integer.
    pub step_5: u64,
    /// Steps 6 and 7.
    pub steps_6_7: [u64; 2],
    /// Step 8.
    pub step_8: u8,
    /// Step 9.
    pub step_9: HeadStep9,
    /// Step 10.
    pub step_10: u8,
    /// Steps 11 to 13.
    pub steps_11_to_13: [u64; 3],
    /// Step 14.
    pub step_14: u8,
    /// Step 15: at most 16 pairs.
    pub step_15_pairs: Vec<[u64; 2]>,
    /// Step 15: the two integers after the pairs.
    pub step_15_tail: [u64; 2],
    /// Steps 16 and 17.
    pub steps_16_17: [u64; 2],
}

/// The slots after mask bit 4 (`FUN_142a2ea20`): a count, the packed values, four scaled bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputSlots {
    /// Slots.
    pub count: u8,
    /// One wide integer per two slots, then one narrow integer for an odd last slot.
    pub values: Vec<u64>,
    /// The four scaled bytes.
    pub bytes: [u8; SLOT_BYTES],
}

/// Mask bit 8 (`FUN_1435ebc30`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bit8 {
    /// Flag 0.
    Empty,
    /// Flag 1: the long body, as raw bytes.
    Body(Vec<u8>),
}

/// The interaction kind of a gather.
pub const GATHER_KIND: u8 = 14;
/// The interaction kind of a water gather (`InteractionUIActions` `GatherWaterResource`; retail
/// `Hive_Dye`: two requests, their gatherable CRC 0).
pub const WATER_GATHER_KIND: u8 = 15;
/// The interaction kind of a loot bag (retail 2026-09-18: the bag of a killed boar and the
/// tutorial's beach bag).
pub const LOOT_KIND: u8 = 28;
/// The interaction kind of a crafting station (beach-quests note: Urda's Campfire).
pub const CRAFTING_STATION_KIND: u8 = 20;
/// The interaction kind of a talk to an NPC (beach-quests note: Aelstrom and Urda).
pub const TALK_KIND: u8 = 63;
/// The interaction kind of a town project board (retail board capture 2026-06-13: name
/// `@ui_territory_board_town_project`).
pub const TOWN_BOARD_KIND: u8 = 51;
/// The interaction kind of a revive of a downed player (the client's `InteractionUIActions`
/// value 30, `RevivePlayer`, registered by `FUN_1435eaa20`).
pub const REVIVE_PLAYER_KIND: u8 = 30;
/// The interaction kind of a trigger object (`InteractionUIActions` value 33, `Trigger`).
pub const TRIGGER_KIND: u8 = 33;
/// The interaction kind of the exit of a dungeon (`InteractionUIActions` value 74,
/// `ExitDungeon`).
pub const EXIT_DUNGEON_KIND: u8 = 74;
/// The interaction kind of a lore note the player reads (retail 2026-09-18: Kubikiri's
/// Occupation and La Rousette's Manifest on the start beach).
pub const LORE_KIND: u8 = 47;
/// Where a gather's body holds the CRC-32 of its gatherable (after the kind, the kind and the
/// facet entity again).
const GATHERABLE_AT: usize = 42;
/// Bytes of a CRC-32.
const CRC_LEN: usize = 4;
/// Where a bit-8 body holds the wide prefix-coded length of the object's name, which follows it
/// (retail 2026-09-18: `@KubikirisBurden_Title_01_01` for a lore note, `@01_Grace` for a talk,
/// empty for a gather).
const NAME_LENGTH_AT: usize = 60;

/// The start of a bit-8 body: the object the client uses (ftue-static-entities note, section 5).
/// The client sends it in the first record of every input until the interaction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionRequest {
    /// The entity id of the object.
    pub target: [u8; RAW_ID_LEN],
    /// Its facet entity.
    pub target_entity: u64,
    /// The interaction id (the server's answer and the client's cancel carry it).
    pub interaction: u64,
    /// The interaction kind ([`GATHER_KIND`], [`LOOT_KIND`], ...).
    pub kind: u8,
    /// A gather's gatherable: the CRC-32 of its lowercase `GatherableID` (gather note: retail
    /// flint `a417137f` = `SingleStone`).
    pub gatherable: Option<u32>,
}

impl Bit8 {
    /// The body of a gather of the object `target` (facet entity `target_entity`) of gatherable
    /// `gatherable` (CRC-32 of its lowercase `GatherableID`), interaction `interaction`, laid
    /// out as the retail flint request starts (gather note, newchar 739603: the object, its
    /// facet entity, the interaction id, the kind twice, the facet entity again and the
    /// gatherable), with the rest of the long body empty: no name, no strings, no vectors, zero
    /// counters.
    pub fn gather(
        target: [u8; RAW_ID_LEN],
        target_entity: u64,
        interaction: u64,
        gatherable: u32,
    ) -> Result<Self, MessageError> {
        const EMPTY: u64 = 0;
        let mut writer = Writer::new();
        writer
            .bytes(&target)
            .u64(target_entity)
            .u64(interaction)
            .u8(GATHER_KIND)
            .u8(GATHER_KIND)
            .u64(target_entity)
            .u32(gatherable)
            .prefixed(EMPTY)?
            .bytes(&[0; LONG_AFTER_NAME_LEN])
            .boolean(false);
        for _ in 0..LONG_STRINGS_11_TO_14 {
            writer.prefixed(EMPTY)?;
        }
        writer
            .f32(0.0)
            .boolean(false)
            .boolean(false)
            .bytes(&[0; LONG_COUNTERS_LEN])
            .prefixed(EMPTY)?
            .u32(0)
            .prefixed(EMPTY)?
            .prefixed(EMPTY)?
            .bytes(&[0; LONG_STEPS_30_31_LEN])
            .boolean(false)
            .u32(0)
            .prefixed(EMPTY)?
            .bytes(&[0; LONG_TAIL_LEN])
            .boolean(false);
        Ok(Self::Body(writer.finish()))
    }

    /// The interaction of a body; `None` for an empty one or a body too short.
    pub fn interaction(&self) -> Option<InteractionRequest> {
        let Self::Body(body) = self else {
            return None;
        };
        let mut reader = Reader::new(body);
        let target = reader.raw_id().ok()?;
        let target_entity = reader.u64().ok()?;
        let interaction = reader.u64().ok()?;
        let kind = reader.u8().ok()?;
        let gatherable = (kind == GATHER_KIND)
            .then(|| body.get(GATHERABLE_AT..GATHERABLE_AT + CRC_LEN))
            .flatten()
            .and_then(|bytes| <[u8; CRC_LEN]>::try_from(bytes).ok())
            .map(u32::from_be_bytes);
        Some(InteractionRequest {
            target,
            target_entity,
            interaction,
            kind,
            gatherable,
        })
    }

    /// The name of the object of the interaction of a body; `None` for an empty one or a body
    /// too short.
    pub fn interaction_name(&self) -> Option<Vec<u8>> {
        let Self::Body(body) = self else {
            return None;
        };
        let mut reader = Reader::new(body.get(NAME_LENGTH_AT..)?);
        let length = usize::try_from(reader.wide_prefixed().ok()?).ok()?;
        reader.bytes(length).ok().map(<[u8]>::to_vec)
    }
}

/// One record (`FUN_142a25d90`). `None` means the field is not in the record.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InputRecord {
    /// The 24-bit mask.
    pub mask: u32,
    /// The big-endian `u16` after the mask.
    pub u16_after_mask: u16,
    /// Bit 0: the record's prefix integer (wide form).
    pub bit_0: Option<u64>,
    /// Bit 1.
    pub bit_1: Option<u8>,
    /// Bit 2.
    pub bit_2: Option<bool>,
    /// Bit 3.
    pub bit_3: Option<u8>,
    /// Bit 4.
    pub bit_4: Option<u16>,
    /// The slots, always present.
    pub slots: InputSlots,
    /// Bit 5.
    pub bit_5: Option<f32>,
    /// Bit 6.
    pub bit_6: Option<[f32; 2]>,
    /// Bit 7: the position.
    pub bit_7: Option<InputPosition>,
    /// The group after the position (`FUN_14416eb00`): `None` for the end tag 3, else the tag and
    /// its 17 bytes.
    pub after_position: Option<(u8, [u8; AFTER_POSITION_LEN])>,
    /// Bit 8.
    pub bit_8: Option<Bit8>,
    /// Row 1: three integers (prefix bit 0).
    pub row_1: Option<[u64; 3]>,
    /// Row 2: a position (prefix bit 11, mask bit 9).
    pub row_2: Option<InputPosition>,
    /// Row 3: a position (prefix bit 8, cursor bit).
    pub row_3_position: Option<InputPosition>,
    /// Row 3: 16 raw bytes and a `u64` (prefix bit 8, next cursor bit).
    pub row_3_id: Option<([u8; ROW_3_RAW_LEN], u64)>,
    /// Row 4: the gate byte (cursor bit).
    pub row_4: Option<u8>,
    /// Row 5: a position (gate byte not 0, cursor bit).
    pub row_5_position: Option<InputPosition>,
    /// Row 5: the 32-byte block (gate byte not 0, next cursor bit).
    pub row_5_block: Option<[u8; ROW_5_BLOCK_LEN]>,
    /// Row 6: 8 raw bytes (prefix bit 14, cursor bit).
    pub row_6: Option<[u8; ROW_6_LEN]>,
    /// Row 7: elements of 104 bytes, raw.
    pub elements_104: Vec<u8>,
    /// Row 8: elements of 192 bytes, raw.
    pub elements_192: Vec<u8>,
    /// Row 9: elements of 96 bytes, raw.
    pub elements_96: Vec<u8>,
    /// Row 10: a position (prefix bit 15, cursor bit).
    pub row_10: Option<InputPosition>,
    /// Row 11: three binary16 values (cursor bit).
    pub row_11: Option<[u16; ROW_11_VALUES]>,
    /// Row 12: a `u64` (prefix bit 16).
    pub row_12: Option<u64>,
    /// Row 13: a `u8` (prefix bit 18).
    pub row_13: Option<u8>,
    /// Row 14: a `u8` (cursor bit + 1).
    pub row_14: Option<u8>,
    /// Row 15: a `u32` (cursor bit + 2).
    pub row_15: Option<u32>,
    /// Row 16: a binary16 (prefix bit 0, cursor bit + 3).
    pub row_16: Option<u16>,
}

/// The whole message.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClientInputMsg {
    /// The first big-endian `u64`.
    pub component: u64,
    /// The second big-endian `u64`.
    pub u64_at_8: u64,
    /// The fixed head.
    pub head: InputHead,
    /// The records; at most 255 (a `u8` count).
    pub records: Vec<InputRecord>,
}

/// What a record leaves to the next one.
#[derive(Debug, Clone, Copy, Default)]
struct Carried {
    prefix: u64,
    gate: u8,
}

impl Carried {
    fn prefix_bit(self, bit: u32) -> bool {
        // The reader tests the low 32 bits.
        (self.prefix >> bit) & 1 == 1
    }
}

const fn mask_bit(mask: u32, bit: u32) -> bool {
    (mask >> bit) & 1 == 1
}

/// Which rows after bit 8 a record holds (`FUN_142a25d90`, the bit cursor): from its mask, its
/// prefix integer and the gate byte after row 4. `decode` reads by it and `encode` checks by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RowLayout {
    row_1: bool,
    row_2: bool,
    row_3_position: bool,
    row_3_id: bool,
    row_4: bool,
    row_5_position: bool,
    row_5_block: bool,
    row_6: bool,
    row_10: bool,
    row_11: bool,
    row_12: bool,
    row_13: bool,
    row_14: bool,
    row_15: bool,
    row_16: bool,
}

impl RowLayout {
    /// The rows of a record with `mask`, the prefix integer `prefix` and the gate byte `gate`
    /// that row 5 sees. Rows 1 to 4 do not depend on `gate`.
    fn new(mask: u32, prefix: u64, gate: u8) -> Self {
        let bit = |n: u32| mask_bit(mask, n);
        let carried = Carried { prefix, gate };
        let mut layout = Self {
            row_1: carried.prefix_bit(PREFIX_ROW_1),
            ..Self::default()
        };
        let mut cursor = FIRST_CURSOR;
        if carried.prefix_bit(PREFIX_ROW_2) {
            cursor = ROW_2_CURSOR;
            layout.row_2 = bit(FIRST_CURSOR);
        }
        if carried.prefix_bit(PREFIX_ROW_3) {
            layout.row_3_position = bit(cursor);
            layout.row_3_id = bit(cursor + 1);
            cursor += 2;
        }
        layout.row_4 = bit(cursor);
        let mut next = cursor + 1;
        if gate != 0 {
            layout.row_5_position = bit(cursor + 1);
            layout.row_5_block = bit(cursor + 2);
            next = cursor + 3;
        }
        let mut cursor = next;
        if carried.prefix_bit(PREFIX_ROW_6) {
            layout.row_6 = bit(cursor);
            cursor += 1;
        }
        if carried.prefix_bit(PREFIX_ROW_10) {
            layout.row_10 = bit(cursor);
            cursor += 1;
        }
        layout.row_11 = bit(cursor);
        layout.row_12 = carried.prefix_bit(PREFIX_ROW_12);
        layout.row_13 = carried.prefix_bit(PREFIX_ROW_13);
        layout.row_14 = bit(cursor + 1);
        layout.row_15 = bit(cursor + 2);
        layout.row_16 = carried.prefix_bit(PREFIX_ROW_16) && bit(cursor + 3);
        layout
    }
}

/// The slot value and its bit that are set while the player holds block. Live capture of the
/// tutorial (2026-09-12): set in the inputs of 25 periods; the server sent `isBlockActive` 1 in
/// them, and all 10 melee hits that retail reported blocked (2218 with 1) fell in them.
const BLOCK_SLOT_VALUE: usize = 1;
const BLOCK_SLOT_BIT: u32 = 62;
/// The slot value and its bit that are set while the player holds the dodge key. Live capture of
/// the tutorial: each press that started a roll cost the dodge stamina (45 on land, 50 in the
/// water) 33 ms later (xp-potion-exit note, section 5).
const DODGE_SLOT_VALUE: usize = 1;
const DODGE_SLOT_BIT: u32 = 3;
/// The slot value and its bit of a press of quick slot 1 (the potion): set in one record only,
/// the one before the retail server used the potion of paperdoll slot 14 (xp-potion-exit note,
/// section 2.1; Inferred from one use).
const QUICK_SLOT_1_VALUE: usize = 1;
const QUICK_SLOT_1_BIT: u32 = 23;
/// The slot value and bit of an attack key press: set in one record only (input note: the
/// client's action set, 317 retail presses; the mask bits 9, 10, 13 and 15 that we read before are
/// field presence bits in 873 to 1042 of about 1200 records).
const ATTACK_PRESS_VALUE: usize = 2;
const ATTACK_PRESS_BIT: u32 = 40;
/// The slot value and bit of the attack key's release: set in one record only.
const ATTACK_RELEASE_VALUE: usize = 2;
const ATTACK_RELEASE_BIT: u32 = 31;
/// The slot value and bit set while the attack key is held (the Azoth catalog input 191, `primary
/// attack (held)`; fishing study, run 15: set from the record after each press until the
/// release in all 9 casts and 7 reels of `open-world/Fishing`).
const ATTACK_HELD_KEY: (usize, u32) = (2, 63);
/// The slot value and bit of a press of the fishing key (fishing study, run 15: in
/// `open-world/Fishing` each of its 4 presses took the pole out or put it away, 63 to 68 ms
/// later, with no other key).
const FISHING_TOGGLE_KEY: (usize, u32) = (4, 49);
/// The slot value and bit of a press of the bait window key (fishing study, run 15: each of the
/// 4 presses with the pole out opened or closed the bait window, the state `ApplyingBait` and
/// back).
const BAIT_WINDOW_KEY: (usize, u32) = (4, 9);
/// The slot value and bit of the forward navigation input (the action lists' `ForwardNav`, the
/// forward input of every move): set while the forward key is held and while the autorun is on.
/// Observed: in the newchar capture it is set in all 12,756 records with the forward key and in no
/// other; in the autorun capture (`brim_runtoggle_spam`) each press of the autorun key (value 1
/// bit 24) turned it on or off with no movement key held, and the own place moved forward at the
/// sprint speed while it was on. (The replication note read it as a run toggle.)
const FORWARD_NAV_KEY: (usize, u32) = (0, 14);
/// The slot values and bits of the presses of the first and second weapon keys (input note: the
/// weapon in the hand changed 331 to 418 ms after each).
const WEAPON_KEYS: [(usize, u32); 2] = [(0, 9), (3, 13)];
/// The slot value and bit of the sheathe key (the Azoth catalog input 428, `sheathe`: input `n` is
/// value `n / 64` bit `n % 64`). Retail, 721 ledgers: with every weapon sheathed the server
/// drew the selected option 42 ms (median) after it, 106 of 109 presses.
const SHEATHE_KEY: (usize, u32) = (6, 44);
/// The slot value and bit of the summon mount key (the Azoth catalog input 26, `summon mount`).
/// Retail, 20 mount ledgers: each press got its 5747, 5937 or 5752 within 40 ms (Mount-quest
/// capture: 13 summons, 13 dismounts, 1 refusal); one record per press.
const SUMMON_MOUNT_KEY: (usize, u32) = (0, 26);
/// The slot values and bits of the movement keys, set while the key is held (movement study
/// 2026-09-20: in 15,000 retail records of the newchar and clean movement captures, the own
/// player's next place moved from the last one at a median angle to the camera yaw of 0.1° with
/// forward alone (10,799 records), 180° with back (1,532), 87° with right (249), -87° with left
/// (163), 44° with forward and right (1,364), -44° with forward and left (759), 136° with back
/// and right (363) and -135° with back and left (270)).
const FORWARD_KEY: (usize, u32) = (6, 32);
const BACK_KEY: (usize, u32) = (6, 5);
const LEFT_KEY: (usize, u32) = (4, 32);
const RIGHT_KEY: (usize, u32) = (4, 1);
/// The slot value and bit of the jump key, set while it is held (movement study: 10 of its 16
/// presses in the newchar capture lifted the own player 0.6 m or more within six states; the
/// others were on stairs and slopes).
const JUMP_KEY: (usize, u32) = (1, 9);
/// The slot value and bit of a press of the walk toggle (movement study: set in one record per
/// press with value 4 bit 3; in the clean toggle captures each of the 20 presses while running
/// switched the speed between 5.5 and 3.25 m/s forward, 4.0 and 3.25 m/s back).
const WALK_TOGGLE_KEY: (usize, u32) = (4, 22);
/// The slot value and bit of a press of the crouch toggle (the Azoth catalog input 277, `crouch
/// toggle`; posture study 2026-09-20: set in one record per press, 7 presses in the three retail
/// `walk_running_crouch_prone` captures, each followed by the crouch speeds: 2.50 m/s forward, the
/// sprint action list's `Crouch` `Speed` 2.5).
const CROUCH_TOGGLE_KEY: (usize, u32) = (4, 21);
/// The slot value and bit of a press of the prone toggle (the Azoth catalog input 331, `prone
/// toggle`; posture study: set in one record per press, 5 presses, after the first of each pair
/// the own place went 1.23 to 1.26 m/s forward, the sprint action list's `Prone` `Speed` 1.25).
const PRONE_TOGGLE_KEY: (usize, u32) = (5, 11);
/// The slot value and bit of the walk key, set while it is held (the Azoth catalog input 313,
/// `walk`; posture study: set in every record of the 4 slow periods of the
/// `walk_running_crouch_prone` captures and in no record of their running periods).
const WALK_KEY: (usize, u32) = (4, 57);
/// The slot values and bits of the presses that change the camera's boom (movement study, in
/// the newchar capture: value 2 bit 35 lengthened the resting boom by 1 m in 9 of 11 presses,
/// value 5 bit 57 shortened it by 1 m or ended the short boom in 28 of 35, value 5 bit 51 set
/// the short 2.75 m boom in all 5).
const BOOM_KEYS: [(usize, u32); 3] = [(2, 35), (5, 57), (5, 51)];
/// Slots of a client record (19 words of the action set, as 10 values).
const ACTION_SLOTS: u8 = 19;
/// The neutral scaled bytes of a record.
const NEUTRAL_SLOT_BYTES: [u8; SLOT_BYTES] = [128; SLOT_BYTES];
/// Input bits in one slot value: an input number is its value x 64 + its bit.
const INPUT_BITS_PER_VALUE: u32 = u64::BITS;

impl InputRecord {
    fn slot_bit(&self, value: usize, bit: u32) -> bool {
        self.slots
            .values
            .get(value)
            .is_some_and(|slots| slots >> bit & 1 == 1)
    }

    /// Whether the player holds block in this record.
    pub fn blocking(&self) -> bool {
        self.slot_bit(BLOCK_SLOT_VALUE, BLOCK_SLOT_BIT)
    }

    /// Whether the player holds the dodge key in this record.
    pub fn dodging(&self) -> bool {
        self.slot_bit(DODGE_SLOT_VALUE, DODGE_SLOT_BIT)
    }

    /// Whether the player presses quick slot 1 in this record.
    pub fn using_quick_slot_1(&self) -> bool {
        self.slot_bit(QUICK_SLOT_1_VALUE, QUICK_SLOT_1_BIT)
    }

    /// Whether this record holds input number `number` (the place of its action name in the
    /// client's input actions, [`nw_formats::input_profile`]).
    pub fn input_on(&self, number: u32) -> bool {
        self.slot_bit(
            (number / INPUT_BITS_PER_VALUE) as usize,
            number % INPUT_BITS_PER_VALUE,
        )
    }

    /// The ability key (0, 1 or 2) whose input this record holds ([`crate::ABILITY_KEYS`]).
    pub fn ability_key(&self) -> Option<usize> {
        crate::ABILITY_KEYS
            .iter()
            .position(|number| self.input_on(*number))
    }

    /// Whether the player pressed the attack key in this record.
    pub fn attack_pressed(&self) -> bool {
        self.slot_bit(ATTACK_PRESS_VALUE, ATTACK_PRESS_BIT)
    }

    /// Whether the player let go of the attack key in this record.
    pub fn attack_released(&self) -> bool {
        self.slot_bit(ATTACK_RELEASE_VALUE, ATTACK_RELEASE_BIT)
    }

    /// Whether the player holds the attack key in this record.
    pub fn attack_held(&self) -> bool {
        self.slot_bit(ATTACK_HELD_KEY.0, ATTACK_HELD_KEY.1)
    }

    /// Whether the player pressed the fishing key in this record.
    pub fn fishing_toggle_pressed(&self) -> bool {
        self.slot_bit(FISHING_TOGGLE_KEY.0, FISHING_TOGGLE_KEY.1)
    }

    /// Whether the player pressed the bait window key in this record.
    pub fn bait_window_pressed(&self) -> bool {
        self.slot_bit(BAIT_WINDOW_KEY.0, BAIT_WINDOW_KEY.1)
    }

    /// The entity the player locked on in this record: the id of row 3 (lock-on note: retail
    /// sent it only while locked on a target).
    pub fn lock_target(&self) -> Option<u64> {
        entity_of_raw_id(&self.row_3_id?.0)
    }

    /// The slayer action the client starts in this record (row 1), when it starts one.
    pub fn slayer_input(&self) -> Option<SlayerInput> {
        let [sequence, states, second_sequence] = self.row_1?;
        (sequence != 0).then_some(SlayerInput {
            sequence,
            state: (states & STATE_MASK) as u16,
            second_state: ((states >> SECOND_STATE_SHIFT) & STATE_MASK) as u16,
            second_sequence,
        })
    }

    /// The weapon key the player pressed in this record: 0 for the first, 1 for the second.
    pub fn weapon_key_pressed(&self) -> Option<u32> {
        (0_u32..)
            .zip(WEAPON_KEYS)
            .find(|(_, (value, bit))| self.slot_bit(*value, *bit))
            .map(|(key, _)| key)
    }

    /// The movement keys the player holds in this record.
    pub fn move_keys(&self) -> MoveKeys {
        let held = |(value, bit): (usize, u32)| self.slot_bit(value, bit);
        MoveKeys {
            forward: held(FORWARD_KEY) || held(FORWARD_NAV_KEY),
            back: held(BACK_KEY),
            left: held(LEFT_KEY),
            right: held(RIGHT_KEY),
            jump: held(JUMP_KEY),
        }
    }

    /// Whether the player presses the walk toggle in this record.
    pub fn walk_toggle_pressed(&self) -> bool {
        self.slot_bit(WALK_TOGGLE_KEY.0, WALK_TOGGLE_KEY.1)
    }

    /// Whether the player presses the crouch toggle in this record.
    pub fn crouch_toggle_pressed(&self) -> bool {
        self.slot_bit(CROUCH_TOGGLE_KEY.0, CROUCH_TOGGLE_KEY.1)
    }

    /// Whether the player presses the prone toggle in this record.
    pub fn prone_toggle_pressed(&self) -> bool {
        self.slot_bit(PRONE_TOGGLE_KEY.0, PRONE_TOGGLE_KEY.1)
    }

    /// Whether the player holds the walk key in this record.
    pub fn walk_held(&self) -> bool {
        self.slot_bit(WALK_KEY.0, WALK_KEY.1)
    }

    /// Whether the player presses the sheathe key in this record.
    pub fn sheathe_pressed(&self) -> bool {
        self.slot_bit(SHEATHE_KEY.0, SHEATHE_KEY.1)
    }

    /// Whether the player presses the summon mount key in this record: a summon, or a
    /// dismount while riding.
    pub fn summon_mount_pressed(&self) -> bool {
        self.slot_bit(SUMMON_MOUNT_KEY.0, SUMMON_MOUNT_KEY.1)
    }

    /// Whether the player presses a key that changes the camera's boom in this record.
    pub fn boom_key_pressed(&self) -> bool {
        BOOM_KEYS
            .iter()
            .any(|&(value, bit)| self.slot_bit(value, bit))
    }
}

/// The movement keys of one input record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoveKeys {
    /// Forward.
    pub forward: bool,
    /// Back.
    pub back: bool,
    /// Left.
    pub left: bool,
    /// Right.
    pub right: bool,
    /// Jump.
    pub jump: bool,
}

/// A slayer action the client starts: row 1 of an input record (slayer note, 2026-09-18: in the
/// retail newchar capture, each of the 73 non-zero rows came back in the next own ALC state, its
/// sequence as child 15, its state as child 13, its second state plus one as child 16 and its
/// second sequence as child 18, started in the state numbered with the record's counter). The
/// server adds its own follow-ups and the movement states; the client sends only these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlayerInput {
    /// Row 1 value 0: the slayer sequence (`slayerSequenceId`).
    pub sequence: u64,
    /// Row 1 value 1, low 16 bits: the slayer state (`slayerStateId`).
    pub state: u16,
    /// Row 1 value 1, bits 16 to 31: the second slot's state, `0xffff` for none.
    pub second_state: u16,
    /// Row 1 value 2: the second slot's sequence, 0 for none.
    pub second_sequence: u64,
}

/// The low 16 bits of row 1 value 1: one slot's state.
const STATE_MASK: u64 = 0xffff;
/// Where the second slot's state starts in row 1 value 1.
const SECOND_STATE_SHIFT: u32 = 16;

/// The slots of a record with only `bit` of value `value` set.
fn one_action(value: usize, bit: u32) -> InputSlots {
    let mut values = vec![0; usize::from(ACTION_SLOTS).div_ceil(2)];
    values[value] = 1 << bit;
    InputSlots {
        count: ACTION_SLOTS,
        values,
        bytes: NEUTRAL_SLOT_BYTES,
    }
}

impl ClientInputMsg {
    /// An input with an empty head and one record that holds only `position`, as the test client
    /// sends it.
    pub fn position(position: InputPosition) -> Self {
        Self::position_with_block(position, false)
    }

    /// As [`Self::position`], with the block key held when `blocking`.
    pub fn position_with_block(position: InputPosition, blocking: bool) -> Self {
        Self {
            records: vec![InputRecord {
                mask: 1 << BIT_7,
                bit_7: Some(position),
                slots: if blocking {
                    one_action(BLOCK_SLOT_VALUE, BLOCK_SLOT_BIT)
                } else {
                    InputSlots::default()
                },
                ..InputRecord::default()
            }],
            ..Self::default()
        }
    }

    /// A light attack at `position`: the press in the record of counter `counter`, the release
    /// in the next, newest first, as the client sends them.
    pub fn attack(position: InputPosition, counter: u16) -> Self {
        let record = |counter, slots| InputRecord {
            mask: 1 << BIT_7,
            u16_after_mask: counter,
            bit_7: Some(position),
            slots,
            ..InputRecord::default()
        };
        Self {
            records: vec![
                record(
                    counter.wrapping_add(1),
                    one_action(ATTACK_RELEASE_VALUE, ATTACK_RELEASE_BIT),
                ),
                record(counter, one_action(ATTACK_PRESS_VALUE, ATTACK_PRESS_BIT)),
            ],
            ..Self::default()
        }
    }

    /// The same input with the interaction `interaction` (bit 8) in its first record: the
    /// client sends its interaction in every input until the interaction ends.
    #[must_use]
    pub fn interacting(mut self, interaction: Bit8) -> Self {
        if let Some(record) = self.records.first_mut() {
            record.mask |= 1 << BIT_8;
            record.bit_8 = Some(interaction);
        }
        self
    }

    /// The same input with the view `view` (yaw and pitch, bit 6) in every record: the client
    /// looks that way.
    #[must_use]
    pub fn looking(mut self, view: [f32; 2]) -> Self {
        for record in &mut self.records {
            record.mask |= 1 << BIT_6;
            record.bit_6 = Some(view);
        }
        self
    }

    /// The newest position: that of the first record that has one. The client sends the newest
    /// record first (see [`Self::blocking`]); the later records repeat older inputs.
    pub fn newest_position(&self) -> Option<InputPosition> {
        self.records.iter().find_map(|record| record.bit_7)
    }

    /// The newest position, the two view values (bit 6) and the counter (`u16` after the mask,
    /// the client's own clock: one record per frame step, input note: 34 ms apart, median) of the
    /// same record: the client's camera point, its yaw and pitch, and when it was. The counter is
    /// that record's, not the newest record's: 3% of retail inputs (762 of 23,932, newchar
    /// capture) have a newest record without a position, and timing their older position by the
    /// newer counter made the camera look still, then too fast (live 2026-09-18: the body
    /// estimate flipped by 1.87 m between two inputs and moves were refused).
    pub fn newest_view(&self) -> Option<(InputPosition, Option<[f32; 2]>, u16)> {
        self.records
            .iter()
            .find_map(|record| Some((record.bit_7?, record.bit_6, record.u16_after_mask)))
    }

    /// Whether the player holds block now: the state of the newest record. The client sends the
    /// newest record first (the counter after the mask falls by one per record in the live
    /// capture). `None` for an input without records.
    pub fn blocking(&self) -> Option<bool> {
        self.records.first().map(InputRecord::blocking)
    }

    /// The interaction of the first record that carries one.
    pub fn interaction(&self) -> Option<InteractionRequest> {
        self.records
            .iter()
            .find_map(|record| record.bit_8.as_ref()?.interaction())
    }

    /// The object name of the interaction of [`ClientInputMsg::interaction`]; empty when its
    /// body is too short to name one.
    pub fn interaction_name(&self) -> Vec<u8> {
        self.records
            .iter()
            .find_map(|record| {
                let bit_8 = record.bit_8.as_ref()?;
                bit_8.interaction()?;
                Some(bit_8.interaction_name().unwrap_or_default())
            })
            .unwrap_or_default()
    }
}

impl BodyCodec for ClientInputMsg {
    const TYPE_INDEX: u64 = RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        writer.u64(self.component).u64(self.u64_at_8);
        self.head.write(&mut writer)?;
        let count = u8::try_from(self.records.len()).map_err(|_| MessageError::CountTooLarge {
            count: u64::try_from(self.records.len()).unwrap_or(u64::MAX),
            limit: u64::from(u8::MAX),
        })?;
        writer.u8(count);
        let mut carried = Carried::default();
        for record in &self.records {
            record.write(&mut writer, &mut carried)?;
        }
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let component = reader.u64()?;
        let u64_at_8 = reader.u64()?;
        let head = InputHead::read(&mut reader)?;
        let count = reader.u8()?;
        let mut carried = Carried::default();
        let records = (0..count)
            .map(|_| InputRecord::read(&mut reader, &mut carried))
            .collect::<Result<Vec<_>, _>>()?;
        reader.finish()?;
        Ok(Self {
            component,
            u64_at_8,
            head,
            records,
        })
    }
}

impl InputHead {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let bytes_1_to_4 = reader.array()?;
        let step_5 = reader.wide_prefixed()?;
        let steps_6_7 = [reader.prefixed()?, reader.prefixed()?];
        let step_8 = reader.u8()?;
        let prefixed = if reader.boolean()? {
            Some(reader.prefixed()?)
        } else {
            None
        };
        let step_9 = HeadStep9 {
            prefixed,
            u64_at_end: reader.u64()?,
        };
        let step_10 = reader.u8()?;
        let steps_11_to_13 = [reader.prefixed()?, reader.prefixed()?, reader.prefixed()?];
        let step_14 = reader.u8()?;
        let pairs = reader.prefixed()?;
        if pairs > MAX_HEAD_PAIRS {
            return Err(MessageError::CountTooLarge {
                count: pairs,
                limit: MAX_HEAD_PAIRS,
            });
        }
        let step_15_pairs = (0..pairs)
            .map(|_| Ok([reader.prefixed()?, reader.prefixed()?]))
            .collect::<Result<Vec<_>, MessageError>>()?;
        let step_15_tail = [reader.prefixed()?, reader.prefixed()?];
        let steps_16_17 = [reader.prefixed()?, reader.prefixed()?];
        Ok(Self {
            bytes_1_to_4,
            step_5,
            steps_6_7,
            step_8,
            step_9,
            step_10,
            steps_11_to_13,
            step_14,
            step_15_pairs,
            step_15_tail,
            steps_16_17,
        })
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let pairs = u64::try_from(self.step_15_pairs.len()).unwrap_or(u64::MAX);
        if pairs > MAX_HEAD_PAIRS {
            return Err(MessageError::CountTooLarge {
                count: pairs,
                limit: MAX_HEAD_PAIRS,
            });
        }
        writer.bytes(&self.bytes_1_to_4).wide_prefixed(self.step_5);
        for value in self.steps_6_7 {
            writer.prefixed(value)?;
        }
        writer
            .u8(self.step_8)
            .boolean(self.step_9.prefixed.is_some());
        if let Some(value) = self.step_9.prefixed {
            writer.prefixed(value)?;
        }
        writer.u64(self.step_9.u64_at_end).u8(self.step_10);
        for value in self.steps_11_to_13 {
            writer.prefixed(value)?;
        }
        writer.u8(self.step_14).prefixed(pairs)?;
        for value in self.step_15_pairs.iter().flatten() {
            writer.prefixed(*value)?;
        }
        for value in self.step_15_tail.iter().chain(&self.steps_16_17) {
            writer.prefixed(*value)?;
        }
        Ok(())
    }
}

impl InputSlots {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let count = reader.u8()?;
        let mut values = Vec::new();
        let mut left = count;
        while left > 0 {
            if left == 1 {
                values.push(reader.prefixed()?);
                left = 0;
            } else {
                values.push(reader.wide_prefixed()?);
                left -= 2;
            }
        }
        Ok(Self {
            count,
            values,
            bytes: reader.array()?,
        })
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let expected = usize::from(self.count).div_ceil(2);
        if self.values.len() != expected {
            return Err(MessageError::LengthMismatch { field: "slots" });
        }
        writer.u8(self.count);
        let odd = self.count % 2 == 1;
        for (index, value) in self.values.iter().enumerate() {
            if odd && index + 1 == self.values.len() {
                writer.prefixed(*value)?;
            } else {
                writer.wide_prefixed(*value);
            }
        }
        writer.bytes(&self.bytes);
        Ok(())
    }
}

impl InputRecord {
    fn read(reader: &mut Reader<'_>, carried: &mut Carried) -> Result<Self, MessageError> {
        let mask_bytes: [u8; MASK_LEN] = reader.array()?;
        let mask = u32::from_le_bytes([mask_bytes[0], mask_bytes[1], mask_bytes[2], 0]);
        let bit = |n| mask_bit(mask, n);
        let mut record = Self {
            mask,
            u16_after_mask: reader.u16()?,
            ..Self::default()
        };
        if bit(BIT_0) {
            let prefix = reader.wide_prefixed()?;
            record.bit_0 = Some(prefix);
            carried.prefix = prefix & u64::from(u32::MAX);
        }
        record.bit_1 = bit(BIT_1).then(|| reader.u8()).transpose()?;
        record.bit_2 = bit(BIT_2).then(|| reader.boolean()).transpose()?;
        record.bit_3 = bit(BIT_3).then(|| reader.u8()).transpose()?;
        record.bit_4 = bit(BIT_4).then(|| reader.u16()).transpose()?;
        record.slots = InputSlots::read(reader)?;
        record.bit_5 = bit(BIT_5).then(|| reader.f32()).transpose()?;
        record.bit_6 = bit(BIT_6)
            .then(|| Ok::<_, MessageError>([reader.f32()?, reader.f32()?]))
            .transpose()?;
        record.bit_7 = read_position(reader, bit(BIT_7))?;
        let tag = reader.u8()?;
        if tag != AFTER_POSITION_END {
            record.after_position = Some((tag, reader.array()?));
        }
        if bit(BIT_8) {
            record.bit_8 = Some(if reader.boolean()? {
                Bit8::Body(read_long_body(reader)?.to_vec())
            } else {
                Bit8::Empty
            });
        }
        record.read_rows(reader, carried)?;
        Ok(record)
    }

    /// The rows after bit 8, as [`RowLayout`] gives them.
    fn read_rows(
        &mut self,
        reader: &mut Reader<'_>,
        carried: &mut Carried,
    ) -> Result<(), MessageError> {
        let first = RowLayout::new(self.mask, carried.prefix, carried.gate);
        if first.row_1 {
            self.row_1 = Some([reader.prefixed()?, reader.prefixed()?, reader.prefixed()?]);
        }
        self.row_2 = read_position(reader, first.row_2)?;
        self.row_3_position = read_position(reader, first.row_3_position)?;
        self.row_3_id = first
            .row_3_id
            .then(|| Ok::<_, MessageError>((reader.array()?, reader.u64()?)))
            .transpose()?;
        if first.row_4 {
            let gate = reader.u8()?;
            self.row_4 = Some(gate);
            carried.gate = gate;
        }
        let layout = RowLayout::new(self.mask, carried.prefix, carried.gate);
        self.row_5_position = read_position(reader, layout.row_5_position)?;
        self.row_5_block = layout.row_5_block.then(|| reader.array()).transpose()?;
        self.row_6 = layout.row_6.then(|| reader.array()).transpose()?;
        let [len_104, len_192, len_96] = ELEMENT_LENS;
        self.elements_104 = read_elements(reader, len_104)?;
        self.elements_192 = read_elements(reader, len_192)?;
        self.elements_96 = read_elements(reader, len_96)?;
        self.row_10 = read_position(reader, layout.row_10)?;
        self.row_11 = layout
            .row_11
            .then(|| Ok::<_, MessageError>([reader.u16()?, reader.u16()?, reader.u16()?]))
            .transpose()?;
        self.row_12 = layout.row_12.then(|| reader.u64()).transpose()?;
        self.row_13 = layout.row_13.then(|| reader.u8()).transpose()?;
        self.row_14 = layout.row_14.then(|| reader.u8()).transpose()?;
        self.row_15 = layout.row_15.then(|| reader.u32()).transpose()?;
        self.row_16 = layout.row_16.then(|| reader.u16()).transpose()?;
        Ok(())
    }

    /// The rows this record holds, in the shape of [`RowLayout`].
    fn present_rows(&self) -> RowLayout {
        RowLayout {
            row_1: self.row_1.is_some(),
            row_2: self.row_2.is_some(),
            row_3_position: self.row_3_position.is_some(),
            row_3_id: self.row_3_id.is_some(),
            row_4: self.row_4.is_some(),
            row_5_position: self.row_5_position.is_some(),
            row_5_block: self.row_5_block.is_some(),
            row_6: self.row_6.is_some(),
            row_10: self.row_10.is_some(),
            row_11: self.row_11.is_some(),
            row_12: self.row_12.is_some(),
            row_13: self.row_13.is_some(),
            row_14: self.row_14.is_some(),
            row_15: self.row_15.is_some(),
            row_16: self.row_16.is_some(),
        }
    }

    /// Writes the record after the records that left `carried`. Refuses a record whose fields
    /// do not match its mask, or whose rows are not the ones `decode` would read.
    fn write(&self, writer: &mut Writer, carried: &mut Carried) -> Result<(), MessageError> {
        self.check_mask()?;
        if let Some(prefix) = self.bit_0 {
            carried.prefix = prefix & u64::from(u32::MAX);
        }
        if let Some(gate) = self.row_4 {
            carried.gate = gate;
        }
        if self.present_rows() != RowLayout::new(self.mask, carried.prefix, carried.gate) {
            return Err(MessageError::RowsMismatch);
        }
        let mask_bytes = self.mask.to_le_bytes();
        writer
            .bytes(&mask_bytes[..MASK_LEN])
            .u16(self.u16_after_mask);
        if let Some(prefix) = self.bit_0 {
            writer.wide_prefixed(prefix);
        }
        if let Some(value) = self.bit_1 {
            writer.u8(value);
        }
        if let Some(value) = self.bit_2 {
            writer.boolean(value);
        }
        if let Some(value) = self.bit_3 {
            writer.u8(value);
        }
        if let Some(value) = self.bit_4 {
            writer.u16(value);
        }
        self.slots.write(writer)?;
        if let Some(value) = self.bit_5 {
            writer.f32(value);
        }
        if let Some([first, second]) = self.bit_6 {
            writer.f32(first).f32(second);
        }
        if let Some(position) = &self.bit_7 {
            position.write(writer);
        }
        match &self.after_position {
            None => {
                writer.u8(AFTER_POSITION_END);
            }
            Some((AFTER_POSITION_END, _)) => {
                return Err(MessageError::BadTag {
                    field: "after_position",
                    tag: AFTER_POSITION_END,
                });
            }
            Some((tag, bytes)) => {
                writer.u8(*tag).bytes(bytes);
            }
        }
        match &self.bit_8 {
            None => {}
            Some(Bit8::Empty) => {
                writer.boolean(false);
            }
            Some(Bit8::Body(body)) => {
                let mut check = Reader::new(body);
                read_long_body(&mut check)?;
                check.finish()?;
                writer.boolean(true).bytes(body);
            }
        }
        self.write_rows(writer)
    }

    fn write_rows(&self, writer: &mut Writer) -> Result<(), MessageError> {
        if let Some(values) = self.row_1 {
            for value in values {
                writer.prefixed(value)?;
            }
        }
        for position in [&self.row_2, &self.row_3_position].into_iter().flatten() {
            position.write(writer);
        }
        if let Some((raw, value)) = &self.row_3_id {
            writer.bytes(raw).u64(*value);
        }
        if let Some(gate) = self.row_4 {
            writer.u8(gate);
        }
        if let Some(position) = &self.row_5_position {
            position.write(writer);
        }
        if let Some(block) = &self.row_5_block {
            writer.bytes(block);
        }
        if let Some(raw) = &self.row_6 {
            writer.bytes(raw);
        }
        for (elements, len) in [&self.elements_104, &self.elements_192, &self.elements_96]
            .into_iter()
            .zip(ELEMENT_LENS)
        {
            write_elements(writer, elements, len)?;
        }
        if let Some(position) = &self.row_10 {
            position.write(writer);
        }
        if let Some(values) = self.row_11 {
            for value in values {
                writer.u16(value);
            }
        }
        if let Some(value) = self.row_12 {
            writer.u64(value);
        }
        for value in [self.row_13, self.row_14].into_iter().flatten() {
            writer.u8(value);
        }
        if let Some(value) = self.row_15 {
            writer.u32(value);
        }
        if let Some(value) = self.row_16 {
            writer.u16(value);
        }
        Ok(())
    }

    /// Each fixed field is present exactly when its mask bit is set.
    fn check_mask(&self) -> Result<(), MessageError> {
        let present = [
            self.bit_0.is_some(),
            self.bit_1.is_some(),
            self.bit_2.is_some(),
            self.bit_3.is_some(),
            self.bit_4.is_some(),
            self.bit_5.is_some(),
            self.bit_6.is_some(),
            self.bit_7.is_some(),
            self.bit_8.is_some(),
        ];
        match (0..FIXED_BITS).find(|&bit| mask_bit(self.mask, bit) != present[bit as usize]) {
            Some(bit) => Err(MessageError::MaskMismatch {
                // `bit` is below 9.
                bit: bit as u8,
            }),
            None => Ok(()),
        }
    }
}

fn read_position(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<Option<InputPosition>, MessageError> {
    present.then(|| InputPosition::read(reader)).transpose()
}

fn read_elements(reader: &mut Reader<'_>, len: usize) -> Result<Vec<u8>, MessageError> {
    let count = reader.prefixed()?;
    if count > MAX_ELEMENTS {
        return Err(MessageError::CountTooLarge {
            count,
            limit: MAX_ELEMENTS,
        });
    }
    let total = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(len))
        .ok_or(MessageError::CountTooLarge {
            count,
            limit: MAX_ELEMENTS,
        })?;
    Ok(reader.bytes(total)?.to_vec())
}

fn write_elements(writer: &mut Writer, elements: &[u8], len: usize) -> Result<(), MessageError> {
    if !elements.len().is_multiple_of(len) {
        return Err(MessageError::LengthMismatch { field: "elements" });
    }
    writer.prefixed(u64::try_from(elements.len() / len).unwrap_or(u64::MAX))?;
    writer.bytes(elements);
    Ok(())
}

/// A string of the bit 8 body (`FUN_14087a3d0`): a prefix-coded length and the bytes.
fn skip_string(reader: &mut Reader<'_>) -> Result<(), MessageError> {
    let len = reader.prefixed()?;
    if len > MAX_STRING {
        return Err(MessageError::CountTooLarge {
            count: len,
            limit: MAX_STRING,
        });
    }
    // `len` is at most `MAX_STRING`.
    reader.bytes(len as usize)?;
    Ok(())
}

/// Long body steps 1 to 7: 16 raw bytes, `u64`, `u64`, `u8`, `u8`, `u64`, `u32`.
const LONG_START_LEN: usize = 46;
/// Steps 8 (after its string) and 9: two `u32`, then an `f32`.
const LONG_AFTER_NAME_LEN: usize = 12;
/// Step 17: two groups of three `f32`, when step 16 is 1.
const LONG_VECTORS_LEN: usize = 24;
/// Steps 19 to 25: a `u32`, a `u8`, five `u32`.
const LONG_COUNTERS_LEN: usize = 25;
/// Steps 30 and 31: a `u32` and a `u8`.
const LONG_STEPS_30_31_LEN: usize = 5;
/// Step 33 after its string, and step 34: a `u32`, a `u64`, a `u32`.
const LONG_TAIL_LEN: usize = 16;
/// Steps 11 to 14 are strings.
const LONG_STRINGS_11_TO_14: usize = 4;

/// Reads the long body of bit 8 (the `FUN_1435ebc30` table of the widths that the movement
/// message note cites) and returns its bytes.
fn read_long_body<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], MessageError> {
    let start = reader.rest();
    let before = reader.remaining();
    reader.bytes(LONG_START_LEN)?;
    skip_string(reader)?;
    reader.bytes(LONG_AFTER_NAME_LEN)?;
    reader.boolean()?;
    for _ in 0..LONG_STRINGS_11_TO_14 {
        skip_string(reader)?;
    }
    reader.f32()?;
    if reader.boolean()? {
        reader.bytes(LONG_VECTORS_LEN)?;
    }
    reader.boolean()?;
    reader.bytes(LONG_COUNTERS_LEN)?;
    reader.prefixed()?;
    reader.u32()?;
    reader.prefixed()?;
    skip_string(reader)?;
    reader.bytes(LONG_STEPS_30_31_LEN)?;
    reader.boolean()?;
    reader.u32()?;
    skip_string(reader)?;
    reader.bytes(LONG_TAIL_LEN)?;
    reader.boolean()?;
    let used = before - reader.remaining();
    Ok(&start[..used])
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn row_1_is_the_slayer_action_the_client_starts() {
        // Retail newchar capture, record 2286: row 1 `[1038, 0x0024001b, 5008]`.
        let record = InputRecord {
            row_1: Some([1038, 0x0024_001b, 5008]),
            ..InputRecord::default()
        };
        assert_eq!(
            record.slayer_input(),
            Some(SlayerInput {
                sequence: 1038,
                state: 0x1b,
                second_state: 0x24,
                second_sequence: 5008,
            })
        );
        let idle = InputRecord {
            row_1: Some([0, 0, 0]),
            ..InputRecord::default()
        };
        assert_eq!(idle.slayer_input(), None, "a zero row starts nothing");
        assert_eq!(InputRecord::default().slayer_input(), None);
    }

    const X: f32 = 11_132.5;
    const Y: f32 = 7_305.9;
    const Z: f32 = 170.0;
    const HEIGHT_STEP_M: f32 = 0.02;
    const PREFIX_MASK_ROW_1: u64 = 1;
    const PREFIX_MASK_ROW_2: u64 = 1 << 11;
    const GATE: u8 = 5;
    const ELEMENT_COUNT: usize = 2;

    fn position() -> InputPosition {
        InputPosition::from_metres(X, Y, Z).expect("finite")
    }

    /// A record with every mask bit 0 to 8 set and the rows of prefix bits 0 and 11.
    fn full_record() -> InputRecord {
        InputRecord {
            mask: 0x1FF | (1 << 9) | (1 << 10),
            u16_after_mask: 7,
            bit_0: Some(PREFIX_MASK_ROW_1 | PREFIX_MASK_ROW_2),
            bit_1: Some(1),
            bit_2: Some(true),
            bit_3: Some(3),
            bit_4: Some(4),
            slots: InputSlots {
                count: 3,
                values: vec![u64::MAX, 9],
                bytes: [1, 2, 3, 4],
            },
            bit_5: Some(1.5),
            bit_6: Some([2.5, 3.5]),
            bit_7: Some(position()),
            after_position: Some((0, [6; AFTER_POSITION_LEN])),
            bit_8: Some(Bit8::Empty),
            row_1: Some([1, 2, 3]),
            row_2: Some(position()),
            row_4: Some(GATE),
            elements_104: vec![8; ELEMENT_LENS[0] * ELEMENT_COUNT],
            ..InputRecord::default()
        }
    }

    /// A record with the retail slot values `values`.
    #[test]
    fn an_ability_key_is_its_input_number() {
        // Input 402 (key 1) is value 6 bit 18; 139 (key 3) is value 2 bit 11.
        let mut values = [0_u64; 10];
        values[6] = 1 << 18;
        assert_eq!(with_slots(values).ability_key(), Some(0));
        let mut values = [0_u64; 10];
        values[2] = 1 << 11;
        assert_eq!(with_slots(values).ability_key(), Some(2));
        assert_eq!(with_slots([0; 10]).ability_key(), None);
    }

    fn with_slots(values: [u64; 10]) -> InputRecord {
        InputRecord {
            slots: InputSlots {
                count: ACTION_SLOTS,
                values: values.to_vec(),
                bytes: NEUTRAL_SLOT_BYTES,
            },
            ..InputRecord::default()
        }
    }

    /// The slot values of a standing retail record: values 7 and 8 are set in every record.
    const STANDING: [u64; 10] = [
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0x0402_0000_0800_0000,
        0x0200_0000_0000_0000,
        0,
    ];

    /// [`STANDING`] with `extra` set in each value.
    fn standing_with(extra: [u64; 10]) -> InputRecord {
        let mut values = STANDING;
        for (value, more) in values.iter_mut().zip(extra) {
            *value |= more;
        }
        with_slots(values)
    }

    #[test]
    fn the_movement_keys_read_as_in_the_retail_records() {
        // Retail records (movement study): forward (running forward capture, every record),
        // back (walking back capture), right and left (newchar records 1903 and 4880), forward
        // with right (newchar 2594) and forward with the jump (newchar open world 22640).
        const FORWARD: [u64; 10] = [
            0x0800_0000_4000,
            0,
            0,
            0,
            0,
            0,
            0x1_0000_0000,
            0,
            0x4000_0000,
            0,
        ];
        const BACK: [u64; 10] = [0, 0, 0, 0x0400_0000_0000_0000, 0, 0x80, 0x20, 0, 0, 0];
        const RIGHT: [u64; 10] = [0, 0, 0, 0, 2, 0, 0, 0, 0x0100_0000_1000_0000, 0];
        const LEFT: [u64; 10] = [0, 0, 0, 0x0100_0000_0080_0000, 0x1_0000_0000, 0, 0, 0, 0, 0];
        const FORWARD_RIGHT: [u64; 10] = [
            0x0800_0000_4000,
            0,
            0,
            0,
            2,
            0,
            0x1_0000_0000,
            0,
            0x0100_0000_5000_0000,
            0,
        ];
        const FORWARD_JUMP: [u64; 10] = [
            0x0040_0800_0000_4000,
            0x0040_0000_0200,
            0,
            0,
            0,
            0x1_0000_0000,
            0x1_0800_0000,
            0,
            0x4000_0000,
            0,
        ];
        let keys = |forward, back, left, right, jump| MoveKeys {
            forward,
            back,
            left,
            right,
            jump,
        };
        let cases = [
            ([0; 10], keys(false, false, false, false, false)),
            (FORWARD, keys(true, false, false, false, false)),
            (BACK, keys(false, true, false, false, false)),
            (RIGHT, keys(false, false, false, true, false)),
            (LEFT, keys(false, false, true, false, false)),
            (FORWARD_RIGHT, keys(true, false, false, true, false)),
            (FORWARD_JUMP, keys(true, false, false, false, true)),
        ];
        for (extra, expected) in cases {
            assert_eq!(standing_with(extra).move_keys(), expected, "{extra:x?}");
        }
        assert_eq!(InputRecord::default().move_keys(), MoveKeys::default());
    }

    #[test]
    fn the_autorun_moves_forward_without_the_forward_key() {
        // Retail autorun capture (`brim_runtoggle_spam`): record 187 presses the autorun key,
        // records 188 on carry the forward navigation bit alone.
        const PRESS: [u64; 10] = [0, 0x100_0000, 0, 0, 0, 0, 0, 0, 0, 0];
        const AUTORUN: [u64; 10] = [0x4000, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(standing_with(PRESS).move_keys(), MoveKeys::default());
        let running = standing_with(AUTORUN);
        assert_eq!(
            running.move_keys(),
            MoveKeys {
                forward: true,
                ..MoveKeys::default()
            }
        );
    }

    #[test]
    fn the_walk_toggle_and_the_boom_keys_read_as_in_the_retail_records() {
        // A walk toggle press while running forward (toggle capture record 321), a press that
        // lengthened the boom (newchar 6453), one that shortened it (newchar 7042) and one that
        // set the short boom (newchar 4521).
        const TOGGLE: [u64; 10] = [
            0x0800_0000_4000,
            0,
            0,
            0,
            0x40_0008,
            0,
            0x1_0000_0000,
            0,
            0x4000_0000,
            0,
        ];
        const LONGER: [u64; 10] = [
            0,
            0x1000_0000_0000,
            0x0808_0000_0000,
            0,
            0,
            0x100_0000,
            0,
            0,
            0,
            0,
        ];
        const SHORTER: [u64; 10] = [
            0x80,
            0x0800_0000_0000,
            0x0800_0000_0000,
            0x8000_0000_0000,
            0,
            0x0200_0000_0000_0000,
            0x0080_0000_0000_0000,
            0,
            0,
            0x200,
        ];
        const SHORT: [u64; 10] = [0, 0, 0, 0, 0, 0x0008_0000_0000_0000, 0, 0, 0, 0];
        let toggle = standing_with(TOGGLE);
        assert!(toggle.walk_toggle_pressed());
        assert!(!toggle.boom_key_pressed());
        assert!(toggle.move_keys().forward);
        for boom in [LONGER, SHORTER, SHORT] {
            assert!(standing_with(boom).boom_key_pressed(), "{boom:x?}");
            assert!(!standing_with(boom).walk_toggle_pressed());
        }
        assert!(!with_slots(STANDING).boom_key_pressed());
    }

    #[test]
    fn the_posture_toggles_and_the_walk_key_read_as_in_the_retail_records() {
        // Retail `walk_running_crouch_prone_2` record 902 (a crouch press), 1328 (a prone press)
        // and `walk_running_crouch_prone` record 600 (running forward with the walk key held).
        const CROUCH: [u64; 10] = [
            0x100_0000_0000_0000,
            0x80_0000_0000_0000,
            0x2000_0000,
            0x4000_0000_0000,
            0x20_0000,
            0,
            0,
            0x402_0000_0800_0000,
            0x200_0000_8000_0000,
            0,
        ];
        const PRONE: [u64; 10] = [
            0,
            0,
            0x40_0000_0000_0000,
            0x800_0000_0000_0000,
            0,
            0x800,
            0x2_0000_0000_0000,
            0x402_0000_0808_0000,
            0x200_0000_0000_0000,
            0,
        ];
        const WALKING: [u64; 10] = [
            0x800_0000_4000,
            0,
            0x800_0000_0000,
            0,
            0x200_0000_0000_0000,
            0,
            0x4001_0000_0000,
            0x402_0000_0800_0000,
            0x200_0004_4000_0000,
            0x200,
        ];
        let crouch = standing_with(CROUCH);
        assert!(crouch.crouch_toggle_pressed());
        assert!(!crouch.prone_toggle_pressed() && !crouch.walk_held());
        let prone = standing_with(PRONE);
        assert!(prone.prone_toggle_pressed());
        assert!(!prone.crouch_toggle_pressed() && !prone.walk_held());
        let walking = standing_with(WALKING);
        assert!(walking.walk_held() && walking.move_keys().forward);
        assert!(!walking.crouch_toggle_pressed() && !walking.prone_toggle_pressed());
        assert!(!walking.walk_toggle_pressed());
    }

    #[test]
    fn the_summon_mount_press_reads_as_in_the_retail_record() {
        // Retail slot values of the record of a summon press (Mount-quest capture, the press
        // that got the summon 40 ms later) and of the record after it.
        const PRESS: [u64; 10] = [
            0x400_0400_0080,
            0x800_0000_0000,
            0x800_0000_0002,
            0,
            0,
            0,
            0x2080_0000_0000_0000,
            0x402_0000_0800_0000,
            0x200_0000_0001_0000,
            0x200,
        ];
        const AFTER: [u64; 10] = [
            0x80,
            0x800_0000_0000,
            0,
            0,
            0,
            0,
            0x80_0000_0000_0000,
            0x402_0000_0800_0000,
            0x200_0000_0000_0000,
            0,
        ];
        assert!(with_slots(PRESS).summon_mount_pressed());
        assert!(!with_slots(AFTER).summon_mount_pressed());
    }

    #[test]
    fn the_newest_record_says_whether_the_player_blocks() {
        // Retail slot values of a record while the player held block (live capture, 93347 ms).
        const BLOCKING: [u64; 10] = [
            8_796_093_022_208,
            4_611_686_018_427_387_904,
            128,
            0,
            2_251_799_813_685_250,
            0,
            0,
            288_797_724_287_172_608,
            216_172_782_382_219_264,
            1024,
        ];
        let blocking = InputRecord {
            slots: InputSlots {
                count: 19,
                values: BLOCKING.to_vec(),
                bytes: [128; SLOT_BYTES],
            },
            ..InputRecord::default()
        };
        let open = full_record();
        assert!(blocking.blocking());
        assert!(!open.blocking());
        assert_eq!(
            message(vec![blocking.clone(), open.clone()]).blocking(),
            Some(true)
        );
        assert_eq!(message(vec![open, blocking]).blocking(), Some(false));
        assert_eq!(message(Vec::new()).blocking(), None);
        // Retail dodge press (slot value 1 bit 3) and quick slot 1 press (bit 23).
        let press = |value: u64| InputRecord {
            slots: InputSlots {
                count: 19,
                values: vec![0, value],
                bytes: [128; SLOT_BYTES],
            },
            ..InputRecord::default()
        };
        assert!(press(8_796_093_022_216).dodging());
        assert!(!press(8_796_093_022_208).dodging());
        assert!(press(8_388_608).using_quick_slot_1());
        assert!(!press(8_388_608).dodging());
        let at = InputPosition::from_metres(1.0, 2.0, 3.0).expect("finite");
        for blocking in [true, false] {
            let input = ClientInputMsg::position_with_block(at, blocking);
            let read = ClientInputMsg::decode(&input.encode().expect("encode")).expect("decode");
            assert_eq!(read.blocking(), Some(blocking));
            assert_eq!(read.newest_position(), Some(at));
        }
    }

    fn message(records: Vec<InputRecord>) -> ClientInputMsg {
        ClientInputMsg {
            component: 1,
            u64_at_8: 2,
            head: InputHead {
                bytes_1_to_4: [1, 2, 3, 4],
                step_5: u64::from(u32::MAX),
                steps_6_7: [6, 7],
                step_8: 8,
                step_9: HeadStep9 {
                    prefixed: Some(9),
                    u64_at_end: 10,
                },
                step_10: 10,
                steps_11_to_13: [11, 12, 13],
                step_14: 14,
                step_15_pairs: vec![[15, 16]],
                step_15_tail: [17, 18],
                steps_16_17: [19, 20],
            },
            records,
        }
    }

    #[test]
    fn a_position_only_input_round_trips_and_gives_its_position() {
        let input = ClientInputMsg::position(position());
        let body = input.encode().expect("encode");
        let decoded = ClientInputMsg::decode(&body).expect("decode");
        assert_eq!(decoded, input);
        assert_eq!(decoded.newest_position(), Some(position()));
        assert_eq!(decoded.encode().expect("again"), body);
    }

    #[test]
    fn the_newest_view_carries_the_counter_of_its_own_record() {
        const NEWEST: u16 = 41;
        const WITH_POSITION: u16 = 40;
        let input = ClientInputMsg {
            records: vec![
                InputRecord {
                    u16_after_mask: NEWEST,
                    ..InputRecord::default()
                },
                InputRecord {
                    u16_after_mask: WITH_POSITION,
                    bit_7: Some(position()),
                    bit_6: Some([1.0, 0.5]),
                    ..InputRecord::default()
                },
            ],
            ..ClientInputMsg::default()
        };
        assert_eq!(
            input.newest_view(),
            Some((position(), Some([1.0, 0.5]), WITH_POSITION))
        );
    }

    #[test]
    fn the_newest_position_is_that_of_the_first_record() {
        let older = InputPosition::from_metres(1.0, 2.0, 3.0).expect("finite");
        let input = ClientInputMsg {
            records: [
                vec![InputRecord::default()],
                ClientInputMsg::position(position()).records,
                ClientInputMsg::position(older).records,
            ]
            .concat(),
            ..ClientInputMsg::default()
        };
        assert_eq!(input.newest_position(), Some(position()));
    }

    #[test]
    fn the_retail_attack_records_press_hold_and_release() {
        // Retail records u1857 (key down), u1858 (held) and u1860 (key up) of the tutorial.
        let record = |values: [u64; 10]| InputRecord {
            slots: InputSlots {
                count: ACTION_SLOTS,
                values: values.to_vec(),
                bytes: NEUTRAL_SLOT_BYTES,
            },
            ..InputRecord::default()
        };
        let down = record([
            576_460_752_303_423_488,
            0,
            1_099_511_627_776,
            536_872_960,
            0,
            549_755_813_888,
            288_230_376_168_488_960,
            288_793_326_239_350_784,
            144_123_984_168_878_080,
            0,
        ]);
        let held = record([
            0,
            0,
            9_223_372_036_854_775_808,
            0,
            576_460_752_303_423_488,
            549_755_813_888,
            0,
            288_793_326_239_350_784,
            144_115_188_092_633_088,
            0,
        ]);
        let up = record([
            576_460_752_303_423_488,
            0,
            2_147_483_648,
            0,
            0,
            0,
            0,
            288_793_326_239_350_784,
            180_143_985_094_819_840,
            16_384,
        ]);
        assert!(down.attack_pressed() && !down.attack_released());
        assert!(!held.attack_pressed() && !held.attack_released());
        assert!(!up.attack_pressed() && up.attack_released());
    }

    #[test]
    fn the_target_row_names_the_locked_entity() {
        const MOB: u64 = 0x4_0000_0004;
        let locked = InputRecord {
            row_3_id: Some((crate::wire::raw_id_of_entity(MOB), 0)),
            ..InputRecord::default()
        };
        assert_eq!(locked.lock_target(), Some(MOB));
        assert_eq!(InputRecord::default().lock_target(), None);
    }

    #[test]
    fn an_attack_input_presses_then_releases_newest_first() {
        const COUNTER: u16 = 40;
        let input = ClientInputMsg::attack(position(), COUNTER);
        let decoded = ClientInputMsg::decode(&input.encode().expect("encode")).expect("decode");
        assert_eq!(decoded, input);
        assert_eq!(decoded.newest_position(), Some(position()));
        let [release, press] = decoded.records.as_slice() else {
            panic!("two records");
        };
        assert!(press.attack_pressed() && press.u16_after_mask == COUNTER);
        assert!(release.attack_released() && release.u16_after_mask == COUNTER + 1);
    }

    #[test]
    fn a_looking_input_carries_its_view_in_every_record() {
        const VIEW: [f32; 2] = [1.5, -0.25];
        let input = ClientInputMsg::attack(position(), 1).looking(VIEW);
        let decoded = ClientInputMsg::decode(&input.encode().expect("encode")).expect("decode");
        assert!(
            decoded
                .records
                .iter()
                .all(|record| record.bit_6 == Some(VIEW))
        );
        assert_eq!(
            decoded.newest_view().and_then(|(_, view, _)| view),
            Some(VIEW)
        );
    }

    #[test]
    fn the_height_maps_from_minus_100_to_1000() {
        let low = InputPosition::from_metres(0.0, 0.0, INPUT_Z_MIN).expect("finite");
        let high = InputPosition::from_metres(0.0, 0.0, INPUT_Z_MAX).expect("finite");
        assert_eq!((low.z_raw, high.z_raw), (0, u16::MAX));
        assert!((position().z() - Z).abs() < HEIGHT_STEP_M);
        assert_eq!(InputPosition::from_metres(f32::NAN, 0.0, 0.0), None);
    }

    #[test]
    fn a_full_record_round_trips() {
        let input = message(vec![full_record()]);
        let body = input.encode().expect("encode");
        assert_eq!(ClientInputMsg::decode(&body), Ok(input));
    }

    #[test]
    fn the_prefix_and_the_gate_carry_to_the_next_record() {
        // The second record sets neither mask bit 0 nor the gate bit: rows 1 and 2 still run
        // (carried prefix), and row 5 runs (carried gate; its position is mask bit 11).
        let carried = InputRecord {
            mask: (1 << 9) | (1 << 11),
            row_1: Some([4, 5, 6]),
            row_2: Some(position()),
            row_5_position: Some(position()),
            ..InputRecord::default()
        };
        let input = message(vec![full_record(), carried]);
        let body = input.encode().expect("encode");
        assert_eq!(ClientInputMsg::decode(&body), Ok(input));
    }

    #[test]
    fn a_long_bit_8_body_keeps_its_bytes() {
        let mut long = Writer::new();
        long.bytes(&[1; 16]).u64(2).u64(3).u8(4).u8(5).u64(6).u32(7);
        long.prefixed(2).expect("string").bytes(b"ab").u32(8).u32(9);
        long.f32(1.0).boolean(true);
        for _ in 0..4 {
            long.prefixed(0).expect("string");
        }
        long.f32(2.0)
            .boolean(true)
            .bytes(&[0; 24])
            .boolean(false)
            .u32(10)
            .u8(11);
        for value in 0..5 {
            long.u32(value);
        }
        long.prefixed(12)
            .expect("prefix")
            .u32(13)
            .prefixed(14)
            .expect("prefix");
        long.prefixed(0)
            .expect("string")
            .u32(15)
            .u8(16)
            .boolean(false);
        long.u32(17)
            .prefixed(1)
            .expect("string")
            .bytes(b"c")
            .u32(18)
            .u64(19);
        long.u32(20).boolean(true);
        let raw = long.finish();
        let record = InputRecord {
            mask: 1 << 8,
            bit_8: Some(Bit8::Body(raw)),
            ..InputRecord::default()
        };
        let input = message(vec![record]);
        let body = input.encode().expect("encode");
        assert_eq!(ClientInputMsg::decode(&body), Ok(input.clone()));
        assert_eq!(
            input.interaction(),
            Some(InteractionRequest {
                target: [1; 16],
                target_entity: 2,
                interaction: 3,
                kind: 4,
                gatherable: None,
            })
        );
        assert_eq!(Bit8::Empty.interaction(), None);
    }

    #[test]
    fn a_gather_names_its_gatherable() {
        // Retail flint request (gather note, newchar 739603): the first 46 of its 130 bytes.
        const RETAIL_START: [u8; 46] = [
            0x5b, 0x12, 0x3e, 0xc6, 0x38, 0xec, 0x89, 0x54, 0x9e, 0x70, 0xd2, 0x6a, 0x66, 0x94,
            0xaf, 0xc5, 0x54, 0x89, 0xec, 0x38, 0xc6, 0x3e, 0x12, 0x5b, 0x05, 0x3c, 0xaa, 0xdb,
            0xcc, 0xbb, 0xf4, 0xe5, 0x0e, 0x0e, 0x54, 0x89, 0xec, 0x38, 0xc6, 0x3e, 0x12, 0x5b,
            0xa4, 0x17, 0x13, 0x7f,
        ];
        let request = Bit8::Body(RETAIL_START.to_vec())
            .interaction()
            .expect("a request");
        assert_eq!(request.kind, GATHER_KIND);
        assert_eq!(request.target_entity, 0x5489_ec38_c63e_125b);
        assert_eq!(request.gatherable, Some(0xa417_137f), "SingleStone");
        let cut = Bit8::Body(RETAIL_START[..40].to_vec())
            .interaction()
            .expect("a request");
        assert_eq!(cut.gatherable, None, "too short for the gatherable");
    }

    #[test]
    fn a_built_gather_reads_back_as_the_request_it_names() {
        const TARGET: [u8; RAW_ID_LEN] = [7; RAW_ID_LEN];
        const ENTITY: u64 = 0x5489_ec38_c63e_125b;
        const INTERACTION: u64 = 0x053c_aadb_ccbb_f4e5;
        const SINGLE_STONE: u32 = 0xa417_137f;
        let body = Bit8::gather(TARGET, ENTITY, INTERACTION, SINGLE_STONE).expect("body");
        let input =
            ClientInputMsg::position(InputPosition::from_metres(10.0, 20.0, 30.0).expect("finite"))
                .interacting(body);
        let read = ClientInputMsg::decode(&input.encode().expect("encode")).expect("decode");
        assert_eq!(
            read.interaction(),
            Some(InteractionRequest {
                target: TARGET,
                target_entity: ENTITY,
                interaction: INTERACTION,
                kind: GATHER_KIND,
                gatherable: Some(SINGLE_STONE),
            })
        );
    }

    #[test]
    fn a_lore_note_names_its_title() {
        // Retail lore read (2026-09-18, 19:21:17): the first 89 of its 158 bytes.
        const RETAIL_LORE: &str = concat!(
            "53197aa7269658ee0570d26a6694afc5ee589626a77a1953c62f1d8915639c32",
            "2f2fee589625a77a19530000000000000000000000000000000000",
            "011c404b7562696b6972697342757264656e5f5469746c655f30315f3031"
        );
        let body: Vec<u8> = (0..RETAIL_LORE.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&RETAIL_LORE[at..at + 2], 16).expect("hex"))
            .collect();
        let lore = Bit8::Body(body.clone());
        assert_eq!(
            lore.interaction().map(|request| request.kind),
            Some(LORE_KIND)
        );
        assert_eq!(
            lore.interaction_name().as_deref(),
            Some(&b"@KubikirisBurden_Title_01_01"[..])
        );
        assert_eq!(
            Bit8::Body(body[..NAME_LENGTH_AT + 3].to_vec()).interaction_name(),
            None,
            "a name cut short"
        );
        let input = message(vec![InputRecord {
            mask: 1 << 8,
            bit_8: Some(lore),
            ..InputRecord::default()
        }]);
        assert_eq!(input.interaction_name(), b"@KubikirisBurden_Title_01_01");
        assert!(message(Vec::new()).interaction_name().is_empty());
    }

    #[test]
    fn a_mask_that_does_not_match_the_fields_is_refused() {
        let record = InputRecord {
            mask: 1 << 7,
            ..InputRecord::default()
        };
        assert!(matches!(
            message(vec![record]).encode(),
            Err(MessageError::MaskMismatch { bit: 7 })
        ));
        let bad_end = InputRecord {
            after_position: Some((AFTER_POSITION_END, [0; AFTER_POSITION_LEN])),
            ..InputRecord::default()
        };
        assert!(message(vec![bad_end]).encode().is_err());
    }

    #[test]
    fn rows_that_the_mask_prefix_and_gate_do_not_hold_are_refused() {
        let position_only = || InputRecord {
            mask: 1 << 7,
            bit_7: Some(position()),
            ..InputRecord::default()
        };
        let gate_without_bit = InputRecord {
            row_4: Some(1),
            ..position_only()
        };
        assert_eq!(
            message(vec![gate_without_bit]).encode(),
            Err(MessageError::RowsMismatch)
        );
        let row_1_without_prefix = InputRecord {
            row_1: Some([1, 2, 3]),
            ..position_only()
        };
        assert_eq!(
            message(vec![row_1_without_prefix]).encode(),
            Err(MessageError::RowsMismatch)
        );
        let short_slots = InputRecord {
            slots: InputSlots {
                count: 3,
                values: vec![1],
                bytes: [0; 4],
            },
            ..position_only()
        };
        assert_eq!(
            message(vec![short_slots]).encode(),
            Err(MessageError::LengthMismatch { field: "slots" })
        );
        let odd_elements = InputRecord {
            elements_96: vec![0; ELEMENT_LENS[2] + 1],
            ..position_only()
        };
        assert_eq!(
            message(vec![odd_elements]).encode(),
            Err(MessageError::LengthMismatch { field: "elements" })
        );
    }

    #[test]
    fn a_short_body_or_bytes_left_over_are_errors() {
        let body = ClientInputMsg::position(position())
            .encode()
            .expect("encode");
        assert!(ClientInputMsg::decode(&body[..body.len() - 1]).is_err());
        let mut longer = body.clone();
        longer.push(0);
        assert!(matches!(
            ClientInputMsg::decode(&longer),
            Err(MessageError::TrailingBytes { left: 1 })
        ));
    }

    proptest! {
        #[test]
        fn random_bodies_never_panic(body in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = ClientInputMsg::decode(&body);
        }
    }
}
