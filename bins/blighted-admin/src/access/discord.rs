// SPDX-License-Identifier: AGPL-3.0-or-later
//! Discord login of a server-mode admin site, for accounts made by a Discord login in the game.
//! It uses the game login's Discord app; the site's redirect, `https://<first host>` +
//! [`DISCORD_CALLBACK_ROUTE`], must be a redirect of that app. A Discord user without a game
//! account, a banned account and an account without an admin rank are refused.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use nw_common::time::unix_ms;
use nw_login::discord::{DISCORD_SCOPE, DiscordApi, DiscordHttp, authorize_url};
use nw_login::keys::{new_random_token, token_hash};
use serde::{Deserialize, Serialize};

use crate::access::api::{LOGIN_PAGE_ROUTE, MS_PER_SECOND, server_error, session_cookie};
use crate::access::session::{Actor, SESSION_TTL_MS};
use crate::server::AppState;

/// `GET`: sends the browser to the Discord login page.
pub const DISCORD_LOGIN_ROUTE: &str = "/login/discord";
/// `GET`: Discord sends the browser back here.
pub const DISCORD_CALLBACK_ROUTE: &str = "/login/discord/callback";
/// `GET`: the login methods the login page shows.
pub const LOGIN_METHODS_ROUTE: &str = "/api/auth/methods";
/// The audit detail of a Discord login.
const VIA_DISCORD: &str = "discord";
const HTTPS_SCHEME: &str = "https://";

/// The Discord API of the site: Discord itself, or a stand-in in the tests.
#[derive(Debug, Clone)]
pub enum DiscordClient {
    /// Discord's HTTP API.
    Http(DiscordHttp),
    /// The tests: the code [`tests::GOOD_CODE`] belongs to [`tests::DISCORD_USER_ID`]
    /// ([`tests::DISCORD_USERNAME`]).
    #[cfg(test)]
    Fake,
}

/// Discord login of the site.
#[derive(Debug, Clone)]
pub struct AdminDiscord {
    client: DiscordClient,
    client_id: String,
    redirect_uri: String,
}

impl AdminDiscord {
    /// Discord login with the app `client_id`; Discord sends the browser back to `host`.
    pub fn new(client: DiscordClient, client_id: String, host: &str) -> Self {
        Self {
            client,
            client_id,
            redirect_uri: format!("{HTTPS_SCHEME}{host}{DISCORD_CALLBACK_ROUTE}"),
        }
    }

    /// The Discord user id and username of a login `code`; `None` when Discord refuses it.
    async fn discord_user(&self, code: &str, verifier: &str) -> Option<(String, String)> {
        match &self.client {
            DiscordClient::Http(http) => {
                match http
                    .user_for_code(code, verifier, &self.redirect_uri, None)
                    .await
                {
                    Ok(user) => Some((user.profile.user_id, user.profile.username)),
                    Err(error) => {
                        tracing::warn!(%error, "Discord login failed");
                        None
                    }
                }
            }
            #[cfg(test)]
            DiscordClient::Fake => tests::fake_user(code),
        }
    }
}

/// The Discord login routes.
pub fn discord_routes() -> Router<AppState> {
    Router::new()
        .route(DISCORD_LOGIN_ROUTE, get(discord_login))
        .route(DISCORD_CALLBACK_ROUTE, get(discord_callback))
        .route(LOGIN_METHODS_ROUTE, get(login_methods))
}

/// What the login page offers.
#[derive(Debug, Serialize)]
struct LoginMethods {
    discord: bool,
}

/// Query of the Discord redirect.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn login_methods(State(state): State<AppState>) -> Json<LoginMethods> {
    Json(LoginMethods {
        discord: state.access.discord().is_some(),
    })
}

async fn discord_login(State(state): State<AppState>) -> Response {
    let Some(discord) = state.access.discord() else {
        return (StatusCode::NOT_FOUND, "Discord login is off").into_response();
    };
    let (Ok(login_state), Ok(verifier)) = (new_random_token(), new_random_token()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(url) = authorize_url(
        &discord.client_id,
        &discord.redirect_uri,
        &login_state,
        &verifier,
        DISCORD_SCOPE,
    ) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if !state
        .access
        .sessions()
        .begin_discord(login_state, verifier, unix_ms())
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many logins wait, try again soon",
        )
            .into_response();
    }
    Redirect::to(&url).into_response()
}

