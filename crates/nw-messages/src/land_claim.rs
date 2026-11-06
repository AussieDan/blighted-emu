// SPDX-License-Identifier: AGPL-3.0-or-later
//! The territory claims: `MB::LandClaimManagerComponentReplicatedState` (type 3086), the state
//! of the land claim manager, one entry per claim in each list (the claim's index).
//!
//! Observed (retail ledgers `newchar-retail-2026-09-18`, `general-other-01`,
//! `travel-shrines-settlements-mount`): the server sends the full state as component
//! [`LAND_CLAIM_MANAGER_COMPONENT`] of the never-carded slot-2 entity, in the same first group
//! as the weekly mutations, and again every 30 s or so. All three hold the same 13 claims in the
//! same order ([`CLAIM_KEYS`]), the same positions and the same tax rates for every claim.
//!
//! Observed (client trace 2026-11-06, `fasttravelicon.luac` proto 9 at instruction 133): the
//! map's shrine and town flyout calls `LandClaimRequestBus.GetTerritoryGovernanceData` for the
//! territory under the icon and reads `failedToPayUpkeep` from the result; without this state
//! the result is nil, the script stops with "attempt to index a nil value", and the icon shows
//! no name. The flyout then reads the `faction` of `GetClaimOwnerData`.
//!
//! The body is the generic slot reader: one slot of 18 children (`crate::slots`), each list in
//! its full form ([`FullList`]). The names are the registered field names; the parts of a
//! governance entry are the `GovernanceTaxData` names (`FUN_142f93d80`).

use crate::items::FullList;
use crate::mutation_scheduler::MUTATION_SCHEDULER_INTEREST;
use crate::payload::{BundleGroup, BundleMember, MemberState};
use crate::slots::{read_slot_byte, read_wide_children, write_wide_children};
use crate::travel_states::{read_text, write_text};
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `MB::LandClaimManagerComponentReplicatedState`.
pub const LAND_CLAIM_MANAGER_TYPE_INDEX: u64 = 3086;
/// The component of the slot-2 entity that takes the state (Observed: component 2 in all three
/// ledgers).
pub const LAND_CLAIM_MANAGER_COMPONENT: u64 = 2;
/// The entity slot of the manager: the slot-2 entity of the weekly mutations (Observed: one
/// group holds both states).
pub const LAND_CLAIM_MANAGER_INTEREST: u64 = MUTATION_SCHEDULER_INTEREST;
/// The territory id of each claim, by claim index (Observed: the same in all three ledgers).
pub const CLAIM_KEYS: [u16; 13] = [10, 12, 6, 5, 17, 8, 2, 4, 13, 15, 16, 11, 14];

/// Children of the state's only slot.
const MANAGER_CHILDREN: u8 = 18;
/// Slots of the state.
const MANAGER_SLOTS: u8 = 1;
/// The slot byte with slot 0 set.
const SLOT_0: u8 = 0x01;
/// The slot byte with no slot set.
const NO_SLOT: u8 = 0;
/// The children, in registration order.
const FCP_LOCK_TIMERS: u8 = 0;
const FCP_LOCK_ANCHOR: u8 = 1;
const CLAIM_KEYS_CHILD: u8 = 2;
const CONFLICT_FACTION: u8 = 3;
const CONFLICT_LOTTERY_END_TIME: u8 = 4;
const CONFLICT_START_TIME: u8 = 5;
const DARKNESS_THRESHOLD: u8 = 6;
const DARKNESS_CYCLE_END_TIME: u8 = 7;
const FACTION_CONTROL_POINT_DATA: u8 = 8;
const FACTION_1_INFLUENCE: u8 = 9;
const FACTION_2_INFLUENCE: u8 = 10;
const FACTION_3_INFLUENCE: u8 = 11;
const GOVERNANCE: u8 = 12;
const INFLUENCE_RACE_START_TIME: u8 = 13;
const POS_DATA: u8 = 14;
const PROGRESSION: u8 = 15;
const OWNER_DATA: u8 = 16;
const WAR_DEC_THRESHOLD_MET_FACTION: u8 = 17;
/// Bytes of a guild id.
const GUILD_ID_LEN: usize = 16;
/// Components of a crest colour (red, green, blue, alpha).
const COLOR_COMPONENTS: usize = 4;
/// Coordinates of a claim position.
const POSITION_LEN: usize = 3;
/// The most entries of a progression array (a count beyond it is a misread body).
const MAX_PROGRESSION_ENTRIES: u64 = 1024;
/// The field name of the errors.
const FIELD: &str = "LandClaimManagerComponentReplicatedState";

