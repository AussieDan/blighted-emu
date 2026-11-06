// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GuildsComponent` (company) calls and pushes other than the look-up by id (3639 and 3610 are
//! [`crate::guild_lookup`]). Layouts from the retail sessions `company_actions`,
//! `creating_a_company`, `leaving_a_company`, `company_ranks` and `ranks-rights-compagny` (every
//! company call of the captures, 1 to 13 each), with the argument kinds of the Azoth catalog
//! catalog.
//!
//! A company list page (1410) is a 3610 body with one more text: the page token the client sends
//! back for the next page (retail: a JSON text of its ordering service; this server's own token
//! is opaque to the client the same way).

use crate::buildables::GuildCrest;
use crate::facet::FacetHeader;
use crate::guild_lookup::UnorderedGuildsMsg;
use crate::social::text::{CharacterRef, read_text, write_text};
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer};

/// `GuildsComponentServerFacet_RequestGuildNameAvailability`.
pub const NAME_AVAILABILITY_TYPE_INDEX: u64 = 3632;
/// `GuildsComponentServerFacet_RequestCreateGuild`.
pub const CREATE_GUILD_TYPE_INDEX: u64 = 3611;
/// `GuildsComponentServerFacet_RequestRefreshGuildData`.
pub const REFRESH_GUILD_DATA_TYPE_INDEX: u64 = 5492;
/// `GuildsComponentServerFacet_RequestCreateGuildInvite`.
pub const CREATE_INVITE_TYPE_INDEX: u64 = 3612;
/// `GuildsComponentServerFacet_RequestRespondToGuildInvite`.
pub const RESPOND_TO_INVITE_TYPE_INDEX: u64 = 118;
/// `GuildsComponentServerFacet_RequestLeaveGuild`.
pub const LEAVE_GUILD_TYPE_INDEX: u64 = 3543;
/// `GuildsComponentServerFacet_RequestRemoveGuildMember`.
pub const REMOVE_MEMBER_TYPE_INDEX: u64 = 2485;
/// `GuildsComponentServerFacet_RequestSetGuildMemberRank`.
pub const SET_MEMBER_RANK_TYPE_INDEX: u64 = 2591;
/// `GuildsComponentServerFacet_RequestDepositGuildTreasuryFunds`.
pub const DEPOSIT_TYPE_INDEX: u64 = 3618;
/// `GuildsComponentServerFacet_RequestWithdrawGuildTreasuryFunds`.
pub const WITHDRAW_TYPE_INDEX: u64 = 3620;
/// `GuildsComponentServerFacet_RequestSetGuildTreasuryWithdrawalLimit`.
pub const SET_WITHDRAWAL_LIMIT_TYPE_INDEX: u64 = 3621;
/// `GuildsComponentServerFacet_RequestEditGuildMOTD`.
pub const EDIT_MOTD_TYPE_INDEX: u64 = 3348;
/// `GuildsComponentServerFacet_RequestEditGuildIcon`.
pub const EDIT_ICON_TYPE_INDEX: u64 = 696;
/// `GuildsComponentServerFacet_RequestEditGuildName`.
pub const EDIT_NAME_TYPE_INDEX: u64 = 3614;
/// `GuildsComponentServerFacet_RequestSetGuildSiegeWindow`.
pub const SET_SIEGE_WINDOW_TYPE_INDEX: u64 = 3615;
/// `GuildsComponentServerFacet_RequestListGuildsByName`.
pub const LIST_BY_NAME_TYPE_INDEX: u64 = 3633;
/// `GuildsComponentServerFacet_RequestListGuildsByMembers`.
pub const LIST_BY_MEMBERS_TYPE_INDEX: u64 = 3635;
/// `GuildsComponentServerFacet_RequestListGuildsByClaims`.
pub const LIST_BY_CLAIMS_TYPE_INDEX: u64 = 3638;
/// `GuildsComponentClientFacet_OnGuildNameAvailableMsg`.
pub const NAME_AVAILABLE_TYPE_INDEX: u64 = 2386;
/// `GuildsComponentClientFacet_OnGuildDataUpdated`.
pub const GUILD_DATA_UPDATED_TYPE_INDEX: u64 = 5429;
/// `GuildsComponentClientFacet_OnGuildInviteRespondedMsg`.
pub const INVITE_RESPONDED_TYPE_INDEX: u64 = 3247;
/// `GuildsComponentClientFacet_OnKickedFromGuildMsg`.
pub const KICKED_TYPE_INDEX: u64 = 3598;
/// `GuildsComponentClientFacet_OnSetGuildMemberRankFailedReasonMsg`.
pub const RANK_FAILED_TYPE_INDEX: u64 = 3604;
/// `GuildsComponentClientFacet_OnDepositGuildTreasuryFundsMsg`.
pub const DEPOSITED_TYPE_INDEX: u64 = 5201;
/// `GuildsComponentClientFacet_OnWithdrawGuildTreasuryFundsMsg`.
pub const WITHDRAWN_TYPE_INDEX: u64 = 5200;
/// `GuildsComponentClientFacet_OnEditGuildCrestSucceededMsg`.
pub const CREST_EDITED_TYPE_INDEX: u64 = 1598;
/// `GuildsComponentClientFacet_OnGetOrderedGuildsMsg`.
pub const ORDERED_GUILDS_TYPE_INDEX: u64 = 1410;

