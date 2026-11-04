// SPDX-License-Identifier: AGPL-3.0-or-later
//! The capture viewer routes (admin tools spec sections 4.3 to 4.5) over the two slots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use nw_capture::{Decoded, Detail, SessionIndex, TrackPoint, message_bytes};
use serde::{Deserialize, Serialize};

use crate::captures::open::{Slot, SlotState};
use crate::server::AppState;

/// Most messages in one page.
pub const MAX_MESSAGES_PER_PAGE: usize = 2_000;
/// Most stream failures listed in a summary.
const SHOWN_FAILURES: usize = 50;
const MS_PER_SECOND: f64 = 1_000.0;

/// The capture viewer routes.
pub fn capture_routes() -> Router<AppState> {
    Router::new()
        .route("/api/captures/open", post(open_session))
        .route("/api/captures/compare", get(compare))
        .route("/api/captures/{slot}/state", get(slot_state))
        .route("/api/captures/{slot}/summary", get(summary))
        .route("/api/captures/{slot}/types", get(types))
        .route("/api/captures/{slot}/types/{idx}/rate", get(type_rate))
        .route("/api/captures/{slot}/tracks", get(tracks))
        .route(
            "/api/captures/{slot}/tracks/{key}/{appearance}",
            get(track_points),
        )
        .route("/api/captures/{slot}/player", get(player))
        .route("/api/captures/{slot}/messages", get(messages))
        .route("/api/captures/{slot}/messages/{n}/bytes", get(bytes))
        .route("/api/captures/{slot}/coverage", get(coverage))
}

#[derive(Debug, Serialize)]
struct StateView {
    state: &'static str,
    id: Option<String>,
    error: Option<String>,
}

impl StateView {
    fn of(state: &SlotState) -> Self {
        let (name, id, error) = match state {
            SlotState::Empty => ("empty", None, None),
            SlotState::Indexing { id } => ("indexing", Some(id.clone()), None),
            SlotState::Ready { id, .. } => ("ready", Some(id.clone()), None),
            SlotState::Failed { id, error } => ("failed", Some(id.clone()), Some(error.clone())),
        };
        Self {
            state: name,
            id,
            error,
        }
    }
}

struct Ready {
    index: Arc<SessionIndex>,
    ledger: Arc<[u8]>,
}

/// Why a slot has no ready session.
enum NotReady {
    /// No slot of that name: 404.
    NoSlot,
    /// The slot's state: 409.
    State(StateView),
}

impl IntoResponse for NotReady {
    fn into_response(self) -> Response {
        match self {
            Self::NoSlot => StatusCode::NOT_FOUND.into_response(),
            Self::State(view) => (StatusCode::CONFLICT, Json(view)).into_response(),
        }
    }
}

/// The ready session of the slot named `name`.
fn ready(state: &AppState, name: &str) -> Result<Ready, NotReady> {
    let slot = Slot::from_name(name).ok_or(NotReady::NoSlot)?;
    match state.captures.open.state(slot) {
        SlotState::Ready { index, ledger, .. } => Ok(Ready { index, ledger }),
        other => Err(NotReady::State(StateView::of(&other))),
    }
}

fn type_name(state: &AppState, index: u64) -> Option<String> {
    state.captures.labels.label(index).name
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    id: String,
    slot: String,
}

#[derive(Debug, Serialize)]
struct OpenAnswer {
    ticket: u64,
}

async fn open_session(State(state): State<AppState>, Json(request): Json<OpenRequest>) -> Response {
    let Some(slot) = Slot::from_name(&request.slot) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(entry) = state
        .captures
        .sessions
        .iter()
        .find(|entry| entry.id == request.id)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ticket = state.captures.open.open(slot, entry);
    (StatusCode::ACCEPTED, Json(OpenAnswer { ticket })).into_response()
}

async fn slot_state(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    match Slot::from_name(&name) {
        Some(slot) => Json(StateView::of(&state.captures.open.state(slot))).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, Serialize)]
struct FailureView {
    record: u32,
    reason: String,
}

#[derive(Debug, Serialize)]
struct SummaryView {
    first_ms: u64,
    last_ms: u64,
    records: u64,
    messages: u64,
    bytes: u64,
    tracks: usize,
    player_points: usize,
    error: Option<String>,
    failure_count: usize,
    failures: Vec<FailureView>,
}

