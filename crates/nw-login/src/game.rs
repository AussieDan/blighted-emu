// SPDX-License-Identifier: AGPL-3.0-or-later
//! Game client routes up to character select (M2 spec sections 3.1 and 3.3, JSON note
//! `docs/re/2026-09-02-login-json-shapes.md`).
//!
//! The channel document on `login.<domain>` puts the game session key into every endpoint in the
//! path form (JSON note section 7), so the client calls
//! `https://game.<domain>/s/<session>/Prod/steam` and
//! `https://game.<domain>/s/<session>/prod/game/...`. Every route checks the session. The Steam
//! tickets in the request bodies are never stored or trusted.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{Method, Uri};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use nw_db::{Character, NewCharacter};
use nw_protocol::game_api::{
    AUTH_STACK_STAGE, AUTH_STACK_TAG, CHARACTER_PATH, CHARACTERS_SEGMENT, ChannelDocument,
    CharacterSummary, Cluster, CreateCharacterReply, CreateCharacterRequestBody, GATEWAY_STAGE,
    GET_LOGIN_INFO_PATH, HTTP_GATEWAY_TAG, LIST_WORLDS_PATH, LOGIN_GATEWAY_TAG, ListWorldsBody,
    ListWorldsReply, LoginInfoList, LoginInfoReply, NO_TRANSFER_REGION, NameRecord,
    PUBLIC_STATUS_OPEN, PublicApi, QUEUE_LOGIN_PATH, STEAM_AUTH_PATH, SteamAuthReply,
    VALIDATOR_SEGMENT, ValidateCharacterRequest, WORLD_SET_LIVE, WORLD_STATUS_ACTIVE,
    WORLD_TYPE_OPEN_WORLD, WORLD_VERSION, WORLDS_PATH, WorldModel,
};
use nw_protocol::{LoginQueueReply, parse_login_queue_request};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::{EdgeConfig, QUEUE_LIMIT_WINDOW_SECONDS, WorldConfig};
use crate::error::{ApiError, parse_json};
use crate::keys::{is_game_session_key, new_random_token};
use crate::look;
use crate::names::check_character_name;
use crate::queue::{QueueAction, QueueSettings, QueueStore, QueueTime, queue_reply};
use crate::secrets::SigningKey;
use crate::store::{CharacterStore, GameSessionStore, RateLimiter};
use crate::time::http_date;
use nw_common::time::unix_seconds;

/// Path part in front of the game session key: `/s/<session>`.
pub const SESSION_PREFIX: &str = "/s";
/// File name ending of the channel document.
pub const CHANNEL_FILE_SUFFIX: &str = ".json";
/// The client may add dot-separated parts around the channel key (JSON note section 7).
const CHANNEL_KEY_SEPARATOR: char = '.';
/// Bytes other than letters, digits and [`CHANNEL_KEY_SEPARATOR`] that a channel key may hold.
const CHANNEL_KEY_EXTRA_BYTES: [u8; 2] = [b'_', b'-'];
/// The channel document route: one path segment, `<channel key>.json`.
const CHANNEL_ROUTE: &str = "/{file}";
/// Id of the one cluster in the channel document. The client also names its `RegionId`
/// remote-config layers after it (`client-connection.authorization.cluster`).
pub(crate) const CLUSTER_ID: &str = "blighted";
/// Region text in every `publicApis` element; the client signs requests with it, and pings
/// `dynamodb.<region>.amazonaws.com` for it (region ping note).
pub const API_REGION: &str = "us-east-1";
/// Value of the three `/steam` credentials; the client signs requests with them and the server
/// ignores the signatures (JSON note section 7).
pub(crate) const AUTH_STACK_CREDENTIAL: &str = "blighted";
/// Text shown in logs in place of a game session key.
const SESSION_MASK: &str = "<session>";
const PATH_SEPARATOR: char = '/';
/// Rate-limit key prefix of queue joins and refreshes: `<prefix><world>:<account>`.
const QUEUE_LIMIT_PREFIX: &str = "queue:";
/// Separator between the world id and the random part of a queue ticket id.
const TICKET_WORLD_SEPARATOR: char = '.';
/// Largest game request body. The character create body is stored (M2 spec section 7).
pub const GAME_BODY_LIMIT_BYTES: usize = 64 * 1024;

/// Everything the game routes need.
#[derive(Clone)]
pub struct GameState<G, C, R, Q> {
    /// Server configuration.
    pub config: Arc<EdgeConfig>,
    /// Game session keys.
    pub game_sessions: G,
    /// Characters and names.
    pub characters: C,
    /// Rate-limit counters.
    pub limits: R,
    /// Login queues.
    pub queue: Q,
    /// Entry ticket signing key.
    pub signing_key: SigningKey,
}

/// The session parameter of a session route; the other path parameters are ignored here.
#[derive(Debug, Deserialize)]
struct SessionPath {
    session: String,
}

/// The ticket parameter of a queue refresh.
#[derive(Debug, Deserialize)]
struct TicketPath {
    ticket_id: String,
}

/// The character parameter of the delete route.
#[derive(Debug, Deserialize)]
struct CharacterPath {
    character_id: Uuid,
}

/// The world parameter of a character route.
#[derive(Debug, Deserialize)]
struct WorldPath {
    world_id: String,
}

/// The account of the live game session named in the path (`/s/{session}/...`). Handlers take it
/// before the body, so a request without a valid session is refused before its body is read
/// (review minor m5).
struct SessionAccount(Uuid);

impl<G, C, R, Q> FromRequestParts<GameState<G, C, R, Q>> for SessionAccount
where
    G: GameSessionStore,
    C: CharacterStore,
    R: Send + Sync,
    Q: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &GameState<G, C, R, Q>,
    ) -> Result<Self, ApiError> {
        let Path(path) = Path::<SessionPath>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::NoSession)?;
        session_account(state, &path.session).await.map(Self)
    }
}

/// `/s/{session}<rest>` as an `axum` route.
fn session_route(rest: &str) -> String {
    format!("{SESSION_PREFIX}/{{session}}{rest}")
}

/// A gateway operation path under the session: `/s/{session}/prod<operation>`.
fn gateway_route(operation: &str) -> String {
    session_route(&format!("{GATEWAY_STAGE}{operation}"))
}