/// One claim's taxes and fees (`GovernanceTaxData`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernanceTaxData {
    /// `m_houseFee`.
    pub house_fee: f32,
    /// `m_craftingFee`.
    pub crafting_fee: f32,
    /// `m_refiningFee`.
    pub refining_fee: f32,
    /// `m_tradingFee`.
    pub trading_fee: f32,
    /// `m_isValid`.
    pub is_valid: bool,
    /// `m_failedToPayUpkeep`.
    pub failed_to_pay_upkeep: bool,
}

impl GovernanceTaxData {
    fn write(&self, writer: &mut Writer) {
        writer
            .f32(self.house_fee)
            .f32(self.crafting_fee)
            .f32(self.refining_fee)
            .f32(self.trading_fee)
            .boolean(self.is_valid)
            .boolean(self.failed_to_pay_upkeep);
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            house_fee: reader.f32()?,
            crafting_fee: reader.f32()?,
            refining_fee: reader.f32()?,
            trading_fee: reader.f32()?,
            is_valid: reader.boolean()?,
            failed_to_pay_upkeep: reader.boolean()?,
        })
    }
}

/// One `(id, tier)` pair of a claim's progression (Observed: 19 to 21 per owned claim, ids 0 to
/// 21, tiers 0 to 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressionTier {
    /// The id.
    pub id: u32,
    /// The tier.
    pub tier: u8,
}

/// One `(id, state, value)` entry of a claim's progression (Observed: 5 per owned claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressionEntry {
    /// The id (a CRC-32 or 0).
    pub id: u32,
    /// The state byte.
    pub state: u8,
    /// The value.
    pub value: u32,
}

/// One claim's progression.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimProgression {
    /// The tiers.
    pub tiers: Vec<ProgressionTier>,
    /// The entries.
    pub entries: Vec<ProgressionEntry>,
    /// The last byte (Observed: 23 to 39 on owned claims).
    pub level: u8,
}

impl ClaimProgression {
    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        writer.prefixed(self.tiers.len() as u64)?;
        for tier in &self.tiers {
            writer.u32(tier.id).u8(tier.tier);
        }
        writer.prefixed(self.entries.len() as u64)?;
        for entry in &self.entries {
            writer.u32(entry.id).u8(entry.state).u32(entry.value);
        }
        writer.u8(self.level);
        Ok(())
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let tiers = (0..progression_count(reader)?)
            .map(|_| {
                Ok(ProgressionTier {
                    id: reader.u32()?,
                    tier: reader.u8()?,
                })
            })
            .collect::<Result<_, MessageError>>()?;
        let entries = (0..progression_count(reader)?)
            .map(|_| {
                Ok(ProgressionEntry {
                    id: reader.u32()?,
                    state: reader.u8()?,
                    value: reader.u32()?,
                })
            })
            .collect::<Result<_, MessageError>>()?;
        Ok(Self {
            tiers,
            entries,
            level: reader.u8()?,
        })
    }
}

fn progression_count(reader: &mut Reader<'_>) -> Result<u64, MessageError> {
    let count = reader.prefixed()?;
    if count > MAX_PROGRESSION_ENTRIES {
        return Err(MessageError::CountTooLarge {
            count,
            limit: MAX_PROGRESSION_ENTRIES,
        });
    }
    Ok(count)
}

/// One claim's owner: the company, its crest and its faction (Observed on owned claims: a
/// company id, its name, two crest icons with their colours, an empty text, the faction 1 to 3
/// and a Unix nanosecond time).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaimOwner {
    /// The company id; all zero for no owner.
    pub guild_id: [u8; GUILD_ID_LEN],
    /// The company name.
    pub guild_name: Vec<u8>,
    /// The crest background icon.
    pub crest_background: u16,
    /// The crest background colour.
    pub crest_background_color: [f32; COLOR_COMPONENTS],
    /// The crest foreground icon.
    pub crest_foreground: u16,
    /// The crest foreground colour.
    pub crest_foreground_color: [f32; COLOR_COMPONENTS],
    /// The text after the crest (Observed: always empty).
    pub text: Vec<u8>,
    /// The owner's faction; 0 for none.
    pub faction: u8,
    /// The time, Unix nanoseconds.
    pub time_ns: u64,
}

