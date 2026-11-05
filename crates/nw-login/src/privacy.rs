// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our privacy settings service on `game.<domain>`: the settings read of the client's
//! `AgsPrivacySettingsServiceEndpointClient` (`FUN_14160ad60`), which the config key
//! `omni.privacy-settings.client-endpoint` points here (retail host
//! `client.agsprivacysettingsservice.amazongames.com`).
//!
//! Observed (`unlock_store_content` capture, call 52): the client posts the names of the
//! settings it wants (`{"settings": [5 names]}`) and retail answers each one with its value, its
//! default, `DEFAULT` status and its allowed values; the player had changed none. A setting the
//! player never changed has its retail default: everyone may send a friend request, see the
//! online status, write and talk. The player changes a setting with a `PUT` of the same path
//! (client, no capture); this server stores it per account (global migration 0006), and the
//! social service of each world follows it (`nw_social::privacy`). Settings are per account,
//! not per character: the path names the account (its persona), never a character.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use nw_db::{
    PRIVACY_EVERYONE, PRIVACY_FRIEND_REQUESTS, PRIVACY_FRIENDS, PRIVACY_NO_ONE,
    PRIVACY_ONLINE_STATUS, PRIVACY_ONLY_ME, PRIVACY_PRIVATE_CHAT, PRIVACY_PUBLIC_CHAT,
    PRIVACY_VOICE, PrivacySetting,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, parse_json};
use crate::shop::account_of;
use crate::store::PrivacyStore;

/// The settings path of a player (retail; `persona` is the `PersonaId` of the login).
const PRIVACY_SETTINGS_PATH: &str = "/players/{persona}/games/new-world/privacy-settings";
/// Largest request body: a list of setting names, or of settings to store (retail read: 246
/// bytes).
pub const PRIVACY_BODY_LIMIT_BYTES: usize = 16 * 1024;
/// The status of a setting the player never changed (retail).
const DEFAULT_STATUS: &str = "DEFAULT";
/// The status of a setting the player changed. Inferred: no capture holds a changed setting;
/// the client reads `status` (`FUN_147429df0`) but its other values are Unknown.
const SET_STATUS: &str = "SET";
/// The update time of a setting the player never changed (retail).
const NEVER_UPDATED_MS: u64 = 0;

/// One setting the service knows: its alias, its default value and its allowed values.
struct KnownSetting {
    alias: &'static str,
    default_value: &'static str,
    allowed_values: &'static [&'static str],
}

/// The settings of the retail reply, in the reply's order (Observed: not the order of the
/// request; one retail reply).
const KNOWN_SETTINGS: [KnownSetting; 5] = [
    KnownSetting {
        alias: PRIVACY_PUBLIC_CHAT,
        default_value: PRIVACY_EVERYONE,
        allowed_values: &[PRIVACY_NO_ONE, PRIVACY_EVERYONE],
    },
    KnownSetting {
        alias: PRIVACY_FRIEND_REQUESTS,
        default_value: PRIVACY_EVERYONE,
        allowed_values: &[PRIVACY_NO_ONE, PRIVACY_EVERYONE],
    },
    KnownSetting {
        alias: PRIVACY_PRIVATE_CHAT,
        default_value: PRIVACY_EVERYONE,
        allowed_values: &[PRIVACY_NO_ONE, PRIVACY_FRIENDS, PRIVACY_EVERYONE],
    },
    KnownSetting {
        alias: PRIVACY_ONLINE_STATUS,
        default_value: PRIVACY_EVERYONE,
        allowed_values: &[PRIVACY_ONLY_ME, PRIVACY_FRIENDS, PRIVACY_EVERYONE],
    },
    KnownSetting {
        alias: PRIVACY_VOICE,
        default_value: PRIVACY_EVERYONE,
        allowed_values: &[PRIVACY_NO_ONE, PRIVACY_FRIENDS, PRIVACY_EVERYONE],
    },
];

/// What the privacy routes need.
#[derive(Clone)]
pub struct PrivacyState<P> {
    /// The stored settings.
    pub store: P,
}

/// `POST .../privacy-settings`: the names of the settings the client wants.
#[derive(Deserialize)]
struct SettingsRequest {
    settings: Vec<String>,
}

/// `PUT .../privacy-settings`: the settings to store (`CreateOrUpdatePrivacySettingsRequest`,
/// client `FUN_14742e170`: `{"settings": [{"alias", "value"}]}`).
#[derive(Deserialize)]
struct UpdateRequest {
    settings: Vec<SettingUpdate>,
}

#[derive(Deserialize)]
struct SettingUpdate {
    alias: String,
    value: String,
}

/// The reply: one entry per known setting the request names.
#[derive(Serialize)]
struct SettingsReply {
    settings: Vec<Setting>,
}

/// One setting of the reply (retail member order).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Setting {
    value: String,
    default_value: &'static str,
    status: &'static str,
    alias: &'static str,
    error_code: Option<()>,
    allowed_values: &'static [&'static str],
    last_update_time_ms: u64,
    last_update_date: Option<()>,
}

