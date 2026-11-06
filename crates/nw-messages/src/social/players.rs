// SPDX-License-Identifier: AGPL-3.0-or-later
//! `SocialComponent` calls and pushes other than the player data look-up (4150 and 4140 are
//! [`crate::player_data`]) and the titles. Layouts from the retail sessions `Hive Social`
//! (blocks, mutes, a report, 22 phase look-ups), `dungeon_dynasty_tank_` (searches, friend
//! requests), `Hive 3v3 group`, `arvaysa_raid`, `invasion` and `msq14` (phase data, online
//! states), and the login of every session (4142, 6444, 2507).
//!
//! A search (1185) is answered like a look-up: a 4140 with the search's request id, or a 1926
//! with reason [`FAILURE_NO_DATA`] when nothing matched (retail: five searches, three answers,
//! two failures). Retail also answered a look-up of an unknown character with that 1926.

use crate::appearance::PlayerIconData;
use crate::facet::FacetHeader;
use crate::player_data::PlayerDataResponseMsg;
use crate::social::text::{CharacterRef, read_text, write_text};
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer};

/// `SocialComponentServerFacet_RequestSearchPlayers`.
pub const SEARCH_PLAYERS_TYPE_INDEX: u64 = 1185;
/// `SocialComponentServerFacet_RequestPlayerPhaseId`.
pub const PLAYER_PHASE_ID_TYPE_INDEX: u64 = 6454;
/// `SocialComponentServerFacet_RequestPlayerPhaseData`.
pub const PLAYER_PHASE_DATA_TYPE_INDEX: u64 = 6676;
/// `SocialComponentServerFacet_RequestStreamerModeDataSync`.
pub const STREAMER_MODE_SYNC_TYPE_INDEX: u64 = 4142;
/// `SocialComponentServerFacet_RequestCrossplayPermissionsDataSync`.
pub const CROSSPLAY_SYNC_TYPE_INDEX: u64 = 6444;
/// `SocialComponentServerFacet_RequestSetChattingState`.
pub const SET_CHATTING_STATE_TYPE_INDEX: u64 = 2507;
/// `SocialComponentServerFacet_SetSocialBlock`.
pub const SET_SOCIAL_BLOCK_TYPE_INDEX: u64 = 2799;
/// `SocialComponentServerFacet_RequestClearSocialBlock`.
pub const CLEAR_SOCIAL_BLOCK_TYPE_INDEX: u64 = 2770;
/// `SocialComponentServerFacet_RequestReportPlayer`.
pub const REPORT_PLAYER_TYPE_INDEX: u64 = 1600;
/// `SocialComponentServerFacet_RequestFriendStatusChangeFromClient`.
pub const FRIEND_STATUS_CHANGE_TYPE_INDEX: u64 = 6533;
/// `SocialComponentClientFacet_PlayerDataResponse` (the answer to a search).
pub const PLAYER_DATA_RESPONSE_TYPE_INDEX: u64 =
    crate::player_data::PLAYER_DATA_RESPONSE_TYPE_INDEX;
/// `SocialComponentClientFacet_OnRequestFailed`.
pub const REQUEST_FAILED_TYPE_INDEX: u64 = 1926;
/// `SocialComponentClientFacet_OnPlayerOnlineStateChange`.
pub const ONLINE_STATE_CHANGE_TYPE_INDEX: u64 = 4128;
/// `SocialComponentClientFacet_PlayersPhaseIdResponse`.
pub const PHASE_ID_RESPONSE_TYPE_INDEX: u64 = 6453;
/// `SocialComponentClientFacet_PlayersPhaseDataResponse`.
pub const PHASE_DATA_RESPONSE_TYPE_INDEX: u64 = 6675;

/// The players calls of the client.
pub(crate) const CALL_TYPES: [u64; 10] = [
    SEARCH_PLAYERS_TYPE_INDEX,
    PLAYER_PHASE_ID_TYPE_INDEX,
    PLAYER_PHASE_DATA_TYPE_INDEX,
    STREAMER_MODE_SYNC_TYPE_INDEX,
    CROSSPLAY_SYNC_TYPE_INDEX,
    SET_CHATTING_STATE_TYPE_INDEX,
    SET_SOCIAL_BLOCK_TYPE_INDEX,
    CLEAR_SOCIAL_BLOCK_TYPE_INDEX,
    REPORT_PLAYER_TYPE_INDEX,
    FRIEND_STATUS_CHANGE_TYPE_INDEX,
];