/// The company calls of the client.
pub(crate) const CALL_TYPES: [u64; 18] = [
    NAME_AVAILABILITY_TYPE_INDEX,
    CREATE_GUILD_TYPE_INDEX,
    REFRESH_GUILD_DATA_TYPE_INDEX,
    CREATE_INVITE_TYPE_INDEX,
    RESPOND_TO_INVITE_TYPE_INDEX,
    LEAVE_GUILD_TYPE_INDEX,
    REMOVE_MEMBER_TYPE_INDEX,
    SET_MEMBER_RANK_TYPE_INDEX,
    DEPOSIT_TYPE_INDEX,
    WITHDRAW_TYPE_INDEX,
    SET_WITHDRAWAL_LIMIT_TYPE_INDEX,
    EDIT_MOTD_TYPE_INDEX,
    EDIT_ICON_TYPE_INDEX,
    EDIT_NAME_TYPE_INDEX,
    SET_SIEGE_WINDOW_TYPE_INDEX,
    LIST_BY_NAME_TYPE_INDEX,
    LIST_BY_MEMBERS_TYPE_INDEX,
    LIST_BY_CLAIMS_TYPE_INDEX,
];

/// The client's `GuildTreasuryResponse` value of a done transfer (both retail 5200). The other
/// names of the enum (`FUN_143f116a0`, in order: GuildDataNotFound, GuildMemberDataNotFound,
/// DoesNotHavePrivilege, NotEnoughPlayerFunds, NotEnoughTreasuryFunds, DepositCappedByTreasuryMax,
/// WithdrawCappedByDailyLimit, CoinTransferalDisabled, Success) put Success ninth, so they are
/// 1 to 8 (Inferred).
pub const TREASURY_SUCCESS: u32 = 9;
/// `GuildTreasuryResponse` `DoesNotHavePrivilege` (Inferred, see [`TREASURY_SUCCESS`]).
pub const TREASURY_NO_PRIVILEGE: u32 = 3;
/// `GuildTreasuryResponse` `NotEnoughPlayerFunds` (Inferred).
pub const TREASURY_NOT_ENOUGH_PLAYER_FUNDS: u32 = 4;
/// `GuildTreasuryResponse` `NotEnoughTreasuryFunds` (Inferred).
pub const TREASURY_NOT_ENOUGH_TREASURY_FUNDS: u32 = 5;
/// `GuildTreasuryResponse` `WithdrawCappedByDailyLimit` (Inferred).
pub const TREASURY_DAILY_LIMIT: u32 = 7;
/// The 3604 reason of the one retail rank change that failed (`company_actions`: a change of the
/// sender's own rank).
pub const RANK_FAILED_SELF: u32 = 0;
/// The page size of every retail list call (12 of 12).
pub const LIST_PAGE_SIZE: u16 = 20;

/// Bytes of a crest on the wire: two big-endian `u16` indexes and two RGBA `f32` colours.
pub const CREST_LEN: usize = 36;

/// The wire bytes of `crest`, as a company stores them.
pub fn crest_bytes(crest: &GuildCrest) -> [u8; CREST_LEN] {
    let mut writer = Writer::new();
    crest.write(&mut writer);
    let mut bytes = [0; CREST_LEN];
    bytes.copy_from_slice(&writer.finish());
    bytes
}

