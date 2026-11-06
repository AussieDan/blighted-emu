// SPDX-License-Identifier: AGPL-3.0-or-later
//! The social facet calls of the player: chat, players (names, presence, phases, blocks,
//! friends, reports), groups, companies and voice-chat setup (run 15 social study,
//! `local-data/scratch-work/run15/social/REPORT.md`).
//!
//! Every body starts with the facet header (target, entity). The five components sit on one
//! child entity of the player prefab ([`SOCIAL_CHILD`]) or, for voice chat, on another
//! ([`VOICE_CHILD`]): `target = (entity XOR child) + component`. The constants were solved from
//! the headers of 21 to 57 retail sessions per component (bit by bit, every session agrees); the
//! bit 0 of a dynamic entity is always clear, so the one bit the sessions leave open does not
//! change a target.
//!
//! A client call decodes to a [`SocialCall`]; a server push is a [`SocialPush`]. The world sends a
//! push as its arguments (the body after the header) and the sim of the receiving player puts
//! its own header in front ([`push_envelope`]).

pub mod chat;
pub mod group_data;
pub mod groups;
pub mod guilds;
pub mod players;
pub mod states;
mod text;
pub mod voice;

use nw_reflect::{Envelope, TypeId};

use crate::facet::FacetHeader;
use crate::wire::{MessageError, Reader, Writer};

pub use chat::{ChatCall, ChatChannel, ChatMessage, ChatPush, ItemLink, MuteNotice};
pub use groups::{GroupCall, GroupPush};
pub use guilds::{CREST_LEN, GuildCall, GuildPush, ListOrder, crest_bytes, crest_of_bytes};
pub use players::{
    FriendRequest, ICON_LEN, PlayerCall, PlayerPush, RequestFailedMsg, icon_bytes, icon_of_bytes,
};
pub use text::CharacterRef;
pub use voice::{VoiceCall, VoicePush};

/// The XOR of the player prefab's main entity and the child entity that holds its chat, social,
/// groups and guilds components.
pub const SOCIAL_CHILD: u64 = 0x3246_6444_0d3d_1c2a;
/// The XOR of the player prefab's main entity and the child entity that holds its voice chat.
pub const VOICE_CHILD: u64 = 0x001b_4c22_0000_0000;
/// `ChatComponent` (57 retail sessions).
pub const CHAT_COMPONENT: u64 = 0x4dc1_50c6_8062_2a0a;
/// `SocialComponent` (21 retail sessions).
pub const SOCIAL_COMPONENT: u64 = 0x40bd_be5a_7a03_4a0e;
/// `GroupsComponent` (19 retail sessions).
pub const GROUPS_COMPONENT: u64 = 0xf89d_abee_ac88_479c;
/// `GuildsComponent` (22 retail sessions).
pub const GUILDS_COMPONENT: u64 = 0xb53c_594a_4deb_7feb;
/// `VoiceChatComponent` (21 retail sessions).
pub const VOICE_CHAT_COMPONENT: u64 = 0x192b_4b45_a22f_a022;

/// The player component a social call or push goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocialFacet {
    /// `ChatComponent`.
    Chat,
    /// `SocialComponent`.
    Social,
    /// `GroupsComponent`.
    Groups,
    /// `GuildsComponent`.
    Guilds,
    /// `VoiceChatComponent`.
    VoiceChat,
}

impl SocialFacet {
    /// The component constant.
    pub const fn component(self) -> u64 {
        match self {
            Self::Chat => CHAT_COMPONENT,
            Self::Social => SOCIAL_COMPONENT,
            Self::Groups => GROUPS_COMPONENT,
            Self::Guilds => GUILDS_COMPONENT,
            Self::VoiceChat => VOICE_CHAT_COMPONENT,
        }
    }

    /// The child entity XOR of the component.
    pub const fn child(self) -> u64 {
        match self {
            Self::VoiceChat => VOICE_CHILD,
            Self::Chat | Self::Social | Self::Groups | Self::Guilds => SOCIAL_CHILD,
        }
    }

    /// The header of a push to this component of the player of entity number `entity_number`.
    pub fn header(self, entity_number: u64) -> FacetHeader {
        FacetHeader::of_child(self.component(), self.child(), entity_number)
    }
}

/// One social call of the client, with its header.
#[derive(Debug, Clone, PartialEq)]
pub enum SocialCall {
    /// A chat call.
    Chat(ChatCall),
    /// A players call (names, phases, blocks, friends, settings, reports).
    Player(PlayerCall),
    /// A group call.
    Group(GroupCall),
    /// A company call.
    Guild(GuildCall),
    /// A voice-chat call.
    Voice(VoiceCall),
}

