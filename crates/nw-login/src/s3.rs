// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixed objects answered as an S3 bucket answers them: `GET /<key>` gives the body with its
//! ETag, `304` when `If-None-Match` names that ETag, and the S3 `NoSuchKey` reply for any other
//! key. The remote config sets ([`crate::remote_config`]) and the news documents
//! ([`crate::cms`]) are served this way. This server checks no request signature.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use data_encoding::{BASE64, HEXLOWER, HEXUPPER};
use md5::{Digest, Md5};

use crate::analytics::xml_text;
use crate::error::ApiError;

/// Content type of an object (retail reply).
pub(crate) const OBJECT_CONTENT_TYPE: &str = "binary/octet-stream";
/// Content type of an S3 error.
pub(crate) const ERROR_CONTENT_TYPE: &str = "application/xml";
/// S3 request id headers.
pub(crate) const REQUEST_ID_HEADER: &str = "x-amz-request-id";
pub(crate) const HOST_ID_HEADER: &str = "x-amz-id-2";
/// Random bytes of an S3 request id (16 upper-case hex digits) and host id (base64).
pub(crate) const REQUEST_ID_BYTES: usize = 8;
const HOST_ID_BYTES: usize = 56;
/// `If-None-Match` value that matches every object.
pub(crate) const ANY_ETAG: &str = "*";
const ETAG_LIST_SEPARATOR: char = ',';
/// Between the key and the leading `/` of the request path.
const PATH_SEPARATOR: char = '/';
const PERCENT: u8 = b'%';
const HEX_RADIX: u32 = 16;
const PERCENT_ESCAPE_LEN: usize = 3;

/// One object: its body and its ETag.
#[derive(Debug, Clone, PartialEq, Eq)]
struct S3Object {
    body: Vec<u8>,
    /// `"<MD5 hex>"`, as S3 gives for an object stored in one part.
    etag: String,
}

impl S3Object {
    fn new(body: Vec<u8>) -> Self {
        let etag = format!("\"{}\"", HEXLOWER.encode(&Md5::digest(&body)));
        Self { body, etag }
    }
}

/// The objects of one bucket, by key.
#[derive(Debug, Clone, Default)]
pub struct S3Objects {
    objects: Arc<HashMap<String, S3Object>>,
}

impl S3Objects {
    /// The objects with these keys and bodies.
    pub fn new(bodies: HashMap<String, Vec<u8>>) -> Self {
        let objects = bodies
            .into_iter()
            .map(|(key, body)| (key, S3Object::new(body)))
            .collect();
        Self {
            objects: Arc::new(objects),
        }
    }

    /// The body of an object, if it exists.
    pub fn body(&self, key: &str) -> Option<&[u8]> {
        self.objects.get(key).map(|object| object.body.as_slice())
    }

    /// Every key, sorted.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.objects.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// These objects with the object `key` set to `body` (a new ETag).
    fn with(&self, key: &str, body: Vec<u8>) -> Self {
        let mut objects = (*self.objects).clone();
        objects.insert(key.to_owned(), S3Object::new(body));
        Self {
            objects: Arc::new(objects),
        }
    }
}

/// Objects that may change while they are served (the message of the day that `edge-http`
/// gets from the website, [`crate::cms`]). Each request is answered from the objects of that
/// moment.
#[derive(Debug, Clone, Default)]
pub struct LiveS3Objects {
    current: Arc<RwLock<S3Objects>>,
}

impl LiveS3Objects {
    /// Live objects that start as `objects`.
    pub fn new(objects: S3Objects) -> Self {
        Self {
            current: Arc::new(RwLock::new(objects)),
        }
    }

