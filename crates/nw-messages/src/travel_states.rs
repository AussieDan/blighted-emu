// SPDX-License-Identifier: AGPL-3.0-or-later
//! The replicated travel and housing states of the player prefab (run 15 travel2 study,
//! `local-data/scratch-work/run15/travel2`): the player time (type 3290, component 5), the
//! housing state (type 2768, component 6), the home points (type 3652, component 20) and the
//! camping state (type 1292, component 25).
//!
//! The children and their readers are the client's state constructors and child readers (build
//! 25810808): `FUN_14407ef70` (3290), `FUN_14356ac50` and the owned-house reader
//! `FUN_1435a0450` / `FUN_143562250` / `FUN_143561ff0` (2768), `FUN_1434e3e80` and the home point
//! reader `FUN_1434fe010` / `FUN_1434dc760` (3652), `FUN_1440c7c20` (1292). Every layout reads
//! back the retail first bodies of the captured sessions (`owncomp/first.json`: 2768 in 313 of
//! 313 whole bodies, 3652 in 477 of 480, 1292 and 3290 in all).
//!
//! A text is a wide prefix-coded length and its bytes (`FUN_14087a570`). A list is the replicated
//! list form ([`FullList`]).

use crate::items::FullList;
use crate::slots::{
    SLOT_0, children_mask, read_slot_0, read_slot_byte, read_wide_children, write_children,
    write_known,
};
use crate::travel::PlotRef;
use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// Type index of `MB::PlayerTimeComponentReplicatedState`.
pub const PLAYER_TIME_TYPE_INDEX: u64 = 3290;
/// The player component of the player time state.
pub const PLAYER_TIME_COMPONENT: u64 = 5;
/// Type index of `MB::PlayerHousingReplicatedState`.
pub const PLAYER_HOUSING_TYPE_INDEX: u64 = 2768;
/// The player component of the housing state.
pub const PLAYER_HOUSING_COMPONENT: u64 = 6;
/// Type index of `MB::PlayerHomeComponentReplicatedState`.
pub const PLAYER_HOME_TYPE_INDEX: u64 = 3652;
/// The player component of the home points.
pub const PLAYER_HOME_COMPONENT: u64 = 20;
/// Type index of `MB::CampingComponentReplicatedState`.
pub const CAMPING_TYPE_INDEX: u64 = 1292;
/// The player component of the camping state.
pub const CAMPING_COMPONENT: u64 = 25;
/// The entity id the housing state names for "no entity" (`m_phasedHousingPlotEntityId` of 60
/// of 64 retail bodies outside a phased house).
pub const NO_PHASED_ENTITY: u64 = 0x0000_0000_ffff_ffff;

/// Children of the player time: `startTimePoint`, `durationAtStart`, `paidDurationAtStart`.
const PLAYER_TIME_CHILDREN: u8 = 3;
/// Children of the housing state (see [`PlayerHousingState`]).
const PLAYER_HOUSING_CHILDREN: u8 = 9;
/// Children of the home state: `homePointList`, `homePointId`.
const PLAYER_HOME_CHILDREN: u8 = 2;
/// Children of the camping state: `currentCampRef`, `currentCampSkinId`, `isCampUnderAttack`,
/// `campBindCooldownEndTime`.
const CAMPING_CHILDREN: u8 = 4;
/// Most bytes of a text (`FUN_14087a570` refuses more than 0x2ffff).
const MAX_TEXT: u64 = 0x2_ffff;
/// Most trophies of a house (the owned-house reader refuses more than 0x200_0000).
const MAX_TROPHIES: u64 = 0x200_0000;
/// Coordinates of a position.
const POSITION_LEN: usize = 3;

const TIME_FIELD: &str = "player time";
const HOUSING_FIELD: &str = "player housing";
const HOME_FIELD: &str = "player home";
const CAMPING_FIELD: &str = "camping";

pub(crate) fn read_text(reader: &mut Reader<'_>) -> Result<Vec<u8>, MessageError> {
    let len = reader.wide_prefixed()?;
    if len > MAX_TEXT {
        return Err(MessageError::CountTooLarge {
            count: len,
            limit: MAX_TEXT,
        });
    }
    // `len` is at most `MAX_TEXT`.
    Ok(reader.bytes(len as usize)?.to_vec())
}

