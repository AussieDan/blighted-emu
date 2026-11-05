// SPDX-License-Identifier: AGPL-3.0-or-later
//! The social state of one world (run 15 social study): who is online on which sim, chat
//! routing with the profanity filter and the rate limit, friends, blocks and chat mutes, groups,
//! companies with ranks, invites and treasury, and voice-chat setup. One `social` service per
//! world keeps it in memory; it is loaded from the world database at start and every change is
//! stored ([`Effect::Store`]). The state takes inputs and gives [`Effect`]s, so it runs without
//! NATS or PostgreSQL in tests.

mod chat;
mod companies;
pub mod effects;
mod groups;
mod people;
pub mod privacy;
pub mod profanity;
pub mod throttle;
mod treasury;
mod views;
pub mod voice;
mod world;

pub use companies::{
    COMPANY_MEMBER_LIMIT, CompanyRules, JOIN_UPDATE_COPIES, NEW_COMPANY_SIEGE_HOUR, other_guild,
};
pub use effects::{Effect, StoreOp, TreasuryOutcome};
pub use profanity::ProfanityFilter;
pub use voice::VoiceService;
pub use world::{Loaded, SocialRules, SocialWorld};