/// The 1926 reason of a look-up or search that found no player (all 52 retail 1926).
pub const FAILURE_NO_DATA: u32 = 2;
/// 4128 state: the player went offline (retail 4128 states 0, 1 and 2).
pub const ONLINE_STATE_OFFLINE: u8 = 0;
/// 4128 state: the player came online.
pub const ONLINE_STATE_ONLINE: u8 = 1;
/// The trailing byte of every retail 6453 (211 of 211).
pub const PHASE_ID_TRAILER: u8 = 0;
/// Bytes of the 6444 crossplay permissions.
const CROSSPLAY_LEN: usize = 10;
/// Bytes of the 4142 streamer-mode settings.
const STREAMER_MODE_LEN: usize = 2;

/// Bytes of a player icon on the wire (seven look bytes and a big-endian `u16` background).
pub const ICON_LEN: usize = 9;

/// The wire bytes of `icon`.
pub fn icon_bytes(icon: &PlayerIconData) -> [u8; ICON_LEN] {
    let mut writer = Writer::new();
    icon.write(&mut writer);
    let mut bytes = [0; ICON_LEN];
    bytes.copy_from_slice(&writer.finish());
    bytes
}

/// The icon of its wire bytes.
pub fn icon_of_bytes(bytes: &[u8; ICON_LEN]) -> PlayerIconData {
    // 9 bytes always read as an icon: every field is a fixed-size number.
    PlayerIconData::read(&mut Reader::new(bytes)).unwrap_or_default()
}

/// The client's `FriendRequestTypes` (`FUN_143f107d0` lists the names in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FriendRequest {
    /// 1: invite a player.
    Invite,
    /// 2: accept an invite.
    Accept,
    /// 3: reject an invite.
    Reject,
    /// 4: remove a friend or a sent invite.
    Remove,
}

impl FriendRequest {
    /// The request of wire value `value`; `None` for `Invalid` (0) and unknown values.
    pub const fn from_wire(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Invite),
            2 => Some(Self::Accept),
            3 => Some(Self::Reject),
            4 => Some(Self::Remove),
            _ => None,
        }
    }
}

/// A players call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerCall {
    /// 1185: search players by the start of their name.
    Search {
        /// The request id; the answer repeats it.
        request: [u8; RAW_ID_LEN],
        /// The start of the name.
        name: String,
        /// The most results (25 in all retail searches).
        limit: u32,
    },
    /// 6454: the phase a player is in.
    PhaseId {
        /// The request id.
        request: [u8; RAW_ID_LEN],
        /// The character id as UUID text.
        character: String,
    },
    /// 6676: the phase data of a player.
    PhaseData {
        /// The request id.
        request: [u8; RAW_ID_LEN],
        /// The character id as UUID text.
        character: String,
    },
    /// 4142: the streamer-mode settings (`00 00` in every retail call; meaning Unknown).
    StreamerMode {
        /// The two setting bytes.
        settings: [u8; STREAMER_MODE_LEN],
    },
    /// 6444: the crossplay permissions (`01 01` and eight zero bytes in every retail call).
    Crossplay {
        /// The ten permission bytes.
        permissions: [u8; CROSSPLAY_LEN],
    },
    /// 2507: the player types in a chat channel (1 in every retail call).
    ChattingState {
        /// The state value.
        state: u32,
    },
    /// 2799: block a player.
    Block {
        /// The character id as UUID text.
        character: String,
    },
    /// 2770: unblock a player.
    Unblock {
        /// The character id as UUID text.
        character: String,
    },
    /// 1600: report a player.
    Report {
        /// The report reason (4 in both retail reports).
        reason: u32,
        /// The character id as UUID text.
        character: String,
        /// The player's comment (empty in the retail reports).
        comment: String,
        /// The reported player's name.
        name: String,
        /// The report category (2 in the retail reports).
        category: u32,
        /// A second text (empty in the retail reports).
        detail: String,
        /// The client's locale (`en-US` in the retail reports).
        locale: String,
    },
    /// 6533: a friend request ([`FriendRequest::from_wire`]).
    Friend {
        /// The request value.
        request: u32,
        /// The character id as UUID text.
        character: String,
    },
}

