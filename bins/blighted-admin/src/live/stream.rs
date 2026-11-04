// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /api/live/stream` (server-sent events: one `state` event with the
//! [`LiveView`](crate::live::state::LiveView) as JSON every `state_interval_ms`, until the server
//! closes) and `GET /api/live/view` (one view). Without a `[live]` section both are 404.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use tokio::time::MissedTickBehavior;

use crate::live::state::RENDER_RADIUS_M;
use crate::live::{Live, lock};
use crate::server::AppState;

/// Query parameters of the stream and one-view routes.
#[derive(Debug, Default, Deserialize)]
pub struct StreamParams {
    /// When set, the player's render-radius square filters the view.
    pub focus: Option<u64>,
}

/// The server-sent events route.
pub const STREAM_ROUTE: &str = "/api/live/stream";
/// The one-view route.
pub const VIEW_ROUTE: &str = "/api/live/view";
/// The script run log lines.
pub const SCRIPT_LOG_ROUTE: &str = "/api/live/script-log";
/// The name of each stream event.
pub const STATE_EVENT: &str = "state";
/// The content type of the stream.
pub const EVENT_STREAM: &str = "text/event-stream";

/// The live routes.
pub fn live_routes() -> Router<AppState> {
    Router::new()
        .route(STREAM_ROUTE, get(stream_views))
        .route(VIEW_ROUTE, get(one_view))
        .route(SCRIPT_LOG_ROUTE, get(script_log))
}

async fn script_log(State(state): State<AppState>) -> Response {
    match &state.live {
        Some(live) => Json(lock(&live.state).script_lines()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn one_view(State(state): State<AppState>, Query(params): Query<StreamParams>) -> Response {
    match &state.live {
        Some(live) => {
            Json(lock(&live.state).view_focused(params.focus, RENDER_RADIUS_M)).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn stream_views(
    State(state): State<AppState>,
    Query(params): Query<StreamParams>,
) -> Response {
    let Some(Live {
        state,
        state_interval,
        mut closing,
        ..
    }) = state.live
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let focus = params.focus;
    let mut timer = tokio::time::interval(state_interval);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let events = stream::unfold(
        (timer, state, focus),
        |(mut timer, state, focus)| async move {
            timer.tick().await;
            let view = lock(&state).view_focused(focus, RENDER_RADIUS_M);
            let event = Event::default().event(STATE_EVENT).json_data(view);
            Some((event, (timer, state, focus)))
        },
    );
    let closed = async move {
        if closing.wait_for(|closing| *closing).await.is_err() {
            // Nobody can close the server any more: the stream runs until the page leaves.
            std::future::pending::<()>().await;
        }
    };
    Sse::new(events.take_until(closed))
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::guard::AdminOrigin;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::*;
    use crate::captures::Captures;
    use crate::live::bots::idle_bots;
    use crate::live::{lock, shared};
    use crate::server::{AppState, router};
    use crate::world::tests::test_layer;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const INTERVAL: Duration = Duration::from_millis(10);
    const EVENT_TEXT: &str = "sim sim-a joined";
    const WAIT: Duration = Duration::from_secs(5);

    fn app(live: Option<Live>) -> axum::Router {
        let dir = std::env::temp_dir();
        router(AppState::new(
            test_layer(CAP),
            dir,
            AdminOrigin::of(LISTEN.parse().expect("address")),
            Captures::default(),
            live,
        ))
    }

    fn live_closing(closing: watch::Receiver<bool>) -> Live {
        let state = shared();
        lock(&state).event(EVENT_TEXT.to_owned(), 1);
        Live::new(state.clone(), INTERVAL, idle_bots(state), closing)
    }

    /// A live part whose server never closes (the sender is gone).
    fn live() -> Live {
        live_closing(watch::channel(false).1)
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header::HOST, LISTEN)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn the_stream_sends_the_view_as_json() {
        let response = app(Some(live()))
            .oneshot(request(STREAM_ROUTE))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(EVENT_STREAM)
        );
        let mut body = response.into_body();
        let frame = body
            .frame()
            .await
            .expect("a frame")
            .expect("frame")
            .into_data()
            .expect("data");
        let text = String::from_utf8(frame.to_vec()).expect("text");
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("data line");
        assert!(text.contains(&format!("event: {STATE_EVENT}")));
        let view: serde_json::Value = serde_json::from_str(data).expect("json");
        assert_eq!(view["events"][0]["text"], EVENT_TEXT);
    }

    #[tokio::test]
    async fn the_stream_ends_when_the_server_closes() {
        let (close, closing) = watch::channel(false);
        let response = app(Some(live_closing(closing)))
            .oneshot(request(STREAM_ROUTE))
            .await
            .expect("response");
        let mut body = response.into_body();
        assert!(body.frame().await.is_some(), "a first state");
        close.send_replace(true);
        let ended =
            tokio::time::timeout(WAIT, async { while body.frame().await.is_some() {} }).await;
        assert!(ended.is_ok(), "the stream ended");
    }

    #[tokio::test]
    async fn the_view_route_gives_one_view() {
        let response = app(Some(live()))
            .oneshot(request(VIEW_ROUTE))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let view: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(view["tiles"]["sims"], 0);
    }

    #[tokio::test]
    async fn live_routes_without_a_live_section_are_404() {
        for uri in [STREAM_ROUTE, VIEW_ROUTE] {
            let response = app(None).oneshot(request(uri)).await.expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }
}
