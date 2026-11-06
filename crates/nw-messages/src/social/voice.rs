// SPDX-License-Identifier: AGPL-3.0-or-later
//! `VoiceChatComponent` calls and pushes: the setup of the client's voice service (retail: a
//! Vivox service). Layouts from the voice bodies of 13 retail sessions (1,124 registrations and
//! the token, channel and error calls of `newchar-retail-2026-09-18`, `dungeon_dynasty_tank_`,
//! `A-Special-Friend`, `Hatchery`, `Hive 3v3 group`).
//!
//! Retail flow (Observed): 1643 gets 4199 (the service's API address, token issuer and SIP
//! domain); 2077 gets a login token (3051, kind [`TOKEN_LOGIN`]); 2805 gets the positional
//! channels of the player's place (405); each 2934 gets a join token (kind [`TOKEN_JOIN`]); 4204
//! gets the group channel (4200); 4203 gets a transcription token (kind
//! [`TOKEN_TRANSCRIPTION`]). Every token answer repeats the call's request number.

use crate::facet::FacetHeader;
use crate::social::text::{read_count, read_text, write_count, write_text};
use crate::wire::{MessageError, Reader, Writer};

/// `VoiceChatComponentServerFacet_RegisterVoiceChatService`.
pub const REGISTER_TYPE_INDEX: u64 = 1643;
/// `VoiceChatComponentServerFacet_GenerateLoginAccessToken`.
pub const LOGIN_TOKEN_TYPE_INDEX: u64 = 2077;
/// `VoiceChatComponentServerFacet_GenerateJoinAccessToken`.
pub const JOIN_TOKEN_TYPE_INDEX: u64 = 2934;
/// `VoiceChatComponentServerFacet_GenerateTranscriptionAccessToken`.
pub const TRANSCRIPTION_TOKEN_TYPE_INDEX: u64 = 4203;
/// `VoiceChatComponentServerFacet_SyncVoiceChatClientActivationState`.
pub const ACTIVATION_STATE_TYPE_INDEX: u64 = 188;
/// `VoiceChatComponentServerFacet_RequestWorldChannelUris`.
pub const WORLD_CHANNELS_TYPE_INDEX: u64 = 2805;
/// `VoiceChatComponentServerFacet_RequestGroupChannelUri`.
pub const GROUP_CHANNEL_TYPE_INDEX: u64 = 4204;
/// `VoiceChatComponentServerFacet_ReportError`.
pub const REPORT_ERROR_TYPE_INDEX: u64 = 4205;
/// `VoiceChatComponentClientFacet_OnVoiceChatServiceRegistered`.
pub const REGISTERED_TYPE_INDEX: u64 = 4199;
/// `VoiceChatComponentClientFacet_OnAccessTokenReceived`.
pub const TOKEN_TYPE_INDEX: u64 = 3051;
/// `VoiceChatComponentClientFacet_On3dChannelUrisReceived`.
pub const WORLD_CHANNEL_URIS_TYPE_INDEX: u64 = 405;
/// `VoiceChatComponentClientFacet_On2dChannelUriReceived`.
pub const GROUP_CHANNEL_URI_TYPE_INDEX: u64 = 4200;

/// The voice calls of the client.
pub(crate) const CALL_TYPES: [u64; 8] = [
    REGISTER_TYPE_INDEX,
    LOGIN_TOKEN_TYPE_INDEX,
    JOIN_TOKEN_TYPE_INDEX,
    TRANSCRIPTION_TOKEN_TYPE_INDEX,
    ACTIVATION_STATE_TYPE_INDEX,
    WORLD_CHANNELS_TYPE_INDEX,
    GROUP_CHANNEL_TYPE_INDEX,
    REPORT_ERROR_TYPE_INDEX,
];

