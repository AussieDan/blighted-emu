// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the social state asks of the service around it: messages to send, rows to store, and the
//! two steps of a treasury transfer.

use nw_bus::{BusId, CoinTransfer, GroupEntityDelivery, OwnStateDelivery, SocialDelivery};
use nw_db::{
    CoinReturn, Company, CompanyInvite, CompanyMember, LinkKind, PlayerReport, SocialGroup,
    SocialSettings, TreasuryReason,
};
use uuid::Uuid;

/// One thing for the service to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send a delivery to one sim.
    Deliver {
        /// The sim of the receiving players.
        sim: BusId,
        /// The delivery.
        delivery: SocialDelivery,
    },
    /// Send a replicated social state change of one player to its sim.
    OwnState {
        /// The sim of the player.
        sim: BusId,
        /// The state.
        delivery: OwnStateDelivery,
    },
    /// Send a player's group entity to its sim.
    GroupEntity {
        /// The sim of the player.
        sim: BusId,
        /// The group entity.
        delivery: GroupEntityDelivery,
    },
    /// Store a change.
    Store(StoreOp),
    /// Read the account of a character the state did not load at start (a character made since);
    /// the answer comes back with `SocialWorld::account_found`.
    FindAccount {
        /// The character.
        character: Uuid,
    },
    /// Move a player's coins through its sim; the answer comes back with
    /// `SocialWorld::coins_moved` and the same ticket.
    MoveCoins {
        /// The transfer's ticket.
        ticket: u64,
        /// The sim that has the player.
        sim: BusId,
        /// The change.
        transfer: CoinTransfer,
    },
    /// Change a treasury in one database transaction with its log row; the answer comes back with
    /// `SocialWorld::treasury_changed` and the same ticket.
    Treasury {
        /// The transfer's ticket.
        ticket: u64,
        /// The company.
        company: Uuid,
        /// The member.
        character: Uuid,
        /// The change, hundredths of a coin.
        change: i64,
        /// Why.
        reason: TreasuryReason,
    },
}

/// One change to store.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreOp {
    /// A character's options and offline look.
    Settings {
        /// The character.
        character: Uuid,
        /// The settings.
        settings: SocialSettings,
    },
    /// A new link.
    AddLink {
        /// The character.
        character: Uuid,
        /// The other.
        other: Uuid,
        /// The kind.
        kind: LinkKind,
    },
    /// A link that ended.
    RemoveLink {
        /// The character.
        character: Uuid,
        /// The other.
        other: Uuid,
        /// The kind.
        kind: LinkKind,
    },
    /// A player report.
    Report(PlayerReport),
    /// A new company with its founder.
    CreateCompany {
        /// The company.
        company: Company,
        /// The founder (governor).
        founder: Uuid,
    },
    /// A company's editable fields.
    UpdateCompany(Company),
    /// A company with no member left.
    DeleteCompany(Uuid),
    /// A member and its rank.
    SetMember(CompanyMember),
    /// A member that left.
    RemoveMember {
        /// The character.
        character: Uuid,
        /// Unix milliseconds when it left.
        left_at_ms: u64,
    },
    /// A company invite.
    AddInvite(CompanyInvite),
    /// A company invite that ended.
    RemoveInvite {
        /// The company.
        company: Uuid,
        /// The invitee.
        character: Uuid,
    },
    /// A group with its members.
    SaveGroup(SocialGroup),
    /// A group that ended.
    DeleteGroup(Uuid),
    /// Coin owed to a character (its sim did not take it).
    AddCoinReturn(CoinReturn),
    /// Coin owed to a character that its sim took now.
    RemoveCoinReturn(Uuid),
}

/// What the database did with a treasury change ([`Effect::Treasury`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreasuryOutcome {
    /// Stored; the treasury holds this much now, hundredths of a coin.
    Stored(u64),
    /// Refused: the treasury holds less than the change takes, or the company is gone.
    Refused,
    /// The database failed for good (a refused value, not a lost connection: the service runs a
    /// change again until the database is back).
    Failed,
}
