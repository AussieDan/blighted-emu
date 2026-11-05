// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our analytics stream (study note `docs/re/2026-09-18-leaderboards-and-analytics.md`).
//!
//! The client has no endpoint key for its analytics: it puts its events on the Kinesis stream at
//! `kinesis.<analytics.region-name>.amazonaws.com` with credentials from an STS `AssumeRole` at
//! `sts.<region>.amazonaws.com`. When those names lead to `edge-http`, these routes answer both:
//! STS hands out credentials that only this server reads (it checks no signature), and Kinesis
//! `PutRecords` stores the events in the global database. Nothing in the server reads them: they
//! are research data, kept [`AnalyticsConfig::retention_days`] days. Each client address may
//! put [`AnalyticsConfig::requests_per_minute`] requests and
//! [`AnalyticsConfig::bytes_per_minute`] bytes a minute, in bodies of at most
//! [`AnalyticsConfig::max_body_bytes`]; more is refused (`429`, `413`).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Router};
use data_encoding::BASE64;
use nw_common::time::{iso8601_seconds, unix_seconds};
use nw_db::AnalyticsRecord;
use nw_protocol::analytics_api::{
    KINESIS_CONTENT_TYPE, MAX_RECORDS_PER_PUT, PUT_RECORDS_TARGET, PutRecordsReply,
    PutRecordsRequest, PutRecordsResult, TARGET_HEADER,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{ApiError, parse_json};
use crate::game::AUTH_STACK_CREDENTIAL;
use crate::store::AnalyticsStore;

/// The Kinesis and STS host names (shared with the launcher, which maps them to this server).
pub use nw_protocol::aws_mirror::{is_kinesis_host, is_sts_host};

/// The default largest `PutRecords` body. Inferred: four times the largest of 6,533 retail
/// bodies (61,922 bytes); the Kinesis limit is 5 MiB.
pub const DEFAULT_ANALYTICS_BODY_BYTES: usize = 256 * 1024;
/// The default requests of one client address a minute. Inferred: well above the most of one
/// retail client (44 in a minute over 255 sessions), for a few clients behind one address.
pub const DEFAULT_ANALYTICS_REQUESTS_PER_MINUTE: u32 = 120;
/// The default body bytes of one client address a minute. Inferred: about four times the most
/// of one retail client (1,096,336 bytes in a minute).
pub const DEFAULT_ANALYTICS_BYTES_PER_MINUTE: u64 = 4 * 1024 * 1024;
/// The default days a record is kept (Inferred: a month of research data).
pub const DEFAULT_ANALYTICS_RETENTION_DAYS: u64 = 30;
/// Time between two clean-ups of old records; one runs after a put once this passed.
const CLEAN_UP_INTERVAL: Duration = Duration::from_secs(3600);
/// The rate window of a client address.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// The most client addresses the rate windows keep; a new address beyond them is refused until
/// old windows end (Inferred: bounds the memory of the windows).
const MAX_TRACKED_ADDRESSES: usize = 10_000;
/// Seconds per day, for the retention.
const SECONDS_PER_DAY: u64 = 86_400;
/// Largest STS body: one form.
const STS_BODY_LIMIT_BYTES: usize = 16 * 1024;
/// Prometheus counter of stored analytics records.
pub const RECORDS_METRIC: &str = "edge_http_analytics_records_total";
/// Prometheus counter of analytics requests over the rate limits.
pub const REFUSED_METRIC: &str = "edge_http_analytics_refused_total";
/// The shard every record goes to: this stream has one.
const SHARD_ID: &str = "shardId-000000000000";
/// The one STS action the client sends.
const ASSUME_ROLE_ACTION: &str = "AssumeRole";
/// Credential lifetime bounds and default of `AssumeRole` (the STS limits).
const MIN_DURATION_SECONDS: u64 = 900;
const MAX_DURATION_SECONDS: u64 = 43_200;
const DEFAULT_DURATION_SECONDS: u64 = 3600;
const XML_CONTENT_TYPE: &str = "text/xml";

/// `[analytics]`: the limits of the analytics stream; every key has a default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnalyticsConfig {
    /// Largest `PutRecords` body.
    pub max_body_bytes: usize,
    /// Requests of one client address a minute.
    pub requests_per_minute: u32,
    /// Body bytes of one client address a minute.
    pub bytes_per_minute: u64,
    /// Days a record is kept.
    pub retention_days: u64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_ANALYTICS_BODY_BYTES,
            requests_per_minute: DEFAULT_ANALYTICS_REQUESTS_PER_MINUTE,
            bytes_per_minute: DEFAULT_ANALYTICS_BYTES_PER_MINUTE,
            retention_days: DEFAULT_ANALYTICS_RETENTION_DAYS,
        }
    }
}

