// SPDX-License-Identifier: AGPL-3.0-or-later
//! Matchmaking and game-mode queue calls of the player's `GameModeParticipantComponent`
//! (retail `opr_que_and_lost`, `3v3_arena_group`, `Hive catacomes` ledgers).
//!
//! Every body starts with the facet header of the component
//! ([`crate::facet::component::GAME_MODE_PARTICIPANT`]). Layouts after the header, from the
//! retail bodies:
//!
//! - 6211: one byte (1 subscribe, 0 unsubscribe; 8 calls, both values).
//! - 6272: one byte, the picked role (1 and 3 in the captures).
//! - 1980: one big-endian `u32`, a client event id (meaning Unknown).
//! - 5741: a text (the match id, a UUID in the one capture) and one byte (1).
//! - 5450 and 5449 and 1234: the header alone.
//! - 6254: a count byte, then per activity three big-endian values (`u16`, `u16`, `u8`); the
//!   first `u16` names the activity (0x3016, 0x3116, 0x3316 in the captures; meaning
//!   Unknown), the other two are Unknown.
//! - 6295: one byte (1), the same activity `u16` as the 6254 it follows, and a `u16` (0).
//! - 6414: a big-endian `u32` (the same value as the 6824 of the same match; meaning
//!   Unknown), then a character reference (tag 5 and 16 bytes, or tag 0 and an empty text).
//! - 6824: the same `u32` as 6414 and one ready byte (1 ready).
//! - 6970: a text, the cutscene name (empty in the one capture).
//! - 1066: the CRC of the game mode (big-endian `u32`) and one byte (1 for a group queue; 4
//!   retail calls, both values).
//! - 3309: two 16-byte values (meaning Unknown; 4 retail calls, each an expedition or arena
//!   left).
//! - 2385: one big-endian `u16` (1 in the one retail call; meaning Unknown).

use crate::combat::{read_string, write_string};
use crate::entity_ids::RAW_ID_LEN;
use crate::facet::FacetHeader;
use crate::social::CharacterRef;
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `GameModeParticipantComponentServerFacet_RequestModifyGroupEligibilityDataSubscription`.
pub const MODIFY_GROUP_ELIGIBILITY_SUBSCRIPTION_TYPE_INDEX: u64 = 6211;
/// Type index of `GameModeParticipantComponentClientFacet_OnQueueGroupRemovedMsg`.
pub const QUEUE_GROUP_REMOVED_TYPE_INDEX: u64 = 1234;
/// Type index of `GameModeParticipantComponentServerFacet_RequestSetMatchmakingRole`.
pub const SET_MATCHMAKING_ROLE_TYPE_INDEX: u64 = 6272;
/// Type index of `GameModeParticipantComponentServerFacet_OnReceivedClientEvent`.
pub const RECEIVED_CLIENT_EVENT_TYPE_INDEX: u64 = 1980;
/// Type index of `GameModeParticipantComponentServerFacet_AcceptMatchmakingServiceMatch`.
pub const ACCEPT_MATCHMAKING_MATCH_TYPE_INDEX: u64 = 5741;
/// Type index of `GameModeParticipantComponentServerFacet_OnMatchmakingLoginConfirmed`.
pub const MATCHMAKING_LOGIN_CONFIRMED_TYPE_INDEX: u64 = 5450;
/// Type index of `GameModeParticipantComponentClientFacet_OnMatchmakingLoginRequested`.
pub const MATCHMAKING_LOGIN_REQUESTED_TYPE_INDEX: u64 = 5449;
/// Type index of `GameModeParticipantComponentServerFacet_RequestJoinQueueForActivities`.
pub const JOIN_QUEUE_FOR_ACTIVITIES_TYPE_INDEX: u64 = 6254;
/// Type index of `GameModeParticipantComponentClientFacet_OnStopCutsceneWithName`.
pub const STOP_CUTSCENE_WITH_NAME_TYPE_INDEX: u64 = 6970;
/// Type index of `GameModeParticipantComponentClientFacet_OnStartMatchmakingRoleSelection`.
pub const START_MATCHMAKING_ROLE_SELECTION_TYPE_INDEX: u64 = 6295;
/// Type index of `GameModeParticipantComponentClientFacet_OnGameModePlayerRemovedUpdate`.
pub const GAME_MODE_PLAYER_REMOVED_TYPE_INDEX: u64 = 6414;
/// Type index of `GameModeParticipantComponentServerFacet_SetReadiedUpStatus`.
pub const SET_READIED_UP_STATUS_TYPE_INDEX: u64 = 6824;
/// Type index of `GameModeParticipantComponentServerFacet_RequestLeaveQueueForGameMode`.
pub const LEAVE_QUEUE_FOR_GAME_MODE_TYPE_INDEX: u64 = 1066;
/// Type index of `GameModeParticipantComponentServerFacet_RequestLeaveGameMode`.
pub const LEAVE_GAME_MODE_TYPE_INDEX: u64 = 3309;
/// Type index of `GameModeParticipantComponentServerFacet_RequestLeaveQueueForAllGameModes`.
pub const LEAVE_QUEUE_FOR_ALL_GAME_MODES_TYPE_INDEX: u64 = 2385;

