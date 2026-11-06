// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lookups the client sends after the level entry, and the server's answers (retail captures
//! `newchar-retail-2026-09-18` and `newchar-linux-2026-09-12`; names from the Azoth catalog
//! catalog). Retail answered each call 25 to 60 ms after it, with the request's facet header:
//!
//! | Call | Answer |
//! |---|---|
//! | 2556 `ContractsComponentServerFacet_LookupContractsByParticipant` | 2278 `ContractsComponentClientFacet_OnLookupContractsByParticipant` |
//! | 2776 `TwitchComponentServerFacet_RequestJoinableSubArmies` | 4178 `TwitchComponentClientFacet_ReceiveJoinableSubArmies` |
//! | 843 `MetaAchievementComponentServerFacet_ServerRequestGetPendingMetaAchievements` | 3802 `MetaAchievementComponentClientFacet_ClientOnReceiveRewardPendingMetaAchievements` |
//! | 5463 `GroupsComponentServerFacet_RequestJoinableLobbyCountsForActivity` | 5458 `GroupsComponentClientFacet_OnJoinableLobbyCountsForActivityReceived` |
//!
//! The trading post answers the contracts lookup from the world database
//! ([`crate::contracts`]). This server has no streamer groups, meta achievement rewards or
//! group finder lobbies, so each of those answers is the empty form retail sent to a new
//! character.

use crate::contracts::ParticipantQuery;
use crate::facet::FacetHeader;
use crate::server::ServerMessage;
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer};

/// Type index of `ContractsComponentServerFacet_LookupContractsByParticipant`.
pub const LOOKUP_CONTRACTS_TYPE_INDEX: u64 = 2556;
/// Type index of `ContractsComponentClientFacet_OnLookupContractsByParticipant`.
pub const CONTRACTS_LOOKED_UP_TYPE_INDEX: u64 = 2278;
/// Type index of `TwitchComponentServerFacet_RequestJoinableSubArmies`.
pub const REQUEST_SUB_ARMIES_TYPE_INDEX: u64 = 2776;
/// Type index of `TwitchComponentClientFacet_ReceiveJoinableSubArmies`.
pub const JOINABLE_SUB_ARMIES_TYPE_INDEX: u64 = 4178;
/// Type index of `MetaAchievementComponentServerFacet_ServerRequestGetPendingMetaAchievements`.
pub const REQUEST_PENDING_META_ACHIEVEMENTS_TYPE_INDEX: u64 = 843;
/// Type index of `MetaAchievementComponentClientFacet_ClientOnReceiveRewardPendingMetaAchievements`.
pub const PENDING_META_ACHIEVEMENTS_TYPE_INDEX: u64 = 3802;
/// Type index of `GroupsComponentServerFacet_RequestJoinableLobbyCountsForActivity`.
pub const REQUEST_LOBBY_COUNTS_TYPE_INDEX: u64 = 5463;
/// Type index of `GroupsComponentClientFacet_OnJoinableLobbyCountsForActivityReceived`.
pub const LOBBY_COUNTS_TYPE_INDEX: u64 = 5458;

/// How many lobby counts a lobby-counts answer carries: 132 in all 81 retail answers of the two
/// own captures and the first 60 open-world sessions (each count a big-endian `u32`, 0 or 1).
pub const LOBBY_COUNT_SLOTS: usize = 132;

/// One lookup call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupRequest {
    /// 2556: the trading post contracts of a participant; the lookup id, which the answer
    /// repeats, ends the body.
    Contracts {
        /// The header (the player's `ContractsComponent`).
        header: FacetHeader,
        /// The participant, filters and paging.
        query: ParticipantQuery,
        /// The lookup id.
        lookup: [u8; RAW_ID_LEN],
    },
    /// 2776: the streamer groups the player could join.
    SubArmies {
        /// The header (the player's `TwitchComponent`).
        header: FacetHeader,
    },
    /// 843: the meta achievements whose reward waits.
    PendingMetaAchievements {
        /// The header (the player's `MetaAchievementComponent`).
        header: FacetHeader,
    },
    /// 5463: how many group finder lobbies wait for one activity.
    LobbyCounts {
        /// The header (the player's `GroupsComponent`).
        header: FacetHeader,
        /// The activity (1 in every retail call).
        activity: u8,
    },
}