impl PlayerCall {
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let call = match type_index {
            SEARCH_PLAYERS_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Search {
                    request: reader.raw_id()?,
                    name: read_text(reader)?,
                    limit: reader.u32()?,
                })
            })(),
            PLAYER_PHASE_ID_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::PhaseId {
                    request: reader.raw_id()?,
                    character: read_text(reader)?,
                })
            })(),
            PLAYER_PHASE_DATA_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::PhaseData {
                    request: reader.raw_id()?,
                    character: read_text(reader)?,
                })
            })(),
            STREAMER_MODE_SYNC_TYPE_INDEX => reader
                .array()
                .map(|settings| Self::StreamerMode { settings }),
            CROSSPLAY_SYNC_TYPE_INDEX => reader
                .array()
                .map(|permissions| Self::Crossplay { permissions }),
            SET_CHATTING_STATE_TYPE_INDEX => {
                reader.u32().map(|state| Self::ChattingState { state })
            }
            SET_SOCIAL_BLOCK_TYPE_INDEX => {
                read_text(reader).map(|character| Self::Block { character })
            }
            CLEAR_SOCIAL_BLOCK_TYPE_INDEX => {
                read_text(reader).map(|character| Self::Unblock { character })
            }
            REPORT_PLAYER_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Report {
                    reason: reader.u32()?,
                    character: read_text(reader)?,
                    comment: read_text(reader)?,
                    name: read_text(reader)?,
                    category: reader.u32()?,
                    detail: read_text(reader)?,
                    locale: read_text(reader)?,
                })
            })(),
            FRIEND_STATUS_CHANGE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Friend {
                    request: reader.u32()?,
                    character: read_text(reader)?,
                })
            })(),
            _ => return None,
        };
        Some(call)
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Search { .. } => SEARCH_PLAYERS_TYPE_INDEX,
            Self::PhaseId { .. } => PLAYER_PHASE_ID_TYPE_INDEX,
            Self::PhaseData { .. } => PLAYER_PHASE_DATA_TYPE_INDEX,
            Self::StreamerMode { .. } => STREAMER_MODE_SYNC_TYPE_INDEX,
            Self::Crossplay { .. } => CROSSPLAY_SYNC_TYPE_INDEX,
            Self::ChattingState { .. } => SET_CHATTING_STATE_TYPE_INDEX,
            Self::Block { .. } => SET_SOCIAL_BLOCK_TYPE_INDEX,
            Self::Unblock { .. } => CLEAR_SOCIAL_BLOCK_TYPE_INDEX,
            Self::Report { .. } => REPORT_PLAYER_TYPE_INDEX,
            Self::Friend { .. } => FRIEND_STATUS_CHANGE_TYPE_INDEX,
        }
    }

    /// The body of the call to the social facet `header`, as [`crate::social::SocialCall::decode`]
    /// reads it (a test client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text does not fit its prefix.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        header.write(&mut writer);
        match self {
            Self::Search {
                request,
                name,
                limit,
            } => {
                writer.bytes(request);
                write_text(&mut writer, name)?;
                writer.u32(*limit);
            }
            Self::PhaseId { request, character } | Self::PhaseData { request, character } => {
                writer.bytes(request);
                write_text(&mut writer, character)?;
            }
            Self::StreamerMode { settings } => {
                writer.bytes(settings);
            }
            Self::Crossplay { permissions } => {
                writer.bytes(permissions);
            }
            Self::ChattingState { state } => {
                writer.u32(*state);
            }
            Self::Block { character } | Self::Unblock { character } => {
                write_text(&mut writer, character)?;
            }
            Self::Report {
                reason,
                character,
                comment,
                name,
                category,
                detail,
                locale,
            } => {
                writer.u32(*reason);
                write_text(&mut writer, character)?;
                write_text(&mut writer, comment)?;
                write_text(&mut writer, name)?;
                writer.u32(*category);
                write_text(&mut writer, detail)?;
                write_text(&mut writer, locale)?;
            }
            Self::Friend { request, character } => {
                writer.u32(*request);
                write_text(&mut writer, character)?;
            }
        }
        Ok(writer.finish())
    }
}

/// 1926: a look-up or search found no player (every retail 1926 has [`FAILURE_NO_DATA`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFailedMsg {
    /// The header of the call.
    pub header: FacetHeader,
    /// The request id of the call.
    pub request: [u8; RAW_ID_LEN],
    /// The reason.
    pub reason: u32,
}