pub(crate) fn write_text(writer: &mut Writer, text: &[u8]) {
    // A `usize` length fits 64 bits.
    writer.wide_prefixed(text.len() as u64).bytes(text);
}

fn read_position(reader: &mut Reader<'_>) -> Result<[f32; POSITION_LEN], MessageError> {
    Ok([reader.f32()?, reader.f32()?, reader.f32()?])
}

fn write_position(writer: &mut Writer, position: &[f32; POSITION_LEN]) {
    for value in position {
        writer.f32(*value);
    }
}

/// Writes the slot byte and slot 0 with `children` set (none: the slot byte 0).
fn write_slot_0(
    writer: &mut Writer,
    children: u16,
    count: u8,
    write_child: impl FnMut(&mut Writer, u8) -> Result<(), MessageError>,
) -> Result<(), MessageError> {
    if children == 0 {
        writer.u8(0);
        return Ok(());
    }
    writer.u8(SLOT_0);
    write_children(writer, children, count, write_child)
}

/// `MB::PlayerTimeComponentReplicatedState`: when the session started and the played time then.
/// Retail: `startTimePoint` is the server time of the first own card in nanoseconds since
/// 2000-01-01 (both new characters of 2026-09-12/22), the durations are equal in every body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayerTimeState {
    /// Child 0 `startTimePoint`: ns since 2000-01-01.
    pub start_time_point: Option<u64>,
    /// Child 1 `durationAtStart`: the character's played time at that point, ns.
    pub duration_at_start_ns: Option<u64>,
    /// Child 2 `paidDurationAtStart`: the played time with a paid account, ns.
    pub paid_duration_at_start_ns: Option<u64>,
}

impl PlayerTimeState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early or sets an unsized child.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        read_slot_0(reader, TIME_FIELD, PLAYER_TIME_CHILDREN, |reader, child| {
            let value = Some(reader.u64()?);
            match child {
                0 => state.start_time_point = value,
                1 => state.duration_at_start_ns = value,
                _ => state.paid_duration_at_start_ns = value,
            }
            Ok(())
        })?;
        Ok(state)
    }

    /// Writes the state for the bundle path.
    pub fn write(&self, writer: &mut Writer) {
        let values = [
            self.start_time_point,
            self.duration_at_start_ns,
            self.paid_duration_at_start_ns,
        ];
        let present = values.map(|value| value.is_some());
        write_known(
            writer,
            children_mask(&present),
            PLAYER_TIME_CHILDREN,
            |writer, child| {
                if let Some(value) = values[usize::from(child)] {
                    writer.u64(value);
                }
            },
        );
    }

    /// The body bytes.
    pub fn body(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        self.write(&mut writer);
        writer.finish()
    }
}

/// The house data entity of a home (24 bytes: a raw id and its facet entity), as the owned house
/// and the client's 544 (`ClientRequestEnterPlot`) name it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HouseDataRef {
    /// The raw id.
    pub id: [u8; RAW_ID_LEN],
    /// Its facet entity.
    pub facet: u64,
}

impl HouseDataRef {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            id: reader.raw_id()?,
            facet: reader.u64()?,
        })
    }

    fn write(&self, writer: &mut Writer) {
        writer.bytes(&self.id).u64(self.facet);
    }
}

/// One owned house of the housing state (`FUN_1435a0450`; the debug dump `FUN_143576c40` names
/// the fields at the same element offsets).
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedHouse {
    /// The house plot, as the housing calls name it (+0x20).
    pub plot: PlotRef,
    /// The house data entity (+0x50).
    pub house_data: HouseDataRef,
    /// The plot's position, metres (+0x70).
    pub position: [f32; POSITION_LEN],
    /// When the taxes are due, Unix ns (+0x88; retail: the purchase or the last payment plus
    /// 7 days).
    pub taxes_due_ns: u64,
    /// When the house was bought, Unix ns (+0x98).
    pub purchased_ns: u64,
    /// The plot's `TerritoryID` (+0xa0, "Territory Id").
    pub territory: u16,
    /// "Buff items active" (+0xa2; retail 1 on 676 of 804 houses).
    pub buff_items_active: bool,
    /// The CRC-32 of each placed trophy's lowercase `HouseItemID` (+0xa8).
    pub trophies: Vec<u32>,
    /// "Decoration Score" (+0xc8).
    pub decoration_score: f32,
    /// A flag of unknown meaning (+0xe5; retail 1 on 756 of 804 houses).
    pub flag: bool,
    /// "Handshake with plot" (+0xe6; retail 1 on 756 of 804 houses).
    pub handshake: bool,
    /// When the fast travel home may be used again, Unix ns (+0xd8; 0 for at once).
    pub fast_travel_ready_ns: u64,
    /// The CRC-32 of the lowercase `HouseTypeID` (+0xe0; `housetypetier2` is `0xd0e40188`).
    pub house_type: u32,
}