/// The crest of its wire bytes.
pub fn crest_of_bytes(bytes: &[u8; CREST_LEN]) -> GuildCrest {
    // 36 bytes always read as a crest: every field is a fixed-size number.
    GuildCrest::read(&mut Reader::new(bytes)).unwrap_or(GuildCrest::NONE)
}

/// The order of a company list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListOrder {
    /// 3633: by name.
    Name,
    /// 3635: by member count.
    Members,
    /// 3638: by territory count.
    Claims,
}

/// A company call of the client.
#[derive(Debug, Clone, PartialEq)]
pub enum GuildCall {
    /// 3632: is a company name free.
    NameAvailability {
        /// The request id; the answer repeats it.
        request: [u8; RAW_ID_LEN],
        /// The name.
        name: String,
    },
    /// 3611: create a company.
    Create {
        /// The name.
        name: String,
        /// The crest.
        crest: GuildCrest,
    },
    /// 5492: send the company data again.
    Refresh,
    /// 3612: invite a player.
    Invite {
        /// The character id as UUID text.
        character: String,
    },
    /// 118: answer a company invite.
    RespondToInvite {
        /// The company id.
        guild: [u8; RAW_ID_LEN],
        /// Whether the player joins.
        accept: bool,
    },
    /// 3543: leave the company.
    Leave,
    /// 2485: remove a member.
    RemoveMember {
        /// The character id as UUID text.
        character: String,
    },
    /// 2591: give a member a rank (0 governor to 3 settler: the ranks of the client's text,
    /// `ui_guildrankgovernor` to `ui_guildranksettler`).
    SetRank {
        /// The character id as UUID text.
        character: String,
        /// The rank.
        rank: u8,
    },
    /// 3618: put coin in the treasury (hundredths of a coin, like every coin value).
    Deposit {
        /// The amount.
        amount: u64,
    },
    /// 3620: take coin from the treasury.
    Withdraw {
        /// The amount.
        amount: u64,
    },
    /// 3621: the daily withdrawal limit of the members below the top ranks.
    SetWithdrawalLimit {
        /// The limit.
        limit: u64,
    },
    /// 3348: the message of the day.
    EditMotd {
        /// The text.
        text: String,
    },
    /// 696: the crest.
    EditCrest {
        /// The crest.
        crest: GuildCrest,
    },
    /// 3614: rename the company.
    EditName {
        /// The name.
        name: String,
    },
    /// 3615: the hour the company's sieges start.
    SetSiegeWindow {
        /// The hour (retail 18.0).
        hour: f32,
    },
    /// 3633, 3635, 3638: a page of the company list.
    List {
        /// The order.
        order: ListOrder,
        /// The request id; the answer repeats it.
        request: [u8; RAW_ID_LEN],
        /// Descending order.
        descending: bool,
        /// Companies per page ([`LIST_PAGE_SIZE`]).
        page_size: u16,
        /// The token of the page before; empty for the first page.
        token: String,
    },
}