impl RequestFailedMsg {
    fn write_args(&self, writer: &mut Writer) {
        writer.bytes(&self.request).u32(self.reason);
    }

    fn read_args(header: FacetHeader, reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            header,
            request: reader.raw_id()?,
            reason: reader.u32()?,
        })
    }
}

impl BodyCodec for RequestFailedMsg {
    const TYPE_INDEX: u64 = REQUEST_FAILED_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        self.write_args(&mut writer);
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let message = Self::read_args(header, &mut reader)?;
        reader.finish()?;
        Ok(message)
    }
}

/// A players push of the server.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerPush {
    /// 4140: the players a search found (the header of the message is not used).
    SearchResults(PlayerDataResponseMsg),
    /// 1926: a look-up or search failed (the header of the message is not used).
    RequestFailed(RequestFailedMsg),
    /// 4128: a friend, company or group member came online or went offline.
    OnlineState {
        /// The character id as UUID text.
        character: String,
        /// [`ONLINE_STATE_ONLINE`] or [`ONLINE_STATE_OFFLINE`].
        state: u8,
    },
    /// 6453: the phase a player is in (no request id; retail answered each 6454 with the
    /// character's id).
    PhaseId {
        /// The character.
        character: CharacterRef,
        /// The phase id: 0 for the open world (retail: 0 outside instances).
        phase: u64,
        /// [`PHASE_ID_TRAILER`].
        trailer: u8,
    },
    /// 6675: the phase data of a player (retail: all zero for a player in no phase).
    PhaseData {
        /// The character.
        character: CharacterRef,
        /// A state byte (0, 1 or 2 in retail; meaning Unknown).
        state: u8,
        /// A first count (meaning Unknown).
        first: u16,
        /// A second count (meaning Unknown).
        second: u16,
        /// The phase or sector id.
        sector: u64,
    },
}

/// The 16 bytes of a facet header in front of the arguments of [`PlayerDataResponseMsg`].
const HEADER_LEN: usize = 16;
/// The header a push holds when it is read from its arguments.
const UNUSED_HEADER: FacetHeader = FacetHeader {
    target: 0,
    entity: 0,
};

