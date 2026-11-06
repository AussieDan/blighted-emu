// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GroupsComponent` calls and pushes of a plain group: invite, answer, leave, leader, kick vote.
//! Layouts from the retail sessions `Hive 3v3 group`, `3v3_arena_group`, `dungeon_dynasty_tank_`,
//! `invasion` and `msq14` (6 invites, 27 answers, 15 leaves, 47 removed invites, 297 removed
//! members), with the argument kinds of the Azoth catalog for 3502 and 3498.
//!
//! Retail flow (Observed): an invite (3492) names the invitee; when the invite ends, both sides
//! get a 1677 with the invite id: the inviter with the invitee and `outbound` 1, the invitee with
//! the inviter and `outbound` 0. A member who leaves (2252) gets a 5283 for each other member,
//! reason [`REMOVED_LEFT`]. An invite of an offline player gets a 3453 with
//! [`ERROR_INVITEE_OFFLINE`] and the invitee.

use crate::facet::FacetHeader;
use crate::social::text::{CharacterRef, read_text, write_text};
use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// `GroupsComponentServerFacet_RequestGroupInvite`.
pub const GROUP_INVITE_TYPE_INDEX: u64 = 3492;
/// `GroupsComponentServerFacet_RequestReplyToGroupInvite`.
pub const REPLY_TO_INVITE_TYPE_INDEX: u64 = 3495;
/// `GroupsComponentServerFacet_RequestLeaveGroup`.
pub const LEAVE_GROUP_TYPE_INDEX: u64 = 2252;
/// `GroupsComponentServerFacet_RequestSetGroupLeader`.
pub const SET_GROUP_LEADER_TYPE_INDEX: u64 = 3502;
/// `GroupsComponentServerFacet_RequestInitiateKickVotePlayer`.
pub const KICK_VOTE_TYPE_INDEX: u64 = 3498;
/// `GroupsComponentClientFacet_OnReceivedGroupMemberRemoved`.
pub const MEMBER_REMOVED_TYPE_INDEX: u64 = 5283;
/// `GroupsComponentClientFacet_OnReceivedGroupInviteRemoved`.
pub const INVITE_REMOVED_TYPE_INDEX: u64 = 1677;
/// `GroupsComponentClientFacet_OnReceivedErrorMessage`.
pub const GROUP_ERROR_TYPE_INDEX: u64 = 3453;
/// `GroupsComponentServerFacet_RequestPing`.
pub const GROUP_PING_TYPE_INDEX: u64 = 2407;
/// `GroupsComponentClientFacet_OnReceivedPreRaidStatus`.
pub const PRE_RAID_STATUS_TYPE_INDEX: u64 = 715;
/// `GroupsComponentClientFacet_OnReceivedSiegeWarfareStarted`.
pub const SIEGE_WARFARE_STARTED_TYPE_INDEX: u64 = 3454;
/// `GroupsComponentClientFacet_OnReceivedSiegeWarfareEnded`.
pub const SIEGE_WARFARE_ENDED_TYPE_INDEX: u64 = 245;
/// `GroupsComponentServerFacet_RequestMoveRaidMember`.
pub const MOVE_RAID_MEMBER_TYPE_INDEX: u64 = 3501;
/// `GroupsComponentServerFacet_RequestSwapRaidMembers`.
pub const SWAP_RAID_MEMBERS_TYPE_INDEX: u64 = 541;

/// Bytes after the header of every retail 2407 (all zero in the war, raid and healer
/// ledgers; meaning Unknown).
pub const PING_DATA_LEN: usize = 14;

/// The group calls of the client.
pub(crate) const CALL_TYPES: [u64; 8] = [
    GROUP_INVITE_TYPE_INDEX,
    REPLY_TO_INVITE_TYPE_INDEX,
    LEAVE_GROUP_TYPE_INDEX,
    SET_GROUP_LEADER_TYPE_INDEX,
    KICK_VOTE_TYPE_INDEX,
    GROUP_PING_TYPE_INDEX,
    MOVE_RAID_MEMBER_TYPE_INDEX,
    SWAP_RAID_MEMBERS_TYPE_INDEX,
];

