// SPDX-License-Identifier: AGPL-3.0-or-later
//! The login routes and the access guard (admin studio spec section 9.1).

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use nw_common::time::unix_ms;
use nw_login::keys::{new_random_token, token_hash};
use serde::{Deserialize, Serialize};

use crate::access::discord::{
    DISCORD_CALLBACK_ROUTE, DISCORD_LOGIN_ROUTE, LOGIN_METHODS_ROUTE, discord_routes,
};
use crate::access::session::{Actor, SESSION_TTL_MS};
use crate::access::{Access, AccessMode, SESSION_COOKIE, session_token};
use crate::server::{
    AppState, CAPTURES_PAGE_ROUTE, GM_PAGE_ROUTE, HOME_PAGE_ROUTE, LIVE_PAGE_ROUTE,
    LIVE3D_PAGE_ROUTE, MAP_PAGE_ROUTE, RECORDS_PAGE_ROUTE, SCRIPTS_PAGE_ROUTE, STUDIO_PAGE_ROUTE,
    TODO_PAGE_ROUTE,
};

/// The login page.
pub const LOGIN_PAGE_ROUTE: &str = "/login";
/// `POST`: log in.
pub const LOGIN_ROUTE: &str = "/api/auth/login";
/// `POST`: log out.
pub const LOGOUT_ROUTE: &str = "/api/auth/logout";
/// `GET`: who is logged in.
pub const ME_ROUTE: &str = "/api/auth/me";
/// Routes that need no session: the login page, its API and the Discord login.
const PUBLIC_ROUTES: [&str; 5] = [
    LOGIN_PAGE_ROUTE,
    LOGIN_ROUTE,
    LOGIN_METHODS_ROUTE,
    DISCORD_LOGIN_ROUTE,
    DISCORD_CALLBACK_ROUTE,
];
/// Prefix of the web files, which need no session.
const WEB_PREFIX: &str = "/web/";
/// Pages that send a visitor without a session to the login page (the map and captures pages
/// too, though they sit among the public web files).
const PAGES: [&str; 10] = [
    HOME_PAGE_ROUTE,
    LIVE_PAGE_ROUTE,
    LIVE3D_PAGE_ROUTE,
    STUDIO_PAGE_ROUTE,
    RECORDS_PAGE_ROUTE,
    SCRIPTS_PAGE_ROUTE,
    TODO_PAGE_ROUTE,
    GM_PAGE_ROUTE,
    MAP_PAGE_ROUTE,
    CAPTURES_PAGE_ROUTE,
];
/// Changing routes that only local mode allows: design work and bot batches are for a
/// team member's own PC (spec section 4).
const LOCAL_ONLY_PREFIXES: [&str; 3] = ["/api/studio/", "/api/live/bots", "/api/todo"];
pub(crate) const MS_PER_SECOND: u64 = 1000;
/// The answer to a name and password on a site without password login.
const PASSWORD_LOGIN_OFF: &str = "password login is off";

/// The login routes.
pub fn access_routes() -> Router<AppState> {
    Router::new()
        .route(LOGIN_ROUTE, post(login))
        .route(LOGOUT_ROUTE, post(logout))
        .route(ME_ROUTE, get(me))
        .merge(discord_routes())
}

/// What the login form sends.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

/// Who is logged in, and the mode.
#[derive(Debug, Serialize)]
struct MeView {
    mode: &'static str,
    actor: Actor,
}

