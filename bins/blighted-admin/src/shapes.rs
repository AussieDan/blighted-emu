// SPDX-License-Identifier: AGPL-3.0-or-later
//! The placed solids of the level (rocks, buildings, props) for the 3D page: the level collision
//! file that the sims read, loaded the first time a page asks for it, and its triangles around a
//! point (`GET /api/world/shapes?x=&y=&half=`).

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nw_world::collision::LevelCollision;
use nw_world::collision_index::CollisionIndex;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::server::AppState;

/// The route of the triangles around a point.
pub const SHAPES_ROUTE: &str = "/api/world/shapes";
/// Largest half side of the asked square, metres (our choice: the 3D page's view distance).
pub const SHAPES_MAX_HALF_M: f32 = 512.0;
/// Most triangles of one answer (our choice: a few megabytes of JSON).
pub const SHAPES_MAX_TRIANGLES: usize = 200_000;

/// The level collision file, read once on first use.
#[derive(Debug, Clone)]
pub struct LevelShapes {
    path: PathBuf,
    client_build: String,
    index: Arc<OnceCell<Option<CollisionIndex>>>,
}

impl LevelShapes {
    /// The collision file at `path`, of `client_build`; nothing is read yet.
    pub fn new(path: PathBuf, client_build: String) -> Self {
        Self {
            path,
            client_build,
            index: Arc::new(OnceCell::new()),
        }
    }

    /// The same shapes from an index already made (tests).
    pub fn of_index(index: CollisionIndex) -> Self {
        Self {
            path: PathBuf::new(),
            client_build: String::new(),
            index: Arc::new(OnceCell::new_with(Some(Some(index)))),
        }
    }

    /// The index, read from the file off the async threads the first time; `None` when the file
    /// cannot be read (logged once).
    async fn index(&self) -> Option<&CollisionIndex> {
        self.index
            .get_or_init(|| {
                let (path, build) = (self.path.clone(), self.client_build.clone());
                async move {
                    let loaded = tokio::task::spawn_blocking(move || {
                        LevelCollision::load(&path, &build)
                            .map(CollisionIndex::new)
                            .map_err(|error| format!("{}: {error}", path.display()))
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|read| read);
                    loaded
                        .inspect_err(|error| {
                            tracing::warn!(%error, "level collision file for the 3D page");
                        })
                        .ok()
                }
            })
            .await
            .as_ref()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ShapeParams {
    x: f32,
    y: f32,
    half: f32,
}

/// Triangles as flat lists, for the page's buffers.
#[derive(Debug, Serialize)]
struct ShapesView {
    /// x, y, z of each corner, metres.
    points: Vec<f32>,
    /// Three corner indices for each triangle.
    triangles: Vec<u32>,
    /// False when the triangle limit left bodies out.
    complete: bool,
}

/// `GET`: the triangles of the solids whose ground-plane bounds meet the square of half side
/// `half` around (`x`, `y`). A bad square is a 400; a site without the collision file is a 404.
pub(crate) async fn shapes(
    State(state): State<AppState>,
    Query(params): Query<ShapeParams>,
) -> Response {
    if ![params.x, params.y, params.half]
        .iter()
        .all(|value| value.is_finite())
    {
        return (StatusCode::BAD_REQUEST, "x, y and half must be finite").into_response();
    }
    if params.half <= 0.0 || params.half > SHAPES_MAX_HALF_M {
        return (
            StatusCode::BAD_REQUEST,
            format!("half must be above 0 and at most {SHAPES_MAX_HALF_M} metres"),
        )
            .into_response();
    }
    let Some(shapes) = &state.shapes else {
        return (StatusCode::NOT_FOUND, "this site serves no level shapes").into_response();
    };
    let Some(index) = shapes.index().await else {
        return (
            StatusCode::NOT_FOUND,
            "the level collision file cannot be read (content-tool collision writes it)",
        )
            .into_response();
    };
    let found = index.triangles_in(
        [params.x - params.half, params.y - params.half],
        [params.x + params.half, params.y + params.half],
        SHAPES_MAX_TRIANGLES,
    );
    Json(ShapesView {
        points: found.points.into_iter().flatten().collect(),
        triangles: found.triangles.into_iter().flatten().collect(),
        complete: found.complete,
    })
    .into_response()
}
