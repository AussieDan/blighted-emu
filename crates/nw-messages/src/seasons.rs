// SPDX-License-Identifier: AGPL-3.0-or-later
//! The season activities and rewards: the client's calls of the `SeasonsRewardsComponent` and
//! the server's answers (retail `Hive_Seasons_Rewards` and `reroll_activities`: 14 calls; economy
//! note `local-data/scratch-work/run15/economy/REPORT.md`, section 5).
//!
//! | Call | Answer |
//! |---|---|
//! | 5645 `ActivitiesRequestStampSquare` (card, square) | 5686 `OnActivitiesStampResponse` |
//! | 5647 `ActivitiesRequestNewCard` | 5687 `OnActivitiesNewCardResponse` |
//! | 5648 `ActivitiesRequestReroll` | 5688 `OnActivitiesRerollResponse` |
//! | 5611 `RequestRedeemSeasonsReward` (reward id) | 5629 `OnRedeemSeasonsRewardResponse` (reward id) |
//!
//! Each answer is a `SeasonsRewardsActivitiesResponse` `u32` on the call's header (retail 1,
//! `Success`, on all 14).

use crate::facet::FacetHeader;
use crate::payload::{BundleMember, MemberState};
use crate::wire::{MessageError, Reader, Writer};

/// `SeasonsRewardsComponentServerFacet_ActivitiesRequestStampSquare`.
pub const STAMP_SQUARE_TYPE_INDEX: u64 = 5645;
/// `SeasonsRewardsComponentServerFacet_ActivitiesRequestNewCard`.
pub const NEW_CARD_TYPE_INDEX: u64 = 5647;
/// `SeasonsRewardsComponentServerFacet_ActivitiesRequestReroll`.
pub const REROLL_TYPE_INDEX: u64 = 5648;
/// `SeasonsRewardsComponentServerFacet_RequestRedeemSeasonsReward`.
pub const REDEEM_REWARD_TYPE_INDEX: u64 = 5611;
/// `SeasonsRewardsComponentClientFacet_OnActivitiesStampResponse`.
pub const STAMP_RESPONSE_TYPE_INDEX: u64 = 5686;
/// `SeasonsRewardsComponentClientFacet_OnActivitiesNewCardResponse`.
pub const NEW_CARD_RESPONSE_TYPE_INDEX: u64 = 5687;
/// `SeasonsRewardsComponentClientFacet_OnActivitiesRerollResponse`.
pub const REROLL_RESPONSE_TYPE_INDEX: u64 = 5688;
/// `SeasonsRewardsComponentClientFacet_OnRedeemSeasonsRewardResponse`.
pub const REDEEM_RESPONSE_TYPE_INDEX: u64 = 5629;
/// Bytes of a stamp call's square (Observed values: row, column and flag bytes, `00 02 00 01`
/// and `11 01 00 00`; their names are Unknown).
pub const SQUARE_LEN: usize = 4;

/// `SeasonsRewardsActivitiesResponse` (enum strings in address order: `Unknown`, `Success`,
/// `Failure`, `Failure_MaxTransactions`, `InsufficientAzoth`, `Failure_Throttled`,
/// `Failure_CrossWorld`).
pub mod response {
    /// The call was carried out.
    pub const SUCCESS: u32 = 1;
    /// The call was refused.
    pub const FAILURE: u32 = 2;
}

/// One season call of the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeasonsCall {
    /// 5645: stamp one square of an activity card.
    StampSquare {
        /// The header.
        header: FacetHeader,
        /// The card index (retail 26 and 29).
        card: u32,
        /// The square.
        square: [u8; SQUARE_LEN],
    },
    /// 5647: a new activity card.
    NewCard {
        /// The header.
        header: FacetHeader,
    },
    /// 5648: reroll the card's tasks.
    Reroll {
        /// The header.
        header: FacetHeader,
    },
    /// 5611: redeem one season reward.
    Redeem {
        /// The header.
        header: FacetHeader,
        /// The reward id.
        reward: u64,
    },
}