impl ClaimOwner {
    fn write(&self, writer: &mut Writer) {
        writer.bytes(&self.guild_id);
        write_text(writer, &self.guild_name);
        writer.u16(self.crest_background);
        write_color(writer, &self.crest_background_color);
        writer.u16(self.crest_foreground);
        write_color(writer, &self.crest_foreground_color);
        write_text(writer, &self.text);
        writer.u8(self.faction).u64(self.time_ns);
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            guild_id: reader.array()?,
            guild_name: read_text(reader)?,
            crest_background: reader.u16()?,
            crest_background_color: read_color(reader)?,
            crest_foreground: reader.u16()?,
            crest_foreground_color: read_color(reader)?,
            text: read_text(reader)?,
            faction: reader.u8()?,
            time_ns: reader.u64()?,
        })
    }
}

fn write_color(writer: &mut Writer, color: &[f32; COLOR_COMPONENTS]) {
    for component in color {
        writer.f32(*component);
    }
}

fn read_color(reader: &mut Reader<'_>) -> Result<[f32; COLOR_COMPONENTS], MessageError> {
    Ok([reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?])
}

/// The manager state: `None` children are not in the body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LandClaimManagerState {
    /// `fcpLockTimers` (Observed: `0xffff` for a claim without a lock).
    pub fcp_lock_timers: Option<FullList<u16>>,
    /// `fcpLockAnchor`.
    pub fcp_lock_anchor: Option<u64>,
    /// `replicatedClaimKeys`: the territory id of each claim.
    pub claim_keys: Option<FullList<u16>>,
    /// `replicatedConflictFaction`.
    pub conflict_faction: Option<FullList<u8>>,
    /// `replicatedConflictLotteryEndTime`.
    pub conflict_lottery_end_time: Option<FullList<u64>>,
    /// `replicatedConflictStartTime`.
    pub conflict_start_time: Option<FullList<u64>>,
    /// `replicatedDarknessThreshold` (Observed: 0 to 3).
    pub darkness_threshold: Option<FullList<u32>>,
    /// `replicatedDarknessCycleEndTime`.
    pub darkness_cycle_end_time: Option<FullList<u64>>,
    /// `replicatedFactionControlPointData` (Observed: 29 bytes, not one per claim).
    pub faction_control_point_data: Option<FullList<u8>>,
    /// `replicatedFaction1InfluencePercentages` to `replicatedFaction3InfluencePercentages`.
    pub faction_influence: [Option<FullList<u8>>; 3],
    /// `replicatedGovernance`.
    pub governance: Option<FullList<GovernanceTaxData>>,
    /// `replicatedInfluenceRaceStartTime`.
    pub influence_race_start_time: Option<FullList<u64>>,
    /// `replicatedPosData`: each claim's position.
    pub positions: Option<FullList<[f32; POSITION_LEN]>>,
    /// `replicatedProgression`.
    pub progression: Option<FullList<ClaimProgression>>,
    /// `replicatedOwnerData`.
    pub owners: Option<FullList<ClaimOwner>>,
    /// `replicatedWarDecThresholdMetFaction`.
    pub war_dec_threshold_met_faction: Option<FullList<u8>>,
}

fn write_list<T>(
    writer: &mut Writer,
    list: Option<&FullList<T>>,
    mut write_element: impl FnMut(&mut Writer, &T) -> Result<(), MessageError>,
) -> Result<(), MessageError> {
    list.map_or(Ok(()), |list| {
        list.write(writer, |writer, element| write_element(writer, element))
    })
}