impl GuildCall {
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let order = match type_index {
            LIST_BY_NAME_TYPE_INDEX => Some(ListOrder::Name),
            LIST_BY_MEMBERS_TYPE_INDEX => Some(ListOrder::Members),
            LIST_BY_CLAIMS_TYPE_INDEX => Some(ListOrder::Claims),
            _ => None,
        };
        if let Some(order) = order {
            return Some((|| -> Result<Self, MessageError> {
                Ok(Self::List {
                    order,
                    request: reader.raw_id()?,
                    descending: reader.boolean()?,
                    page_size: reader.u16()?,
                    token: read_text(reader)?,
                })
            })());
        }
        let call = match type_index {
            NAME_AVAILABILITY_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::NameAvailability {
                    request: reader.raw_id()?,
                    name: read_text(reader)?,
                })
            })(),
            CREATE_GUILD_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Create {
                    name: read_text(reader)?,
                    crest: GuildCrest::read(reader)?,
                })
            })(),
            REFRESH_GUILD_DATA_TYPE_INDEX => Ok(Self::Refresh),
            CREATE_INVITE_TYPE_INDEX => {
                read_text(reader).map(|character| Self::Invite { character })
            }
            RESPOND_TO_INVITE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::RespondToInvite {
                    guild: reader.raw_id()?,
                    accept: reader.boolean()?,
                })
            })(),
            LEAVE_GUILD_TYPE_INDEX => Ok(Self::Leave),
            REMOVE_MEMBER_TYPE_INDEX => {
                read_text(reader).map(|character| Self::RemoveMember { character })
            }
            SET_MEMBER_RANK_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::SetRank {
                    character: read_text(reader)?,
                    rank: reader.u8()?,
                })
            })(),
            DEPOSIT_TYPE_INDEX => reader.u64().map(|amount| Self::Deposit { amount }),
            WITHDRAW_TYPE_INDEX => reader.u64().map(|amount| Self::Withdraw { amount }),
            SET_WITHDRAWAL_LIMIT_TYPE_INDEX => {
                reader.u64().map(|limit| Self::SetWithdrawalLimit { limit })
            }
            EDIT_MOTD_TYPE_INDEX => read_text(reader).map(|text| Self::EditMotd { text }),
            EDIT_ICON_TYPE_INDEX => GuildCrest::read(reader).map(|crest| Self::EditCrest { crest }),
            EDIT_NAME_TYPE_INDEX => read_text(reader).map(|name| Self::EditName { name }),
            SET_SIEGE_WINDOW_TYPE_INDEX => reader.f32().map(|hour| Self::SetSiegeWindow { hour }),
            _ => return None,
        };
        Some(call)
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::NameAvailability { .. } => NAME_AVAILABILITY_TYPE_INDEX,
            Self::Create { .. } => CREATE_GUILD_TYPE_INDEX,
            Self::Refresh => REFRESH_GUILD_DATA_TYPE_INDEX,
            Self::Invite { .. } => CREATE_INVITE_TYPE_INDEX,
            Self::RespondToInvite { .. } => RESPOND_TO_INVITE_TYPE_INDEX,
            Self::Leave => LEAVE_GUILD_TYPE_INDEX,
            Self::RemoveMember { .. } => REMOVE_MEMBER_TYPE_INDEX,
            Self::SetRank { .. } => SET_MEMBER_RANK_TYPE_INDEX,
            Self::Deposit { .. } => DEPOSIT_TYPE_INDEX,
            Self::Withdraw { .. } => WITHDRAW_TYPE_INDEX,
            Self::SetWithdrawalLimit { .. } => SET_WITHDRAWAL_LIMIT_TYPE_INDEX,
            Self::EditMotd { .. } => EDIT_MOTD_TYPE_INDEX,
            Self::EditCrest { .. } => EDIT_ICON_TYPE_INDEX,
            Self::EditName { .. } => EDIT_NAME_TYPE_INDEX,
            Self::SetSiegeWindow { .. } => SET_SIEGE_WINDOW_TYPE_INDEX,
            Self::List { order, .. } => match order {
                ListOrder::Name => LIST_BY_NAME_TYPE_INDEX,
                ListOrder::Members => LIST_BY_MEMBERS_TYPE_INDEX,
                ListOrder::Claims => LIST_BY_CLAIMS_TYPE_INDEX,
            },
        }
    }

    /// The body of the call to the guilds facet `header`, as [`crate::social::SocialCall::decode`]
    /// reads it (a test client's call).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a text does not fit its prefix.
    pub fn encode(&self, header: FacetHeader) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        header.write(&mut writer);
        match self {
            Self::NameAvailability { request, name } => {
                writer.bytes(request);
                write_text(&mut writer, name)?;
            }
            Self::Create { name, crest } => {
                write_text(&mut writer, name)?;
                crest.write(&mut writer);
            }
            Self::Refresh | Self::Leave => {}
            Self::Invite { character } | Self::RemoveMember { character } => {
                write_text(&mut writer, character)?;
            }
            Self::RespondToInvite { guild, accept } => {
                writer.bytes(guild).boolean(*accept);
            }
            Self::SetRank { character, rank } => {
                write_text(&mut writer, character)?;
                writer.u8(*rank);
            }
            Self::Deposit { amount } | Self::Withdraw { amount } => {
                writer.u64(*amount);
            }
            Self::SetWithdrawalLimit { limit } => {
                writer.u64(*limit);
            }
            Self::EditMotd { text } => {
                write_text(&mut writer, text)?;
            }
            Self::EditCrest { crest } => {
                crest.write(&mut writer);
            }
            Self::EditName { name } => {
                write_text(&mut writer, name)?;
            }
            Self::SetSiegeWindow { hour } => {
                writer.f32(*hour);
            }
            Self::List {
                request,
                descending,
                page_size,
                token,
                ..
            } => {
                writer.bytes(request).boolean(*descending).u16(*page_size);
                write_text(&mut writer, token)?;
            }
        }
        Ok(writer.finish())
    }
}