impl SeasonsCall {
    /// Decodes `body` when `type_index` is a season call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            let header = FacetHeader::read(reader)?;
            Ok(match type_index {
                STAMP_SQUARE_TYPE_INDEX => Self::StampSquare {
                    header,
                    card: reader.u32()?,
                    square: reader.array()?,
                },
                NEW_CARD_TYPE_INDEX => Self::NewCard { header },
                REROLL_TYPE_INDEX => Self::Reroll { header },
                _ => Self::Redeem {
                    header,
                    reward: reader.u64()?,
                },
            })
        };
        match type_index {
            STAMP_SQUARE_TYPE_INDEX
            | NEW_CARD_TYPE_INDEX
            | REROLL_TYPE_INDEX
            | REDEEM_REWARD_TYPE_INDEX => {
                let mut reader = Reader::new(body);
                Some(read(&mut reader).and_then(|call| {
                    reader.finish()?;
                    Ok(call)
                }))
            }
            _ => None,
        }
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::StampSquare { .. } => STAMP_SQUARE_TYPE_INDEX,
            Self::NewCard { .. } => NEW_CARD_TYPE_INDEX,
            Self::Reroll { .. } => REROLL_TYPE_INDEX,
            Self::Redeem { .. } => REDEEM_REWARD_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::StampSquare {
                header,
                card,
                square,
            } => {
                header.write(&mut writer);
                writer.u32(*card).bytes(square);
            }
            Self::NewCard { header } | Self::Reroll { header } => header.write(&mut writer),
            Self::Redeem { header, reward } => {
                header.write(&mut writer);
                writer.u64(*reward);
            }
        }
        writer.finish()
    }

    /// The answer to the call with `result` (a [`response`] value).
    pub const fn answer(&self, result: u32) -> SeasonsMessage {
        match *self {
            Self::StampSquare { header, .. } => SeasonsMessage::Response {
                type_index: STAMP_RESPONSE_TYPE_INDEX,
                header,
                result,
            },
            Self::NewCard { header } => SeasonsMessage::Response {
                type_index: NEW_CARD_RESPONSE_TYPE_INDEX,
                header,
                result,
            },
            Self::Reroll { header } => SeasonsMessage::Response {
                type_index: REROLL_RESPONSE_TYPE_INDEX,
                header,
                result,
            },
            Self::Redeem { header, reward } => SeasonsMessage::Redeemed {
                header,
                reward,
                result,
            },
        }
    }
}

/// One season answer of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeasonsMessage {
    /// 5686, 5687 or 5688.
    Response {
        /// The type index.
        type_index: u64,
        /// The header of the call.
        header: FacetHeader,
        /// A [`response`] value.
        result: u32,
    },
    /// 5629.
    Redeemed {
        /// The header of the call.
        header: FacetHeader,
        /// The reward id of the call.
        reward: u64,
        /// A [`response`] value.
        result: u32,
    },
}

impl SeasonsMessage {
    /// Decodes `body` when `type_index` is a season answer; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            let header = FacetHeader::read(reader)?;
            Ok(if type_index == REDEEM_RESPONSE_TYPE_INDEX {
                Self::Redeemed {
                    header,
                    reward: reader.u64()?,
                    result: reader.u32()?,
                }
            } else {
                Self::Response {
                    type_index,
                    header,
                    result: reader.u32()?,
                }
            })
        };
        match type_index {
            STAMP_RESPONSE_TYPE_INDEX
            | NEW_CARD_RESPONSE_TYPE_INDEX
            | REROLL_RESPONSE_TYPE_INDEX
            | REDEEM_RESPONSE_TYPE_INDEX => {
                let mut reader = Reader::new(body);
                Some(read(&mut reader).and_then(|message| {
                    reader.finish()?;
                    Ok(message)
                }))
            }
            _ => None,
        }
    }

    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Response { type_index, .. } => *type_index,
            Self::Redeemed { .. } => REDEEM_RESPONSE_TYPE_INDEX,
        }
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::Response { header, result, .. } => {
                header.write(&mut writer);
                writer.u32(*result);
            }
            Self::Redeemed {
                header,
                reward,
                result,
            } => {
                header.write(&mut writer);
                writer.u64(*reward).u32(*result);
            }
        }
        writer.finish()
    }
}

