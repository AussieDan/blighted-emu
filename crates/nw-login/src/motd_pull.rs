// SPDX-License-Identifier: AGPL-3.0-or-later
//! The message of the day from the community website ([`WebsiteConfig::motd_url`]).
//!
//! The client asks for the message of the day once a minute ([`crate::cms`]). With `[website]`
//! set, `edge-http` asks the website just as often, with `If-None-Match` and the ETag the website
//! gave last. A new document with the retail top-level members replaces the served one. A `304`,
//! an error, a body that is too large or a document of another shape keeps the last good copy
//! (at start, the `[cms]` file). The website opens no connection to this server.

use std::time::Duration;

use reqwest::header::{CONTENT_LENGTH, ETAG, IF_NONE_MATCH};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use tokio::time::MissedTickBehavior;

use crate::cms::{CmsDocument, missing_image};
use crate::config::WebsiteConfig;
use crate::s3::LiveS3Objects;

/// Time between two requests: the client's own poll period of the message of the day.
pub const MOTD_PULL_PERIOD: Duration = Duration::from_secs(60);
/// The largest message of the day this server takes (the retail document is about 42 KiB).
pub const MAX_MOTD_BYTES: usize = 1024 * 1024;
/// The longest wait for one answer of the website.
pub const MOTD_PULL_TIME_LIMIT: Duration = Duration::from_secs(10);

/// The HTTP client of the pull, with the user agent `user_agent` (so the website owner finds
/// the pull in the logs).
///
/// # Errors
///
/// Returns the [`reqwest::Error`] when the client cannot start.
pub fn motd_client(user_agent: &str) -> Result<Client, reqwest::Error> {
    Client::builder()
        .user_agent(user_agent)
        .timeout(MOTD_PULL_TIME_LIMIT)
        .build()
}

/// What one request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
    /// The website sent a new document; it is served now.
    Updated,
    /// The website answered `304`, or sent the document that is served already.
    Unchanged,
}

/// Why one request changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// The website did not answer.
    #[error("the website did not answer: {0}")]
    Http(#[from] reqwest::Error),
    /// The website answered with an error status.
    #[error("the website answered {0}")]
    Status(StatusCode),
    /// The body is larger than [`MAX_MOTD_BYTES`].
    #[error("the message of the day is larger than {MAX_MOTD_BYTES} bytes")]
    TooLarge,
    /// The body is no JSON with the retail members, or it names an image (the message of the
    /// day has none).
    #[error("the message of the day is not JSON with the members of the retail document")]
    Shape,
}

/// Gets the message of the day from one website.
#[derive(Debug, Clone)]
pub struct MotdPull {
    http: Client,
    url: String,
    documents: LiveS3Objects,
    etag: Option<String>,
}

impl MotdPull {
    /// A pull from `website` into `documents`, with the client `http` (its user agent names
    /// `edge-http`).
    pub fn new(http: Client, website: &WebsiteConfig, documents: LiveS3Objects) -> Self {
        Self {
            http,
            url: website.motd_url(),
            documents,
            etag: None,
        }
    }

