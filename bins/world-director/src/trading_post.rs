// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trading post's expiries: the director, one per world, resolves the orders whose posting
//! time ran out (world migration 0016). Each expired order owes its creator the units left and
//! an expiry notice; the creator's sim takes them when the player is online, or at its next join.

use std::time::Duration;

use nw_common::time::unix_ms;
use nw_db::{PgPool, expire_orders};

/// Time between two expiry sweeps.
pub const EXPIRY_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Most orders one transaction expires; a sweep repeats until fewer expire.
pub const EXPIRY_BATCH: u32 = 500;
/// Metric: trading post orders expired.
pub const METRIC_EXPIRED_ORDERS: &str = "director_trading_post_expired_total";
/// Metric: expiry sweeps that failed.
pub const METRIC_FAILED_SWEEPS: &str = "director_trading_post_failed_sweeps_total";

/// Sweeps the expired orders of `world` every `interval`, until the runtime stops.
pub async fn sweep_expired_orders(world: PgPool, interval: Duration) {
    let mut timer = tokio::time::interval(interval);
    loop {
        timer.tick().await;
        sweep_once(&world, unix_ms()).await;
    }
}

/// Expires every order whose time ran out by `now_ms`, [`EXPIRY_BATCH`] at a time. A failed
/// batch ends the sweep; the next sweep tries again.
pub async fn sweep_once(world: &PgPool, now_ms: u64) -> u64 {
    let mut total = 0_u64;
    loop {
        match expire_orders(world, now_ms, EXPIRY_BATCH).await {
            Ok(expired) => {
                total += u64::from(expired);
                metrics::counter!(METRIC_EXPIRED_ORDERS).increment(u64::from(expired));
                if expired < EXPIRY_BATCH {
                    if total > 0 {
                        tracing::info!(expired = total, "trading post orders expired");
                    }
                    return total;
                }
            }
            Err(error) => {
                metrics::counter!(METRIC_FAILED_SWEEPS).increment(1);
                tracing::warn!(%error, "trading post expiry sweep failed");
                return total;
            }
        }
    }
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use nw_db::test_db::with_test_schema;
    use nw_db::{
        NewCharacter, NewOrder, OrderItem, OrderKind, WORLD_MIGRATOR, insert_character, post_order,
        take_deliveries,
    };
    use uuid::Uuid;

    const NOW_MS: u64 = 1_798_761_600_000;
    const DAY_MS: u64 = 86_400_000;

    #[tokio::test]
    async fn a_sweep_expires_what_ran_out_once() {
        let (first, second, owed) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let creator = insert_character(
                pool,
                &NewCharacter {
                    id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    name: "Trader".to_owned(),
                    appearance: serde_json::json!({}),
                },
            )
            .await
            .expect("insert")
            .id;
            post_order(
                pool,
                &NewOrder {
                    id: Uuid::new_v4(),
                    kind: OrderKind::Buy,
                    creator,
                    outpost: 1,
                    item: OrderItem {
                        item_id: "WoodT1".to_owned(),
                        item_crc: 0x8a91_4427,
                        category: "Resources".to_owned(),
                        family: "RawResources".to_owned(),
                        group: "Wood".to_owned(),
                        tier: 1,
                        gear_score: 0,
                        required_level: None,
                        perks: Vec::new(),
                        gem_perk_count: 0,
                        rolls: nw_db::SavedItemRolls::default(),
                    },
                    price: 100,
                    quantity: 5,
                    tax_rate: 0.05,
                    created_ms: NOW_MS,
                    expires_ms: NOW_MS + DAY_MS,
                    posting_fee: 0,
                    max_open: 40,
                },
            )
            .await
            .expect("post")
            .expect("posted");
            let first = sweep_once(pool, NOW_MS + 2 * DAY_MS).await;
            let second = sweep_once(pool, NOW_MS + 2 * DAY_MS).await;
            let owed = take_deliveries(pool, &[creator]).await.expect("take");
            (first, second, owed)
        })
        .await;
        assert_eq!((first, second), (1, 0));
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].delivery.coins, 500);
    }
}