/// The component of the player's `Javelin::SeasonsRewardsReplicatedState`.
pub const SEASONS_REWARDS_COMPONENT: u64 = 72;
/// Type index of `Javelin::SeasonsRewardsReplicatedState`.
pub const SEASONS_REWARDS_TYPE_INDEX: u64 = 5485;
/// The first full season rewards state of a new character: retail's (newchar-retail-2026-09-18
/// at 9.06 s and newchar-linux-2026-09-12: the same bytes but for the list versions, a counter of
/// the retail session, and 4 bytes of one 8-byte field, Inferred `wildStampAwardBound`), with
/// our list version [`crate::items::FIRST_VERSION`] and the 2026-09-18 field. It sets
/// `IsInitialized` and the season data (season ids, bitmasks, XP).
const NEW_CHARACTER_SEASONS_REWARDS: [u8; 248] = [
    0x01, 0xff, 0x01, 0x00, 0x01, 0x03, 0x01, 0x00, 0x01, 0x00, 0x01, 0x03, 0x01, 0x00, 0x02, 0x00,
    0x01, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x15, 0xab, 0xb9, 0x1f, 0xb7, 0x35, 0xdd, 0x8a, 0x14,
    0x69, 0x93, 0x16, 0x0a, 0x64, 0xea, 0xe9, 0x8b, 0x19, 0x9d, 0x1d, 0xce, 0xf3, 0x38, 0x30, 0x03,
    0x94, 0x75, 0xc5, 0xd2, 0x94, 0x92, 0x7d, 0xff, 0xe3, 0x72, 0xf5, 0x44, 0xac, 0xd4, 0xdb, 0xae,
    0x4f, 0xc7, 0xba, 0x48, 0x17, 0x22, 0x6a, 0xb7, 0x51, 0xb1, 0x4f, 0x10, 0x84, 0x3f, 0x00, 0x95,
    0xe4, 0xf8, 0x89, 0x70, 0x8e, 0x2b, 0x3b, 0x0d, 0x0d, 0x7c, 0x94, 0x68, 0x6e, 0x9a, 0x2d, 0x58,
    0x38, 0xc0, 0x8a, 0xde, 0xdb, 0xd3, 0xeb, 0x38, 0xf9, 0x2c, 0x0b, 0x9b, 0xff, 0x00, 0x0a, 0x00,
    0x03, 0x18, 0xe0, 0xff, 0x85, 0x60, 0x03, 0x00, 0x00, 0xff, 0x00, 0x01, 0x01, 0x00, 0x01, 0x03,
    0x02, 0x22, 0x21, 0x5a, 0xd9, 0xc2, 0x28, 0x71, 0x8c, 0x37, 0x00, 0x01, 0x03, 0x06, 0x04, 0x00,
    0x04, 0x00, 0x00, 0x00, 0x00, 0x01, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xa4, 0xa3,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x03, 0x04, 0x00, 0x00, 0x00, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x0f, 0xff, 0x00, 0x00, 0x11, 0x00, 0xdf, 0x00, 0x6e, 0x00, 0x00, 0x01, 0x03, 0x04,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x18, 0x70, 0x3e, 0xe3, 0x52, 0x00, 0x88, 0x84,
];

/// The player's first full season rewards state ([`NEW_CHARACTER_SEASONS_REWARDS`]).
///
/// **Observed** (retail newchar 2026-09-18): the server sends it 9.06 s after the own card, and
/// the client pins its tracked quest (1458) 4 ms later. Client build 25810808: the season
/// rewards client facet (`FUN_143dbcdf0`) starts the objectives facet's sync of the server's
/// tracked list with its own pins (`FUN_143a58de0`, the flag at `+0x5b8`) for the local player.
/// Without it no tracked quest is pinned on its own (live 2026-09-29 under gdb: the tracked list
/// arrived and the sync never ran). A body with only `IsInitialized` did not start it: the
/// facet's state handler (`FUN_143db8ad0`) also reads the season lists.
pub fn seasons_rewards_initialized() -> BundleMember {
    BundleMember {
        component: SEASONS_REWARDS_COMPONENT,
        state: MemberState::Recorded {
            class: SEASONS_REWARDS_TYPE_INDEX,
            body: NEW_CHARACTER_SEASONS_REWARDS.to_vec(),
        },
    }
}