/// 3051 kind of a login token (`vxa` `login` in the token).
pub const TOKEN_LOGIN: u32 = 1;
/// 3051 kind of a join token (`vxa` `join`).
pub const TOKEN_JOIN: u32 = 2;
/// 3051 kind of a transcription token (`vxa` `trxn`).
pub const TOKEN_TRANSCRIPTION: u32 = 6;
/// 2934 kind of a positional channel (179 of 216 retail joins: the names of 405).
pub const CHANNEL_POSITIONAL: u32 = 1;
/// 2934 kind of a group channel (37 of 216 retail joins: the names of 4200).
pub const CHANNEL_GROUP: u32 = 0;
/// The last byte of every retail 4199 (1,124 of 1,124).
pub const REGISTERED_FLAG: u8 = 0;

/// A voice call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceCall {
    /// 1643: set up the voice service.
    Register,
    /// 2077: a login token.
    LoginToken {
        /// The request number; the answer repeats it.
        request: u16,
    },
    /// 2934: a token to join a channel.
    JoinToken {
        /// The request number.
        request: u16,
        /// The channel name (a 405 name or a 4200 id).
        channel: String,
        /// [`CHANNEL_POSITIONAL`] or [`CHANNEL_GROUP`].
        kind: u32,
    },
    /// 4203: a token to transcribe a channel.
    TranscriptionToken {
        /// The request number.
        request: u16,
        /// The channel's SIP address.
        channel_uri: String,
    },
    /// 188: the client turned its voice chat on or off.
    ActivationState {
        /// Whether it is on.
        active: bool,
    },
    /// 2805: the positional channels of the player's place.
    WorldChannels,
    /// 4204: the channel of the player's group.
    GroupChannel,
    /// 4205: the client's voice service failed (2 in every retail report).
    ReportError {
        /// The error value.
        error: u32,
    },
}

impl VoiceCall {
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let call = match type_index {
            REGISTER_TYPE_INDEX => Ok(Self::Register),
            LOGIN_TOKEN_TYPE_INDEX => reader.u16().map(|request| Self::LoginToken { request }),
            JOIN_TOKEN_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::JoinToken {
                    request: reader.u16()?,
                    channel: read_text(reader)?,
                    kind: reader.u32()?,
                })
            })(),
            TRANSCRIPTION_TOKEN_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::TranscriptionToken {
                    request: reader.u16()?,
                    channel_uri: read_text(reader)?,
                })
            })(),
            ACTIVATION_STATE_TYPE_INDEX => reader
                .boolean()
                .map(|active| Self::ActivationState { active }),
            WORLD_CHANNELS_TYPE_INDEX => Ok(Self::WorldChannels),
            GROUP_CHANNEL_TYPE_INDEX => Ok(Self::GroupChannel),
            REPORT_ERROR_TYPE_INDEX => reader.u32().map(|error| Self::ReportError { error }),
            _ => return None,
        };
        Some(call)
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Register => REGISTER_TYPE_INDEX,
            Self::LoginToken { .. } => LOGIN_TOKEN_TYPE_INDEX,
            Self::JoinToken { .. } => JOIN_TOKEN_TYPE_INDEX,
            Self::TranscriptionToken { .. } => TRANSCRIPTION_TOKEN_TYPE_INDEX,
            Self::ActivationState { .. } => ACTIVATION_STATE_TYPE_INDEX,
            Self::WorldChannels => WORLD_CHANNELS_TYPE_INDEX,
            Self::GroupChannel => GROUP_CHANNEL_TYPE_INDEX,
            Self::ReportError { .. } => REPORT_ERROR_TYPE_INDEX,
        }
    }

    /// The body of the call to the voice chat facet `header`, as
    /// [`crate::social::SocialCall::decode`] reads it (a test client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text does not fit its prefix.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        header.write(&mut writer);
        match self {
            Self::Register | Self::WorldChannels | Self::GroupChannel => {}
            Self::LoginToken { request } => {
                writer.u16(*request);
            }
            Self::JoinToken {
                request,
                channel,
                kind,
            } => {
                writer.u16(*request);
                write_text(&mut writer, channel)?;
                writer.u32(*kind);
            }
            Self::TranscriptionToken {
                request,
                channel_uri,
            } => {
                writer.u16(*request);
                write_text(&mut writer, channel_uri)?;
            }
            Self::ActivationState { active } => {
                writer.boolean(*active);
            }
            Self::ReportError { error } => {
                writer.u32(*error);
            }
        }
        Ok(writer.finish())
    }
}