/// The game routes on `game.<domain>`.
pub fn game_router<G, C, R, Q>(state: GameState<G, C, R, Q>) -> Router
where
    G: GameSessionStore,
    C: CharacterStore,
    R: RateLimiter,
    Q: QueueStore,
{
    Router::new()
        .route(
            &session_route(&format!("{AUTH_STACK_STAGE}{STEAM_AUTH_PATH}")),
            post(steam_auth::<G, C, R, Q>),
        )
        .route(
            &gateway_route(GET_LOGIN_INFO_PATH),
            get(login_info::<G, C, R, Q>).post(login_info::<G, C, R, Q>),
        )
        .route(
            &gateway_route(LIST_WORLDS_PATH),
            get(list_worlds::<G, C, R, Q>).post(list_worlds::<G, C, R, Q>),
        )
        .route(
            &gateway_route(&format!("{WORLDS_PATH}/{{world_id}}{CHARACTERS_SEGMENT}")),
            post(create_character::<G, C, R, Q>),
        )
        .route(
            &gateway_route(&format!("{WORLDS_PATH}/{{world_id}}{VALIDATOR_SEGMENT}")),
            post(validate_character::<G, C, R, Q>),
        )
        .route(
            &gateway_route(&format!("{CHARACTER_PATH}/{{character_id}}")),
            delete(delete_character::<G, C, R, Q>),
        )
        .route(
            &gateway_route(QUEUE_LOGIN_PATH),
            post(queue_join::<G, C, R, Q>),
        )
        .route(
            &gateway_route(&format!("{QUEUE_LOGIN_PATH}/{{ticket_id}}")),
            post(queue_refresh::<G, C, R, Q>),
        )
        .layer(DefaultBodyLimit::max(GAME_BODY_LIMIT_BYTES))
        .fallback(unknown_route)
        .method_not_allowed_fallback(unknown_route)
        .with_state(state)
}

/// The channel document route on `login.<domain>`: `GET /<key>.json`.
pub fn channel_router<G, C, R, Q>(state: GameState<G, C, R, Q>) -> Router
where
    G: GameSessionStore,
    C: CharacterStore,
    R: RateLimiter,
    Q: QueueStore,
{
    Router::new()
        .route(CHANNEL_ROUTE, get(channel::<G, C, R, Q>))
        .with_state(state)
}

/// The account of a live game session; extends the session. The session of a banned account
/// ends here (the admin tool bans in the global database, which has no list of its sessions):
/// its next call, the channel document, a queue refresh included, is refused, so a banned
/// player in the queue is never admitted.
async fn session_account<G: GameSessionStore, C: CharacterStore, R, Q>(
    state: &GameState<G, C, R, Q>,
    session: &str,
) -> Result<Uuid, ApiError> {
    if !is_game_session_key(session) {
        return Err(ApiError::NoSession);
    }
    let account = state
        .game_sessions
        .resolve(session, state.config.auth.game_session_ttl_seconds)
        .await?
        .ok_or(ApiError::NoSession)?;
    if state.characters.account_banned(account).await? {
        state.game_sessions.revoke(session).await?;
        tracing::info!(%account, "game session of a banned account ended");
        return Err(ApiError::Banned);
    }
    Ok(account)
}

fn is_channel_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || char::from(byte) == CHANNEL_KEY_SEPARATOR
        || CHANNEL_KEY_EXTRA_BYTES.contains(&byte)
}

/// The game session key inside a channel key such as `<key>` or
/// `SANDBOX_ID.x.<key>.CROSSPLAY.off` (the file name without `.json`, JSON note section 7). The
/// channel key may hold only ASCII letters, digits, `.`, `_` and `-`.
pub fn channel_session_key(channel_key: &str) -> Option<&str> {
    if !channel_key.bytes().all(is_channel_key_byte) {
        return None;
    }
    channel_key
        .split(CHANNEL_KEY_SEPARATOR)
        .find(|part| is_game_session_key(part))
}

/// The channel document for one game session: every endpoint is `game.<domain>/s/<session>`,
/// with the public HTTPS port after the host when it is not the default.
pub fn channel_document(config: &EdgeConfig, session: &str) -> ChannelDocument {
    let endpoint = format!("{}{SESSION_PREFIX}/{session}", config.game_authority());
    let api = |tag: &str| PublicApi {
        client_tag: tag.to_owned(),
        api_endpoint: endpoint.clone(),
        aws_region: API_REGION.to_owned(),
    };
    ChannelDocument {
        channel_name: config.server_name.clone(),
        clusters: [(
            CLUSTER_ID.to_owned(),
            Cluster {
                localized_name: config.server_name.clone(),
                public_apis: vec![
                    api(AUTH_STACK_TAG),
                    api(LOGIN_GATEWAY_TAG),
                    api(HTTP_GATEWAY_TAG),
                ],
            },
        )]
        .into(),
    }
}

async fn channel<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    Path(file): Path<String>,
    method: Method,
    uri: Uri,
) -> Result<Json<ChannelDocument>, ApiError>
where
    G: GameSessionStore,
    C: CharacterStore,
{
    let Some(key) = file.strip_suffix(CHANNEL_FILE_SUFFIX) else {
        return Err(unknown_route(method, uri).await);
    };
    let session = channel_session_key(key).ok_or(ApiError::NoSession)?;
    session_account(&state, session).await?;
    Ok(Json(channel_document(&state.config, session)))
}

fn session_seconds(config: &EdgeConfig) -> i64 {
    i64::try_from(config.auth.game_session_ttl_seconds).unwrap_or(i64::MAX)
}

async fn steam_auth<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    _session: SessionAccount,
) -> Result<Json<SteamAuthReply>, ApiError> {
    let expires_at = unix_seconds().saturating_add(session_seconds(&state.config));
    Ok(Json(SteamAuthReply {
        access_key_id: AUTH_STACK_CREDENTIAL.to_owned(),
        secret_key: AUTH_STACK_CREDENTIAL.to_owned(),
        session_token: AUTH_STACK_CREDENTIAL.to_owned(),
        expiration: http_date(expires_at),
    }))
}

fn to_json_integer(value: u32) -> i64 {
    i64::from(value)
}

/// The world model of `world`; `is_full` when `world-director` reports no free player slot
/// (its players are at its capacity, or no report came: the queue then admits nobody either).
fn world_model(world: &WorldConfig, is_full: bool) -> WorldModel {
    WorldModel {
        is_full,
        max_account_characters: to_json_integer(world.max_characters_per_account),
        max_connection_count: to_json_integer(world.capacity),
        public_name: world.name.clone(),
        public_status_code: PUBLIC_STATUS_OPEN,
        transfer_to_region: NO_TRANSFER_REGION.to_owned(),
        world_id: world.id.to_string(),
        world_name: world.name.clone(),
        world_set: WORLD_SET_LIVE.to_owned(),
        world_status: WORLD_STATUS_ACTIVE.to_owned(),
        world_type: WORLD_TYPE_OPEN_WORLD.to_owned(),
        world_version: WORLD_VERSION.to_owned(),
    }
}

/// Character slots of an account on the whole server: the sum of the per-world limits.
fn channel_character_slots(config: &EdgeConfig) -> i64 {
    config
        .worlds
        .iter()
        .map(|world| to_json_integer(world.max_characters_per_account))
        .sum()
}

async fn world_models<G, C, R, Q: QueueStore>(
    state: &GameState<G, C, R, Q>,
) -> Result<Vec<WorldModel>, ApiError> {
    let mut models = Vec::with_capacity(state.config.worlds.len());
    for world in &state.config.worlds {
        let is_full = state.queue.free_slots(world.id).await? == 0;
        models.push(world_model(world, is_full));
    }
    Ok(models)
}