/// The use of one client address in its rate window.
#[derive(Debug, Clone, Copy)]
struct Window {
    started: Instant,
    requests: u32,
    bytes: u64,
}

/// The rate windows of the client addresses and the time of the last clean-up.
#[derive(Debug, Default)]
struct Usage {
    windows: HashMap<IpAddr, Window>,
    cleaned: Option<Instant>,
}

/// What the Kinesis route needs.
#[derive(Clone)]
pub struct AnalyticsState<A> {
    /// The stored records.
    pub analytics: A,
    /// The limits.
    pub limits: AnalyticsConfig,
    usage: Arc<Mutex<Usage>>,
}

impl<A> AnalyticsState<A> {
    /// The state of `analytics` under `limits`.
    pub fn new(analytics: A, limits: AnalyticsConfig) -> Self {
        Self {
            analytics,
            limits,
            usage: Arc::default(),
        }
    }

    /// Counts a request of `bytes` from `address` at `now`; false when it is over the limits.
    fn allow(&self, address: IpAddr, bytes: usize, now: Instant) -> bool {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if usage.windows.len() >= MAX_TRACKED_ADDRESSES && !usage.windows.contains_key(&address) {
            usage
                .windows
                .retain(|_, window| now.duration_since(window.started) < RATE_WINDOW);
            if usage.windows.len() >= MAX_TRACKED_ADDRESSES {
                return false;
            }
        }
        let window = usage.windows.entry(address).or_insert(Window {
            started: now,
            requests: 0,
            bytes: 0,
        });
        if now.duration_since(window.started) >= RATE_WINDOW {
            *window = Window {
                started: now,
                requests: 0,
                bytes: 0,
            };
        }
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let requests = window.requests.saturating_add(1);
        let total = window.bytes.saturating_add(bytes);
        if requests > self.limits.requests_per_minute || total > self.limits.bytes_per_minute {
            return false;
        }
        window.requests = requests;
        window.bytes = total;
        true
    }

    /// Whether a clean-up is due at `now`; it then counts as done.
    fn clean_up_due(&self, now: Instant) -> bool {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = usage
            .cleaned
            .is_none_or(|cleaned| now.duration_since(cleaned) >= CLEAN_UP_INTERVAL);
        if due {
            usage.cleaned = Some(now);
        }
        due
    }
}