impl OwnedHouse {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let plot = PlotRef::read(reader)?;
        let house_data = HouseDataRef::read(reader)?;
        let position = read_position(reader)?;
        let taxes_due_ns = reader.u64()?;
        let purchased_ns = reader.u64()?;
        let territory = reader.u16()?;
        let buff_items_active = reader.boolean()?;
        let count = reader.wide_prefixed()?;
        if count > MAX_TROPHIES {
            return Err(MessageError::CountTooLarge {
                count,
                limit: MAX_TROPHIES,
            });
        }
        let trophies = (0..count).map(|_| reader.u32()).collect::<Result<_, _>>()?;
        Ok(Self {
            plot,
            house_data,
            position,
            taxes_due_ns,
            purchased_ns,
            territory,
            buff_items_active,
            trophies,
            decoration_score: reader.f32()?,
            flag: reader.boolean()?,
            handshake: reader.boolean()?,
            fast_travel_ready_ns: reader.u64()?,
            house_type: reader.u32()?,
        })
    }

    fn write(&self, writer: &mut Writer) {
        self.plot.write(writer);
        self.house_data.write(writer);
        write_position(writer, &self.position);
        writer
            .u64(self.taxes_due_ns)
            .u64(self.purchased_ns)
            .u16(self.territory)
            .boolean(self.buff_items_active)
            // A `usize` count fits 64 bits.
            .wide_prefixed(self.trophies.len() as u64);
        for trophy in &self.trophies {
            writer.u32(*trophy);
        }
        writer
            .f32(self.decoration_score)
            .boolean(self.flag)
            .boolean(self.handshake)
            .u64(self.fast_travel_ready_ns)
            .u32(self.house_type);
    }
}

/// `MB::PlayerHousingReplicatedState` (`FUN_14356ac50`). Retail sends `hasPurchasedHouse`,
/// `ownedHouses` and `m_isFastTravelChanneling` to a house owner and only the channeling flag
/// (`01 10 00`) to a player without a house.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerHousingState {
    /// Child 0 `hasPurchasedHouse`.
    pub has_purchased_house: Option<bool>,
    /// Child 1 `ownedHouses`.
    pub owned_houses: Option<FullList<OwnedHouse>>,
    /// Child 2 `lastHouseRequestResponse` (0 in every retail body).
    pub last_house_request_response: Option<u8>,
    /// Child 3 `m_isWithinAPlot`.
    pub is_within_a_plot: Option<bool>,
    /// Child 4 `m_isFastTravelChanneling`.
    pub is_fast_travel_channeling: Option<bool>,
    /// Child 5 `m_phasedHousingPlotEntityId` ([`NO_PHASED_ENTITY`] for none).
    pub phased_plot: Option<u64>,
    /// Child 6 `m_phasedHouseDataEntityId` ([`NO_PHASED_ENTITY`] for none).
    pub phased_house_data: Option<u64>,
    /// Child 7 `m_phasedHouseOwnerCharacterId`: the owner's character id as UUID text.
    pub phased_owner: Option<Vec<u8>>,
    /// Child 8 `m_debugDataString`.
    pub debug_data: Option<Vec<u8>>,
}

