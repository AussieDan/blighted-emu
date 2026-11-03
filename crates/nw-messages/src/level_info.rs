// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Javelin::ClientMessagesTrait::LevelInfoChangedMsg` (type 1635). It tells the client which
//! level to load and the session context to use from then on. The client handler
//! (`FUN_146446800`) runs `LoadContextAndLevel` only after both this message and
//! `PlayerManagerSelfIdentification` (type 1628) have arrived, which is one of the conditions for
//! the client to leave `WaitingForPlayerSpawn` and enter the world.
//!
//! Layout derived from existing data, not a fresh decompile: the field list of
//! `local-data/azoth-catalog/packets/1635.json` plus the captured body lengths in the
//! bootstrap-bodies note (open world is the 106-byte body). The published field list sums to 89
//! bytes; the 17-byte remainder is exactly one phasing entry (`u8` + `u32` + `u64` + `u32`), so
//! the open-world message carries `phasing.count = 1`. The byte offsets in that archive are from
//! one sample and are not reliable; the field order and the 106-byte total are.
//!
//! Two hard client rules from the handler: `is_master` must be 1 (with 0 the handler logs
//! "IsMasterPlayer is false" and drops the message), and each phasing entry's `phase_id` must not
//! be 0.

use crate::wire::{BodyCodec, MessageError, Reader, Writer};

/// File slot of `LevelInfoChangedMsg`.
pub const LEVEL_INFO_CHANGED_MSG_TYPE_INDEX: u64 = 1635;

/// One level the client can load: what `LevelInfoChangedMsg` names and the frame it sends
/// (new-player-start note: the open world and the tutorial `FTUE_V2`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelSpec {
    /// Our level folder name (world files and sim config), for example `ftue_v2`.
    pub folder: &'static str,
    /// The level name the client loads (`map <name>`).
    pub map_name: &'static [u8],
    /// Where the level's terrain data lives.
    pub coatlicue_path: &'static [u8],
    /// Phasing-grid origin x and y, metres.
    pub phasing_origin: [f32; 2],
    /// Phasing-grid width and height, metres.
    pub phasing_bounds: [f32; 2],
    /// `flagA3`: 0 in the open world, 1 in the tutorial, 0 in every expedition, raid and trial.
    pub flag_a3: u8,
    /// Whether every visit is a private instance: the tutorial and each expedition, raid and
    /// trial (phasing key type 3, the phase id also in `reserved`), not the shared open world.
    pub private: bool,
}

/// The open world.
pub const OPEN_WORLD_LEVEL: LevelSpec = LevelSpec {
    folder: "newworld_vitaeeterna",
    map_name: b"NewWorld_VitaeEterna",
    coatlicue_path: b"coatlicue/NewWorld_VitaeEterna",
    // 12272 m = 48 cells of 256 m; 10250 m = 41 cells of 256 m (Azoth catalog 1635 spec).
    phasing_origin: [2048.0, 16.0],
    phasing_bounds: [12272.0, 10250.0],
    flag_a3: 0,
    private: false,
};

/// The new-player tutorial: a private level before the open world.
pub const TUTORIAL_LEVEL: LevelSpec = LevelSpec {
    folder: "ftue_v2",
    map_name: b"FTUE_V2",
    coatlicue_path: b"coatlicue/FTUE_V2",
    phasing_origin: [400.0, 150.0],
    phasing_bounds: [416.0, 736.0],
    flag_a3: 1,
    private: true,
};

/// Every level this server can name.
pub const LEVELS: [LevelSpec; 2] = [OPEN_WORLD_LEVEL, TUTORIAL_LEVEL];

/// `flagA3` of an expedition, raid or trial level (Observed: 0 in the 1635 of every instance of
/// 7 retail kinds: expeditions, raids, soul trials, catacombs and the instanced open-world areas).
const INSTANCE_FLAG_A3: u8 = 0;

impl LevelSpec {
    /// The level whose folder name is `folder`.
    pub fn of_folder(folder: &str) -> Option<Self> {
        LEVELS.into_iter().find(|level| level.folder == folder)
    }

    /// The private instance level of a game mode map (`GameModeMapData`): retail's 1635 of every
    /// expedition, raid and trial names the map and terrain path of its `CoatlicueName` and sends
    /// its `WorldBounds` (x, y, width, height) as the phasing frame (Observed, 7 instance kinds).
    /// The strings live for the rest of the process: build each level once, at start.
    pub fn instance(folder: &str, map_name: &str, coatlicue_path: &str, frame: [f32; 4]) -> Self {
        let [x, y, width, height] = frame;
        Self {
            folder: Box::leak(folder.to_owned().into_boxed_str()),
            map_name: Box::leak(map_name.as_bytes().to_vec().into_boxed_slice()),
            coatlicue_path: Box::leak(coatlicue_path.as_bytes().to_vec().into_boxed_slice()),
            phasing_origin: [x, y],
            phasing_bounds: [width, height],
            flag_a3: INSTANCE_FLAG_A3,
            private: true,
        }
    }
}