/// The character summary the client shows (JSON note section 5.1.1). Its `currentLevel` is
/// zero-based, as the client's own create request (`"currentLevel":0`) and the saved level are.
pub(crate) fn character_summary(world_id: Uuid, character: &Character) -> CharacterSummary {
    CharacterSummary {
        published_data: look::published_data(
            &character.appearance,
            character.level,
            character.published_look.as_deref(),
        ),
        character_id: character.id.to_string(),
        created_date: http_date(character.created_at_unix),
        name: character.name.clone(),
        normalized_name: character.name.to_lowercase(),
        persona_id: character.account_id.to_string(),
        world_id: world_id.to_string(),
    }
}

fn name_record(character: &Character) -> NameRecord {
    NameRecord {
        created_date: http_date(character.created_at_unix),
        display_name: character.name.clone(),
        normalized_name: character.name.to_lowercase(),
        owning_resource_id: character.id.to_string(),
        persona_id: character.account_id.to_string(),
    }
}

async fn login_info<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    SessionAccount(account): SessionAccount,
) -> Result<Json<LoginInfoReply>, ApiError>
where
    C: CharacterStore,
    Q: QueueStore,
{
    let mut characters = Vec::new();
    let mut name_reservations = Vec::new();
    for world in &state.config.worlds {
        for character in state.characters.characters(world.id, account).await? {
            name_reservations.push(name_record(&character));
            characters.push(character_summary(world.id, &character));
        }
    }
    Ok(Json(LoginInfoReply {
        login_info_list: LoginInfoList {
            characters,
            max_channel_characters: channel_character_slots(&state.config),
            name_reservations,
            worlds: world_models(&state).await?,
        },
    }))
}

async fn list_worlds<G, C, R, Q: QueueStore>(
    State(state): State<GameState<G, C, R, Q>>,
    _session: SessionAccount,
) -> Result<Json<ListWorldsReply>, ApiError> {
    Ok(Json(ListWorldsReply {
        list_worlds_response_body: ListWorldsBody {
            worlds: world_models(&state).await?,
        },
    }))
}

/// The configured world of a path's world id.
fn configured_world<'a>(
    config: &'a EdgeConfig,
    world_id: &str,
) -> Result<&'a WorldConfig, ApiError> {
    Uuid::parse_str(world_id)
        .ok()
        .and_then(|id| config.world(id))
        .ok_or(ApiError::UnknownWorld)
}

async fn create_character<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    SessionAccount(account): SessionAccount,
    Path(path): Path<WorldPath>,
    body: Bytes,
) -> Result<Json<CreateCharacterReply>, ApiError>
where
    C: CharacterStore,
{
    let world = configured_world(&state.config, &path.world_id)?;
    let body: CreateCharacterRequestBody = parse_json(&body)?;
    let mut request = body.create_character_request.ok_or(ApiError::BadRequest)?;
    let name = request.name.clone().ok_or(ApiError::BadRequest)?;
    check_character_name(&name)?;
    let owned = state.characters.characters(world.id, account).await?.len();
    let limit = usize::try_from(world.max_characters_per_account).unwrap_or(usize::MAX);
    if owned >= limit {
        return Err(ApiError::CharacterLimit);
    }
    // The stored request keeps everything but the Steam ticket.
    request.steam_auth_ticket = None;
    let appearance = serde_json::to_value(&request).map_err(|_| ApiError::BadRequest)?;
    let new = NewCharacter {
        id: Uuid::new_v4(),
        account_id: account,
        name,
        appearance,
    };
    let character = state.characters.create(world.id, &new).await?;
    Ok(Json(CreateCharacterReply {
        character: character_summary(world.id, &character),
    }))
}

/// Deletes a character of the session's account. The client shows error 16 for any status but
/// 200 (JSON note section 3); the reply body model is Unknown, so a deleted character gets an
/// empty JSON object.
async fn delete_character<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    SessionAccount(account): SessionAccount,
    Path(path): Path<CharacterPath>,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>, ApiError>
where
    C: CharacterStore,
{
    let world = world_of_character(&state, account, path.character_id).await?;
    if !state
        .characters
        .delete(world.id, account, path.character_id)
        .await?
    {
        return Err(ApiError::UnknownCharacter);
    }
    Ok(Json(serde_json::Map::new()))
}

/// Checks a name. The reply body model is Unknown (JSON note section 5.4), so a free name gets an
/// empty JSON object and a refused name gets an error status.
async fn validate_character<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    _session: SessionAccount,
    Path(path): Path<WorldPath>,
    body: Bytes,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>, ApiError>
where
    C: CharacterStore,
{
    configured_world(&state.config, &path.world_id)?;
    let request: ValidateCharacterRequest = parse_json(&body)?;
    let name = request
        .validate_character_body
        .and_then(|body| body.name)
        .ok_or(ApiError::BadRequest)?;
    check_character_name(&name)?;
    if state.characters.name_taken(&name).await? {
        return Err(ApiError::CharacterNameTaken);
    }
    Ok(Json(serde_json::Map::new()))
}

/// A new queue ticket id: `<world id><separator><random>`, so a refresh finds its world.
fn new_ticket_id(world_id: Uuid) -> Result<String, ApiError> {
    let random = new_random_token().map_err(|_| ApiError::Unavailable)?;
    Ok(format!(
        "{}{TICKET_WORLD_SEPARATOR}{random}",
        world_id.simple()
    ))
}

/// The configured world of a queue ticket id.
fn ticket_world<'a>(config: &'a EdgeConfig, ticket_id: &str) -> Option<&'a WorldConfig> {
    let (world_id, _) = ticket_id.split_once(TICKET_WORLD_SEPARATOR)?;
    config.world(Uuid::parse_str(world_id).ok()?)
}

/// Counts one queue request of `account` in `world` (M2 spec section 8).
async fn limit_queue<G, C, R: RateLimiter, Q>(
    state: &GameState<G, C, R, Q>,
    world: &WorldConfig,
    account: Uuid,
) -> Result<(), ApiError> {
    let key = format!("{QUEUE_LIMIT_PREFIX}{}:{account}", world.id);
    let requests = state
        .limits
        .increment(&key, QUEUE_LIMIT_WINDOW_SECONDS)
        .await?;
    if requests > u64::from(world.queue_requests_per_minute) {
        Err(ApiError::RateLimited)
    } else {
        Ok(())
    }
}

/// Runs one queue step and builds the reply.
async fn queue_step<G, C, R, Q: QueueStore>(
    state: &GameState<G, C, R, Q>,
    world: &WorldConfig,
    account: Uuid,
    action: QueueAction,
) -> Result<Json<LoginQueueReply>, ApiError> {
    let status = state
        .queue
        .update(
            &QueueSettings::for_world(world),
            account,
            action,
            QueueTime::StoreClock,
        )
        .await?
        .ok_or(ApiError::UnknownTicket)?;
    Ok(Json(queue_reply(
        &status,
        world,
        &state.signing_key,
        unix_seconds(),
    )))
}

