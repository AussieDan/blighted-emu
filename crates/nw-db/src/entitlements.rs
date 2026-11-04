// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store of each account (global migration 0005): the store entitlements it holds and its
//! wallet of Marks of Fortune. A store purchase pays from the wallet (nothing in a free store) and
//! adds the entitlement; the store service lists both, and the sims let a player use what its
//! account holds.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// One entitlement an account holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEntitlement {
    /// The game data's `UniqueTagID`.
    pub entitlement: String,
    /// The id the store service gives the client for it.
    pub instance: Uuid,
    /// When the account got it, Unix milliseconds.
    pub granted_at_ms: i64,
}

type EntitlementRow = (String, Uuid, i64);

/// The columns of one entitlement, its time in Unix milliseconds.
const COLUMNS: &str =
    "entitlement, instance_id, CAST(EXTRACT(EPOCH FROM granted_at) * 1000 AS BIGINT)";

fn of_row((entitlement, instance, granted_at_ms): EntitlementRow) -> AccountEntitlement {
    AccountEntitlement {
        entitlement,
        instance,
        granted_at_ms,
    }
}

/// The entitlements `account` holds, oldest first.
///
/// # Errors
///
/// Returns [`DbError`] when the query fails.
pub async fn account_entitlements(
    global: &PgPool,
    account: Uuid,
) -> Result<Vec<AccountEntitlement>, DbError> {
    let rows: Vec<EntitlementRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM account_entitlement WHERE account_id = $1 \
         ORDER BY granted_at, entitlement"
    )))
    .bind(account)
    .fetch_all(global)
    .await?;
    Ok(rows.into_iter().map(of_row).collect())
}

/// An account's Marks of Fortune, the store currency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wallet {
    /// The balance.
    pub marks: i64,
    /// When it last changed, Unix milliseconds; 0 for an account that never had any.
    pub updated_at_ms: i64,
}

type WalletRow = (i64, i64);

/// The columns of a wallet, its time in Unix milliseconds.
const WALLET_COLUMNS: &str =
    "marks_of_fortune, CAST(EXTRACT(EPOCH FROM updated_at) * 1000 AS BIGINT)";

fn wallet_of((marks, updated_at_ms): WalletRow) -> Wallet {
    Wallet {
        marks,
        updated_at_ms,
    }
}

/// The wallet of `account`; an empty one when it never had any.
///
/// # Errors
///
/// Returns [`DbError`] when the query fails.
pub async fn wallet(global: &PgPool, account: Uuid) -> Result<Wallet, DbError> {
    let row: Option<WalletRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {WALLET_COLUMNS} FROM account_wallet WHERE account_id = $1"
    )))
    .bind(account)
    .fetch_optional(global)
    .await?;
    Ok(row.map(wallet_of).unwrap_or_default())
}

/// Adds `amount` Marks of Fortune to the wallet of `account` (a negative amount takes them) and
/// returns the wallet.
///
/// # Errors
///
/// Returns [`DbError`] when the account does not exist, the balance would go below 0, or the
/// query fails.
pub async fn give_marks(global: &PgPool, account: Uuid, amount: i64) -> Result<Wallet, DbError> {
    open_wallet(global, account).await?;
    let row: WalletRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE account_wallet SET marks_of_fortune = marks_of_fortune + $2, updated_at = now() \
         WHERE account_id = $1 RETURNING {WALLET_COLUMNS}"
    )))
    .bind(account)
    .bind(amount)
    .fetch_one(global)
    .await?;
    Ok(wallet_of(row))
}

/// Gives `account` an empty wallet when it has none.
async fn open_wallet<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: Uuid,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO account_wallet (account_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(account)
        .execute(executor)
        .await?;
    Ok(())
}

/// What a store purchase did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purchase {
    /// The account paid the price and now holds the entitlement.
    Bought {
        /// The entitlement.
        held: AccountEntitlement,
        /// The wallet after the payment.
        wallet: Wallet,
    },
    /// The account already held the entitlement; nothing was paid.
    AlreadyHeld {
        /// The entitlement.
        held: AccountEntitlement,
        /// The wallet.
        wallet: Wallet,
    },
    /// The wallet holds less than the price; nothing changed.
    TooPoor {
        /// The wallet.
        wallet: Wallet,
    },
}

