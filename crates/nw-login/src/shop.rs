// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shop and entitlement services (study note `docs/re/2026-11-02-shop-services.md`). The
//! client's `FcsEndpointClient` (`FUN_1416190f0`) calls them at the hosts of the remote config
//! keys `omni.fcs.endpoints.*`, which this server fills with `game.<domain>`
//! ([`crate::remote_config::GAME_HOST_ENDPOINTS`]). Without the keys the client calls Amazon.
//!
//! This server sells nothing and grants no entitlement: the catalog and every store are empty,
//! the entitlement list is empty, and a sync is accepted. The shapes, the `ETag` and the `304`
//! follow the retail replies.

use axum::Router;
use axum::http::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use data_encoding::BASE64;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The route prefix: the player's persona, the game alias and the platform.
const PLAYER_GAME_PREFIX: &str = "/players/{persona}/games/new-world/platforms/{platform}";
/// Currency and country of the prices. Inferred: retail answers in the player's own currency
/// and country; this server has one region, `us-east`.
const CURRENCY_CODE: &str = "USD";
const COUNTRY_CODE: &str = "US";
/// Content type of a reply (retail).
const JSON_CONTENT_TYPE: &str = "application/json";
/// Content type of a `304` (retail).
const NOT_MODIFIED_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// `GET .../catalog?type=MTX`: the offers for sale.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    currency_code: &'static str,
    offers: [(); 0],
    country_code: &'static str,
    etag: String,
}

/// `GET .../stores/<alias>`: the stores with this alias. Retail sends one with its slots;
/// this server has none.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Stores {
    currency_code: &'static str,
    stores: [(); 0],
    country_code: &'static str,
    etag: String,
}

/// `GET .../entitlements`: one page of the player's entitlements. Inferred: the last page leaves
/// out `paginationToken` (no retail last page was captured).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entitlements {
    has_more_results: bool,
    entitlements: [(); 0],
}

/// The ETag of a reply: base64 of the SHA-256 of its body without the ETag. Inferred from the
/// retail form (44 base64 characters).
fn etag_of(body: &[u8]) -> String {
    BASE64.encode(&Sha256::digest(body))
}

/// The shop routes, merged into the routes of `game.<domain>`.
pub fn shop_router() -> Router {
    Router::new()
        .route(&format!("{PLAYER_GAME_PREFIX}/catalog"), get(catalog))
        .route(
            &format!("{PLAYER_GAME_PREFIX}/stores/{{alias}}"),
            get(stores),
        )
        .route(
            &format!("{PLAYER_GAME_PREFIX}/entitlements"),
            get(entitlements),
        )
        .route(
            &format!("{PLAYER_GAME_PREFIX}/entitlements/sync"),
            post(entitlements_sync),
        )
}

/// A reply with an ETag in its body and its header, or `304` when `If-None-Match` names it.
/// `with_etag` builds the reply with the ETag it is given; the ETag is taken over the reply with
/// an empty one.
fn tagged_reply<T: Serialize>(headers: &HeaderMap, with_etag: impl Fn(String) -> T) -> Response {
    let Ok(untagged) = serde_json::to_vec(&with_etag(String::new())) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let etag = etag_of(&untagged);
    let Ok(etag_header) = HeaderValue::from_str(&etag) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let known = headers
        .get(IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == etag.as_bytes());
    if known {
        return (
            StatusCode::NOT_MODIFIED,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static(NOT_MODIFIED_CONTENT_TYPE),
            )],
        )
            .into_response();
    }
    let Ok(body) = serde_json::to_vec(&with_etag(etag)) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE)),
            (ETAG, etag_header),
        ],
        body,
    )
        .into_response()
}

async fn catalog(headers: HeaderMap) -> Response {
    tagged_reply(&headers, |etag| Catalog {
        currency_code: CURRENCY_CODE,
        offers: [],
        country_code: COUNTRY_CODE,
        etag,
    })
}

async fn stores(headers: HeaderMap) -> Response {
    tagged_reply(&headers, |etag| Stores {
        currency_code: CURRENCY_CODE,
        stores: [],
        country_code: COUNTRY_CODE,
        etag,
    })
}

async fn entitlements() -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE))],
        axum::Json(Entitlements {
            has_more_results: false,
            entitlements: [],
        }),
    )
        .into_response()
}

/// `POST .../entitlements/sync`: retail answers `202` with no body.
async fn entitlements_sync() -> StatusCode {
    StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, Bytes};
    use axum::http::Request;
    use serde_json::Value;

    use super::*;

    const PLAYER: &str = "/players/amzn1.developerPersonaId.abc/games/new-world/platforms/steam";
    /// Length of a retail ETag: base64 of 32 bytes.
    const ETAG_LEN: usize = 44;

    async fn send(method: &str, path: &str, etag: Option<&str>) -> (StatusCode, HeaderMap, Bytes) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = tower::ServiceExt::oneshot(
            shop_router(),
            request.body(Body::empty()).expect("request"),
        )
        .await
        .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        (status, headers, body)
    }

    #[tokio::test]
    async fn the_catalog_and_stores_are_empty_with_a_working_etag() {
        for (path, list) in [
            (format!("{PLAYER}/catalog?type=MTX"), "offers"),
            (format!("{PLAYER}/stores/Featured"), "stores"),
            (format!("{PLAYER}/stores/Special1"), "stores"),
        ] {
            let (status, headers, body) = send("GET", &path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let value: Value = serde_json::from_slice(&body).expect("json");
            assert_eq!(value[list], Value::Array(Vec::new()), "{path}");
            assert_eq!(value["currencyCode"], CURRENCY_CODE);
            assert_eq!(value["countryCode"], COUNTRY_CODE);
            let etag = headers[ETAG].to_str().expect("etag").to_owned();
            assert_eq!(etag.len(), ETAG_LEN);
            assert_eq!(value["etag"], etag.as_str());
            let (status, headers, body) = send("GET", &path, Some(&etag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}");
            assert_eq!(headers[CONTENT_TYPE], NOT_MODIFIED_CONTENT_TYPE);
            assert!(body.is_empty());
            let (status, _, _) = send("GET", &path, Some("other")).await;
            assert_eq!(status, StatusCode::OK, "{path}");
        }
    }

    #[tokio::test]
    async fn the_entitlements_are_empty_and_a_sync_is_accepted() {
        let (status, headers, body) = send("GET", &format!("{PLAYER}/entitlements"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], JSON_CONTENT_TYPE);
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            value,
            serde_json::json!({ "hasMoreResults": false, "entitlements": [] })
        );
        let (status, _, body) = send("POST", &format!("{PLAYER}/entitlements/sync"), None).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn other_games_and_paths_are_not_served() {
        for path in [
            "/players/p/games/other-game/platforms/steam/catalog",
            "/players/p/games/new-world/platforms/steam/purchases",
        ] {
            let (status, _, _) = send("GET", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }
}