/// The `clientCtx` of the first level of a session; each later level change takes the next one.
pub const LOGIN_CLIENT_CONTEXT: u8 = 1;

/// `flagA0` in the spec: 1 in every capture.
const FLAG_A0: u8 = 1;
/// `is_master`: must be 1 or the client drops the message.
const IS_MASTER: u8 = 1;
/// The `reserved` field of the open world: 0 in the captures.
const OPEN_WORLD_RESERVED: u64 = 0;
/// `keyType` of a territory or point-of-interest area entry.
const AREA_KEY_TYPE: u8 = 1;
/// `keyType` of a background grid cell entry.
const GRID_KEY_TYPE: u8 = 2;
/// The area of the start beach (open-world note: retail sent it after the tutorial exit, 654279
/// ms; the client showed "You cannot access this area" at the beach with our key 0).
const START_BEACH_AREA: u32 = 0x2331;
/// The width of one phasing grid cell, metres (Azoth catalog 1635 spec: the open world is 48 x 41
/// cells of 256 m).
const PHASING_CELL_M: f32 = 256.0;
/// The grid cells a player is phased in: the 2 x 2 nearest it (Inferred from retail updates).
const NEAR_CELLS: usize = 2;
/// Bits of the x cell in a grid key: `key_id = (cell_y << 16) | cell_x` (Observed).
const GRID_KEY_Y_SHIFT: u32 = 16;
/// `keyType` of the tutorial's phasing entry: a private instance.
const INSTANCE_KEY_TYPE: u8 = 3;
/// `extra` of the tutorial's phasing entry.
const INSTANCE_EXTRA: u32 = 2;

/// One phasing entry: which phase the player is in for one sector of the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhasingEntry {
    /// `keyType`: 1 = territory/point-of-interest area, 2 = background grid cell, 3 = private
    /// instance (tutorial).
    pub key_type: u8,
    /// The sector key, read according to `key_type`.
    pub key_id: u32,
    /// The phase the player is in for that sector. Must not be 0.
    pub phase_id: u64,
    /// A trailing value the captures show as 0 or 2.
    pub extra: u32,
}

impl PhasingEntry {
    /// The start beach's area entry in `phase_id`.
    pub const fn start_beach(phase_id: u64) -> Self {
        Self::area(START_BEACH_AREA, phase_id)
    }

    /// The entry of the phasing area `area` (its `TerritoryID`) in `phase_id`.
    pub const fn area(area: u32, phase_id: u64) -> Self {
        Self {
            key_type: AREA_KEY_TYPE,
            key_id: area,
            phase_id,
            extra: 0,
        }
    }

    /// The open-world entries of a player at (`x`, `y`) in `phase_id`: the level's phasing
    /// areas `areas` and the 2 x 2 grid cells nearest the player inside the grid of `level`. A
    /// sector around the player without an entry makes the client show "You cannot access this
    /// area" (client `FUN_143f03170`), and the level objects in a phasing area without an entry
    /// carry phase 0 in their ids: the client then names no entry into the areas there (live
    /// 2026-11-03, Brysedd Overlook Shrine in Monarch's Bluffs settlement's area 30155; retail
    /// `12a_msq_12_camptutorial` sent 30155 and 30112 entries and the shrine's id carried
    /// 30155's phase).
    pub fn open_world(
        level: &LevelSpec,
        x: f32,
        y: f32,
        phase_id: u64,
        areas: &[u32],
    ) -> Vec<Self> {
        let [origin_x, origin_y] = level.phasing_origin;
        let [width, height] = level.phasing_bounds;
        // The cells of one axis: the cell of the point and its neighbour on the nearer side.
        let near = |at: f32, origin: f32, size: f32| -> Vec<u32> {
            let cells = (size / PHASING_CELL_M).ceil();
            let cell = ((at - origin) / PHASING_CELL_M).floor();
            let neighbour = if (at - origin) / PHASING_CELL_M - cell < 0.5 {
                cell - 1.0
            } else {
                cell + 1.0
            };
            let mut axis: Vec<u32> = [cell.min(neighbour), cell.max(neighbour)]
                .into_iter()
                .filter(|cell| (0.0..cells).contains(cell))
                // Below the grid size, a few dozen.
                .map(|cell| cell as u32)
                .collect();
            axis.truncate(NEAR_CELLS);
            axis
        };
        let columns = near(x, origin_x, width);
        let rows = near(y, origin_y, height);
        areas
            .iter()
            .map(|area| Self::area(*area, phase_id))
            .chain(rows.iter().flat_map(|row| {
                columns.iter().map(move |column| Self {
                    key_type: GRID_KEY_TYPE,
                    key_id: (row << GRID_KEY_Y_SHIFT) | column,
                    phase_id,
                    extra: 0,
                })
            }))
            .collect()
    }