impl Setting {
    /// `known` with the value the player stored, or its default.
    fn of(known: &KnownSetting, stored: Option<&PrivacySetting>) -> Self {
        Self {
            value: stored
                .map_or(known.default_value, |stored| &stored.value)
                .to_owned(),
            default_value: known.default_value,
            status: if stored.is_some() {
                SET_STATUS
            } else {
                DEFAULT_STATUS
            },
            alias: known.alias,
            error_code: None,
            allowed_values: known.allowed_values,
            last_update_time_ms: stored.map_or(NEVER_UPDATED_MS, |stored| stored.updated_at_ms),
            last_update_date: None,
        }
    }
}

/// The privacy settings routes, merged into the routes of `game.<domain>`: `POST` reads
/// (`GetPrivacySettingsForPlayer`, Observed), `PUT` stores (`CreateOrUpdatePrivacySettings`:
/// the client function `FUN_14742bea0` sends the same path with the AWS SDK method value 3,
/// `PUT`; no capture holds one).
///
/// The persona of the path is the player's account id, as for the store routes. Inferred: the
/// client sends no proof of its account to these routes that this server checks, so whoever
/// names an account's id reads and changes its settings; the id is never shown to other
/// players.
pub fn privacy_router<P: PrivacyStore>(state: PrivacyState<P>) -> Router {
    Router::new()
        .route(
            PRIVACY_SETTINGS_PATH,
            post(privacy_settings::<P>).put(update_privacy_settings::<P>),
        )
        .layer(DefaultBodyLimit::max(PRIVACY_BODY_LIMIT_BYTES))
        .with_state(state)
}

/// The known settings `body` names, with the values the account stored. A name the service does
/// not know gets no entry (Inferred: retail's answer to one has no capture).
async fn privacy_settings<P: PrivacyStore>(
    State(state): State<PrivacyState<P>>,
    Path(persona): Path<String>,
    body: Bytes,
) -> Result<Json<SettingsReply>, ApiError> {
    let request: SettingsRequest = parse_json(&body)?;
    let stored = match account_of(&persona) {
        Some(account) => state
            .store
            .settings(account)
            .await
            .map_err(|_| ApiError::Unavailable)?,
        None => Vec::new(),
    };
    let settings = KNOWN_SETTINGS
        .iter()
        .filter(|known| request.settings.iter().any(|name| name == known.alias))
        .map(|known| {
            let saved = stored.iter().find(|saved| saved.alias == known.alias);
            Setting::of(known, saved)
        })
        .collect();
    Ok(Json(SettingsReply { settings }))
}

