// SPDX-License-Identifier: AGPL-3.0-or-later
//! The HTTP routes of `blighted-admin` (admin tools spec sections 3 and 7).

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};

use crate::access::Access;
use crate::access::api::{LOGIN_PAGE_ROUTE, access_guard, access_routes, local_access};
use crate::assets::asset;
use crate::captures::Captures;
use crate::captures::api::capture_routes;
use crate::captures::catalog::search;
use crate::gm::Commands;
use crate::gm::api::gm_routes;
use crate::guard::{AdminOrigin, admin_guard};
use crate::host::{HOST_ROUTE, host};
use crate::live::Live;
use crate::live::bots::bot_routes;
use crate::live::stream::live_routes;
use crate::relief::relief_tile;
use crate::shapes::{LevelShapes, SHAPES_ROUTE, shapes};
use crate::studio::Studio;
use crate::studio::api::studio_routes;
use crate::studio::loot::loot_routes;
use crate::studio::quests::quest_routes;
use crate::todo::api::todo_routes;
use crate::world::{SPAWN_CELL_M, SpawnCell, SpawnKind, SpawnQuery, WorldLayer, WorldSlot};
use nw_world::map_tiles::tile_file;

/// Content type of map tiles.
pub const PNG: &str = "image/png";
const INDEX: &str = "index.html";
const LIVE_PAGE: &str = "live.html";
/// The live page.
pub const LIVE_PAGE_ROUTE: &str = "/live";
const LIVE3D_PAGE: &str = "live3d.html";
/// The live 3D map page.
pub const LIVE3D_PAGE_ROUTE: &str = "/live3d";
const STUDIO_PAGE: &str = "studio.html";
const LOGIN_PAGE: &str = "login.html";
const GM_PAGE: &str = "gm.html";
const RECORDS_PAGE: &str = "records.html";
const TODO_PAGE: &str = "todo.html";
const SCRIPTS_PAGE: &str = "scripts.html";
/// The GM page.
pub const GM_PAGE_ROUTE: &str = "/gm";
/// The studio page.
pub const STUDIO_PAGE_ROUTE: &str = "/studio";
/// The record editor page.
pub const RECORDS_PAGE_ROUTE: &str = "/records";
/// The to-do page.
pub const TODO_PAGE_ROUTE: &str = "/todo";
/// The script editor page.
pub const SCRIPTS_PAGE_ROUTE: &str = "/scripts";

/// What every route shares.
#[derive(Debug, Clone)]
pub struct AppState {
    pub(crate) world: WorldSlot,
    map_dir: Arc<PathBuf>,
    origin: AdminOrigin,
    pub(crate) captures: Arc<Captures>,
    pub(crate) live: Option<Live>,
    pub(crate) studio: Option<Studio>,
    pub(crate) access: Access,
    pub(crate) commands: Option<Commands>,
    pub(crate) shapes: Option<LevelShapes>,
}

impl AppState {
    /// The state of a server with the request checks of `origin`, `world`, the map tiles in
    /// `map_dir`, the capture sessions, and the live panel when configured.
    pub fn new(
        world: WorldLayer,
        map_dir: PathBuf,
        origin: AdminOrigin,
        captures: Captures,
        live: Option<Live>,
    ) -> Self {
        Self {
            world: WorldSlot::new(world),
            map_dir: Arc::new(map_dir),
            origin,
            captures: Arc::new(captures),
            live,
            studio: None,
            access: local_access(),
            commands: None,
            shapes: None,
        }
    }

    /// The same state with the level's solids for the 3D page.
    pub fn with_shapes(self, shapes: LevelShapes) -> Self {
        Self {
            shapes: Some(shapes),
            ..self
        }
    }

    /// The same state with GM commands sent by `commands`.
    pub fn with_commands(self, commands: Commands) -> Self {
        Self {
            commands: Some(commands),
            ..self
        }
    }

    /// The same state with `access` (server mode: logins and ranks).
    pub fn with_access(self, access: Access) -> Self {
        Self { access, ..self }
    }

    /// The same state with the studio routes served by `studio`.
    pub fn with_studio(self, studio: Studio) -> Self {
        Self {
            studio: Some(studio),
            ..self
        }
    }
}

