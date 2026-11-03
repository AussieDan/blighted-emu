// SPDX-License-Identifier: AGPL-3.0-or-later
//! The launcher API on `login.<domain>` (M2 spec sections 5 and 6).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use nw_db::Account;
use nw_protocol::aws_mirror::AwsMirror;
use nw_protocol::launcher_api::{
    AuthMode, DISCORD_POLL_PATH, DISCORD_START_PATH, DiscordPoll, DiscordPollReply, DiscordStart,
    INFO_PATH, LOGIN_PATH, LOGOUT_PATH, LauncherInfo, LauncherSession, PLAY_PATH,
    PasswordCredentials, PlayReply, REGISTER_PATH,
};
use serde::Deserialize;

use crate::config::{AuthConfig, EdgeConfig};
use crate::discord::{DISCORD_CALLBACK_PATH, DiscordApi, authorize_url, login_scope};
use crate::error::{ApiError, parse_json};
use crate::keys::{is_launcher_token, new_game_session_key, new_random_token, token_hash};
use crate::names::{check_password, normalize_username};
use crate::password::PasswordHasher;
use crate::store::{
    AccountStore, DiscordAttempt, DiscordStateStore, GameSessionStore, RateLimiter,
};
use nw_common::time::unix_seconds;

/// Lifetime of a launcher session: 30 days.
pub const LAUNCHER_SESSION_SECONDS: i64 = 30 * 24 * 60 * 60;
/// Time to use a new game session key.
pub const GAME_SESSION_CLAIM_SECONDS: u64 = 600;
/// Lifetime of a Discord login attempt.
pub const DISCORD_LOGIN_SECONDS: u64 = 300;

const BEARER_PREFIX: &str = "Bearer ";
const LOGIN_IP_LIMIT_PREFIX: &str = "login-ip:";
const LOGIN_USER_LIMIT_PREFIX: &str = "login-user:";
const REGISTER_IP_LIMIT_PREFIX: &str = "register-ip:";
const DISCORD_START_IP_LIMIT_PREFIX: &str = "discord-start-ip:";
const CALLBACK_DONE: &str = "Login finished. Return to the launcher.";
const CALLBACK_EXPIRED: &str = "This login expired. Start again in the launcher.";
const CALLBACK_FAILED: &str = "Discord login failed. Start again in the launcher.";
const CALLBACK_BANNED: &str = "This account is banned.";
const CALLBACK_NOT_MEMBER: &str =
    "Join this server's Discord first, then start again in the launcher.";

/// Everything the launcher routes need.
#[derive(Clone)]
pub struct LauncherState<A, G, R, D, X> {
    /// Server configuration.
    pub config: Arc<EdgeConfig>,
    /// Accounts and launcher sessions.
    pub accounts: A,
    /// Game session keys.
    pub game_sessions: G,
    /// Rate-limit counters.
    pub limits: R,
    /// Discord login attempts.
    pub discord_states: D,
    /// Discord API; `None` in password mode.
    pub discord: Option<X>,
    /// Argon2id worker pool.
    pub hasher: PasswordHasher,
    /// The certificate this server shows for the `[aws_mirror]` hosts, as PEM.
    pub aws_mirror_certificate_pem: Arc<str>,
}

/// The launcher routes.
pub fn launcher_router<A, G, R, D, X>(state: LauncherState<A, G, R, D, X>) -> Router
where
    A: AccountStore,
    G: GameSessionStore,
    R: RateLimiter,
    D: DiscordStateStore,
    X: DiscordApi,
{
    Router::new()
        .route(INFO_PATH, get(info::<A, G, R, D, X>))
        .route(REGISTER_PATH, post(register::<A, G, R, D, X>))
        .route(LOGIN_PATH, post(login::<A, G, R, D, X>))
        .route(PLAY_PATH, post(play::<A, G, R, D, X>))
        .route(LOGOUT_PATH, post(logout::<A, G, R, D, X>))
        .route(DISCORD_START_PATH, post(discord_start::<A, G, R, D, X>))
        .route(
            DISCORD_CALLBACK_PATH,
            get(discord_callback::<A, G, R, D, X>),
        )
        .route(DISCORD_POLL_PATH, post(discord_poll::<A, G, R, D, X>))
        .with_state(state)
}