/// The most activities of one 6254 (a body is at most `0x30000` bytes and an entry 5).
const MAX_ACTIVITIES: usize = 0x1_0000;
/// Bytes of one queued activity after the header: `u16`, `u16`, `u8`.
const ACTIVITY_BYTES: usize = 5;

/// One activity of a 6254 queue request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueActivity {
    /// The activity (0x3016, 0x3116, 0x3316 in retail; meaning Unknown).
    pub activity: u16,
    /// A value of Unknown meaning (0 and 1 in retail).
    pub extra: u16,
    /// A byte of Unknown meaning (0 and 1 in retail).
    pub flag: u8,
}

/// A matchmaking call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameModeClientMsg {
    /// 6211: start (1) or stop (0) the group eligibility updates.
    ModifyGroupEligibilitySubscription {
        /// The header.
        header: FacetHeader,
        /// Whether the updates are wanted.
        subscribed: bool,
    },
    /// 6272: the role the player picked.
    SetMatchmakingRole {
        /// The header.
        header: FacetHeader,
        /// The role (1 and 3 in retail).
        role: u8,
    },
    /// 1980: a client event in a game mode.
    ReceivedClientEvent {
        /// The header.
        header: FacetHeader,
        /// The event id (meaning Unknown).
        event: u32,
    },
    /// 5741: the player accepted (last byte 1 in retail) the found match `match_id`.
    AcceptMatch {
        /// The header.
        header: FacetHeader,
        /// The match id as text (a UUID in retail).
        match_id: String,
        /// The last byte (1 in retail; meaning Unknown).
        flag: u8,
    },
    /// 5450: the client signed in to the matchmaking service.
    MatchmakingLoginConfirmed {
        /// The header.
        header: FacetHeader,
    },
    /// 6254: join the queue for these activities.
    JoinQueueForActivities {
        /// The header.
        header: FacetHeader,
        /// The ticked activities.
        activities: Vec<QueueActivity>,
    },
    /// 6824: the player readied up (or unreadied) for match `match_id`.
    SetReadiedUpStatus {
        /// The header.
        header: FacetHeader,
        /// The match (the same value as in the 6414 of the match; meaning Unknown).
        match_id: u32,
        /// Whether the player is ready.
        ready: bool,
    },
    /// 1066: leave the queue of one game mode.
    LeaveQueueForGameMode {
        /// The header.
        header: FacetHeader,
        /// The CRC of the game mode.
        game_mode: u32,
        /// Whether the player queued with its group.
        group: bool,
    },
    /// 3309: leave the game mode (expedition, trial or arena) the player is in.
    LeaveGameMode {
        /// The header.
        header: FacetHeader,
        /// The first 16 bytes (meaning Unknown).
        first: [u8; RAW_ID_LEN],
        /// The second 16 bytes (meaning Unknown).
        second: [u8; RAW_ID_LEN],
    },
    /// 2385: leave every queue (retail: as the group enters its expedition).
    LeaveQueueForAllGameModes {
        /// The header.
        header: FacetHeader,
        /// One value (1 in the one retail call; meaning Unknown).
        flags: u16,
    },
}

