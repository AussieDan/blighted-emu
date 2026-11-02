// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our own news, answered as the client's news host [`CMS_HOST`] and its image host
//! [`CMS_IMAGE_HOST`] (study note `docs/re/2026-11-02-cms-news-hosts.md`).
//!
//! After login the client reads three documents from the news host and polls them again with
//! `If-None-Match`: the message of the day with the world announcements, the news stories and
//! the menu tiles. Their image URLs name the image host. Retail serves all of them from S3
//! behind CloudFront, so they are answered as S3 objects ([`crate::s3`]). The `[cms] directory`
//! holds one subfolder per host, named after it; every file in a subfolder is served at its path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use axum::Router;
use nw_protocol::aws_mirror::{CMS_HOST, CMS_IMAGE_HOST};
use serde_json::Value;

use crate::config::EdgeConfig;
use crate::remote_config::PRODUCT_ID;
use crate::s3::{S3Objects, s3_router};

/// Between the parts of an object key.
const KEY_SEPARATOR: &str = "/";
/// Start of an image URL the client fetches from the image host.
const HTTPS_SCHEME: &str = "https://";

/// The three documents the client reads from the news host, and the member each must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmsDocument {
    /// `motd/worlds_<product>.json` (cvar `g_worldsCMSKey`): `worlds`, `worldSets` and
    /// `globalAnnouncement` objects.
    MessageOfTheDay,
    /// `newsstories/<product>/metadata.json` (cvar `g_worldsCMSNewsStoriesKey`): a
    /// `newsStories` array.
    NewsStories,
    /// `marketingtiles/<product>/metadata.json` (cvar `g_worldsCMSMarketingKey`): a `tileData`
    /// array.
    MarketingTiles,
}

impl CmsDocument {
    /// Every document.
    pub const ALL: [Self; 3] = [
        Self::MessageOfTheDay,
        Self::NewsStories,
        Self::MarketingTiles,
    ];

    /// The object key the client asks for.
    pub fn key(self) -> String {
        match self {
            Self::MessageOfTheDay => format!("motd/worlds_{PRODUCT_ID}.json"),
            Self::NewsStories => format!("newsstories/{PRODUCT_ID}/metadata.json"),
            Self::MarketingTiles => format!("marketingtiles/{PRODUCT_ID}/metadata.json"),
        }
    }

    /// Whether `document` has the top-level members of the retail document.
    fn has_retail_shape(self, document: &Value) -> bool {
        match self {
            Self::MessageOfTheDay => ["worlds", "worldSets", "globalAnnouncement"]
                .iter()
                .all(|member| document.get(member).is_some_and(Value::is_object)),
            Self::NewsStories => document.get("newsStories").is_some_and(Value::is_array),
            Self::MarketingTiles => document.get("tileData").is_some_and(Value::is_array),
        }
    }
}

/// The news files could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum CmsError {
    /// A file or folder cannot be read.
    #[error("cannot read {}: {source}", .path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The folder holds a file or folder that this server does not serve.
    #[error("news file {} is no document or image this server serves", .0.display())]
    Unexpected(PathBuf),
    /// A document the client asks for has no file.
    #[error("news document {key} is missing: no file {}", .path.display())]
    Missing {
        /// The key.
        key: String,
        /// The file it needs.
        path: PathBuf,
    },
    /// A document is not JSON with the retail top-level members.
    #[error("news document {key} is not JSON with the members of the retail document")]
    Shape {
        /// The key.
        key: String,
    },
    /// A document names an image on the image host that has no file.
    #[error("news document {key} names {url}, which has no file in the image folder")]
    MissingImage {
        /// The document key.
        key: String,
        /// The image URL.
        url: String,
    },
}

/// The loaded news documents and images, one bucket per host.
#[derive(Debug, Clone)]
pub struct CmsObjects {
    news: S3Objects,
    images: S3Objects,
}

/// The routes of the two news hosts.
pub struct CmsRouters {
    /// The documents, on [`CMS_HOST`].
    pub news: Router,
    /// The images, on [`CMS_IMAGE_HOST`].
    pub images: Router,
}