fn require_mode<A, G, R, D, X>(
    state: &LauncherState<A, G, R, D, X>,
    mode: AuthMode,
) -> Result<(), ApiError> {
    if state.config.auth.mode == mode {
        Ok(())
    } else {
        Err(ApiError::WrongMode)
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(BEARER_PREFIX))
        .filter(|token| is_launcher_token(token))
        .ok_or(ApiError::NoSession)
}

/// IPv6 segments that name one network block (/64); one home or server usually has a whole block.
const IPV6_NETWORK_SEGMENTS: usize = 4;
/// Prefix length written after an IPv6 block in a limit key.
const IPV6_BLOCK_SUFFIX: &str = "/64";

/// The address a per-IP limit counts: the IPv4 address, or the /64 block of an IPv6 address, so a
/// client cannot get around the limit by changing the low bits of its IPv6 address.
fn limit_address(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let mut segments = v6.segments();
                segments
                    .iter_mut()
                    .skip(IPV6_NETWORK_SEGMENTS)
                    .for_each(|segment| *segment = 0);
                format!("{}{IPV6_BLOCK_SUFFIX}", Ipv6Addr::from(segments))
            }
        },
    }
}

/// Counts one attempt on `key` and refuses it when the count goes above the login limit.
async fn reserve_attempt<R: RateLimiter>(
    limits: &R,
    auth: &AuthConfig,
    key: &str,
) -> Result<(), ApiError> {
    let attempts = limits
        .increment(key, auth.login_fail_window_seconds)
        .await?;
    if attempts > u64::from(auth.login_fail_limit) {
        Err(ApiError::RateLimited)
    } else {
        Ok(())
    }
}

/// Starts a launcher session for an account that may log in.
async fn new_launcher_session<A: AccountStore>(
    accounts: &A,
    account: &Account,
) -> Result<LauncherSession, ApiError> {
    if account.banned {
        return Err(ApiError::Banned);
    }
    let token = new_random_token().map_err(|_| ApiError::Unavailable)?;
    let expires_at = unix_seconds() + LAUNCHER_SESSION_SECONDS;
    accounts
        .create_launcher_session(token_hash(&token), account.id, expires_at)
        .await?;
    Ok(LauncherSession {
        launcher_token: token,
        expires_at,
    })
}

async fn info<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
) -> Json<LauncherInfo> {
    let config = &state.config;
    Json(LauncherInfo {
        server_name: config.server_name.clone(),
        auth_mode: config.auth.mode,
        allow_registration: config.auth.allow_registration,
        launch_arguments: config.launch_arguments(),
        aws_mirror: AwsMirror {
            hosts: config.aws_mirror.hosts.clone(),
            certificate_pem: state.aws_mirror_certificate_pem.to_string(),
            ping_port: config.public_ping_port,
        },
    })
}

async fn register<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Result<Json<LauncherSession>, ApiError>
where
    A: AccountStore,
    R: RateLimiter,
{
    require_mode(&state, AuthMode::Password)?;
    if !state.config.auth.allow_registration {
        return Err(ApiError::RegistrationClosed);
    }
    reserve_attempt(
        &state.limits,
        &state.config.auth,
        &format!("{REGISTER_IP_LIMIT_PREFIX}{}", limit_address(peer.ip())),
    )
    .await?;
    let credentials: PasswordCredentials = parse_json(&body)?;
    let username = normalize_username(&credentials.username)?;
    check_password(&credentials.password)?;
    let hash = state
        .hasher
        .hash(credentials.password)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let account = state
        .accounts
        .create_password_account(&username, &hash)
        .await?;
    Ok(Json(new_launcher_session(&state.accounts, &account).await?))
}