impl GameModeClientMsg {
    /// Decodes `body` when `type_index` is a matchmaking call of the client; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            MODIFY_GROUP_ELIGIBILITY_SUBSCRIPTION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ModifyGroupEligibilitySubscription {
                    header,
                    subscribed: reader.boolean()?,
                })
            }),
            SET_MATCHMAKING_ROLE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetMatchmakingRole {
                    header,
                    role: reader.u8()?,
                })
            }),
            RECEIVED_CLIENT_EVENT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ReceivedClientEvent {
                    header,
                    event: reader.u32()?,
                })
            }),
            ACCEPT_MATCHMAKING_MATCH_TYPE_INDEX => read(|header, reader| {
                Ok(Self::AcceptMatch {
                    header,
                    match_id: String::from_utf8_lossy(&read_string(reader)?).into_owned(),
                    flag: reader.u8()?,
                })
            }),
            MATCHMAKING_LOGIN_CONFIRMED_TYPE_INDEX => {
                read(|header, _| Ok(Self::MatchmakingLoginConfirmed { header }))
            }
            JOIN_QUEUE_FOR_ACTIVITIES_TYPE_INDEX => read(|header, reader| {
                let count = usize::from(reader.u8()?);
                if count > MAX_ACTIVITIES || count * ACTIVITY_BYTES > reader.remaining() {
                    return Err(MessageError::CountTooLarge {
                        count: count as u64,
                        limit: (reader.remaining() / ACTIVITY_BYTES) as u64,
                    });
                }
                let activities = (0..count)
                    .map(|_| {
                        Ok(QueueActivity {
                            activity: reader.u16()?,
                            extra: reader.u16()?,
                            flag: reader.u8()?,
                        })
                    })
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::JoinQueueForActivities { header, activities })
            }),
            SET_READIED_UP_STATUS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetReadiedUpStatus {
                    header,
                    match_id: reader.u32()?,
                    ready: reader.boolean()?,
                })
            }),
            LEAVE_QUEUE_FOR_GAME_MODE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::LeaveQueueForGameMode {
                    header,
                    game_mode: reader.u32()?,
                    group: reader.boolean()?,
                })
            }),
            LEAVE_GAME_MODE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::LeaveGameMode {
                    header,
                    first: reader.raw_id()?,
                    second: reader.raw_id()?,
                })
            }),
            LEAVE_QUEUE_FOR_ALL_GAME_MODES_TYPE_INDEX => read(|header, reader| {
                Ok(Self::LeaveQueueForAllGameModes {
                    header,
                    flags: reader.u16()?,
                })
            }),
            _ => return None,
        })
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::ModifyGroupEligibilitySubscription { .. } => {
                MODIFY_GROUP_ELIGIBILITY_SUBSCRIPTION_TYPE_INDEX
            }
            Self::SetMatchmakingRole { .. } => SET_MATCHMAKING_ROLE_TYPE_INDEX,
            Self::ReceivedClientEvent { .. } => RECEIVED_CLIENT_EVENT_TYPE_INDEX,
            Self::AcceptMatch { .. } => ACCEPT_MATCHMAKING_MATCH_TYPE_INDEX,
            Self::MatchmakingLoginConfirmed { .. } => MATCHMAKING_LOGIN_CONFIRMED_TYPE_INDEX,
            Self::JoinQueueForActivities { .. } => JOIN_QUEUE_FOR_ACTIVITIES_TYPE_INDEX,
            Self::SetReadiedUpStatus { .. } => SET_READIED_UP_STATUS_TYPE_INDEX,
            Self::LeaveQueueForGameMode { .. } => LEAVE_QUEUE_FOR_GAME_MODE_TYPE_INDEX,
            Self::LeaveGameMode { .. } => LEAVE_GAME_MODE_TYPE_INDEX,
            Self::LeaveQueueForAllGameModes { .. } => LEAVE_QUEUE_FOR_ALL_GAME_MODES_TYPE_INDEX,
        }
    }

    /// The header of the call.
    pub const fn header(&self) -> FacetHeader {
        match self {
            Self::ModifyGroupEligibilitySubscription { header, .. }
            | Self::SetMatchmakingRole { header, .. }
            | Self::ReceivedClientEvent { header, .. }
            | Self::AcceptMatch { header, .. }
            | Self::MatchmakingLoginConfirmed { header }
            | Self::JoinQueueForActivities { header, .. }
            | Self::SetReadiedUpStatus { header, .. }
            | Self::LeaveQueueForGameMode { header, .. }
            | Self::LeaveGameMode { header, .. }
            | Self::LeaveQueueForAllGameModes { header, .. } => *header,
        }
    }

    /// The body bytes, as the client sends them.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text is too long or the activity list too large.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header().write(&mut writer);
        match self {
            Self::ModifyGroupEligibilitySubscription { subscribed, .. } => {
                writer.boolean(*subscribed);
            }
            Self::SetMatchmakingRole { role, .. } => {
                writer.u8(*role);
            }
            Self::ReceivedClientEvent { event, .. } => {
                writer.u32(*event);
            }
            Self::AcceptMatch { match_id, flag, .. } => {
                write_string(&mut writer, match_id.as_bytes())?;
                writer.u8(*flag);
            }
            Self::MatchmakingLoginConfirmed { .. } => {}
            Self::JoinQueueForActivities { activities, .. } => {
                let count =
                    u8::try_from(activities.len()).map_err(|_| MessageError::CountTooLarge {
                        count: activities.len() as u64,
                        limit: u64::from(u8::MAX),
                    })?;
                writer.u8(count);
                for activity in activities {
                    writer
                        .u16(activity.activity)
                        .u16(activity.extra)
                        .u8(activity.flag);
                }
            }
            Self::SetReadiedUpStatus {
                match_id, ready, ..
            } => {
                writer.u32(*match_id).boolean(*ready);
            }
            Self::LeaveQueueForGameMode {
                game_mode, group, ..
            } => {
                writer.u32(*game_mode).boolean(*group);
            }
            Self::LeaveGameMode { first, second, .. } => {
                writer.bytes(first).bytes(second);
            }
            Self::LeaveQueueForAllGameModes { flags, .. } => {
                writer.u16(*flags);
            }
        }
        Ok(writer.finish())
    }
}