/// Whether `character_id` is a character of `account` in `world`.
async fn owns_character<G, C: CharacterStore, R, Q>(
    state: &GameState<G, C, R, Q>,
    world: &WorldConfig,
    account: Uuid,
    character_id: Uuid,
) -> Result<bool, ApiError> {
    Ok(state
        .characters
        .character(world.id, character_id)
        .await?
        .is_some_and(|character| character.account_id == account))
}

/// The world that holds `character_id` of `account`. Right after it creates a character, the
/// client queues with an empty `WorldId` (exit test, 2026-09-02), so every world is searched.
async fn world_of_character<G, C: CharacterStore, R, Q>(
    state: &GameState<G, C, R, Q>,
    account: Uuid,
    character_id: Uuid,
) -> Result<&WorldConfig, ApiError> {
    for world in &state.config.worlds {
        if owns_character(state, world, account, character_id).await? {
            return Ok(world);
        }
    }
    Err(ApiError::UnknownCharacter)
}

/// `QueueLogin`: joins the queue with a character of the session's account (review finding C3).
async fn queue_join<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    SessionAccount(account): SessionAccount,
    body: Bytes,
) -> Result<Json<LoginQueueReply>, ApiError>
where
    C: CharacterStore,
    R: RateLimiter,
    Q: QueueStore,
{
    let request = parse_login_queue_request(&body).map_err(|_| ApiError::BadRequest)?;
    let character_id = request
        .character_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(ApiError::UnknownCharacter)?;
    let world = match request.world_id.as_deref().filter(|id| !id.is_empty()) {
        Some(world_id) => {
            let world = configured_world(&state.config, world_id)?;
            limit_queue(&state, world, account).await?;
            if !owns_character(&state, world, account, character_id).await? {
                return Err(ApiError::UnknownCharacter);
            }
            world
        }
        None => {
            let world = world_of_character(&state, account, character_id).await?;
            limit_queue(&state, world, account).await?;
            world
        }
    };
    let action = QueueAction::Join {
        character_id,
        new_ticket_id: new_ticket_id(world.id)?,
    };
    queue_step(&state, world, account, action).await
}

/// `RefreshQueuedLogin`: the ticket id in the path is the whole request (JSON note section 5.5).
async fn queue_refresh<G, C, R, Q>(
    State(state): State<GameState<G, C, R, Q>>,
    SessionAccount(account): SessionAccount,
    Path(path): Path<TicketPath>,
) -> Result<Json<LoginQueueReply>, ApiError>
where
    R: RateLimiter,
    Q: QueueStore,
{
    let world = ticket_world(&state.config, &path.ticket_id).ok_or(ApiError::UnknownTicket)?;
    limit_queue(&state, world, account).await?;
    let action = QueueAction::Refresh {
        ticket_id: path.ticket_id,
    };
    queue_step(&state, world, account, action).await
}

/// `text` split at `separator`, with `mask` applied to each part.
fn mask_parts(text: &str, separator: char, mask: impl Fn(&str) -> String) -> String {
    text.split(separator)
        .map(mask)
        .collect::<Vec<_>>()
        .join(&separator.to_string())
}

/// `path` with every game session key replaced by [`SESSION_MASK`], also inside a path segment
/// such as `SANDBOX_ID.x.<key>.json`.
pub fn masked_path(path: &str) -> String {
    mask_parts(path, PATH_SEPARATOR, |segment| {
        mask_parts(segment, CHANNEL_KEY_SEPARATOR, |part| {
            if is_game_session_key(part) {
                SESSION_MASK.to_owned()
            } else {
                part.to_owned()
            }
        })
    })
}

/// What [`LogBudget::decide`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogDecision {
    /// Write the line; `dropped_before` lines were dropped since the last written line.
    Write {
        /// Lines dropped since the last written line.
        dropped_before: u64,
    },
    /// Drop the line.
    Drop,
}

#[derive(Debug)]
struct BudgetState {
    window_start: Option<Instant>,
    written: u32,
    dropped: u64,
}

/// At most `lines` log lines per `window`; it counts the lines it drops.
#[derive(Debug)]
pub(crate) struct LogBudget {
    lines: u32,
    window: Duration,
    state: Mutex<BudgetState>,
}

impl LogBudget {
    /// A budget with no line written yet.
    pub(crate) const fn new(lines: u32, window: Duration) -> Self {
        Self {
            lines,
            window,
            state: Mutex::new(BudgetState {
                window_start: None,
                written: 0,
                dropped: 0,
            }),
        }
    }

    /// Whether a line at `now` may be written.
    pub(crate) fn decide(&self, now: Instant) -> LogDecision {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let new_window = state
            .window_start
            .is_none_or(|start| now.saturating_duration_since(start) >= self.window);
        if new_window {
            state.window_start = Some(now);
            state.written = 0;
        }
        if state.written < self.lines {
            state.written += 1;
            LogDecision::Write {
                dropped_before: std::mem::take(&mut state.dropped),
            }
        } else {
            state.dropped += 1;
            LogDecision::Drop
        }
    }
}

/// Unknown-route log lines allowed per window, so one client cannot fill the log.
const UNKNOWN_ROUTE_LOG_LINES: u32 = 60;
const UNKNOWN_ROUTE_LOG_WINDOW: Duration = Duration::from_secs(60);
static UNKNOWN_ROUTE_LOGS: LogBudget =
    LogBudget::new(UNKNOWN_ROUTE_LOG_LINES, UNKNOWN_ROUTE_LOG_WINDOW);

