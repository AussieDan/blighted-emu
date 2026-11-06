// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic invites: one player asks another to an activity of `GenericInvites`
//! (`javelindata_genericinvites`: duel, trade, musical performance, shard), the other accepts or
//! declines (economy note `local-data/scratch-work/run15/economy/REPORT.md`, section 4).
//!
//! | Call | Answer |
//! |---|---|
//! | 3404 `PlayerGenericInviteComponentServerFacet_RequestSendNewInvite` (activity, forward type, target character) | the invite in the state of both players, or 2454 |
//! | 3406 `PlayerGenericInviteComponentServerFacet_RequestReplyToInvite` (accept) | the activity (a trade: 2652 to both), or 2454 |
//!
//! 2454 `PlayerGenericInviteComponentClientFacet_OnInviteFailed` carries the activity and a
//! reason. The invite itself is `Javelin::PlayerGenericInviteReplicatedState` (type 3408,
//! own player component 58): slot 0 `m_inviteId`; slot 1 `m_activityCrc`, `m_forwardType`,
//! `m_inviteParticipants` and `m_expiryTimePoint` (`FUN_1433a9ab0`). 214 states of four
//! `trading_between_players` captures read with this layout; retail sends slot 1 whole.

use crate::combat::{read_string, write_string};
use crate::facet::FacetHeader;
use crate::p2p_trade::PLAYER_REF_KIND;
use crate::slots::{read_children, read_slot_byte, write_children, write_known_children};
use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// `PlayerGenericInviteComponentServerFacet_RequestSendNewInvite`.
pub const SEND_NEW_INVITE_TYPE_INDEX: u64 = 3404;
/// `PlayerGenericInviteComponentServerFacet_RequestReplyToInvite`.
pub const REPLY_TO_INVITE_TYPE_INDEX: u64 = 3406;
/// `PlayerGenericInviteComponentClientFacet_OnInviteFailed`.
pub const INVITE_FAILED_TYPE_INDEX: u64 = 2454;
/// `Javelin::PlayerGenericInviteReplicatedState`.
pub const GENERIC_INVITE_STATE_TYPE_INDEX: u64 = 3408;
/// The player's `PlayerGenericInviteComponent` (Observed: header target minus entity of every
/// retail 3404, 3406 and 2454).
pub const GENERIC_INVITE_COMPONENT: u64 = 0xe205_60b9_53cf_8a8c;
/// The own player component of the invite state (Observed: member key 58).
pub const GENERIC_INVITE_STATE_KEY: u64 = 58;
/// The platform `u32` of a participant without platform data (as `PlayerDataView`).
pub const NO_PLATFORM: u32 = u32::MAX;

/// The reasons of 2454: `eGenericInviteEnd` in client order (`FUN_1433be9f0`; the values are
/// Inferred from the order). Retail sent 1 to an accept that failed, 6 to an inviter about one
/// second after its invite, and 5 once (`trading_between_players_2` and `_3`).
pub mod end {
    /// `eGenericInviteEnd_Success`.
    pub const SUCCESS: u32 = 0;
    /// `eGenericInviteEnd_Eligibility_Failed`.
    pub const ELIGIBILITY_FAILED: u32 = 1;
    /// `eGenericInviteEnd_System_Error`.
    pub const SYSTEM_ERROR: u32 = 2;
    /// `eGenericInviteEnd_Cancelled`.
    pub const CANCELLED: u32 = 3;
    /// `eGenericInviteEnd_Participant_Blocked`.
    pub const PARTICIPANT_BLOCKED: u32 = 4;
    /// `eGenericInviteEnd_Participant_FastTravel_Channeling`.
    pub const PARTICIPANT_FAST_TRAVEL_CHANNELING: u32 = 5;
    /// `eGenericInviteEnd_Insufficient_Participants`.
    pub const INSUFFICIENT_PARTICIPANTS: u32 = 6;
}

