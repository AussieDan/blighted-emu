// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ChatComponent` calls and pushes. Layouts from every retail chat body of 17 sessions: 726
//! chat messages in 4118 and 293 read to their last byte with [`ChatMessage`], and the 13 retail
//! 1291 calls with [`ChatCall::Send`]. The channel enum is the client's `eChatMessageType`
//! (`FUN_1407e96d0`, which names each value).
//!
//! Profanity (retail, Observed): a line with no listed word comes as `body` with an empty
//! `filtered_body`. A line with one comes with `filtered_body` set (each listed word as `*`s);
//! `body` keeps the raw line only for a receiver who turned the filter off (4125 with 0), else it
//! is empty. A client that never sends 4125 has the filter on.

use crate::facet::FacetHeader;
use crate::social::text::{read_count, read_text, write_count, write_text};
use crate::wire::{MessageError, Reader, Writer};

/// `ChatComponentServerFacet_SendChatMessage`.
pub const SEND_CHAT_MESSAGE_TYPE_INDEX: u64 = 1291;
/// `ChatComponentServerFacet_SetProfanityFilterEnabled`.
pub const SET_PROFANITY_FILTER_TYPE_INDEX: u64 = 4125;
/// `ChatComponentServerFacet_SetLanguageEnum`.
pub const SET_LANGUAGE_TYPE_INDEX: u64 = 4126;
/// `ChatComponentServerFacet_RequestSetChatMute`.
pub const SET_CHAT_MUTE_TYPE_INDEX: u64 = 1015;
/// `ChatComponentServerFacet_RequestClearChatMuteFromClient`.
pub const CLEAR_CHAT_MUTE_TYPE_INDEX: u64 = 4124;
/// `ChatComponentClientFacet_ReceiveChatMessage`.
pub const RECEIVE_CHAT_MESSAGE_TYPE_INDEX: u64 = 4118;
/// `ChatComponentClientFacet_ReceiveBatchedChatMessages`.
pub const RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX: u64 = 293;
/// `ChatComponentClientFacet_ReceiveLocalizedSystemChatMessage`.
pub const RECEIVE_LOCALIZED_SYSTEM_MESSAGE_TYPE_INDEX: u64 = 1886;
/// `ChatComponentClientFacet_OnChatMuteSet`.
pub const CHAT_MUTE_SET_TYPE_INDEX: u64 = 1866;
/// `ChatComponentClientFacet_OnChatMuteCleared`.
pub const CHAT_MUTE_CLEARED_TYPE_INDEX: u64 = 1629;

/// The chat calls of the client.
pub(crate) const CALL_TYPES: [u64; 5] = [
    SEND_CHAT_MESSAGE_TYPE_INDEX,
    SET_PROFANITY_FILTER_TYPE_INDEX,
    SET_LANGUAGE_TYPE_INDEX,
    SET_CHAT_MUTE_TYPE_INDEX,
    CLEAR_CHAT_MUTE_TYPE_INDEX,
];

/// `sender_kind` of every retail chat message (726 of 726, players and system lines alike).
pub const SENDER_KIND: u8 = 2;
/// The bytes after `filtered_body` of every retail chat message: all zero (726 of 726).
pub const MESSAGE_RESERVED: [u8; MESSAGE_RESERVED_LEN] = [0; MESSAGE_RESERVED_LEN];
const MESSAGE_RESERVED_LEN: usize = 5;
/// `trailer` of every retail chat message (726 of 726).
pub const MESSAGE_TRAILER: u8 = 3;
/// `platform` of the retail system line (the 1886 of a war declaration).
pub const SYSTEM_LINE_PLATFORM: u32 = 1;
/// `duration` of every retail 1866 and 1629 (15 of 15): no end.
pub const MUTE_FOREVER: u32 = u32::MAX;
/// Bytes of one item link (all 8 retail links, in 4 messages).
pub const ITEM_LINK_LEN: usize = 17;