    /// Reads one entry: key type, key id, phase id, extra.
    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            key_type: reader.u8()?,
            key_id: reader.u32()?,
            phase_id: reader.u64()?,
            extra: reader.u32()?,
        })
    }

    /// Writes the entry.
    pub(crate) fn write(&self, writer: &mut Writer) {
        writer
            .u8(self.key_type)
            .u32(self.key_id)
            .u64(self.phase_id)
            .u32(self.extra);
    }
}

/// Tells the client which level to load and the session context to use.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelInfoChangedMsg {
    /// The level.
    pub level: LevelSpec,
    /// `reserved`: 0 in the open world; the instance phase id in the tutorial.
    pub reserved: u64,
    /// `clientCtx`: this level's client-context id. Every state bundle that follows carries the
    /// same number.
    pub client_context: u8,
    /// `instance`: a clock stamp for this level instance; the client only compares it with the
    /// previous one, so it must differ from the last and (first time) be non-zero.
    pub instance: u64,
    /// The phasing entries.
    pub phasing: Vec<PhasingEntry>,
}

impl LevelInfoChangedMsg {
    /// The open-world message: one phasing entry that places the player in `phase_id` of the
    /// whole-world area, with the given client-context id and instance stamp.
    pub fn open_world(client_context: u8, instance: u64, phase_id: u64) -> Self {
        Self {
            level: OPEN_WORLD_LEVEL,
            reserved: OPEN_WORLD_RESERVED,
            client_context,
            instance,
            phasing: vec![PhasingEntry::start_beach(phase_id)],
        }
    }

    /// The open-world message of a player who comes back from an instance at `position` (x, y): one
    /// phasing entry, the grid cell of that spot (retail: 4 of 6 instance exits; the other two
    /// named a territory area).
    pub fn open_world_at(
        client_context: u8,
        instance: u64,
        phase_id: u64,
        position: [f32; 2],
    ) -> Self {
        let [x, y] = position;
        let [origin_x, origin_y] = OPEN_WORLD_LEVEL.phasing_origin;
        // Cell indexes of a spot on the map, a few dozen.
        let cell = |at: f32, origin: f32| ((at - origin) / PHASING_CELL_M).floor().max(0.0) as u32;
        Self {
            level: OPEN_WORLD_LEVEL,
            reserved: OPEN_WORLD_RESERVED,
            client_context,
            instance,
            phasing: vec![PhasingEntry {
                key_type: GRID_KEY_TYPE,
                key_id: (cell(y, origin_y) << GRID_KEY_Y_SHIFT) | cell(x, origin_x),
                phase_id,
                extra: 0,
            }],
        }
    }

    /// The tutorial message: the private instance `phase_id` (also in `reserved`, as in the
    /// captures).
    pub fn tutorial(client_context: u8, instance: u64, phase_id: u64) -> Self {
        Self::private_instance(TUTORIAL_LEVEL, client_context, instance, phase_id)
    }

    /// The message of a private instance of `level` (the tutorial, an expedition, raid or trial):
    /// the instance `phase_id` in `reserved` and in its one phasing entry (Observed in the
    /// tutorial and every captured instance).
    pub fn private_instance(
        level: LevelSpec,
        client_context: u8,
        instance: u64,
        phase_id: u64,
    ) -> Self {
        Self {
            level,
            reserved: phase_id,
            client_context,
            instance,
            phasing: vec![PhasingEntry {
                key_type: INSTANCE_KEY_TYPE,
                key_id: 0,
                phase_id,
                extra: INSTANCE_EXTRA,
            }],
        }
    }

    /// The message of `level`: [`Self::private_instance`] for a private level (the tutorial or
    /// an instance), else [`Self::open_world`].
    pub fn of_level(level: LevelSpec, client_context: u8, instance: u64, phase_id: u64) -> Self {
        if level.private {
            Self::private_instance(level, client_context, instance, phase_id)
        } else {
            Self::open_world(client_context, instance, phase_id)
        }
    }
}

