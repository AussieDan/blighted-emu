// SPDX-License-Identifier: AGPL-3.0-or-later
//! Accounts and their logins in the global database. Names and passwords are checked and hashed
//! by `nw-login`; this module only stores what it gets.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, is_unique_violation};
use crate::{Account, DiscordProfile};

const PASSWORD_LOGIN_PKEY: &str = "password_login_pkey";
const DISCORD_LOGIN_PKEY: &str = "discord_login_pkey";
/// One Discord login per account (`discord_login.account_id UNIQUE`).
const DISCORD_LOGIN_ACCOUNT_KEY: &str = "discord_login_account_id_key";

/// Columns of [`Account`], in this order, for `query_as`.
const ACCOUNT_COLUMNS: &str = "account.account_id, account.display_name, \
     CAST(EXTRACT(EPOCH FROM account.created_at) AS BIGINT), account.banned";

type AccountRow = (Uuid, String, i64, bool);

fn account_from_row((id, display_name, created_at_unix, banned): AccountRow) -> Account {
    Account {
        id,
        display_name,
        created_at_unix,
        banned,
    }
}

/// Creates an account with a password login. `username` must already be in lower case.
pub async fn create_password_account(
    pool: &PgPool,
    username: &str,
    password_hash: &str,
) -> Result<Account, DbError> {
    let mut transaction = pool.begin().await?;
    let account = insert_account(&mut transaction, username).await?;
    let inserted = sqlx::query(
        "INSERT INTO password_login (username, account_id, password_hash) VALUES ($1, $2, $3)",
    )
    .bind(username)
    .bind(account.id)
    .bind(password_hash)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error, PASSWORD_LOGIN_PKEY) => {
            return Err(DbError::DuplicateUsername);
        }
        Err(error) => return Err(error.into()),
    }
    transaction.commit().await?;
    Ok(account)
}

/// The account and stored password hash of a lower-case username.
pub async fn password_login(
    pool: &PgPool,
    username: &str,
) -> Result<Option<(Account, String)>, DbError> {
    let row: Option<(Uuid, String, i64, bool, String)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {ACCOUNT_COLUMNS}, password_login.password_hash FROM password_login \
             JOIN account ON account.account_id = password_login.account_id \
             WHERE password_login.username = $1"
        )))
        .bind(username)
        .fetch_optional(pool)
        .await?;
    Ok(
        row.map(|(id, display_name, created_at_unix, banned, hash)| {
            (
                account_from_row((id, display_name, created_at_unix, banned)),
                hash,
            )
        }),
    )
}

/// Finds the account of a Discord user, or creates it with the display name of `profile`. Each
/// login writes the Discord username and email of `profile` again, so staff see the newest ones.
pub async fn discord_account(pool: &PgPool, profile: &DiscordProfile) -> Result<Account, DbError> {
    if let Some(account) = account_by_discord(pool, &profile.user_id).await? {
        update_discord_contact(pool, profile).await?;
        return Ok(account);
    }
    let mut transaction = pool.begin().await?;
    let account = insert_account(&mut transaction, &profile.display_name).await?;
    let inserted = sqlx::query(
        "INSERT INTO discord_login \
         (discord_user_id, account_id, discord_username, email, email_verified, last_login_at) \
         VALUES ($1, $2, $3, $4, $5, now())",
    )
    .bind(&profile.user_id)
    .bind(account.id)
    .bind(&profile.username)
    .bind(&profile.email)
    .bind(profile.email_verified)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(_) => {
            transaction.commit().await?;
            Ok(account)
        }
        // Another request created the account first; use that one.
        Err(error) if is_unique_violation(&error, DISCORD_LOGIN_PKEY) => {
            drop(transaction);
            let account = account_by_discord(pool, &profile.user_id)
                .await?
                .ok_or(DbError::Database(Box::new(error)))?;
            update_discord_contact(pool, profile).await?;
            Ok(account)
        }
        Err(error) => Err(error.into()),
    }
}

