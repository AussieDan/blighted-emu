// SPDX-License-Identifier: AGPL-3.0-or-later
//! The HTTPS part of the test client (M3 spec section 8): the launcher login, then the game
//! routes up to the login queue's signed token, as the game calls them.

use std::net::SocketAddr;
use std::time::Duration;

use nw_login::game::SESSION_PREFIX;
use nw_protocol::game_api::{
    CHARACTERS_SEGMENT, CreateCharacterReply, CreateCharacterRequest, CreateCharacterRequestBody,
    GATEWAY_STAGE, GET_LOGIN_INFO_PATH, LoginInfoReply, QUEUE_LOGIN_PATH, WORLDS_PATH,
};
use nw_protocol::launcher_api::{
    LOGIN_PATH, LauncherSession, PLAY_PATH, PasswordCredentials, PlayReply, REGISTER_PATH,
};
use nw_protocol::{LoginQueueReply, LoginQueueRequest, LoginQueueRequestBody, LoginQueueToken};
use nw_reflect::LoginToken;
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// Character creation settings the server stores as given.
pub const CHARACTER_CREATION_PARAMS: &str = "{}";
/// Shortest wait between queue refreshes.
pub const MIN_REFRESH: Duration = Duration::from_secs(1);
/// Most queue refreshes before the walk gives up.
pub const MAX_REFRESHES: usize = 120;

/// Where and as whom the test client logs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkSettings {
    /// `https://login.<domain>`.
    pub login_url: String,
    /// `https://game.<domain>`.
    pub game_url: String,
    /// Account name; the walk registers it when it is new.
    pub account: String,
    /// Account password.
    pub password: String,
    /// Name of the character the walk creates when the account has none.
    pub character_name: String,
    /// The creation settings of that character ([`CHARACTER_CREATION_PARAMS`]: none).
    pub creation_params: String,
}

/// A failed HTTPS step.
#[derive(Debug, Error)]
pub enum WalkError {
    /// The server could not be reached or the reply is not the expected JSON.
    #[error("{step}: {source}")]
    Http {
        /// Step name.
        step: &'static str,
        /// Cause.
        #[source]
        source: reqwest::Error,
    },
    /// The server refused a step.
    #[error("{step}: HTTP {status}: {body}")]
    Refused {
        /// Step name.
        step: &'static str,
        /// HTTP status.
        status: u16,
        /// Reply body.
        body: String,
    },
    /// The login info lists no world.
    #[error("the server lists no world")]
    NoWorld,
    /// The queue gave no token in time.
    #[error("no queue token after {MAX_REFRESHES} refreshes")]
    NoToken,
    /// The token's `RepAddress` is not `IP:port`.
    #[error("token RepAddress {0:?} is not IP:port")]
    BadRepAddress(String),
}

/// The admitted queue entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Admission {
    /// The signed token.
    pub token: LoginQueueToken,
    /// Where the gateway listens.
    pub rep_address: SocketAddr,
}

/// Logs in, makes a character when there is none, and waits in the login queue until the token
/// arrives. `report` gets one line per step.
///
/// # Errors
///
/// Returns [`WalkError`] for the first step that fails.
pub async fn walk(
    client: &Client,
    settings: &WalkSettings,
    mut report: impl FnMut(String),
) -> Result<Admission, WalkError> {
    let credentials = PasswordCredentials {
        username: settings.account.clone(),
        password: settings.password.clone(),
    };
    let register_url = format!("{}{REGISTER_PATH}", settings.login_url);
    let session: LauncherSession =
        match post(client, "register", &register_url, None, &credentials).await {
            Err(WalkError::Refused { status, .. }) if status == StatusCode::CONFLICT.as_u16() => {
                let login_url = format!("{}{LOGIN_PATH}", settings.login_url);
                post(client, "login", &login_url, None, &credentials).await?
            }
            other => other?,
        };
    report(format!("account {} ready", settings.account));
    let play_url = format!("{}{PLAY_PATH}", settings.login_url);
    let play: PlayReply = post(
        client,
        "play",
        &play_url,
        Some(&session.launcher_token),
        &(),
    )
    .await?;
    let base = format!(
        "{}{SESSION_PREFIX}/{}{GATEWAY_STAGE}",
        settings.game_url, play.game_session_key
    );
    let info: LoginInfoReply = send(
        "login info",
        client.get(format!("{base}{GET_LOGIN_INFO_PATH}")),
    )
    .await?;
    let list = info.login_info_list;
    let world = list
        .worlds
        .first()
        .ok_or(WalkError::NoWorld)?
        .world_id
        .clone();
    let existing = list
        .characters
        .iter()
        .find(|character| character.world_id == world)
        .cloned();
    let character = match existing {
        Some(character) => character,
        None => {
            let body = CreateCharacterRequestBody {
                create_character_request: Some(CreateCharacterRequest {
                    name: Some(settings.character_name.clone()),
                    character_creation_params: Some(settings.creation_params.clone()),
                    ..CreateCharacterRequest::default()
                }),
            };
            let url = format!("{base}{WORLDS_PATH}/{world}{CHARACTERS_SEGMENT}");
            let created: CreateCharacterReply =
                post(client, "create character", &url, None, &body).await?;
            created.character
        }
    };
    report(format!("character {} in world {world}", character.name));
    let join = LoginQueueRequestBody {
        login_queue_request: Some(LoginQueueRequest {
            world_id: Some(world),
            character_id: Some(character.character_id),
            ..LoginQueueRequest::default()
        }),
    };
    let queue_url = format!("{base}{QUEUE_LOGIN_PATH}");
    let mut reply: LoginQueueReply = post(client, "queue", &queue_url, None, &join).await?;
    for _ in 0..MAX_REFRESHES {
        let response = reply.login_queue_response;
        if let Some(token) = response.token {
            let text = token.rep_address.clone().unwrap_or_default();
            let rep_address = text.parse().map_err(|_| WalkError::BadRepAddress(text))?;
            report(format!("queue admitted; gateway {rep_address}"));
            return Ok(Admission { token, rep_address });
        }
        let ticket = response.ticket_id.unwrap_or_default();
        report(format!(
            "queue position {}",
            response.position.unwrap_or_default()
        ));
        let wait = response
            .refresh_interval
            .and_then(|seconds| u64::try_from(seconds).ok())
            .map_or(MIN_REFRESH, Duration::from_secs)
            .max(MIN_REFRESH);
        tokio::time::sleep(wait).await;
        let refresh_url = format!("{queue_url}/{ticket}");
        reply = post(client, "queue refresh", &refresh_url, None, &()).await?;
    }
    Err(WalkError::NoToken)
}