async fn login<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Result<Json<LauncherSession>, ApiError>
where
    A: AccountStore,
    R: RateLimiter,
{
    require_mode(&state, AuthMode::Password)?;
    let credentials: PasswordCredentials = parse_json(&body)?;
    let username = normalize_username(&credentials.username).ok();
    let mut limit_keys = vec![format!(
        "{LOGIN_IP_LIMIT_PREFIX}{}",
        limit_address(peer.ip())
    )];
    limit_keys.extend(
        username
            .as_ref()
            .map(|name| format!("{LOGIN_USER_LIMIT_PREFIX}{name}")),
    );
    // Every attempt is counted before the hash, so parallel requests cannot pass the limit
    // (review finding 2). A correct password takes its attempt back.
    for key in &limit_keys {
        reserve_attempt(&state.limits, &state.config.auth, key).await?;
    }
    let stored = match &username {
        Some(name) => state.accounts.password_login(name).await?,
        None => None,
    };
    let (account, hash) = match stored {
        Some((account, hash)) => (Some(account), Some(hash)),
        None => (None, None),
    };
    let matches = state
        .hasher
        .verify(credentials.password, hash)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    match account {
        Some(account) if matches => {
            for key in &limit_keys {
                state.limits.release(key).await?;
            }
            Ok(Json(new_launcher_session(&state.accounts, &account).await?))
        }
        _ => Err(ApiError::InvalidCredentials),
    }
}

async fn session_account<A: AccountStore>(
    accounts: &A,
    headers: &HeaderMap,
) -> Result<Account, ApiError> {
    let token = bearer_token(headers)?;
    let account = accounts
        .launcher_session_account(token_hash(token))
        .await?
        .ok_or(ApiError::NoSession)?;
    if account.banned {
        return Err(ApiError::Banned);
    }
    Ok(account)
}

async fn play<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    headers: HeaderMap,
) -> Result<Json<PlayReply>, ApiError>
where
    A: AccountStore,
    G: GameSessionStore,
{
    let account = session_account(&state.accounts, &headers).await?;
    let key = new_game_session_key().map_err(|_| ApiError::Unavailable)?;
    state
        .game_sessions
        .create(&key, account.id, GAME_SESSION_CLAIM_SECONDS)
        .await?;
    Ok(Json(PlayReply {
        game_session_key: key,
    }))
}

async fn logout<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError>
where
    A: AccountStore,
{
    let token = bearer_token(&headers)?;
    state
        .accounts
        .delete_launcher_session(token_hash(token))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn callback_url(config: &EdgeConfig) -> String {
    format!(
        "https://{}{DISCORD_CALLBACK_PATH}",
        config.login_authority()
    )
}

async fn discord_start<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Json<DiscordStart>, ApiError>
where
    R: RateLimiter,
    D: DiscordStateStore,
{
    require_mode(&state, AuthMode::Discord)?;
    reserve_attempt(
        &state.limits,
        &state.config.auth,
        &format!(
            "{DISCORD_START_IP_LIMIT_PREFIX}{}",
            limit_address(peer.ip())
        ),
    )
    .await?;
    let login_state = new_random_token().map_err(|_| ApiError::Unavailable)?;
    let verifier = new_random_token().map_err(|_| ApiError::Unavailable)?;
    let url = authorize_url(
        &state.config.discord.client_id,
        &callback_url(&state.config),
        &login_state,
        &verifier,
        login_scope(state.config.discord.required_guild_id()),
    )
    .map_err(|_| ApiError::Unavailable)?;
    state
        .discord_states
        .begin(&login_state, &verifier, DISCORD_LOGIN_SECONDS)
        .await?;
    Ok(Json(DiscordStart {
        authorize_url: url,
        state: login_state,
    }))
}

/// Query of the Discord redirect.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn discord_callback<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse
where
    A: AccountStore,
    D: DiscordStateStore,
    X: DiscordApi,
{
    match finish_discord_login(&state, query).await {
        Ok(()) => (StatusCode::OK, CALLBACK_DONE),
        Err(ApiError::Banned) => (StatusCode::FORBIDDEN, CALLBACK_BANNED),
        Err(ApiError::NotGuildMember) => (StatusCode::FORBIDDEN, CALLBACK_NOT_MEMBER),
        Err(ApiError::UnknownLogin | ApiError::WrongMode) => {
            (StatusCode::NOT_FOUND, CALLBACK_EXPIRED)
        }
        Err(error) => (error.status(), CALLBACK_FAILED),
    }
}

async fn finish_discord_login<A, G, R, D, X>(
    state: &LauncherState<A, G, R, D, X>,
    query: CallbackQuery,
) -> Result<(), ApiError>
where
    A: AccountStore,
    D: DiscordStateStore,
    X: DiscordApi,
{
    require_mode(state, AuthMode::Discord)?;
    let discord = state.discord.as_ref().ok_or(ApiError::WrongMode)?;
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return Err(ApiError::Discord);
    };
    let verifier = state
        .discord_states
        .take_verifier(&login_state)
        .await?
        .ok_or(ApiError::UnknownLogin)?;
    let user = discord
        .user_for_code(
            &code,
            &verifier,
            &callback_url(&state.config),
            state.config.discord.required_guild_id(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "Discord login failed");
            ApiError::Discord
        })?;
    if user.guild_member == Some(false) {
        return Err(ApiError::NotGuildMember);
    }
    let account = state.accounts.discord_account(&user.profile).await?;
    let session = new_launcher_session(&state.accounts, &account).await?;
    state
        .discord_states
        .complete(&login_state, &session, DISCORD_LOGIN_SECONDS)
        .await?;
    Ok(())
}

