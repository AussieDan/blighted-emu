// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admin site ranks and the audit log in the global database (admin studio spec section 9).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// A rank on the admin site, lowest first: a higher rank can do all a lower one can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRank {
    /// Support helper: looks at players, moves self to them, mutes and kicks.
    Csr,
    /// Game master: live commands and events.
    GameMaster,
    /// Head admin: everything, and gives ranks.
    HeadAdmin,
}

/// Every rank with its stored name.
const RANK_NAMES: [(AdminRank, &str); 3] = [
    (AdminRank::Csr, "csr"),
    (AdminRank::GameMaster, "game_master"),
    (AdminRank::HeadAdmin, "head_admin"),
];

impl AdminRank {
    /// The stored name.
    pub fn name(self) -> &'static str {
        RANK_NAMES
            .iter()
            .find(|(rank, _)| *rank == self)
            .map_or("", |(_, name)| name)
    }

    /// The rank of a stored name.
    pub fn from_name(name: &str) -> Option<Self> {
        RANK_NAMES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(rank, _)| *rank)
    }
}

/// A new audit row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuditEntry {
    /// The account; `None` in local mode.
    pub account: Option<Uuid>,
    /// Its rank at the time.
    pub rank: Option<AdminRank>,
    /// The command, for example `teleport` or `login`.
    pub command: String,
    /// What it acted on, for example `player 41`.
    pub target: String,
    /// The command's values.
    pub detail: serde_json::Value,
    /// `done`, `not_mine`, `failed: <why>`, `timeout`, `refused`.
    pub result: String,
}

/// A stored audit row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuditRow {
    /// Row id, rising.
    pub id: i64,
    /// Unix time in milliseconds.
    pub at_unix_ms: i64,
    /// The entry.
    pub entry: AuditEntry,
}

/// The rank of `account`, if it has one.
pub async fn admin_rank(pool: &PgPool, account: Uuid) -> Result<Option<AdminRank>, DbError> {
    let name: Option<(String,)> =
        sqlx::query_as("SELECT rank FROM admin_rank WHERE account_id = $1")
            .bind(account)
            .fetch_optional(pool)
            .await?;
    Ok(name.and_then(|(name,)| AdminRank::from_name(&name)))
}

/// Gives `account` the rank `rank` (replacing any rank it had), given by `given_by`.
pub async fn set_admin_rank(
    pool: &PgPool,
    account: Uuid,
    rank: AdminRank,
    given_by: Option<Uuid>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO admin_rank (account_id, rank, given_by) VALUES ($1, $2, $3) \
         ON CONFLICT (account_id) DO UPDATE SET rank = $2, given_by = $3, given_at = now()",
    )
    .bind(account)
    .bind(rank.name())
    .bind(given_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes the rank of `account`; `false` when it had none.
pub async fn remove_admin_rank(pool: &PgPool, account: Uuid) -> Result<bool, DbError> {
    let done = sqlx::query("DELETE FROM admin_rank WHERE account_id = $1")
        .bind(account)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// One account with an admin rank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RankRow {
    /// The account.
    pub account: Uuid,
    /// Its password login name, else its Discord username, when it has one.
    pub username: Option<String>,
    /// The rank.
    pub rank: AdminRank,
    /// Who gave it.
    pub given_by: Option<Uuid>,
    /// When, Unix time in milliseconds.
    pub given_at_unix_ms: i64,
}

/// A row of `admin_rank` with the login name: account, name, rank, given by, given at (ms).
type RankRecord = (Uuid, Option<String>, String, Option<Uuid>, i64);

/// Every account with an admin rank, highest rank first, then by name.
pub async fn admin_ranks(pool: &PgPool) -> Result<Vec<RankRow>, DbError> {
    let rows: Vec<RankRecord> = sqlx::query_as(
        "SELECT admin_rank.account_id, \
         COALESCE(password_login.username, discord_login.discord_username), admin_rank.rank, \
         admin_rank.given_by, CAST(EXTRACT(EPOCH FROM admin_rank.given_at) * 1000 AS BIGINT) \
         FROM admin_rank \
         LEFT JOIN password_login ON password_login.account_id = admin_rank.account_id \
         LEFT JOIN discord_login ON discord_login.account_id = admin_rank.account_id",
    )
    .fetch_all(pool)
    .await?;
    let mut ranks: Vec<RankRow> = rows
        .into_iter()
        .filter_map(|(account, username, rank, given_by, given_at_unix_ms)| {
            Some(RankRow {
                account,
                username,
                rank: AdminRank::from_name(&rank)?,
                given_by,
                given_at_unix_ms,
            })
        })
        .collect();
    ranks.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| left.username.cmp(&right.username))
    });
    Ok(ranks)
}