impl BodyCodec for LevelInfoChangedMsg {
    const TYPE_INDEX: u64 = LEVEL_INFO_CHANGED_MSG_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        writer.prefixed(self.level.map_name.len() as u64)?;
        writer.bytes(self.level.map_name);
        writer.prefixed(self.level.coatlicue_path.len() as u64)?;
        writer.bytes(self.level.coatlicue_path);
        writer
            .f32(self.level.phasing_origin[0])
            .f32(self.level.phasing_origin[1])
            .f32(self.level.phasing_bounds[0])
            .f32(self.level.phasing_bounds[1])
            .u64(self.reserved);
        writer.prefixed(self.phasing.len() as u64)?;
        for entry in &self.phasing {
            entry.write(&mut writer);
        }
        writer
            .u8(FLAG_A0)
            .u8(self.client_context)
            .u8(IS_MASTER)
            .u8(self.level.flag_a3)
            .u64(self.instance);
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let name = read_string(&mut reader)?;
        let level = LEVELS
            .into_iter()
            .find(|level| level.map_name == name)
            .ok_or(MessageError::LengthMismatch {
                field: "LevelInfoChangedMsg level name",
            })?;
        if read_string(&mut reader)? != level.coatlicue_path {
            return Err(MessageError::LengthMismatch {
                field: "LevelInfoChangedMsg terrain path",
            });
        }
        reader.f32()?;
        reader.f32()?;
        reader.f32()?;
        reader.f32()?;
        let reserved = reader.u64()?;
        let count = reader.prefixed()?;
        let mut phasing = Vec::new();
        for _ in 0..count {
            phasing.push(PhasingEntry::read(&mut reader)?);
        }
        reader.u8()?;
        let client_context = reader.u8()?;
        reader.u8()?;
        reader.u8()?;
        let instance = reader.u64()?;
        reader.finish()?;
        Ok(Self {
            level,
            reserved,
            client_context,
            instance,
            phasing,
        })
    }
}

