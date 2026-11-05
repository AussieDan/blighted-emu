// SPDX-License-Identifier: AGPL-3.0-or-later
//! The privacy settings an account set (global migration 0006): what the client's privacy settings
//! service stores, and what the social services read to follow them.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// The setting of who a player writes with in public and group chat (retail alias).
pub const PRIVACY_PUBLIC_CHAT: &str = "who-can-I-communicate-with-via-public-or-group-text-chat";
/// The setting of who sends the player a friend request (retail alias).
pub const PRIVACY_FRIEND_REQUESTS: &str = "who-can-send-me-a-friend-request";
/// The setting of who a player whispers with (retail alias).
pub const PRIVACY_PRIVATE_CHAT: &str =
    "who-can-I-communicate-with-via-text-messages-or-private-text-chat";
/// The setting of who sees the player's online status (retail alias).
pub const PRIVACY_ONLINE_STATUS: &str = "who-can-see-my-online-status";
/// The setting of who a player talks with by voice (retail alias).
pub const PRIVACY_VOICE: &str = "who-can-I-communicate-with-via-voice";
/// A setting value: every player (the retail default of every setting).
pub const PRIVACY_EVERYONE: &str = "Everyone";
/// A setting value: the player's friends.
pub const PRIVACY_FRIENDS: &str = "Friends";
/// A setting value: no player.
pub const PRIVACY_NO_ONE: &str = "NoOne";
/// A setting value of the online status: only the player itself.
pub const PRIVACY_ONLY_ME: &str = "OnlyMe";

/// Milliseconds per second, for stored times.
const MS_PER_SECOND: f64 = 1000.0;
/// Seconds per millisecond, for asked times.
const SECONDS_PER_MS: f64 = 0.001;

/// One privacy setting an account set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacySetting {
    /// The setting, for example `who-can-send-me-a-friend-request`.
    pub alias: String,
    /// Its value, for example `NoOne`.
    pub value: String,
    /// Unix milliseconds of the last change.
    pub updated_at_ms: u64,
}

/// A privacy setting of some account, as the social services read the changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPrivacySetting {
    /// The account.
    pub account: Uuid,
    /// The setting.
    pub setting: PrivacySetting,
}

type SettingRow = (Uuid, String, String, f64);

fn of_row((account, alias, value, updated_at): SettingRow) -> AccountPrivacySetting {
    AccountPrivacySetting {
        account,
        setting: PrivacySetting {
            alias,
            value,
            // A stored time is after 1970 and well inside `u64` milliseconds.
            updated_at_ms: (updated_at * MS_PER_SECOND) as u64,
        },
    }
}

const SETTING_COLUMNS: &str = "account_id, alias, value, extract(epoch FROM updated_at)::float8";

/// The settings `account` set, by alias.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn privacy_settings(
    global: &PgPool,
    account: Uuid,
) -> Result<Vec<PrivacySetting>, DbError> {
    let rows: Vec<SettingRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SETTING_COLUMNS} FROM account_privacy_setting WHERE account_id = $1 \
         ORDER BY alias"
    )))
    .bind(account)
    .fetch_all(global)
    .await?;
    Ok(rows.into_iter().map(|row| of_row(row).setting).collect())
}

/// Stores `settings` (alias, value) of `account` in one transaction and returns them as stored.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (an unknown account among them); then no
/// setting is stored.
pub async fn set_privacy_settings(
    global: &PgPool,
    account: Uuid,
    settings: &[(String, String)],
) -> Result<Vec<PrivacySetting>, DbError> {
    let mut transaction = global.begin().await?;
    let mut stored = Vec::with_capacity(settings.len());
    for (alias, value) in settings {
        let row: SettingRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO account_privacy_setting (account_id, alias, value) VALUES ($1, $2, $3) \
             ON CONFLICT (account_id, alias) DO UPDATE SET value = EXCLUDED.value, \
             updated_at = now() RETURNING {SETTING_COLUMNS}"
        )))
        .bind(account)
        .bind(alias)
        .bind(value)
        .fetch_one(&mut *transaction)
        .await?;
        stored.push(of_row(row).setting);
    }
    transaction.commit().await?;
    Ok(stored)
}

/// Every setting changed at or after Unix milliseconds `since_ms`, oldest first. A reader that
/// asks again from the newest time it got reads that change again, which changes nothing.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn privacy_changes_since(
    global: &PgPool,
    since_ms: u64,
) -> Result<Vec<AccountPrivacySetting>, DbError> {
    let rows: Vec<SettingRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SETTING_COLUMNS} FROM account_privacy_setting \
         WHERE updated_at >= to_timestamp($1) ORDER BY updated_at"
    )))
    .bind(since_ms as f64 * SECONDS_PER_MS)
    .fetch_all(global)
    .await?;
    Ok(rows.into_iter().map(of_row).collect())
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use crate::migrate::GLOBAL_MIGRATOR;
    use crate::test_db::with_test_schema;

    const FRIEND_REQUESTS: &str = PRIVACY_FRIEND_REQUESTS;
    const NO_ONE: &str = PRIVACY_NO_ONE;
    const EVERYONE: &str = PRIVACY_EVERYONE;

    #[tokio::test]
    async fn a_setting_is_stored_changed_and_read_back() {
        with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let account = crate::create_password_account(pool, "ann", "hash")
                .await
                .expect("account")
                .id;
            assert!(
                privacy_settings(pool, account)
                    .await
                    .expect("read")
                    .is_empty()
            );
            let set = |value: &str| vec![(FRIEND_REQUESTS.to_owned(), value.to_owned())];
            let first = set_privacy_settings(pool, account, &set(NO_ONE))
                .await
                .expect("set");
            assert_eq!(first[0].value, NO_ONE);
            set_privacy_settings(pool, account, &set(EVERYONE))
                .await
                .expect("change");
            let read = privacy_settings(pool, account).await.expect("read");
            assert_eq!(read.len(), 1);
            assert_eq!(read[0].value, EVERYONE);
            let changes = privacy_changes_since(pool, first[0].updated_at_ms)
                .await
                .expect("changes");
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].account, account);
            assert!(
                set_privacy_settings(pool, Uuid::new_v4(), &set(NO_ONE))
                    .await
                    .is_err(),
                "an unknown account stores nothing"
            );
        })
        .await;
    }
}