const FIELD: &str = "generic invite";
/// The invite state has two slots.
const SLOTS: u8 = 2;
const SLOT_0: u8 = 0b01;
const SLOT_1: u8 = 0b10;
/// Slot 0 has `m_inviteId`.
const SLOT_0_CHILDREN: u8 = 1;
const INVITE_ID_CHILD: u16 = 1;
/// Slot 1 has the four other fields; retail sends all four.
const SLOT_1_CHILDREN: u8 = 4;
const ALL_DETAILS: u16 = 0b1111;
/// Most participants a state carries (retail: one, the inviter).
const MAX_PARTICIPANTS: u64 = 64;

/// One generic invite call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteCall {
    /// 3404: invite a character.
    Send {
        /// The header (the player's `PlayerGenericInviteComponent`).
        header: FacetHeader,
        /// The CRC-32 of the lowercase activity (`trade` for a trade).
        activity: u32,
        /// `forwardType` (retail 1).
        forward: u8,
        /// The invited character: its id as dashed lowercase UUID text.
        target: Vec<u8>,
    },
    /// 3406: answer the invite.
    Reply {
        /// The header.
        header: FacetHeader,
        /// Whether it is accepted.
        accept: bool,
    },
}

impl InviteCall {
    /// Decodes `body` when `type_index` is an invite call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            let header = FacetHeader::read(reader)?;
            Ok(match type_index {
                SEND_NEW_INVITE_TYPE_INDEX => Self::Send {
                    header,
                    activity: reader.u32()?,
                    forward: reader.u8()?,
                    target: read_string(reader)?,
                },
                _ => Self::Reply {
                    header,
                    accept: reader.boolean()?,
                },
            })
        };
        match type_index {
            SEND_NEW_INVITE_TYPE_INDEX | REPLY_TO_INVITE_TYPE_INDEX => {
                let mut reader = Reader::new(body);
                Some(read(&mut reader).and_then(|call| {
                    reader.finish()?;
                    Ok(call)
                }))
            }
            _ => None,
        }
    }

    /// The header of the call.
    pub const fn header(&self) -> FacetHeader {
        match self {
            Self::Send { header, .. } | Self::Reply { header, .. } => *header,
        }
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Send { .. } => SEND_NEW_INVITE_TYPE_INDEX,
            Self::Reply { .. } => REPLY_TO_INVITE_TYPE_INDEX,
        }
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] for a target text that is too long.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        match self {
            Self::Send {
                header,
                activity,
                forward,
                target,
            } => {
                header.write(&mut writer);
                writer.u32(*activity).u8(*forward);
                write_string(&mut writer, target)?;
            }
            Self::Reply { header, accept } => {
                header.write(&mut writer);
                writer.boolean(*accept);
            }
        }
        Ok(writer.finish())
    }
}

/// 2454: an invite failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteFailedMsg {
    /// The header (the player's `PlayerGenericInviteComponent`).
    pub header: FacetHeader,
    /// The CRC-32 of the lowercase activity.
    pub activity: u32,
    /// The reason ([`end`]).
    pub reason: u32,
}

impl InviteFailedMsg {
    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer.u32(self.activity).u32(self.reason);
        writer.finish()
    }

    /// Decodes `body` when `type_index` is 2454; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        if type_index != INVITE_FAILED_TYPE_INDEX {
            return None;
        }
        let mut reader = Reader::new(body);
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            Ok(Self {
                header: FacetHeader::read(reader)?,
                activity: reader.u32()?,
                reason: reader.u32()?,
            })
        };
        Some(read(&mut reader).and_then(|message| {
            reader.finish()?;
            Ok(message)
        }))
    }
}

/// One participant of an invite: the inviter on both players' states (retail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteParticipant {
    /// The character id.
    pub character: [u8; RAW_ID_LEN],
    /// The platform (retail 1 with a 17-digit platform id; [`NO_PLATFORM`] without one).
    pub platform: u32,
    /// The platform id text.
    pub platform_id: Vec<u8>,
}

/// `m_expiryTimePoint`: ten bytes, a `u16` (0 in every retail state, meaning Unknown) and the
/// time in nanoseconds since 2000-01-01 (retail: the invite plus `DurationSec`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InviteExpiry {
    /// The leading `u16`.
    pub word: u16,
    /// Nanoseconds since 2000-01-01; 0 for none.
    pub since_2000_ns: u64,
}