/// Adds an audit row; returns its id.
pub async fn insert_audit(pool: &PgPool, entry: &AuditEntry) -> Result<i64, DbError> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO admin_audit (account_id, rank, command, target, detail, result) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(entry.account)
    .bind(entry.rank.map(AdminRank::name))
    .bind(&entry.command)
    .bind(&entry.target)
    .bind(&entry.detail)
    .bind(&entry.result)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

type AuditRecord = (
    i64,
    i64,
    Option<Uuid>,
    Option<String>,
    String,
    String,
    serde_json::Value,
    String,
);

/// The newest `limit` audit rows, newest first; only those of `account` when it is set.
pub async fn audit_rows(
    pool: &PgPool,
    account: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AuditRow>, DbError> {
    let rows: Vec<AuditRecord> = sqlx::query_as(
        "SELECT id, CAST(EXTRACT(EPOCH FROM at) * 1000 AS BIGINT), account_id, rank, command, \
         target, detail, result FROM admin_audit \
         WHERE $1::uuid IS NULL OR account_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(account)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, at_unix_ms, account, rank, command, target, detail, result)| AuditRow {
                id,
                at_unix_ms,
                entry: AuditEntry {
                    account,
                    rank: rank.as_deref().and_then(AdminRank::from_name),
                    command,
                    target,
                    detail,
                    result,
                },
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_ordered_and_named() {
        assert!(AdminRank::Csr < AdminRank::GameMaster);
        assert!(AdminRank::GameMaster < AdminRank::HeadAdmin);
        for (rank, name) in RANK_NAMES {
            assert_eq!(AdminRank::from_name(name), Some(rank));
            assert_eq!(rank.name(), name);
        }
        assert_eq!(AdminRank::from_name("king"), None);
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use super::*;
    use crate::DiscordProfile;
    use crate::account::{create_password_account, discord_account};
    use crate::migrate::GLOBAL_MIGRATOR;
    use crate::test_db::with_test_schema;

    const AUDIT_LIMIT: i64 = 10;

    #[tokio::test]
    async fn ranks_and_audit_rows_are_stored() {
        let (rank, changed, removed, rows, own) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let account = create_password_account(pool, "gm", "$argon2id$x")
                .await
                .expect("account");
            set_admin_rank(pool, account.id, AdminRank::Csr, None)
                .await
                .expect("set");
            let rank = admin_rank(pool, account.id).await.expect("rank");
            set_admin_rank(pool, account.id, AdminRank::GameMaster, Some(account.id))
                .await
                .expect("change");
            let changed = admin_rank(pool, account.id).await.expect("rank");
            let entry = |account| AuditEntry {
                account,
                rank: Some(AdminRank::GameMaster),
                command: "teleport".to_owned(),
                target: "player 41".to_owned(),
                detail: serde_json::json!({ "x": 1.0 }),
                result: "done".to_owned(),
            };
            insert_audit(pool, &entry(Some(account.id)))
                .await
                .expect("audit");
            insert_audit(pool, &entry(None)).await.expect("audit");
            let rows = audit_rows(pool, None, AUDIT_LIMIT).await.expect("rows");
            let own = audit_rows(pool, Some(account.id), AUDIT_LIMIT)
                .await
                .expect("own");
            let discord = discord_account(
                pool,
                &DiscordProfile {
                    user_id: "80351110224678912".to_owned(),
                    username: "nelly".to_owned(),
                    display_name: "Nelly".to_owned(),
                    email: None,
                    email_verified: false,
                },
            )
            .await
            .expect("discord account");
            set_admin_rank(pool, discord.id, AdminRank::Csr, None)
                .await
                .expect("discord rank");
            let listed = admin_ranks(pool).await.expect("ranks");
            assert_eq!(listed.len(), 2);
            assert_eq!(listed[0].username.as_deref(), Some("gm"));
            assert_eq!(listed[0].rank, AdminRank::GameMaster);
            assert_eq!(
                listed[1].username.as_deref(),
                Some("nelly"),
                "a Discord account is listed by its Discord username"
            );
            let removed = remove_admin_rank(pool, account.id).await.expect("remove");
            (rank, changed, removed, rows, own)
        })
        .await;
        assert_eq!(rank, Some(AdminRank::Csr));
        assert_eq!(changed, Some(AdminRank::GameMaster));
        assert!(removed);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].id > rows[1].id, "newest first");
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].entry.detail["x"], 1.0);
    }
}