/// A matchmaking push of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameModeServerMsg {
    /// 1234: the group the player queued with was removed from the queue.
    QueueGroupRemoved {
        /// The header.
        header: FacetHeader,
    },
    /// 5449: sign in to the matchmaking service.
    MatchmakingLoginRequested {
        /// The header.
        header: FacetHeader,
    },
    /// 6295: open the role picking screen for this activity.
    StartMatchmakingRoleSelection {
        /// The header.
        header: FacetHeader,
        /// The first byte (1 in retail; meaning Unknown).
        flag: u8,
        /// The activity, as in the 6254 it follows.
        activity: u16,
        /// A value of Unknown meaning (0 in retail).
        extra: u16,
    },
    /// 6414: a player was removed from the game mode.
    GameModePlayerRemoved {
        /// The header.
        header: FacetHeader,
        /// The match (the same value as in a 6824 of the match; meaning Unknown).
        match_id: u32,
        /// The removed player.
        player: CharacterRef,
    },
    /// 6970: stop the cutscene of this name (empty in retail).
    StopCutsceneWithName {
        /// The header.
        header: FacetHeader,
        /// The cutscene name.
        name: String,
    },
}

impl GameModeServerMsg {
    /// Whether `type_index` is a matchmaking push of the server.
    pub const fn is_type(type_index: u64) -> bool {
        matches!(
            type_index,
            QUEUE_GROUP_REMOVED_TYPE_INDEX
                | MATCHMAKING_LOGIN_REQUESTED_TYPE_INDEX
                | START_MATCHMAKING_ROLE_SELECTION_TYPE_INDEX
                | GAME_MODE_PLAYER_REMOVED_TYPE_INDEX
                | STOP_CUTSCENE_WITH_NAME_TYPE_INDEX
        )
    }