/// `account` buys `entitlement` for `price` Marks of Fortune (0 for a free store), in one
/// transaction: an entitlement it holds is not bought again, and a wallet below the price buys
/// nothing.
///
/// # Errors
///
/// Returns [`DbError`] when the account does not exist or a query fails.
pub async fn buy_entitlement(
    global: &PgPool,
    account: Uuid,
    entitlement: &str,
    price: i64,
) -> Result<Purchase, DbError> {
    let mut transaction = global.begin().await?;
    open_wallet(&mut *transaction, account).await?;
    let before: WalletRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {WALLET_COLUMNS} FROM account_wallet WHERE account_id = $1 FOR UPDATE"
    )))
    .bind(account)
    .fetch_one(&mut *transaction)
    .await?;
    let held: Option<EntitlementRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM account_entitlement WHERE account_id = $1 AND entitlement = $2"
    )))
    .bind(account)
    .bind(entitlement)
    .fetch_optional(&mut *transaction)
    .await?;
    let before = wallet_of(before);
    if let Some(held) = held {
        return Ok(Purchase::AlreadyHeld {
            held: of_row(held),
            wallet: before,
        });
    }
    if before.marks < price {
        return Ok(Purchase::TooPoor { wallet: before });
    }
    let after: WalletRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE account_wallet SET marks_of_fortune = marks_of_fortune - $2, updated_at = now() \
         WHERE account_id = $1 RETURNING {WALLET_COLUMNS}"
    )))
    .bind(account)
    .bind(price)
    .fetch_one(&mut *transaction)
    .await?;
    let held: EntitlementRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO account_entitlement (account_id, entitlement, instance_id) \
         VALUES ($1, $2, $3) RETURNING {COLUMNS}"
    )))
    .bind(account)
    .bind(entitlement)
    .bind(Uuid::new_v4())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Purchase::Bought {
        held: of_row(held),
        wallet: wallet_of(after),
    })
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use super::*;
    use crate::account::create_password_account;
    use crate::migrate::GLOBAL_MIGRATOR;
    use crate::test_db::with_test_schema;

    const BEAR: &str = "Ent_Mount_MTX_7_bear_Ronin";
    const WOLF: &str = "Ent_Mount_MTX_1_wolf";
    /// A retail mount price, and a wallet that buys one mount and not two.
    const PRICE: i64 = 20_000;
    const GIVEN: i64 = 30_000;

    #[tokio::test]
    async fn a_purchase_pays_once_and_a_short_wallet_buys_nothing() {
        let (empty, given, bought, again, poor, free, listed, taken, below) =
            with_test_schema(async |pool| {
                GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
                let account = create_password_account(pool, "rider", "$argon2id$x")
                    .await
                    .expect("account");
                let empty = wallet(pool, account.id).await.expect("empty");
                let given = give_marks(pool, account.id, GIVEN).await.expect("give");
                let bought = buy_entitlement(pool, account.id, BEAR, PRICE)
                    .await
                    .expect("buy");
                let again = buy_entitlement(pool, account.id, BEAR, PRICE)
                    .await
                    .expect("again");
                let poor = buy_entitlement(pool, account.id, WOLF, PRICE)
                    .await
                    .expect("poor");
                let free = buy_entitlement(pool, account.id, WOLF, 0)
                    .await
                    .expect("free");
                let listed = account_entitlements(pool, account.id).await.expect("list");
                let taken = give_marks(pool, account.id, -1).await.expect("take");
                let below = give_marks(pool, account.id, -GIVEN).await.is_err();
                (
                    empty, given, bought, again, poor, free, listed, taken, below,
                )
            })
            .await;
        assert_eq!(empty, Wallet::default());
        assert_eq!(given.marks, GIVEN);
        let Purchase::Bought { held, wallet } = bought else {
            panic!("bought: {bought:?}");
        };
        assert_eq!(
            (held.entitlement.as_str(), wallet.marks),
            (BEAR, GIVEN - PRICE)
        );
        assert!(matches!(
            again,
            Purchase::AlreadyHeld { held: ref again, wallet } if *again == held && wallet.marks == GIVEN - PRICE
        ));
        assert!(matches!(poor, Purchase::TooPoor { wallet } if wallet.marks == GIVEN - PRICE));
        assert!(matches!(free, Purchase::Bought { wallet, .. } if wallet.marks == GIVEN - PRICE));
        let names: Vec<&str> = listed.iter().map(|e| e.entitlement.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&BEAR) && names.contains(&WOLF));
        assert_eq!(taken.marks, GIVEN - PRICE - 1);
        assert!(below, "a wallet never goes below 0");
    }
}