    /// The objects now.
    pub fn current(&self) -> S3Objects {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the object `key` to `body`.
    pub fn replace(&self, key: &str, body: Vec<u8>) {
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
        *current = current.with(key, body);
    }
}

/// The bucket route: `GET /<key>` (and `HEAD`).
pub fn s3_router(objects: S3Objects) -> Router {
    Router::new()
        .route("/{*key}", get(get_object))
        .with_state(objects)
}

/// The bucket route of live objects: each request reads the objects of that moment.
pub fn live_s3_router(objects: LiveS3Objects) -> Router {
    Router::new()
        .route("/{*key}", get(get_live_object))
        .with_state(objects)
}

async fn get_live_object(
    State(objects): State<LiveS3Objects>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    get_object(State(objects.current()), uri, headers).await
}

async fn get_object(State(objects): State<S3Objects>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(ids) = RequestIds::new() else {
        return ApiError::Unavailable.into_response();
    };
    let raw = uri.path().trim_start_matches(PATH_SEPARATOR);
    let key = percent_decode(raw).unwrap_or_else(|| raw.to_owned());
    let mut response = match objects.objects.get(&key) {
        Some(object) => object_reply(object, &headers),
        None => no_such_key(&key, &ids),
    };
    ids.add_headers(&mut response);
    response
}

/// `200` with the body, or `304` when `If-None-Match` names its ETag; both carry the ETag.
fn object_reply(object: &S3Object, headers: &HeaderMap) -> Response {
    let mut response = if etag_matches(headers, &object.etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(CONTENT_TYPE, HeaderValue::from_static(OBJECT_CONTENT_TYPE))],
            Body::from(object.body.clone()),
        )
            .into_response()
    };
    if let Ok(etag) = HeaderValue::from_str(&object.etag) {
        response.headers_mut().insert(ETAG, etag);
    }
    response
}

/// Whether `If-None-Match` names `etag` (or `*`).
fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(ETAG_LIST_SEPARATOR))
        .map(str::trim)
        .any(|candidate| candidate == etag || candidate == ANY_ETAG)
}

/// The S3 `NoSuchKey` reply.
fn no_such_key(key: &str, ids: &RequestIds) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchKey</Code>\
         <Message>The specified key does not exist.</Message><Key>{}</Key>\
         <RequestId>{}</RequestId><HostId>{}</HostId></Error>",
        xml_text(key),
        ids.request_id,
        ids.host_id
    );
    (
        StatusCode::NOT_FOUND,
        [(CONTENT_TYPE, HeaderValue::from_static(ERROR_CONTENT_TYPE))],
        body,
    )
        .into_response()
}

/// Fresh S3 request ids: 16 upper-case hex digits and a base64 host id.
struct RequestIds {
    request_id: String,
    host_id: String,
}

impl RequestIds {
    /// New random ids, or none when the system random source fails.
    fn new() -> Option<Self> {
        let mut request_id = [0_u8; REQUEST_ID_BYTES];
        let mut host_id = [0_u8; HOST_ID_BYTES];
        getrandom::fill(&mut request_id).ok()?;
        getrandom::fill(&mut host_id).ok()?;
        Some(Self {
            request_id: HEXUPPER.encode(&request_id),
            host_id: BASE64.encode(&host_id),
        })
    }

    fn add_headers(&self, response: &mut Response) {
        for (name, value) in [
            (REQUEST_ID_HEADER, &self.request_id),
            (HOST_ID_HEADER, &self.host_id),
        ] {
            if let Ok(value) = HeaderValue::from_str(value) {
                response.headers_mut().insert(name, value);
            }
        }
    }
}

/// `%XX` escapes decoded, or none when an escape or the result is not valid.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == PERCENT {
            let escape = text.get(index + 1..index + PERCENT_ESCAPE_LEN)?;
            out.push(u8::from_str_radix(escape, HEX_RADIX).ok()?);
            index += PERCENT_ESCAPE_LEN;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decode("a%3Bb%3ab").as_deref(), Some("a;b:b"));
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        for bad in ["%", "%3", "%zz", "%FF"] {
            assert_eq!(percent_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn objects_keep_their_bodies_and_sorted_keys() {
        let objects = S3Objects::new(HashMap::from([
            ("b/c".to_owned(), b"two".to_vec()),
            ("a".to_owned(), b"one".to_vec()),
        ]));
        assert_eq!(objects.keys(), ["a", "b/c"]);
        assert_eq!(objects.body("b/c"), Some(b"two".as_slice()));
        assert_eq!(objects.body("c"), None);
    }

    #[test]
    fn a_replaced_live_object_gets_a_new_body_and_etag() {
        let etag = |objects: &S3Objects| objects.objects.get("motd").map(|o| o.etag.clone());
        let live = LiveS3Objects::new(S3Objects::new(HashMap::from([
            ("motd".to_owned(), b"old".to_vec()),
            ("other".to_owned(), b"kept".to_vec()),
        ])));
        let before = live.current();
        live.replace("motd", b"new".to_vec());
        let after = live.current();
        assert_eq!(after.body("motd"), Some(b"new".as_slice()));
        assert_eq!(after.body("other"), Some(b"kept".as_slice()));
        assert_ne!(etag(&after), etag(&before));
        assert_eq!(
            before.body("motd"),
            Some(b"old".as_slice()),
            "a copy stays as it was"
        );
    }
}
