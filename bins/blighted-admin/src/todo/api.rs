// SPDX-License-Identifier: AGPL-3.0-or-later
//! The to-do routes: the list, new tasks, changes, GitHub import, and the design files that
//! differ from the last commit (read-only `git`).

use std::collections::BTreeMap;
use std::path::Path;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use nw_design::{TODO_FILE, file_version};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::server::AppState;
use crate::studio::Studio;
use crate::todo::github::parse_issue_url;
use crate::todo::{Task, TaskKind, TaskState, TodoList, editor_url, load, save};

/// `GET`: the list; `POST`: a new task.
pub const TODO_ROUTE: &str = "/api/todo";
/// `PUT`: changes one task; `DELETE`: removes it. With `If-Match: <version>` (the task's
/// `version` in the list) the edit is refused with 412 when the task changed meanwhile.
pub const TASK_ROUTE: &str = "/api/todo/task/{id}";
/// `POST`: a new task from a GitHub issue.
pub const IMPORT_ROUTE: &str = "/api/todo/import";
/// `GET`: the design files that differ from the last commit.
pub const CHANGED_ROUTE: &str = "/api/todo/changed";
const NO_TITLE: &str = "a task needs a title";
const NO_TASK: &str = "no such task";
const TASK_CHANGED: &str = "someone changed this task after the list was read";
const BAD_ISSUE_URL: &str =
    "not a GitHub issue address (https://github.com/<owner>/<repo>/issues/<number>)";
/// The `git` program.
const GIT: &str = "git";

/// The to-do routes; every change needs the admin header (the guard) and local mode.
pub fn todo_routes() -> Router<AppState> {
    Router::new()
        .route(TODO_ROUTE, get(list).post(add))
        .route(TASK_ROUTE, put(change).delete(remove))
        .route(IMPORT_ROUTE, post(import))
        .route(CHANGED_ROUTE, get(changed))
}

/// The list with the editor page of every linked design file.
#[derive(Debug, Serialize)]
struct ListView {
    tasks: Vec<TaskView>,
    editors: BTreeMap<String, String>,
}

/// One task and its version, which a change sends back in `If-Match`.
#[derive(Debug, Serialize)]
struct TaskView {
    #[serde(flatten)]
    task: Task,
    version: String,
}

/// The version of `task`: the [`file_version`] of its JSON text.
fn task_version(task: &Task) -> String {
    file_version(&serde_json::to_string(task).unwrap_or_default())
}

/// The version an edit of one task expects, from its `If-Match` header.
fn expected_version(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().trim_matches('"').to_owned())
}

/// The task `id` of `list`, refused when it is gone or is not the `expected` version.
fn task_to_edit<'list>(
    list: &'list mut TodoList,
    id: &str,
    expected: Option<&str>,
) -> Result<&'list mut Task, Refusal> {
    let task = list.get_mut(id).ok_or((StatusCode::NOT_FOUND, NO_TASK))?;
    match expected {
        Some(version) if task_version(task) != version => {
            Err((StatusCode::PRECONDITION_FAILED, TASK_CHANGED))
        }
        _ => Ok(task),
    }
}

/// A new task.
#[derive(Debug, Deserialize)]
struct NewTask {
    title: String,
    #[serde(default)]
    text: String,
    kind: TaskKind,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    records: Vec<String>,
}

/// Every field of a task but its id.
#[derive(Debug, Deserialize)]
struct TaskChange {
    title: String,
    #[serde(default)]
    text: String,
    kind: TaskKind,
    state: TaskState,
    #[serde(default)]
    taken_by: Option<String>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    records: Vec<String>,
}

/// The issue to import.
#[derive(Debug, Deserialize)]
struct ImportRequest {
    url: String,
}

/// Why a list edit was refused: the status and the text.
type Refusal = (StatusCode, &'static str);

fn failed(error: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, error).into_response()
}

fn view(list: TodoList) -> ListView {
    let editors = list
        .task
        .iter()
        .flat_map(|task| &task.records)
        .map(|record| (record.clone(), editor_url(record)))
        .collect();
    let tasks = list
        .task
        .into_iter()
        .map(|task| TaskView {
            version: task_version(&task),
            task,
        })
        .collect();
    ListView { tasks, editors }
}