impl SocialCall {
    /// Decodes `body` when `type_index` is a social call; `None` otherwise.
    pub fn decode(
        type_index: u64,
        body: &[u8],
    ) -> Option<Result<(FacetHeader, Self), MessageError>> {
        let read = |reader: &mut Reader<'_>| -> Option<Result<Self, MessageError>> {
            if let Some(call) = ChatCall::read(type_index, reader) {
                return Some(call.map(Self::Chat));
            }
            if let Some(call) = PlayerCall::read(type_index, reader) {
                return Some(call.map(Self::Player));
            }
            if let Some(call) = GroupCall::read(type_index, reader) {
                return Some(call.map(Self::Group));
            }
            if let Some(call) = GuildCall::read(type_index, reader) {
                return Some(call.map(Self::Guild));
            }
            VoiceCall::read(type_index, reader).map(|call| call.map(Self::Voice))
        };
        if !Self::is_call(type_index) {
            return None;
        }
        let mut reader = Reader::new(body);
        let header = match FacetHeader::read(&mut reader) {
            Ok(header) => header,
            Err(error) => return Some(Err(error)),
        };
        let call = read(&mut reader)?;
        Some(call.and_then(|call| {
            reader.finish()?;
            Ok((header, call))
        }))
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Chat(call) => call.type_index(),
            Self::Player(call) => call.type_index(),
            Self::Group(call) => call.type_index(),
            Self::Guild(call) => call.type_index(),
            Self::Voice(call) => call.type_index(),
        }
    }

    /// The body of the call to the facet `header`, as [`SocialCall::decode`] reads it (a test
    /// client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a value does not fit its field.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        match self {
            Self::Chat(call) => call.encode(header),
            Self::Player(call) => call.encode(header),
            Self::Group(call) => call.encode(header),
            Self::Guild(call) => call.encode(header),
            Self::Voice(call) => call.encode(header),
        }
    }

    /// Whether `type_index` is a social call of the client.
    pub fn is_call(type_index: u64) -> bool {
        chat::CALL_TYPES.contains(&type_index)
            || players::CALL_TYPES.contains(&type_index)
            || groups::CALL_TYPES.contains(&type_index)
            || guilds::CALL_TYPES.contains(&type_index)
            || voice::CALL_TYPES.contains(&type_index)
    }
}

/// One social push of the server: its arguments, without the header.
#[derive(Debug, Clone, PartialEq)]
pub enum SocialPush {
    /// A chat push.
    Chat(chat::ChatPush),
    /// A players push.
    Player(players::PlayerPush),
    /// A group push.
    Group(groups::GroupPush),
    /// A company push.
    Guild(guilds::GuildPush),
    /// A voice-chat push.
    Voice(voice::VoicePush),
}

impl SocialPush {
    /// The type index.
    pub fn type_index(&self) -> u64 {
        match self {
            Self::Chat(push) => push.type_index(),
            Self::Player(push) => push.type_index(),
            Self::Group(push) => push.type_index(),
            Self::Guild(push) => push.type_index(),
            Self::Voice(push) => push.type_index(),
        }
    }

    /// The component that takes it.
    pub const fn facet(&self) -> SocialFacet {
        match self {
            Self::Chat(_) => SocialFacet::Chat,
            Self::Player(_) => SocialFacet::Social,
            Self::Group(_) => SocialFacet::Groups,
            Self::Guild(_) => SocialFacet::Guilds,
            Self::Voice(_) => SocialFacet::VoiceChat,
        }
    }

    /// The arguments: the body after the header.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a value does not fit its field.
    pub fn encode_args(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        match self {
            Self::Chat(push) => push.write(&mut writer)?,
            Self::Player(push) => push.write(&mut writer)?,
            Self::Group(push) => push.write(&mut writer)?,
            Self::Guild(push) => push.write(&mut writer)?,
            Self::Voice(push) => push.write(&mut writer)?,
        }
        Ok(writer.finish())
    }

    /// Decodes the whole body `body` of push `type_index`, its header first; `None` for another
    /// type.
    pub fn decode(
        type_index: u64,
        body: &[u8],
    ) -> Option<Result<(FacetHeader, Self), MessageError>> {
        let mut reader = Reader::new(body);
        let header = match FacetHeader::read(&mut reader) {
            Ok(header) => header,
            Err(error) => return Some(Err(error)),
        };
        let args = &body[body.len() - reader.remaining()..];
        Self::decode_args(type_index, args).map(|push| push.map(|push| (header, push)))
    }

