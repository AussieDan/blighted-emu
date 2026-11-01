// SPDX-License-Identifier: AGPL-3.0-or-later
//! PostgreSQL migrations and records (M2 spec sections 5.1 and 7).
//!
//! The global database holds accounts, logins, launcher sessions, the global character name
//! table, the stats and leaderboards, and the analytics stream. Each world has its own database
//! with its characters and their saved positions, items, quests and progression, and its
//! trading post. Migrations run
//! only through `edge-http migrate`; the server checks the schema version at start.

mod account;
mod admin;
mod analytics;
mod arena;
mod character;
mod connect;
mod consumable_cooldowns;
mod entity;
mod error;
mod faction;
mod items;
mod meta_achievements;
mod migrate;
mod mounts;
mod music;
mod notifications;
mod position;
mod progression;
mod quests;
mod recipe_cooldowns;
mod session;
mod social;
mod social_graph;
mod stats;
mod status_effects;
mod trading_post;
mod travel;

#[cfg(any(test, feature = "test-util"))]
pub mod test_db;

pub use account::{
    account_by_id, create_password_account, discord_account, password_login, set_banned,
};
pub use admin::{
    AdminRank, AuditEntry, AuditRow, RankRow, admin_rank, admin_ranks, audit_rows, insert_audit,
    remove_admin_rank, set_admin_rank,
};
pub use analytics::{AnalyticsRecord, insert_analytics_events};
pub use arena::{DUNGEON_ID_LEN, SavedArena, SavedStay};
pub use character::{
    character_by_id, character_name_taken, characters_for_account, delete_character,
    insert_character, release_character_name, reserve_character_name,
};
pub use connect::{DatabaseConfig, connect, connect_lazy, connect_options};
pub use consumable_cooldowns::SavedConsumableCooldown;
pub use entity::{MAX_ENTITY_NUMBER, next_entity_number};
pub use error::DbError;
pub use faction::SavedFaction;
pub use items::{
    AccountSkins, SavedInventory, SavedItem, SavedItemRolls, SavedLoadout, SavedPlace,
    SavedStoredItem, character_inventory, character_items, collect_account_skins,
    save_character_items,
};
pub use meta_achievements::SavedSlot;
pub use migrate::{GLOBAL_MIGRATOR, WORLD_MIGRATOR, check_schema};
pub use mounts::{DYE_LEN, SavedMount, SavedMounts};
pub use music::SavedMusic;
pub use notifications::SavedNotifications;
pub use position::{SaveOutcome, SavedPosition, character_position, save_character_position};
pub use progression::{
    SavedLootLimit, SavedProgression, character_progression, save_character_progression,
};
pub use quests::{SavedQuests, character_quests, save_character_quests};
pub use recipe_cooldowns::SavedRecipeCooldown;
pub use session::{create_launcher_session, delete_launcher_session, launcher_session_account};
pub use social::{
    AccountTitle, StoredPlayer, clear_new_account_titles, earn_account_titles, stored_players,
};
pub use social_graph::{
    CREST_LEN, CROSSPLAY_LEN, Company, CompanyInvite, CompanyListing, CompanyMember, ICON_LEN,
    LinkKind, PlayerReport, STREAMER_MODE_LEN, SocialCharacter, SocialGroup, SocialRecords,
    SocialSettings, TreasuryReason, WITHDRAWAL_WINDOW_MS, add_company_invite, add_social_link,
    change_treasury, company_listings, create_company, delete_company, delete_group,
    insert_player_report, load_social, remove_company_invite, remove_company_member,
    remove_social_link, save_group, save_social_settings, set_company_member, update_company,
};
pub use sqlx::PgPool;
pub use stats::{
    BoardEntry, BoardOrder, BoardPage, GLOBAL_ENTITY, StatMerge, StatUpdate, StatValue, StatValues,
    leaderboard_entries, leaderboard_page, stat_values, submit_stats,
};
pub use status_effects::SavedStatusEffect;
pub use trading_post::{
    CharacterOrders, Completed, Delivery, DeliveryNotice, MAX_COUNTED_MATCHES, NewOrder, Order,
    OrderEnd, OrderItem, OrderKind, OrderPage, OrderRefusal, OrderSearch, OrderStatus,
    TakenDelivery, TradeSide, cancel_order, character_orders, claim_shop_purchase, complete_order,
    expire_orders, orders_by_id, post_order, record_trade, release_shop_purchase,
    return_deliveries, search_orders, take_deliveries, tax_of,
};
pub use travel::{
    PLOT_ID_LEN, SavedCamp, SavedHome, SavedHomeRecord, SavedHousingItem, SavedTravel,
};
pub use uuid::Uuid;

/// An account row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Account id.
    pub id: Uuid,
    /// Name shown to other players and in logs.
    pub display_name: String,
    /// Unix time of creation, seconds.
    pub created_at_unix: i64,
    /// A banned account cannot log in or play.
    pub banned: bool,
}

/// A character row in one world database.
#[derive(Debug, Clone, PartialEq)]
pub struct Character {
    /// Character id.
    pub id: Uuid,
    /// Owning account id (global database).
    pub account_id: Uuid,
    /// Name as the player typed it.
    pub name: String,
    /// The client's create request body, stored as received.
    pub appearance: serde_json::Value,
    /// Unix time of creation, seconds.
    pub created_at_unix: i64,
    /// The level (zero-based) when it was last online; 0 before its first time in game.
    pub level: u32,
}

/// A character to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCharacter {
    /// New character id.
    pub id: Uuid,
    /// Owning account id.
    pub account_id: Uuid,
    /// Name as the player typed it.
    pub name: String,
    /// The client's create request body.
    pub appearance: serde_json::Value,
}