async fn discord_callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let access = &state.access;
    let (Some(discord), Some(store)) = (access.discord(), access.store()) else {
        return (StatusCode::NOT_FOUND, "Discord login is off").into_response();
    };
    let now_ms = unix_ms();
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return refused(StatusCode::BAD_REQUEST, "Discord sent no login.");
    };
    let Some(verifier) = access.sessions().take_discord(&login_state, now_ms) else {
        return refused(StatusCode::NOT_FOUND, "This login expired. Start again.");
    };
    let Some((discord_user_id, discord_username)) = discord.discord_user(&code, &verifier).await
    else {
        return refused(
            StatusCode::BAD_GATEWAY,
            "Discord login failed. Start again.",
        );
    };
    let record = match store.discord_account(&discord_user_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return refused(
                StatusCode::FORBIDDEN,
                "This Discord user has no game account. Log in to the game with Discord first.",
            );
        }
        Err(error) => return server_error(&error),
    };
    if record.banned {
        return refused(StatusCode::FORBIDDEN, "The account is banned.");
    }
    let rank = match store.rank(record.account).await {
        Ok(Some(rank)) => rank,
        Ok(None) => return refused(StatusCode::FORBIDDEN, "The account has no admin rank."),
        Err(error) => return server_error(&error),
    };
    let username = discord_username.to_lowercase();
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
        .record(
            &actor,
            "login",
            &username,
            serde_json::json!({ "via": VIA_DISCORD }),
            "done",
        )
        .await;
    // The session cookie is SameSite=Strict, so a browser does not send it on a redirect that
    // started on discord.com. This page opens the site itself, which then sends it.
    let mut response = Html(page("Logged in. Opening the admin site.", "/")).into_response();
    if let Ok(cookie) =
        HeaderValue::from_str(&session_cookie(&token, SESSION_TTL_MS / MS_PER_SECOND))
    {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

/// A refused Discord login: the reason, and a link back to the login page.
fn refused(status: StatusCode, reason: &str) -> Response {
    (status, Html(page(reason, LOGIN_PAGE_ROUTE))).into_response()
}

/// A short page with `message` that opens `target`; a refusal page only links to it.
fn page(message: &str, target: &str) -> String {
    let refresh = if target == LOGIN_PAGE_ROUTE {
        String::new()
    } else {
        format!(r#"<meta http-equiv="refresh" content="0;url={target}">"#)
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">{refresh}<title>Log in · Blighted Emu</title><link rel="stylesheet" href="/web/style.css"></head><body class="login-page"><main class="work"><p>{message}</p><p><a href="{target}">Continue</a></p></main></body></html>"#
    )
}

#[cfg(test)]
pub mod tests {
    //! The Discord stand-in and the page tests.

    use super::*;

    /// The code the stand-in accepts.
    pub const GOOD_CODE: &str = "good";
    /// The Discord user of [`GOOD_CODE`].
    pub const DISCORD_USER_ID: &str = "80351110224678912";
    /// The Discord username of [`DISCORD_USER_ID`].
    pub const DISCORD_USERNAME: &str = "Nelly";

    /// The Discord user of `code` at the stand-in.
    pub fn fake_user(code: &str) -> Option<(String, String)> {
        (code == GOOD_CODE).then(|| (DISCORD_USER_ID.to_owned(), DISCORD_USERNAME.to_owned()))
    }

    #[test]
    fn the_redirect_is_the_callback_on_the_first_host() {
        let discord = AdminDiscord::new(DiscordClient::Fake, "1".to_owned(), "admin.example.test");
        assert_eq!(
            discord.redirect_uri,
            "https://admin.example.test/login/discord/callback"
        );
    }

    #[test]
    fn a_refusal_page_links_back_and_opens_nothing() {
        let refusal = page("No.", LOGIN_PAGE_ROUTE);
        assert!(!refusal.contains("http-equiv"));
        assert!(refusal.contains(r#"href="/login""#));
        assert!(page("Yes.", "/").contains(r#"content="0;url=/""#));
    }
}