/// Slot 1 of the invite state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InviteDetails {
    /// `m_activityCrc`; 0 for none.
    pub activity: u32,
    /// `m_forwardType`.
    pub forward: u8,
    /// `m_inviteParticipants`.
    pub participants: Vec<InviteParticipant>,
    /// `m_expiryTimePoint`.
    pub expiry: InviteExpiry,
}

/// `Javelin::PlayerGenericInviteReplicatedState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenericInviteState {
    /// `m_inviteId`; all zero for none.
    pub invite_id: Option<[u8; RAW_ID_LEN]>,
    /// The other fields.
    pub details: Option<InviteDetails>,
}

impl GenericInviteState {
    /// The state retail sends when an invite ends: every field zero.
    pub fn cleared() -> Self {
        Self {
            invite_id: Some([0; RAW_ID_LEN]),
            details: Some(InviteDetails::default()),
        }
    }

    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, a slot or child is not one retail
    /// sends, or a list is too long.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slots = read_slot_byte(reader, FIELD, SLOTS)?;
        let mut state = Self::default();
        if slots & SLOT_0 != 0 {
            read_children(reader, FIELD, SLOT_0_CHILDREN, |reader, _| {
                state.invite_id = Some(reader.raw_id()?);
                Ok(())
            })?;
        }
        if slots & SLOT_1 != 0 {
            let mut details = InviteDetails::default();
            let children = read_children(reader, FIELD, SLOT_1_CHILDREN, |reader, child| {
                match child {
                    0 => details.activity = reader.u32()?,
                    1 => details.forward = reader.u8()?,
                    2 => details.participants = read_participants(reader)?,
                    _ => {
                        details.expiry = InviteExpiry {
                            word: reader.u16()?,
                            since_2000_ns: reader.u64()?,
                        };
                    }
                }
                Ok(())
            })?;
            if children != ALL_DETAILS {
                return Err(MessageError::Unsized {
                    field: FIELD,
                    // A slot 1 without every field: no retail state has one.
                    byte: u8::try_from(children).unwrap_or(u8::MAX),
                });
            }
            state.details = Some(details);
        }
        Ok(state)
    }

    /// Writes the state for the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] for a platform id that is too long.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let slots = if self.invite_id.is_some() { SLOT_0 } else { 0 }
            | if self.details.is_some() { SLOT_1 } else { 0 };
        writer.u8(slots);
        if let Some(id) = self.invite_id {
            write_known_children(writer, INVITE_ID_CHILD, SLOT_0_CHILDREN, |writer, _| {
                writer.bytes(&id);
            });
        }
        let Some(details) = &self.details else {
            return Ok(());
        };
        write_children(writer, ALL_DETAILS, SLOT_1_CHILDREN, |writer, child| {
            match child {
                0 => {
                    writer.u32(details.activity);
                }
                1 => {
                    writer.u8(details.forward);
                }
                2 => {
                    // A `usize` count fits 64 bits.
                    writer.wide_prefixed(details.participants.len() as u64);
                    for participant in &details.participants {
                        writer
                            .u8(PLAYER_REF_KIND)
                            .bytes(&participant.character)
                            .u32(participant.platform);
                        write_string(writer, &participant.platform_id)?;
                    }
                }
                _ => {
                    writer
                        .u16(details.expiry.word)
                        .u64(details.expiry.since_2000_ns);
                }
            }
            Ok(())
        })
    }
}