/// The client's `eChatMessageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChatChannel {
    /// 0: `Area`.
    Area,
    /// 1: `Area_Announce`.
    AreaAnnounce,
    /// 2: `Global`.
    Global,
    /// 3: `Guild` (company).
    Guild,
    /// 4: `Guild_Officer` (consul).
    GuildOfficer,
    /// 5: `Guild_Announce`.
    GuildAnnounce,
    /// 6: `Emote`.
    Emote,
    /// 7: `Group`.
    Group,
    /// 8: `Group_Announce`.
    GroupAnnounce,
    /// 9: `Raid`.
    Raid,
    /// 10: `Raid_Announce`.
    RaidAnnounce,
    /// 11: `Whisper`.
    Whisper,
    /// 12: `System`.
    System,
    /// 13: `War`.
    War,
    /// 14: `Recruitment`.
    Recruitment,
    /// 15: `Help`.
    Help,
    /// 16: `Group_Alert`.
    GroupAlert,
    /// 17: `Faction`.
    Faction,
    /// 18: `Trading`.
    Trading,
    /// 19: `Lobby`.
    Lobby,
    /// 20: `Game Mode`.
    GameMode,
}

/// Every channel, in wire order.
const CHANNELS: [ChatChannel; 21] = [
    ChatChannel::Area,
    ChatChannel::AreaAnnounce,
    ChatChannel::Global,
    ChatChannel::Guild,
    ChatChannel::GuildOfficer,
    ChatChannel::GuildAnnounce,
    ChatChannel::Emote,
    ChatChannel::Group,
    ChatChannel::GroupAnnounce,
    ChatChannel::Raid,
    ChatChannel::RaidAnnounce,
    ChatChannel::Whisper,
    ChatChannel::System,
    ChatChannel::War,
    ChatChannel::Recruitment,
    ChatChannel::Help,
    ChatChannel::GroupAlert,
    ChatChannel::Faction,
    ChatChannel::Trading,
    ChatChannel::Lobby,
    ChatChannel::GameMode,
];

impl ChatChannel {
    /// The channel of wire value `value`; `None` for `Invalid` (21) and above.
    pub fn from_wire(value: u32) -> Option<Self> {
        usize::try_from(value)
            .ok()
            .and_then(|index| CHANNELS.get(index))
            .copied()
    }

    /// The wire value.
    pub fn wire(self) -> u32 {
        // The table has 21 entries.
        CHANNELS
            .iter()
            .position(|channel| *channel == self)
            .unwrap_or_default() as u32
    }
}

/// One item link of a chat line: 17 bytes. Their fields are Unknown; the server passes them on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLink(pub [u8; ITEM_LINK_LEN]);

fn read_links(reader: &mut Reader<'_>) -> Result<Vec<ItemLink>, MessageError> {
    (0..read_count(reader)?)
        .map(|_| reader.array().map(ItemLink))
        .collect()
}

fn write_links(writer: &mut Writer, links: &[ItemLink]) -> Result<(), MessageError> {
    write_count(writer, links.len())?;
    for link in links {
        writer.bytes(&link.0);
    }
    Ok(())
}

/// One chat line as the client gets it (4118, each entry of 293, the head of 1886).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// The sender's character id as UUID text; empty for a system line.
    pub sender_id: String,
    /// The sender's name; empty for a system line.
    pub sender_name: String,
    /// [`SENDER_KIND`] on every retail line.
    pub sender_kind: u8,
    /// The receiver's name of a whisper; empty on other channels.
    pub recipient_name: String,
    /// The channel ([`ChatChannel::wire`]).
    pub channel: u32,
    /// The line as sent, or empty (see the module text).
    pub body: String,
    /// The line with its listed words starred; empty when it has none.
    pub filtered_body: String,
    /// [`MESSAGE_RESERVED`] on every retail line.
    pub reserved: [u8; MESSAGE_RESERVED_LEN],
    /// The linked items.
    pub links: Vec<ItemLink>,
    /// The sender's platform (players data note: 1, 13 or 14 on retail lines; `u32::MAX` for
    /// none).
    pub platform: u32,
    /// The sender's platform account id; empty for none.
    pub platform_id: String,
    /// 1 on 699 of 701 retail player lines, 0 on the other 2 and on system lines (meaning
    /// Unknown).
    pub flag: bool,
    /// [`MESSAGE_TRAILER`] on every retail line.
    pub trailer: u8,
}