/// Logs a request no route serves, so that the exit test shows what the client asked for. At most
/// [`UNKNOWN_ROUTE_LOG_LINES`] lines per minute are written; the next written line says how many
/// were dropped.
pub async fn unknown_route(method: Method, uri: Uri) -> ApiError {
    if let LogDecision::Write { dropped_before } = UNKNOWN_ROUTE_LOGS.decide(Instant::now()) {
        tracing::warn!(
            %method,
            path = %masked_path(uri.path()),
            dropped_before,
            "request for an unknown route"
        );
    }
    ApiError::UnknownRoute
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::body::Bytes;
    use axum::http::StatusCode;
    use nw_db::NewCharacter;

    use super::*;
    use crate::config::tests::{valid, valid_two_worlds};
    use crate::keys::new_game_session_key;
    use crate::memory::{MemoryCharacters, MemoryGameSessions, MemoryQueue, MemoryRateLimiter};
    use crate::signature::verify_entry;
    use crate::test_http::{self, LogCapture, error_code};

    const CLAIM_SECONDS: u64 = 600;
    const GMT_SUFFIX: &str = " GMT";
    /// The token path that earlier builds served.
    const OLD_TOKEN_PATH: &str = "/token";
    const UNKNOWN_ROUTE_LOG: &str = "request for an unknown route";
    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const ONE_SLOT: u32 = 1;
    const FEW_REQUESTS: u32 = 2;
    const FIRST_IN_LINE: i64 = 1;
    const NAME: &str = "Ada";
    const SECOND_NAME: &str = "Bea";
    const THIRD_NAME: &str = "Cid";
    const STEAM_TICKET: &str = "ticket";
    /// `max_characters_per_account` of the test configuration.
    const TEST_CHARACTER_LIMIT: u32 = 2;
    const OTHER_KEY: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst";

    pub(crate) type TestState =
        GameState<MemoryGameSessions, MemoryCharacters, MemoryRateLimiter, MemoryQueue>;

    pub(crate) struct Fixture {
        pub(crate) state: TestState,
        pub(crate) session: String,
        pub(crate) account: Uuid,
    }

    /// A live game session of a new account.
    async fn new_session(state: &TestState) -> (String, Uuid) {
        let session = new_game_session_key().expect("key");
        let account = Uuid::new_v4();
        state
            .game_sessions
            .create(&session, account, CLAIM_SECONDS)
            .await
            .expect("session");
        (session, account)
    }

    /// Free player slots of each test world: more than any test uses.
    const FIXTURE_FREE_SLOTS: u32 = 1_000;
    /// The slot count lives longer than any test.
    const FIXTURE_SLOTS_TTL_MS: u64 = 3_600_000;

    async fn fixture_with(config: EdgeConfig) -> Fixture {
        let world_ids: Vec<Uuid> = config.worlds.iter().map(|world| world.id).collect();
        let state = GameState {
            config: Arc::new(config),
            game_sessions: MemoryGameSessions::default(),
            characters: MemoryCharacters::new(&world_ids),
            limits: MemoryRateLimiter::default(),
            queue: MemoryQueue::default(),
            signing_key: SigningKey::from_hex(KEY_HEX).expect("key"),
        };
        // The director reports free player slots for every world.
        for world in &world_ids {
            state
                .queue
                .set_free_slots(*world, FIXTURE_FREE_SLOTS, FIXTURE_SLOTS_TTL_MS)
                .await
                .expect("free slots");
        }
        let (session, account) = new_session(&state).await;
        Fixture {
            state,
            session,
            account,
        }
    }

    pub(crate) async fn fixture() -> Fixture {
        fixture_with(valid()).await
    }

    fn game_path(session: &str, rest: &str) -> String {
        format!("{SESSION_PREFIX}/{session}{rest}")
    }

    async fn call(app: &Router, method: &str, uri: &str) -> (StatusCode, Bytes) {
        test_http::send(app, test_http::request(method, uri, "")).await
    }

    async fn post(app: &Router, uri: &str, body: &str) -> (StatusCode, Bytes) {
        test_http::send(app, test_http::request("POST", uri, body)).await
    }

    fn character_path(session: &str, world: &str, segment: &str) -> String {
        game_path(
            session,
            &format!("{GATEWAY_STAGE}{WORLDS_PATH}/{world}{segment}"),
        )
    }

    fn create_body(name: &str) -> String {
        serde_json::json!({"CreateCharacterRequest": {
            "Name": name,
            "CharacterCreationParams": "{}",
            "SteamAuthTicket": STEAM_TICKET,
        }})
        .to_string()
    }

    fn validate_body(name: &str) -> String {
        serde_json::json!({"ValidateCharacterBody": {"Name": name}}).to_string()
    }

    #[test]
    fn channel_key_is_found_in_the_file_name() {
        assert_eq!(channel_session_key(OTHER_KEY), Some(OTHER_KEY));
        let decorated = format!("SANDBOX_ID.test.{OTHER_KEY}.CROSSPLAY.off");
        assert_eq!(channel_session_key(&decorated), Some(OTHER_KEY));
        assert_eq!(channel_session_key("live"), None);
        assert_eq!(channel_session_key(&format!("{OTHER_KEY}\"x")), None);
    }

    #[test]
    fn channel_document_points_every_api_at_the_session() {
        let config = valid();
        let document = channel_document(&config, OTHER_KEY);
        let endpoint = format!("game.example.org/s/{OTHER_KEY}");
        let cluster = &document.clusters[CLUSTER_ID];
        let tags: Vec<&str> = cluster
            .public_apis
            .iter()
            .map(|api| api.client_tag.as_str())
            .collect();
        assert_eq!(tags, [AUTH_STACK_TAG, LOGIN_GATEWAY_TAG, HTTP_GATEWAY_TAG]);
        assert!(
            cluster
                .public_apis
                .iter()
                .all(|api| api.api_endpoint == endpoint)
        );
        // No Omni settings: the client reads them only on the backend that skips `/steam`, and
        // then calls Amazon's identity host (unknowns note, second pass sections 2 and 8).
        let value = serde_json::to_value(&document).expect("json");
        assert!(value.get("platformMetadata").is_none());
    }

    #[test]
    fn channel_document_names_another_public_port() {
        const PUBLIC_HTTPS_PORT: u16 = 8443;
        let mut config = valid();
        config.public_https_port = PUBLIC_HTTPS_PORT;
        let document = channel_document(&config, OTHER_KEY);
        let endpoint = format!("game.example.org:8443/s/{OTHER_KEY}");
        assert!(
            document.clusters[CLUSTER_ID]
                .public_apis
                .iter()
                .all(|api| api.api_endpoint == endpoint)
        );
    }

    #[test]
    fn session_keys_are_masked_in_logs() {
        assert_eq!(
            masked_path(&format!("/s/{OTHER_KEY}/prod/game/x")),
            "/s/<session>/prod/game/x"
        );
        assert_eq!(
            masked_path(&format!("/x/SANDBOX_ID.y.{OTHER_KEY}.json")),
            "/x/SANDBOX_ID.y.<session>.json"
        );
    }

    const BUDGET_LINES: u32 = 2;
    const BUDGET_WINDOW: Duration = Duration::from_secs(60);
    const LATER: Duration = Duration::from_secs(1);
    const TWO_DROPPED: u64 = 2;

    #[test]
    fn the_log_budget_drops_lines_above_its_limit() {
        let budget = LogBudget::new(BUDGET_LINES, BUDGET_WINDOW);
        let start = Instant::now();
        assert_eq!(
            budget.decide(start),
            LogDecision::Write { dropped_before: 0 }
        );
        assert_eq!(
            budget.decide(start + LATER),
            LogDecision::Write { dropped_before: 0 }
        );
        assert_eq!(budget.decide(start + LATER), LogDecision::Drop);
        assert_eq!(budget.decide(start + LATER), LogDecision::Drop);
        assert_eq!(
            budget.decide(start + BUDGET_WINDOW),
            LogDecision::Write {
                dropped_before: TWO_DROPPED
            }
        );
        assert_eq!(
            budget.decide(start + BUDGET_WINDOW + LATER),
            LogDecision::Write { dropped_before: 0 }
        );
    }

    #[tokio::test]
    async fn wrong_methods_are_logged_as_unknown_routes() {
        let fixture = fixture().await;
        let app = game_router(fixture.state.clone());
        let logs = LogCapture::start();
        let steam = game_path(
            &fixture.session,
            &format!("{AUTH_STACK_STAGE}{STEAM_AUTH_PATH}"),
        );
        let (status, body) = call(&app, "GET", &steam).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_route")
        );
        let text = logs.text();
        assert!(text.contains(UNKNOWN_ROUTE_LOG), "{text}");
        assert!(!text.contains(&fixture.session), "{text}");
    }

    #[tokio::test]
    async fn channel_requests_that_are_not_json_are_logged() {
        let fixture = fixture().await;
        let app = channel_router(fixture.state.clone());
        let logs = LogCapture::start();
        let (status, body) = call(&app, "GET", "/favicon.ico").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_route")
        );
        assert!(logs.text().contains(UNKNOWN_ROUTE_LOG));
    }

    #[tokio::test]
    async fn channel_route_needs_a_live_session() {
        let fixture = fixture().await;
        let app = channel_router(fixture.state.clone());
        let (status, body) = call(&app, "GET", &format!("/{}.json", fixture.session)).await;
        assert_eq!(status, StatusCode::OK);
        let document: ChannelDocument = serde_json::from_slice(&body).expect("document");
        assert_eq!(document.channel_name, fixture.state.config.server_name);
        let (status, body) = call(&app, "GET", &format!("/{OTHER_KEY}.json")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::UNAUTHORIZED, "no_session")
        );
        let (status, _) = call(&app, "GET", &format!("/{}", fixture.session)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn steam_calls_need_a_session() {
        let fixture = fixture().await;
        let app = game_router(fixture.state.clone());
        let steam = format!("{AUTH_STACK_STAGE}{STEAM_AUTH_PATH}");
        let (status, body) = call(&app, "POST", &game_path(&fixture.session, &steam)).await;
        assert_eq!(status, StatusCode::OK);
        let reply: SteamAuthReply = serde_json::from_slice(&body).expect("steam");
        assert!(reply.expiration.ends_with(GMT_SUFFIX));
        let (status, _) = call(&app, "POST", &game_path(OTHER_KEY, &steam)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(&app, "POST", &game_path("short", &steam)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The client never uses a token URL from the channel document (unknowns note section 2).
        let (status, body) = call(&app, "POST", &game_path(&fixture.session, OLD_TOKEN_PATH)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_route")
        );
    }

    #[tokio::test]
    async fn login_info_lists_the_account_characters_and_worlds() {
        let fixture = fixture().await;
        let world_id = fixture.state.config.worlds[0].id;
        let created = fixture
            .state
            .characters
            .create(
                world_id,
                &NewCharacter {
                    id: Uuid::new_v4(),
                    account_id: fixture.account,
                    name: NAME.to_owned(),
                    appearance: serde_json::json!({}),
                },
            )
            .await
            .expect("create");
        let app = game_router(fixture.state.clone());
        let path = game_path(
            &fixture.session,
            &format!("{GATEWAY_STAGE}{GET_LOGIN_INFO_PATH}"),
        );
        for method in ["GET", "POST"] {
            let (status, body) = call(&app, method, &path).await;
            assert_eq!(status, StatusCode::OK, "{method}");
            let reply: LoginInfoReply = serde_json::from_slice(&body).expect("login info");
            let list = reply.login_info_list;
            assert_eq!(list.characters, vec![character_summary(world_id, &created)]);
            assert_eq!(list.characters[0].name, NAME);
            let slots: u32 = fixture
                .state
                .config
                .worlds
                .iter()
                .map(|world| world.max_characters_per_account)
                .sum();
            assert_eq!(list.max_channel_characters, i64::from(slots));
            assert_eq!(list.name_reservations[0].display_name, NAME);
            assert_eq!(
                list.worlds,
                world_models(&fixture.state).await.expect("worlds")
            );
            assert!(
                list.worlds.iter().all(|world| !world.is_full),
                "the director reported free slots"
            );
        }
        let (status, _) = call(
            &app,
            "POST",
            &game_path(OTHER_KEY, &format!("{GATEWAY_STAGE}{GET_LOGIN_INFO_PATH}")),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn the_select_screen_shows_the_saved_level() {
        const SAVED_LEVEL: u32 = 7;
        let character = Character {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            name: NAME.to_owned(),
            appearance: crate::look::tests::saved_appearance(),
            created_at_unix: 0,
            level: SAVED_LEVEL,
            published_look: None,
        };
        let summary = character_summary(Uuid::new_v4(), &character);
        let published = summary.published_data.expect("published");
        let text = crate::look::tests::published_text(&published);
        let level = format!(r#""currentLevel":{SAVED_LEVEL}}}"#);
        assert!(text.ends_with(level.as_bytes()), "the saved level is shown");
    }

    #[tokio::test]
    async fn world_list_shows_selectable_worlds() {
        let fixture = fixture().await;
        let app = game_router(fixture.state.clone());
        let path = game_path(
            &fixture.session,
            &format!("{GATEWAY_STAGE}{LIST_WORLDS_PATH}"),
        );
        let (status, body) = call(&app, "POST", &path).await;
        assert_eq!(status, StatusCode::OK);
        let reply: ListWorldsReply = serde_json::from_slice(&body).expect("worlds");
        let world = &reply.list_worlds_response_body.worlds[0];
        let configured = &fixture.state.config.worlds[0];
        assert_eq!(world.world_id, configured.id.to_string());
        assert_eq!(
            world.max_account_characters,
            i64::from(configured.max_characters_per_account)
        );
        assert_eq!(world.world_status, WORLD_STATUS_ACTIVE);
        assert_eq!(world.public_status_code, PUBLIC_STATUS_OPEN);
        assert_eq!(world.world_type, WORLD_TYPE_OPEN_WORLD);
        assert_eq!(world.transfer_to_region, NO_TRANSFER_REGION);
        assert!(!world.is_full);
        fixture
            .state
            .queue
            .set_free_slots(configured.id, 0, FIXTURE_SLOTS_TTL_MS)
            .await
            .expect("no slot");
        let (_, body) = call(&app, "POST", &path).await;
        let reply: ListWorldsReply = serde_json::from_slice(&body).expect("worlds");
        assert!(
            reply.list_worlds_response_body.worlds[0].is_full,
            "its players are at its capacity"
        );
    }

    #[tokio::test]
    async fn a_banned_account_loses_its_session_and_its_queue() {
        let fixture = fixture().await;
        let app = game_router(fixture.state.clone());
        let path = game_path(
            &fixture.session,
            &format!("{GATEWAY_STAGE}{LIST_WORLDS_PATH}"),
        );
        let (status, _) = call(&app, "POST", &path).await;
        assert_eq!(status, StatusCode::OK);
        fixture.state.characters.ban(fixture.account);
        let (status, body) = call(&app, "POST", &path).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::FORBIDDEN, "banned")
        );
        assert_eq!(
            fixture
                .state
                .game_sessions
                .resolve(&fixture.session, CLAIM_SECONDS)
                .await
                .expect("read"),
            None,
            "the session key is gone"
        );
        let queue = game_path(
            &fixture.session,
            &format!("{GATEWAY_STAGE}{QUEUE_LOGIN_PATH}/ticket"),
        );
        let (status, _) = call(&app, "POST", &queue).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no queue refresh");
    }

    #[tokio::test]
    async fn unknown_game_routes_get_404() {
        let fixture = fixture().await;
        let app = game_router(fixture.state.clone());
        let (status, body) = call(&app, "GET", &game_path(&fixture.session, "/prod/other")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_route")
        );
    }

    #[tokio::test]
    async fn characters_are_created_once_per_name() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id;
        let app = game_router(fixture.state.clone());
        let path = character_path(&fixture.session, &world.to_string(), CHARACTERS_SEGMENT);
        let (status, body) = post(&app, &path, &create_body(NAME)).await;
        assert_eq!(status, StatusCode::OK);
        let reply: CreateCharacterReply = serde_json::from_slice(&body).expect("reply");
        assert_eq!(reply.character.normalized_name, NAME.to_lowercase());
        assert_eq!(reply.character.world_id, world.to_string());
        let stored = fixture
            .state
            .characters
            .characters(world, fixture.account)
            .await
            .expect("stored");
        assert_eq!(stored[0].appearance["Name"], NAME);
        assert!(stored[0].appearance.get("SteamAuthTicket").is_none());
        let (status, body) = post(&app, &path, &create_body(&NAME.to_uppercase())).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::CONFLICT, "character_name_taken")
        );
        let (status, body) = post(&app, &path, &create_body("A1")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::BAD_REQUEST, "invalid_character_name")
        );
        let (status, body) = post(&app, &path, "{}").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::BAD_REQUEST, "bad_request")
        );
    }

    #[tokio::test]
    async fn a_character_is_deleted_by_its_own_account_only() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id;
        let app = game_router(fixture.state.clone());
        let own = character_of(&fixture.state, fixture.account, NAME).await;
        let (_, other_account) = new_session(&fixture.state).await;
        let foreign = character_of(&fixture.state, other_account, "Other").await;
        let delete_path = |character: Uuid| {
            game_path(
                &fixture.session,
                &format!("{GATEWAY_STAGE}{CHARACTER_PATH}/{character}"),
            )
        };
        let (status, body) = call(&app, "DELETE", &delete_path(foreign)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_character")
        );
        let (status, body) = call(&app, "DELETE", &delete_path(own)).await;
        assert_eq!((status, &body[..]), (StatusCode::OK, &b"{}"[..]));
        let left = fixture
            .state
            .characters
            .characters(world, fixture.account)
            .await
            .expect("list");
        assert!(left.is_empty());
        assert!(
            !fixture
                .state
                .characters
                .name_taken(NAME)
                .await
                .expect("free")
        );
        let (status, _) = call(&app, "DELETE", &delete_path(own)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "deleted once");
        let (status, _) = call(
            &app,
            "DELETE",
            &game_path(
                OTHER_KEY,
                &format!("{GATEWAY_STAGE}{CHARACTER_PATH}/{foreign}"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_character_limit_is_kept() {
        let fixture = fixture().await;
        let world = &fixture.state.config.worlds[0];
        assert_eq!(world.max_characters_per_account, TEST_CHARACTER_LIMIT);
        let app = game_router(fixture.state.clone());
        let path = character_path(&fixture.session, &world.id.to_string(), CHARACTERS_SEGMENT);
        for name in [NAME, SECOND_NAME] {
            let (status, _) = post(&app, &path, &create_body(name)).await;
            assert_eq!(status, StatusCode::OK, "{name}");
        }
        let (status, body) = post(&app, &path, &create_body(THIRD_NAME)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::CONFLICT, "character_limit")
        );
    }

    #[tokio::test]
    async fn character_routes_check_session_world_and_size() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id.to_string();
        let app = game_router(fixture.state.clone());
        for segment in [CHARACTERS_SEGMENT, VALIDATOR_SEGMENT] {
            let path = character_path(OTHER_KEY, &world, segment);
            let (status, _) = post(&app, &path, &create_body(NAME)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{segment}");
            for unknown in [Uuid::new_v4().to_string(), "not-a-uuid".to_owned()] {
                let path = character_path(&fixture.session, &unknown, segment);
                let (status, body) = post(&app, &path, &validate_body(NAME)).await;
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (StatusCode::NOT_FOUND, "unknown_world"),
                    "{segment} {unknown}"
                );
            }
        }
        let path = character_path(&fixture.session, &world, CHARACTERS_SEGMENT);
        let huge = "x".repeat(GAME_BODY_LIMIT_BYTES + 1);
        let (status, _) = post(&app, &path, &huge).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_session_is_checked_before_the_body_is_read() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id.to_string();
        let app = game_router(fixture.state.clone());
        let huge = "x".repeat(GAME_BODY_LIMIT_BYTES + 1);
        let paths = [
            character_path(OTHER_KEY, &world, CHARACTERS_SEGMENT),
            character_path(OTHER_KEY, &world, VALIDATOR_SEGMENT),
            queue_path(OTHER_KEY, None),
        ];
        for path in paths {
            let (status, body) = post(&app, &path, &huge).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (StatusCode::UNAUTHORIZED, "no_session"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn validator_reports_free_taken_and_bad_names() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id.to_string();
        let app = game_router(fixture.state.clone());
        let validator = character_path(&fixture.session, &world, VALIDATOR_SEGMENT);
        let (status, body) = post(&app, &validator, &validate_body(NAME)).await;
        assert_eq!((status, &body[..]), (StatusCode::OK, &b"{}"[..]));
        let create = character_path(&fixture.session, &world, CHARACTERS_SEGMENT);
        post(&app, &create, &create_body(NAME)).await;
        let (status, body) = post(&app, &validator, &validate_body(NAME)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::CONFLICT, "character_name_taken")
        );
        let (status, _) = post(&app, &validator, &validate_body("no")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, &validator, "{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    fn queue_path(session: &str, ticket: Option<&str>) -> String {
        let rest = match ticket {
            Some(ticket) => format!("{GATEWAY_STAGE}{QUEUE_LOGIN_PATH}/{ticket}"),
            None => format!("{GATEWAY_STAGE}{QUEUE_LOGIN_PATH}"),
        };
        game_path(session, &rest)
    }

    fn queue_body(world: Uuid, character: Uuid) -> String {
        serde_json::json!({"LoginQueueRequest": {
            "WorldId": world.to_string(),
            "CharacterId": character.to_string(),
            "SteamAuthTicket": STEAM_TICKET,
        }})
        .to_string()
    }

    /// A queue join with an empty `WorldId`, as the client sends right after it creates a
    /// character.
    fn empty_world_queue_body(character: Uuid) -> String {
        serde_json::json!({"LoginQueueRequest": {
            "WorldId": "",
            "CharacterId": character.to_string(),
        }})
        .to_string()
    }

    async fn character_of(state: &TestState, account: Uuid, name: &str) -> Uuid {
        character_in(state, state.config.worlds[0].id, account, name).await
    }

    async fn character_in(state: &TestState, world: Uuid, account: Uuid, name: &str) -> Uuid {
        let new = NewCharacter {
            id: Uuid::new_v4(),
            account_id: account,
            name: name.to_owned(),
            appearance: serde_json::json!({}),
        };
        state
            .characters
            .create(world, &new)
            .await
            .expect("character")
            .id
    }

    fn reply(body: &Bytes) -> nw_protocol::LoginQueueResponse {
        serde_json::from_slice::<LoginQueueReply>(body)
            .expect("queue reply")
            .login_queue_response
    }

    #[tokio::test]
    async fn an_admitted_join_carries_a_signed_token() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id;
        let character = character_of(&fixture.state, fixture.account, NAME).await;
        let app = game_router(fixture.state.clone());
        let (status, body) = post(
            &app,
            &queue_path(&fixture.session, None),
            &queue_body(world, character),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let response = reply(&body);
        let token = response.token.expect("token");
        assert_eq!(token.character_id, Some(character.to_string()));
        assert_eq!(
            token.rep_address,
            Some(fixture.state.config.worlds[0].rep_address.clone())
        );
        let ticket_id = token.ticket_id.clone().expect("ticket");
        assert!(ticket_id.starts_with(&world.simple().to_string()));
        let records = fixture
            .state
            .queue
            .update(
                &QueueSettings::for_world(&fixture.state.config.worlds[0]),
                fixture.account,
                QueueAction::Refresh {
                    ticket_id: ticket_id.clone(),
                },
                QueueTime::StoreClock,
            )
            .await
            .expect("refresh");
        let Some(crate::queue::QueueStatus::Admitted(record)) = records else {
            panic!("expected an admitted entry");
        };
        let signature = token.signature.expect("signature");
        assert!(verify_entry(
            &fixture.state.signing_key,
            &record.claims(),
            &signature
        ));
    }

    #[tokio::test]
    async fn each_world_admits_to_its_own_rep_gateway() {
        let fixture = fixture_with(valid_two_worlds()).await;
        let app = game_router(fixture.state.clone());
        let worlds = fixture.state.config.worlds.clone();
        for (world, name) in worlds.iter().zip([NAME, SECOND_NAME]) {
            let character = character_in(&fixture.state, world.id, fixture.account, name).await;
            // An empty `WorldId` (as right after character creation) still finds the world.
            for body in [
                queue_body(world.id, character),
                empty_world_queue_body(character),
            ] {
                let (status, body) = post(&app, &queue_path(&fixture.session, None), &body).await;
                assert_eq!(status, StatusCode::OK);
                let token = reply(&body).token.expect("token");
                assert_eq!(token.world_id, Some(world.id.to_string()));
                assert_eq!(token.rep_address, Some(world.rep_address.clone()));
            }
        }
    }

    #[tokio::test]
    async fn a_full_world_keeps_others_waiting() {
        let mut config = valid();
        config.worlds[0].capacity = ONE_SLOT;
        let fixture = fixture_with(config).await;
        let world = fixture.state.config.worlds[0].id;
        let app = game_router(fixture.state.clone());
        let first = character_of(&fixture.state, fixture.account, NAME).await;
        let (status, _) = post(
            &app,
            &queue_path(&fixture.session, None),
            &queue_body(world, first),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (other_session, other_account) = new_session(&fixture.state).await;
        let second = character_of(&fixture.state, other_account, SECOND_NAME).await;
        let (status, body) = post(
            &app,
            &queue_path(&other_session, None),
            &queue_body(world, second),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let waiting = reply(&body);
        assert_eq!(waiting.position, Some(FIRST_IN_LINE));
        assert!(waiting.token.is_none());
        let ticket = waiting.ticket_id.expect("ticket");
        let (status, body) = post(&app, &queue_path(&other_session, Some(&ticket)), "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply(&body).ticket_id, Some(ticket.clone()));
        let (status, body) = post(&app, &queue_path(&fixture.session, Some(&ticket)), "").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_ticket")
        );
    }

    #[tokio::test]
    async fn queue_joins_need_an_own_character() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id;
        let app = game_router(fixture.state.clone());
        let (_, other_account) = new_session(&fixture.state).await;
        let foreign = character_of(&fixture.state, other_account, NAME).await;
        let join = queue_path(&fixture.session, None);
        for character in [foreign, Uuid::new_v4()] {
            let (status, body) = post(&app, &join, &queue_body(world, character)).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (StatusCode::NOT_FOUND, "unknown_character")
            );
        }
        let (status, body) = post(&app, &join, &queue_body(Uuid::new_v4(), foreign)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_world")
        );
        let (status, _) = post(&app, &join, "not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(
            &app,
            &queue_path(OTHER_KEY, None),
            &queue_body(world, foreign),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = post(&app, &queue_path(&fixture.session, Some("no-world")), "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_join_without_world_id_uses_the_world_of_the_character() {
        let fixture = fixture().await;
        let world = fixture.state.config.worlds[0].id;
        let character = character_of(&fixture.state, fixture.account, NAME).await;
        let (_, other_account) = new_session(&fixture.state).await;
        let foreign = character_of(&fixture.state, other_account, SECOND_NAME).await;
        let app = game_router(fixture.state.clone());
        let join = queue_path(&fixture.session, None);
        let empty_world = empty_world_queue_body(character);
        let no_world = serde_json::json!({"LoginQueueRequest": {
            "CharacterId": character.to_string(),
        }})
        .to_string();
        for body in [empty_world, no_world] {
            let (status, reply_body) = post(&app, &join, &body).await;
            assert_eq!(status, StatusCode::OK);
            let token = reply(&reply_body).token.expect("token");
            assert_eq!(token.character_id, Some(character.to_string()));
            assert_eq!(token.world_id, Some(world.to_string()));
        }
        let (status, body) = post(&app, &join, &empty_world_queue_body(foreign)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::NOT_FOUND, "unknown_character")
        );
    }

    #[tokio::test]
    async fn queue_requests_are_rate_limited_per_account() {
        let mut config = valid();
        config.worlds[0].queue_requests_per_minute = FEW_REQUESTS;
        let fixture = fixture_with(config).await;
        let world = fixture.state.config.worlds[0].id;
        let character = character_of(&fixture.state, fixture.account, NAME).await;
        let app = game_router(fixture.state.clone());
        let join = queue_path(&fixture.session, None);
        for _ in 0..FEW_REQUESTS {
            let (status, _) = post(&app, &join, &queue_body(world, character)).await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, body) = post(&app, &join, &queue_body(world, character)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        );
    }
}