async fn update_discord_contact(pool: &PgPool, profile: &DiscordProfile) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE discord_login SET discord_username = $2, email = $3, email_verified = $4, \
         last_login_at = now() WHERE discord_user_id = $1",
    )
    .bind(&profile.user_id)
    .bind(&profile.username)
    .bind(&profile.email)
    .bind(profile.email_verified)
    .execute(pool)
    .await?;
    Ok(())
}

/// Points the Discord user `discord_user_id` at `account_id`, so its next Discord login opens
/// that account and its characters. A Discord login of another account moves over; the caller
/// checks first that the other account has nothing to lose. The Discord username and email are
/// written at the next login.
pub async fn link_discord_login(
    pool: &PgPool,
    discord_user_id: &str,
    account_id: Uuid,
) -> Result<(), DbError> {
    let linked = sqlx::query(
        "INSERT INTO discord_login (discord_user_id, account_id) VALUES ($1, $2) \
         ON CONFLICT (discord_user_id) DO UPDATE SET account_id = EXCLUDED.account_id",
    )
    .bind(discord_user_id)
    .bind(account_id)
    .execute(pool)
    .await;
    match linked {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error, DISCORD_LOGIN_ACCOUNT_KEY) => {
            Err(DbError::AccountHasDiscordLogin)
        }
        Err(error) => Err(error.into()),
    }
}

/// Loads one account.
pub async fn account_by_id(pool: &PgPool, account_id: Uuid) -> Result<Option<Account>, DbError> {
    let row: Option<AccountRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM account WHERE account.account_id = $1"
    )))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(account_from_row))
}

/// Sets or clears the ban flag. Returns false when the account does not exist.
pub async fn set_banned(pool: &PgPool, account_id: Uuid, banned: bool) -> Result<bool, DbError> {
    let result = sqlx::query("UPDATE account SET banned = $2 WHERE account_id = $1")
        .bind(account_id)
        .bind(banned)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The Discord user id of an account's Discord login, if it has one.
pub async fn discord_user_id(pool: &PgPool, account_id: Uuid) -> Result<Option<String>, DbError> {
    let id = sqlx::query_scalar("SELECT discord_user_id FROM discord_login WHERE account_id = $1")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}

/// The account of a Discord user.
pub async fn account_by_discord(
    pool: &PgPool,
    discord_user_id: &str,
) -> Result<Option<Account>, DbError> {
    let row: Option<AccountRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM discord_login \
         JOIN account ON account.account_id = discord_login.account_id \
         WHERE discord_login.discord_user_id = $1"
    )))
    .bind(discord_user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(account_from_row))
}