/// The answer of an accepted invite (27 of 27 retail answers; the client sends no other in the
/// captures).
pub const REPLY_ACCEPT: u32 = 0;
/// The 2252 value of every retail leave (15 of 15).
pub const LEAVE_VALUE: u32 = 0;
/// The 5283 reason of a member who left (every retail 5283 after a 2252).
pub const REMOVED_LEFT: u32 = 0;
/// The 1677 `kind` of an invite that ended by an answer (46 of 47 retail).
pub const INVITE_ENDED_ANSWERED: u32 = 0;
/// The 3453 error of an invite of an offline player (`3v3_arena_group`: the invitee's online
/// notice came 18 s after the error, and the next invite went through).
pub const ERROR_INVITEE_OFFLINE: u32 = 6;
/// The bytes after the invitee of every retail 3492 (6 of 6): 17 zero bytes.
const INVITE_EXTRA_LEN: usize = RAW_ID_LEN;

/// A group call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupCall {
    /// 3492: invite a player.
    Invite {
        /// The invitee's character id as UUID text.
        character: String,
        /// A 16-byte id after it (zero in every retail invite; meaning Unknown).
        extra: [u8; INVITE_EXTRA_LEN],
        /// The last byte (0 in every retail invite; meaning Unknown).
        flag: u8,
    },
    /// 3495: answer an invite.
    Reply {
        /// The invite id.
        invite: [u8; RAW_ID_LEN],
        /// [`REPLY_ACCEPT`], or another value to decline.
        answer: u32,
    },
    /// 2252: leave the group.
    Leave {
        /// [`LEAVE_VALUE`].
        value: u32,
    },
    /// 3502: make a member the leader.
    SetLeader {
        /// The member's character id as UUID text.
        character: String,
    },
    /// 3498: start a vote to remove a member.
    KickVote {
        /// The member's character id as UUID text.
        character: String,
    },
    /// 2407: place a group ping. The bytes after the header are kept whole (all zero in
    /// retail; meaning Unknown).
    Ping {
        /// The ping bytes.
        data: [u8; PING_DATA_LEN],
    },
    /// 3501: move a raid member to another group of the raid (retail
    /// `raid_group_influence_race`: the member as UUID text, then the group index).
    MoveRaidMember {
        /// The member's character id as UUID text.
        character: String,
        /// The group the member goes to (1 and 2 in retail).
        group: u8,
    },
    /// 541: swap two raid members (two character ids as UUID text).
    SwapRaidMembers {
        /// The first member's character id as UUID text.
        first: String,
        /// The second member's character id as UUID text.
        second: String,
    },
}

impl GroupCall {
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let call = match type_index {
            GROUP_INVITE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Invite {
                    character: read_text(reader)?,
                    extra: reader.raw_id()?,
                    flag: reader.u8()?,
                })
            })(),
            REPLY_TO_INVITE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Reply {
                    invite: reader.raw_id()?,
                    answer: reader.u32()?,
                })
            })(),
            LEAVE_GROUP_TYPE_INDEX => reader.u32().map(|value| Self::Leave { value }),
            SET_GROUP_LEADER_TYPE_INDEX => {
                read_text(reader).map(|character| Self::SetLeader { character })
            }
            KICK_VOTE_TYPE_INDEX => read_text(reader).map(|character| Self::KickVote { character }),
            GROUP_PING_TYPE_INDEX => reader.array().map(|data| Self::Ping { data }),
            MOVE_RAID_MEMBER_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::MoveRaidMember {
                    character: read_text(reader)?,
                    group: reader.u8()?,
                })
            })(),
            SWAP_RAID_MEMBERS_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::SwapRaidMembers {
                    first: read_text(reader)?,
                    second: read_text(reader)?,
                })
            })(),
            _ => return None,
        };
        Some(call)
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Invite { .. } => GROUP_INVITE_TYPE_INDEX,
            Self::Reply { .. } => REPLY_TO_INVITE_TYPE_INDEX,
            Self::Leave { .. } => LEAVE_GROUP_TYPE_INDEX,
            Self::SetLeader { .. } => SET_GROUP_LEADER_TYPE_INDEX,
            Self::KickVote { .. } => KICK_VOTE_TYPE_INDEX,
            Self::Ping { .. } => GROUP_PING_TYPE_INDEX,
            Self::MoveRaidMember { .. } => MOVE_RAID_MEMBER_TYPE_INDEX,
            Self::SwapRaidMembers { .. } => SWAP_RAID_MEMBERS_TYPE_INDEX,
        }
    }

    /// The body of the call to the groups facet `header`, as [`crate::social::SocialCall::decode`]
    /// reads it (a test client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text does not fit its prefix.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        header.write(&mut writer);
        match self {
            Self::Invite {
                character,
                extra,
                flag,
            } => {
                write_text(&mut writer, character)?;
                writer.bytes(extra).u8(*flag);
            }
            Self::Reply { invite, answer } => {
                writer.bytes(invite).u32(*answer);
            }
            Self::Leave { value } => {
                writer.u32(*value);
            }
            Self::SetLeader { character } | Self::KickVote { character } => {
                write_text(&mut writer, character)?;
            }
            Self::Ping { data } => {
                writer.bytes(data);
            }
            Self::MoveRaidMember { character, group } => {
                write_text(&mut writer, character)?;
                writer.u8(*group);
            }
            Self::SwapRaidMembers { first, second } => {
                write_text(&mut writer, first)?;
                write_text(&mut writer, second)?;
            }
        }
        Ok(writer.finish())
    }
}