/// A company push of the server.
#[derive(Debug, Clone, PartialEq)]
pub enum GuildPush {
    /// 2386: whether a company name is free.
    NameAvailable {
        /// The request id.
        request: [u8; RAW_ID_LEN],
        /// The name.
        name: String,
        /// Whether no company has it.
        available: bool,
    },
    /// 5429: the player's company changed (the client then asks for it again; retail sent it
    /// after each change and each 5492).
    DataUpdated {
        /// The company id (zero for none).
        guild: [u8; RAW_ID_LEN],
    },
    /// 3247: the company took the player's answer to its invite.
    InviteResponded {
        /// Whether the player joined.
        accepted: bool,
    },
    /// 3598: the player was removed from the company.
    Kicked,
    /// 3604: a rank change failed.
    RankFailed {
        /// The member.
        character: CharacterRef,
        /// The reason ([`RANK_FAILED_SELF`]).
        reason: u32,
    },
    /// 5201: coin went into the treasury.
    Deposited {
        /// The amount.
        amount: u64,
    },
    /// 5200: the answer to a withdrawal.
    Withdrawn {
        /// The amount asked for.
        amount: u64,
        /// The result ([`TREASURY_SUCCESS`]).
        result: u32,
    },
    /// 1598: the new crest is set.
    CrestEdited,
    /// 1410: a page of the company list (the header of the listing is not used).
    OrderedGuilds {
        /// The request id and companies, as a 3610.
        listing: UnorderedGuildsMsg,
        /// The token of this page; the client sends it back for the next one.
        token: String,
    },
}

/// The 16 bytes of a facet header in front of the arguments of [`UnorderedGuildsMsg`].
const HEADER_LEN: usize = 16;