    /// Decodes the arguments `args` of push `type_index`; `None` for another type.
    pub fn decode_args(type_index: u64, args: &[u8]) -> Option<Result<Self, MessageError>> {
        let mut reader = Reader::new(args);
        let push = if let Some(push) = chat::ChatPush::read(type_index, &mut reader) {
            push.map(Self::Chat)
        } else if let Some(push) = players::PlayerPush::read(type_index, &mut reader) {
            push.map(Self::Player)
        } else if let Some(push) = groups::GroupPush::read(type_index, &mut reader) {
            push.map(Self::Group)
        } else if let Some(push) = guilds::GuildPush::read(type_index, &mut reader) {
            push.map(Self::Guild)
        } else {
            voice::VoicePush::read(type_index, &mut reader)?.map(Self::Voice)
        };
        Some(push.and_then(|push| {
            reader.finish()?;
            Ok(push)
        }))
    }
}

/// The envelope of push `type_index` with arguments `args` to the player whose header is
/// `header`.
///
/// # Errors
///
/// Returns [`MessageError::Envelope`] when the envelope cannot be built.
pub fn push_envelope(
    type_index: u64,
    header: FacetHeader,
    args: &[u8],
) -> Result<Vec<u8>, MessageError> {
    let mut writer = Writer::new();
    header.write(&mut writer);
    writer.bytes(args);
    Ok(Envelope::message(TypeId::FileSlot(type_index), writer.finish()).encode()?)
}

/// Checks that each of `calls`, encoded to the facet `header`, reads back to the same header and
/// call.
#[cfg(test)]
pub(crate) fn assert_calls_round_trip(header: FacetHeader, calls: &[SocialCall]) {
    for call in calls {
        let body = call.encode(header).expect("encode");
        assert_eq!(
            SocialCall::decode(call.type_index(), &body),
            Some(Ok((header, call.clone()))),
            "{call:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_ids::number_of_facet;

    /// The entity of the retail newchar session (2026-09-18) and its social targets: 4142 (social),
    /// 1643 (voice), 5463 (groups) and the 293 pushes (chat).
    const RETAIL_ENTITY: u64 = 0x2715_f9bc_a18f_dece;
    /// The entity of the retail Hive 3v3 group session and its 1291 (chat) and 3639 (guilds)
    /// targets.
    const GROUP_SESSION_ENTITY: u64 = 0xe34e_14f3_44a9_74cc;

    #[test]
    fn the_solved_constants_give_the_retail_targets() {
        let number = number_of_facet(RETAIL_ENTITY);
        let cases = [
            (SocialFacet::Social, 0x5611_5c53_26b6_0cf2),
            (SocialFacet::VoiceChat, 0x403a_00e4_43bf_7ef0),
            (SocialFacet::Groups, 0x0df1_49e7_593b_0a80),
            (SocialFacet::Chat, 0x6314_eebf_2d14_ecee),
        ];
        for (facet, target) in cases {
            let header = facet.header(number);
            assert_eq!(header.entity, RETAIL_ENTITY);
            assert_eq!(header.target, target, "{facet:?}");
        }
        let other = number_of_facet(GROUP_SESSION_ENTITY);
        assert_eq!(
            SocialFacet::Chat.header(other).target,
            0x1ec9_c17d_c9f6_92f0
        );
        assert_eq!(
            SocialFacet::Social.header(other).target,
            0x11c6_2f11_c397_b2f4
        );
    }

    #[test]
    fn a_push_envelope_starts_with_the_header() {
        let header = FacetHeader {
            target: 1,
            entity: 2,
        };
        let bytes =
            push_envelope(chat::RECEIVE_CHAT_MESSAGE_TYPE_INDEX, header, &[9]).expect("envelope");
        let envelope = Envelope::decode(&bytes).expect("decode");
        assert_eq!(
            envelope.type_index(),
            Some(chat::RECEIVE_CHAT_MESSAGE_TYPE_INDEX)
        );
        assert_eq!(
            envelope.body,
            [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 9]
        );
    }

    #[test]
    fn a_whole_push_body_reads_back_with_its_header() {
        let header = FacetHeader {
            target: 3,
            entity: 4,
        };
        let push = SocialPush::Chat(chat::ChatPush::Batch(vec![chat::ChatMessage::system(
            chat::ChatChannel::System,
            "Done.".to_owned(),
        )]));
        let bytes = push_envelope(
            push.type_index(),
            header,
            &push.encode_args().expect("args"),
        )
        .expect("envelope");
        let envelope = Envelope::decode(&bytes).expect("decode");
        let read = SocialPush::decode(push.type_index(), &envelope.body)
            .expect("a push")
            .expect("read");
        let type_index = push.type_index();
        assert_eq!(read, (header, push));
        assert!(SocialPush::decode(type_index, &[1, 2]).is_some_and(|read| read.is_err()));
    }

    #[test]
    fn other_types_are_not_social_calls() {
        assert!(SocialCall::decode(crate::lookups::REQUEST_LOBBY_COUNTS_TYPE_INDEX, &[]).is_none());
        assert!(!SocialCall::is_call(
            crate::lookups::REQUEST_LOBBY_COUNTS_TYPE_INDEX
        ));
    }
}