/// The component of the player's `Javelin::SeasonsRewardsTrackedStatReplicatedState`.
pub const SEASONS_TRACKED_STATS_COMPONENT: u64 = 73;
/// Type index of `Javelin::SeasonsRewardsTrackedStatReplicatedState`.
pub const SEASONS_TRACKED_STATS_TYPE_INDEX: u64 = 5606;
/// The first full tracked stats state of a new character: retail's (newchar-retail-2026-09-18
/// at 9.06 s, in the bundle of [`NEW_CHARACTER_SEASONS_REWARDS`]; newchar-linux-2026-09-12 has
/// the same bytes but for the list versions and 5 stat counts), with our list version
/// [`crate::items::FIRST_VERSION`]. Its first byte sets `initialized`.
const NEW_CHARACTER_SEASONS_TRACKED_STATS: [u8; 337] = [
    0x01, 0x1f, 0x01, 0x00, 0x01, 0x03, 0x02, 0x22, 0x21, 0x5a, 0xd9, 0x8d, 0x00, 0x24, 0xe9, 0x00,
    0x01, 0x03, 0x02, 0x00, 0x1e, 0x00, 0x15, 0x00, 0x01, 0x03, 0x33, 0x00, 0x14, 0x00, 0x22, 0x00,
    0x27, 0x00, 0x4f, 0x00, 0x53, 0x00, 0x69, 0x00, 0x6e, 0x00, 0x75, 0x00, 0x7e, 0x00, 0x89, 0x00,
    0x8e, 0x00, 0x9f, 0x00, 0xa2, 0x00, 0xaa, 0x00, 0xb3, 0x00, 0xb9, 0x00, 0xbd, 0x00, 0xd7, 0x00,
    0xdf, 0x00, 0xfd, 0x01, 0x02, 0x01, 0x0d, 0x01, 0x23, 0x01, 0x32, 0x01, 0x47, 0x01, 0x48, 0x01,
    0x50, 0x01, 0x5c, 0x01, 0x5f, 0x01, 0x75, 0x00, 0x1f, 0x00, 0x40, 0x00, 0x48, 0x00, 0x4e, 0x00,
    0x58, 0x00, 0x5c, 0x00, 0x6a, 0x00, 0x8c, 0x00, 0x94, 0x00, 0xbf, 0x00, 0xda, 0x00, 0xee, 0x00,
    0xf1, 0x00, 0xfc, 0x01, 0x09, 0x01, 0x1a, 0x01, 0x40, 0x01, 0x42, 0x01, 0x51, 0x01, 0x5b, 0x01,
    0x75, 0x00, 0x01, 0x03, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x0c, 0x00, 0x00, 0x00, 0xb7, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x01, 0xba, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00,
    0x16, 0x00, 0x00, 0x01, 0x8a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
    0x8a, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
    0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xcf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00,
];