impl ChatMessage {
    /// A server line on `channel` with no sender, laid out as the retail system line (the 1886
    /// of a war declaration).
    pub fn system(channel: ChatChannel, body: String) -> Self {
        Self {
            sender_id: String::new(),
            sender_name: String::new(),
            sender_kind: SENDER_KIND,
            recipient_name: String::new(),
            channel: channel.wire(),
            body,
            filtered_body: String::new(),
            reserved: MESSAGE_RESERVED,
            links: Vec::new(),
            platform: SYSTEM_LINE_PLATFORM,
            platform_id: String::new(),
            flag: false,
            trailer: MESSAGE_TRAILER,
        }
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            sender_id: read_text(reader)?,
            sender_name: read_text(reader)?,
            sender_kind: reader.u8()?,
            recipient_name: read_text(reader)?,
            channel: reader.u32()?,
            body: read_text(reader)?,
            filtered_body: read_text(reader)?,
            reserved: reader.array()?,
            links: read_links(reader)?,
            platform: reader.u32()?,
            platform_id: read_text(reader)?,
            flag: reader.boolean()?,
            trailer: reader.u8()?,
        })
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        write_text(writer, &self.sender_id)?;
        write_text(writer, &self.sender_name)?;
        writer.u8(self.sender_kind);
        write_text(writer, &self.recipient_name)?;
        writer.u32(self.channel);
        write_text(writer, &self.body)?;
        write_text(writer, &self.filtered_body)?;
        writer.bytes(&self.reserved);
        write_links(writer, &self.links)?;
        writer.u32(self.platform);
        write_text(writer, &self.platform_id)?;
        writer.boolean(self.flag).u8(self.trailer);
        Ok(())
    }
}

/// A chat call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCall {
    /// 1291: say a line.
    Send {
        /// The channel ([`ChatChannel::from_wire`]).
        channel: u32,
        /// The line.
        body: String,
        /// The receiver's name of a whisper; empty on other channels.
        recipient_name: String,
        /// The linked items (none in the 13 retail calls; the list reads as in [`ChatMessage`]).
        links: Vec<ItemLink>,
    },
    /// 4125: turn the profanity filter on or off (retail sends 0 when the player turns it off).
    SetProfanityFilter {
        /// Whether lines come starred.
        enabled: bool,
    },
    /// 4126: the client's language (`8` in the one retail call; the enum is Unknown).
    SetLanguage {
        /// The language value.
        language: u32,
    },
    /// 1015: mute a player's chat.
    SetMute {
        /// The character id as UUID text.
        character: String,
        /// 0 in both retail calls (meaning Unknown).
        flag: u8,
    },
    /// 4124: unmute a player's chat.
    ClearMute {
        /// The character id as UUID text.
        character: String,
        /// 0 in the retail call (meaning Unknown).
        flag: u8,
    },
}

impl ChatCall {
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let call = match type_index {
            SEND_CHAT_MESSAGE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Send {
                    channel: reader.u32()?,
                    body: read_text(reader)?,
                    recipient_name: read_text(reader)?,
                    links: read_links(reader)?,
                })
            })(),
            SET_PROFANITY_FILTER_TYPE_INDEX => reader
                .boolean()
                .map(|enabled| Self::SetProfanityFilter { enabled }),
            SET_LANGUAGE_TYPE_INDEX => reader.u32().map(|language| Self::SetLanguage { language }),
            SET_CHAT_MUTE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::SetMute {
                    character: read_text(reader)?,
                    flag: reader.u8()?,
                })
            })(),
            CLEAR_CHAT_MUTE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::ClearMute {
                    character: read_text(reader)?,
                    flag: reader.u8()?,
                })
            })(),
            _ => return None,
        };
        Some(call)
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Send { .. } => SEND_CHAT_MESSAGE_TYPE_INDEX,
            Self::SetProfanityFilter { .. } => SET_PROFANITY_FILTER_TYPE_INDEX,
            Self::SetLanguage { .. } => SET_LANGUAGE_TYPE_INDEX,
            Self::SetMute { .. } => SET_CHAT_MUTE_TYPE_INDEX,
            Self::ClearMute { .. } => CLEAR_CHAT_MUTE_TYPE_INDEX,
        }
    }

    /// The body of the call to the chat facet `header`, as [`crate::social::SocialCall::decode`]
    /// reads it (a test client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text or the link count does not fit its prefix.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        header.write(&mut writer);
        match self {
            Self::Send {
                channel,
                body,
                recipient_name,
                links,
            } => {
                writer.u32(*channel);
                write_text(&mut writer, body)?;
                write_text(&mut writer, recipient_name)?;
                write_links(&mut writer, links)?;
            }
            Self::SetProfanityFilter { enabled } => {
                writer.boolean(*enabled);
            }
            Self::SetLanguage { language } => {
                writer.u32(*language);
            }
            Self::SetMute { character, flag } | Self::ClearMute { character, flag } => {
                write_text(&mut writer, character)?;
                writer.u8(*flag);
            }
        }
        Ok(writer.finish())
    }
}