async fn insert_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    display_name: &str,
) -> Result<Account, DbError> {
    let row: AccountRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO account (account_id, display_name) VALUES ($1, $2) \
         RETURNING {ACCOUNT_COLUMNS}"
    )))
    .bind(Uuid::new_v4())
    .bind(display_name)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(account_from_row(row))
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use crate::migrate::GLOBAL_MIGRATOR;
    use crate::test_db::with_test_schema;

    const USERNAME: &str = "ada";
    const HASH: &str = "$argon2id$stored";
    const DISCORD_ID: &str = "80351110224678912";
    const DISCORD_NAME: &str = "Nelly";
    const DISCORD_USERNAME: &str = "nelly";
    const DISCORD_EMAIL: &str = "nelly@example.org";

    #[tokio::test]
    async fn password_accounts_are_unique_and_found_by_username() {
        let (created, found, duplicate, missing) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let created = create_password_account(pool, USERNAME, HASH)
                .await
                .expect("create");
            let found = password_login(pool, USERNAME).await.expect("find");
            let duplicate = create_password_account(pool, USERNAME, HASH).await;
            let missing = password_login(pool, "nobody").await.expect("missing");
            (created, found, duplicate, missing)
        })
        .await;
        assert_eq!(created.display_name, USERNAME);
        assert!(!created.banned);
        assert!(created.created_at_unix > 0);
        assert_eq!(found, Some((created, HASH.to_owned())));
        assert!(matches!(duplicate, Err(DbError::DuplicateUsername)));
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn discord_account_is_created_once_and_keeps_the_newest_contact() {
        let first_login = DiscordProfile {
            user_id: DISCORD_ID.to_owned(),
            username: DISCORD_USERNAME.to_owned(),
            display_name: DISCORD_NAME.to_owned(),
            email: None,
            email_verified: false,
        };
        let second_login = DiscordProfile {
            username: "nelly_new".to_owned(),
            display_name: "renamed".to_owned(),
            email: Some(DISCORD_EMAIL.to_owned()),
            email_verified: true,
            ..first_login.clone()
        };
        let (first, second, contact, rank, user_id) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let first = discord_account(pool, &first_login).await.expect("first");
            let second = discord_account(pool, &second_login).await.expect("second");
            let rank = crate::admin_rank(pool, first.id).await.expect("rank");
            let user_id = discord_user_id(pool, first.id).await.expect("user id");
            let contact: (String, Option<String>, bool, bool) = sqlx::query_as(
                "SELECT discord_username, email, email_verified, last_login_at IS NOT NULL \
                 FROM discord_login WHERE discord_user_id = $1",
            )
            .bind(DISCORD_ID)
            .fetch_one(pool)
            .await
            .expect("contact");
            (first, second, contact, rank, user_id)
        })
        .await;
        assert_eq!(first, second);
        assert_eq!(rank, None, "a new Discord account is a normal player");
        assert_eq!(user_id.as_deref(), Some(DISCORD_ID));
        assert_eq!(first.display_name, DISCORD_NAME);
        assert_eq!(
            contact,
            (
                "nelly_new".to_owned(),
                Some(DISCORD_EMAIL.to_owned()),
                true,
                true
            )
        );
    }

    #[tokio::test]
    async fn a_linked_discord_login_opens_the_old_account_and_moves_from_a_new_one() {
        let profile = DiscordProfile {
            user_id: DISCORD_ID.to_owned(),
            username: DISCORD_USERNAME.to_owned(),
            display_name: DISCORD_NAME.to_owned(),
            email: Some(DISCORD_EMAIL.to_owned()),
            email_verified: true,
        };
        let (local, first_discord, linked, login, other_user) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let local = create_password_account(pool, USERNAME, HASH)
                .await
                .expect("create");
            let first_discord = discord_account(pool, &profile).await.expect("new account");
            link_discord_login(pool, DISCORD_ID, local.id)
                .await
                .expect("link");
            let linked = account_by_discord(pool, DISCORD_ID).await.expect("linked");
            let login = discord_account(pool, &profile).await.expect("login");
            let other_user = link_discord_login(pool, "175928847299117063", local.id).await;
            (local, first_discord, linked, login, other_user)
        })
        .await;
        assert_ne!(first_discord.id, local.id);
        assert_eq!(linked.as_ref(), Some(&local));
        assert_eq!(login, local);
        assert!(matches!(other_user, Err(DbError::AccountHasDiscordLogin)));
    }

    #[tokio::test]
    async fn ban_flag_is_stored() {
        let (banned, loaded, unknown) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let account = create_password_account(pool, USERNAME, HASH)
                .await
                .expect("create");
            let banned = set_banned(pool, account.id, true).await.expect("ban");
            let loaded = account_by_id(pool, account.id).await.expect("load");
            let unknown = set_banned(pool, Uuid::new_v4(), true)
                .await
                .expect("unknown");
            (banned, loaded, unknown)
        })
        .await;
        assert!(banned);
        assert!(loaded.expect("row").banned);
        assert!(!unknown);
    }
}