impl CmsObjects {
    /// Loads `<directory>/<CMS_HOST>/...` and `<directory>/<CMS_IMAGE_HOST>/...`. The news
    /// folder must hold exactly the [`CmsDocument`] files, each with its retail members; the
    /// image folder may be absent. Every image URL in a document that names the image host
    /// needs its file.
    pub fn load(config: &EdgeConfig) -> Result<Self, CmsError> {
        let root = &config.cms.directory;
        let news_root = root.join(CMS_HOST);
        let image_root = root.join(CMS_IMAGE_HOST);
        for entry in read_dir(root)? {
            if entry != news_root && entry != image_root {
                return Err(CmsError::Unexpected(entry));
            }
        }
        let mut news = files_by_key(&news_root)?;
        let images = if image_root.exists() {
            files_by_key(&image_root)?
        } else {
            HashMap::new()
        };
        let image_keys: HashSet<&str> = images.keys().map(String::as_str).collect();
        let mut documents = HashMap::new();
        for document in CmsDocument::ALL {
            let key = document.key();
            let path = news_root.join(&key);
            let body = news.remove(&key).ok_or_else(|| CmsError::Missing {
                key: key.clone(),
                path,
            })?;
            let value: Value = serde_json::from_slice(&body)
                .ok()
                .filter(|value| document.has_retail_shape(value))
                .ok_or_else(|| CmsError::Shape { key: key.clone() })?;
            if let Some(url) = missing_image(&value, &image_keys) {
                return Err(CmsError::MissingImage { key, url });
            }
            documents.insert(key, body);
        }
        if let Some(extra) = news.keys().next() {
            return Err(CmsError::Unexpected(news_root.join(extra)));
        }
        Ok(Self {
            news: S3Objects::new(documents),
            images: S3Objects::new(images),
        })
    }

    /// The routes of both hosts.
    pub fn routers(self) -> CmsRouters {
        CmsRouters {
            news: s3_router(self.news),
            images: s3_router(self.images),
        }
    }
}

