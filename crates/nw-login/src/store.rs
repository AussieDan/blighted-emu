// SPDX-License-Identifier: AGPL-3.0-or-later
//! Storage interfaces of the login logic and the stats and analytics services. PostgreSQL ([`crate::pg`]) and Valkey
//! ([`crate::valkey`]) implement them for `edge-http`; the tests use in-memory versions.

use std::future::Future;

use nw_db::{
    Account, AnalyticsRecord, BoardEntry, BoardOrder, BoardPage, Character, DiscordProfile,
    NewCharacter, StatValues,
};
use nw_protocol::launcher_api::LauncherSession;
use uuid::Uuid;

/// A store could not answer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// That username is already registered.
    #[error("username is already registered")]
    DuplicateUsername,
    /// That character name is used in some world.
    #[error("character name is taken")]
    DuplicateCharacterName,
    /// No world with that id is configured.
    #[error("unknown world")]
    UnknownWorld,
    /// PostgreSQL or Valkey failed.
    #[error("store is unavailable")]
    Unavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    /// Wraps a backend error.
    pub fn unavailable(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Unavailable(Box::new(error))
    }
}

/// Accounts and launcher sessions (global database).
pub trait AccountStore: Clone + Send + Sync + 'static {
    /// Creates an account with a password login; `username` is already lower case.
    fn create_password_account(
        &self,
        username: &str,
        password_hash: &str,
    ) -> impl Future<Output = Result<Account, StoreError>> + Send;

    /// The account and stored hash of a lower-case username.
    fn password_login(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<(Account, String)>, StoreError>> + Send;

    /// Finds or creates the account of a Discord user and stores the user's newest Discord
    /// username and email.
    fn discord_account(
        &self,
        profile: &DiscordProfile,
    ) -> impl Future<Output = Result<Account, StoreError>> + Send;

    /// The Discord user id of an account's Discord login, if it has one.
    fn discord_user_id(
        &self,
        account_id: Uuid,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send;

    /// Stores a launcher session by token hash.
    fn create_launcher_session(
        &self,
        token_hash: [u8; 32],
        account_id: Uuid,
        expires_at: i64,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The account of a live launcher session.
    fn launcher_session_account(
        &self,
        token_hash: [u8; 32],
    ) -> impl Future<Output = Result<Option<Account>, StoreError>> + Send;

    /// Ends a launcher session.
    fn delete_launcher_session(
        &self,
        token_hash: [u8; 32],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// Game session keys (Valkey).
pub trait GameSessionStore: Clone + Send + Sync + 'static {
    /// Stores a new key for `account_id`; it must be used within `claim_seconds`.
    fn create(
        &self,
        key: &str,
        account_id: Uuid,
        claim_seconds: u64,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The account of a live key; extends its life to `ttl_seconds`.
    fn resolve(
        &self,
        key: &str,
        ttl_seconds: u64,
    ) -> impl Future<Output = Result<Option<Uuid>, StoreError>> + Send;
}

/// Characters (world databases) and their names (global database).
pub trait CharacterStore: Clone + Send + Sync + 'static {
    /// The characters of an account in a world, oldest first.
    fn characters(
        &self,
        world_id: Uuid,
        account_id: Uuid,
    ) -> impl Future<Output = Result<Vec<Character>, StoreError>> + Send;

    /// One character of a world.
    fn character(
        &self,
        world_id: Uuid,
        character_id: Uuid,
    ) -> impl Future<Output = Result<Option<Character>, StoreError>> + Send;

    /// Whether a character name (compared in lower case) is used in any world.
    fn name_taken(&self, name: &str) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Reserves the name over all worlds, then inserts the character into its world; frees the
    /// name again when the insert fails (M2 spec section 7).
    fn create(
        &self,
        world_id: Uuid,
        new: &NewCharacter,
    ) -> impl Future<Output = Result<Character, StoreError>> + Send;

    /// Deletes the character `character_id` of `account_id` from its world and frees its name.
    /// Returns whether the account had that character there.
    fn delete(
        &self,
        world_id: Uuid,
        account_id: Uuid,
        character_id: Uuid,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;
}

/// Counters for rate limits (Valkey).
pub trait RateLimiter: Clone + Send + Sync + 'static {
    /// Adds one to `key`; a new counter lives for `window_seconds`. Returns the new count.
    fn increment(
        &self,
        key: &str,
        window_seconds: u64,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Takes one back from a live counter above 0; does nothing when `key` is absent or expired.
    fn release(&self, key: &str) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// State of one Discord login attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordAttempt {
    /// No such attempt, or it expired.
    Unknown,
    /// Started; the player has not finished in the browser.
    Pending,
    /// Finished. Returned once, then removed.
    Ready(LauncherSession),
}

/// Discord login attempts (Valkey).
pub trait DiscordStateStore: Clone + Send + Sync + 'static {
    /// Starts an attempt with its PKCE verifier.
    fn begin(
        &self,
        state: &str,
        verifier: &str,
        ttl_seconds: u64,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The PKCE verifier of a started attempt, one time only: the attempt stays pending, and a
    /// second call returns `None`, so a repeated callback cannot finish the login again.
    fn take_verifier(
        &self,
        state: &str,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send;

    /// Stores the launcher session of a finished attempt.
    fn complete(
        &self,
        state: &str,
        session: &LauncherSession,
        ttl_seconds: u64,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The attempt state; `Ready` is removed when it is returned.
    fn poll(&self, state: &str) -> impl Future<Output = Result<DiscordAttempt, StoreError>> + Send;
}

/// Stats and leaderboards (global database).
pub trait StatsStore: Clone + Send + Sync + 'static {
    /// Page `page_number` (from 1) of `page_size` rows of a leaderboard.
    fn leaderboard_page(
        &self,
        namespace: &str,
        board_id: &str,
        order: BoardOrder,
        page_number: u32,
        page_size: u32,
    ) -> impl Future<Output = Result<BoardPage, StoreError>> + Send;

    /// The rows of `entity_ids` on a leaderboard; an entity with no value has no row.
    fn leaderboard_entries(
        &self,
        namespace: &str,
        board_id: &str,
        order: BoardOrder,
        entity_ids: &[String],
    ) -> impl Future<Output = Result<Vec<BoardEntry>, StoreError>> + Send;

    /// The values of `stat_ids` in `scope` for `entity_ids`.
    fn stat_values(
        &self,
        namespace: &str,
        scope: &str,
        stat_ids: &[String],
        entity_ids: &[String],
    ) -> impl Future<Output = Result<StatValues, StoreError>> + Send;
}

/// The analytics stream (global database).
pub trait AnalyticsStore: Clone + Send + Sync + 'static {
    /// Stores `records` of `stream_name`; returns their ids in order.
    fn put_records(
        &self,
        stream_name: &str,
        records: &[AnalyticsRecord],
    ) -> impl Future<Output = Result<Vec<i64>, StoreError>> + Send;
}
