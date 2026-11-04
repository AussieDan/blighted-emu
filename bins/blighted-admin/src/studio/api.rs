// SPDX-License-Identifier: AGPL-3.0-or-later
//! The studio routes: the design files, one file's text and its edits, the full check, and
//! Apply.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use nw_design::{
    DesignError, DesignFile, Expected, Kind, check_delete, check_put, check_schema, delete_file,
    design_path, file_version, is_expected, load_dir, read_file, record_field_names, write_file,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::server::AppState;
use crate::studio::Studio;

/// The design files with their schema problems.
pub const FILES_ROUTE: &str = "/api/studio/files";
/// `GET`: one design file's text; `PUT`: writes it when the check finds nothing; `DELETE`:
/// removes it when no other file needs it. The file is `?path=`, relative to the design folder.
/// An edit with `If-Match: <version>` replaces only that version of the file, and one with
/// `If-None-Match: *` only makes a new file; otherwise the answer is 412. A done `PUT` gives
/// the version of the written text in `ETag`.
pub const FILE_ROUTE: &str = "/api/studio/file";
/// Search of the client records of a kind: `?q=` (id or name, any case).
pub const RECORDS_ROUTE: &str = "/api/studio/records/{kind}";
/// One client record with its field names and the design files that change or copy it.
pub const RECORD_ROUTE: &str = "/api/studio/records/{kind}/{id}";
/// The script blocks, for the script editor.
pub const BLOCKS_ROUTE: &str = "/api/studio/blocks";
/// The full check; writes nothing.
pub const CHECK_ROUTE: &str = "/api/studio/check";
/// `GET`: the Apply state; `POST`: starts Apply.
pub const APPLY_ROUTE: &str = "/api/studio/apply";
/// The answer when the work slot is taken.
const BUSY: &str = "an Apply or a Check runs";
/// Most records one search returns.
const SEARCH_LIMIT: usize = 200;
/// The answer to a kind the record routes do not serve.
const NO_RECORDS: &str = "this kind has no client records";
/// The answer to an edit whose file is not the one its editor opened.
const CHANGED: &str = "the file changed after it was opened: open it again";
/// The `If-None-Match` value of an edit that makes a new file.
const ANY_FILE: &str = "*";
/// The answer to a path outside the design folder.
const BAD_PATH: &str = "the path must name a .toml file inside the design folder";

/// The studio routes; every `POST` needs the admin header (the guard).
pub fn studio_routes() -> Router<AppState> {
    Router::new()
        .route(FILES_ROUTE, get(files))
        .route(
            FILE_ROUTE,
            get(read_design_file)
                .put(put_design_file)
                .delete(delete_design_file),
        )
        .route(
            BLOCKS_ROUTE,
            get(|| async { Json(nw_world::script::BLOCKS) }),
        )
        .route(RECORDS_ROUTE, get(search_records))
        .route(RECORD_ROUTE, get(read_record))
        .route(CHECK_ROUTE, post(check))
        .route(APPLY_ROUTE, get(apply_status).post(start_apply))
}

/// What the files route returns.
#[derive(Debug, Serialize)]
struct FilesView {
    files: Vec<DesignFile>,
    errors: Vec<DesignError>,
}

/// The file of a file route.
#[derive(Debug, Deserialize)]
struct FileParams {
    path: String,
}

/// The query of the records route.
#[derive(Debug, Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
}

/// One client record and the design files about it.
#[derive(Debug, Serialize)]
struct RecordView {
    id: String,
    /// The folder of this kind's design files.
    folder: &'static str,
    name: Option<String>,
    fields: Map<String, Value>,
    field_names: Vec<&'static str>,
    files: Vec<DesignFile>,
}

/// The problems that refused an edit.
#[derive(Debug, Serialize)]
struct Refused {
    errors: Vec<DesignError>,
}

/// How an edit of one design file ended.
enum EditOutcome {
    Done,
    Refused(Vec<DesignError>),
    Missing,
    Changed,
    Failed(std::io::Error),
}