/// Every route, behind [`admin_guard`].
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| web_file(UrlPath(INDEX.to_owned()))))
        .route(
            LIVE_PAGE_ROUTE,
            get(|| web_file(UrlPath(LIVE_PAGE.to_owned()))),
        )
        .route(
            LIVE3D_PAGE_ROUTE,
            get(|| web_file(UrlPath(LIVE3D_PAGE.to_owned()))),
        )
        .route(
            STUDIO_PAGE_ROUTE,
            get(|| web_file(UrlPath(STUDIO_PAGE.to_owned()))),
        )
        .route(GM_PAGE_ROUTE, get(|| web_file(UrlPath(GM_PAGE.to_owned()))))
        .route(
            RECORDS_PAGE_ROUTE,
            get(|| web_file(UrlPath(RECORDS_PAGE.to_owned()))),
        )
        .route(
            TODO_PAGE_ROUTE,
            get(|| web_file(UrlPath(TODO_PAGE.to_owned()))),
        )
        .route(
            SCRIPTS_PAGE_ROUTE,
            get(|| web_file(UrlPath(SCRIPTS_PAGE.to_owned()))),
        )
        .route(
            LOGIN_PAGE_ROUTE,
            get(|| web_file(UrlPath(LOGIN_PAGE.to_owned()))),
        )
        .route("/web/{file}", get(web_file))
        .route("/api/world/info", get(world_info))
        .route("/api/world/tiles/{level}/{row}/{column}", get(map_tile))
        .route("/api/world/relief/{level}/{row}/{column}", get(relief))
        .route("/api/world/spawns", get(spawns))
        .route("/api/world/spawn-cells", get(spawn_cells))
        .route(SHAPES_ROUTE, get(shapes))
        .route(HOST_ROUTE, get(host))
        .route(HEIGHT_ROUTE, get(ground_height))
        .route(HEIGHT_GRID_ROUTE, get(height_grid))
        .route("/api/captures/sessions", get(sessions))
        .route("/api/captures/types/{idx}", get(type_label))
        .merge(capture_routes())
        .merge(live_routes())
        .merge(bot_routes())
        .merge(studio_routes())
        .merge(loot_routes())
        .merge(quest_routes())
        .merge(todo_routes())
        .merge(access_routes())
        .merge(gm_routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            access_guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.origin.clone(),
            admin_guard,
        ))
        .with_state(state)
}