    /// Asks the website once.
    ///
    /// # Errors
    ///
    /// Returns [`PullError`] when the website does not answer, answers an error, or sends a
    /// document this server does not take; the served document stays.
    pub async fn pull_once(&mut self) -> Result<PullOutcome, PullError> {
        let mut request = self.http.get(&self.url);
        if let Some(etag) = &self.etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(PullOutcome::Unchanged);
        }
        if !status.is_success() {
            return Err(PullError::Status(status));
        }
        let announced = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok());
        if announced.is_some_and(|length| length > MAX_MOTD_BYTES) {
            return Err(PullError::TooLarge);
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await?;
        if body.len() > MAX_MOTD_BYTES {
            return Err(PullError::TooLarge);
        }
        let document = CmsDocument::MessageOfTheDay;
        serde_json::from_slice::<Value>(&body)
            .ok()
            .filter(|value| {
                document.has_retail_shape(value)
                    && missing_image(value, &Default::default()).is_none()
            })
            .ok_or(PullError::Shape)?;
        self.etag = etag;
        let key = document.key();
        if self.documents.current().body(&key) == Some(body.as_ref()) {
            return Ok(PullOutcome::Unchanged);
        }
        self.documents.replace(&key, body.to_vec());
        Ok(PullOutcome::Updated)
    }

    /// Asks the website every [`MOTD_PULL_PERIOD`], the first time at once. Logs when a new
    /// document is served and when the pull starts or stops failing. Runs until the task is
    /// dropped.
    pub async fn run(mut self) {
        let mut timer = tokio::time::interval(MOTD_PULL_PERIOD);
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut failing = false;
        loop {
            timer.tick().await;
            match self.pull_once().await {
                Ok(outcome) => {
                    if failing {
                        tracing::info!(url = %self.url, "message of the day pull works again");
                    }
                    failing = false;
                    if outcome == PullOutcome::Updated {
                        tracing::info!(url = %self.url, "new message of the day from the website");
                    }
                }
                Err(error) => {
                    if !failing {
                        tracing::warn!(
                            url = %self.url,
                            %error,
                            "message of the day not pulled; the last good copy stays"
                        );
                    }
                    failing = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;
    use crate::cms::tests::committed_objects;
    use crate::config::SITE_MOTD_PATH;
    use crate::config::tests::valid;

    const SITE_ETAG: &str = "\"0123456789abcdef0123456789abcdef\"";
    const MOTD: &str = r#"{"worlds":{"8d0f4c2e-0000-4000-8000-000000000001":{"announcement":"Patch tonight","warning":""}},"globalAnnouncement":{"title":"Hello","description":"Line one\nLine two"},"worldSets":{"live":{"setName":"Blighted"}}}"#;

    /// What the fake website answers, and the `If-None-Match` values it got.
    #[derive(Debug, Clone)]
    struct Site {
        answer: Arc<Mutex<(StatusCode, String)>>,
        seen: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn motd(State(site): State<Site>, headers: HeaderMap) -> Response {
        let if_none_match = headers
            .get(IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        site.seen.lock().expect("lock").push(if_none_match.clone());
        if if_none_match.as_deref() == Some(SITE_ETAG) {
            return (StatusCode::NOT_MODIFIED, [(ETAG, SITE_ETAG)]).into_response();
        }
        let (status, body) = site.answer.lock().expect("lock").clone();
        let mut response = (status, body).into_response();
        response
            .headers_mut()
            .insert(ETAG, HeaderValue::from_static(SITE_ETAG));
        response
    }

    async fn site(status: StatusCode, body: &str) -> (Site, WebsiteConfig) {
        let site = Site {
            answer: Arc::new(Mutex::new((status, body.to_owned()))),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let app = Router::new()
            .route(SITE_MOTD_PATH, get(motd))
            .with_state(site.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let website = WebsiteConfig {
            site_url: format!("http://{address}"),
        };
        (site, website)
    }

    fn pull_from(website: &WebsiteConfig) -> (MotdPull, LiveS3Objects) {
        crate::install_crypto_provider();
        let documents = committed_objects(&valid()).documents();
        let client = motd_client("blighted-edge-http/test").expect("client");
        let pull = MotdPull::new(client, website, documents.clone());
        (pull, documents)
    }

    fn served(documents: &LiveS3Objects) -> Vec<u8> {
        documents
            .current()
            .body(&CmsDocument::MessageOfTheDay.key())
            .expect("motd")
            .to_vec()
    }

    #[tokio::test]
    async fn a_new_motd_is_served_and_asked_again_with_its_etag() {
        let (site, website) = site(StatusCode::OK, MOTD).await;
        let (mut pull, documents) = pull_from(&website);
        assert_eq!(pull.pull_once().await.expect("pull"), PullOutcome::Updated);
        assert_eq!(served(&documents), MOTD.as_bytes());
        assert_eq!(
            pull.pull_once().await.expect("pull"),
            PullOutcome::Unchanged
        );
        assert_eq!(
            *site.seen.lock().expect("lock"),
            [None, Some(SITE_ETAG.to_owned())]
        );
    }

    #[tokio::test]
    async fn a_bad_answer_keeps_the_last_good_copy() {
        let (_site, website) = site(StatusCode::OK, r#"{"worlds":{}}"#).await;
        let (mut pull, documents) = pull_from(&website);
        let before = served(&documents);
        assert!(matches!(pull.pull_once().await, Err(PullError::Shape)));
        assert_eq!(served(&documents), before);
        let image_motd = MOTD.replace(
            "Patch tonight",
            "https://client.content-service.amazongames.com/images/x.dds",
        );
        let (_site, website) = site(StatusCode::OK, &image_motd).await;
        let (mut pull, _) = pull_from(&website);
        assert!(matches!(pull.pull_once().await, Err(PullError::Shape)));
        let (_site, website) = site(StatusCode::INTERNAL_SERVER_ERROR, "").await;
        let (mut pull, documents) = pull_from(&website);
        assert!(matches!(
            pull.pull_once().await,
            Err(PullError::Status(StatusCode::INTERNAL_SERVER_ERROR))
        ));
        assert_eq!(served(&documents), before);
    }

    #[tokio::test]
    async fn a_too_large_motd_is_refused() {
        let large = format!(
            r#"{{"worlds":{{}},"globalAnnouncement":{{"title":"{}"}},"worldSets":{{}}}}"#,
            "x".repeat(MAX_MOTD_BYTES)
        );
        let (_site, website) = site(StatusCode::OK, &large).await;
        let (mut pull, _) = pull_from(&website);
        assert!(matches!(pull.pull_once().await, Err(PullError::TooLarge)));
    }

    #[tokio::test]
    async fn a_closed_site_is_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let website = WebsiteConfig {
            site_url: format!("http://{address}"),
        };
        let (mut pull, _) = pull_from(&website);
        assert!(matches!(pull.pull_once().await, Err(PullError::Http(_))));
    }
}
