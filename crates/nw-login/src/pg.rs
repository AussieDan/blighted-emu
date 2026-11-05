// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`AccountStore`], [`CharacterStore`], [`StatsStore`], [`AnalyticsStore`] and
//! [`PrivacyStore`] on PostgreSQL.

use std::collections::HashMap;
use std::sync::Arc;

use nw_db::{
    Account, AccountEntitlement, AnalyticsRecord, BoardEntry, BoardOrder, BoardPage, Character,
    DbError, DiscordProfile, NewCharacter, PgPool, PrivacySetting, StatValues, Wallet,
    account_by_id, account_entitlements, privacy_settings, set_privacy_settings, wallet,
};
use uuid::Uuid;

use crate::store::{
    AccountStore, AnalyticsStore, CharacterStore, EntitlementStore, PrivacyStore, StatsStore,
    StoreError,
};

/// Accounts and launcher sessions in PostgreSQL.
#[derive(Debug, Clone)]
pub struct PgAccounts {
    pool: PgPool,
}

impl PgAccounts {
    /// Uses a pool of the migrated global database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn store_error(error: DbError) -> StoreError {
    match error {
        DbError::DuplicateUsername => StoreError::DuplicateUsername,
        DbError::DuplicateCharacterName => StoreError::DuplicateCharacterName,
        other => StoreError::unavailable(other),
    }
}

/// Reserves the name, inserts the character, and frees the name when the insert fails.
async fn create_character(
    global: &PgPool,
    world: &PgPool,
    world_id: Uuid,
    new: &NewCharacter,
) -> Result<Character, StoreError> {
    nw_db::reserve_character_name(global, &new.name, world_id, new.id)
        .await
        .map_err(store_error)?;
    match nw_db::insert_character(world, new).await {
        Ok(character) => Ok(character),
        Err(error) => {
            if let Err(release) = nw_db::release_character_name(global, new.id).await {
                tracing::error!(error = %release, "character name not released");
            }
            Err(store_error(error))
        }
    }
}

/// Deletes a character from its world, then frees its name.
async fn delete_character(
    global: &PgPool,
    world: &PgPool,
    account_id: Uuid,
    character_id: Uuid,
) -> Result<bool, StoreError> {
    let deleted = nw_db::delete_character(world, account_id, character_id)
        .await
        .map_err(store_error)?;
    if deleted {
        nw_db::release_character_name(global, character_id)
            .await
            .map_err(store_error)?;
    }
    Ok(deleted)
}

/// Characters in the world databases and their names in the global database.
#[derive(Debug, Clone)]
pub struct PgCharacters {
    global: PgPool,
    worlds: Arc<HashMap<Uuid, PgPool>>,
}

impl PgCharacters {
    /// Uses the migrated global database and one migrated database per world id.
    pub fn new(global: PgPool, worlds: HashMap<Uuid, PgPool>) -> Self {
        Self {
            global,
            worlds: Arc::new(worlds),
        }
    }

    fn world(&self, world_id: Uuid) -> Result<&PgPool, StoreError> {
        self.worlds.get(&world_id).ok_or(StoreError::UnknownWorld)
    }
}

impl CharacterStore for PgCharacters {
    async fn characters(
        &self,
        world_id: Uuid,
        account_id: Uuid,
    ) -> Result<Vec<Character>, StoreError> {
        nw_db::characters_for_account(self.world(world_id)?, account_id)
            .await
            .map_err(store_error)
    }

    async fn character(
        &self,
        world_id: Uuid,
        character_id: Uuid,
    ) -> Result<Option<Character>, StoreError> {
        nw_db::character_by_id(self.world(world_id)?, character_id)
            .await
            .map_err(store_error)
    }

    async fn name_taken(&self, name: &str) -> Result<bool, StoreError> {
        nw_db::character_name_taken(&self.global, name)
            .await
            .map_err(store_error)
    }

    async fn account_banned(&self, account: Uuid) -> Result<bool, StoreError> {
        Ok(account_by_id(&self.global, account)
            .await
            .map_err(store_error)?
            .is_some_and(|found| found.banned))
    }

    /// Runs in its own task, so a dropped request cannot stop between the name reservation and
    /// the insert or the release (review finding 2 of plan M2-B).
    async fn create(&self, world_id: Uuid, new: &NewCharacter) -> Result<Character, StoreError> {
        let world = self.world(world_id)?.clone();
        let global = self.global.clone();
        let new = new.clone();
        tokio::spawn(async move { create_character(&global, &world, world_id, &new).await })
            .await
            .map_err(StoreError::unavailable)?
    }