/// Puts the request's [`Actor`] into its extensions. In server mode a request without a session
/// gets the login page (pages) or 401 (the rest), except [`PUBLIC_ROUTES`] and the web files;
/// the local-only routes get 403.
pub async fn access_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let access = &state.access;
    let path = request.uri().path().to_owned();
    let changing = request.method() != Method::GET && request.method() != Method::HEAD;
    if access.mode() == AccessMode::Server
        && changing
        && LOCAL_ONLY_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
    {
        return (StatusCode::FORBIDDEN, "local mode only").into_response();
    }
    match access.actor(request.headers()) {
        Some(actor) => {
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        None => {
            let public = PUBLIC_ROUTES.contains(&path.as_str()) || path.starts_with(WEB_PREFIX);
            if !changing && PAGES.contains(&path.as_str()) {
                Redirect::to(LOGIN_PAGE_ROUTE).into_response()
            } else if public {
                next.run(request).await
            } else {
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
    }
}

async fn login(State(state): State<AppState>, Json(form): Json<LoginRequest>) -> Response {
    let access = &state.access;
    let (Some(store), Some(hasher)) = (access.store(), access.hasher()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !access.password_login() {
        return (StatusCode::NOT_FOUND, PASSWORD_LOGIN_OFF).into_response();
    }
    let username = form.username.trim().to_lowercase();
    let now_ms = unix_ms();
    if access.sessions().locked(&username, now_ms) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "locked for a while: too many failed logins",
        )
            .into_response();
    }
    let record = match store.login(&username).await {
        Ok(record) => record,
        Err(error) => return server_error(&error),
    };
    let stored = record.as_ref().map(|record| record.password_hash.clone());
    let good = match hasher.verify(form.password, stored).await {
        Ok(good) => good,
        Err(error) => return server_error(&error),
    };
    let Some(record) = record.filter(|_| good) else {
        access.sessions().failed(&username, now_ms);
        return (StatusCode::UNAUTHORIZED, "wrong name or password").into_response();
    };
    access.sessions().succeeded(&username);
    if record.banned {
        return (StatusCode::FORBIDDEN, "the account is banned").into_response();
    }
    let rank = match store.rank(record.account).await {
        Ok(Some(rank)) => rank,
        Ok(None) => {
            return (StatusCode::FORBIDDEN, "the account has no admin rank").into_response();
        }
        Err(error) => return server_error(&error),
    };
    let Ok(token) = new_random_token() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let actor = Actor {
        account: Some(record.account),
        username: username.clone(),
        rank,
    };
    access
        .sessions()
        .open(token_hash(&token), actor.clone(), now_ms);
    access
        .record(&actor, "login", &username, serde_json::Value::Null, "done")
        .await;
    let mut response = Json(&actor).into_response();
    if let Ok(cookie) =
        HeaderValue::from_str(&session_cookie(&token, SESSION_TTL_MS / MS_PER_SECOND))
    {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        state.access.sessions().close(&token_hash(&token));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(cookie) = HeaderValue::from_str(&session_cookie("", 0)) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let mode = match state.access.mode() {
        AccessMode::Local => "local",
        AccessMode::Server => "server",
    };
    match state.access.actor(&headers) {
        Some(actor) => Json(MeView { mode, actor }).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// The `Set-Cookie` value of a session token that lives `max_age_s` seconds.
pub(crate) fn session_cookie(token: &str, max_age_s: u64) -> String {
    format!(
        "{SESSION_COOKIE}={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={max_age_s}"
    )
}

pub(crate) fn server_error(error: &dyn std::error::Error) -> Response {
    tracing::warn!(%error, "login failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// The access of an app without login: local mode, no store.
pub fn local_access() -> Access {
    Access::new(AccessMode::Local, None, None)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use nw_db::AdminRank;
    use nw_login::password::PasswordHasher;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::access::discord::tests::{DISCORD_USER_ID, DISCORD_USERNAME, GOOD_CODE};
    use crate::access::discord::{AdminDiscord, DiscordClient};
    use crate::access::session::LOCK_FAILS;
    use crate::access::store::memory::MemoryStore;
    use crate::access::store::{AccountRecord, LoginRecord, Store};
    use crate::captures::Captures;
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE, AdminOrigin};
    use crate::server::{AppState, router};
    use crate::world::tests::test_layer;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const PASSWORD: &str = "correct horse";
    const GM: &str = "gm";
    const NO_RANK: &str = "player";
    const ONE_HASH: usize = 1;
    /// The account of the Discord user of the tests.
    const DISCORD_ACCOUNT: Uuid = Uuid::from_u128(9);
    const ADMIN_HOST: &str = "admin.example.test";

    struct Fixture {
        memory: Arc<Mutex<MemoryStore>>,
        state: AppState,
    }

    async fn fixture() -> Fixture {
        fixture_with(None).await
    }

    /// The fixture with Discord login; the Discord user has a game account with `rank`.
    async fn discord_fixture(rank: Option<AdminRank>) -> Fixture {
        fixture_with(Some(rank)).await
    }

    async fn fixture_with(discord: Option<Option<AdminRank>>) -> Fixture {
        let hasher = PasswordHasher::new(ONE_HASH).expect("hasher");
        let hash = hasher.hash(PASSWORD.to_owned()).await.expect("hash");
        let mut memory = MemoryStore::default();
        for (index, name) in [GM, NO_RANK].into_iter().enumerate() {
            let account = Uuid::from_u128(u128::try_from(index).expect("index") + 1);
            memory.logins.insert(
                name.to_owned(),
                LoginRecord {
                    account,
                    banned: false,
                    password_hash: hash.clone(),
                },
            );
            if name == GM {
                memory.ranks.insert(account, AdminRank::GameMaster);
            }
        }
        if let Some(rank) = discord {
            memory.discord.insert(
                DISCORD_USER_ID.to_owned(),
                (
                    DISCORD_USERNAME.to_owned(),
                    AccountRecord {
                        account: DISCORD_ACCOUNT,
                        banned: false,
                    },
                ),
            );
            if let Some(rank) = rank {
                memory.ranks.insert(DISCORD_ACCOUNT, rank);
            }
        }
        let memory = Arc::new(Mutex::new(memory));
        let mut access = Access::new(
            AccessMode::Server,
            Some(Store::Memory(memory.clone())),
            Some(hasher),
        );
        if discord.is_some() {
            access = access.with_discord(AdminDiscord::new(
                DiscordClient::Fake,
                "1".to_owned(),
                ADMIN_HOST,
            ));
        }
        let listen = LISTEN.parse().expect("address");
        let state = AppState::new(
            test_layer(CAP),
            std::env::temp_dir(),
            AdminOrigin::of(listen),
            Captures::default(),
            None,
        )
        .with_access(access);
        Fixture { memory, state }
    }

    fn request(method: Method, uri: &str, cookie: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, LISTEN)
            .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::from(body.to_owned())).expect("request")
    }

    async fn send(fixture: &Fixture, request: Request<Body>) -> Response {
        router(fixture.state.clone())
            .oneshot(request)
            .await
            .expect("response")
    }

    fn form(username: &str, password: &str) -> String {
        serde_json::json!({ "username": username, "password": password }).to_string()
    }

    /// Logs in `username` and gives the `name=value` part of its session cookie.
    async fn log_in(fixture: &Fixture, username: &str) -> (StatusCode, Option<String>) {
        let response = send(
            fixture,
            request(Method::POST, LOGIN_ROUTE, None, &form(username, PASSWORD)),
        )
        .await;
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        (response.status(), cookie)
    }

    #[tokio::test]
    async fn a_good_login_gives_a_strict_session_cookie_and_an_audit_row() {
        let fixture = fixture().await;
        let (status, cookie) = log_in(&fixture, "GM").await;
        assert_eq!(status, StatusCode::OK);
        let cookie = cookie.expect("cookie");
        for flag in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(cookie.contains(flag), "{flag} in {cookie}");
        }
        let pair = cookie.split(';').next().expect("pair").to_owned();
        let response = send(&fixture, request(Method::GET, ME_ROUTE, Some(&pair), "")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let me: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(me["actor"]["rank"], "game_master");
        let audit = &crate::access::store::memory::lock(&fixture.memory).audit;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].command, "login");
    }

    #[tokio::test]
    async fn a_wrong_password_and_no_rank_are_refused() {
        let fixture = fixture().await;
        let wrong = send(
            &fixture,
            request(Method::POST, LOGIN_ROUTE, None, &form(GM, "nope")),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let unknown = send(
            &fixture,
            request(Method::POST, LOGIN_ROUTE, None, &form("ghost", PASSWORD)),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        let (no_rank, cookie) = log_in(&fixture, NO_RANK).await;
        assert_eq!(no_rank, StatusCode::FORBIDDEN);
        assert!(cookie.is_none());
    }

    #[tokio::test]
    async fn five_wrong_passwords_lock_even_the_right_one() {
        let fixture = fixture().await;
        for _ in 0..LOCK_FAILS {
            send(
                &fixture,
                request(Method::POST, LOGIN_ROUTE, None, &form(GM, "nope")),
            )
            .await;
        }
        let (status, cookie) = log_in(&fixture, GM).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(cookie.is_none());
    }

    #[tokio::test]
    async fn without_a_session_pages_go_to_login_and_data_is_refused() {
        let fixture = fixture().await;
        for path in PAGES {
            let page = send(&fixture, request(Method::GET, path, None, "")).await;
            assert_eq!(page.status(), StatusCode::SEE_OTHER, "{path}");
            assert_eq!(page.headers()[header::LOCATION], LOGIN_PAGE_ROUTE, "{path}");
        }
        let data = send(&fixture, request(Method::GET, "/api/world/info", None, "")).await;
        assert_eq!(data.status(), StatusCode::UNAUTHORIZED);
        let login_page = send(&fixture, request(Method::GET, LOGIN_PAGE_ROUTE, None, "")).await;
        assert_eq!(login_page.status(), StatusCode::OK);
        let script = send(&fixture, request(Method::GET, "/web/login.js", None, "")).await;
        assert_eq!(script.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn design_and_bot_routes_are_local_only() {
        let fixture = fixture().await;
        let (_, cookie) = log_in(&fixture, GM).await;
        let pair = cookie
            .expect("cookie")
            .split(';')
            .next()
            .expect("pair")
            .to_owned();
        for uri in ["/api/studio/apply", "/api/live/bots"] {
            let response = send(&fixture, request(Method::POST, uri, Some(&pair), "{}")).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        }
        let logout = send(
            &fixture,
            request(Method::POST, LOGOUT_ROUTE, Some(&pair), ""),
        )
        .await;
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        let after = send(&fixture, request(Method::GET, ME_ROUTE, Some(&pair), "")).await;
        assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
    }

    async fn body_text(response: Response) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("text")
    }

    /// Starts a Discord login and gives the state of its Discord page link.
    async fn discord_state(fixture: &Fixture) -> String {
        let start = send(fixture, request(Method::GET, DISCORD_LOGIN_ROUTE, None, "")).await;
        assert_eq!(start.status(), StatusCode::SEE_OTHER);
        let location = start.headers()[header::LOCATION]
            .to_str()
            .expect("location")
            .to_owned();
        assert!(location.starts_with("https://discord.com/oauth2/authorize?"));
        assert!(location.contains(
            "redirect_uri=https%3A%2F%2Fadmin.example.test%2Flogin%2Fdiscord%2Fcallback"
        ));
        location
            .split('&')
            .find_map(|pair| pair.strip_prefix("state="))
            .expect("state")
            .to_owned()
    }

    fn callback(code: &str, state: &str) -> String {
        format!("{DISCORD_CALLBACK_ROUTE}?code={code}&state={state}")
    }

    #[tokio::test]
    async fn the_login_page_offers_discord_only_when_it_is_on() {
        for (fixture, on) in [
            (fixture().await, false),
            (discord_fixture(None).await, true),
        ] {
            let methods = send(
                &fixture,
                request(Method::GET, LOGIN_METHODS_ROUTE, None, ""),
            )
            .await;
            assert_eq!(methods.status(), StatusCode::OK);
            let methods: serde_json::Value =
                serde_json::from_str(&body_text(methods).await).expect("json");
            assert_eq!(methods["discord"], on);
            assert_eq!(methods["password"], !on);
        }
        let off = send(
            &fixture().await,
            request(Method::GET, DISCORD_LOGIN_ROUTE, None, ""),
        )
        .await;
        assert_eq!(off.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_site_with_discord_login_refuses_a_name_and_password() {
        let fixture = discord_fixture(Some(AdminRank::HeadAdmin)).await;
        let response = send(
            &fixture,
            request(Method::POST, LOGIN_ROUTE, None, &form(GM, PASSWORD)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_text(response).await, PASSWORD_LOGIN_OFF);
    }

    #[tokio::test]
    async fn a_discord_login_of_a_ranked_account_opens_a_session_once() {
        let fixture = discord_fixture(Some(AdminRank::HeadAdmin)).await;
        let state = discord_state(&fixture).await;
        let done = send(
            &fixture,
            request(Method::GET, &callback(GOOD_CODE, &state), None, ""),
        )
        .await;
        assert_eq!(done.status(), StatusCode::OK);
        let cookie = done.headers()[header::SET_COOKIE]
            .to_str()
            .expect("cookie")
            .to_owned();
        assert!(cookie.contains("SameSite=Strict"));
        assert!(body_text(done).await.contains(r#"content="0;url=/""#));
        let pair = cookie.split(';').next().expect("pair").to_owned();
        let me = send(&fixture, request(Method::GET, ME_ROUTE, Some(&pair), "")).await;
        let me: serde_json::Value = serde_json::from_str(&body_text(me).await).expect("json");
        assert_eq!(me["actor"]["rank"], "head_admin");
        assert_eq!(me["actor"]["username"], "nelly");
        let via = crate::access::store::memory::lock(&fixture.memory)
            .audit
            .last()
            .expect("audit")
            .detail["via"]
            .clone();
        assert_eq!(via, "discord");
        let again = send(
            &fixture,
            request(Method::GET, &callback(GOOD_CODE, &state), None, ""),
        )
        .await;
        assert_eq!(
            again.status(),
            StatusCode::NOT_FOUND,
            "a state is used once"
        );
    }

    #[tokio::test]
    async fn a_discord_login_without_a_rank_an_account_or_a_good_code_is_refused() {
        let no_rank = discord_fixture(None).await;
        let state = discord_state(&no_rank).await;
        let refused = send(
            &no_rank,
            request(Method::GET, &callback(GOOD_CODE, &state), None, ""),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert!(!refused.headers().contains_key(header::SET_COOKIE));

        let no_account = discord_fixture(Some(AdminRank::GameMaster)).await;
        crate::access::store::memory::lock(&no_account.memory)
            .discord
            .clear();
        let state = discord_state(&no_account).await;
        let refused = send(
            &no_account,
            request(Method::GET, &callback(GOOD_CODE, &state), None, ""),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert!(body_text(refused).await.contains("no game account"));

        let bad_code = discord_fixture(Some(AdminRank::GameMaster)).await;
        let state = discord_state(&bad_code).await;
        let refused = send(
            &bad_code,
            request(Method::GET, &callback("bad", &state), None, ""),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::BAD_GATEWAY);
        let unknown = send(
            &bad_code,
            request(Method::GET, &callback(GOOD_CODE, "unknown"), None, ""),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn staff_give_a_rank_and_ban_by_discord_username_or_id() {
        let fixture = discord_fixture(None).await;
        let store = fixture.state.access.store().expect("store").clone();
        assert!(
            store
                .set_rank("NELLY", Some(AdminRank::Csr), None)
                .await
                .expect("rank")
        );
        assert_eq!(
            store.rank(DISCORD_ACCOUNT).await.expect("rank"),
            Some(AdminRank::Csr)
        );
        let rows = store.ranks().await.expect("ranks");
        assert!(
            rows.iter()
                .any(|row| row.username.as_deref() == Some(DISCORD_USERNAME))
        );
        assert!(
            store
                .set_banned(DISCORD_USER_ID, true)
                .await
                .expect("ban")
                .is_some()
        );
        assert_eq!(
            store
                .discord_account(DISCORD_USER_ID)
                .await
                .expect("account")
                .map(|record| record.banned),
            Some(true)
        );
        assert!(
            store
                .set_banned("ghost", true)
                .await
                .expect("unknown")
                .is_none()
        );
    }
}