async fn summary(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let index = &session.index;
    Json(SummaryView {
        first_ms: index.summary.first_ms,
        last_ms: index.summary.last_ms,
        records: index.summary.records,
        messages: index.summary.messages,
        bytes: index.summary.bytes,
        tracks: index.tracks.len(),
        player_points: index.player.len(),
        error: index.error.clone(),
        failure_count: index.failures.len(),
        failures: index
            .failures
            .iter()
            .take(SHOWN_FAILURES)
            .map(|failure| FailureView {
                record: failure.record,
                reason: failure.reason.clone(),
            })
            .collect(),
    })
    .into_response()
}

/// Seconds of a session, at least 1.
fn seconds(index: &SessionIndex) -> f64 {
    let span = index.summary.last_ms.saturating_sub(index.summary.first_ms) as f64;
    (span / MS_PER_SECOND).max(1.0)
}

#[derive(Debug, Serialize)]
struct TypeView {
    type_index: u64,
    direction: u32,
    name: Option<String>,
    azoth_status: Option<String>,
    count: u64,
    bytes: u64,
    first_ms: u64,
    last_ms: u64,
    per_second: f64,
}

async fn types(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let span = seconds(&session.index);
    let rows: Vec<TypeView> = session
        .index
        .types
        .iter()
        .map(|stats| {
            let label = state.captures.labels.label(stats.type_index);
            TypeView {
                type_index: stats.type_index,
                direction: stats.direction,
                name: label.name,
                azoth_status: label.azoth.map(|azoth| azoth.status),
                count: stats.count,
                bytes: stats.bytes,
                first_ms: stats.first_ms,
                last_ms: stats.last_ms,
                per_second: stats.count as f64 / span,
            }
        })
        .collect();
    Json(rows).into_response()
}

#[derive(Debug, Deserialize)]
struct DirectionParams {
    direction: u32,
}

async fn type_rate(
    State(state): State<AppState>,
    UrlPath((name, idx)): UrlPath<(String, u64)>,
    Query(params): Query<DirectionParams>,
) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    match session
        .index
        .types
        .iter()
        .find(|stats| stats.type_index == idx && stats.direction == params.direction)
    {
        Some(stats) => Json(&stats.per_second).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, Serialize)]
struct TrackView {
    key: u64,
    appearance: u32,
    classes: Vec<u64>,
    points: usize,
    first_ms: u64,
    last_ms: u64,
}

async fn tracks(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let rows: Vec<TrackView> = session
        .index
        .tracks
        .iter()
        .map(|track| TrackView {
            key: track.key,
            appearance: track.appearance,
            classes: track.classes.clone(),
            points: track.points.len(),
            first_ms: track.first_ms,
            last_ms: track.last_ms,
        })
        .collect();
    Json(rows).into_response()
}

#[derive(Debug, Serialize)]
struct PointView {
    time_ms: u64,
    x: f32,
    y: f32,
    z: f32,
}

fn point_views(points: &[TrackPoint]) -> Vec<PointView> {
    points
        .iter()
        .map(|point| PointView {
            time_ms: point.time_ms,
            x: point.x,
            y: point.y,
            z: point.z,
        })
        .collect()
}