/// How a list edit ended.
enum ListEdit {
    Saved(TodoList),
    Refused(Refusal),
    Failed(String),
}

/// Runs `edit` on the list under the studio's to-do lock and saves it, off the async threads;
/// the answer of `edit` when it refuses, else the new list.
async fn edit_list(
    studio: &Studio,
    edit: impl FnOnce(&mut TodoList) -> Result<(), Refusal> + Send + 'static,
) -> Response {
    let _held = studio.todo_lock.lock().await;
    let design_dir = studio.paths.design_dir.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut list = match load(&design_dir) {
            Ok(list) => list,
            Err(error) => return ListEdit::Failed(error),
        };
        if let Err(refused) = edit(&mut list) {
            return ListEdit::Refused(refused);
        }
        match save(&design_dir, &list) {
            Ok(()) => ListEdit::Saved(list),
            Err(error) => ListEdit::Failed(error),
        }
    })
    .await;
    match outcome {
        Ok(ListEdit::Saved(list)) => Json(view(list)).into_response(),
        Ok(ListEdit::Refused(refused)) => refused.into_response(),
        Ok(ListEdit::Failed(error)) => failed(error),
        Err(error) => failed(error.to_string()),
    }
}

/// The issue link of a task: none, or a GitHub issue address (the page makes it a link, so
/// any other scheme, such as `javascript:`, is refused).
fn issue_of(issue: Option<String>) -> Result<Option<String>, Refusal> {
    match issue.filter(|issue| !issue.trim().is_empty()) {
        Some(issue) if parse_issue_url(&issue).is_none() => {
            Err((StatusCode::BAD_REQUEST, BAD_ISSUE_URL))
        }
        issue => Ok(issue.map(|issue| issue.trim().to_owned())),
    }
}

/// A new open task of `list`.
fn push_task(list: &mut TodoList, new: NewTask) -> Result<(), Refusal> {
    let title = new.title.trim();
    if title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, NO_TITLE));
    }
    let issue = issue_of(new.issue)?;
    let id = list.next_id();
    list.task.push(Task {
        id,
        title: title.to_owned(),
        text: new.text,
        kind: new.kind,
        state: TaskState::Open,
        taken_by: None,
        issue,
        records: new.records,
    });
    Ok(())
}

async fn list(State(state): State<AppState>) -> Response {
    let Some(studio) = state.studio.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let design_dir = studio.paths.design_dir.clone();
    match tokio::task::spawn_blocking(move || load(&design_dir)).await {
        Ok(Ok(list)) => Json(view(list)).into_response(),
        Ok(Err(error)) => failed(error),
        Err(error) => failed(error.to_string()),
    }
}