impl PlayerPush {
    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::SearchResults(_) => PLAYER_DATA_RESPONSE_TYPE_INDEX,
            Self::RequestFailed(_) => REQUEST_FAILED_TYPE_INDEX,
            Self::OnlineState { .. } => ONLINE_STATE_CHANGE_TYPE_INDEX,
            Self::PhaseId { .. } => PHASE_ID_RESPONSE_TYPE_INDEX,
            Self::PhaseData { .. } => PHASE_DATA_RESPONSE_TYPE_INDEX,
        }
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::SearchResults(results) => {
                let body = results.encode()?;
                writer.bytes(&body[HEADER_LEN..]);
            }
            Self::RequestFailed(failed) => failed.write_args(writer),
            Self::OnlineState { character, state } => {
                write_text(writer, character)?;
                writer.u8(*state);
            }
            Self::PhaseId {
                character,
                phase,
                trailer,
            } => {
                character.write(writer);
                writer.u64(*phase).u8(*trailer);
            }
            Self::PhaseData {
                character,
                state,
                first,
                second,
                sector,
            } => {
                character.write(writer);
                writer.u8(*state).u16(*first).u16(*second).u64(*sector);
            }
        }
        Ok(())
    }

    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let push = match type_index {
            PLAYER_DATA_RESPONSE_TYPE_INDEX => {
                let mut body = vec![0; HEADER_LEN];
                body.extend_from_slice(reader.rest());
                let results = PlayerDataResponseMsg::decode(&body).map(Self::SearchResults);
                if results.is_ok() {
                    // The whole rest was read.
                    let rest = reader.remaining();
                    if let Err(error) = reader.bytes(rest) {
                        return Some(Err(error));
                    }
                }
                results
            }
            REQUEST_FAILED_TYPE_INDEX => {
                RequestFailedMsg::read_args(UNUSED_HEADER, reader).map(Self::RequestFailed)
            }
            ONLINE_STATE_CHANGE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::OnlineState {
                    character: read_text(reader)?,
                    state: reader.u8()?,
                })
            })(),
            PHASE_ID_RESPONSE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::PhaseId {
                    character: CharacterRef::read(reader)?,
                    phase: reader.u64()?,
                    trailer: reader.u8()?,
                })
            })(),
            PHASE_DATA_RESPONSE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::PhaseData {
                    character: CharacterRef::read(reader)?,
                    state: reader.u8()?,
                    first: reader.u16()?,
                    second: reader.u16()?,
                    sector: reader.u64()?,
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

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn hex_of(text: &str) -> String {
        text.bytes().map(|byte| format!("{byte:02x}")).collect()
    }

    const HEADER: &str = "c2a7dec3a310d6b4b3ac442d2430908c";
    const REQUEST: &str = "c035bb6b0d984dfcb658da0273ed6916";
    const CHARACTER: &str = "00000000-0000-4000-8000-000000000002";

    /// The call of the retail body of `type_index` with arguments `args`; its encode gives the
    /// same body back.
    fn call(type_index: u64, args: &str) -> PlayerCall {
        let body = hex(&format!("{HEADER}{args}"));
        let (header, call) = SocialCall::decode(type_index, &body)
            .expect("a social call")
            .expect("reads");
        assert_eq!(header.target, 0xc2a7_dec3_a310_d6b4);
        assert_eq!(call.type_index(), type_index);
        assert_eq!(call.encode(header).expect("encode"), body);
        let SocialCall::Player(call) = call else {
            panic!("a players call: {call:?}");
        };
        call
    }

    /// The retail forms (dungeon_dynasty_tank_, Hive Social, every login), names and ids
    /// replaced.
    #[test]
    fn the_retail_calls_read() {
        assert_eq!(
            call(
                SEARCH_PLAYERS_TYPE_INDEX,
                &format!("{REQUEST}05{}00000019", hex_of("arvay"))
            ),
            PlayerCall::Search {
                request: hex(REQUEST).try_into().expect("16 bytes"),
                name: "arvay".to_owned(),
                limit: 25,
            }
        );
        assert_eq!(
            call(STREAMER_MODE_SYNC_TYPE_INDEX, "0000"),
            PlayerCall::StreamerMode { settings: [0, 0] }
        );
        assert_eq!(
            call(CROSSPLAY_SYNC_TYPE_INDEX, "01010000000000000000"),
            PlayerCall::Crossplay {
                permissions: [1, 1, 0, 0, 0, 0, 0, 0, 0, 0]
            }
        );
        assert_eq!(
            call(SET_CHATTING_STATE_TYPE_INDEX, "00000001"),
            PlayerCall::ChattingState { state: 1 }
        );
        assert_eq!(
            call(
                FRIEND_STATUS_CHANGE_TYPE_INDEX,
                &format!("0000000124{}", hex_of(CHARACTER))
            ),
            PlayerCall::Friend {
                request: 1,
                character: CHARACTER.to_owned(),
            }
        );
        assert_eq!(FriendRequest::from_wire(1), Some(FriendRequest::Invite));
        assert_eq!(
            call(
                REPORT_PLAYER_TYPE_INDEX,
                &format!(
                    "0000000424{}0003{}000000020005{}",
                    hex_of(CHARACTER),
                    hex_of("Bob"),
                    hex_of("en-US")
                )
            ),
            PlayerCall::Report {
                reason: 4,
                character: CHARACTER.to_owned(),
                comment: String::new(),
                name: "Bob".to_owned(),
                category: 2,
                detail: String::new(),
                locale: "en-US".to_owned(),
            }
        );
        assert_eq!(
            call(
                SET_SOCIAL_BLOCK_TYPE_INDEX,
                &format!("24{}", hex_of(CHARACTER))
            ),
            PlayerCall::Block {
                character: CHARACTER.to_owned()
            }
        );
        assert_eq!(
            call(
                PLAYER_PHASE_ID_TYPE_INDEX,
                &format!("{REQUEST}24{}", hex_of(CHARACTER))
            ),
            PlayerCall::PhaseId {
                request: hex(REQUEST).try_into().expect("16 bytes"),
                character: CHARACTER.to_owned(),
            }
        );
    }

    /// The retail answers (Hive Social, dungeon_dynasty_tank_) with the ids replaced.
    #[test]
    fn the_retail_pushes_have_the_retail_bytes() {
        let id = [0x11; RAW_ID_LEN];
        let cases = [
            (
                PlayerPush::RequestFailed(RequestFailedMsg {
                    header: UNUSED_HEADER,
                    request: hex(REQUEST).try_into().expect("16 bytes"),
                    reason: FAILURE_NO_DATA,
                }),
                format!("{REQUEST}00000002"),
            ),
            (
                PlayerPush::OnlineState {
                    character: CHARACTER.to_owned(),
                    state: ONLINE_STATE_ONLINE,
                },
                format!("24{}01", hex_of(CHARACTER)),
            ),
            (
                PlayerPush::PhaseId {
                    character: CharacterRef(Some(id)),
                    phase: 0,
                    trailer: PHASE_ID_TRAILER,
                },
                format!("05{}000000000000000000", "11".repeat(RAW_ID_LEN)),
            ),
            (
                PlayerPush::PhaseData {
                    character: CharacterRef(Some(id)),
                    state: 2,
                    first: 0x10,
                    second: 0x0c,
                    sector: 0xa0a6_30c3_6a1f_1268,
                },
                format!("05{}020010000ca0a630c36a1f1268", "11".repeat(RAW_ID_LEN)),
            ),
        ];
        for (push, expected) in cases {
            let push = SocialPush::Player(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(args, hex(&expected));
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
    }

    /// The retail 1926 of `dungeon_dynasty_tank_` (a search that found nobody), whole body.
    #[test]
    fn a_failure_has_the_retail_body() {
        let body = hex(&format!("{HEADER}{REQUEST}00000002"));
        let failed = RequestFailedMsg::decode(&body).expect("decodes");
        assert_eq!(failed.reason, FAILURE_NO_DATA);
        assert_eq!(failed.header.target, 0xc2a7_dec3_a310_d6b4);
        assert_eq!(failed.encode().expect("encodes"), body);
    }

    #[test]
    fn an_icon_round_trips_its_bytes() {
        let bytes = [0, 0, 8, 6, 6, 0, 7, 0, 0x14];
        assert_eq!(icon_bytes(&icon_of_bytes(&bytes)), bytes);
        assert_eq!(icon_of_bytes(&bytes).background_color, 0x14);
    }

    #[test]
    fn search_results_are_a_player_data_answer_without_its_header() {
        let results = PlayerDataResponseMsg {
            header: UNUSED_HEADER,
            request: [3; RAW_ID_LEN],
            players: Vec::new(),
        };
        let push = SocialPush::Player(PlayerPush::SearchResults(results));
        let args = push.encode_args().expect("encode");
        assert_eq!(args, [[3; RAW_ID_LEN].as_slice(), &[0]].concat());
        assert_eq!(
            SocialPush::decode_args(PLAYER_DATA_RESPONSE_TYPE_INDEX, &args),
            Some(Ok(push))
        );
    }

    /// A header of synthetic ids.
    const ROUND_TRIP_HEADER: FacetHeader = FacetHeader {
        target: 0x0102_0304_0506_0708,
        entity: 0x1112_1314_1516_1718,
    };

    #[test]
    fn every_call_encodes_and_reads_back() {
        let calls = [
            PlayerCall::Search {
                request: [1; RAW_ID_LEN],
                name: "arv".to_owned(),
                limit: 25,
            },
            PlayerCall::PhaseId {
                request: [2; RAW_ID_LEN],
                character: CHARACTER.to_owned(),
            },
            PlayerCall::PhaseData {
                request: [3; RAW_ID_LEN],
                character: CHARACTER.to_owned(),
            },
            PlayerCall::StreamerMode { settings: [1, 2] },
            PlayerCall::Crossplay {
                permissions: [1, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            },
            PlayerCall::ChattingState { state: 3 },
            PlayerCall::Block {
                character: CHARACTER.to_owned(),
            },
            PlayerCall::Unblock {
                character: CHARACTER.to_owned(),
            },
            PlayerCall::Report {
                reason: 4,
                character: CHARACTER.to_owned(),
                comment: "spam".to_owned(),
                name: "Bob".to_owned(),
                category: 2,
                detail: "chat".to_owned(),
                locale: "en-US".to_owned(),
            },
            PlayerCall::Friend {
                request: 2,
                character: CHARACTER.to_owned(),
            },
        ]
        .map(SocialCall::Player);
        crate::social::assert_calls_round_trip(ROUND_TRIP_HEADER, &calls);
    }
}