impl GuildPush {
    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::NameAvailable { .. } => NAME_AVAILABLE_TYPE_INDEX,
            Self::DataUpdated { .. } => GUILD_DATA_UPDATED_TYPE_INDEX,
            Self::InviteResponded { .. } => INVITE_RESPONDED_TYPE_INDEX,
            Self::Kicked => KICKED_TYPE_INDEX,
            Self::RankFailed { .. } => RANK_FAILED_TYPE_INDEX,
            Self::Deposited { .. } => DEPOSITED_TYPE_INDEX,
            Self::Withdrawn { .. } => WITHDRAWN_TYPE_INDEX,
            Self::CrestEdited => CREST_EDITED_TYPE_INDEX,
            Self::OrderedGuilds { .. } => ORDERED_GUILDS_TYPE_INDEX,
        }
    }

    pub(crate) fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::NameAvailable {
                request,
                name,
                available,
            } => {
                writer.bytes(request);
                write_text(writer, name)?;
                writer.boolean(*available);
            }
            Self::DataUpdated { guild } => {
                writer.bytes(guild);
            }
            Self::InviteResponded { accepted } => {
                writer.boolean(*accepted);
            }
            Self::Kicked | Self::CrestEdited => {}
            Self::RankFailed { character, reason } => {
                character.write(writer);
                writer.u32(*reason);
            }
            Self::Deposited { amount } => {
                writer.u64(*amount);
            }
            Self::Withdrawn { amount, result } => {
                writer.u64(*amount).u32(*result);
            }
            Self::OrderedGuilds { listing, token } => {
                let body = listing.encode()?;
                writer.bytes(&body[HEADER_LEN..]);
                write_text(writer, token)?;
            }
        }
        Ok(())
    }

    /// Reads the arguments of push `type_index`. A list page (1410) is not read here: its entries
    /// have no reader outside [`UnorderedGuildsMsg`].
    pub(crate) fn read(
        type_index: u64,
        reader: &mut Reader<'_>,
    ) -> Option<Result<Self, MessageError>> {
        let push = match type_index {
            NAME_AVAILABLE_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::NameAvailable {
                    request: reader.raw_id()?,
                    name: read_text(reader)?,
                    available: reader.boolean()?,
                })
            })(),
            GUILD_DATA_UPDATED_TYPE_INDEX => {
                reader.raw_id().map(|guild| Self::DataUpdated { guild })
            }
            INVITE_RESPONDED_TYPE_INDEX => reader
                .boolean()
                .map(|accepted| Self::InviteResponded { accepted }),
            KICKED_TYPE_INDEX => Ok(Self::Kicked),
            RANK_FAILED_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::RankFailed {
                    character: CharacterRef::read(reader)?,
                    reason: reader.u32()?,
                })
            })(),
            DEPOSITED_TYPE_INDEX => reader.u64().map(|amount| Self::Deposited { amount }),
            WITHDRAWN_TYPE_INDEX => (|| -> Result<Self, MessageError> {
                Ok(Self::Withdrawn {
                    amount: reader.u64()?,
                    result: reader.u32()?,
                })
            })(),
            CREST_EDITED_TYPE_INDEX => Ok(Self::CrestEdited),
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

    const HEADER: &str = "5b8e6adfc9307f522896c8c14210296e";
    const REQUEST: &str = "a0ac561b314744908773b6c3194290b6";
    const CHARACTER: &str = "00000000-0000-4000-8000-000000000004";
    /// The crest of the retail 3611 (`creating_a_company`).
    const CREST: &str = "00ec3f0787883e48c8ca3f53d3d53f800000001c3eb0b0b13ef8f8fa3f3cbcbd3f800000";

    /// The call of the retail body of `type_index` with arguments `args`; its encode gives the
    /// same body back.
    fn call(type_index: u64, args: &str) -> GuildCall {
        let body = hex(&format!("{HEADER}{args}"));
        match SocialCall::decode(type_index, &body).expect("a call") {
            Ok((header, SocialCall::Guild(call))) => {
                assert_eq!(call.type_index(), type_index);
                assert_eq!(call.encode(header).expect("encode"), body);
                call
            }
            other => panic!("a company call: {other:?}"),
        }
    }

    /// The retail forms of the company calls (`company_actions`, `creating_a_company`,
    /// `ranks-rights-compagny`), names and ids replaced.
    #[test]
    fn the_retail_calls_read() {
        let GuildCall::Create { name, crest } = call(
            CREATE_GUILD_TYPE_INDEX,
            &format!("04{}{CREST}", hex_of("Test")),
        ) else {
            panic!("a create call");
        };
        assert_eq!(name, "Test");
        assert_eq!((crest.background, crest.foreground), (0xec, 0x1c));
        assert_eq!(
            call(LIST_BY_NAME_TYPE_INDEX, &format!("{REQUEST}00001400")),
            GuildCall::List {
                order: ListOrder::Name,
                request: hex(REQUEST).try_into().expect("16 bytes"),
                descending: false,
                page_size: LIST_PAGE_SIZE,
                token: String::new(),
            }
        );
        assert_eq!(
            call(
                SET_MEMBER_RANK_TYPE_INDEX,
                &format!("24{}03", hex_of(CHARACTER))
            ),
            GuildCall::SetRank {
                character: CHARACTER.to_owned(),
                rank: 3,
            }
        );
        assert_eq!(
            call(RESPOND_TO_INVITE_TYPE_INDEX, &format!("{REQUEST}01")),
            GuildCall::RespondToInvite {
                guild: hex(REQUEST).try_into().expect("16 bytes"),
                accept: true,
            }
        );
        assert_eq!(
            call(WITHDRAW_TYPE_INDEX, "000000000000c350"),
            GuildCall::Withdraw { amount: 50_000 }
        );
        assert_eq!(
            call(SET_SIEGE_WINDOW_TYPE_INDEX, "41900000"),
            GuildCall::SetSiegeWindow { hour: 18.0 }
        );
        assert_eq!(call(LEAVE_GUILD_TYPE_INDEX, ""), GuildCall::Leave);
        assert_eq!(call(REFRESH_GUILD_DATA_TYPE_INDEX, ""), GuildCall::Refresh);
        assert_eq!(
            call(EDIT_MOTD_TYPE_INDEX, &format!("02{}", hex_of("hi"))),
            GuildCall::EditMotd {
                text: "hi".to_owned()
            }
        );
    }

    /// The retail answers (`company_actions`, `ranks-rights-compagny`), ids replaced.
    #[test]
    fn the_retail_pushes_have_the_retail_bytes() {
        let cases = [
            (
                GuildPush::NameAvailable {
                    request: hex(REQUEST).try_into().expect("16 bytes"),
                    name: "Open".to_owned(),
                    available: true,
                },
                format!("{REQUEST}04{}01", hex_of("Open")),
            ),
            (
                GuildPush::Withdrawn {
                    amount: 50_000,
                    result: TREASURY_SUCCESS,
                },
                "000000000000c35000000009".to_owned(),
            ),
            (
                GuildPush::Deposited { amount: 50_000 },
                "000000000000c350".to_owned(),
            ),
            (
                GuildPush::InviteResponded { accepted: true },
                "01".to_owned(),
            ),
            (GuildPush::Kicked, String::new()),
            (GuildPush::CrestEdited, String::new()),
            (
                GuildPush::RankFailed {
                    character: CharacterRef(Some([0x33; RAW_ID_LEN])),
                    reason: RANK_FAILED_SELF,
                },
                format!("05{}00000000", "33".repeat(RAW_ID_LEN)),
            ),
            (
                GuildPush::DataUpdated {
                    guild: [0x44; RAW_ID_LEN],
                },
                "44".repeat(RAW_ID_LEN),
            ),
        ];
        for (push, expected) in cases {
            let push = SocialPush::Guild(push);
            let args = push.encode_args().expect("encode");
            assert_eq!(args, hex(&expected));
            assert_eq!(
                SocialPush::decode_args(push.type_index(), &args),
                Some(Ok(push))
            );
        }
    }

    #[test]
    fn a_crest_round_trips_its_bytes() {
        let bytes: [u8; CREST_LEN] = hex(CREST).try_into().expect("36 bytes");
        let crest = crest_of_bytes(&bytes);
        assert_eq!(crest.background, 0xec);
        assert_eq!(crest_bytes(&crest), bytes);
    }

    #[test]
    fn a_list_page_is_a_lookup_answer_and_its_token() {
        let listing = UnorderedGuildsMsg {
            header: FacetHeader {
                target: 0,
                entity: 0,
            },
            request: [5; RAW_ID_LEN],
            guilds: Vec::new(),
        };
        let push = SocialPush::Guild(GuildPush::OrderedGuilds {
            listing: listing.clone(),
            token: "p1".to_owned(),
        });
        let args = push.encode_args().expect("encode");
        let body = listing.encode().expect("encode");
        assert_eq!(args, [&body[HEADER_LEN..], &[2, b'p', b'1']].concat());
    }

    /// A header of synthetic ids.
    const ROUND_TRIP_HEADER: FacetHeader = FacetHeader {
        target: 0x0102_0304_0506_0708,
        entity: 0x1112_1314_1516_1718,
    };

    #[test]
    fn every_call_encodes_and_reads_back() {
        let crest = crest_of_bytes(&hex(CREST).try_into().expect("36 bytes"));
        let list = |order| GuildCall::List {
            order,
            request: [6; RAW_ID_LEN],
            descending: true,
            page_size: LIST_PAGE_SIZE,
            token: "p2".to_owned(),
        };
        let calls = [
            GuildCall::NameAvailability {
                request: [1; RAW_ID_LEN],
                name: "Open".to_owned(),
            },
            GuildCall::Create {
                name: "Test".to_owned(),
                crest,
            },
            GuildCall::Refresh,
            GuildCall::Invite {
                character: CHARACTER.to_owned(),
            },
            GuildCall::RespondToInvite {
                guild: [2; RAW_ID_LEN],
                accept: true,
            },
            GuildCall::Leave,
            GuildCall::RemoveMember {
                character: CHARACTER.to_owned(),
            },
            GuildCall::SetRank {
                character: CHARACTER.to_owned(),
                rank: 2,
            },
            GuildCall::Deposit { amount: 50_000 },
            GuildCall::Withdraw { amount: 40_000 },
            GuildCall::SetWithdrawalLimit { limit: 30_000 },
            GuildCall::EditMotd {
                text: "hi".to_owned(),
            },
            GuildCall::EditCrest { crest },
            GuildCall::EditName {
                name: "Renamed".to_owned(),
            },
            GuildCall::SetSiegeWindow { hour: 18.0 },
            list(ListOrder::Name),
            list(ListOrder::Members),
            list(ListOrder::Claims),
        ]
        .map(SocialCall::Guild);
        crate::social::assert_calls_round_trip(ROUND_TRIP_HEADER, &calls);
    }
}