async fn post<T: DeserializeOwned>(
    client: &Client,
    step: &'static str,
    url: &str,
    bearer: Option<&str>,
    body: &impl Serialize,
) -> Result<T, WalkError> {
    let mut request = client.post(url).json(body);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    send(step, request).await
}

async fn send<T: DeserializeOwned>(
    step: &'static str,
    request: reqwest::RequestBuilder,
) -> Result<T, WalkError> {
    let http = |source| WalkError::Http { step, source };
    let response = request.send().await.map_err(http)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(WalkError::Refused {
            step,
            status: status.as_u16(),
            body,
        });
    }
    response.json().await.map_err(http)
}

/// The V3 view of a queue token. Absent JSON values become empty text or 0.
pub fn login_token(token: &LoginQueueToken) -> LoginToken {
    let text = |value: &Option<String>| value.clone().unwrap_or_default().into_bytes();
    let number = |value: Option<i64>| {
        value
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default()
    };
    // Unix seconds fit u32 until 2106; the client's struct field is 32 bits.
    let seconds = |value: Option<f64>| value.map_or(0, |value| value as u32);
    LoginToken {
        signature: text(&token.signature),
        rep_address: text(&token.rep_address),
        world_id: text(&token.world_id),
        character_id: text(&token.character_id),
        persona_id: text(&token.persona_id),
        steam_app_id: number(token.steam_app_id),
        steam_user_id: text(&token.steam_user_id),
        channel_id: text(&token.channel_id),
        ticket_id: text(&token.ticket_id),
        location_group_id: text(&token.location_group_id),
        location_id: text(&token.location_id),
        generate_time: seconds(token.generate_time),
        issue_time: seconds(token.issue_time),
        account_age: number(token.account_age),
        client_capabilities: text(&token.client_capabilities),
        token_version: number(token.token_version),
        owner_flags: 0,
        host_hash: text(&token.host_hash),
        account_is_locked: token.account_is_locked.map(u8::from),
        jwt_claims: text(&token.jwt_claims),
        jwt_strings: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKET: &str = "ticket";
    const STEAM_APP_ID: i64 = 1_063_730;
    const TOKEN_VERSION: i64 = 10;
    const GENERATE_TIME: f64 = 1_782_067_831.0;
    const GENERATE_SECONDS: u32 = 1_782_067_831;
    const NEGATIVE: i64 = -1;

    #[test]
    fn a_queue_token_maps_to_the_v3_fields() {
        let token = LoginQueueToken {
            ticket_id: Some(TICKET.to_owned()),
            signature: Some("abc".to_owned()),
            rep_address: Some("127.0.0.1:33435".to_owned()),
            steam_app_id: Some(STEAM_APP_ID),
            token_version: Some(TOKEN_VERSION),
            generate_time: Some(GENERATE_TIME),
            account_is_locked: Some(false),
            account_age: Some(NEGATIVE),
            ..LoginQueueToken::default()
        };
        let mapped = login_token(&token);
        assert_eq!(mapped.ticket_id, TICKET.as_bytes());
        assert_eq!(mapped.signature, b"abc");
        assert_eq!(mapped.rep_address, b"127.0.0.1:33435");
        assert_eq!(i64::from(mapped.steam_app_id), STEAM_APP_ID);
        assert_eq!(i64::from(mapped.token_version), TOKEN_VERSION);
        assert_eq!(mapped.generate_time, GENERATE_SECONDS);
        assert_eq!(mapped.account_is_locked, Some(0));
        assert_eq!(mapped.account_age, 0);
        assert!(mapped.persona_id.is_empty());
    }
}
