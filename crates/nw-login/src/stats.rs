// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our stats service on `game.<domain>` (study note
//! `docs/re/2026-09-18-leaderboards-and-analytics.md`): the leaderboard and stat reads of the
//! client's `StatsserviceapiClient`, which the config key `omni.leaderboards.client-endpoint`
//! points here. Leaderboards are public, so these routes take no session. The sims write the
//! values ([`nw_db::submit_stats`]).

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use nw_common::time::iso8601_millis;
use nw_db::{BoardEntry, BoardOrder, GLOBAL_ENTITY};
use nw_protocol::stats_api::{
    EntitiesRequest, GAME_ALIAS, LEADERBOARDS_SEGMENT, LOWEST_FIRST_PREFIX, LeaderboardEntry,
    LeaderboardReply, PageQuery, STATS_SEGMENT, StatEntry, StatsReply, StatsRequest,
};
use serde::Deserialize;

use crate::error::{ApiError, parse_json};
use crate::store::StatsStore;

/// Largest request body: a list of ids.
pub const STATS_BODY_LIMIT_BYTES: usize = 64 * 1024;
/// Most rows one page holds: the retail client asks for 100 (our limit; the retail limit is
/// Unknown).
pub const MAX_PAGE_SIZE: u32 = 100;
/// Most entity or stat ids one request names (our limit; retail requests named at most 3).
pub const MAX_IDS_PER_REQUEST: usize = 100;
/// Longest board, stat or entity id (our limit; the longest retail id has 184 characters).
pub const MAX_ID_LEN: usize = 512;
/// The first page.
const FIRST_PAGE: u32 = 1;
/// Asks a page for the board size only.
const NO_ROWS: u32 = 0;

/// What the stats routes need.
#[derive(Clone)]
pub struct StatsState<S> {
    /// The stored stats.
    pub stats: S,
}

/// The path of a leaderboard request.
#[derive(Debug, Deserialize)]
struct BoardPath {
    game_alias: String,
    namespace: String,
    board_id: String,
}

/// The path of a stats request.
#[derive(Debug, Deserialize)]
struct StatsPath {
    game_alias: String,
    namespace: String,
}

/// The stats routes, merged into the routes of `game.<domain>`.
pub fn stats_router<S: StatsStore>(state: StatsState<S>) -> Router {
    Router::new()
        .route(
            &format!("/{{game_alias}}/{{namespace}}/{LEADERBOARDS_SEGMENT}/{{board_id}}"),
            get(leaderboard::<S>).post(leaderboard_for_entities::<S>),
        )
        .route(
            &format!("/{{game_alias}}/{{namespace}}/{STATS_SEGMENT}"),
            post(stats::<S>),
        )
        .layer(DefaultBodyLimit::max(STATS_BODY_LIMIT_BYTES))
        .with_state(state)
}

/// Whether `id` can be a board, stat, entity or namespace id: printable ASCII without spaces.
fn is_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Checks the alias and namespace of a path.
fn check_path(game_alias: &str, namespace: &str) -> Result<(), ApiError> {
    if game_alias != GAME_ALIAS {
        return Err(ApiError::UnknownRoute);
    }
    if !is_id(namespace) {
        return Err(ApiError::BadRequest);
    }
    Ok(())
}

/// Checks a list of ids from a request.
fn check_ids(ids: &[String]) -> Result<(), ApiError> {
    if ids.len() > MAX_IDS_PER_REQUEST || !ids.iter().all(|id| is_id(id)) {
        return Err(ApiError::BadRequest);
    }
    Ok(())
}

/// The order of a board: `min-` boards rank their lowest value first.
pub fn board_order(board_id: &str) -> BoardOrder {
    if board_id.starts_with(LOWEST_FIRST_PREFIX) {
        BoardOrder::LowestFirst
    } else {
        BoardOrder::HighestFirst
    }
}

fn found(entry: BoardEntry) -> LeaderboardEntry {
    LeaderboardEntry {
        value: Some(entry.value),
        entity_id: entry.entity_id,
        match_id: None,
        rank: Some(entry.rank),
    }
}