impl LookupRequest {
    /// Decodes `body` when `type_index` is a lookup call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let mut reader = Reader::new(body);
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            let header = FacetHeader::read(reader)?;
            let request = match type_index {
                LOOKUP_CONTRACTS_TYPE_INDEX => Self::Contracts {
                    header,
                    query: ParticipantQuery::read(reader)?,
                    lookup: reader.raw_id()?,
                },
                REQUEST_SUB_ARMIES_TYPE_INDEX => Self::SubArmies { header },
                REQUEST_PENDING_META_ACHIEVEMENTS_TYPE_INDEX => {
                    Self::PendingMetaAchievements { header }
                }
                _ => Self::LobbyCounts {
                    header,
                    activity: reader.u8()?,
                },
            };
            Ok(request)
        };
        match type_index {
            LOOKUP_CONTRACTS_TYPE_INDEX
            | REQUEST_SUB_ARMIES_TYPE_INDEX
            | REQUEST_PENDING_META_ACHIEVEMENTS_TYPE_INDEX
            | REQUEST_LOBBY_COUNTS_TYPE_INDEX => Some(read(&mut reader).and_then(|request| {
                reader.finish()?;
                Ok(request)
            })),
            _ => None,
        }
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Contracts { .. } => LOOKUP_CONTRACTS_TYPE_INDEX,
            Self::SubArmies { .. } => REQUEST_SUB_ARMIES_TYPE_INDEX,
            Self::PendingMetaAchievements { .. } => REQUEST_PENDING_META_ACHIEVEMENTS_TYPE_INDEX,
            Self::LobbyCounts { .. } => REQUEST_LOBBY_COUNTS_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] when a list of the contracts query is too long.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        match self {
            Self::Contracts {
                header,
                query,
                lookup,
            } => {
                header.write(&mut writer);
                query.write(&mut writer)?;
                writer.bytes(lookup);
            }
            Self::SubArmies { header } | Self::PendingMetaAchievements { header } => {
                header.write(&mut writer);
            }
            Self::LobbyCounts { header, activity } => {
                header.write(&mut writer);
                writer.u8(*activity);
            }
        }
        Ok(writer.finish())
    }

    /// The answer retail sent to a player with no streamer group, waiting reward or open
    /// lobby; `None` for the contracts lookup, which the trading post answers.
    pub fn answer(&self) -> Option<ServerMessage> {
        match *self {
            Self::Contracts { .. } => None,
            Self::SubArmies { header } => {
                Some(ServerMessage::JoinableSubArmies(JoinableSubArmiesMsg {
                    header,
                }))
            }
            Self::PendingMetaAchievements { header } => Some(
                ServerMessage::PendingMetaAchievements(PendingMetaAchievementsMsg {
                    header,
                    achievements: Vec::new(),
                }),
            ),
            Self::LobbyCounts { header, activity } => {
                Some(ServerMessage::LobbyCounts(LobbyCountsMsg {
                    header,
                    activity,
                    counts: vec![0; LOBBY_COUNT_SLOTS],
                }))
            }
        }
    }
}

/// 4178: the streamer groups the player could join; none here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinableSubArmiesMsg {
    /// The header of the call.
    pub header: FacetHeader,
}

impl BodyCodec for JoinableSubArmiesMsg {
    const TYPE_INDEX: u64 = JOINABLE_SUB_ARMIES_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer.prefixed(0)?;
        Ok(writer.finish())
    }

    /// A list that is not empty is [`MessageError::CountTooLarge`]: the layout of a streamer
    /// group is Unknown.
    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let count = reader.prefixed()?;
        if count != 0 {
            return Err(MessageError::CountTooLarge { count, limit: 0 });
        }
        reader.finish()?;
        Ok(Self { header })
    }
}

/// 3802: the meta achievements whose reward waits (retail linux 2026-09-12 sent one id,
/// `ed846955`; retail 2026-09-18 none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMetaAchievementsMsg {
    /// The header of the call.
    pub header: FacetHeader,
    /// The meta achievement ids.
    pub achievements: Vec<u32>,
}

impl BodyCodec for PendingMetaAchievementsMsg {
    const TYPE_INDEX: u64 = PENDING_META_ACHIEVEMENTS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        write_u32_list(&mut writer, &self.achievements)?;
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let achievements = read_u32_list(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            header,
            achievements,
        })
    }
}