/// A voice push of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoicePush {
    /// 4199: the voice service of this world.
    Registered {
        /// The service's API address (retail `https://<host>/api2/`).
        api_url: String,
        /// The token issuer (the `iss` of each token).
        issuer: String,
        /// The SIP domain, with its leading `@`.
        domain: String,
        /// [`REGISTERED_FLAG`].
        flag: u8,
    },
    /// 3051: an access token.
    Token {
        /// The token (`e30.<claims>.<signature>`).
        token: String,
        /// [`TOKEN_LOGIN`], [`TOKEN_JOIN`] or [`TOKEN_TRANSCRIPTION`].
        kind: u32,
        /// The request number of the call.
        request: u16,
    },
    /// 405: the positional channels (name, SIP address) of the player's place.
    WorldChannels {
        /// The channels.
        channels: Vec<(String, String)>,
    },
    /// 4200: the group channel.
    GroupChannel {
        /// Its SIP address.
        uri: String,
        /// Its id (the name a 2934 joins).
        channel: String,
    },
}

impl VoicePush {
    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Registered { .. } => REGISTERED_TYPE_INDEX,
            Self::Token { .. } => TOKEN_TYPE_INDEX,
            Self::WorldChannels { .. } => WORLD_CHANNEL_URIS_TYPE_INDEX,
            Self::GroupChannel { .. } => GROUP_CHANNEL_URI_TYPE_INDEX,
        }
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::Registered {
                api_url,
                issuer,
                domain,
                flag,
            } => {
                write_text(writer, api_url)?;
                write_text(writer, issuer)?;
                write_text(writer, domain)?;
                writer.u8(*flag);
            }
            Self::Token {
                token,
                kind,
                request,
            } => {
                write_text(writer, token)?;
                writer.u32(*kind).u16(*request);
            }
            Self::WorldChannels { channels } => {
                write_count(writer, channels.len())?;
                for (name, uri) in channels {
                    write_text(writer, name)?;
                    write_text(writer, uri)?;
                }
            }
            Self::GroupChannel { uri, channel } => {
                write_text(writer, uri)?;
                write_text(writer, channel)?;
            }
        }
        Ok(())
    }

    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let push = match type_index {
            REGISTERED_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Registered {
                    api_url: read_text(reader)?,
                    issuer: read_text(reader)?,
                    domain: read_text(reader)?,
                    flag: reader.u8()?,
                })
            })(),
            TOKEN_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Token {
                    token: read_text(reader)?,
                    kind: reader.u32()?,
                    request: reader.u16()?,
                })
            })(),
            WORLD_CHANNEL_URIS_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                let channels = (0..read_count(reader)?)
                    .map(|_| -> Result<(String, String), MessageError> {
                        Ok((read_text(reader)?, read_text(reader)?))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Self::WorldChannels { channels })
            })(),
            GROUP_CHANNEL_URI_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::GroupChannel {
                    uri: read_text(reader)?,
                    channel: read_text(reader)?,
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

    const HEADER: &str = "403a00e443bf7ef02715f9bca18fdece";

    /// The call of the retail body of `type_index` with arguments `args`; its encode gives the
    /// same body back.
    fn call(type_index: u64, args: &str) -> VoiceCall {
        let body = hex(&format!("{HEADER}{args}"));
        match SocialCall::decode(type_index, &body).expect("a call") {
            Ok((header, SocialCall::Voice(call))) => {
                assert_eq!(call.type_index(), type_index);
                assert_eq!(call.encode(header).expect("encode"), body);
                call
            }
            other => panic!("a voice call: {other:?}"),
        }
    }

    /// The retail calls of newchar 2026-09-18, `dungeon_dynasty_tank_` and `Hatchery`.
    #[test]
    fn the_retail_calls_read() {
        assert_eq!(call(REGISTER_TYPE_INDEX, ""), VoiceCall::Register);
        assert_eq!(
            call(LOGIN_TOKEN_TYPE_INDEX, "0001"),
            VoiceCall::LoginToken { request: 1 }
        );
        assert_eq!(
            call(
                JOIN_TOKEN_TYPE_INDEX,
                &format!("000208{}00000001", hex_of("013008_0"))
            ),
            VoiceCall::JoinToken {
                request: 2,
                channel: "013008_0".to_owned(),
                kind: CHANNEL_POSITIONAL,
            }
        );
        assert_eq!(
            call(
                TRANSCRIPTION_TOKEN_TYPE_INDEX,
                &format!("000305{}", hex_of("sip:x"))
            ),
            VoiceCall::TranscriptionToken {
                request: 3,
                channel_uri: "sip:x".to_owned(),
            }
        );
        assert_eq!(
            call(ACTIVATION_STATE_TYPE_INDEX, "01"),
            VoiceCall::ActivationState { active: true }
        );
        assert_eq!(
            call(WORLD_CHANNELS_TYPE_INDEX, ""),
            VoiceCall::WorldChannels
        );
        assert_eq!(call(GROUP_CHANNEL_TYPE_INDEX, ""), VoiceCall::GroupChannel);
        assert_eq!(
            call(REPORT_ERROR_TYPE_INDEX, "00000002"),
            VoiceCall::ReportError { error: 2 }
        );
    }

    /// The retail 4199 of newchar 2026-09-18: three texts and a zero byte (82 bytes in all).
    #[test]
    fn the_registration_has_the_retail_bytes() {
        let push = SocialPush::Voice(VoicePush::Registered {
            api_url: "https://nwxp.www.vivox.com/api2/".to_owned(),
            issuer: "amazon9050-ne83".to_owned(),
            domain: "@nwxp.vivox.com".to_owned(),
            flag: REGISTERED_FLAG,
        });
        let args = push.encode_args().expect("encode");
        assert_eq!(
            args,
            hex(&format!(
                "20{}0f{}0f{}00",
                hex_of("https://nwxp.www.vivox.com/api2/"),
                hex_of("amazon9050-ne83"),
                hex_of("@nwxp.vivox.com")
            ))
        );
        assert_eq!(args.len() + 16, 82);
        assert_eq!(
            SocialPush::decode_args(REGISTERED_TYPE_INDEX, &args),
            Some(Ok(push))
        );
    }

    #[test]
    fn the_answers_round_trip() {
        let pushes = [
            VoicePush::Token {
                token: "e30.e30.sig".to_owned(),
                kind: TOKEN_JOIN,
                request: 6,
            },
            VoicePush::WorldChannels {
                channels: vec![
                    ("013007_0".to_owned(), "sip:a".to_owned()),
                    ("013008_0".to_owned(), "sip:b".to_owned()),
                ],
            },
            VoicePush::GroupChannel {
                uri: "sip:g".to_owned(),
                channel: "g".to_owned(),
            },
        ];
        for push in pushes {
            let push = SocialPush::Voice(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
        let token = SocialPush::Voice(VoicePush::Token {
            token: "t".to_owned(),
            kind: TOKEN_LOGIN,
            request: 1,
        });
        // Retail: the token, then `00000001 0001`.
        assert_eq!(
            token.encode_args().expect("encode"),
            hex("0174000000010001")
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
            VoiceCall::Register,
            VoiceCall::LoginToken { request: 1 },
            VoiceCall::JoinToken {
                request: 2,
                channel: "013008_0".to_owned(),
                kind: CHANNEL_POSITIONAL,
            },
            VoiceCall::TranscriptionToken {
                request: 3,
                channel_uri: "sip:x".to_owned(),
            },
            VoiceCall::ActivationState { active: true },
            VoiceCall::WorldChannels,
            VoiceCall::GroupChannel,
            VoiceCall::ReportError { error: 2 },
        ]
        .map(SocialCall::Voice);
        crate::social::assert_calls_round_trip(ROUND_TRIP_HEADER, &calls);
    }
}
