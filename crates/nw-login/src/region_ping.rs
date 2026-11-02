// SPDX-License-Identifier: AGPL-3.0-or-later
//! The region ping (study note `docs/re/2026-11-02-region-ping.md`). The client's
//! `AwsRegionPinger` sends `GET /ping` over plain HTTP, port 80, to
//! `dynamodb.<region>.amazonaws.com` for the region our channel document names. Retail answers
//! `healthy: <host> ` as `text/plain` with the CRC-32 of the body in `x-amz-crc32`. This server
//! answers the same on its plain HTTP listener, so the ping measures the way to this server.

use axum::Router;
use axum::extract::Request;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

use nw_protocol::aws_mirror::is_dynamodb_host;

use crate::error::ApiError;
use crate::host::request_host;

/// The path the client pings.
pub const PING_PATH: &str = "/ping";
/// Start of the reply body; the host and one space follow.
const HEALTHY_PREFIX: &str = "healthy: ";
/// Last byte of the reply body.
const BODY_END: char = ' ';
/// Content type of the reply (retail).
const PING_CONTENT_TYPE: &str = "text/plain";
/// The CRC-32 of the body, in decimal (retail).
const CRC32_HEADER: &str = "x-amz-crc32";

/// The reply body for `host`: `healthy: <host> ` (retail, 42 bytes for `us-east-1`).
pub fn ping_body(host: &str) -> String {
    format!("{HEALTHY_PREFIX}{host}{BODY_END}")
}

/// The plain HTTP routes: `GET /ping` on a DynamoDB host. Any other host gets `421`, any other
/// request `404`.
pub fn region_ping_router() -> Router {
    Router::new().fallback(ping)
}

async fn ping(request: Request) -> Response {
    let Some(host) = request_host(&request)
        .map(str::to_ascii_lowercase)
        .filter(|host| is_dynamodb_host(host))
    else {
        return ApiError::UnknownHost.into_response();
    };
    let is_ping = request.uri().path() == PING_PATH
        && matches!(*request.method(), Method::GET | Method::HEAD);
    if !is_ping {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = ping_body(&host);
    let crc = crc32fast::hash(body.as_bytes()).to_string();
    let mut response = (
        [(CONTENT_TYPE, HeaderValue::from_static(PING_CONTENT_TYPE))],
        body,
    )
        .into_response();
    if let Ok(crc) = HeaderValue::from_str(&crc) {
        response.headers_mut().insert(CRC32_HEADER, crc);
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::HOST;

    use super::*;
    use crate::test_http;

    const PING_HOST: &str = "dynamodb.us-east-1.amazonaws.com";
    /// Retail reply length for `us-east-1` (region ping note).
    const RETAIL_BODY_LEN: usize = 42;

    async fn send(method: &str, uri: &str, host: Option<&str>) -> (StatusCode, Response) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(host) = host {
            request = request.header(HOST, host);
        }
        let response = tower::ServiceExt::oneshot(
            region_ping_router(),
            request.body(Body::empty()).expect("request"),
        )
        .await
        .expect("response");
        (response.status(), response)
    }

    #[test]
    fn the_body_matches_retail() {
        let body = ping_body(PING_HOST);
        assert_eq!(body, "healthy: dynamodb.us-east-1.amazonaws.com ");
        assert_eq!(body.len(), RETAIL_BODY_LEN);
    }

    #[tokio::test]
    async fn a_ping_gets_the_healthy_reply() {
        for (uri, host) in [
            ("/ping", Some("DYNAMODB.us-east-1.amazonaws.com:80")),
            ("http://dynamodb.us-east-1.amazonaws.com/ping", None),
        ] {
            let (status, response) = send("GET", uri, host).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            let headers = response.headers().clone();
            let body = http_body_util::BodyExt::collect(response.into_body())
                .await
                .expect("body")
                .to_bytes();
            assert_eq!(body.as_ref(), ping_body(PING_HOST).as_bytes());
            assert_eq!(headers[CONTENT_TYPE], PING_CONTENT_TYPE);
            assert_eq!(
                headers[CRC32_HEADER],
                crc32fast::hash(&body).to_string().as_str()
            );
        }
    }

    #[tokio::test]
    async fn other_requests_are_refused() {
        let (status, _) = send("GET", "/", Some(PING_HOST)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let request = test_http::request("POST", "/ping", "{}");
        let (status, _) = test_http::send(&region_ping_router(), request).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        for host in ["login.example.org", "kinesis.us-east-1.amazonaws.com"] {
            let (status, _) = send("GET", "/ping", Some(host)).await;
            assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}");
        }
    }
}