/// The player's first full tracked stats state ([`NEW_CHARACTER_SEASONS_TRACKED_STATS`]).
///
/// **Observed** (client build 25810808): the tracked stat facet's state handler
/// (`FUN_143ddbd50`) sends its bus event (`FUN_143dcbfa0`) when `initialized` turns 1 on the
/// local player's active entity. The season rewards facet's handler of that event
/// (`FUN_143dbcdf0`) starts the objectives facet's sync of the tracked list (`FUN_143a58de0`).
/// Live 2026-09-29 under gdb with only [`seasons_rewards_initialized`]: that handler never ran.
pub fn seasons_tracked_stats_initialized() -> BundleMember {
    BundleMember {
        component: SEASONS_TRACKED_STATS_COMPONENT,
        state: MemberState::Recorded {
            class: SEASONS_TRACKED_STATS_TYPE_INDEX,
            body: NEW_CHARACTER_SEASONS_TRACKED_STATS.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hex_bytes;

    const HEADER: &str = "e5fe5c07dc18f1c3e34e14f344a974cc";

    /// Retail `Hive_Seasons_Rewards` 23847 ms and 23885 ms: a stamp and its answer.
    /// Decodes the call of `type_index` in `body` and checks that it names that index and
    /// writes back `body`.
    fn decode_call(type_index: u64, body: &[u8]) -> SeasonsCall {
        let call = SeasonsCall::decode(type_index, body)
            .expect("a call")
            .expect("decodes");
        assert_eq!(call.type_index(), type_index);
        assert_eq!(call.encode(), body);
        call
    }

    #[test]
    fn a_retail_stamp_gets_the_retail_answer_form() {
        let call = decode_call(
            STAMP_SQUARE_TYPE_INDEX,
            &hex_bytes(&format!("{HEADER}0000001a00020001")),
        );
        let SeasonsCall::StampSquare { card, square, .. } = call else {
            panic!("stamp");
        };
        assert_eq!((card, square), (26, [0, 2, 0, 1]));
        let answer = call.answer(response::SUCCESS);
        assert_eq!(answer.type_index(), STAMP_RESPONSE_TYPE_INDEX);
        assert_eq!(answer.encode(), hex_bytes(&format!("{HEADER}00000001")));
    }

    /// Retail `Hive_Seasons_Rewards` 49498 ms and 49636 ms: a redeem and its answer.
    #[test]
    fn a_retail_redeem_repeats_its_reward() {
        let call = decode_call(
            REDEEM_REWARD_TYPE_INDEX,
            &hex_bytes(&format!("{HEADER}a17b8b9a22215ad9")),
        );
        let answer = call.answer(response::SUCCESS);
        let body = hex_bytes(&format!("{HEADER}a17b8b9a22215ad900000001"));
        assert_eq!(answer.encode(), body);
        assert_eq!(
            SeasonsMessage::decode(REDEEM_RESPONSE_TYPE_INDEX, &body),
            Some(Ok(answer))
        );
    }

    #[test]
    fn every_season_call_writes_what_it_reads() {
        let header = FacetHeader {
            target: 0xe5fe_5c07_dc18_f1c3,
            entity: 0xe34e_14f3_44a9_74cc,
        };
        let calls = [
            SeasonsCall::StampSquare {
                header,
                card: 29,
                square: [0x11, 1, 0, 0],
            },
            SeasonsCall::NewCard { header },
            SeasonsCall::Reroll { header },
            SeasonsCall::Redeem {
                header,
                reward: 0xa17b_8b9a_2221_5ad9,
            },
        ];
        for call in calls {
            assert_eq!(
                SeasonsCall::decode(call.type_index(), &call.encode()),
                Some(Ok(call))
            );
        }
    }

    #[test]
    fn calls_without_arguments_have_no_body_after_the_header() {
        for type_index in [NEW_CARD_TYPE_INDEX, REROLL_TYPE_INDEX] {
            decode_call(type_index, &hex_bytes(HEADER));
            assert!(matches!(
                SeasonsCall::decode(type_index, &hex_bytes(&format!("{HEADER}00"))),
                Some(Err(_))
            ));
        }
    }

    #[test]
    fn the_first_season_rewards_state_is_retails_with_our_list_version() {
        /// Retail newchar 2026-09-18's body at 9.06 s, and the list version in it.
        const RETAIL: &str = "01ff01000106010001000106010002000106010000000015abb91fb735dd8a146993160a64eae98b199d1dcef33830039475c5d294927dffe372f544acd4dbae4fc7ba4817226ab751b14f10843f0095e4f889708e2b3b0d0d7c94686e9a2d5838c08adedbd3eb38f92c0b9bff000a000318e0ff8560030000ff0001010001060222215ad9c228718c370001060604000400000000010602000000000001a4a3000000000000000000010604000000ffffffffffffffffe0000000000000000000000fff00001100df006e0000010604000000000000000000000000000000000000000000000000000000000000000018703ee352008884";
        const RETAIL_VERSION: u8 = 6;
        const VERSION_AT: [usize; 8] = [5, 11, 17, 127, 140, 150, 170, 206];
        let mut expected = hex_bytes(RETAIL);
        for at in VERSION_AT {
            assert_eq!(expected[at], RETAIL_VERSION);
            expected[at] = u8::try_from(crate::items::FIRST_VERSION).expect("one byte");
        }
        let member = seasons_rewards_initialized();
        assert_eq!(member.component, SEASONS_REWARDS_COMPONENT);
        assert_eq!(
            member.state,
            MemberState::Recorded {
                class: SEASONS_REWARDS_TYPE_INDEX,
                body: expected,
            }
        );
    }

    #[test]
    fn the_first_tracked_stats_state_is_retails_with_our_list_version() {
        /// Retail newchar 2026-09-18's body at 9.06 s, and the list version in it.
        const RETAIL: &str = "011f010001060222215ad98d0024e900010602001e001500010633001400220027004f00530069006e0075007e0089008e009f00a200aa00b300b900bd00d700df00fd0102010d01230132014701480150015c015f0175001f00400048004e0058005c006a008c009400bf00da00ee00f100fc0109011a014001420151015b01750001063300000000000000000000000000000000000000b400000000000000000000000000000000000000000000000c000000b7000000050000000000000002000000000000000a0000000000000001000001ba000000000000000a000000160000018a0000000100000000000000000000000000000000000000000000028a0000001200000000000000060000000600000000000000000000000000000000000000000000000000000000000000cf0000000000000000000000190000000000000000000000000000000000000000";
        const RETAIL_VERSION: u8 = 6;
        const VERSION_AT: [usize; 4] = [5, 17, 25, 131];
        let mut expected = hex_bytes(RETAIL);
        for at in VERSION_AT {
            assert_eq!(expected[at], RETAIL_VERSION);
            expected[at] = u8::try_from(crate::items::FIRST_VERSION).expect("one byte");
        }
        let member = seasons_tracked_stats_initialized();
        assert_eq!(member.component, SEASONS_TRACKED_STATS_COMPONENT);
        assert_eq!(
            member.state,
            MemberState::Recorded {
                class: SEASONS_TRACKED_STATS_TYPE_INDEX,
                body: expected,
            }
        );
    }
}