async fn discord_poll<A, G, R, D, X>(
    State(state): State<LauncherState<A, G, R, D, X>>,
    body: Bytes,
) -> Result<Json<DiscordPollReply>, ApiError>
where
    D: DiscordStateStore,
{
    require_mode(&state, AuthMode::Discord)?;
    let poll: DiscordPoll = parse_json(&body)?;
    match state.discord_states.poll(&poll.state).await? {
        DiscordAttempt::Unknown => Err(ApiError::UnknownLogin),
        DiscordAttempt::Pending => Ok(Json(DiscordPollReply::Pending)),
        DiscordAttempt::Ready(session) => Ok(Json(DiscordPollReply::Ready(session))),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::HeaderValue;

    use super::*;
    use crate::config::tests::valid;
    use crate::discord::tests::{
        FAKE_EMAIL, FAKE_GUILD_ID, FAKE_USER_ID, FAKE_USERNAME, FakeDiscord, GOOD_CODE,
    };
    use crate::memory::{
        MemoryAccounts, MemoryDiscordStates, MemoryGameSessions, MemoryRateLimiter,
    };
    use crate::test_http::{self, error_code};
    use crate::test_support::TEST_AWS_MIRROR_CERTIFICATE_PEM;

    const PEER: ([u8; 4], u16) = ([192, 0, 2, 7], 50_000);
    const USERNAME: &str = "Ada";
    const PASSWORD: &str = "password-ok";
    const ONE_HASH_AT_A_TIME: usize = 1;
    /// Wrong-password logins sent at the same time.
    const PARALLEL_LOGINS: usize = 40;

    pub(crate) type TestState = LauncherState<
        MemoryAccounts,
        MemoryGameSessions,
        MemoryRateLimiter,
        MemoryDiscordStates,
        FakeDiscord,
    >;

    pub(crate) fn state(mode: AuthMode) -> TestState {
        let mut config = valid();
        config.auth.mode = mode;
        config.discord.client_id = "123".to_owned();
        LauncherState {
            config: Arc::new(config),
            accounts: MemoryAccounts::default(),
            game_sessions: MemoryGameSessions::default(),
            limits: MemoryRateLimiter::default(),
            discord_states: MemoryDiscordStates::default(),
            discord: (mode == AuthMode::Discord).then_some(FakeDiscord),
            hasher: PasswordHasher::new(ONE_HASH_AT_A_TIME).expect("hasher"),
            aws_mirror_certificate_pem: TEST_AWS_MIRROR_CERTIFICATE_PEM.into(),
        }
    }

    fn app(state: TestState) -> Router {
        launcher_router(state).layer(MockConnectInfo(SocketAddr::from(PEER)))
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> (StatusCode, Bytes) {
        let mut request = test_http::request(method, uri, body);
        if let Some(token) = bearer {
            let value = HeaderValue::from_str(&format!("{BEARER_PREFIX}{token}")).expect("header");
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
        test_http::send(app, request).await
    }

    fn credentials(username: &str, password: &str) -> String {
        serde_json::to_string(&PasswordCredentials {
            username: username.to_owned(),
            password: password.to_owned(),
        })
        .expect("json")
    }

    async fn register_ok(app: &Router) -> LauncherSession {
        let (status, body) = send(
            app,
            "POST",
            REGISTER_PATH,
            None,
            &credentials(USERNAME, PASSWORD),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_slice(&body).expect("session")
    }

    #[test]
    fn ipv6_limits_count_per_network_block() {
        let address = |text: &str| text.parse::<std::net::IpAddr>().expect("address");
        assert_eq!(limit_address(address("192.0.2.7")), "192.0.2.7");
        assert_eq!(limit_address(address("::ffff:192.0.2.7")), "192.0.2.7");
        assert_eq!(
            limit_address(address("2001:db8:1:2:aaaa::1")),
            limit_address(address("2001:db8:1:2:bbbb::2"))
        );
        assert_ne!(
            limit_address(address("2001:db8:1:2::1")),
            limit_address(address("2001:db8:1:3::1"))
        );
    }

    #[tokio::test]
    async fn info_describes_the_server() {
        let (status, body) =
            send(&app(state(AuthMode::Password)), "GET", INFO_PATH, None, "").await;
        assert_eq!(status, StatusCode::OK);
        let info: LauncherInfo = serde_json::from_slice(&body).expect("info");
        assert_eq!(info.server_name, "Test server");
        assert_eq!(info.auth_mode, AuthMode::Password);
        assert!(info.allow_registration);
        assert_eq!(info.launch_arguments, valid().launch_arguments());
        assert_eq!(info.aws_mirror.hosts, valid().aws_mirror.hosts);
        assert_eq!(
            info.aws_mirror.certificate_pem,
            TEST_AWS_MIRROR_CERTIFICATE_PEM
        );
    }

    #[tokio::test]
    async fn register_login_play_and_logout() {
        let app = app(state(AuthMode::Password));
        let registered = register_ok(&app).await;
        assert!(is_launcher_token(&registered.launcher_token));
        let (status, body) = send(
            &app,
            "POST",
            LOGIN_PATH,
            None,
            &credentials("ADA", PASSWORD),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let session: LauncherSession = serde_json::from_slice(&body).expect("session");
        let token = session.launcher_token.as_str();
        let (status, body) = send(&app, "POST", PLAY_PATH, Some(token), "").await;
        assert_eq!(status, StatusCode::OK);
        let play: PlayReply = serde_json::from_slice(&body).expect("play");
        assert!(crate::keys::is_game_session_key(&play.game_session_key));
        let (status, _) = send(&app, "POST", LOGOUT_PATH, Some(token), "").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = send(&app, "POST", PLAY_PATH, Some(token), "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(&body), "no_session");
    }

    #[tokio::test]
    async fn bad_sign_ups_are_refused() {
        let app = app(state(AuthMode::Password));
        register_ok(&app).await;
        let cases = [
            (
                credentials(USERNAME, PASSWORD),
                StatusCode::CONFLICT,
                "username_taken",
            ),
            (
                credentials("a b", PASSWORD),
                StatusCode::BAD_REQUEST,
                "invalid_username",
            ),
            (
                credentials("bob", "short"),
                StatusCode::BAD_REQUEST,
                "invalid_password",
            ),
            (
                "not json".to_owned(),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
        ];
        for (body, status, code) in cases {
            let (found, reply) = send(&app, "POST", REGISTER_PATH, None, &body).await;
            assert_eq!((found, error_code(&reply).as_str()), (status, code));
        }
    }

    #[tokio::test]
    async fn closed_registration_and_wrong_mode_are_refused() {
        let mut closed = state(AuthMode::Password);
        let mut config = (*closed.config).clone();
        config.auth.allow_registration = false;
        closed.config = Arc::new(config);
        let (status, body) = send(
            &app(closed),
            "POST",
            REGISTER_PATH,
            None,
            &credentials(USERNAME, PASSWORD),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::FORBIDDEN, "registration_closed")
        );
        let discord = app(state(AuthMode::Discord));
        let (status, body) = send(
            &discord,
            "POST",
            LOGIN_PATH,
            None,
            &credentials(USERNAME, PASSWORD),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "wrong_mode")
        );
        let password = app(state(AuthMode::Password));
        let (status, _) = send(&password, "POST", DISCORD_START_PATH, None, "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn failed_logins_are_rate_limited() {
        let app = app(state(AuthMode::Password));
        register_ok(&app).await;
        let limit = valid().auth.login_fail_limit;
        for _ in 0..limit {
            let (status, body) = send(
                &app,
                "POST",
                LOGIN_PATH,
                None,
                &credentials(USERNAME, "wrong-password"),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (StatusCode::UNAUTHORIZED, "invalid_credentials")
            );
        }
        let (status, body) = send(
            &app,
            "POST",
            LOGIN_PATH,
            None,
            &credentials(USERNAME, PASSWORD),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        );
    }

    #[tokio::test]
    async fn parallel_failed_logins_are_rate_limited() {
        let app = app(state(AuthMode::Password));
        register_ok(&app).await;
        let mut logins = tokio::task::JoinSet::new();
        for _ in 0..PARALLEL_LOGINS {
            let app = app.clone();
            logins.spawn(async move {
                let wrong = credentials(USERNAME, "wrong-password");
                send(&app, "POST", LOGIN_PATH, None, &wrong).await.0
            });
        }
        let statuses = logins.join_all().await;
        let checked = statuses
            .iter()
            .filter(|status| **status == StatusCode::UNAUTHORIZED)
            .count();
        let limit = usize::try_from(valid().auth.login_fail_limit).expect("small");
        assert_eq!(checked, limit);
    }

    #[tokio::test]
    async fn successful_logins_do_not_use_up_the_limit() {
        let app = app(state(AuthMode::Password));
        register_ok(&app).await;
        for _ in 0..=valid().auth.login_fail_limit {
            let (status, _) = send(
                &app,
                "POST",
                LOGIN_PATH,
                None,
                &credentials(USERNAME, PASSWORD),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn discord_starts_are_rate_limited() {
        let app = app(state(AuthMode::Discord));
        for _ in 0..valid().auth.login_fail_limit {
            let (status, _) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, body) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        );
    }

    #[tokio::test]
    async fn unknown_user_and_banned_account_cannot_log_in() {
        let state = state(AuthMode::Password);
        let accounts = state.accounts.clone();
        let app = app(state);
        let (status, _) = send(
            &app,
            "POST",
            LOGIN_PATH,
            None,
            &credentials("nobody", PASSWORD),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let session = register_ok(&app).await;
        let (account, _) = accounts
            .password_login("ada")
            .await
            .expect("store")
            .expect("account");
        accounts.set_banned(account.id, true);
        let (status, body) = send(
            &app,
            "POST",
            LOGIN_PATH,
            None,
            &credentials(USERNAME, PASSWORD),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::FORBIDDEN, "banned")
        );
        let (status, _) = send(&app, "POST", PLAY_PATH, Some(&session.launcher_token), "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn play_needs_a_well_formed_token() {
        let app = app(state(AuthMode::Password));
        let (status, _) = send(&app, "POST", PLAY_PATH, None, "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(&app, "POST", PLAY_PATH, Some("not-a-token"), "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn discord_login_finishes_through_the_callback() {
        let state = state(AuthMode::Discord);
        let accounts = state.accounts.clone();
        let app = app(state);
        let (status, body) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
        assert_eq!(status, StatusCode::OK);
        let start: DiscordStart = serde_json::from_slice(&body).expect("start");
        assert!(
            start
                .authorize_url
                .contains(&format!("state={}", start.state))
        );
        let poll = serde_json::to_string(&DiscordPoll {
            state: start.state.clone(),
        })
        .expect("poll");
        let (_, body) = send(&app, "POST", DISCORD_POLL_PATH, None, &poll).await;
        assert_eq!(
            serde_json::from_slice::<DiscordPollReply>(&body).expect("pending"),
            DiscordPollReply::Pending
        );
        let callback = format!(
            "{DISCORD_CALLBACK_PATH}?code={GOOD_CODE}&state={}",
            start.state
        );
        let (status, body) = send(&app, "GET", &callback, None, "").await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::OK, CALLBACK_DONE.as_bytes())
        );
        let (status, body) = send(&app, "GET", &callback, None, "").await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::NOT_FOUND, CALLBACK_EXPIRED.as_bytes()),
            "a second callback with the same state is refused"
        );
        let (status, body) = send(&app, "POST", DISCORD_POLL_PATH, None, &poll).await;
        assert_eq!(status, StatusCode::OK);
        let DiscordPollReply::Ready(session) = serde_json::from_slice(&body).expect("ready") else {
            panic!("expected a finished login");
        };
        let (status, _) = send(&app, "POST", PLAY_PATH, Some(&session.launcher_token), "").await;
        assert_eq!(status, StatusCode::OK);
        let stored = accounts
            .discord_profile(FAKE_USER_ID)
            .expect("stored profile");
        assert_eq!(
            (
                stored.username.as_str(),
                stored.email.as_deref(),
                stored.email_verified
            ),
            (FAKE_USERNAME, Some(FAKE_EMAIL), true)
        );
        let (status, body) = send(&app, "POST", DISCORD_POLL_PATH, None, &poll).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_login")
        );
    }

    #[tokio::test]
    async fn discord_callback_refuses_bad_codes_and_unknown_states() {
        let state = state(AuthMode::Discord);
        let accounts = state.accounts.clone();
        let app = app(state);
        let (_, body) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
        let start: DiscordStart = serde_json::from_slice(&body).expect("start");
        let bad = format!("{DISCORD_CALLBACK_PATH}?code=bad&state={}", start.state);
        let (status, _) = send(&app, "GET", &bad, None, "").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let retry = format!(
            "{DISCORD_CALLBACK_PATH}?code={GOOD_CODE}&state={}",
            start.state
        );
        let (status, _) = send(&app, "GET", &retry, None, "").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a failed callback uses up its state"
        );
        let unknown = format!("{DISCORD_CALLBACK_PATH}?code={GOOD_CODE}&state=unknown");
        let (status, body) = send(&app, "GET", &unknown, None, "").await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::NOT_FOUND, CALLBACK_EXPIRED.as_bytes())
        );
        let account = accounts
            .discord_account(&nw_db::DiscordProfile {
                user_id: FAKE_USER_ID.to_owned(),
                username: FAKE_USERNAME.to_owned(),
                display_name: "Nelly".to_owned(),
                email: None,
                email_verified: false,
            })
            .await
            .expect("account");
        accounts.set_banned(account.id, true);
        // The failed callback above used up its state, so the banned login starts again.
        let (_, body) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
        let again: DiscordStart = serde_json::from_slice(&body).expect("start again");
        let banned = format!(
            "{DISCORD_CALLBACK_PATH}?code={GOOD_CODE}&state={}",
            again.state
        );
        let (status, _) = send(&app, "GET", &banned, None, "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// Logs in through the callback with a server that `guild_id` names; returns the callback
    /// status and page.
    async fn guild_login(guild_id: &str) -> (StatusCode, Vec<u8>, MemoryAccounts) {
        let mut state = state(AuthMode::Discord);
        let mut config = (*state.config).clone();
        config.discord.guild_id = guild_id.to_owned();
        state.config = Arc::new(config);
        let accounts = state.accounts.clone();
        let app = app(state);
        let (_, body) = send(&app, "POST", DISCORD_START_PATH, None, "").await;
        let start: DiscordStart = serde_json::from_slice(&body).expect("start");
        assert!(start.authorize_url.contains("guilds.members.read"));
        let callback = format!(
            "{DISCORD_CALLBACK_PATH}?code={GOOD_CODE}&state={}",
            start.state
        );
        let (status, body) = send(&app, "GET", &callback, None, "").await;
        (status, body.to_vec(), accounts)
    }

    #[tokio::test]
    async fn a_required_discord_server_refuses_users_outside_it() {
        let (status, body, accounts) = guild_login("999").await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::FORBIDDEN, CALLBACK_NOT_MEMBER.as_bytes())
        );
        assert_eq!(
            accounts.discord_profile(FAKE_USER_ID),
            None,
            "no account for a user outside the server"
        );
        let (status, body, accounts) = guild_login(FAKE_GUILD_ID).await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::OK, CALLBACK_DONE.as_bytes())
        );
        assert!(accounts.discord_profile(FAKE_USER_ID).is_some());
    }
}