/// Stores the settings of `body` and answers them as stored. A persona that names no account,
/// an unknown setting or a value the setting does not allow refuses the whole request (Inferred:
/// retail's answer has no capture).
async fn update_privacy_settings<P: PrivacyStore>(
    State(state): State<PrivacyState<P>>,
    Path(persona): Path<String>,
    body: Bytes,
) -> Result<Json<SettingsReply>, ApiError> {
    let account = account_of(&persona).ok_or(ApiError::BadRequest)?;
    let request: UpdateRequest = parse_json(&body)?;
    let mut changes = Vec::with_capacity(request.settings.len());
    for update in request.settings {
        let known = KNOWN_SETTINGS
            .iter()
            .find(|known| known.alias == update.alias)
            .filter(|known| known.allowed_values.contains(&update.value.as_str()))
            .ok_or(ApiError::BadRequest)?;
        changes.push((known.alias.to_owned(), update.value));
    }
    let stored = state
        .store
        .set(account, &changes)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let settings = stored
        .iter()
        .filter_map(|saved| {
            KNOWN_SETTINGS
                .iter()
                .find(|known| known.alias == saved.alias)
                .map(|known| Setting::of(known, Some(saved)))
        })
        .collect();
    Ok(Json(SettingsReply { settings }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};

    use super::*;
    use crate::memory::MemoryPrivacy;
    use crate::test_http;

    /// The retail capture's path (with a test persona), request and reply
    /// (`unlock_store_content`, call 52).
    const RETAIL_PATH: &str = "/players/amzn1.developerPersonaId.00000000-0000-0000-0000-000000000001/games/new-world/privacy-settings";
    const RETAIL_REQUEST: &str = r#"{"settings":["who-can-send-me-a-friend-request","who-can-see-my-online-status","who-can-I-communicate-with-via-text-messages-or-private-text-chat","who-can-I-communicate-with-via-public-or-group-text-chat","who-can-I-communicate-with-via-voice"]}"#;
    const RETAIL_REPLY: &str = r#"{"settings":[{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-public-or-group-text-chat","errorCode":null,"allowedValues":["NoOne","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-send-me-a-friend-request","errorCode":null,"allowedValues":["NoOne","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-text-messages-or-private-text-chat","errorCode":null,"allowedValues":["NoOne","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-see-my-online-status","errorCode":null,"allowedValues":["OnlyMe","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-voice","errorCode":null,"allowedValues":["NoOne","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null}]}"#;

    fn router() -> (Router, MemoryPrivacy) {
        let store = MemoryPrivacy::default();
        let router = privacy_router(PrivacyState {
            store: store.clone(),
        });
        (router, store)
    }

    async fn send_body(
        router: &Router,
        method: &str,
        path: &str,
        body: impl Into<Body>,
    ) -> (StatusCode, Bytes) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(body.into())
            .expect("request");
        test_http::send(router, request).await
    }

    async fn post_body(path: &str, body: impl Into<Body>) -> (StatusCode, Bytes) {
        send_body(&router().0, "POST", path, body).await
    }

    #[tokio::test]
    async fn the_retail_request_gets_the_retail_reply_byte_for_byte() {
        let (status, body) = post_body(RETAIL_PATH, RETAIL_REQUEST).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(std::str::from_utf8(&body).expect("text"), RETAIL_REPLY);
    }

    #[tokio::test]
    async fn only_the_named_known_settings_are_answered() {
        let (status, body) = post_body(
            RETAIL_PATH,
            r#"{"settings":["who-can-I-communicate-with-via-voice","who-can-fly"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let reply: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let aliases: Vec<&str> = reply["settings"]
            .as_array()
            .expect("settings")
            .iter()
            .map(|setting| setting["alias"].as_str().expect("alias"))
            .collect();
        assert_eq!(aliases, ["who-can-I-communicate-with-via-voice"]);
        let (_, body) = post_body(RETAIL_PATH, r#"{"settings":[]}"#).await;
        assert_eq!(&body[..], br#"{"settings":[]}"#);
    }

    #[tokio::test]
    async fn a_bad_or_too_large_body_is_refused() {
        for body in ["", "{}", r#"{"settings":"who-can-fly"}"#] {
            let (status, _) = post_body(RETAIL_PATH, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        }
        let huge = format!(
            r#"{{"settings":["{}"]}}"#,
            "x".repeat(PRIVACY_BODY_LIMIT_BYTES)
        );
        let (status, _) = post_body(RETAIL_PATH, huge).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn a_stored_setting_is_read_back_per_account() {
        let (router, store) = router();
        let change = format!(
            r#"{{"settings":[{{"alias":"{PRIVACY_FRIEND_REQUESTS}","value":"{PRIVACY_NO_ONE}"}}]}}"#
        );
        let (status, body) = send_body(&router, "PUT", RETAIL_PATH, change).await;
        assert_eq!(status, StatusCode::OK);
        let reply: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(reply["settings"][0]["value"], PRIVACY_NO_ONE);
        assert_eq!(reply["settings"][0]["status"], SET_STATUS);
        let account = account_of(RETAIL_PATH.split('/').nth(2).expect("persona")).expect("id");
        assert_eq!(store.settings(account).await.expect("stored").len(), 1);
        let (_, body) = send_body(&router, "POST", RETAIL_PATH, RETAIL_REQUEST).await;
        let reply: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let friend_requests = reply["settings"]
            .as_array()
            .expect("settings")
            .iter()
            .find(|setting| setting["alias"] == PRIVACY_FRIEND_REQUESTS)
            .expect("asked")
            .clone();
        assert_eq!(friend_requests["value"], PRIVACY_NO_ONE);
        let other = RETAIL_PATH.replace("000000000001", "000000000002");
        let (_, body) = send_body(&router, "POST", &other, RETAIL_REQUEST).await;
        assert_eq!(
            std::str::from_utf8(&body).expect("text"),
            RETAIL_REPLY,
            "another account keeps the defaults"
        );
    }

    #[tokio::test]
    async fn a_bad_change_stores_nothing() {
        let (router, store) = router();
        for body in [
            format!(r#"{{"settings":[{{"alias":"who-can-fly","value":"{PRIVACY_NO_ONE}"}}]}}"#),
            format!(
                r#"{{"settings":[{{"alias":"{PRIVACY_FRIEND_REQUESTS}","value":"{PRIVACY_FRIENDS}"}}]}}"#
            ),
            r#"{"settings":"x"}"#.to_owned(),
        ] {
            let (status, _) = send_body(&router, "PUT", RETAIL_PATH, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        let (status, _) = send_body(
            &router,
            "PUT",
            "/players/nobody/games/new-world/privacy-settings",
            r#"{"settings":[]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a persona with no account");
        let account = account_of(RETAIL_PATH.split('/').nth(2).expect("persona")).expect("id");
        assert!(store.settings(account).await.expect("read").is_empty());
    }

    #[tokio::test]
    async fn another_method_or_path_is_no_route() {
        let request = Request::builder()
            .method("DELETE")
            .uri(RETAIL_PATH)
            .body(Body::from(RETAIL_REQUEST))
            .expect("request");
        let (status, _) = test_http::send(&router().0, request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let (status, _) =
            post_body("/players/x/games/other/privacy-settings", RETAIL_REQUEST).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