impl PlayerHousingState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, a list is a delta, or a flag is not 0
    /// or 1.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        if read_slot_byte(reader, HOUSING_FIELD, 1)? == 0 {
            return Ok(state);
        }
        read_wide_children(
            reader,
            HOUSING_FIELD,
            PLAYER_HOUSING_CHILDREN,
            |reader, child| {
                match child {
                    0 => state.has_purchased_house = Some(reader.boolean()?),
                    1 => state.owned_houses = Some(FullList::read(reader, OwnedHouse::read)?),
                    2 => state.last_house_request_response = Some(reader.u8()?),
                    3 => state.is_within_a_plot = Some(reader.boolean()?),
                    4 => state.is_fast_travel_channeling = Some(reader.boolean()?),
                    5 => state.phased_plot = Some(reader.u64()?),
                    6 => state.phased_house_data = Some(reader.u64()?),
                    7 => state.phased_owner = Some(read_text(reader)?),
                    _ => state.debug_data = Some(read_text(reader)?),
                }
                Ok(())
            },
        )?;
        Ok(state)
    }

    /// Writes the state for the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a list cannot be written.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let present = [
            self.has_purchased_house.is_some(),
            self.owned_houses.is_some(),
            self.last_house_request_response.is_some(),
            self.is_within_a_plot.is_some(),
            self.is_fast_travel_channeling.is_some(),
            self.phased_plot.is_some(),
            self.phased_house_data.is_some(),
            self.phased_owner.is_some(),
            self.debug_data.is_some(),
        ];
        write_slot_0(
            writer,
            children_mask(&present),
            PLAYER_HOUSING_CHILDREN,
            |writer, child| {
                match child {
                    0 => {
                        self.has_purchased_house.map(|flag| writer.boolean(flag));
                    }
                    1 => {
                        if let Some(houses) = &self.owned_houses {
                            houses.write(writer, |writer, house| {
                                house.write(writer);
                                Ok(())
                            })?;
                        }
                    }
                    2 => {
                        self.last_house_request_response
                            .map(|response| writer.u8(response));
                    }
                    3 => {
                        self.is_within_a_plot.map(|flag| writer.boolean(flag));
                    }
                    4 => {
                        self.is_fast_travel_channeling
                            .map(|flag| writer.boolean(flag));
                    }
                    5 => {
                        self.phased_plot.map(|id| writer.u64(id));
                    }
                    6 => {
                        self.phased_house_data.map(|id| writer.u64(id));
                    }
                    7 => {
                        if let Some(owner) = &self.phased_owner {
                            write_text(writer, owner);
                        }
                    }
                    _ => {
                        if let Some(debug) = &self.debug_data {
                            write_text(writer, debug);
                        }
                    }
                }
                Ok(())
            },
        )
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a list cannot be written.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.write(&mut writer)?;
        Ok(writer.finish())
    }
}

/// The kind of a home point (the `u32` of each retail element: 4 forts, 5 houses, 6 inns,
/// 7 settlement respawns, 9 camps, 10 dungeon and shrine respawns, 11 spirit shrines,
/// 12 permanent camps; the client enum's name is Unknown).
pub mod home_point_kind {
    /// A house (`House`, no cooldown).
    pub const HOUSE: u32 = 5;
    /// An inn (`@NN_InnName`, a 900 s cooldown in 449 of 449 retail elements).
    pub const INN: u32 = 6;
    /// A town or outpost the player has been in (`@Respawn_OutpostName`: 22 of 22 retail
    /// points). The world map asks for a point of this kind in a territory first, then for a
    /// spirit shrine (`FUN_143589fe0`).
    pub const TOWN: u32 = 7;
    /// A player's camp (`@CampTn_MasterName`, a 1 s cooldown).
    pub const CAMP: u32 = 9;
    /// A charted spirit shrine (`@ui_spirit_shrine`, no cooldown: 131 of 131 retail points) or
    /// crow's nest (no name: 3 of 3). The world map counts a shrine charted when the player has
    /// a point of this kind in its territory (`HasFastTravelPointInTerritory`,
    /// `FUN_143589fe0`).
    pub const SPIRIT_SHRINE: u32 = 11;
}

/// One home point of the player (`FUN_1434fe010`, `FUN_1434dc760`): a place it may respawn at.
#[derive(Debug, Clone, PartialEq)]
pub struct HomePoint {
    /// The point's entity.
    pub entity: PlotRef,
    /// Its display name.
    pub name: Vec<u8>,
    /// Its position, metres.
    pub position: [f32; POSITION_LEN],
    /// Its cooldown, ns.
    pub cooldown_ns: u64,
    /// Unix ns: an inn's or a camp's last use (in the past in every retail element), a house's
    /// cooldown end (in the future); 0 for none.
    pub used_ns: u64,
    /// Its kind ([`home_point_kind`]).
    pub kind: u32,
    /// A flag (0 in every retail element).
    pub flag: bool,
    /// A byte (0 in every retail element).
    pub extra: u8,
    /// Its id text: `[<entity id>]<index>`, the id the home point state names.
    pub id: Vec<u8>,
    /// The index the id ends with.
    pub index: u32,
}

