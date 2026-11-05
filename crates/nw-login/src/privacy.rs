// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our privacy settings service on `game.<domain>`: the settings read of the client's
//! `AgsPrivacySettingsServiceEndpointClient` (`FUN_14160ad60`), which the config key
//! `omni.privacy-settings.client-endpoint` points here (retail host
//! `client.agsprivacysettingsservice.amazongames.com`).
//!
//! Observed (`unlock_store_content` capture, call 52): the client posts the names of the
//! settings it wants (`{"settings": [5 names]}`) and retail answers each one with its value, its
//! default, `DEFAULT` status and its allowed values; the player had changed none. This server
//! keeps no privacy settings, so every setting has its retail default: everyone may send a friend
//! request, see the online status, write and talk. No retail call that changes a setting was
//! captured, so this server has no such route.

use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, parse_json};

/// The settings path of a player (retail; `persona` is the `PersonaId` of the login).
const PRIVACY_SETTINGS_PATH: &str = "/players/{persona}/games/new-world/privacy-settings";
/// Largest request body: a list of setting names (retail: 246 bytes).
pub const PRIVACY_BODY_LIMIT_BYTES: usize = 16 * 1024;
/// The status of a setting the player never changed (retail).
const DEFAULT_STATUS: &str = "DEFAULT";
/// The update time of a setting the player never changed (retail).
const NEVER_UPDATED_MS: u64 = 0;

/// One setting the service knows: its alias, its default value and its allowed values.
struct KnownSetting {
    alias: &'static str,
    default_value: &'static str,
    allowed_values: &'static [&'static str],
}

const EVERYONE: &str = "Everyone";
const NO_ONE: &str = "NoOne";
const FRIENDS: &str = "Friends";
const ONLY_ME: &str = "OnlyMe";

/// The settings of the retail reply, in the reply's order (Observed: not the order of the
/// request; one retail reply).
const KNOWN_SETTINGS: [KnownSetting; 5] = [
    KnownSetting {
        alias: "who-can-I-communicate-with-via-public-or-group-text-chat",
        default_value: EVERYONE,
        allowed_values: &[NO_ONE, EVERYONE],
    },
    KnownSetting {
        alias: "who-can-send-me-a-friend-request",
        default_value: EVERYONE,
        allowed_values: &[NO_ONE, EVERYONE],
    },
    KnownSetting {
        alias: "who-can-I-communicate-with-via-text-messages-or-private-text-chat",
        default_value: EVERYONE,
        allowed_values: &[NO_ONE, FRIENDS, EVERYONE],
    },
    KnownSetting {
        alias: "who-can-see-my-online-status",
        default_value: EVERYONE,
        allowed_values: &[ONLY_ME, FRIENDS, EVERYONE],
    },
    KnownSetting {
        alias: "who-can-I-communicate-with-via-voice",
        default_value: EVERYONE,
        allowed_values: &[NO_ONE, FRIENDS, EVERYONE],
    },
];

/// `POST .../privacy-settings`: the names of the settings the client wants.
#[derive(Deserialize)]
struct SettingsRequest {
    settings: Vec<String>,
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
    value: &'static str,
    default_value: &'static str,
    status: &'static str,
    alias: &'static str,
    error_code: Option<()>,
    allowed_values: &'static [&'static str],
    last_update_time_ms: u64,
    last_update_date: Option<()>,
}

impl From<&KnownSetting> for Setting {
    fn from(known: &KnownSetting) -> Self {
        Self {
            value: known.default_value,
            default_value: known.default_value,
            status: DEFAULT_STATUS,
            alias: known.alias,
            error_code: None,
            allowed_values: known.allowed_values,
            last_update_time_ms: NEVER_UPDATED_MS,
            last_update_date: None,
        }
    }
}

/// The privacy settings route, merged into the routes of `game.<domain>`.
pub fn privacy_router() -> Router {
    Router::new()
        .route(PRIVACY_SETTINGS_PATH, post(privacy_settings))
        .layer(DefaultBodyLimit::max(PRIVACY_BODY_LIMIT_BYTES))
}

/// The known settings `body` names, at their defaults. A name the service does not know gets no
/// entry (Inferred: retail's answer to one has no capture).
async fn privacy_settings(body: Bytes) -> Result<Json<SettingsReply>, ApiError> {
    let request: SettingsRequest = parse_json(&body)?;
    let settings = KNOWN_SETTINGS
        .iter()
        .filter(|known| request.settings.iter().any(|name| name == known.alias))
        .map(Setting::from)
        .collect();
    Ok(Json(SettingsReply { settings }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};

    use super::*;
    use crate::test_http;

    /// The retail capture's path (with a test persona), request and reply
    /// (`unlock_store_content`, call 52).
    const RETAIL_PATH: &str = "/players/amzn1.developerPersonaId.00000000-0000-0000-0000-000000000001/games/new-world/privacy-settings";
    const RETAIL_REQUEST: &str = r#"{"settings":["who-can-send-me-a-friend-request","who-can-see-my-online-status","who-can-I-communicate-with-via-text-messages-or-private-text-chat","who-can-I-communicate-with-via-public-or-group-text-chat","who-can-I-communicate-with-via-voice"]}"#;
    const RETAIL_REPLY: &str = r#"{"settings":[{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-public-or-group-text-chat","errorCode":null,"allowedValues":["NoOne","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-send-me-a-friend-request","errorCode":null,"allowedValues":["NoOne","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-text-messages-or-private-text-chat","errorCode":null,"allowedValues":["NoOne","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-see-my-online-status","errorCode":null,"allowedValues":["OnlyMe","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null},{"value":"Everyone","defaultValue":"Everyone","status":"DEFAULT","alias":"who-can-I-communicate-with-via-voice","errorCode":null,"allowedValues":["NoOne","Friends","Everyone"],"lastUpdateTimeMs":0,"lastUpdateDate":null}]}"#;

    async fn post_body(path: &str, body: impl Into<Body>) -> (StatusCode, Bytes) {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(body.into())
            .expect("request");
        test_http::send(&privacy_router(), request).await
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
    async fn a_change_or_another_path_is_no_route() {
        let request = Request::builder()
            .method("PUT")
            .uri(RETAIL_PATH)
            .body(Body::from(RETAIL_REQUEST))
            .expect("request");
        let (status, _) = test_http::send(&privacy_router(), request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let (status, _) = post_body("/players/x/games/other/privacy-settings", RETAIL_REQUEST).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