/// A group push of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupPush {
    /// 5283: a member left the player's group (or the player left: one per other member).
    MemberRemoved {
        /// The member.
        character: CharacterRef,
        /// [`REMOVED_LEFT`].
        reason: u32,
    },
    /// 1677: an invite of the player ended.
    InviteRemoved {
        /// The invite id.
        invite: [u8; RAW_ID_LEN],
        /// The other side: the invitee for the inviter, the inviter for the invitee (none for a
        /// system invite).
        other: CharacterRef,
        /// How it ended ([`INVITE_ENDED_ANSWERED`]; 16 in the one other retail body).
        kind: u32,
        /// A second character (the invitee in the one retail body of kind 16; else none).
        second: CharacterRef,
        /// Whether the player sent the invite.
        outbound: bool,
        /// The last byte (0 in every retail body).
        flag: u8,
    },
    /// 3453: a group request failed.
    Error {
        /// The error ([`ERROR_INVITEE_OFFLINE`]).
        error: u32,
        /// The player it is about.
        character: CharacterRef,
    },
    /// 2407: a group ping of another member, sent on as it came.
    Ping {
        /// The ping bytes.
        data: [u8; PING_DATA_LEN],
    },
    /// 715: the pre-raid status (retail war ledger: a `u32` of 4, then the raid id).
    PreRaidStatus {
        /// A status value (4 in retail; meaning Unknown).
        status: u32,
        /// The raid id (the same 16 bytes as the 3454 of the war).
        raid: [u8; RAW_ID_LEN],
    },
    /// 3454: the raid's war started.
    SiegeWarfareStarted {
        /// The raid id.
        raid: [u8; RAW_ID_LEN],
    },
    /// 245: the raid's war ended (a byte of 1 in retail, then a big-endian `u64`; meanings
    /// Unknown).
    SiegeWarfareEnded {
        /// The first byte.
        flag: u8,
        /// The value after it.
        value: u64,
    },
}

