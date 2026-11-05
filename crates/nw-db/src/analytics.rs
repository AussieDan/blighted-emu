// SPDX-License-Identifier: AGPL-3.0-or-later
//! The analytics stream (global database; study note
//! `docs/re/2026-09-18-leaderboards-and-analytics.md`): the records the client puts on its
//! Kinesis stream, kept as received.

use sqlx::PgPool;

use crate::error::DbError;

/// The record field that names the event.
const TYPE_FIELD: &str = "type";

/// One record the client put on its stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsRecord {
    /// The record's partition key.
    pub partition_key: String,
    /// The record's data, decoded from base64.
    pub data: Vec<u8>,
}

/// Stores `records` of stream `stream_name` in one transaction. A record whose data is JSON is
/// stored as JSON, with the `type` of an object as its event type; other data is stored as
/// bytes. Returns the ids of the records, in order.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails; then no record is stored.
pub async fn insert_analytics_events(
    pool: &PgPool,
    stream_name: &str,
    records: &[AnalyticsRecord],
) -> Result<Vec<i64>, DbError> {
    let mut transaction = pool.begin().await?;
    let mut ids = Vec::with_capacity(records.len());
    for record in records {
        let payload: Option<serde_json::Value> = serde_json::from_slice(&record.data).ok();
        let event_type = payload
            .as_ref()
            .and_then(|value| value.get(TYPE_FIELD))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let raw = payload.is_none().then_some(record.data.as_slice());
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO analytics_event (stream_name, partition_key, event_type, payload, raw) \
             VALUES ($1, $2, $3, $4, $5) RETURNING event_id",
        )
        .bind(stream_name)
        .bind(&record.partition_key)
        .bind(event_type)
        .bind(payload)
        .bind(raw)
        .fetch_one(&mut *transaction)
        .await?;
        ids.push(id);
    }
    transaction.commit().await?;
    Ok(ids)
}

/// Deletes the records received more than `retention_seconds` ago and returns how many went.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn delete_old_analytics_events(
    pool: &PgPool,
    retention_seconds: u64,
) -> Result<u64, DbError> {
    // A retention of days is well inside the `f64` range of `make_interval`.
    let result = sqlx::query(
        "DELETE FROM analytics_event WHERE received_at < now() - make_interval(secs => $1)",
    )
    .bind(retention_seconds as f64)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use crate::migrate::GLOBAL_MIGRATOR;
    use crate::test_db::with_test_schema;

    const STREAM: &str = "regional-client-javelin-stream";

    type Row = (
        String,
        String,
        Option<String>,
        Option<serde_json::Value>,
        Option<Vec<u8>>,
    );

    #[tokio::test]
    async fn the_clean_up_deletes_only_old_records() {
        const DAY_SECONDS: u64 = 86_400;
        let (deleted, left) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let record = AnalyticsRecord {
                partition_key: "one".to_owned(),
                data: vec![1],
            };
            insert_analytics_events(pool, STREAM, &[record.clone(), record])
                .await
                .expect("insert");
            sqlx::query(
                "UPDATE analytics_event SET received_at = now() - interval '2 days' \
                 WHERE event_id = (SELECT min(event_id) FROM analytics_event)",
            )
            .execute(pool)
            .await
            .expect("age one");
            let deleted = delete_old_analytics_events(pool, DAY_SECONDS)
                .await
                .expect("clean");
            let left: i64 = sqlx::query_scalar("SELECT count(*) FROM analytics_event")
                .fetch_one(pool)
                .await
                .expect("count");
            (deleted, left)
        })
        .await;
        assert_eq!((deleted, left), (1, 1));
    }

    #[tokio::test]
    async fn json_records_keep_their_type_and_others_their_bytes() {
        let (ids, stored) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            let ids = insert_analytics_events(
                pool,
                STREAM,
                &[
                    AnalyticsRecord {
                        partition_key: "one".to_owned(),
                        data: br#"{"type":"leaderboard_view","metrics":{}}"#.to_vec(),
                    },
                    AnalyticsRecord {
                        partition_key: "two".to_owned(),
                        data: vec![0xff, 0x00],
                    },
                ],
            )
            .await
            .expect("insert");
            let stored: Vec<Row> = sqlx::query_as(
                "SELECT stream_name, partition_key, event_type, payload, raw \
                 FROM analytics_event ORDER BY event_id",
            )
            .fetch_all(pool)
            .await
            .expect("read");
            (ids, stored)
        })
        .await;
        assert_eq!(ids.len(), 2);
        assert!(ids[0] < ids[1]);
        assert_eq!(
            stored,
            [
                (
                    STREAM.to_owned(),
                    "one".to_owned(),
                    Some("leaderboard_view".to_owned()),
                    Some(serde_json::json!({"type": "leaderboard_view", "metrics": {}})),
                    None
                ),
                (
                    STREAM.to_owned(),
                    "two".to_owned(),
                    None,
                    None,
                    Some(vec![0xff, 0x00])
                ),
            ]
        );
    }
}