impl LandClaimManagerState {
    /// The child bitmap of the present children.
    fn mask(&self) -> u32 {
        let [faction_1, faction_2, faction_3] = &self.faction_influence;
        [
            (FCP_LOCK_TIMERS, self.fcp_lock_timers.is_some()),
            (FCP_LOCK_ANCHOR, self.fcp_lock_anchor.is_some()),
            (CLAIM_KEYS_CHILD, self.claim_keys.is_some()),
            (CONFLICT_FACTION, self.conflict_faction.is_some()),
            (
                CONFLICT_LOTTERY_END_TIME,
                self.conflict_lottery_end_time.is_some(),
            ),
            (CONFLICT_START_TIME, self.conflict_start_time.is_some()),
            (DARKNESS_THRESHOLD, self.darkness_threshold.is_some()),
            (
                DARKNESS_CYCLE_END_TIME,
                self.darkness_cycle_end_time.is_some(),
            ),
            (
                FACTION_CONTROL_POINT_DATA,
                self.faction_control_point_data.is_some(),
            ),
            (FACTION_1_INFLUENCE, faction_1.is_some()),
            (FACTION_2_INFLUENCE, faction_2.is_some()),
            (FACTION_3_INFLUENCE, faction_3.is_some()),
            (GOVERNANCE, self.governance.is_some()),
            (
                INFLUENCE_RACE_START_TIME,
                self.influence_race_start_time.is_some(),
            ),
            (POS_DATA, self.positions.is_some()),
            (PROGRESSION, self.progression.is_some()),
            (OWNER_DATA, self.owners.is_some()),
            (
                WAR_DEC_THRESHOLD_MET_FACTION,
                self.war_dec_threshold_met_faction.is_some(),
            ),
        ]
        .into_iter()
        .filter(|(_, present)| *present)
        .fold(0, |mask, (child, _)| mask | 1 << child)
    }