impl GroupPush {
    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::MemberRemoved { .. } => MEMBER_REMOVED_TYPE_INDEX,
            Self::InviteRemoved { .. } => INVITE_REMOVED_TYPE_INDEX,
            Self::Error { .. } => GROUP_ERROR_TYPE_INDEX,
            Self::Ping { .. } => GROUP_PING_TYPE_INDEX,
            Self::PreRaidStatus { .. } => PRE_RAID_STATUS_TYPE_INDEX,
            Self::SiegeWarfareStarted { .. } => SIEGE_WARFARE_STARTED_TYPE_INDEX,
            Self::SiegeWarfareEnded { .. } => SIEGE_WARFARE_ENDED_TYPE_INDEX,
        }
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::MemberRemoved { character, reason } => {
                character.write(writer);
                writer.u32(*reason);
            }
            Self::InviteRemoved {
                invite,
                other,
                kind,
                second,
                outbound,
                flag,
            } => {
                writer.bytes(invite);
                other.write(writer);
                writer.u32(*kind);
                second.write(writer);
                writer.boolean(*outbound).u8(*flag);
            }
            Self::Error { error, character } => {
                writer.u32(*error);
                character.write(writer);
            }
            Self::Ping { data } => {
                writer.bytes(data);
            }
            Self::PreRaidStatus { status, raid } => {
                writer.u32(*status).bytes(raid);
            }
            Self::SiegeWarfareStarted { raid } => {
                writer.bytes(raid);
            }
            Self::SiegeWarfareEnded { flag, value } => {
                writer.u8(*flag).u64(*value);
            }
        }
        Ok(())
    }

    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let push = match type_index {
            MEMBER_REMOVED_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::MemberRemoved {
                    character: CharacterRef::read(reader)?,
                    reason: reader.u32()?,
                })
            })(),
            INVITE_REMOVED_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::InviteRemoved {
                    invite: reader.raw_id()?,
                    other: CharacterRef::read(reader)?,
                    kind: reader.u32()?,
                    second: CharacterRef::read(reader)?,
                    outbound: reader.boolean()?,
                    flag: reader.u8()?,
                })
            })(),
            GROUP_ERROR_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Error {
                    error: reader.u32()?,
                    character: CharacterRef::read(reader)?,
                })
            })(),
            GROUP_PING_TYPE_INDEX => reader.array().map(|data| Self::Ping { data }),
            PRE_RAID_STATUS_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::PreRaidStatus {
                    status: reader.u32()?,
                    raid: reader.raw_id()?,
                })
            })(),
            SIEGE_WARFARE_STARTED_TYPE_INDEX => reader
                .raw_id()
                .map(|raid| Self::SiegeWarfareStarted { raid }),
            SIEGE_WARFARE_ENDED_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::SiegeWarfareEnded {
                    flag: reader.u8()?,
                    value: reader.u64()?,
                })
            })(),
            _ => return None,
        };
        Some(push)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::social::{SocialCall, SocialPush};

    fn text_bytes(text: &str) -> Vec<u8> {
        let mut writer = Writer::new();
        write_text(&mut writer, text).expect("short text");
        writer.finish()
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    const HEADER: &str = "11c62f11c397b2f4e34e14f344a974cc";
    const CHARACTER: &str = "00000000-0000-4000-8000-000000000003";
    const INVITE: &str = "63f5b7c17a794a4fab110a31c021e598";

    /// The call of the retail body of `type_index` with arguments `args`; its encode gives the
    /// same body back.
    fn call(type_index: u64, args: &[u8]) -> GroupCall {
        let body = [hex(HEADER), args.to_vec()].concat();
        match SocialCall::decode(type_index, &body).expect("a social call") {
            Ok((header, SocialCall::Group(call))) => {
                assert_eq!(call.type_index(), type_index);
                assert_eq!(call.encode(header).expect("encode"), body);
                call
            }
            other => panic!("a group call: {other:?}"),
        }
    }

    /// The retail forms (dungeon_dynasty_tank_, Hive 3v3 group), the id replaced.
    #[test]
    fn the_retail_calls_read() {
        let invite = [text_bytes(CHARACTER), vec![0; INVITE_EXTRA_LEN + 1]].concat();
        assert_eq!(invite.len() + 16, 70, "the retail body size");
        assert_eq!(
            call(GROUP_INVITE_TYPE_INDEX, &invite),
            GroupCall::Invite {
                character: CHARACTER.to_owned(),
                extra: [0; INVITE_EXTRA_LEN],
                flag: 0,
            }
        );
        assert_eq!(
            call(
                REPLY_TO_INVITE_TYPE_INDEX,
                &hex(&format!("{INVITE}00000000"))
            ),
            GroupCall::Reply {
                invite: hex(INVITE).try_into().expect("16 bytes"),
                answer: REPLY_ACCEPT,
            }
        );
        assert_eq!(
            call(LEAVE_GROUP_TYPE_INDEX, &hex("00000000")),
            GroupCall::Leave { value: LEAVE_VALUE }
        );
        assert_eq!(
            call(SET_GROUP_LEADER_TYPE_INDEX, &text_bytes(CHARACTER)),
            GroupCall::SetLeader {
                character: CHARACTER.to_owned()
            }
        );
    }

    /// Retail `raid_group_influence_race` and the war ledger: raid moves, a swap, a ping and
    /// the siege pushes (the ids replaced where a test needs a fixed one).
    #[test]
    fn the_retail_raid_calls_and_pushes_read() {
        let member = text_bytes("561138d6-1b4f-4d0a-a0f0-eb0d09e02782");
        assert_eq!(
            call(
                MOVE_RAID_MEMBER_TYPE_INDEX,
                &[member.clone(), vec![1]].concat()
            ),
            GroupCall::MoveRaidMember {
                character: "561138d6-1b4f-4d0a-a0f0-eb0d09e02782".to_owned(),
                group: 1,
            }
        );
        let swap = [
            text_bytes("68ae0505-1380-45c8-8f94-60ed13aa849c"),
            text_bytes("e584623a-9fb9-40ab-b3ae-7dae4e83430f"),
        ]
        .concat();
        assert!(matches!(
            call(SWAP_RAID_MEMBERS_TYPE_INDEX, &swap),
            GroupCall::SwapRaidMembers { .. }
        ));
        assert_eq!(
            call(GROUP_PING_TYPE_INDEX, &[0; PING_DATA_LEN]),
            GroupCall::Ping {
                data: [0; PING_DATA_LEN]
            }
        );
        let raid = [9; RAW_ID_LEN];
        for (push, expected) in [
            (
                GroupPush::PreRaidStatus { status: 4, raid },
                format!("00000004{}", "09".repeat(RAW_ID_LEN)),
            ),
            (
                GroupPush::SiegeWarfareStarted { raid },
                "09".repeat(RAW_ID_LEN),
            ),
            (
                GroupPush::SiegeWarfareEnded {
                    flag: 1,
                    value: 0x18c0_66e7_6a1b_6afc,
                },
                "0118c066e76a1b6afc".to_owned(),
            ),
        ] {
            let push = SocialPush::Group(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(args, hex(&expected), "{push:?}");
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
    }

    /// The retail bodies (Hive 3v3 group, 3v3_arena_group), the ids replaced.
    #[test]
    fn the_retail_pushes_have_the_retail_bytes() {
        let id = [0x22; RAW_ID_LEN];
        let member = format!("05{}", "22".repeat(RAW_ID_LEN));
        let cases = [
            (
                GroupPush::MemberRemoved {
                    character: CharacterRef(Some(id)),
                    reason: REMOVED_LEFT,
                },
                format!("{member}00000000"),
            ),
            (
                GroupPush::InviteRemoved {
                    invite: hex(INVITE).try_into().expect("16 bytes"),
                    other: CharacterRef(Some(id)),
                    kind: INVITE_ENDED_ANSWERED,
                    second: CharacterRef(None),
                    outbound: true,
                    flag: 0,
                },
                format!("{INVITE}{member}0000000000000100"),
            ),
            (
                GroupPush::InviteRemoved {
                    invite: hex(INVITE).try_into().expect("16 bytes"),
                    other: CharacterRef(None),
                    kind: INVITE_ENDED_ANSWERED,
                    second: CharacterRef(None),
                    outbound: false,
                    flag: 0,
                },
                format!("{INVITE}00000000000000000000"),
            ),
            (
                GroupPush::Error {
                    error: ERROR_INVITEE_OFFLINE,
                    character: CharacterRef(Some(id)),
                },
                format!("00000006{member}"),
            ),
        ];
        for (push, expected) in cases {
            let push = SocialPush::Group(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(args, hex(&expected));
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
    }

    /// A header of synthetic ids.
    const ROUND_TRIP_HEADER: FacetHeader = FacetHeader {
        target: 0x0102_0304_0506_0708,
        entity: 0x1112_1314_1516_1718,
    };

    #[test]
    fn every_call_encodes_and_reads_back() {
        let calls = [
            GroupCall::Invite {
                character: CHARACTER.to_owned(),
                extra: [7; INVITE_EXTRA_LEN],
                flag: 1,
            },
            GroupCall::Reply {
                invite: [8; RAW_ID_LEN],
                answer: 1,
            },
            GroupCall::Leave { value: 2 },
            GroupCall::SetLeader {
                character: CHARACTER.to_owned(),
            },
            GroupCall::KickVote {
                character: CHARACTER.to_owned(),
            },
            GroupCall::Ping {
                data: [5; PING_DATA_LEN],
            },
            GroupCall::MoveRaidMember {
                character: CHARACTER.to_owned(),
                group: 2,
            },
            GroupCall::SwapRaidMembers {
                first: CHARACTER.to_owned(),
                second: "00000000-0000-4000-8000-000000000009".to_owned(),
            },
        ]
        .map(SocialCall::Group);
        crate::social::assert_calls_round_trip(ROUND_TRIP_HEADER, &calls);
    }
}