async fn track_points(
    State(state): State<AppState>,
    UrlPath((name, key, appearance)): UrlPath<(String, u64, u32)>,
) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    match session
        .index
        .tracks
        .iter()
        .find(|track| track.key == key && track.appearance == appearance)
    {
        Some(track) => Json(point_views(&track.points)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn player(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    match ready(&state, &name) {
        Ok(session) => Json(point_views(&session.index.player)).into_response(),
        Err(not_ready) => not_ready.into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct MessageParams {
    from_ms: Option<u64>,
    to_ms: Option<u64>,
    #[serde(rename = "type")]
    type_index: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct MessageView {
    n: usize,
    record: u32,
    time_ms: u64,
    direction: u32,
    channel: u8,
    reliable: bool,
    type_index: Option<u64>,
    name: Option<String>,
    size: u32,
    members: usize,
    stopped_at: Option<u64>,
    position: Option<[f32; 3]>,
    failed: Option<String>,
}

fn message_view(state: &AppState, n: usize, message: &Decoded) -> MessageView {
    let (members, stopped_at, position, failed) = match &message.detail {
        Detail::Bundle {
            members,
            stopped_at,
            failed,
        } => (
            members.len(),
            *stopped_at,
            None,
            failed.as_ref().map(|failure| failure.reason.clone()),
        ),
        Detail::Input { position } => (0, None, *position, None),
        Detail::Failed(reason) => (0, None, None, Some(reason.clone())),
        Detail::None => (0, None, None, None),
    };
    MessageView {
        n,
        record: message.record,
        time_ms: message.time_ms,
        direction: message.direction,
        channel: message.channel,
        reliable: message.reliable,
        type_index: message.type_index,
        name: message.type_index.and_then(|index| type_name(state, index)),
        size: message.size,
        members,
        stopped_at,
        position,
        failed,
    }
}

async fn messages(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    Query(params): Query<MessageParams>,
) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let all = &session.index.messages;
    let start = params.from_ms.map_or(0, |from| {
        all.partition_point(|message| message.time_ms < from)
    });
    let limit = params
        .limit
        .unwrap_or(MAX_MESSAGES_PER_PAGE)
        .min(MAX_MESSAGES_PER_PAGE);
    let page: Vec<MessageView> = all
        .iter()
        .enumerate()
        .skip(start)
        .take_while(|(_, message)| params.to_ms.is_none_or(|to| message.time_ms <= to))
        .filter(|(_, message)| {
            params
                .type_index
                .is_none_or(|wanted| message.type_index == Some(wanted))
        })
        .take(limit)
        .map(|(n, message)| message_view(&state, n, message))
        .collect();
    Json(page).into_response()
}

#[derive(Debug, Serialize)]
struct MemberView {
    key: u64,
    class: u64,
    name: Option<String>,
    start: u32,
    end: u32,
    position: Option<[f32; 3]>,
}

#[derive(Debug, Serialize)]
struct BytesView {
    hex: String,
    members: Vec<MemberView>,
    stopped_at: Option<u64>,
    stopped_name: Option<String>,
    failed_class: Option<u64>,
    failed_name: Option<String>,
    failed_reason: Option<String>,
}

async fn bytes(
    State(state): State<AppState>,
    UrlPath((name, n)): UrlPath<(String, usize)>,
) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let Some(message) = session.index.messages.get(n).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ledger = Arc::clone(&session.ledger);
    let body =
        tokio::task::spawn_blocking(move || message_bytes(&ledger, message.record, message.slot))
            .await;
    let body = match body {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut hex = String::with_capacity(body.len() * 2);
    for byte in &body {
        // Writing to a String does not fail.
        let _ = write!(hex, "{byte:02x}");
    }
    let (members, stopped_at, failed) = match &session.index.messages[n].detail {
        Detail::Bundle {
            members,
            stopped_at,
            failed,
        } => (
            members
                .iter()
                .map(|member| MemberView {
                    key: member.key,
                    class: member.class,
                    name: type_name(&state, member.class),
                    start: member.range.start,
                    end: member.range.end,
                    position: member.position,
                })
                .collect(),
            *stopped_at,
            failed.clone(),
        ),
        Detail::Input { .. } | Detail::Failed(_) | Detail::None => (Vec::new(), None, None),
    };
    Json(BytesView {
        hex,
        members,
        stopped_name: stopped_at.and_then(|class| type_name(&state, class)),
        stopped_at,
        failed_name: failed
            .as_ref()
            .and_then(|failure| type_name(&state, failure.class)),
        failed_class: failed.as_ref().map(|failure| failure.class),
        failed_reason: failed.map(|failure| failure.reason),
    })
    .into_response()
}

#[derive(Debug, Serialize)]
struct CoverageView {
    class: u64,
    name: Option<String>,
    count: u64,
}

async fn coverage(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let session = match ready(&state, &name) {
        Ok(session) => session,
        Err(not_ready) => return not_ready.into_response(),
    };
    let rows: Vec<CoverageView> = session
        .index
        .coverage
        .iter()
        .map(|(class, count)| CoverageView {
            class: *class,
            name: type_name(&state, *class),
            count: *count,
        })
        .collect();
    Json(rows).into_response()
}

#[derive(Debug, Serialize)]
struct CompareRow {
    type_index: u64,
    direction: u32,
    name: Option<String>,
    main_per_second: Option<f64>,
    compare_per_second: Option<f64>,
    ratio: Option<f64>,
}

async fn compare(State(state): State<AppState>) -> Response {
    let (main, other) = match (ready(&state, "main"), ready(&state, "compare")) {
        (Ok(main), Ok(other)) => (main, other),
        (Err(not_ready), _) | (_, Err(not_ready)) => return not_ready.into_response(),
    };
    let mut rows: BTreeMap<(u64, u32), (Option<f64>, Option<f64>)> = BTreeMap::new();
    for (session, is_main) in [(&main, true), (&other, false)] {
        let span = seconds(&session.index);
        for stats in &session.index.types {
            let entry = rows.entry((stats.type_index, stats.direction)).or_default();
            let rate = Some(stats.count as f64 / span);
            if is_main {
                entry.0 = rate;
            } else {
                entry.1 = rate;
            }
        }
    }
    let rows: Vec<CompareRow> = rows
        .into_iter()
        .map(
            |((type_index, direction), (main_rate, other_rate))| CompareRow {
                type_index,
                direction,
                name: type_name(&state, type_index),
                main_per_second: main_rate,
                compare_per_second: other_rate,
                ratio: main_rate
                    .zip(other_rate)
                    .and_then(|(main, other)| (main > 0.0).then_some(other / main)),
            },
        )
        .collect();
    Json(rows).into_response()
}

#[cfg(test)]
mod tests {
    use crate::guard::AdminOrigin;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use nw_capture::test_support::{
        RELIABLE_START, TEST_TIME_MS, bundle_with_a_failing_member, envelope, position_bundle,
        record_at, server_datagram_at,
    };
    use nw_capture::{LEDGER_MAGIC, SERVER_TO_CLIENT};
    use nw_messages::REPLICATED_STATE_BUNDLE_TYPE_INDEX;
    use nw_reflect::PING_MSG_TYPE_INDEX;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::captures::Captures;
    use crate::captures::catalog::SessionEntry;
    use crate::captures::open::OpenSessions;
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE};
    use crate::server::{AppState, router};
    use crate::world::tests::test_layer;

    const LISTEN: &str = "127.0.0.1:9180";
    const CAP: usize = 10;
    const PING_BODY: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
    const WAIT_STEP: Duration = Duration::from_millis(5);

    /// A ledger: a ping at 0 ms and a position bundle at 500 ms; with `pings` more pings
    /// after them, one per 100 ms.
    fn ledger(pings: u16) -> Vec<u8> {
        let mut bytes = record_at(
            TEST_TIME_MS,
            SERVER_TO_CLIENT,
            &server_datagram_at(
                RELIABLE_START,
                &[envelope(PING_MSG_TYPE_INDEX, PING_BODY.to_vec())],
            ),
        );
        bytes.extend(record_at(
            TEST_TIME_MS + 500,
            SERVER_TO_CLIENT,
            &server_datagram_at(
                RELIABLE_START + 1,
                &[envelope(
                    REPLICATED_STATE_BUNDLE_TYPE_INDEX,
                    position_bundle(),
                )],
            ),
        ));
        for extra in 0..pings {
            bytes.extend(record_at(
                TEST_TIME_MS + 600 + u64::from(extra) * 100,
                SERVER_TO_CLIENT,
                &server_datagram_at(
                    RELIABLE_START + 2 + extra,
                    &[envelope(PING_MSG_TYPE_INDEX, PING_BODY.to_vec())],
                ),
            ));
        }
        bytes
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        app: axum::Router,
    }

    fn fixture(files: &[(&str, Vec<u8>)], open: OpenSessions) -> Fixture {
        let dir = tempfile::tempdir().expect("dir");
        let sessions = files
            .iter()
            .map(|(id, bytes)| {
                let path = dir.path().join(id);
                std::fs::write(&path, bytes).expect("ledger");
                SessionEntry {
                    id: (*id).to_owned(),
                    path,
                    bytes: bytes.len() as u64,
                    name: None,
                    started_at: None,
                    post_title: None,
                }
            })
            .collect();
        let captures = Captures {
            sessions,
            open,
            ..Captures::default()
        };
        let app = router(AppState::new(
            test_layer(CAP),
            dir.path().to_path_buf(),
            AdminOrigin::of(LISTEN.parse().expect("address")),
            captures,
            None,
        ));
        Fixture { _dir: dir, app }
    }

    async fn call(app: &axum::Router, mut request: Request<Body>) -> (StatusCode, Value) {
        request
            .headers_mut()
            .insert("host", LISTEN.parse().expect("host"));
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
        call(
            app,
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn open(app: &axum::Router, id: &str, slot: &str) -> StatusCode {
        let request = Request::builder()
            .method("POST")
            .uri("/api/captures/open")
            .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
            .header("content-type", "application/json")
            .body(Body::from(json!({ "id": id, "slot": slot }).to_string()))
            .expect("request");
        call(app, request).await.0
    }

    async fn ready(app: &axum::Router, slot: &str) -> Value {
        loop {
            let (_, state) = get(app, &format!("/api/captures/{slot}/state")).await;
            if state["state"] != "indexing" {
                return state;
            }
            tokio::time::sleep(WAIT_STEP).await;
        }
    }

    #[tokio::test]
    async fn opening_a_session_indexes_it() {
        let fixture = fixture(&[("a.bin", ledger(0))], OpenSessions::default());
        assert_eq!(
            open(&fixture.app, "a.bin", "main").await,
            StatusCode::ACCEPTED
        );
        assert_eq!(ready(&fixture.app, "main").await["state"], "ready");
        let (status, summary) = get(&fixture.app, "/api/captures/main/summary").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(summary["messages"], 2);
        let (_, types) = get(&fixture.app, "/api/captures/main/types").await;
        assert_eq!(types.as_array().map(Vec::len), Some(2));
        let (_, tracks) = get(&fixture.app, "/api/captures/main/tracks").await;
        assert_eq!(tracks[0]["points"], 1);
        let key = tracks[0]["key"].as_u64().expect("key");
        let (_, points) = get(&fixture.app, &format!("/api/captures/main/tracks/{key}/0")).await;
        assert_eq!(points[0]["x"], 1.0);
    }

    #[tokio::test]
    async fn a_broken_ledger_opens_with_its_error() {
        let mut broken = ledger(0);
        broken.extend_from_slice(&LEDGER_MAGIC);
        let fixture = fixture(&[("cut.bin", broken)], OpenSessions::default());
        open(&fixture.app, "cut.bin", "main").await;
        assert_eq!(ready(&fixture.app, "main").await["state"], "ready");
        let (_, summary) = get(&fixture.app, "/api/captures/main/summary").await;
        assert_eq!(summary["messages"], 2);
        assert!(summary["error"].is_string());
    }

    #[tokio::test]
    async fn messages_are_paged_and_capped() {
        let fixture = fixture(&[("a.bin", ledger(3))], OpenSessions::default());
        open(&fixture.app, "a.bin", "main").await;
        ready(&fixture.app, "main").await;
        let (_, page) = get(&fixture.app, "/api/captures/main/messages?limit=2").await;
        assert_eq!(page.as_array().map(Vec::len), Some(2));
        let from = TEST_TIME_MS + 550;
        let (_, page) = get(
            &fixture.app,
            &format!("/api/captures/main/messages?from_ms={from}&type={PING_MSG_TYPE_INDEX}"),
        )
        .await;
        assert_eq!(page.as_array().map(Vec::len), Some(3));
        let (_, page) = get(&fixture.app, "/api/captures/main/messages?limit=999999999").await;
        assert_eq!(page.as_array().map(Vec::len), Some(5));
    }

    #[tokio::test]
    async fn bytes_come_with_member_ranges() {
        let fixture = fixture(&[("a.bin", ledger(0))], OpenSessions::default());
        open(&fixture.app, "a.bin", "main").await;
        ready(&fixture.app, "main").await;
        let (status, bytes) = get(&fixture.app, "/api/captures/main/messages/1/bytes").await;
        assert_eq!(status, StatusCode::OK);
        let hex = bytes["hex"].as_str().expect("hex");
        let member = &bytes["members"][0];
        let (start, end) = (
            member["start"].as_u64().expect("start") as usize,
            member["end"].as_u64().expect("end") as usize,
        );
        assert!(start < end && end * 2 <= hex.len());
        let (status, _) = get(&fixture.app, "/api/captures/main/messages/99/bytes").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_failing_member_shows_with_the_members_before_it() {
        let ledger = record_at(
            TEST_TIME_MS,
            SERVER_TO_CLIENT,
            &server_datagram_at(
                RELIABLE_START,
                &[envelope(
                    REPLICATED_STATE_BUNDLE_TYPE_INDEX,
                    bundle_with_a_failing_member(),
                )],
            ),
        );
        let fixture = fixture(&[("a.bin", ledger)], OpenSessions::default());
        open(&fixture.app, "a.bin", "main").await;
        ready(&fixture.app, "main").await;
        let (_, messages) = get(&fixture.app, "/api/captures/main/messages").await;
        assert_eq!(messages[0]["members"], 1);
        assert!(messages[0]["failed"].is_string());
        let (_, bytes) = get(&fixture.app, "/api/captures/main/messages/0/bytes").await;
        assert_eq!(bytes["members"].as_array().map(Vec::len), Some(1));
        assert_eq!(bytes["failed_class"], nw_messages::ALC_TYPE_INDEX);
        assert!(bytes["failed_reason"].is_string());
    }

    #[tokio::test]
    async fn compare_joins_types_and_gives_ratios() {
        let fixture = fixture(
            &[("a.bin", ledger(0)), ("b.bin", ledger(4))],
            OpenSessions::default(),
        );
        open(&fixture.app, "a.bin", "main").await;
        open(&fixture.app, "b.bin", "compare").await;
        ready(&fixture.app, "main").await;
        ready(&fixture.app, "compare").await;
        let (status, rows) = get(&fixture.app, "/api/captures/compare").await;
        assert_eq!(status, StatusCode::OK);
        let ping = rows
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["type_index"] == PING_MSG_TYPE_INDEX)
            .expect("ping row");
        assert!(ping["main_per_second"].is_number() && ping["compare_per_second"].is_number());
        assert!(ping["ratio"].is_number());
    }

    #[tokio::test]
    async fn a_type_in_one_session_only_has_no_ratio() {
        let only_pings = record_at(
            TEST_TIME_MS,
            SERVER_TO_CLIENT,
            &server_datagram_at(
                RELIABLE_START,
                &[envelope(PING_MSG_TYPE_INDEX, PING_BODY.to_vec())],
            ),
        );
        let fixture = fixture(
            &[("a.bin", ledger(0)), ("b.bin", only_pings)],
            OpenSessions::default(),
        );
        open(&fixture.app, "a.bin", "main").await;
        open(&fixture.app, "b.bin", "compare").await;
        ready(&fixture.app, "main").await;
        ready(&fixture.app, "compare").await;
        let (_, rows) = get(&fixture.app, "/api/captures/compare").await;
        let bundle = rows
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["type_index"] == REPLICATED_STATE_BUNDLE_TYPE_INDEX)
            .expect("bundle row");
        assert!(bundle["compare_per_second"].is_null() && bundle["ratio"].is_null());
    }

    #[tokio::test]
    async fn an_unknown_session_or_slot_is_404() {
        let fixture = fixture(&[("a.bin", ledger(0))], OpenSessions::default());
        assert_eq!(
            open(&fixture.app, "missing.bin", "main").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            open(&fixture.app, "a.bin", "third").await,
            StatusCode::NOT_FOUND
        );
        let (status, _) = get(&fixture.app, "/api/captures/third/summary").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_slot_still_indexing_is_409() {
        let slow = OpenSessions::with_indexer(std::sync::Arc::new(|path: &std::path::Path| {
            std::thread::sleep(Duration::from_millis(300));
            crate::captures::open::read_and_index(path)
        }));
        let fixture = fixture(&[("a.bin", ledger(0))], slow);
        open(&fixture.app, "a.bin", "main").await;
        let (status, state) = get(&fixture.app, "/api/captures/main/summary").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(state["state"], "indexing");
        let (status, _) = get(&fixture.app, "/api/captures/compare").await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn opening_needs_the_admin_header() {
        let fixture = fixture(&[("a.bin", ledger(0))], OpenSessions::default());
        let request = Request::builder()
            .method("POST")
            .uri("/api/captures/open")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "id": "a.bin", "slot": "main" }).to_string(),
            ))
            .expect("request");
        assert_eq!(call(&fixture.app, request).await.0, StatusCode::FORBIDDEN);
    }
}