async fn add(State(state): State<AppState>, Json(new): Json<NewTask>) -> Response {
    match state.studio.as_ref() {
        Some(studio) => edit_list(studio, move |list| push_task(list, new)).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn change(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    Json(change): Json<TaskChange>,
) -> Response {
    let Some(studio) = state.studio.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let expected = expected_version(&headers);
    edit_list(studio, move |list| {
        let title = change.title.trim();
        if title.is_empty() {
            return Err((StatusCode::BAD_REQUEST, NO_TITLE));
        }
        let issue = issue_of(change.issue)?;
        let task = task_to_edit(list, &id, expected.as_deref())?;
        *task = Task {
            id: task.id.clone(),
            title: title.to_owned(),
            text: change.text,
            kind: change.kind,
            state: change.state,
            taken_by: change.taken_by.filter(|name| !name.trim().is_empty()),
            issue,
            records: change.records,
        };
        Ok(())
    })
    .await
}

async fn remove(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(studio) = state.studio.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let expected = expected_version(&headers);
    edit_list(studio, move |list| {
        task_to_edit(list, &id, expected.as_deref())?;
        list.task.retain(|task| task.id != id);
        Ok(())
    })
    .await
}

async fn import(State(state): State<AppState>, Json(request): Json<ImportRequest>) -> Response {
    let Some(studio) = state.studio.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(issue) = parse_issue_url(&request.url) else {
        return (StatusCode::BAD_REQUEST, BAD_ISSUE_URL).into_response();
    };
    let read = match studio.github.issue(&issue).await {
        Ok(read) => read,
        Err(error) => return (StatusCode::BAD_GATEWAY, error).into_response(),
    };
    let new = NewTask {
        title: read.title,
        text: read.body.unwrap_or_default(),
        kind: TaskKind::Other,
        issue: Some(read.html_url),
        records: Vec::new(),
    };
    edit_list(studio, move |list| push_task(list, new)).await
}

/// The output lines of `git <args>` run in `dir`.
async fn git_lines(dir: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("{GIT}: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The design files (relative to the design folder) that are new, changed, removed or staged
/// since the last commit; the to-do list is left out.
async fn changed_files(design_dir: &Path) -> Result<Vec<String>, String> {
    let mut files = git_lines(
        design_dir,
        &[
            "ls-files",
            "--modified",
            "--others",
            "--deleted",
            "--exclude-standard",
        ],
    )
    .await?;
    files.extend(
        git_lines(
            design_dir,
            &["diff", "--cached", "--name-only", "--relative"],
        )
        .await?,
    );
    files.retain(|file| file != TODO_FILE);
    files.sort();
    files.dedup();
    Ok(files)
}

async fn changed(State(state): State<AppState>) -> Response {
    let Some(studio) = state.studio.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match changed_files(&studio.paths.design_dir).await {
        Ok(files) => Json(files).into_response(),
        Err(error) => failed(error),
    }
}

#[cfg(test)]
mod tests {
    use crate::guard::AdminOrigin;
    use axum::body::Body;
    use axum::http::{Method, Request, header};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::captures::Captures;
    use crate::config::{GITHUB_API_URL, StudioConfig};
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE};
    use crate::server::router;
    use crate::world::tests::test_layer;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const BUILD: &str = "22469132";

    struct Fixture {
        root: tempfile::TempDir,
        studio: Studio,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("dir");
            let design_dir = root.path().join("content");
            std::fs::create_dir_all(&design_dir).expect("dir");
            let config = StudioConfig {
                design_dir,
                retail_dir: root.path().join("retail"),
                control_fifo: root.path().join("control"),
                restart_timeout_s: 1,
                github_api_url: GITHUB_API_URL.to_owned(),
                github_token_env: None,
            };
            let studio =
                Studio::new(&config, root.path().join("out"), BUILD, None).expect("studio");
            Self { root, studio }
        }

        fn content(&self) -> std::path::PathBuf {
            self.root.path().join("content")
        }

        async fn call(
            &self,
            method: Method,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.send(method, uri, body, None).await
        }

        /// `call` with `If-Match: version`.
        async fn call_with(
            &self,
            method: Method,
            uri: &str,
            body: Option<Value>,
            version: &str,
        ) -> (StatusCode, Value) {
            self.send(method, uri, body, Some(version)).await
        }

        async fn send(
            &self,
            method: Method,
            uri: &str,
            body: Option<Value>,
            version: Option<&str>,
        ) -> (StatusCode, Value) {
            let app = router(
                AppState::new(
                    test_layer(CAP),
                    std::env::temp_dir(),
                    AdminOrigin::of(LISTEN.parse().expect("address")),
                    Captures::default(),
                    None,
                )
                .with_studio(self.studio.clone()),
            );
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, LISTEN)
                .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(version) = version {
                request = request.header(header::IF_MATCH, version);
            }
            let request = request
                .body(Body::from(
                    body.map(|body| body.to_string()).unwrap_or_default(),
                ))
                .expect("request");
            let response = app.oneshot(request).await.expect("response");
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
    }

    #[tokio::test]
    async fn tasks_are_added_taken_done_and_removed() {
        let fixture = Fixture::new();
        let (status, empty) = fixture.call(Method::GET, TODO_ROUTE, None).await;
        assert_eq!(
            (status, empty["tasks"].as_array().map(Vec::len)),
            (StatusCode::OK, Some(0))
        );
        let new = json!({"title": "Wolves drop nothing", "kind": "loot", "records": ["loot/tables/LT_Wolf.toml"]});
        let (status, added) = fixture.call(Method::POST, TODO_ROUTE, Some(new)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(added["tasks"][0]["id"], "t1");
        assert_eq!(added["tasks"][0]["state"], "open");
        assert_eq!(
            added["editors"]["loot/tables/LT_Wolf.toml"],
            "/records?kind=loot_table&id=LT_Wolf"
        );
        let taken = json!({"title": "Wolves drop nothing", "kind": "loot", "state": "taken", "taken_by": "Ada"});
        let (status, changed) = fixture
            .call(Method::PUT, "/api/todo/task/t1", Some(taken))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed["tasks"][0]["taken_by"], "Ada");
        let text = std::fs::read_to_string(fixture.content().join(TODO_FILE)).expect("read");
        assert!(text.contains("state = \"taken\""), "{text}");
        let (status, _) = fixture
            .call(Method::DELETE, "/api/todo/task/t9", None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, removed) = fixture
            .call(Method::DELETE, "/api/todo/task/t1", None)
            .await;
        assert_eq!(
            (status, removed["tasks"].as_array().map(Vec::len)),
            (StatusCode::OK, Some(0))
        );
    }

    #[tokio::test]
    async fn a_change_of_a_task_that_changed_meanwhile_is_refused() {
        let fixture = Fixture::new();
        let new = json!({"title": "Boar loot", "kind": "loot"});
        let (_, list) = fixture.call(Method::POST, TODO_ROUTE, Some(new)).await;
        let opened = list["tasks"][0]["version"]
            .as_str()
            .expect("version")
            .to_owned();
        let take = |name: &str| json!({"title": "Boar loot", "kind": "loot", "state": "taken", "taken_by": name});
        let first = fixture
            .call_with(Method::PUT, "/api/todo/task/t1", Some(take("Ada")), &opened)
            .await;
        assert_eq!(first.0, StatusCode::OK);
        assert_ne!(first.1["tasks"][0]["version"], opened.as_str());
        let second = fixture
            .call_with(Method::PUT, "/api/todo/task/t1", Some(take("Bob")), &opened)
            .await;
        assert_eq!(second.0, StatusCode::PRECONDITION_FAILED);
        let removed = fixture
            .call_with(Method::DELETE, "/api/todo/task/t1", None, &opened)
            .await;
        assert_eq!(removed.0, StatusCode::PRECONDITION_FAILED);
        let (_, list) = fixture.call(Method::GET, TODO_ROUTE, None).await;
        assert_eq!(list["tasks"][0]["taken_by"], "Ada");
    }

    #[tokio::test]
    async fn a_task_needs_a_title_and_an_import_a_github_issue() {
        let fixture = Fixture::new();
        let (status, _) = fixture
            .call(
                Method::POST,
                TODO_ROUTE,
                Some(json!({"title": "  ", "kind": "other"})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = fixture
            .call(
                Method::POST,
                IMPORT_ROUTE,
                Some(json!({"url": "https://example.com/a/b/issues/1"})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let script_link = json!({"title": "x", "kind": "other", "issue": "javascript:alert(1)"});
        let (status, _) = fixture
            .call(Method::POST, TODO_ROUTE, Some(script_link))
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "only a GitHub issue is a link"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new(GIT)
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn changed_design_files_come_from_git() {
        let fixture = Fixture::new();
        let root = fixture.root.path();
        git(root, &["init", "-q"]);
        std::fs::create_dir_all(fixture.content().join("ai")).expect("dir");
        std::fs::write(fixture.content().join("ai/kept.toml"), "a").expect("write");
        git(root, &["add", "."]);
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        );
        std::fs::write(fixture.content().join("ai/kept.toml"), "b").expect("write");
        std::fs::write(fixture.content().join("ai/new.toml"), "c").expect("write");
        std::fs::write(fixture.content().join(TODO_FILE), "").expect("write");
        let (status, files) = fixture.call(Method::GET, CHANGED_ROUTE, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(files, json!(["ai/kept.toml", "ai/new.toml"]));
    }
}