    /// The type index of the push.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::QueueGroupRemoved { .. } => QUEUE_GROUP_REMOVED_TYPE_INDEX,
            Self::MatchmakingLoginRequested { .. } => MATCHMAKING_LOGIN_REQUESTED_TYPE_INDEX,
            Self::StartMatchmakingRoleSelection { .. } => {
                START_MATCHMAKING_ROLE_SELECTION_TYPE_INDEX
            }
            Self::GameModePlayerRemoved { .. } => GAME_MODE_PLAYER_REMOVED_TYPE_INDEX,
            Self::StopCutsceneWithName { .. } => STOP_CUTSCENE_WITH_NAME_TYPE_INDEX,
        }
    }

    /// The header of the push.
    pub const fn header(&self) -> FacetHeader {
        match self {
            Self::QueueGroupRemoved { header }
            | Self::MatchmakingLoginRequested { header }
            | Self::StartMatchmakingRoleSelection { header, .. }
            | Self::GameModePlayerRemoved { header, .. }
            | Self::StopCutsceneWithName { header, .. } => *header,
        }
    }

    /// Decodes the body of the push of `type_index`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body does not match its layout or `type_index` is not
    /// a matchmaking push.
    pub fn decode(type_index: u64, body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let message = match type_index {
            QUEUE_GROUP_REMOVED_TYPE_INDEX => Self::QueueGroupRemoved { header },
            MATCHMAKING_LOGIN_REQUESTED_TYPE_INDEX => Self::MatchmakingLoginRequested { header },
            START_MATCHMAKING_ROLE_SELECTION_TYPE_INDEX => Self::StartMatchmakingRoleSelection {
                header,
                flag: reader.u8()?,
                activity: reader.u16()?,
                extra: reader.u16()?,
            },
            GAME_MODE_PLAYER_REMOVED_TYPE_INDEX => Self::GameModePlayerRemoved {
                header,
                match_id: reader.u32()?,
                player: CharacterRef::read(&mut reader)?,
            },
            STOP_CUTSCENE_WITH_NAME_TYPE_INDEX => Self::StopCutsceneWithName {
                header,
                name: String::from_utf8_lossy(&read_string(&mut reader)?).into_owned(),
            },
            _ => {
                return Err(MessageError::BadTag {
                    field: "game mode",
                    tag: u8::try_from(type_index).unwrap_or(u8::MAX),
                });
            }
        };
        reader.finish()?;
        Ok(message)
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text is too long.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header().write(&mut writer);
        match self {
            Self::QueueGroupRemoved { .. } | Self::MatchmakingLoginRequested { .. } => {}
            Self::StartMatchmakingRoleSelection {
                flag,
                activity,
                extra,
                ..
            } => {
                writer.u8(*flag).u16(*activity).u16(*extra);
            }
            Self::GameModePlayerRemoved {
                match_id, player, ..
            } => {
                writer.u32(*match_id);
                player.write(&mut writer);
            }
            Self::StopCutsceneWithName { name, .. } => {
                write_string(&mut writer, name.as_bytes())?;
            }
        }
        Ok(writer.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gear::tests::hex;

    /// Retail `opr_que_and_lost`: the player's game-mode header and its queue calls.
    const HEADER: &str = "b67cfc1a4d9fe84e2896c8c14210296e";
    /// The activity of the retail 6254 and 6295 (Outpost Rush).
    const ACTIVITY: u16 = 0x3116;
    /// The match value of the retail 6824 and 6414.
    const MATCH_ID: u32 = 0x91b9_9a18;
    /// The match id text of the retail 5741.
    const MATCH_TEXT: &str = "019ee134-cb70-7ea0-8511-951c6721fb0f";

    fn client(type_index: u64, body: &str) -> GameModeClientMsg {
        GameModeClientMsg::decode(type_index, &hex(body))
            .expect("a game-mode call")
            .expect("reads")
    }

    fn server(type_index: u64, body: &str) -> GameModeServerMsg {
        GameModeServerMsg::decode(type_index, &hex(body)).expect("reads")
    }

    #[test]
    fn the_retail_client_calls_read_and_write_back() {
        let cases: [(u64, &str); 10] = [
            (MODIFY_GROUP_ELIGIBILITY_SUBSCRIPTION_TYPE_INDEX, "01"),
            // Retail 3v3 arena group and expedition runs.
            (LEAVE_QUEUE_FOR_GAME_MODE_TYPE_INDEX, "1fad9d4201"),
            (
                LEAVE_GAME_MODE_TYPE_INDEX,
                "1b3c2283eb3fbd0840a1a96ab8bb2c8b08bd3feb83223c1bb8b056cd19007cc8",
            ),
            (LEAVE_QUEUE_FOR_ALL_GAME_MODES_TYPE_INDEX, "0001"),
            (SET_MATCHMAKING_ROLE_TYPE_INDEX, "03"),
            (RECEIVED_CLIENT_EVENT_TYPE_INDEX, "9bb080d0"),
            (MATCHMAKING_LOGIN_CONFIRMED_TYPE_INDEX, ""),
            (JOIN_QUEUE_FOR_ACTIVITIES_TYPE_INDEX, "013116000001"),
            (SET_READIED_UP_STATUS_TYPE_INDEX, "91b99a1801"),
            (
                ACCEPT_MATCHMAKING_MATCH_TYPE_INDEX,
                "2430313965653133342d636237302d376561302d383531312d39353163363732316662306601",
            ),
        ];
        for (type_index, args) in cases {
            let body = format!("{HEADER}{args}");
            let call = client(type_index, &body);
            assert_eq!(call.type_index(), type_index);
            assert_eq!(call.encode().expect("writes"), hex(&body));
        }
        let GameModeClientMsg::JoinQueueForActivities { activities, .. } = client(
            JOIN_QUEUE_FOR_ACTIVITIES_TYPE_INDEX,
            &format!("{HEADER}013116000001"),
        ) else {
            panic!("a join queue");
        };
        assert_eq!(
            activities,
            [QueueActivity {
                activity: ACTIVITY,
                extra: 0,
                flag: 1,
            }]
        );
        let GameModeClientMsg::AcceptMatch { match_id, flag, .. } = client(
            ACCEPT_MATCHMAKING_MATCH_TYPE_INDEX,
            &format!(
                "{HEADER}2430313965653133342d636237302d376561302d383531312d39353163363732316662306601"
            ),
        ) else {
            panic!("an accept");
        };
        assert_eq!((match_id.as_str(), flag), (MATCH_TEXT, 1));
    }

    #[test]
    fn the_retail_server_pushes_read_and_write_back() {
        let cases: [(u64, &str); 4] = [
            (QUEUE_GROUP_REMOVED_TYPE_INDEX, ""),
            (MATCHMAKING_LOGIN_REQUESTED_TYPE_INDEX, ""),
            (START_MATCHMAKING_ROLE_SELECTION_TYPE_INDEX, "0131160000"),
            (
                GAME_MODE_PLAYER_REMOVED_TYPE_INDEX,
                "91b99a18053478731ca555476da9a0b79343e3acbe",
            ),
        ];
        for (type_index, args) in cases {
            let body = format!("{HEADER}{args}");
            let push = server(type_index, &body);
            assert_eq!(push.type_index(), type_index);
            assert!(GameModeServerMsg::is_type(type_index));
            assert_eq!(push.encode().expect("writes"), hex(&body));
        }
        let GameModeServerMsg::GameModePlayerRemoved {
            match_id, player, ..
        } = server(
            GAME_MODE_PLAYER_REMOVED_TYPE_INDEX,
            &format!("{HEADER}91b99a18053478731ca555476da9a0b79343e3acbe"),
        )
        else {
            panic!("a removal");
        };
        assert_eq!(match_id, MATCH_ID);
        assert!(player.0.is_some());
        // Retail `Hive catacomes`: stop the cutscene with an empty name.
        let stop = server(
            STOP_CUTSCENE_WITH_NAME_TYPE_INDEX,
            "7134484c503933ace34e14f344a974cc00",
        );
        assert_eq!(
            stop.encode().expect("writes"),
            hex("7134484c503933ace34e14f344a974cc00")
        );
    }

    #[test]
    fn other_types_and_long_bodies_are_refused() {
        assert!(GameModeClientMsg::decode(1, &[]).is_none());
        let mut long = hex(&format!("{HEADER}01"));
        long.push(0);
        assert!(matches!(
            GameModeClientMsg::decode(MODIFY_GROUP_ELIGIBILITY_SUBSCRIPTION_TYPE_INDEX, &long),
            Some(Err(_))
        ));
        assert!(matches!(
            GameModeServerMsg::decode(1, &hex(HEADER)),
            Err(MessageError::BadTag { .. })
        ));
    }
}