/// The Kinesis route: `POST /` with `X-Amz-Target: Kinesis_20131202.PutRecords`.
pub fn kinesis_router<A: AnalyticsStore>(state: AnalyticsState<A>) -> Router {
    let body_limit = state.limits.max_body_bytes;
    Router::new()
        .route("/", post(put_records::<A>))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

/// Deletes the records older than the retention, in its own task so the put does not wait.
fn clean_up<A: AnalyticsStore>(state: &AnalyticsState<A>) {
    let analytics = state.analytics.clone();
    let retention_seconds = state.limits.retention_days.saturating_mul(SECONDS_PER_DAY);
    tokio::spawn(async move {
        match analytics.delete_older_than(retention_seconds).await {
            Ok(deleted) => tracing::info!(deleted, "old analytics records deleted"),
            Err(error) => tracing::warn!(%error, "analytics clean-up failed; next put tries again"),
        }
    });
}

/// The STS route: `POST /` with an `AssumeRole` form.
pub fn sts_router() -> Router {
    Router::new()
        .route("/", post(assume_role))
        .layer(DefaultBodyLimit::max(STS_BODY_LIMIT_BYTES))
}

async fn put_records<A: AnalyticsStore>(
    State(state): State<AnalyticsState<A>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let now = Instant::now();
    if !state.allow(peer.ip(), body.len(), now) {
        metrics::counter!(REFUSED_METRIC).increment(1);
        return Err(ApiError::RateLimited);
    }
    let target = headers
        .get(TARGET_HEADER)
        .and_then(|value| value.to_str().ok());
    if target != Some(PUT_RECORDS_TARGET) {
        return Err(ApiError::UnknownRoute);
    }
    let request: PutRecordsRequest = parse_json(&body)?;
    if request.records.is_empty() || request.records.len() > MAX_RECORDS_PER_PUT {
        return Err(ApiError::BadRequest);
    }
    let records = request
        .records
        .into_iter()
        .map(|entry| {
            BASE64
                .decode(entry.data.as_bytes())
                .map(|data| AnalyticsRecord {
                    partition_key: entry.partition_key,
                    data,
                })
                .map_err(|_| ApiError::BadRequest)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ids = state
        .analytics
        .put_records(&request.stream_name, &records)
        .await?;
    metrics::counter!(RECORDS_METRIC).increment(u64::try_from(ids.len()).unwrap_or(u64::MAX));
    if state.clean_up_due(now) {
        clean_up(&state);
    }
    let reply = PutRecordsReply {
        failed_record_count: 0,
        records: ids
            .into_iter()
            .map(|id| PutRecordsResult {
                sequence_number: id.to_string(),
                shard_id: SHARD_ID.to_owned(),
            })
            .collect(),
    };
    let json = serde_json::to_vec(&reply).map_err(|_| ApiError::Unavailable)?;
    Ok(([(CONTENT_TYPE, KINESIS_CONTENT_TYPE)], json).into_response())
}

/// The form of an STS request; the other fields are ignored.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StsForm {
    action: String,
    role_arn: Option<String>,
    role_session_name: Option<String>,
    duration_seconds: Option<u64>,
}

/// `text` with the five XML special characters escaped.
pub(crate) fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// `AssumeRole`: credentials for the role the client names, valid for the time it asks (within
/// the STS bounds). The keys are the same as those of the `/steam` reply; this server checks no
/// signature.
async fn assume_role(Form(form): Form<StsForm>) -> Result<Response, ApiError> {
    if form.action != ASSUME_ROLE_ACTION {
        return Err(ApiError::BadRequest);
    }
    let duration = form
        .duration_seconds
        .unwrap_or(DEFAULT_DURATION_SECONDS)
        .clamp(MIN_DURATION_SECONDS, MAX_DURATION_SECONDS);
    let now = u64::try_from(unix_seconds()).unwrap_or(0);
    let expiration = iso8601_seconds(now.saturating_add(duration));
    let role = xml_text(form.role_arn.as_deref().unwrap_or_default());
    let session = xml_text(form.role_session_name.as_deref().unwrap_or_default());
    let body = format!(
        "<AssumeRoleResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">\
         <AssumeRoleResult>\
         <AssumedRoleUser><AssumedRoleId>{AUTH_STACK_CREDENTIAL}:{session}</AssumedRoleId>\
         <Arn>{role}</Arn></AssumedRoleUser>\
         <Credentials><AccessKeyId>{AUTH_STACK_CREDENTIAL}</AccessKeyId>\
         <SecretAccessKey>{AUTH_STACK_CREDENTIAL}</SecretAccessKey>\
         <SessionToken>{AUTH_STACK_CREDENTIAL}</SessionToken>\
         <Expiration>{expiration}</Expiration></Credentials>\
         </AssumeRoleResult>\
         <ResponseMetadata><RequestId>{}</RequestId></ResponseMetadata>\
         </AssumeRoleResponse>",
        Uuid::new_v4()
    );
    Ok((StatusCode::OK, [(CONTENT_TYPE, XML_CONTENT_TYPE)], body).into_response())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;

    use super::*;
    use crate::memory::MemoryAnalytics;
    use crate::test_http::{self, error_code};

    const STREAM: &str = "regional-client-javelin-stream";
    const PEER: ([u8; 4], u16) = ([10, 0, 0, 1], 50_000);
    const VALID: &str = r#"{"Records":[{"Data":"e30=","PartitionKey":"k"}],"StreamName":"s"}"#;

    fn app(analytics: &MemoryAnalytics, limits: AnalyticsConfig) -> Router {
        kinesis_router(AnalyticsState::new(analytics.clone(), limits))
            .layer(MockConnectInfo(SocketAddr::from(PEER)))
    }

    #[tokio::test]
    async fn an_address_over_its_limits_is_refused() {
        let analytics = MemoryAnalytics::default();
        let limits = AnalyticsConfig {
            requests_per_minute: 2,
            ..AnalyticsConfig::default()
        };
        let limited = app(&analytics, limits);
        for _ in 0..2 {
            let (status, _) = test_http::send(&limited, kinesis(PUT_RECORDS_TARGET, VALID)).await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, bytes) = test_http::send(&limited, kinesis(PUT_RECORDS_TARGET, VALID)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_code(&bytes), "rate_limited");
        assert_eq!(analytics.records().len(), 2);
        let limits = AnalyticsConfig {
            bytes_per_minute: u64::try_from(VALID.len()).expect("short"),
            ..AnalyticsConfig::default()
        };
        let state = AnalyticsState::new(MemoryAnalytics::default(), limits);
        let now = Instant::now();
        let address = SocketAddr::from(PEER).ip();
        assert!(state.allow(address, VALID.len(), now));
        assert!(!state.allow(address, 1, now), "over the bytes of a minute");
        assert!(
            state.allow(address, VALID.len(), now + RATE_WINDOW),
            "a new minute"
        );
        let small = AnalyticsConfig {
            max_body_bytes: VALID.len() - 1,
            ..AnalyticsConfig::default()
        };
        let (status, _) =
            test_http::send(&app(&analytics, small), kinesis(PUT_RECORDS_TARGET, VALID)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn a_clean_up_runs_once_an_hour() {
        let state = AnalyticsState::new(MemoryAnalytics::default(), AnalyticsConfig::default());
        let now = Instant::now();
        assert!(state.clean_up_due(now), "the first put cleans");
        assert!(!state.clean_up_due(now + Duration::from_secs(1)));
        assert!(state.clean_up_due(now + CLEAN_UP_INTERVAL));
    }

    fn kinesis(target: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/")
            .header(TARGET_HEADER, target)
            .header(CONTENT_TYPE, KINESIS_CONTENT_TYPE)
            .body(Body::from(body.to_owned()))
            .expect("request")
    }

    fn sts(body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .expect("request")
    }

    #[tokio::test]
    async fn put_records_stores_each_decoded_record() {
        let analytics = MemoryAnalytics::default();
        let app = app(&analytics, AnalyticsConfig::default());
        let event = br#"{"type":"leaderboard_view"}"#;
        let body = serde_json::json!({
            "Records": [
                {"Data": BASE64.encode(event), "PartitionKey": "k1"},
                {"Data": BASE64.encode(b"x"), "PartitionKey": "k2"}
            ],
            "StreamName": STREAM
        })
        .to_string();
        let (status, bytes) = test_http::send(&app, kinesis(PUT_RECORDS_TARGET, &body)).await;
        assert_eq!(status, StatusCode::OK);
        let reply: PutRecordsReply = serde_json::from_slice(&bytes).expect("reply");
        assert_eq!(reply.failed_record_count, 0);
        assert_eq!(
            reply
                .records
                .iter()
                .map(|record| record.sequence_number.as_str())
                .collect::<Vec<_>>(),
            ["0", "1"]
        );
        assert_eq!(
            analytics.records(),
            [
                (
                    STREAM.to_owned(),
                    AnalyticsRecord {
                        partition_key: "k1".to_owned(),
                        data: event.to_vec()
                    }
                ),
                (
                    STREAM.to_owned(),
                    AnalyticsRecord {
                        partition_key: "k2".to_owned(),
                        data: b"x".to_vec()
                    }
                ),
            ]
        );
    }

    #[tokio::test]
    async fn other_kinesis_requests_are_refused() {
        let analytics = MemoryAnalytics::default();
        let app = app(&analytics, AnalyticsConfig::default());
        let cases = [
            ("Kinesis_20131202.DeleteStream", VALID, "unknown_route"),
            (
                PUT_RECORDS_TARGET,
                r#"{"Records":[],"StreamName":"s"}"#,
                "bad_request",
            ),
            (
                PUT_RECORDS_TARGET,
                r#"{"Records":[{"Data":"not base64!","PartitionKey":"k"}],"StreamName":"s"}"#,
                "bad_request",
            ),
        ];
        for (target, body, code) in cases {
            let (status, bytes) = test_http::send(&app, kinesis(target, body)).await;
            assert!(status.is_client_error(), "{target}");
            assert_eq!(error_code(&bytes), code, "{body}");
        }
        assert!(analytics.records().is_empty());
    }

    #[tokio::test]
    async fn assume_role_hands_out_credentials() {
        let app = sts_router();
        let (status, bytes) = test_http::send(
            &app,
            sts("Action=AssumeRole&RoleArn=arn%3Aaws%3Aiam%3A%3A1%3Arole%2Fr%3Cx&RoleSessionName=s&DurationSeconds=900&Version=2011-06-15"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = String::from_utf8(bytes.to_vec()).expect("text");
        assert!(
            text.contains("<AccessKeyId>blighted</AccessKeyId>"),
            "{text}"
        );
        assert!(
            text.contains("<Arn>arn:aws:iam::1:role/r&lt;x</Arn>"),
            "{text}"
        );
        assert!(text.contains("<Expiration>"), "{text}");
        let (status, _) = test_http::send(&app, sts("Action=GetCallerIdentity")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