/// 5458: the open lobbies of one activity, one count per slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyCountsMsg {
    /// The header of the call.
    pub header: FacetHeader,
    /// The activity of the call.
    pub activity: u8,
    /// The counts ([`LOBBY_COUNT_SLOTS`] of them).
    pub counts: Vec<u32>,
}

impl BodyCodec for LobbyCountsMsg {
    const TYPE_INDEX: u64 = LOBBY_COUNTS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer.u8(self.activity);
        write_u32_list(&mut writer, &self.counts)?;
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let activity = reader.u8()?;
        let counts = read_u32_list(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            header,
            activity,
            counts,
        })
    }
}

/// A prefix-coded count, then that many big-endian `u32`s.
fn write_u32_list(writer: &mut Writer, values: &[u32]) -> Result<(), MessageError> {
    writer.prefixed(u64::try_from(values.len()).unwrap_or(u64::MAX))?;
    for value in values {
        writer.u32(*value);
    }
    Ok(())
}

fn read_u32_list(reader: &mut Reader<'_>) -> Result<Vec<u32>, MessageError> {
    let count = reader.prefixed()?;
    let limit = u64::try_from(reader.remaining() / size_of::<u32>()).unwrap_or(u64::MAX);
    if count > limit {
        return Err(MessageError::CountTooLarge { count, limit });
    }
    (0..count).map(|_| reader.u32()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{ContractListMsg, ContractsLookedUpMsg};

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    /// Decodes the lookup of `type_index` in the bytes of `request` and checks that it names
    /// that index and writes back those bytes.
    fn lookup(type_index: u64, request: &str) -> LookupRequest {
        let body = bytes(request);
        let lookup = LookupRequest::decode(type_index, &body)
            .expect("a lookup")
            .expect("decodes");
        assert_eq!(lookup.type_index(), type_index);
        assert_eq!(lookup.encode().expect("encodes"), body);
        lookup
    }

    fn answer_of(type_index: u64, request: &str) -> Vec<u8> {
        lookup(type_index, request)
            .answer()
            .expect("an answer")
            .encode()
            .expect("encodes")
    }

    fn contracts_query(request: &str) -> (FacetHeader, ParticipantQuery, [u8; RAW_ID_LEN]) {
        let LookupRequest::Contracts {
            header,
            query,
            lookup,
        } = lookup(LOOKUP_CONTRACTS_TYPE_INDEX, request)
        else {
            panic!("a contracts lookup");
        };
        (header, query, lookup)
    }

    /// Retail 2026-09-18, 9117 ms: the first contracts lookup.
    const FIRST_CONTRACTS_LOOKUP: &str = "b39cd7472ccfb38e2715f9bca18fdececede8fa1bcf915277764e12f21bd772800000000000000010000000100000000000a0100000001010009883e449578cc43b3a08879f64b511b7e";

    /// Retail 2026-09-18, 9117 and 9180 ms: the first lookup (the player's open sell orders, ten
    /// per page) and the answer of a player with none.
    #[test]
    fn a_contracts_lookup_reads_its_query_and_the_empty_answer_has_the_retail_bytes() {
        const ANSWER: &str =
            "b39cd7472ccfb38e2715f9bca18fdece883e449578cc43b3a08879f64b511b7e000000000000";
        let (header, query, lookup) = contracts_query(FIRST_CONTRACTS_LOOKUP);
        assert_eq!(query.participant.kind, 0);
        assert_eq!(
            (
                query.open_filter,
                query.role_filter,
                query.page,
                query.page_size
            ),
            (
                crate::contracts::open_filter::OPEN,
                crate::contracts::role_filter::CREATOR,
                0,
                10
            )
        );
        assert_eq!(query.kinds, vec![1]);
        assert_eq!(query.tail, vec![9]);
        let empty = ContractsLookedUpMsg(ContractListMsg {
            header,
            lookup,
            contracts: Vec::new(),
        });
        assert_eq!(empty.encode().expect("encodes"), bytes(ANSWER));
        let message =
            ServerMessage::decode(CONTRACTS_LOOKED_UP_TYPE_INDEX, &bytes(ANSWER)).expect("known");
        assert_eq!(
            message.expect("decodes").encode().expect("encodes"),
            bytes(ANSWER)
        );
    }

    /// Retail 2026-09-18, 18776 ms: the second form of the call (72 bytes, every role and state,
    /// 100 per page, the make-good contracts).
    #[test]
    fn the_short_contracts_lookup_reads() {
        const REQUEST: &str = "b39cd7472ccfb38e2715f9bca18fdececede8fa1bcf915277764e12f21bd772800000000000000000000000000000000006401000000020050b74284e0f14ae5b9cacd3470c8acff";
        let (_, query, lookup) = contracts_query(REQUEST);
        assert_eq!((query.open_filter, query.page_size), (0, 100));
        assert_eq!(query.kinds, vec![2]);
        assert!(query.tail.is_empty());
        assert_eq!(lookup[0], 0x50);
    }

    /// Retail 2026-09-18, 16342 and 16367 ms.
    #[test]
    fn a_sub_army_request_gets_the_empty_list() {
        const CALL: &str = "b12b7008f35b090b2715f9bca18fdece";
        assert_eq!(
            answer_of(REQUEST_SUB_ARMIES_TYPE_INDEX, CALL),
            bytes(&format!("{CALL}00"))
        );
    }

    /// Retail 2026-09-18, 43615 and 43643 ms; the one-id form of retail 2026-09-12 (34628 ms)
    /// reads too.
    #[test]
    fn a_pending_reward_request_gets_the_empty_list() {
        const CALL: &str = "5562d7a2e6d7c87b2715f9bca18fdece";
        assert_eq!(
            answer_of(REQUEST_PENDING_META_ACHIEVEMENTS_TYPE_INDEX, CALL),
            bytes(&format!("{CALL}00"))
        );
        let one = PendingMetaAchievementsMsg::decode(&bytes(
            "edf64d24e82b5407bfa96f3ea2e36a5a01ed846955",
        ))
        .expect("decodes");
        assert_eq!(one.achievements, vec![0xed84_6955]);
    }

    /// Retail 2026-09-18, 10582 and 10614 ms: the activity, 132 counts, all 0.
    #[test]
    fn a_lobby_count_request_gets_132_zero_counts() {
        const CALL: &str = "0df149e7593b0a802715f9bca18fdece";
        let answer = answer_of(REQUEST_LOBBY_COUNTS_TYPE_INDEX, &format!("{CALL}01"));
        let mut expected = bytes(&format!("{CALL}018402"));
        expected.extend([0; LOBBY_COUNT_SLOTS * size_of::<u32>()]);
        assert_eq!(answer, expected);
        assert_eq!(answer.len(), 547, "the retail body size");
    }

    #[test]
    fn other_types_and_short_bodies_are_not_lookups() {
        assert!(LookupRequest::decode(REQUEST_SUB_ARMIES_TYPE_INDEX + 1, &[]).is_none());
        assert!(matches!(
            LookupRequest::decode(
                LOOKUP_CONTRACTS_TYPE_INDEX,
                &bytes("b39cd7472ccfb38e2715f9bca18fdece00")
            ),
            Some(Err(_))
        ));
        assert!(matches!(
            LookupRequest::decode(
                REQUEST_SUB_ARMIES_TYPE_INDEX,
                &bytes("b12b7008f35b090b2715f9bca18fdece00")
            ),
            Some(Err(_))
        ));
    }

    #[test]
    fn every_lookup_writes_what_it_reads() {
        let header = FacetHeader {
            target: 0xb39c_d747_2ccf_b38e,
            entity: 0x2715_f9bc_a18f_dece,
        };
        let (_, query, _) = contracts_query(FIRST_CONTRACTS_LOOKUP);
        let calls = [
            LookupRequest::Contracts {
                header,
                query: ParticipantQuery {
                    page: 3,
                    kinds: vec![1, 2],
                    ..query
                },
                lookup: [0x88; RAW_ID_LEN],
            },
            LookupRequest::SubArmies { header },
            LookupRequest::PendingMetaAchievements { header },
            LookupRequest::LobbyCounts {
                header,
                activity: 2,
            },
        ];
        for call in calls {
            let body = call.encode().expect("encodes");
            assert_eq!(
                LookupRequest::decode(call.type_index(), &body),
                Some(Ok(call))
            );
        }
    }
}