/// The notice of a mute or a social block that was set or cleared (1866, 1629).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuteNotice {
    /// The muted or blocked character id as UUID text.
    pub character: String,
    /// [`MUTE_FOREVER`] in every retail notice.
    pub duration: u32,
    /// 0 in every retail notice (meaning Unknown).
    pub flag: u8,
    /// Whether it is a social block (2799 and 2770 got 1) rather than a chat mute (1015 and 4124
    /// got 0).
    pub social_block: bool,
}

impl MuteNotice {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            character: read_text(reader)?,
            duration: reader.u32()?,
            flag: reader.u8()?,
            social_block: reader.boolean()?,
        })
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        write_text(writer, &self.character)?;
        writer
            .u32(self.duration)
            .u8(self.flag)
            .boolean(self.social_block);
        Ok(())
    }
}

/// A chat push of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatPush {
    /// 4118: one line (retail: area and emote lines).
    Message(ChatMessage),
    /// 293: lines in one body (retail: the other channels).
    Batch(Vec<ChatMessage>),
    /// 1886: a system line whose body is a localization key, with its parameters (name CRC-32,
    /// value).
    LocalizedSystem {
        /// The line.
        message: ChatMessage,
        /// The parameters.
        parameters: Vec<(u32, String)>,
    },
    /// 1866: a mute or block is set.
    MuteSet(MuteNotice),
    /// 1629: a mute or block is cleared.
    MuteCleared(MuteNotice),
}