fn read_participants(reader: &mut Reader<'_>) -> Result<Vec<InviteParticipant>, MessageError> {
    let count = reader.wide_prefixed()?;
    if count > MAX_PARTICIPANTS {
        return Err(MessageError::CountTooLarge {
            count,
            limit: MAX_PARTICIPANTS,
        });
    }
    (0..count)
        .map(|_| {
            let kind = reader.u8()?;
            if kind != PLAYER_REF_KIND {
                return Err(MessageError::Unsized {
                    field: FIELD,
                    byte: kind,
                });
            }
            Ok(InviteParticipant {
                character: reader.raw_id()?,
                platform: reader.u32()?,
                platform_id: read_string(reader)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hex_bytes;

    /// The retail forms with made-up ids: a trade invite (3404: `trade`, forward type 1, the
    /// target as UUID text), an accept (3406) and a failure (2454, reason 6).
    const SEND: &str = "0a9c297a95dfb3fa2896c8c14210296e7e1a43660124\
        31313131313131312d323232322d333333332d343434342d353535353535353535353535";
    const ACCEPT: &str = "0a9c297a95dfb3fa2896c8c14210296e01";
    const FAILED: &str = "0a9c297a95dfb3fa2896c8c14210296e7e1a436600000006";
    /// A retail-form invite state (slot 0 and 1, one participant with a 17-digit platform id,
    /// the expiry ten seconds after the invite) and the cleared state, both with made-up ids.
    const INVITE_STATE: &str = "030111111111111111111111111111111111\
        0f7e1a4366010105222222222222222222222222222222220000000111\
        3730303030303030303030303030303031\
        00000b96fdb35049d83a";
    const CLEARED_STATE: &str = "030100000000000000000000000000000000\
        0f00000000000000000000000000000000";

    #[test]
    fn retail_calls_read_and_write_back() {
        for (type_index, hex) in [
            (SEND_NEW_INVITE_TYPE_INDEX, SEND),
            (REPLY_TO_INVITE_TYPE_INDEX, ACCEPT),
        ] {
            let call = InviteCall::decode(type_index, &hex_bytes(hex))
                .expect("a call")
                .expect("decodes");
            assert_eq!(call.type_index(), type_index);
            assert_eq!(call.encode().expect("encode"), hex_bytes(hex));
        }
        let Some(Ok(InviteCall::Send {
            activity, target, ..
        })) = InviteCall::decode(SEND_NEW_INVITE_TYPE_INDEX, &hex_bytes(SEND))
        else {
            panic!("send");
        };
        assert_eq!(activity, 0x7e1a_4366, "the CRC-32 of `trade`");
        assert_eq!(target, b"11111111-2222-3333-4444-555555555555");
        let failed = InviteFailedMsg::decode(INVITE_FAILED_TYPE_INDEX, &hex_bytes(FAILED))
            .expect("a failure")
            .expect("decodes");
        assert_eq!(failed.reason, end::INSUFFICIENT_PARTICIPANTS);
        assert_eq!(failed.encode(), hex_bytes(FAILED));
        assert_eq!(
            failed.header.target.wrapping_sub(failed.header.entity),
            GENERIC_INVITE_COMPONENT
        );
    }

    #[test]
    fn retail_states_read_and_write_back() {
        for (hex, expected_participants) in [(INVITE_STATE, 1), (CLEARED_STATE, 0)] {
            let bytes = hex_bytes(hex);
            let mut reader = Reader::new(&bytes);
            let state = GenericInviteState::read(&mut reader).expect("read");
            assert!(reader.is_empty());
            let details = state.details.clone().expect("slot 1");
            assert_eq!(details.participants.len(), expected_participants);
            let mut writer = Writer::new();
            state.write(&mut writer).expect("write");
            assert_eq!(writer.finish(), bytes);
        }
        let mut writer = Writer::new();
        GenericInviteState::cleared()
            .write(&mut writer)
            .expect("write");
        assert_eq!(writer.finish(), hex_bytes(CLEARED_STATE));
    }

    #[test]
    fn the_id_alone_is_slot_0() {
        let state = GenericInviteState {
            invite_id: Some([7; RAW_ID_LEN]),
            details: None,
        };
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        let bytes = writer.finish();
        assert_eq!(&bytes[..2], &[SLOT_0, 0x01]);
        assert_eq!(
            GenericInviteState::read(&mut Reader::new(&bytes)).expect("read"),
            state
        );
    }

    #[test]
    fn other_types_are_not_invite_calls() {
        assert!(InviteCall::decode(SEND_NEW_INVITE_TYPE_INDEX + 1, &[]).is_none());
        assert!(InviteFailedMsg::decode(INVITE_FAILED_TYPE_INDEX + 1, &[]).is_none());
    }
}