async fn web_file(UrlPath(file): UrlPath<String>) -> Response {
    match asset(&file) {
        Some(found) => ([(header::CONTENT_TYPE, found.content_type)], found.body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn world_info(State(state): State<AppState>) -> Response {
    Json(state.world.get().info()).into_response()
}

async fn map_tile(
    State(state): State<AppState>,
    UrlPath((level, row, column)): UrlPath<(u8, u32, u32)>,
) -> Response {
    let path = tile_file(&state.map_dir, level, row, column);
    match tokio::fs::read(&path).await {
        Ok(bytes) => png_response(bytes),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn relief(
    State(state): State<AppState>,
    UrlPath((level, row, column)): UrlPath<(u8, u32, u32)>,
) -> Response {
    let world = state.world.get();
    let drawn = tokio::task::spawn_blocking(move || {
        relief_tile(world.world(), world.tiles(), level, row, column)
    })
    .await;
    match drawn {
        Ok(Some(bytes)) => png_response(bytes),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Ground height at `?x=&y=` (metres): `{ "z": metres }`, `null` off the terrain.
pub const HEIGHT_ROUTE: &str = "/api/world/height";

/// Ground heights on a square grid at `?x=&y=&half=&step=` (metres): `{ x0, y0, step, size,
/// heights: [[z|null; size]; size] }`. A size over [`HEIGHT_GRID_MAX_SIZE`], a step under
/// [`HEIGHT_GRID_MIN_STEP_M`], a non-positive `half` or `step`, or an edge that is not finite
/// gets a 400.
pub const HEIGHT_GRID_ROUTE: &str = "/api/world/height-grid";
/// Most grid cells per side: a 512 x 512 grid at 4 bytes a cell is a 1 MiB payload at most.
pub const HEIGHT_GRID_MAX_SIZE: u32 = 512;
/// Smallest step between two height samples, in metres.
pub const HEIGHT_GRID_MIN_STEP_M: f32 = 0.5;

#[derive(Debug, Deserialize)]
struct HeightParams {
    x: f32,
    y: f32,
}

#[derive(Debug, Serialize)]
struct HeightView {
    z: Option<f32>,
}

async fn ground_height(State(state): State<AppState>, Query(at): Query<HeightParams>) -> Response {
    Json(HeightView {
        z: state.world.get().ground_height(at.x, at.y),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
struct HeightGridParams {
    x: f32,
    y: f32,
    half: f32,
    step: f32,
}

#[derive(Debug, Serialize)]
struct HeightGridView {
    x0: f32,
    y0: f32,
    step: f32,
    size: u32,
    heights: Vec<Vec<Option<f32>>>,
}

async fn height_grid(
    State(state): State<AppState>,
    Query(params): Query<HeightGridParams>,
) -> Response {
    let edges = [params.x, params.y, params.half, params.step];
    if !edges.iter().all(|edge| edge.is_finite()) {
        return (StatusCode::BAD_REQUEST, "edges must be finite").into_response();
    }
    if params.half <= 0.0 || params.step <= 0.0 {
        return (StatusCode::BAD_REQUEST, "half and step must be positive").into_response();
    }
    if params.step < HEIGHT_GRID_MIN_STEP_M {
        return (
            StatusCode::BAD_REQUEST,
            format!("step must be at least {HEIGHT_GRID_MIN_STEP_M} metres"),
        )
            .into_response();
    }
    let samples_per_side = ((params.half * 2.0) / params.step).floor() as u32 + 1;
    if samples_per_side > HEIGHT_GRID_MAX_SIZE {
        return (
            StatusCode::BAD_REQUEST,
            format!("grid side must be at most {HEIGHT_GRID_MAX_SIZE}"),
        )
            .into_response();
    }
    let world = state.world.get();
    let x0 = params.x - params.half;
    let y0 = params.y - params.half;
    let heights: Vec<Vec<Option<f32>>> = (0..samples_per_side)
        .map(|row| {
            (0..samples_per_side)
                .map(|column| {
                    let sx = x0 + (column as f32) * params.step;
                    let sy = y0 + (row as f32) * params.step;
                    world.ground_height(sx, sy)
                })
                .collect()
        })
        .collect();
    Json(HeightGridView {
        x0,
        y0,
        step: params.step,
        size: samples_per_side,
        heights,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
struct SpawnParams {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    kinds: String,
}

async fn spawns(State(state): State<AppState>, Query(params): Query<SpawnParams>) -> Response {
    let answer = SpawnKind::parse_list(&params.kinds).and_then(|kinds| {
        state.world.get().spawns(&SpawnQuery {
            min_x: params.min_x,
            min_y: params.min_y,
            max_x: params.max_x,
            max_y: params.max_y,
            kinds,
        })
    });
    match answer {
        Ok(points) => Json(points).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct KindParams {
    kinds: String,
}

/// The spawn counts per cell of the zoomed out map, with the cell edge.
#[derive(Debug, Serialize)]
struct SpawnCells {
    cell_m: f32,
    cells: Vec<SpawnCell>,
}

async fn spawn_cells(State(state): State<AppState>, Query(params): Query<KindParams>) -> Response {
    match SpawnKind::parse_list(&params.kinds) {
        Ok(kinds) => Json(SpawnCells {
            cell_m: SPAWN_CELL_M,
            cells: state.world.get().spawn_cells(&kinds),
        })
        .into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct SessionParams {
    #[serde(default)]
    q: String,
}

async fn sessions(State(state): State<AppState>, Query(params): Query<SessionParams>) -> Response {
    Json(search(&state.captures.sessions, &params.q)).into_response()
}

async fn type_label(State(state): State<AppState>, UrlPath(idx): UrlPath<u64>) -> Response {
    Json(state.captures.labels.label(idx)).into_response()
}

fn png_response(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, PNG)], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::captures::Captures;
    use crate::captures::catalog::SessionEntry;
    use crate::world::tests::{REGION_M, test_layer};

    /// Ground height of the south-east land quarter of the test world.
    const LAND_M: f64 = 100.0;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";

    fn app(map_dir: &Path) -> Router {
        app_with(map_dir, Captures::default())
    }

    fn app_with(map_dir: &Path, captures: Captures) -> Router {
        router(AppState::new(
            test_layer(CAP),
            map_dir.to_path_buf(),
            AdminOrigin::of(LISTEN.parse().expect("address")),
            captures,
            None,
        ))
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, Option<String>, Vec<u8>) {
        let response = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::HOST, LISTEN)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, content_type, body.to_vec())
    }

    #[tokio::test]
    async fn the_index_and_web_files_are_served() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, content_type, body) = get(app(dir.path()), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some(crate::assets::HTML));
        assert!(String::from_utf8_lossy(&body).contains("<html"));
        let (status, content_type, _) = get(app(dir.path()), "/web/style.css").await;
        assert_eq!(
            (status, content_type.as_deref()),
            (StatusCode::OK, Some(crate::assets::CSS))
        );
    }

    #[tokio::test]
    async fn the_live_page_is_served_at_live() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, content_type, body) = get(app(dir.path()), LIVE_PAGE_ROUTE).await;
        assert_eq!(
            (status, content_type.as_deref()),
            (StatusCode::OK, Some(crate::assets::HTML))
        );
        assert!(String::from_utf8_lossy(&body).contains("/web/live.js"));
    }

    #[tokio::test]
    async fn the_studio_page_is_served_at_studio() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, content_type, body) = get(app(dir.path()), STUDIO_PAGE_ROUTE).await;
        assert_eq!(
            (status, content_type.as_deref()),
            (StatusCode::OK, Some(crate::assets::HTML))
        );
        assert!(String::from_utf8_lossy(&body).contains("/web/studio.js"));
        for file in [
            "studio.js",
            "apply.js",
            "design.js",
            "editors.js",
            "record-edit.js",
            "records.js",
        ] {
            let (status, _, _) = get(app(dir.path()), &format!("/web/{file}")).await;
            assert_eq!(status, StatusCode::OK, "{file}");
        }
    }

    #[tokio::test]
    async fn the_records_page_is_served_at_records() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(app(dir.path()), RECORDS_PAGE_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("/web/records.js"));
        let (status, _, body) = get(app(dir.path()), TODO_PAGE_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("/web/todo.js"));
        let (status, _, body) = get(app(dir.path()), SCRIPTS_PAGE_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("/web/scripts.js"));
        for file in ["todo.js", "todo-model.js", "scripts.js", "script-edit.js"] {
            let (status, _, _) = get(app(dir.path()), &format!("/web/{file}")).await;
            assert_eq!(status, StatusCode::OK, "{file}");
        }
    }

    #[tokio::test]
    async fn the_gm_page_is_served_at_gm() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(app(dir.path()), GM_PAGE_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("/web/gm-page.js"));
        for file in ["gm-page.js", "gm.js"] {
            let (status, _, _) = get(app(dir.path()), &format!("/web/{file}")).await;
            assert_eq!(status, StatusCode::OK, "{file}");
        }
    }

    #[tokio::test]
    async fn an_unknown_web_file_is_404() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, _) = get(app(dir.path()), "/web/secret.txt").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn info_gives_the_world_size() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(app(dir.path()), "/api/world/info").await;
        assert_eq!(status, StatusCode::OK);
        let info: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(info["world_m"], REGION_M);
    }

    #[tokio::test]
    async fn height_gives_the_ground_or_null() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(app(dir.path()), &format!("{HEIGHT_ROUTE}?x=1500&y=200")).await;
        assert_eq!(status, StatusCode::OK);
        let land: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(land["z"], LAND_M);
        let (_, _, body) = get(app(dir.path()), &format!("{HEIGHT_ROUTE}?x=-5&y=200")).await;
        let off: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(off["z"].is_null());
    }

    #[tokio::test]
    async fn a_missing_tile_is_404_and_a_present_one_is_png() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, _) = get(app(dir.path()), "/api/world/tiles/1/0/0").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        std::fs::create_dir_all(dir.path().join("1")).expect("dir");
        std::fs::write(dir.path().join("1").join("0_0.png"), b"png bytes").expect("write");
        let (status, content_type, body) = get(app(dir.path()), "/api/world/tiles/1/0/0").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some(PNG));
        assert_eq!(body, b"png bytes");
    }

    #[tokio::test]
    async fn relief_tiles_are_png_and_outside_ones_404() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, content_type, _) = get(app(dir.path()), "/api/world/relief/4/0/0").await;
        assert_eq!(
            (status, content_type.as_deref()),
            (StatusCode::OK, Some(PNG))
        );
        let (status, _, _) = get(app(dir.path()), "/api/world/relief/4/5/0").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn spawns_come_as_json_and_a_bad_box_is_400() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(
            app(dir.path()),
            "/api/world/spawns?min_x=0&min_y=0&max_x=2048&max_y=2048&kinds=vitals",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let points: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(points.as_array().map(Vec::len), Some(1));
        let (status, _, _) = get(
            app(dir.path()),
            "/api/world/spawns?min_x=9&min_y=0&max_x=1&max_y=2048&kinds=vitals",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn spawn_cells_come_with_the_cell_edge_and_a_bad_kind_is_400() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(
            app(dir.path()),
            "/api/world/spawn-cells?kinds=vitals,landmarks",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let answer: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(answer["cell_m"].as_f64(), Some(f64::from(SPAWN_CELL_M)));
        assert_eq!(answer["cells"].as_array().map(Vec::len), Some(2));
        let (status, _, _) = get(app(dir.path()), "/api/world/spawn-cells?kinds=dragons").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn shapes_come_as_flat_triangles_and_need_the_collision_file() {
        use nw_world::collision::{Body, Geometry, IDENTITY_AFFINE, LevelCollision};
        use nw_world::collision_index::CollisionIndex;

        let dir = tempfile::tempdir().expect("dir");
        let (status, _, _) = get(app(dir.path()), "/api/world/shapes?x=0&y=0&half=10").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no shapes on this site");
        let index = CollisionIndex::new(LevelCollision {
            geometries: vec![Geometry::Box {
                half_extents: [1.0, 1.0, 1.0],
            }],
            bodies: vec![Body {
                geometry: 0,
                place: IDENTITY_AFFINE,
            }],
            ..LevelCollision::default()
        });
        let with_shapes = || {
            router(
                AppState::new(
                    test_layer(CAP),
                    dir.path().to_path_buf(),
                    AdminOrigin::of(LISTEN.parse().expect("address")),
                    Captures::default(),
                    None,
                )
                .with_shapes(LevelShapes::of_index(index.clone())),
            )
        };
        let (status, _, body) = get(with_shapes(), "/api/world/shapes?x=0&y=0&half=10").await;
        assert_eq!(status, StatusCode::OK);
        let answer: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(answer["triangles"].as_array().map(Vec::len), Some(12 * 3));
        assert_eq!(answer["points"].as_array().map(Vec::len), Some(8 * 3));
        assert_eq!(answer["complete"], serde_json::Value::Bool(true));
        let (status, _, _) = get(with_shapes(), "/api/world/shapes?x=0&y=0&half=5000").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "too large a square");
    }

    #[tokio::test]
    async fn sessions_are_listed_and_searched() {
        let dir = tempfile::tempdir().expect("dir");
        let entry = |id: &str| SessionEntry {
            id: id.to_owned(),
            path: dir.path().join(id),
            bytes: 1,
            name: None,
            started_at: None,
            post_title: None,
        };
        let captures = Captures {
            sessions: vec![entry("dummy/ledger.bin"), entry("raid/ledger.bin")],
            ..Captures::default()
        };
        let (status, _, body) = get(
            app_with(dir.path(), captures),
            "/api/captures/sessions?q=raid",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let sessions: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(sessions[0]["id"], "raid/ledger.bin");
        assert_eq!(sessions.as_array().map(Vec::len), Some(1));
        assert!(sessions[0].get("path").is_none(), "no local path is sent");
    }

    #[tokio::test]
    async fn a_type_label_comes_as_json() {
        let dir = tempfile::tempdir().expect("dir");
        let (status, _, body) = get(app(dir.path()), "/api/captures/types/8").await;
        assert_eq!(status, StatusCode::OK);
        let label: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(label["name"].is_null());
    }
}