fn studio_of(state: &AppState) -> Option<&Studio> {
    state.studio.as_ref()
}

async fn files(State(state): State<AppState>) -> Response {
    let Some(studio) = studio_of(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let design_dir = studio.paths.design_dir.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let (set, mut errors) = load_dir(&design_dir);
        errors.extend(check_schema(&set));
        FilesView {
            files: set.files,
            errors,
        }
    })
    .await;
    match loaded {
        Ok(view) => Json(view).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The catalogue kind of a route's `kind`; `None` for a kind with no client records.
fn catalog_kind(name: &str) -> Option<Kind> {
    Kind::from_name(name).filter(|kind| !record_field_names(*kind).is_empty())
}

async fn search_records(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Query(params): Query<SearchParams>,
) -> Response {
    let Some(studio) = studio_of(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(kind) = catalog_kind(&kind) else {
        return (StatusCode::NOT_FOUND, NO_RECORDS).into_response();
    };
    match studio.catalog().await {
        Ok(catalog) => Json(catalog.search(kind, &params.q, SEARCH_LIMIT)).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    }
}

async fn read_record(
    State(state): State<AppState>,
    UrlPath((kind, id)): UrlPath<(String, String)>,
) -> Response {
    let Some(studio) = studio_of(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(kind) = catalog_kind(&kind) else {
        return (StatusCode::NOT_FOUND, NO_RECORDS).into_response();
    };
    let catalog = match studio.catalog().await {
        Ok(catalog) => catalog,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    let Some(record) = catalog.record(kind, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let design_dir = studio.paths.design_dir.clone();
    let record_id = record.id.clone();
    let files = tokio::task::spawn_blocking(move || {
        let (set, _) = load_dir(&design_dir);
        set.files
            .into_iter()
            .filter(|file| {
                file.kind == kind
                    && (file.id.eq_ignore_ascii_case(&record_id)
                        || file
                            .base
                            .as_deref()
                            .is_some_and(|base| base.eq_ignore_ascii_case(&record_id)))
            })
            .collect::<Vec<_>>()
    })
    .await;
    match files {
        Ok(files) => Json(RecordView {
            id: record.id.clone(),
            folder: kind.folder(),
            name: record.name.clone(),
            fields: record.fields(),
            field_names: record_field_names(kind),
            files,
        })
        .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn read_design_file(
    State(state): State<AppState>,
    Query(params): Query<FileParams>,
) -> Response {
    let Some(studio) = studio_of(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(relative) = design_path(&params.path) else {
        return (StatusCode::BAD_REQUEST, BAD_PATH).into_response();
    };
    let dir = studio.paths.design_dir.clone();
    match tokio::task::spawn_blocking(move || read_file(&dir, &relative)).await {
        Ok(Ok(text)) => text.into_response(),
        Ok(Err(error)) => io_error_response(&error),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// The answer to a failed read, write or removal of a design file.
fn io_error_response(error: &std::io::Error) -> Response {
    match error.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND.into_response(),
        std::io::ErrorKind::PermissionDenied => {
            (StatusCode::FORBIDDEN, error.to_string()).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn put_design_file(
    State(state): State<AppState>,
    Query(params): Query<FileParams>,
    headers: HeaderMap,
    text: String,
) -> Response {
    edit(&state, &params.path, expected(&headers), Some(text)).await
}

async fn delete_design_file(
    State(state): State<AppState>,
    Query(params): Query<FileParams>,
    headers: HeaderMap,
) -> Response {
    edit(&state, &params.path, expected(&headers), None).await
}

/// What an edit expects at its path, from its `If-Match` or `If-None-Match` header.
fn expected(headers: &HeaderMap) -> Expected {
    let value = |name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().trim_matches('"').to_owned())
    };
    if let Some(version) = value(header::IF_MATCH) {
        Expected::Version(version)
    } else if value(header::IF_NONE_MATCH).as_deref() == Some(ANY_FILE) {
        Expected::Nothing
    } else {
        Expected::Anything
    }
}

/// Writes `text` to the design file `raw` (removes it when `None`) unless the check finds a
/// problem. It takes the work slot, so no Apply or Check reads the folder meanwhile.
async fn edit(state: &AppState, raw: &str, expected: Expected, text: Option<String>) -> Response {
    let Some(studio) = studio_of(state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(relative) = design_path(raw) else {
        return (StatusCode::BAD_REQUEST, BAD_PATH).into_response();
    };
    let Ok(permit) = Arc::clone(&studio.work).try_acquire_owned() else {
        return (StatusCode::CONFLICT, BUSY).into_response();
    };
    let dir = studio.paths.design_dir.clone();
    let written_version = text.as_deref().map(file_version);
    let outcome = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        edit_blocking(&dir, &relative, &expected, text.as_deref())
    })
    .await;
    match outcome {
        Ok(EditOutcome::Done) => match written_version {
            Some(version) => (StatusCode::NO_CONTENT, [(header::ETAG, version)]).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
        Ok(EditOutcome::Refused(errors)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(Refused { errors })).into_response()
        }
        Ok(EditOutcome::Missing) => StatusCode::NOT_FOUND.into_response(),
        Ok(EditOutcome::Changed) => (StatusCode::PRECONDITION_FAILED, CHANGED).into_response(),
        Ok(EditOutcome::Failed(error)) => io_error_response(&error),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

fn edit_blocking(
    dir: &Path,
    relative: &Path,
    expected: &Expected,
    text: Option<&str>,
) -> EditOutcome {
    match is_expected(dir, relative, expected) {
        Ok(true) => {}
        Ok(false) => return EditOutcome::Changed,
        Err(error) => return EditOutcome::Failed(error),
    }
    let errors = match text {
        Some(text) => check_put(dir, relative, text),
        None if !dir.join(relative).is_file() => return EditOutcome::Missing,
        None => check_delete(dir, relative),
    };
    if !errors.is_empty() {
        return EditOutcome::Refused(errors);
    }
    let written = match text {
        Some(text) => write_file(dir, relative, text),
        None => delete_file(dir, relative),
    };
    match written {
        Ok(()) => EditOutcome::Done,
        Err(error) => EditOutcome::Failed(error),
    }
}

async fn check(State(state): State<AppState>) -> Response {
    let Some(studio) = studio_of(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match studio.check().await {
        Some(Ok(report)) => Json(report).into_response(),
        Some(Err(error)) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
        None => (StatusCode::CONFLICT, BUSY).into_response(),
    }
}

async fn apply_status(State(state): State<AppState>) -> Response {
    match studio_of(&state) {
        Some(studio) => Json(studio.apply_status()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn start_apply(State(state): State<AppState>) -> Response {
    match studio_of(&state) {
        Some(studio) if studio.start_apply() => StatusCode::ACCEPTED.into_response(),
        Some(_) => (StatusCode::CONFLICT, BUSY).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::guard::AdminOrigin;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Method, Request, header};
    use http_body_util::BodyExt;
    use nw_bus::{BusId, SimHeartbeat};
    use nw_common::time::unix_ms;
    use nw_content::ContentBundle;
    use nw_content::bundle::bundle_path;
    use nw_world::WorldFile;
    use nw_world::records::{LevelInfo, VitalsSpawn};
    use nw_world::world_file::world_file_path;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::captures::Captures;
    use crate::config::StudioConfig;
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE};
    use crate::live::{SharedState, lock, shared};
    use crate::server::{AppState, router};
    use crate::studio::catalog::tests::{SWORD, SWORD_NAME, TIER};
    use crate::studio::{Studio, catalog};
    use crate::world::tests::test_layer;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const BUILD: &str = "22469132";
    const MAP: &str = "lv";
    const SIM: &str = "sim-1";
    const POLLS: usize = 200;
    const POLL: Duration = Duration::from_millis(50);
    const SHORT_TIMEOUT_S: u64 = 1;
    const LONG_TIMEOUT_S: u64 = 30;

    struct Fixture {
        root: tempfile::TempDir,
        live: SharedState,
        studio: Studio,
    }

    impl Fixture {
        fn path(&self, relative: &str) -> PathBuf {
            self.root.path().join(relative)
        }

        fn app(&self) -> axum::Router {
            router(
                AppState::new(
                    test_layer(CAP),
                    std::env::temp_dir(),
                    AdminOrigin::of(LISTEN.parse().expect("address")),
                    Captures::default(),
                    None,
                )
                .with_studio(self.studio.clone()),
            )
        }

        fn spawn_file(&self, key: &str, fields: &str) {
            let path = self.path(&format!("content/spawns/{MAP}/{key}.toml"));
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(
                path,
                format!("schema = 1\nkind = \"spawn_point\"\naction = \"override\"\nid = \"{key}\"\nmap = \"{MAP}\"\n{fields}\n"),
            )
            .expect("write");
        }

        fn merged_bundle(&self) -> PathBuf {
            bundle_path(&self.path("out"), BUILD)
        }
    }

    fn world() -> WorldFile {
        let mut world = WorldFile {
            level: LevelInfo {
                name: MAP.to_owned(),
                region_size: 256,
                grid_width: 4,
                grid_height: 4,
                ..LevelInfo::default()
            },
            vitals: vec![VitalsSpawn {
                vitals_id: 1,
                level: 5,
                position: [1.0, 2.0, 3.0],
                ..VitalsSpawn::default()
            }],
            ..WorldFile::default()
        };
        world.fill_point_keys();
        world
    }

    fn key() -> String {
        world().vitals_keys[0].clone()
    }

    fn fixture(fifo: bool, restart_timeout_s: u64) -> Fixture {
        let root = tempfile::tempdir().expect("dir");
        let retail = root.path().join("retail");
        ContentBundle {
            client_build: BUILD.to_owned(),
            ..catalog::tests::bundle()
        }
        .save(&bundle_path(&retail, BUILD))
        .expect("bundle");
        world()
            .save(&world_file_path(&retail, BUILD, MAP), BUILD)
            .expect("world");
        let control_fifo = root.path().join("control");
        if fifo {
            let status = std::process::Command::new("mkfifo")
                .arg(&control_fifo)
                .status()
                .expect("mkfifo");
            assert!(status.success());
        }
        let config = StudioConfig {
            design_dir: root.path().join("content"),
            retail_dir: retail,
            control_fifo,
            restart_timeout_s,
            github_api_url: crate::config::GITHUB_API_URL.to_owned(),
            github_token_env: None,
        };
        let live = shared();
        let studio = Studio::new(&config, root.path().join("out"), BUILD, Some(live.clone()))
            .expect("studio");
        Fixture { root, live, studio }
    }

    fn beat(started_at_ms: u64) -> SimHeartbeat {
        SimHeartbeat {
            id: BusId::try_from(SIM.to_owned()).expect("id"),
            started_at_ms,
            players: 0,
            max_players: 1,
            level: MAP.to_owned(),
            instance: None,
        }
    }

    /// Reads the control FIFO like `live-stack.sh` (opened read-write), and calls `on_line`
    /// for each line.
    fn read_control(path: &Path, on_line: impl Fn(&str) + Send + 'static) {
        let fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open fifo");
        std::thread::spawn(move || {
            for line in BufReader::new(fifo).lines() {
                on_line(&line.expect("line"));
            }
        });
    }

    fn request(method: Method, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, LISTEN)
            .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
            .body(Body::from(body.to_owned()))
            .expect("request")
    }

    /// The status and raw body of `method uri` with `body`.
    async fn send(
        fixture: &Fixture,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, Vec<u8>) {
        let response = fixture
            .app()
            .oneshot(request(method, uri, body))
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, bytes.to_vec())
    }

    async fn call(fixture: &Fixture, method: Method, uri: &str) -> (StatusCode, Value) {
        let (status, bytes) = send(fixture, method, uri, "").await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    const PATH_FILE: &str = "paths/lv/walk.toml";
    const PATH_TEXT: &str = "schema = 1\nkind = \"path\"\naction = \"add\"\nid = \"walk\"\nmap = \"lv\"\npoints = [[1.0, 2.0, 3.0]]\n";
    const GROUP_FILE: &str = "spawns/lv/wolves.toml";
    const GROUP_TEXT: &str = "schema = 1\nkind = \"spawn_group\"\naction = \"add\"\nid = \"wolves\"\nmap = \"lv\"\nvitals = \"Wolf\"\nlevel = 5\npoints = [[1.0, 2.0, 3.0]]\npath = \"walk\"\n";

    fn file_uri(path: &str) -> String {
        format!("{FILE_ROUTE}?path={}", path.replace('/', "%2F"))
    }

    async fn put(fixture: &Fixture, path: &str, text: &str) -> (StatusCode, Value) {
        let (status, bytes) = send(fixture, Method::PUT, &file_uri(path), text).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn records_are_searched_and_read_with_their_design_files() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let (status, hits) = call(&fixture, Method::GET, "/api/studio/records/item?q=IRON").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hits[0]["id"], SWORD);
        assert_eq!(hits[0]["name"], SWORD_NAME);
        let (status, _) = call(&fixture, Method::GET, "/api/studio/records/path").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let override_text = format!(
            "schema = 1\nkind = \"item\"\naction = \"override\"\nid = \"{SWORD}\"\ntier = 3\n"
        );
        let (status, _) = put(&fixture, &format!("items/{SWORD}.toml"), &override_text).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let uri = format!("/api/studio/records/item/{}", SWORD.to_uppercase());
        let (status, record) = call(&fixture, Method::GET, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(record["id"], SWORD);
        assert_eq!(record["folder"], "items");
        assert_eq!(record["fields"]["tier"], TIER);
        assert!(
            record["field_names"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "gear_score_override"))
        );
        assert_eq!(record["files"][0]["fields"]["tier"], 3);
        let (status, _) = call(&fixture, Method::GET, "/api/studio/records/item/NoSuchItem").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_simulator_checks_its_request_and_no_loot_lists_mobs() {
        use crate::studio::loot::{NO_LOOT_ROUTE, SIMULATE_ROUTE};
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let body = |table: &str, rolls: u32| {
            format!(r#"{{"table":"{table}","rolls":{rolls},"enemy_level":1,"content_level":1}}"#)
        };
        let post = |uri: &str, text: String| {
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::HOST, LISTEN)
                .header(header::CONTENT_TYPE, "application/json")
                .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
                .body(Body::from(text))
                .expect("request")
        };
        let too_many = fixture
            .app()
            .oneshot(post(SIMULATE_ROUTE, body("Any", 100_001)))
            .await
            .expect("response");
        assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);
        let missing = fixture
            .app()
            .oneshot(post(SIMULATE_ROUTE, body("NoSuchTable", 10)))
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let (status, mobs) = call(&fixture, Method::GET, NO_LOOT_ROUTE).await;
        assert_eq!((status, mobs), (StatusCode::OK, serde_json::json!([])));
    }

    #[tokio::test]
    async fn the_first_catalogue_load_waits_for_the_work_slot() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let held = Arc::clone(&fixture.studio.work)
            .try_acquire_owned()
            .expect("slot");
        let studio = fixture.studio.clone();
        let load = tokio::spawn(async move { studio.catalog().await.map(|_| ()) });
        tokio::time::sleep(POLL).await;
        assert!(!load.is_finished(), "it waits while an Apply runs");
        drop(held);
        let loaded = tokio::time::timeout(Duration::from_secs(LONG_TIMEOUT_S), load)
            .await
            .expect("in time")
            .expect("task");
        assert_eq!(loaded, Ok(()));
    }

    #[tokio::test]
    async fn the_blocks_route_lists_the_registry() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let (status, blocks) = call(&fixture, Method::GET, BLOCKS_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            blocks.as_array().map(Vec::len),
            Some(nw_world::script::BLOCKS.len())
        );
        assert_eq!(blocks[0]["category"], "trigger");
        assert_eq!(blocks[0]["values"][0]["kind"], "position");
    }

    #[tokio::test]
    async fn a_put_writes_a_file_that_get_returns() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let (status, _) = put(&fixture, PATH_FILE, PATH_TEXT).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, text) = send(&fixture, Method::GET, &file_uri(PATH_FILE), "").await;
        assert_eq!(
            (status, text.as_slice()),
            (StatusCode::OK, PATH_TEXT.as_bytes())
        );
        let (status, _) = send(&fixture, Method::GET, &file_uri("paths/lv/none.toml"), "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The status of `method` on `path` with `text` and the header `name: value`.
    async fn send_with(
        fixture: &Fixture,
        method: Method,
        path: &str,
        text: &str,
        (name, value): (header::HeaderName, &str),
    ) -> (StatusCode, Option<String>) {
        let mut request = request(method, &file_uri(path), text);
        request
            .headers_mut()
            .insert(name, value.parse().expect("header"));
        let response = fixture.app().oneshot(request).await.expect("response");
        let version = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        (response.status(), version)
    }

    #[tokio::test]
    async fn an_edit_of_a_changed_file_is_refused() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let new = (header::IF_NONE_MATCH, ANY_FILE);
        let made = send_with(&fixture, Method::PUT, PATH_FILE, PATH_TEXT, new.clone()).await;
        assert_eq!(made.0, StatusCode::NO_CONTENT);
        assert_eq!(made.1.as_deref(), Some(file_version(PATH_TEXT).as_str()));
        let again = send_with(&fixture, Method::PUT, PATH_FILE, PATH_TEXT, new).await;
        assert_eq!(
            again.0,
            StatusCode::PRECONDITION_FAILED,
            "it is not new now"
        );
        let (_, files) = call(&fixture, Method::GET, FILES_ROUTE).await;
        let opened = files["files"][0]["version"]
            .as_str()
            .expect("version")
            .to_owned();
        let other = PATH_TEXT.replace("[[1.0", "[[5.0");
        let first = (header::IF_MATCH, opened.as_str());
        let saved = send_with(&fixture, Method::PUT, PATH_FILE, &other, first.clone()).await;
        assert_eq!(saved.0, StatusCode::NO_CONTENT);
        let late = send_with(&fixture, Method::PUT, PATH_FILE, PATH_TEXT, first.clone()).await;
        assert_eq!(late.0, StatusCode::PRECONDITION_FAILED, "the second editor");
        let removed = send_with(&fixture, Method::DELETE, PATH_FILE, "", first).await;
        assert_eq!(removed.0, StatusCode::PRECONDITION_FAILED);
        let (_, text) = send(&fixture, Method::GET, &file_uri(PATH_FILE), "").await;
        assert_eq!(text, other.as_bytes());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_through_a_symbolic_link_is_refused() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("walk.toml"), PATH_TEXT).expect("write");
        std::fs::create_dir_all(fixture.path("content")).expect("dir");
        std::os::unix::fs::symlink(outside.path(), fixture.path("content/paths")).expect("link");
        let linked = "paths/walk.toml";
        let (status, _) = send(&fixture, Method::GET, &file_uri(linked), "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = put(&fixture, linked, PATH_TEXT).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&fixture, Method::DELETE, &file_uri(linked)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(outside.path().join("walk.toml").exists());
    }

    #[tokio::test]
    async fn a_path_outside_the_design_folder_is_refused() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        for path in ["../x.toml", "/tmp/x.toml", "ai/x.txt", "todo.toml"] {
            let (status, _) = put(&fixture, path, PATH_TEXT).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            let (status, _) = send(&fixture, Method::GET, &file_uri(path), "").await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        }
        assert!(!fixture.path("x.toml").exists());
    }

    #[tokio::test]
    async fn a_put_with_problems_is_refused_and_writes_nothing() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let (status, body) = put(&fixture, GROUP_FILE, GROUP_TEXT).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["problem"]["code"], "unknown_reference");
        assert!(!fixture.path("content").join(GROUP_FILE).exists());
        put(&fixture, PATH_FILE, PATH_TEXT).await;
        let (status, _) = put(&fixture, GROUP_FILE, GROUP_TEXT).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn a_delete_that_another_file_needs_is_refused() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        put(&fixture, PATH_FILE, PATH_TEXT).await;
        put(&fixture, GROUP_FILE, GROUP_TEXT).await;
        let (status, body) = call(&fixture, Method::DELETE, &file_uri(PATH_FILE)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["path"], GROUP_FILE);
        let (status, _) = call(&fixture, Method::DELETE, &file_uri(GROUP_FILE)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&fixture, Method::DELETE, &file_uri(GROUP_FILE)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_edit_while_apply_runs_is_refused() {
        let fixture = fixture(true, SHORT_TIMEOUT_S);
        read_control(&fixture.path("control"), |_| {});
        lock(&fixture.live).heartbeat(beat(1), unix_ms());
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        let (status, _) = put(&fixture, PATH_FILE, PATH_TEXT).await;
        assert_eq!(status, StatusCode::CONFLICT);
        finished(&fixture).await;
    }

    /// The Apply state once it is no longer running.
    async fn finished(fixture: &Fixture) -> Value {
        for _ in 0..POLLS {
            let (_, status) = call(fixture, Method::GET, APPLY_ROUTE).await;
            if status["phase"] != "running" {
                return status;
            }
            tokio::time::sleep(POLL).await;
        }
        panic!("apply still running");
    }

    #[tokio::test]
    async fn files_lists_design_files_and_errors() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        fixture.spawn_file(&key(), "level = 30");
        fixture.spawn_file("1_0.0_0.0_0.0", "level = 66");
        let (status, body) = call(&fixture, Method::GET, FILES_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["files"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["errors"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["errors"][0]["problem"]["code"], "out_of_range");
    }

    #[tokio::test]
    async fn check_reports_unknown_ids_without_writing() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        fixture.spawn_file("1_0.0_0.0_0.0", "level = 30");
        let (status, body) = call(&fixture, Method::POST, CHECK_ROUTE).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["errors"][0]["problem"]["code"], "unknown_id");
        assert_eq!(body["written"], false);
        assert!(!fixture.merged_bundle().exists());
    }

    #[tokio::test]
    async fn apply_writes_and_finishes_without_fifo() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        fixture.spawn_file(&key(), "level = 30");
        let (status, _) = call(&fixture, Method::POST, APPLY_ROUTE).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let end = finished(&fixture).await;
        assert_eq!(end["phase"], "done", "{end}");
        assert!(
            end["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("control")
        );
        assert!(fixture.merged_bundle().exists());
    }

    #[tokio::test]
    async fn apply_with_errors_fails_and_writes_nothing() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        fixture.spawn_file("1_0.0_0.0_0.0", "level = 30");
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        let end = finished(&fixture).await;
        assert_eq!(end["phase"], "failed");
        assert_eq!(end["step"], "check");
        assert_eq!(end["errors"][0]["problem"]["code"], "unknown_id");
        assert!(!fixture.merged_bundle().exists());
    }

    #[tokio::test]
    async fn a_second_apply_while_running_is_refused() {
        let fixture = fixture(true, SHORT_TIMEOUT_S);
        read_control(&fixture.path("control"), |_| {});
        lock(&fixture.live).heartbeat(beat(1), unix_ms());
        let (first, _) = call(&fixture, Method::POST, APPLY_ROUTE).await;
        let (second, _) = call(&fixture, Method::POST, APPLY_ROUTE).await;
        assert_eq!(
            (first, second),
            (StatusCode::ACCEPTED, StatusCode::CONFLICT)
        );
        finished(&fixture).await;
    }

    #[tokio::test]
    async fn a_check_while_apply_runs_is_refused() {
        let fixture = fixture(true, SHORT_TIMEOUT_S);
        read_control(&fixture.path("control"), |_| {});
        lock(&fixture.live).heartbeat(beat(1), unix_ms());
        let (apply, _) = call(&fixture, Method::POST, APPLY_ROUTE).await;
        let (check, _) = call(&fixture, Method::POST, CHECK_ROUTE).await;
        assert_eq!((apply, check), (StatusCode::ACCEPTED, StatusCode::CONFLICT));
        finished(&fixture).await;
        let (after, _) = call(&fixture, Method::POST, CHECK_ROUTE).await;
        assert_eq!(after, StatusCode::OK, "the slot is free again");
    }

    #[tokio::test]
    async fn apply_fails_when_sims_do_not_return() {
        let fixture = fixture(true, SHORT_TIMEOUT_S);
        read_control(&fixture.path("control"), |_| {});
        lock(&fixture.live).heartbeat(beat(1), unix_ms());
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        let end = finished(&fixture).await;
        assert_eq!(end["phase"], "failed", "{end}");
        assert_eq!(end["step"], "wait_sims");
    }

    #[tokio::test]
    async fn apply_finishes_when_every_sim_restarts() {
        let fixture = fixture(true, LONG_TIMEOUT_S);
        let live = fixture.live.clone();
        read_control(&fixture.path("control"), move |line| {
            if line == "restart-world" {
                lock(&live).heartbeat(beat(unix_ms() + 1), unix_ms());
            }
        });
        lock(&fixture.live).heartbeat(beat(1), unix_ms());
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        let end = finished(&fixture).await;
        assert_eq!(end["phase"], "done", "{end}");
        assert_eq!(end["detail"], "1 of 1 sims back");
    }

    #[tokio::test]
    async fn apply_waits_for_sims_that_sent_no_heartbeat_before() {
        let fixture = fixture(true, LONG_TIMEOUT_S);
        let live = fixture.live.clone();
        read_control(&fixture.path("control"), move |line| {
            if line == "restart-world" {
                lock(&live).heartbeat(beat(unix_ms() + 1), unix_ms());
            }
        });
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        let end = finished(&fixture).await;
        assert_eq!(end["phase"], "done", "{end}");
        assert_eq!(
            end["detail"],
            "1 sims back; no sim was known before the restart"
        );
    }

    #[tokio::test]
    async fn apply_reads_the_map_world_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slot = crate::world::WorldSlot::new(test_layer(CAP));
        let before = slot.get();
        let loads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&loads);
        let mut fixture = fixture(false, LONG_TIMEOUT_S);
        fixture.studio = fixture.studio.clone().with_world_reload(
            slot.clone(),
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(test_layer(CAP))
            }),
        );
        call(&fixture, Method::POST, APPLY_ROUTE).await;
        assert_eq!(finished(&fixture).await["phase"], "done");
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(
            !Arc::ptr_eq(&before, &slot.get()),
            "the slot holds the new layer"
        );
    }

    #[tokio::test]
    async fn a_stopped_apply_job_ends_failed_at_its_step() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let error = tokio::spawn(async { panic!("job") })
            .await
            .expect_err("panic");
        let end = fixture.studio.stopped(&error);
        assert_eq!(end.phase, crate::studio::apply::ApplyPhase::Failed);
        assert_eq!(end.step, Some(crate::studio::apply::ApplyStep::Check));
        assert!(
            end.detail.starts_with("the Apply job stopped"),
            "{}",
            end.detail
        );
    }

    #[tokio::test]
    async fn the_apply_route_needs_the_admin_header() {
        let fixture = fixture(false, LONG_TIMEOUT_S);
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(APPLY_ROUTE)
                    .header(header::HOST, LISTEN)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