impl HomePoint {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            entity: PlotRef::read(reader)?,
            name: read_text(reader)?,
            position: read_position(reader)?,
            cooldown_ns: reader.u64()?,
            used_ns: reader.u64()?,
            kind: reader.u32()?,
            flag: reader.boolean()?,
            extra: reader.u8()?,
            id: read_text(reader)?,
            index: reader.u32()?,
        })
    }

    fn write(&self, writer: &mut Writer) {
        self.entity.write(writer);
        write_text(writer, &self.name);
        write_position(writer, &self.position);
        writer
            .u64(self.cooldown_ns)
            .u64(self.used_ns)
            .u32(self.kind)
            .boolean(self.flag)
            .u8(self.extra);
        write_text(writer, &self.id);
        writer.u32(self.index);
    }
}

/// The id text of the home point of entity `entity` with index `index` (retail
/// `[17680737194352937251]0` for an inn).
pub fn home_point_id(entity: u64, index: u32) -> Vec<u8> {
    format!("[{entity}]{index}").into_bytes()
}

/// `MB::PlayerHomeComponentReplicatedState` (`FUN_1434e3e80`): the player's home points and the
/// id of the one it is bound to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerHomeState {
    /// Child 0 `homePointList`.
    pub home_points: Option<FullList<HomePoint>>,
    /// Child 1 `homePointId`: the bound home point's id text (146 of 477 retail bodies).
    pub home_point_id: Option<Vec<u8>>,
}

impl PlayerHomeState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early or the list is a delta.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        read_slot_0(reader, HOME_FIELD, PLAYER_HOME_CHILDREN, |reader, child| {
            match child {
                0 => state.home_points = Some(FullList::read(reader, HomePoint::read)?),
                _ => state.home_point_id = Some(read_text(reader)?),
            }
            Ok(())
        })?;
        Ok(state)
    }

    /// Writes the state for the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the list cannot be written.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let present = [self.home_points.is_some(), self.home_point_id.is_some()];
        write_slot_0(
            writer,
            children_mask(&present),
            PLAYER_HOME_CHILDREN,
            |writer, child| {
                match (child, &self.home_points, &self.home_point_id) {
                    (0, Some(points), _) => points.write(writer, |writer, point| {
                        point.write(writer);
                        Ok(())
                    })?,
                    (_, _, Some(id)) => write_text(writer, id),
                    _ => {}
                }
                Ok(())
            },
        )
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the list cannot be written.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.write(&mut writer)?;
        Ok(writer.finish())
    }
}

/// The camp entity of the camping state (`FUN_1415ad8a0`: a raw id and its facet entity).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CampRef {
    /// The raw id.
    pub id: [u8; RAW_ID_LEN],
    /// Its facet entity.
    pub facet: u64,
}

/// `MB::CampingComponentReplicatedState` (`FUN_1440c7c20`). Retail: `01 0a <skin> <end>` for a
/// player without a camp (355 of 410 first bodies), `01 0b`/`01 0f` with the camp.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CampingState {
    /// Child 0 `currentCampRef`.
    pub current_camp: Option<CampRef>,
    /// Child 1 `currentCampSkinId`: the CRC-32 of the lowercase `CampSkinID`.
    pub camp_skin: Option<u32>,
    /// Child 2 `isCampUnderAttack`.
    pub under_attack: Option<bool>,
    /// Child 3 `campBindCooldownEndTime`, Unix ns.
    pub bind_cooldown_end_ns: Option<u64>,
}