impl ChatPush {
    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Message(_) => RECEIVE_CHAT_MESSAGE_TYPE_INDEX,
            Self::Batch(_) => RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX,
            Self::LocalizedSystem { .. } => RECEIVE_LOCALIZED_SYSTEM_MESSAGE_TYPE_INDEX,
            Self::MuteSet(_) => CHAT_MUTE_SET_TYPE_INDEX,
            Self::MuteCleared(_) => CHAT_MUTE_CLEARED_TYPE_INDEX,
        }
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::Message(message) => message.write(writer),
            Self::Batch(messages) => {
                write_count(writer, messages.len())?;
                messages
                    .iter()
                    .try_for_each(|message| message.write(writer))
            }
            Self::LocalizedSystem {
                message,
                parameters,
            } => {
                message.write(writer)?;
                write_count(writer, parameters.len())?;
                for (name, value) in parameters {
                    writer.u32(*name);
                    write_text(writer, value)?;
                }
                Ok(())
            }
            Self::MuteSet(notice) | Self::MuteCleared(notice) => notice.write(writer),
        }
    }

    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let push = match type_index {
            RECEIVE_CHAT_MESSAGE_TYPE_INDEX => ChatMessage::read(reader).map(Self::Message),
            RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                let count = read_count(reader)?;
                (0..count)
                    .map(|_| ChatMessage::read(reader))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::Batch)
            })(),
            RECEIVE_LOCALIZED_SYSTEM_MESSAGE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                let message = ChatMessage::read(reader)?;
                let parameters = (0..read_count(reader)?)
                    .map(|_| -> Result<(u32, String), MessageError> {
                        Ok((reader.u32()?, read_text(reader)?))
                    })
                    .collect::<Result<Vec<_>, MessageError>>()?;
                Ok(Self::LocalizedSystem {
                    message,
                    parameters,
                })
            })(),
            CHAT_MUTE_SET_TYPE_INDEX => MuteNotice::read(reader).map(Self::MuteSet),
            CHAT_MUTE_CLEARED_TYPE_INDEX => MuteNotice::read(reader).map(Self::MuteCleared),
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

    /// A 36-character id, as the retail lines carry.
    const SENDER: &str = "00000000-0000-4000-8000-000000000001";
    /// A 17-digit platform account id, as the retail lines carry.
    const PLATFORM_ID: &str = "70000000000000001";
    /// The retail platform value of those ids.
    const PLATFORM: u32 = 1;

    fn line(channel: ChatChannel, body: &str, filtered: &str) -> ChatMessage {
        ChatMessage {
            sender_id: SENDER.to_owned(),
            sender_name: "Ann".to_owned(),
            sender_kind: SENDER_KIND,
            recipient_name: String::new(),
            channel: channel.wire(),
            body: body.to_owned(),
            filtered_body: filtered.to_owned(),
            reserved: MESSAGE_RESERVED,
            links: Vec::new(),
            platform: PLATFORM,
            platform_id: PLATFORM_ID.to_owned(),
            flag: true,
            trailer: MESSAGE_TRAILER,
        }
    }

    #[test]
    fn every_chat_call_reads_back_as_it_was_written() {
        let header = FacetHeader {
            target: 0x6314_eebf_2d14_ecee,
            entity: 0x2715_f9bc_a18f_dece,
        };
        let calls = [
            ChatCall::Send {
                channel: ChatChannel::Area.wire(),
                body: "!help".to_owned(),
                recipient_name: String::new(),
                links: vec![ItemLink([7; ITEM_LINK_LEN])],
            },
            ChatCall::SetProfanityFilter { enabled: false },
            ChatCall::SetLanguage { language: 8 },
            ChatCall::SetMute {
                character: SENDER.to_owned(),
                flag: 0,
            },
            ChatCall::ClearMute {
                character: SENDER.to_owned(),
                flag: 0,
            },
        ];
        for call in calls {
            let body = call.encode(header).expect("encode");
            let read = SocialCall::decode(call.type_index(), &body)
                .expect("a social call")
                .expect("decode");
            assert_eq!(read, (header, SocialCall::Chat(call)));
        }
    }

    #[test]
    fn the_channel_values_are_the_client_enum() {
        assert_eq!(ChatChannel::Area.wire(), 0);
        assert_eq!(ChatChannel::Global.wire(), 2);
        assert_eq!(ChatChannel::Emote.wire(), 6);
        assert_eq!(ChatChannel::Whisper.wire(), 11);
        assert_eq!(ChatChannel::War.wire(), 13);
        assert_eq!(ChatChannel::Recruitment.wire(), 14);
        assert_eq!(ChatChannel::GameMode.wire(), 20);
        assert_eq!(ChatChannel::from_wire(17), Some(ChatChannel::Faction));
        assert_eq!(ChatChannel::from_wire(21), None);
    }

    /// The retail shape of a plain line (newchar 2026-09-18: a recruitment line): the tail after
    /// the empty filtered text is 5 zero bytes, no link, platform 1 and its id, then `01 03`.
    #[test]
    fn a_line_has_the_retail_tail() {
        let mut writer = Writer::new();
        line(ChatChannel::Recruitment, "x", "")
            .write(&mut writer)
            .expect("write");
        let bytes = writer.finish();
        let tail = hex(&format!(
            "0000000e01780000000000000000000001{:02x}{}0103",
            PLATFORM_ID.len(),
            hex_of(PLATFORM_ID)
        ));
        assert!(bytes.ends_with(&tail), "{}", hex_of_bytes(&bytes));
    }

    fn hex_of(text: &str) -> String {
        hex_of_bytes(text.as_bytes())
    }

    fn hex_of_bytes(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// A whisper call of retail msq14 (header, channel 11, line, receiver name, no link), with
    /// the line and the name replaced.
    #[test]
    fn a_whisper_call_reads() {
        let body = hex(&format!(
            "b654d6530c054fd25ad5e1c8869e39e20000000b02{}03{}00",
            hex_of("hi"),
            hex_of("Bob")
        ));
        let (header, call) = SocialCall::decode(SEND_CHAT_MESSAGE_TYPE_INDEX, &body)
            .expect("a social call")
            .expect("reads");
        assert_eq!(header.target, 0xb654_d653_0c05_4fd2);
        assert_eq!(
            call,
            SocialCall::Chat(ChatCall::Send {
                channel: ChatChannel::Whisper.wire(),
                body: "hi".to_owned(),
                recipient_name: "Bob".to_owned(),
                links: Vec::new(),
            })
        );
    }

    #[test]
    fn the_settings_and_mute_calls_read() {
        let header = "1ec9c17dc9f692f0e34e14f344a974cc";
        let cases = [
            (
                SET_PROFANITY_FILTER_TYPE_INDEX,
                format!("{header}00"),
                ChatCall::SetProfanityFilter { enabled: false },
            ),
            (
                SET_LANGUAGE_TYPE_INDEX,
                format!("{header}00000008"),
                ChatCall::SetLanguage { language: 8 },
            ),
            (
                SET_CHAT_MUTE_TYPE_INDEX,
                format!("{header}24{}00", hex_of(SENDER)),
                ChatCall::SetMute {
                    character: SENDER.to_owned(),
                    flag: 0,
                },
            ),
            (
                CLEAR_CHAT_MUTE_TYPE_INDEX,
                format!("{header}24{}00", hex_of(SENDER)),
                ChatCall::ClearMute {
                    character: SENDER.to_owned(),
                    flag: 0,
                },
            ),
        ];
        for (type_index, body, expected) in cases {
            let (_, call) = SocialCall::decode(type_index, &hex(&body))
                .expect("a social call")
                .expect("reads");
            assert_eq!(call, SocialCall::Chat(expected));
        }
    }

    /// The retail 1866 of a social block (Hive Social): the id, `ffffffff`, 0, 1.
    #[test]
    fn a_mute_notice_has_the_retail_layout() {
        let notice = ChatPush::MuteSet(MuteNotice {
            character: SENDER.to_owned(),
            duration: MUTE_FOREVER,
            flag: 0,
            social_block: true,
        });
        let args = SocialPush::Chat(notice.clone())
            .encode_args()
            .expect("encode");
        assert_eq!(args, hex(&format!("24{}ffffffff0001", hex_of(SENDER))));
        assert_eq!(
            SocialPush::decode_args(CHAT_MUTE_SET_TYPE_INDEX, &args),
            Some(Ok(SocialPush::Chat(notice)))
        );
    }

    #[test]
    fn pushes_round_trip() {
        let mut linked = line(ChatChannel::Trading, "[x", "");
        linked.links = vec![ItemLink([4; ITEM_LINK_LEN]); 2];
        let pushes = [
            ChatPush::Message(line(ChatChannel::Emote, "Ann waves", "")),
            ChatPush::Batch(vec![line(ChatChannel::Global, "", "a ***"), linked]),
            ChatPush::LocalizedSystem {
                message: ChatMessage {
                    sender_id: String::new(),
                    sender_name: String::new(),
                    channel: ChatChannel::War.wire(),
                    body: "@ui_war_declared_chat_notification".to_owned(),
                    platform_id: String::new(),
                    flag: false,
                    ..line(ChatChannel::War, "", "")
                },
                parameters: vec![(0xf951_eee8, "@WeaversFen".to_owned())],
            },
            ChatPush::MuteCleared(MuteNotice {
                character: SENDER.to_owned(),
                duration: MUTE_FOREVER,
                flag: 0,
                social_block: false,
            }),
        ];
        for push in pushes {
            let push = SocialPush::Chat(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
    }

    /// The retail 1886 of a war declaration (Hive 3v3 group): the line, then 3 parameters (the
    /// two company names replaced).
    #[test]
    fn the_retail_system_line_reads() {
        let args = hex(concat!(
            "000002000000000d224075695f7761725f6465636c617265645f636861745f6e6f74696669636174696f6e",
            "000000000000000000000100000303",
            "c2add5aa05416c706861",
            "f951eee80b405765617665727346656e",
            "5ba484100442657461"
        ));
        let Some(Ok(SocialPush::Chat(ChatPush::LocalizedSystem {
            message,
            parameters,
        }))) = SocialPush::decode_args(RECEIVE_LOCALIZED_SYSTEM_MESSAGE_TYPE_INDEX, &args)
        else {
            panic!("reads");
        };
        assert_eq!(message.channel, ChatChannel::War.wire());
        assert_eq!(message.body, "@ui_war_declared_chat_notification");
        assert!(!message.flag);
        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters[1], (0xf951_eee8, "@WeaversFen".to_owned()));
        assert_eq!(
            message,
            ChatMessage::system(ChatChannel::War, message.body.clone())
        );
    }
}