/// `GetLeaderboard`: one page of a board.
async fn leaderboard<S: StatsStore>(
    State(state): State<StatsState<S>>,
    Path(path): Path<BoardPath>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<LeaderboardReply>, ApiError> {
    check_path(&path.game_alias, &path.namespace)?;
    let Query(page) = query.map_err(|_| ApiError::BadRequest)?;
    if !is_id(&path.board_id)
        || page.page_number < FIRST_PAGE
        || !(FIRST_PAGE..=MAX_PAGE_SIZE).contains(&page.page_size)
    {
        return Err(ApiError::BadRequest);
    }
    let board = state
        .stats
        .leaderboard_page(
            &path.namespace,
            &path.board_id,
            board_order(&path.board_id),
            page.page_number,
            page.page_size,
        )
        .await?;
    Ok(Json(LeaderboardReply {
        leaderboard_entries: board.entries.into_iter().map(found).collect(),
        leaderboard_size: board.size,
        is_backed_up: None,
    }))
}

/// `GetLeaderboardForEntities`: the rows of some entities, in the order asked; an entity with no
/// value gets a row of `null`s, as in retail.
async fn leaderboard_for_entities<S: StatsStore>(
    State(state): State<StatsState<S>>,
    Path(path): Path<BoardPath>,
    body: Bytes,
) -> Result<Json<LeaderboardReply>, ApiError> {
    check_path(&path.game_alias, &path.namespace)?;
    let request: EntitiesRequest = parse_json(&body)?;
    if !is_id(&path.board_id) {
        return Err(ApiError::BadRequest);
    }
    check_ids(&request.entity_ids)?;
    let order = board_order(&path.board_id);
    let mut rows = state
        .stats
        .leaderboard_entries(&path.namespace, &path.board_id, order, &request.entity_ids)
        .await?;
    let size = state
        .stats
        .leaderboard_page(&path.namespace, &path.board_id, order, FIRST_PAGE, NO_ROWS)
        .await?
        .size;
    let entries = request
        .entity_ids
        .into_iter()
        .map(
            |entity_id| match rows.iter().position(|row| row.entity_id == entity_id) {
                Some(index) => found(rows.swap_remove(index)),
                None => LeaderboardEntry {
                    value: None,
                    entity_id,
                    match_id: None,
                    rank: None,
                },
            },
        )
        .collect();
    Ok(Json(LeaderboardReply {
        leaderboard_entries: entries,
        leaderboard_size: size,
        is_backed_up: None,
    }))
}

/// `GetStats`: stored values of some stats; no entity means the global values.
async fn stats<S: StatsStore>(
    State(state): State<StatsState<S>>,
    Path(path): Path<StatsPath>,
    body: Bytes,
) -> Result<Json<StatsReply>, ApiError> {
    check_path(&path.game_alias, &path.namespace)?;
    let request: StatsRequest = parse_json(&body)?;
    check_ids(&request.stat_ids)?;
    check_ids(&request.entity_ids)?;
    if !is_id(&request.stat_scope) {
        return Err(ApiError::BadRequest);
    }
    let entity_ids = if request.entity_ids.is_empty() {
        vec![GLOBAL_ENTITY.to_owned()]
    } else {
        request.entity_ids
    };
    let values = state
        .stats
        .stat_values(
            &path.namespace,
            &request.stat_scope,
            &request.stat_ids,
            &entity_ids,
        )
        .await?;
    Ok(Json(StatsReply {
        stats: values
            .values
            .into_iter()
            .map(|value| StatEntry {
                value: value.value,
                stat_id: value.stat_id,
                entity_id: (value.entity_id != GLOBAL_ENTITY).then_some(value.entity_id),
            })
            .collect(),
        last_updated_date: values.last_updated_ms.map(iso8601_millis),
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use nw_db::{BoardPage, StatValue, StatValues};

    use super::*;
    use crate::memory::MemoryStats;
    use crate::test_http::{self, error_code, request};

    const BOARD: &str = "top-boss-trial-completions.HercyneRaid_Reward.w1.m41";
    const CLEAR_TIME: &str = "min-dungeon-group-gold-medal-expedition-clear-time.Dungeon.3.w178";
    const FACTION_STAT: &str = "influence$open-world.Faction1.w1.s11";
    /// 2026-06-30 09:53:59.123 UTC.
    const UPDATED_MS: u64 = 1_782_813_239_123;

    fn entry(entity: &str, value: f64, rank: u64) -> BoardEntry {
        BoardEntry {
            entity_id: entity.to_owned(),
            value,
            rank,
        }
    }

    fn app() -> (Router, MemoryStats) {
        let stats = MemoryStats::default();
        stats.set_board(
            BOARD,
            BoardPage {
                entries: vec![entry("a", 5.0, 1), entry("b", 2.0, 2)],
                size: 2,
            },
        );
        stats.set_values(StatValues {
            values: vec![StatValue {
                stat_id: FACTION_STAT.to_owned(),
                entity_id: GLOBAL_ENTITY.to_owned(),
                value: 7.0,
            }],
            last_updated_ms: Some(UPDATED_MS),
        });
        (
            stats_router(StatsState {
                stats: stats.clone(),
            }),
            stats,
        )
    }

    async fn json(app: &Router, method: &str, uri: &str, body: &str) -> serde_json::Value {
        let (status, bytes) = test_http::send(app, request(method, uri, body)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn a_page_has_the_retail_shape() {
        let (app, stats) = app();
        let reply = json(
            &app,
            "GET",
            &format!("/new-world/pc/leaderboards/{BOARD}?pageNumber=1&pageSize=100"),
            "",
        )
        .await;
        assert_eq!(
            reply,
            serde_json::json!({
                "leaderboardEntries": [
                    {"value": 5.0, "entityId": "a", "matchId": null, "rank": 1},
                    {"value": 2.0, "entityId": "b", "matchId": null, "rank": 2}
                ],
                "leaderboardSize": 2,
                "isBackedUp": null
            })
        );
        assert_eq!(
            stats.last_page(),
            Some((
                "pc".to_owned(),
                BOARD.to_owned(),
                BoardOrder::HighestFirst,
                1,
                100
            ))
        );
        json(
            &app,
            "GET",
            &format!("/new-world/pc/leaderboards/{CLEAR_TIME}?pageNumber=2&pageSize=10"),
            "",
        )
        .await;
        assert_eq!(
            stats.last_page().map(|page| page.2),
            Some(BoardOrder::LowestFirst),
            "min- boards rank the lowest value first"
        );
    }

    #[tokio::test]
    async fn entities_without_a_value_get_null_rows_in_the_order_asked() {
        let (app, _) = app();
        let reply = json(
            &app,
            "POST",
            &format!("/new-world/pc/leaderboards/{BOARD}"),
            r#"{"entityIds":["nobody","b"]}"#,
        )
        .await;
        assert_eq!(
            reply,
            serde_json::json!({
                "leaderboardEntries": [
                    {"value": null, "entityId": "nobody", "matchId": null, "rank": null},
                    {"value": 2.0, "entityId": "b", "matchId": null, "rank": 2}
                ],
                "leaderboardSize": 2,
                "isBackedUp": null
            })
        );
    }

    #[tokio::test]
    async fn global_stats_have_no_entity() {
        let (app, stats) = app();
        let reply = json(
            &app,
            "POST",
            "/new-world/pc/stats",
            &format!(r#"{{"entityIds":[],"statIds":["{FACTION_STAT}"],"statScope":"Global"}}"#),
        )
        .await;
        assert_eq!(
            reply,
            serde_json::json!({
                "stats": [{"value": 7.0, "statId": FACTION_STAT, "entityId": null}],
                "lastUpdatedDate": "2026-06-30T09:53:59.123Z"
            })
        );
        assert_eq!(
            stats.last_values_query(),
            Some((
                "Global".to_owned(),
                vec![FACTION_STAT.to_owned()],
                vec![GLOBAL_ENTITY.to_owned()]
            ))
        );
    }

    #[tokio::test]
    async fn bad_requests_are_refused() {
        let (app, _) = app();
        let too_many: Vec<String> = (0..=MAX_IDS_PER_REQUEST).map(|n| n.to_string()).collect();
        let cases = [
            (
                "GET",
                format!("/other-game/pc/leaderboards/{BOARD}?pageNumber=1&pageSize=1"),
                String::new(),
                "unknown_route",
            ),
            (
                "GET",
                format!("/new-world/pc/leaderboards/{BOARD}?pageNumber=0&pageSize=1"),
                String::new(),
                "bad_request",
            ),
            (
                "GET",
                format!("/new-world/pc/leaderboards/{BOARD}?pageNumber=1&pageSize=101"),
                String::new(),
                "bad_request",
            ),
            (
                "GET",
                format!("/new-world/pc/leaderboards/{BOARD}"),
                String::new(),
                "bad_request",
            ),
            (
                "POST",
                format!("/new-world/pc/leaderboards/{BOARD}"),
                "{}".to_owned(),
                "bad_request",
            ),
            (
                "POST",
                format!("/new-world/pc/leaderboards/{BOARD}"),
                serde_json::json!({"entityIds": too_many}).to_string(),
                "bad_request",
            ),
            (
                "POST",
                "/new-world/pc/stats".to_owned(),
                r#"{"entityIds":["a b"],"statIds":[],"statScope":"Character"}"#.to_owned(),
                "bad_request",
            ),
        ];
        for (method, uri, body, code) in cases {
            let (status, bytes) = test_http::send(&app, request(method, &uri, &body)).await;
            assert!(status.is_client_error(), "{uri}: {status}");
            assert_eq!(error_code(&bytes), code, "{uri}");
        }
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use axum::http::StatusCode;
    use nw_db::test_db::with_test_schema;
    use nw_db::{GLOBAL_MIGRATOR, StatMerge, StatUpdate, submit_stats};

    use super::*;
    use crate::pg::PgStats;
    use crate::test_http::{self, request};

    const BOARD: &str = "min-expedition-clear-time.w1.w178";

    fn clear_time(entity: &str, seconds: f64) -> StatUpdate {
        StatUpdate {
            namespace: "pc".to_owned(),
            stat_id: BOARD.to_owned(),
            scope: "Group".to_owned(),
            entity_id: entity.to_owned(),
            value: seconds,
            merge: StatMerge::Least,
        }
    }

    #[tokio::test]
    async fn the_service_reads_what_a_sim_wrote() {
        let (page, mine) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            submit_stats(pool, &[clear_time("a_b", 600.0), clear_time("c_d", 500.0)])
                .await
                .expect("submit");
            let app = stats_router(StatsState {
                stats: PgStats::new(pool.clone()),
            });
            let uri = format!("/new-world/pc/leaderboards/{BOARD}");
            let page = test_http::send(
                &app,
                request("GET", &format!("{uri}?pageNumber=1&pageSize=100"), ""),
            )
            .await;
            let mine =
                test_http::send(&app, request("POST", &uri, r#"{"entityIds":["a_b"]}"#)).await;
            (page, mine)
        })
        .await;
        assert_eq!(page.0, StatusCode::OK);
        let page: serde_json::Value = serde_json::from_slice(&page.1).expect("page");
        assert_eq!(page["leaderboardSize"], 2);
        assert_eq!(page["leaderboardEntries"][0]["entityId"], "c_d");
        assert_eq!(page["leaderboardEntries"][0]["rank"], 1);
        let mine: serde_json::Value = serde_json::from_slice(&mine.1).expect("mine");
        assert_eq!(
            mine["leaderboardEntries"],
            serde_json::json!([{"value": 600.0, "entityId": "a_b", "matchId": null, "rank": 2}])
        );
    }
}