    /// Writes the body: the slot byte, then the children.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a list or array does not fit its prefix code.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let mask = self.mask();
        if mask == 0 {
            writer.u8(NO_SLOT);
            return Ok(());
        }
        writer.u8(SLOT_0);
        write_wide_children(writer, mask, MANAGER_CHILDREN, |writer, child| {
            let u8_list = |writer: &mut Writer, list: Option<&FullList<u8>>| {
                write_list(writer, list, |writer, value| {
                    writer.u8(*value);
                    Ok(())
                })
            };
            let u64_list = |writer: &mut Writer, list: Option<&FullList<u64>>| {
                write_list(writer, list, |writer, value| {
                    writer.u64(*value);
                    Ok(())
                })
            };
            let u16_list = |writer: &mut Writer, list: Option<&FullList<u16>>| {
                write_list(writer, list, |writer, value| {
                    writer.u16(*value);
                    Ok(())
                })
            };
            match child {
                FCP_LOCK_TIMERS => u16_list(writer, self.fcp_lock_timers.as_ref()),
                FCP_LOCK_ANCHOR => {
                    writer.u64(self.fcp_lock_anchor.unwrap_or_default());
                    Ok(())
                }
                CLAIM_KEYS_CHILD => u16_list(writer, self.claim_keys.as_ref()),
                CONFLICT_FACTION => u8_list(writer, self.conflict_faction.as_ref()),
                CONFLICT_LOTTERY_END_TIME => {
                    u64_list(writer, self.conflict_lottery_end_time.as_ref())
                }
                CONFLICT_START_TIME => u64_list(writer, self.conflict_start_time.as_ref()),
                DARKNESS_THRESHOLD => {
                    write_list(writer, self.darkness_threshold.as_ref(), |writer, value| {
                        writer.u32(*value);
                        Ok(())
                    })
                }
                DARKNESS_CYCLE_END_TIME => u64_list(writer, self.darkness_cycle_end_time.as_ref()),
                FACTION_CONTROL_POINT_DATA => {
                    u8_list(writer, self.faction_control_point_data.as_ref())
                }
                FACTION_1_INFLUENCE | FACTION_2_INFLUENCE | FACTION_3_INFLUENCE => u8_list(
                    writer,
                    self.faction_influence[usize::from(child - FACTION_1_INFLUENCE)].as_ref(),
                ),
                GOVERNANCE => write_list(writer, self.governance.as_ref(), |writer, entry| {
                    entry.write(writer);
                    Ok(())
                }),
                INFLUENCE_RACE_START_TIME => {
                    u64_list(writer, self.influence_race_start_time.as_ref())
                }
                POS_DATA => write_list(writer, self.positions.as_ref(), |writer, position| {
                    for coordinate in position {
                        writer.f32(*coordinate);
                    }
                    Ok(())
                }),
                PROGRESSION => write_list(writer, self.progression.as_ref(), |writer, entry| {
                    entry.write(writer)
                }),
                OWNER_DATA => write_list(writer, self.owners.as_ref(), |writer, owner| {
                    owner.write(writer);
                    Ok(())
                }),
                _ => u8_list(writer, self.war_dec_threshold_met_faction.as_ref()),
            }
        })
    }

    /// Reads a body (lists in their full form only).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body does not match the layout.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        if read_slot_byte(reader, FIELD, MANAGER_SLOTS)? == NO_SLOT {
            return Ok(state);
        }
        read_wide_children(reader, FIELD, MANAGER_CHILDREN, |reader, child| {
            let u8_list = |reader: &mut Reader<'_>| FullList::read(reader, |reader| reader.u8());
            let u16_list = |reader: &mut Reader<'_>| FullList::read(reader, |reader| reader.u16());
            let u64_list = |reader: &mut Reader<'_>| FullList::read(reader, |reader| reader.u64());
            match child {
                FCP_LOCK_TIMERS => state.fcp_lock_timers = Some(u16_list(reader)?),
                FCP_LOCK_ANCHOR => state.fcp_lock_anchor = Some(reader.u64()?),
                CLAIM_KEYS_CHILD => state.claim_keys = Some(u16_list(reader)?),
                CONFLICT_FACTION => state.conflict_faction = Some(u8_list(reader)?),
                CONFLICT_LOTTERY_END_TIME => {
                    state.conflict_lottery_end_time = Some(u64_list(reader)?);
                }
                CONFLICT_START_TIME => state.conflict_start_time = Some(u64_list(reader)?),
                DARKNESS_THRESHOLD => {
                    state.darkness_threshold = Some(FullList::read(reader, |reader| reader.u32())?);
                }
                DARKNESS_CYCLE_END_TIME => state.darkness_cycle_end_time = Some(u64_list(reader)?),
                FACTION_CONTROL_POINT_DATA => {
                    state.faction_control_point_data = Some(u8_list(reader)?);
                }
                FACTION_1_INFLUENCE | FACTION_2_INFLUENCE | FACTION_3_INFLUENCE => {
                    state.faction_influence[usize::from(child - FACTION_1_INFLUENCE)] =
                        Some(u8_list(reader)?);
                }
                GOVERNANCE => {
                    state.governance = Some(FullList::read(reader, GovernanceTaxData::read)?);
                }
                INFLUENCE_RACE_START_TIME => {
                    state.influence_race_start_time = Some(u64_list(reader)?);
                }
                POS_DATA => {
                    state.positions = Some(FullList::read(reader, |reader| {
                        Ok([reader.f32()?, reader.f32()?, reader.f32()?])
                    })?);
                }
                PROGRESSION => {
                    state.progression = Some(FullList::read(reader, ClaimProgression::read)?);
                }
                OWNER_DATA => state.owners = Some(FullList::read(reader, ClaimOwner::read)?),
                _ => state.war_dec_threshold_met_faction = Some(u8_list(reader)?),
            }
            Ok(())
        })?;
        Ok(state)
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a list or array does not fit its prefix code.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.write(&mut writer)?;
        Ok(writer.finish())
    }

    /// The state of the body `body`, which it must use up.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body does not match the layout.
    pub fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let state = Self::read(&mut reader)?;
        reader.finish()?;
        Ok(state)
    }

    /// The bundle member with this state (component [`LAND_CLAIM_MANAGER_COMPONENT`]).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the state cannot be encoded.
    pub fn member(&self) -> Result<BundleMember, MessageError> {
        Ok(BundleMember {
            component: LAND_CLAIM_MANAGER_COMPONENT,
            state: MemberState::Recorded {
                class: LAND_CLAIM_MANAGER_TYPE_INDEX,
                body: self.encode()?,
            },
        })
    }

    /// The bundle group of the slot-2 entity (slot [`LAND_CLAIM_MANAGER_INTEREST`]) with this
    /// state as its only member.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the state cannot be encoded.
    pub fn group(&self) -> Result<BundleGroup, MessageError> {
        Ok(BundleGroup {
            interest: LAND_CLAIM_MANAGER_INTEREST,
            members: vec![self.member()?],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::member_layouts::{layout, skip_body};

    /// The retail governance entry of every claim: house fee 0.2, crafting and refining fees
    /// 0.5, trading fee 1.0, valid, upkeep paid (Observed: all 13 claims in all three ledgers).
    const RETAIL_GOVERNANCE: &str = "3e4ccccd3f0000003f0000003f8000000100";
    /// The retail first claim's position (territory 10).
    const FIRST_POSITION: [f32; POSITION_LEN] = [6851.21, 1330.6776, 62.924_44];

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn full_state() -> LandClaimManagerState {
        let claims = CLAIM_KEYS.len();
        let list = |value| FullList::first(vec![value; claims]);
        LandClaimManagerState {
            fcp_lock_timers: Some(FullList::first(vec![u16::MAX; claims])),
            fcp_lock_anchor: Some(0),
            claim_keys: Some(FullList::first(CLAIM_KEYS.to_vec())),
            conflict_faction: Some(list(0)),
            conflict_lottery_end_time: Some(FullList::first(vec![0; claims])),
            conflict_start_time: Some(FullList::first(vec![0; claims])),
            darkness_threshold: Some(FullList::first(vec![0; claims])),
            darkness_cycle_end_time: Some(FullList::first(vec![0; claims])),
            faction_control_point_data: Some(list(0)),
            faction_influence: [Some(list(0)), Some(list(0)), Some(list(0))],
            governance: Some(FullList::first(vec![
                GovernanceTaxData::read(
                    &mut Reader::new(&bytes(RETAIL_GOVERNANCE))
                )
                .expect("governance");
                claims
            ])),
            influence_race_start_time: Some(FullList::first(vec![0; claims])),
            positions: Some(FullList::first(vec![FIRST_POSITION; claims])),
            progression: Some(FullList::first(vec![
                ClaimProgression {
                    tiers: vec![ProgressionTier { id: 11, tier: 2 }],
                    entries: vec![ProgressionEntry {
                        id: 9_437_075,
                        state: 1,
                        value: 0,
                    }],
                    level: 28,
                };
                claims
            ])),
            owners: Some(FullList::first(vec![
                ClaimOwner {
                    guild_name: b"Company".to_vec(),
                    faction: 2,
                    ..ClaimOwner::default()
                };
                claims
            ])),
            war_dec_threshold_met_faction: Some(list(0)),
        }
    }

    #[test]
    fn the_retail_governance_entry_reads_and_writes_back_the_same_bytes() {
        let body = bytes(RETAIL_GOVERNANCE);
        let entry = GovernanceTaxData::read(&mut Reader::new(&body)).expect("read");
        assert_eq!(entry.house_fee, 0.2);
        assert_eq!(entry.crafting_fee, 0.5);
        assert_eq!(entry.refining_fee, 0.5);
        assert_eq!(entry.trading_fee, 1.0);
        assert!(entry.is_valid);
        assert!(!entry.failed_to_pay_upkeep);
        let mut writer = Writer::new();
        entry.write(&mut writer);
        assert_eq!(writer.finish(), body);
    }

    #[test]
    fn a_full_state_reads_back_and_the_client_layout_reads_it_to_its_end() {
        let state = full_state();
        let body = state.encode().expect("encode");
        assert_eq!(LandClaimManagerState::decode(&body), Ok(state));
        let layout = layout(LAND_CLAIM_MANAGER_TYPE_INDEX).expect("layout");
        assert_eq!(layout.name, "MB::LandClaimManagerComponentReplicatedState");
        assert_eq!(layout.slots[0].len(), usize::from(MANAGER_CHILDREN));
        let mut reader = Reader::new(&body);
        skip_body(layout, &mut reader).expect("walk");
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn the_children_follow_the_registration_order() {
        let layout = layout(LAND_CLAIM_MANAGER_TYPE_INDEX).expect("layout");
        let names: Vec<&str> = layout.slots[0].iter().map(|child| child.name).collect();
        assert_eq!(names[usize::from(CLAIM_KEYS_CHILD)], "replicatedClaimKeys");
        assert_eq!(names[usize::from(GOVERNANCE)], "replicatedGovernance");
        assert_eq!(names[usize::from(POS_DATA)], "replicatedPosData");
        assert_eq!(names[usize::from(OWNER_DATA)], "replicatedOwnerData");
        assert_eq!(
            names[usize::from(WAR_DEC_THRESHOLD_MET_FACTION)],
            "replicatedWarDecThresholdMetFaction"
        );
    }

    #[test]
    fn the_member_is_component_two_of_slot_two() {
        let group = full_state().group().expect("group");
        assert_eq!(group.interest, LAND_CLAIM_MANAGER_INTEREST);
        assert_eq!(group.members.len(), 1);
        assert_eq!(group.members[0].component, LAND_CLAIM_MANAGER_COMPONENT);
        assert_eq!(
            group.members[0].state.type_index(),
            LAND_CLAIM_MANAGER_TYPE_INDEX
        );
    }

    #[test]
    fn an_empty_state_is_the_bare_slot_byte() {
        let state = LandClaimManagerState::default();
        assert_eq!(state.encode().expect("encode"), [NO_SLOT]);
        assert_eq!(LandClaimManagerState::decode(&[NO_SLOT]), Ok(state));
    }
}