    /// Runs in its own task, so a dropped request cannot stop between the delete and the name
    /// release.
    async fn delete(
        &self,
        world_id: Uuid,
        account_id: Uuid,
        character_id: Uuid,
    ) -> Result<bool, StoreError> {
        let world = self.world(world_id)?.clone();
        let global = self.global.clone();
        tokio::spawn(
            async move { delete_character(&global, &world, account_id, character_id).await },
        )
        .await
        .map_err(StoreError::unavailable)?
    }
}

impl AccountStore for PgAccounts {
    async fn create_password_account(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<Account, StoreError> {
        nw_db::create_password_account(&self.pool, username, password_hash)
            .await
            .map_err(store_error)
    }

    async fn password_login(
        &self,
        username: &str,
    ) -> Result<Option<(Account, String)>, StoreError> {
        nw_db::password_login(&self.pool, username)
            .await
            .map_err(store_error)
    }

    async fn discord_account(&self, profile: &DiscordProfile) -> Result<Account, StoreError> {
        nw_db::discord_account(&self.pool, profile)
            .await
            .map_err(store_error)
    }

    async fn discord_user_id(&self, account_id: Uuid) -> Result<Option<String>, StoreError> {
        nw_db::discord_user_id(&self.pool, account_id)
            .await
            .map_err(store_error)
    }

    async fn create_launcher_session(
        &self,
        token_hash: [u8; 32],
        account_id: Uuid,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        nw_db::create_launcher_session(&self.pool, &token_hash, account_id, expires_at)
            .await
            .map_err(store_error)
    }

    async fn launcher_session_account(
        &self,
        token_hash: [u8; 32],
    ) -> Result<Option<Account>, StoreError> {
        nw_db::launcher_session_account(&self.pool, &token_hash)
            .await
            .map_err(store_error)
    }

    async fn delete_launcher_session(&self, token_hash: [u8; 32]) -> Result<(), StoreError> {
        nw_db::delete_launcher_session(&self.pool, &token_hash)
            .await
            .map_err(store_error)
    }
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use nw_db::test_db::with_test_schema;
    use nw_db::{GLOBAL_MIGRATOR, WORLD_MIGRATOR};

    use super::*;
    use crate::memory::contract;

    const NAME: &str = "Ada";
    /// Time after which the waiting create is dropped.
    const CANCEL_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
    /// Longest wait for the dropped create to finish.
    const FINISH_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
    const LOCK_CHARACTERS: &str = "LOCK TABLE player_character IN ACCESS EXCLUSIVE MODE";

    #[tokio::test]
    async fn pg_accounts_follow_the_contract() {
        with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            contract::accounts(PgAccounts::new(pool.clone())).await;
        })
        .await;
    }

    #[tokio::test]
    async fn pg_characters_follow_the_contract() {
        with_test_schema(async |global| {
            GLOBAL_MIGRATOR.run(global).await.expect("migrate global");
            with_test_schema(async |first| {
                WORLD_MIGRATOR.run(first).await.expect("migrate first");
                with_test_schema(async |second| {
                    WORLD_MIGRATOR.run(second).await.expect("migrate second");
                    let (first_id, second_id) = (Uuid::new_v4(), Uuid::new_v4());
                    let worlds =
                        HashMap::from([(first_id, first.clone()), (second_id, second.clone())]);
                    let store = PgCharacters::new(global.clone(), worlds);
                    contract::characters(store, first_id, second_id).await;
                })
                .await;
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn a_dropped_create_still_finishes() {
        with_test_schema(async |global| {
            GLOBAL_MIGRATOR.run(global).await.expect("migrate global");
            with_test_schema(async |world| {
                WORLD_MIGRATOR.run(world).await.expect("migrate world");
                let world_id = Uuid::new_v4();
                let store =
                    PgCharacters::new(global.clone(), HashMap::from([(world_id, world.clone())]));
                // A lock in another transaction holds the world insert back.
                let mut blocker = world.begin().await.expect("begin");
                sqlx::query(LOCK_CHARACTERS)
                    .execute(&mut *blocker)
                    .await
                    .expect("lock");
                let account = Uuid::new_v4();
                let new = NewCharacter {
                    id: Uuid::new_v4(),
                    account_id: account,
                    name: NAME.to_owned(),
                    appearance: serde_json::json!({}),
                };
                let dropped =
                    tokio::time::timeout(CANCEL_AFTER, store.create(world_id, &new)).await;
                assert!(dropped.is_err(), "the create must still wait for the lock");
                blocker.rollback().await.expect("unlock");
                tokio::time::timeout(FINISH_LIMIT, async {
                    while store
                        .characters(world_id, account)
                        .await
                        .expect("list")
                        .is_empty()
                    {
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                })
                .await
                .expect("the dropped create finished its insert");
                assert!(store.name_taken(NAME).await.expect("name"));
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn failed_world_insert_frees_the_name() {
        with_test_schema(async |global| {
            GLOBAL_MIGRATOR.run(global).await.expect("migrate global");
            // The world database is not migrated, so the insert fails.
            with_test_schema(async |world| {
                let world_id = Uuid::new_v4();
                let store =
                    PgCharacters::new(global.clone(), HashMap::from([(world_id, world.clone())]));
                let new = NewCharacter {
                    id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    name: NAME.to_owned(),
                    appearance: serde_json::json!({}),
                };
                assert!(matches!(
                    store.create(world_id, &new).await,
                    Err(StoreError::Unavailable(_))
                ));
                assert!(!store.name_taken(NAME).await.expect("name"));
            })
            .await;
        })
        .await;
    }
}

/// Stats, leaderboards and the analytics stream in the global database.
#[derive(Debug, Clone)]
pub struct PgStats {
    pool: PgPool,
}

impl PgStats {
    /// Uses a pool of the migrated global database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The store entitlements of accounts in the global database.
#[derive(Debug, Clone)]
pub struct PgEntitlements {
    pool: PgPool,
}

impl PgEntitlements {
    /// Uses a pool of the migrated global database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl EntitlementStore for PgEntitlements {
    async fn held(&self, account: Uuid) -> Result<Vec<AccountEntitlement>, StoreError> {
        account_entitlements(&self.pool, account)
            .await
            .map_err(StoreError::unavailable)
    }
    async fn wallet(&self, account: Uuid) -> Result<Wallet, StoreError> {
        wallet(&self.pool, account)
            .await
            .map_err(StoreError::unavailable)
    }
}

impl StatsStore for PgStats {
    async fn leaderboard_page(
        &self,
        namespace: &str,
        board_id: &str,
        order: BoardOrder,
        page_number: u32,
        page_size: u32,
    ) -> Result<BoardPage, StoreError> {
        nw_db::leaderboard_page(
            &self.pool,
            namespace,
            board_id,
            order,
            page_number,
            page_size,
        )
        .await
        .map_err(store_error)
    }

    async fn leaderboard_entries(
        &self,
        namespace: &str,
        board_id: &str,
        order: BoardOrder,
        entity_ids: &[String],
    ) -> Result<Vec<BoardEntry>, StoreError> {
        nw_db::leaderboard_entries(&self.pool, namespace, board_id, order, entity_ids)
            .await
            .map_err(store_error)
    }

    async fn stat_values(
        &self,
        namespace: &str,
        scope: &str,
        stat_ids: &[String],
        entity_ids: &[String],
    ) -> Result<StatValues, StoreError> {
        nw_db::stat_values(&self.pool, namespace, scope, stat_ids, entity_ids)
            .await
            .map_err(store_error)
    }
}

impl AnalyticsStore for PgStats {
    async fn put_records(
        &self,
        stream_name: &str,
        records: &[AnalyticsRecord],
    ) -> Result<Vec<i64>, StoreError> {
        nw_db::insert_analytics_events(&self.pool, stream_name, records)
            .await
            .map_err(store_error)
    }

    async fn delete_older_than(&self, retention_seconds: u64) -> Result<u64, StoreError> {
        nw_db::delete_old_analytics_events(&self.pool, retention_seconds)
            .await
            .map_err(store_error)
    }
}

/// The privacy settings of accounts in the global database.
#[derive(Debug, Clone)]
pub struct PgPrivacy {
    pool: PgPool,
}

impl PgPrivacy {
    /// Uses a pool of the migrated global database.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl PrivacyStore for PgPrivacy {
    async fn settings(&self, account: Uuid) -> Result<Vec<PrivacySetting>, StoreError> {
        privacy_settings(&self.pool, account)
            .await
            .map_err(store_error)
    }

    async fn set(
        &self,
        account: Uuid,
        settings: &[(String, String)],
    ) -> Result<Vec<PrivacySetting>, StoreError> {
        set_privacy_settings(&self.pool, account, settings)
            .await
            .map_err(store_error)
    }
}