/// The first string in `value` that is an image-host URL with no key in `images`.
fn missing_image(value: &Value, images: &HashSet<&str>) -> Option<String> {
    let prefix = format!("{HTTPS_SCHEME}{CMS_IMAGE_HOST}{KEY_SEPARATOR}");
    match value {
        Value::String(text) => text
            .strip_prefix(&prefix)
            .filter(|key| !images.contains(key))
            .map(|_| text.clone()),
        Value::Array(items) => items.iter().find_map(|item| missing_image(item, images)),
        Value::Object(members) => members
            .values()
            .find_map(|member| missing_image(member, images)),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn read_dir(path: &Path) -> Result<Vec<PathBuf>, CmsError> {
    let io_error = |source| CmsError::Io {
        path: path.to_path_buf(),
        source,
    };
    std::fs::read_dir(path)
        .map_err(io_error)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(io_error))
        .collect()
}

/// Every file under `root`, by its path below `root` with `/` between the parts.
fn files_by_key(root: &Path) -> Result<HashMap<String, Vec<u8>>, CmsError> {
    let mut files = HashMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for path in read_dir(&folder)? {
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            let key = path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| {
                    relative
                        .iter()
                        .map(|part| part.to_str())
                        .collect::<Option<Vec<_>>>()
                })
                .map(|parts| parts.join(KEY_SEPARATOR))
                .ok_or_else(|| CmsError::Unexpected(path.clone()))?;
            let body = std::fs::read(&path).map_err(|source| CmsError::Io {
                path: path.clone(),
                source,
            })?;
            files.insert(key, body);
        }
    }
    Ok(files)
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::body::Body;
    use axum::http::header::{ETAG, IF_NONE_MATCH};
    use axum::http::{Request, StatusCode};

    use super::*;
    use crate::config::tests::valid;
    use crate::test_http;
    use nw_protocol::launcher_api::FORBIDDEN_DOMAINS;

    /// The tracked news folder.
    fn committed_directory() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/cms")
    }

    /// The tracked news files, loaded for `config`.
    pub(crate) fn committed_objects(config: &EdgeConfig) -> CmsObjects {
        let mut config = config.clone();
        config.cms.directory = committed_directory();
        CmsObjects::load(&config).expect("tracked news files")
    }

    /// A copy of the tracked news folder and a configuration that points at it.
    fn copied() -> (tempfile::TempDir, EdgeConfig) {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = committed_directory().join(CMS_HOST);
        for (key, body) in files_by_key(&source).expect("tracked files") {
            let path = dir.path().join(CMS_HOST).join(&key);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
            std::fs::write(path, body).expect("write");
        }
        let mut config = valid();
        config.cms.directory = dir.path().to_path_buf();
        (dir, config)
    }

    fn write(config: &EdgeConfig, host: &str, key: &str, body: &[u8]) {
        let path = config.cms.directory.join(host).join(key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn tracked_files_hold_the_three_documents_and_no_amazon_text() {
        let objects = committed_objects(&valid());
        let keys: Vec<String> = CmsDocument::ALL.iter().map(|doc| doc.key()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(objects.news.keys(), sorted);
        assert!(objects.images.keys().is_empty());
        for key in &keys {
            let text = std::str::from_utf8(objects.news.body(key).expect("body")).expect("utf-8");
            for domain in FORBIDDEN_DOMAINS {
                assert!(!text.contains(domain), "{key} names {domain}");
            }
        }
    }

    #[test]
    fn the_keys_are_the_ones_the_client_asks_for() {
        assert_eq!(
            CmsDocument::MessageOfTheDay.key(),
            "motd/worlds_STEAM_APP_ID.1063730.json"
        );
        assert_eq!(
            CmsDocument::NewsStories.key(),
            "newsstories/STEAM_APP_ID.1063730/metadata.json"
        );
        assert_eq!(
            CmsDocument::MarketingTiles.key(),
            "marketingtiles/STEAM_APP_ID.1063730/metadata.json"
        );
    }

    #[test]
    fn a_missing_or_wrong_document_is_an_error() {
        let (_dir, config) = copied();
        let key = CmsDocument::NewsStories.key();
        let path = config.cms.directory.join(CMS_HOST).join(&key);
        std::fs::remove_file(&path).expect("remove");
        assert!(matches!(
            CmsObjects::load(&config),
            Err(CmsError::Missing { key: found, .. }) if found == key
        ));
        for body in [&b"not json"[..], b"{\"newsStories\": {}}", b"[]"] {
            write(&config, CMS_HOST, &key, body);
            assert!(matches!(
                CmsObjects::load(&config),
                Err(CmsError::Shape { key: found }) if found == key
            ));
        }
    }

    #[test]
    fn the_folders_hold_only_served_files() {
        let (_dir, config) = copied();
        write(&config, CMS_HOST, "motd/other.json", b"{}");
        assert!(matches!(
            CmsObjects::load(&config),
            Err(CmsError::Unexpected(_))
        ));
        let (_dir, config) = copied();
        write(&config, "d2c74t4zimux3r.cloudfront.net", "x.json", b"{}");
        assert!(matches!(
            CmsObjects::load(&config),
            Err(CmsError::Unexpected(_))
        ));
    }

    #[test]
    fn each_image_url_needs_its_file() {
        let (_dir, config) = copied();
        let image_key = "images/ours/tile.dds";
        let url = format!("https://{CMS_IMAGE_HOST}/{image_key}");
        let tiles = serde_json::json!({ "tileData": [{ "imgKey": url, "title": "Ours" }] });
        write(
            &config,
            CMS_HOST,
            &CmsDocument::MarketingTiles.key(),
            tiles.to_string().as_bytes(),
        );
        assert!(matches!(
            CmsObjects::load(&config),
            Err(CmsError::MissingImage { url: found, .. }) if found == url
        ));
        write(&config, CMS_IMAGE_HOST, image_key, b"DDS image");
        let objects = CmsObjects::load(&config).expect("load");
        assert_eq!(objects.images.keys(), [image_key]);
        assert_eq!(
            objects.images.body(image_key),
            Some(b"DDS image".as_slice())
        );
    }

    async fn get(router: &Router, path: &str, etag: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().method("GET").uri(path);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = tower::ServiceExt::oneshot(
            router.clone(),
            request.body(Body::empty()).expect("request"),
        )
        .await
        .expect("response");
        let status = response.status();
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        (status, etag)
    }

    #[tokio::test]
    async fn documents_are_served_with_their_etag() {
        let routers = committed_objects(&valid()).routers();
        for document in CmsDocument::ALL {
            let path = format!("/{}", document.key());
            let (status, etag) = get(&routers.news, &path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let (status, _) = get(&routers.news, &path, Some(&etag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}");
        }
        let (status, _) = get(&routers.news, "/motd/other.json", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&routers.images, "/images/none.dds", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let request =
            test_http::request("PUT", &format!("/{}", CmsDocument::NewsStories.key()), "{}");
        let (status, _) = test_http::send(&routers.news, request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }
}