/// Reads a prefix-coded string.
fn read_string<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], MessageError> {
    let len = reader.prefixed()?;
    reader.bytes(usize::try_from(len).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured open-world body length.
    const OPEN_WORLD_BODY_LEN: usize = 106;

    #[test]
    fn the_open_world_body_is_the_captured_length_and_round_trips() {
        let message = LevelInfoChangedMsg::open_world(1, 0x0102_0304_0506_0708, 1);
        let body = message.encode().expect("encode");
        assert_eq!(body.len(), OPEN_WORLD_BODY_LEN);
        assert_eq!(LevelInfoChangedMsg::decode(&body), Ok(message));
    }

    #[test]
    fn a_player_is_phased_in_the_level_s_areas_and_its_four_nearest_cells() {
        // The retail beach spot (7305, 3093): cell x 20 (7168-7424, east half: and 21), cell y 12
        // (3088-3344, south half: and 11).
        /// Two phasing areas of the level: the start beach and Monarch's Bluffs settlement.
        const AREAS: [u32; 2] = [START_BEACH_AREA, 30_155];
        let entries = PhasingEntry::open_world(&OPEN_WORLD_LEVEL, 7305.0, 3093.0, 1, &AREAS);
        let keys: Vec<(u8, u32)> = entries
            .iter()
            .map(|entry| (entry.key_type, entry.key_id))
            .collect();
        assert_eq!(
            keys,
            [
                (AREA_KEY_TYPE, START_BEACH_AREA),
                (AREA_KEY_TYPE, 30_155),
                (GRID_KEY_TYPE, (11 << 16) | 20),
                (GRID_KEY_TYPE, (11 << 16) | 21),
                (GRID_KEY_TYPE, (12 << 16) | 20),
                (GRID_KEY_TYPE, (12 << 16) | 21),
            ]
        );
        assert!(entries.iter().all(|entry| entry.phase_id == 1));
        // Outside the grid, only the cells inside it.
        let corner = PhasingEntry::open_world(&OPEN_WORLD_LEVEL, 2050.0, 20.0, 1, &AREAS);
        assert_eq!(corner.len(), 3, "the areas and cell (0, 0)");
    }

    #[test]
    fn an_empty_phasing_list_round_trips() {
        let message = LevelInfoChangedMsg {
            level: OPEN_WORLD_LEVEL,
            reserved: 0,
            client_context: 2,
            instance: 9,
            phasing: Vec::new(),
        };
        let body = message.encode().expect("encode");
        assert_eq!(LevelInfoChangedMsg::decode(&body), Ok(message));
    }

    #[test]
    fn a_wrong_level_name_is_rejected() {
        let message = LevelInfoChangedMsg::open_world(1, 1, 1);
        let mut body = message.encode().expect("encode");
        // Corrupt the first byte of the map name (after its 1-byte length prefix).
        body[1] ^= 0xff;
        assert!(matches!(
            LevelInfoChangedMsg::decode(&body),
            Err(MessageError::LengthMismatch { .. })
        ));
    }

    /// The captured tutorial body length (new-player-start note).
    const TUTORIAL_BODY_LEN: usize = 80;

    #[test]
    fn the_tutorial_body_is_the_captured_shape_and_round_trips() {
        const PHASE: u64 = 0x8b2c_bbb8_6aa9_9537;
        let message = LevelInfoChangedMsg::tutorial(LOGIN_CLIENT_CONTEXT, 7, PHASE);
        let body = message.encode().expect("encode");
        assert_eq!(body.len(), TUTORIAL_BODY_LEN);
        assert_eq!(&body[1..8], b"FTUE_V2");
        let decoded = LevelInfoChangedMsg::decode(&body).expect("decode");
        assert_eq!(decoded, message);
        assert_eq!(decoded.reserved, PHASE);
        assert_eq!(decoded.phasing[0].key_type, INSTANCE_KEY_TYPE);
        assert_eq!(LevelSpec::of_folder("ftue_v2"), Some(TUTORIAL_LEVEL));
        assert_eq!(
            LevelInfoChangedMsg::of_level(TUTORIAL_LEVEL, 1, 7, PHASE),
            message
        );
    }

    /// The frame of the Amrine expedition (`GameModeMapData` `WorldBounds` of `DungeonAmrine`).
    const AMRINE_FRAME: [f32; 4] = [610.0, 560.0, 352.0, 512.0];

    #[test]
    fn an_instance_level_names_its_map_and_frame_and_is_private() {
        const PHASE: u64 = 0xbd6e_b5a2_6a1f_2b2f;
        const STAMP: u64 = 0x0102;
        let level = LevelSpec::instance(
            "nw_dungeon_windsward_00",
            "NW_Dungeon_Windsward_00",
            "coatlicue/NW_Dungeon_Windsward_00",
            AMRINE_FRAME,
        );
        assert!(level.private);
        assert_eq!(level.flag_a3, INSTANCE_FLAG_A3);
        let message = LevelInfoChangedMsg::of_level(level, 2, STAMP, PHASE);
        assert_eq!(message.reserved, PHASE);
        let body = message.encode().expect("encode");
        // Retail Amrine 1635: 112 bytes; name, path, frame, reserved, one type-3 entry, then
        // `01 02 01 00` and the stamp.
        assert_eq!(body.len(), 112);
        assert_eq!(&body[1..24], b"NW_Dungeon_Windsward_00");
        let frame_at = 1 + 23 + 1 + 33;
        assert_eq!(&body[frame_at..frame_at + 4], &610.0_f32.to_be_bytes());
        let tail = body.len() - 12;
        assert_eq!(&body[tail..tail + 4], &[1, 2, 1, 0]);
        let entry = frame_at + 16 + 8 + 1;
        assert_eq!(body[entry], INSTANCE_KEY_TYPE);
        assert_eq!(&body[entry + 5..entry + 13], &PHASE.to_be_bytes());
        assert_eq!(&body[entry + 13..entry + 17], &INSTANCE_EXTRA.to_be_bytes());
    }

    #[test]
    fn a_return_from_an_instance_names_the_grid_cell_of_the_spot() {
        // Retail Amrine exit: one grid entry, key (28 << 16) | 22 for a spot in that cell.
        let x = 2048.0 + 22.0 * PHASING_CELL_M + 10.0;
        let y = 16.0 + 28.0 * PHASING_CELL_M + 10.0;
        let message = LevelInfoChangedMsg::open_world_at(3, 9, OPEN_WORLD_PHASE, [x, y]);
        assert_eq!(message.phasing.len(), 1);
        assert_eq!(message.phasing[0].key_type, GRID_KEY_TYPE);
        assert_eq!(message.phasing[0].key_id, (28 << 16) | 22);
        assert_eq!(message.reserved, OPEN_WORLD_RESERVED);
        let body = message.encode().expect("encode");
        assert_eq!(LevelInfoChangedMsg::decode(&body), Ok(message));
    }

    /// The phase of the shared open world in these tests.
    const OPEN_WORLD_PHASE: u64 = 1;
}