impl CampingState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early or a flag is not 0 or 1.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        read_slot_0(reader, CAMPING_FIELD, CAMPING_CHILDREN, |reader, child| {
            match child {
                0 => {
                    state.current_camp = Some(CampRef {
                        id: reader.raw_id()?,
                        facet: reader.u64()?,
                    });
                }
                1 => state.camp_skin = Some(reader.u32()?),
                2 => state.under_attack = Some(reader.boolean()?),
                _ => state.bind_cooldown_end_ns = Some(reader.u64()?),
            }
            Ok(())
        })?;
        Ok(state)
    }

    /// Writes the state for the bundle path.
    pub fn write(&self, writer: &mut Writer) {
        let present = [
            self.current_camp.is_some(),
            self.camp_skin.is_some(),
            self.under_attack.is_some(),
            self.bind_cooldown_end_ns.is_some(),
        ];
        write_known(
            writer,
            children_mask(&present),
            CAMPING_CHILDREN,
            |writer, child| {
                match child {
                    0 => self
                        .current_camp
                        .map(|camp| writer.bytes(&camp.id).u64(camp.facet)),
                    1 => self.camp_skin.map(|skin| writer.u32(skin)),
                    2 => self.under_attack.map(|flag| writer.boolean(flag)),
                    _ => self.bind_cooldown_end_ns.map(|end| writer.u64(end)),
                };
            },
        );
    }

    /// The body bytes.
    pub fn body(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        self.write(&mut writer);
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::FIRST_VERSION;

    /// A retail new character's player time (newchar 2026-09-12 shape): its card time in ns
    /// since 2000 and a played time of 448.2 s, paid the same.
    const NEW_CHARACTER_TIME: [u8; 26] = [
        0x01, 0x07, 0x0b, 0xbd, 0xa2, 0xd4, 0x37, 0x49, 0x1c, 0x7d, 0x00, 0x00, 0x00, 0x68, 0x54,
        0x69, 0xbd, 0x3e, 0x00, 0x00, 0x00, 0x68, 0x54, 0x69, 0xbd, 0x3e,
    ];
    const NEW_CHARACTER_START: u64 = 0x0bbd_a2d4_3749_1c7d;
    const NEW_CHARACTER_PLAYED_NS: u64 = 0x68_5469_bd3e;
    /// The retail housing state of a player without a house: only the channeling flag, false.
    const NO_HOUSE: [u8; 3] = [0x01, 0x10, 0x00];
    /// The retail camping state of a player without a camp: a skin and a bind cooldown end.
    const NO_CAMP: [u8; 14] = [
        0x01, 0x0a, 0xbc, 0x9a, 0x0d, 0xac, 0x18, 0xb8, 0xb0, 0x6d, 0x69, 0x31, 0x68, 0x18,
    ];
    /// `campskin_metalfest02_a`.
    const METALFEST_SKIN: u32 = 0xbc9a_0dac;
    const BIND_END_NS: u64 = 0x18b8_b06d_6931_6818;
    /// The retail home state of a new character: an empty list at version 3.
    const NEW_CHARACTER_HOME: [u8; 6] = [0x01, 0x01, 0x00, 0x01, 0x03, 0x00];
    /// `housetypetier2`.
    const TIER_2: u32 = 0xd0e4_0188;
    /// A synthetic level id and its plot reference (retail form).
    const PLOT_ID: u64 = 0x6a41_43a0_cd66_37c7;
    const INN_ENTITY: u64 = 17_680_737_194_352_937_251;
    const INN_COOLDOWN_NS: u64 = 900_000_000_000;

    fn plot() -> PlotRef {
        let mut id = [0; RAW_ID_LEN];
        id[..8].copy_from_slice(&PLOT_ID.to_le_bytes());
        PlotRef {
            id,
            facet: PLOT_ID,
            extra: 0x99ed_e7e1_d5a5_b24d,
        }
    }

    fn read<T>(bytes: &[u8], read: fn(&mut Reader<'_>) -> Result<T, MessageError>) -> T {
        let mut reader = Reader::new(bytes);
        let state = read(&mut reader).expect("reads");
        reader.finish().expect("whole body");
        state
    }

    #[test]
    fn the_player_time_is_the_retail_body() {
        let state = read(&NEW_CHARACTER_TIME, PlayerTimeState::read);
        assert_eq!(
            state,
            PlayerTimeState {
                start_time_point: Some(NEW_CHARACTER_START),
                duration_at_start_ns: Some(NEW_CHARACTER_PLAYED_NS),
                paid_duration_at_start_ns: Some(NEW_CHARACTER_PLAYED_NS),
            }
        );
        assert_eq!(state.body(), NEW_CHARACTER_TIME);
    }

    #[test]
    fn the_housing_state_without_a_house_is_the_retail_body() {
        let state = PlayerHousingState {
            is_fast_travel_channeling: Some(false),
            ..PlayerHousingState::default()
        };
        assert_eq!(state.body().expect("body"), NO_HOUSE);
        assert_eq!(read(&NO_HOUSE, PlayerHousingState::read), state);
    }

    #[test]
    fn an_owned_house_and_the_phased_children_read_back() {
        let house = OwnedHouse {
            plot: plot(),
            house_data: HouseDataRef {
                id: [7; RAW_ID_LEN],
                facet: 0xc13e_b7f0_5396_3b4c,
            },
            position: [9_359.1, 2_782.1, 87.4],
            taxes_due_ns: 1_790_000_000_000_000_000,
            purchased_ns: 1_789_395_200_000_000_000,
            territory: 6,
            buff_items_active: true,
            trophies: vec![0xfce5_62b2],
            decoration_score: 2.87,
            flag: true,
            handshake: true,
            fast_travel_ready_ns: 0,
            house_type: TIER_2,
        };
        let state = PlayerHousingState {
            has_purchased_house: Some(true),
            owned_houses: Some(FullList::first(vec![house])),
            last_house_request_response: Some(0),
            is_within_a_plot: Some(false),
            is_fast_travel_channeling: Some(false),
            phased_plot: Some(NO_PHASED_ENTITY),
            phased_house_data: Some(NO_PHASED_ENTITY),
            phased_owner: Some(b"e584623a-9fb9-40ab-b3ae-7da8e4e83430".to_vec()),
            debug_data: None,
        };
        let body = state.body().expect("body");
        // Slot 0; children 0 to 6 and a continuation, `hasPurchasedHouse`, the list's full form.
        assert_eq!(&body[..4], &[SLOT_0, 0xff, 0x01, 0x00]);
        let tail = &body[body.len() - 37..];
        assert_eq!(tail[0], 36, "the owner text's length");
        assert_eq!(read(&body, PlayerHousingState::read), state);
    }

    #[test]
    fn the_home_points_read_back_and_a_new_character_has_none() {
        let empty = read(&NEW_CHARACTER_HOME, PlayerHomeState::read);
        assert_eq!(empty.home_points, Some(FullList::first(Vec::new())));
        assert_eq!(empty.body().expect("body"), NEW_CHARACTER_HOME);
        let inn = HomePoint {
            entity: plot(),
            name: b"@12_InnName".to_vec(),
            position: [7_383.5, 3_732.5, 112.7],
            cooldown_ns: INN_COOLDOWN_NS,
            used_ns: 0,
            kind: home_point_kind::INN,
            flag: false,
            extra: 0,
            id: home_point_id(INN_ENTITY, 0),
            index: 0,
        };
        let state = PlayerHomeState {
            home_points: Some(FullList {
                version: Some(FIRST_VERSION + 1),
                elements: vec![inn],
            }),
            home_point_id: Some(home_point_id(INN_ENTITY, 0)),
        };
        let body = state.body().expect("body");
        assert_eq!(&body[..2], &[SLOT_0, 0b11]);
        assert_eq!(read(&body, PlayerHomeState::read), state);
        assert_eq!(home_point_id(INN_ENTITY, 0), b"[17680737194352937251]0");
    }

    #[test]
    fn the_camping_state_is_the_retail_body_and_the_camp_reads_back() {
        let state = read(&NO_CAMP, CampingState::read);
        assert_eq!(
            state,
            CampingState {
                current_camp: None,
                camp_skin: Some(METALFEST_SKIN),
                under_attack: None,
                bind_cooldown_end_ns: Some(BIND_END_NS),
            }
        );
        assert_eq!(state.body(), NO_CAMP);
        let camped = CampingState {
            current_camp: Some(CampRef {
                id: [9; RAW_ID_LEN],
                facet: 0x7bd3_dba4_9249_2077,
            }),
            under_attack: Some(false),
            ..state
        };
        let body = camped.body();
        assert_eq!(&body[..2], &[SLOT_0, 0x0f]);
        assert_eq!(read(&body, CampingState::read), camped);
    }
}
