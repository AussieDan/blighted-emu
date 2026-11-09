// SPDX-License-Identifier: GPL-3.0-or-later
//! The server news: the site's launcher feed, named by the server at
//! [`nw_protocol::launcher_api::NEWS_SOURCE_PATH`]. The last feed and its scaled pictures are kept
//! in the settings folder, so the window shows them at once on start; a worker thread refreshes
//! them. Login and play never wait for the news.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, FixedOffset, Local, TimeDelta, Utc};
use eframe::egui::ColorImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits, RgbImage};
use nw_protocol::launcher_api::is_forbidden_host;
use reqwest::Url;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::api::{ApiError, LauncherApi};

/// Time limit of each news call: the source, the feed and each picture.
pub const NEWS_TIMEOUT: Duration = Duration::from_secs(5);
/// The feed is not fetched again within this time (the site sends `max-age=60`).
pub const FEED_MIN_AGE: Duration = Duration::from_secs(60);
/// Time between refreshes while the window is open.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(600);
/// Number of posts shown.
pub const SHOWN_POSTS: usize = 5;
/// A downtime post younger than this many hours is pinned at the top.
const PIN_HOURS: i64 = 24;
/// Largest feed and largest picture read.
const MAX_FEED_BYTES: u64 = 1024 * 1024;
const MAX_PICTURE_BYTES: u64 = 4 * 1024 * 1024;
/// Largest side of a picture that is decoded (the site sends at most 1600 px), and the most
/// memory one decode may take.
const MAX_SOURCE_SIDE: u32 = 4096;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
/// Width a picture is scaled down to, and the JPEG quality of the cached copy.
pub const PICTURE_WIDTH: u32 = 960;
const PICTURE_QUALITY: u8 = 85;
/// Cache file of the last feed, in the settings folder.
pub const NEWS_FILE: &str = "news.json";
/// Cache folder of the scaled pictures, in the settings folder.
pub const PICTURES_DIR: &str = "news-pictures";
const PICTURE_EXTENSION: &str = "jpg";
/// Suffix of the feed file while it is written.
const PARTIAL_SUFFIX: &str = ".partial";
/// Date of a post and of the cache, for example `7 November 2026`.
const DATE_FORMAT: &str = "%-d %B %Y";
/// Schemes of a feed or picture address; plain http only for a test server on this computer.
const HTTPS_SCHEME: &str = "https";
const HTTP_SCHEME: &str = "http";
const LOOPBACK_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// A news call that failed. The window shows each as offline.
#[derive(Debug, thiserror::Error)]
pub enum NewsError {
    /// The server did not name its feed.
    #[error("news source: {0}")]
    Source(#[from] ApiError),
    /// The address is not https (or http to this computer), or not valid.
    #[error("refused news address: {0}")]
    BadUrl(String),
    /// The address is an Amazon or CloudFront host.
    #[error("forbidden news host: {0}")]
    Forbidden(String),
    /// The feed or picture host could not be reached.
    #[error("news host unreachable: {0}")]
    Unreachable(#[source] reqwest::Error),
    /// The host answered with an error status.
    #[error("news host answered {0}")]
    Status(u16),
    /// The answer is larger than allowed.
    #[error("news answer too large")]
    TooLarge,
    /// The feed is not the expected JSON.
    #[error("news feed is not valid: {0}")]
    BadFeed(#[from] serde_json::Error),
    /// The picture is not a JPEG.
    #[error("news picture is not a JPEG")]
    NotJpeg,
    /// The picture could not be decoded or encoded.
    #[error("news picture: {0}")]
    Picture(#[from] image::ImageError),
    /// A read failed.
    #[error("news read: {0}")]
    Io(#[from] std::io::Error),
}

/// What a post is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostKind {
    /// General news.
    News,
    /// Notes of a new build.
    PatchNotes,
    /// Work in progress.
    DevUpdate,
    /// A planned downtime.
    Downtime,
    /// A kind this launcher does not know.
    #[serde(other)]
    Other,
}

impl PostKind {
    /// The badge text.
    pub fn label(self) -> &'static str {
        match self {
            Self::News => "News",
            Self::PatchNotes => "Patch notes",
            Self::DevUpdate => "Dev update",
            Self::Downtime => "Downtime",
            Self::Other => "Post",
        }
    }
}

/// One post of the feed. The feed's `bodyMarkdown` is not read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    /// The site's post id.
    pub id: i64,
    /// What the post is about.
    pub kind: PostKind,
    /// Title.
    pub title: String,
    /// Body, sanitized by the site ([`crate::news_html`]).
    pub body_html: String,
    /// Absolute address of a JPEG of at most 1600 px.
    pub picture_url: Option<String>,
    /// When the post was made.
    pub created_at: DateTime<FixedOffset>,
}

/// The feed: the posts, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Feed {
    /// Newest first.
    pub posts: Vec<Post>,
}

/// The feed in `bytes`.
pub fn parse_feed(bytes: &[u8]) -> Result<Feed, NewsError> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Whether `post` is a downtime post younger than [`PIN_HOURS`] at `now`.
pub fn is_pinned(post: &Post, now: DateTime<Utc>) -> bool {
    post.kind == PostKind::Downtime
        && now.signed_duration_since(post.created_at) < TimeDelta::hours(PIN_HOURS)
}

/// The posts shown at `now`: pinned downtime posts first, then the others newest first, at most
/// [`SHOWN_POSTS`].
pub fn shown_posts(feed: &Feed, now: DateTime<Utc>) -> Vec<&Post> {
    let (mut shown, others): (Vec<&Post>, Vec<&Post>) =
        feed.posts.iter().partition(|post| is_pinned(post, now));
    shown.extend(others);
    shown.truncate(SHOWN_POSTS);
    shown
}

/// The local date of a post.
pub fn post_date(time: DateTime<FixedOffset>) -> String {
    time.with_timezone(&Local).format(DATE_FORMAT).to_string()
}

/// The local date of a file time.
pub fn file_date(time: SystemTime) -> String {
    DateTime::<Local>::from(time)
        .format(DATE_FORMAT)
        .to_string()
}

/// A feed or picture address the launcher may read: https, or http to this computer, with no
/// user name and not an Amazon or CloudFront host.
pub fn checked_url(text: &str) -> Result<Url, NewsError> {
    let refused = || NewsError::BadUrl(text.to_owned());
    let url = Url::parse(text).map_err(|_| refused())?;
    let host = url.host_str().ok_or_else(refused)?;
    let scheme_allowed = match url.scheme() {
        HTTPS_SCHEME => true,
        HTTP_SCHEME => LOOPBACK_HOSTS.contains(&host),
        _ => false,
    };
    if !scheme_allowed || !url.username().is_empty() || url.password().is_some() {
        return Err(refused());
    }
    if is_forbidden_host(host) {
        return Err(NewsError::Forbidden(host.to_owned()));
    }
    Ok(url)
}

/// The client of the news calls: no redirects (a redirect could lead past [`checked_url`]) and
/// the news time limit. Needs the process crypto provider ([`crate::install_crypto_provider`]).
fn news_client() -> Result<Client, NewsError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(NEWS_TIMEOUT)
        .build()
        .map_err(NewsError::Unreachable)
}

/// The body at `url`, at most `limit` bytes.
fn fetch(client: &Client, url: Url, limit: u64) -> Result<Vec<u8>, NewsError> {
    let response = client.get(url).send().map_err(NewsError::Unreachable)?;
    let status = response.status();
    if !status.is_success() {
        return Err(NewsError::Status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(NewsError::TooLarge);
    }
    let mut body = Vec::new();
    response.take(limit + 1).read_to_end(&mut body)?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > limit {
        return Err(NewsError::TooLarge);
    }
    Ok(body)
}

/// A JPEG decoded and scaled down to at most [`PICTURE_WIDTH`]; any other format is refused.
pub fn scaled_picture(jpeg: &[u8]) -> Result<RgbImage, NewsError> {
    if image::guess_format(jpeg).ok() != Some(ImageFormat::Jpeg) {
        return Err(NewsError::NotJpeg);
    }
    let mut reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let picture = reader.decode()?;
    let picture = if picture.width() > PICTURE_WIDTH {
        picture.resize(PICTURE_WIDTH, u32::MAX, FilterType::Triangle)
    } else {
        picture
    };
    Ok(picture.to_rgb8())
}

/// The egui image of a picture.
pub fn color_image(picture: &RgbImage) -> ColorImage {
    let size = [picture.width(), picture.height()].map(|side| side as usize);
    ColorImage::from_rgb(size, picture.as_raw())
}

fn encode_jpeg(picture: &RgbImage) -> Result<Vec<u8>, NewsError> {
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, PICTURE_QUALITY).encode_image(picture)?;
    Ok(bytes)
}

/// The cached feed and when it was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFeed {
    /// The feed.
    pub feed: Feed,
    /// When it was saved.
    pub saved: SystemTime,
}

/// The news cache in the settings folder: [`NEWS_FILE`] (the last feed JSON as read) and
/// [`PICTURES_DIR`]`/<post id>.jpg` (the scaled pictures).
#[derive(Debug, Clone)]
pub struct NewsCache {
    dir: PathBuf,
}

impl NewsCache {
    /// The cache in folder `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn feed_path(&self) -> PathBuf {
        self.dir.join(NEWS_FILE)
    }

    fn pictures_dir(&self) -> PathBuf {
        self.dir.join(PICTURES_DIR)
    }

    fn picture_path(&self, id: i64) -> PathBuf {
        self.pictures_dir()
            .join(format!("{id}.{PICTURE_EXTENSION}"))
    }

    /// The cached feed; `None` when there is none or it cannot be read.
    pub fn load_feed(&self) -> Option<CachedFeed> {
        let path = self.feed_path();
        let feed = parse_feed(&std::fs::read(&path).ok()?).ok()?;
        let saved = std::fs::metadata(&path).ok()?.modified().ok()?;
        Some(CachedFeed { feed, saved })
    }

    /// Saves the feed JSON `raw`; a reader never sees a half-written file.
    pub fn save_feed(&self, raw: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let partial = self.dir.join(format!("{NEWS_FILE}{PARTIAL_SUFFIX}"));
        std::fs::write(&partial, raw)?;
        std::fs::rename(partial, self.feed_path())
    }

    /// Saves the scaled picture of post `id`.
    pub fn save_picture(&self, id: i64, jpeg: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(self.pictures_dir())?;
        std::fs::write(self.picture_path(id), jpeg)
    }

    /// The cached picture of post `id`.
    pub fn load_picture(&self, id: i64) -> Option<RgbImage> {
        scaled_picture(&std::fs::read(self.picture_path(id)).ok()?).ok()
    }

    /// Deletes each cached picture whose post id is not in `keep`. Other files stay.
    pub fn prune_pictures(&self, keep: &HashSet<i64>) -> std::io::Result<()> {
        let entries = match std::fs::read_dir(self.pictures_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            if picture_id(&path).is_some_and(|id| !keep.contains(&id)) {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// Deletes the cached feed and every cached picture.
    pub fn clear(&self) -> std::io::Result<()> {
        if let Err(error) = std::fs::remove_file(self.feed_path())
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error);
        }
        self.prune_pictures(&HashSet::new())
    }
}

/// The post id of a cached picture file name (`<id>.jpg`).
fn picture_id(path: &Path) -> Option<i64> {
    if path.extension()? != PICTURE_EXTENSION {
        return None;
    }
    path.file_stem()?.to_str()?.parse().ok()
}

/// A message from a news worker.
pub enum NewsUpdate {
    /// A fresh feed; it is saved in the cache.
    Feed(Feed),
    /// The server has no news; the cache is deleted.
    NoNews,
    /// The server or the feed could not be read, or the feed is not valid.
    Offline,
    /// The scaled picture of a post.
    Picture(i64, RgbImage),
}

/// The ids of the posts with a picture.
fn posts_with_pictures(feed: &Feed) -> HashSet<i64> {
    feed.posts
        .iter()
        .filter(|post| post.picture_url.is_some())
        .map(|post| post.id)
        .collect()
}

/// The fresh feed of the server of `api`, saved in `cache`; `None` when the server has no news.
fn fetch_feed(
    api: &LauncherApi,
    client: &Client,
    cache: &NewsCache,
) -> Result<Option<Feed>, NewsError> {
    let Some(news_url) = api.news_source(NEWS_TIMEOUT)? else {
        return Ok(None);
    };
    let raw = fetch(client, checked_url(&news_url)?, MAX_FEED_BYTES)?;
    let feed = parse_feed(&raw)?;
    // The fresh feed is shown even when it cannot be cached; the next start then shows the
    // older cache until it refreshes.
    let _ = cache.save_feed(&raw);
    Ok(Some(feed))
}

/// Downloads, checks, scales and caches the picture of post `id`.
fn download_picture(
    client: &Client,
    cache: &NewsCache,
    id: i64,
    url: &str,
) -> Result<RgbImage, NewsError> {
    let jpeg = fetch(client, checked_url(url)?, MAX_PICTURE_BYTES)?;
    let picture = scaled_picture(&jpeg)?;
    // A picture that cannot be cached is still shown; it is downloaded again next time.
    let _ = cache.save_picture(id, &encode_jpeg(&picture)?);
    Ok(picture)
}

/// The picture of `post`: the cached one when `previous` had the same picture address, else a
/// fresh download. `None` when the post has none or it cannot be read.
fn post_picture(
    client: &Client,
    cache: &NewsCache,
    previous: Option<&Feed>,
    post: &Post,
) -> Option<RgbImage> {
    let url = post.picture_url.as_deref()?;
    let unchanged = previous.is_some_and(|feed| {
        feed.posts
            .iter()
            .any(|old| old.id == post.id && old.picture_url.as_deref() == Some(url))
    });
    unchanged
        .then(|| cache.load_picture(post.id))
        .flatten()
        .or_else(|| download_picture(client, cache, post.id, url).ok())
}

/// Refreshes the news of the server of `api`: the feed first, then the pictures of the shown
/// posts. Sends each result to `updates`; stops early when nobody reads them.
pub fn refresh(
    api: &LauncherApi,
    cache: &NewsCache,
    previous: Option<&Feed>,
    updates: &Sender<NewsUpdate>,
) {
    let fetched = news_client().and_then(|client| {
        fetch_feed(api, &client, cache).map(|feed| feed.map(|feed| (client, feed)))
    });
    let (client, feed) = match fetched {
        Ok(Some(found)) => found,
        Ok(None) => {
            // Stale news of an earlier source must not show again on the next start.
            let _ = cache.clear();
            let _ = updates.send(NewsUpdate::NoNews);
            return;
        }
        Err(_) => {
            let _ = updates.send(NewsUpdate::Offline);
            return;
        }
    };
    if updates.send(NewsUpdate::Feed(feed.clone())).is_err() {
        return;
    }
    // A picture that cannot be deleted only takes disk space.
    let _ = cache.prune_pictures(&posts_with_pictures(&feed));
    for post in shown_posts(&feed, Utc::now()) {
        if let Some(picture) = post_picture(&client, cache, previous, post)
            && updates.send(NewsUpdate::Picture(post.id, picture)).is_err()
        {
            return;
        }
    }
}

/// Sends the cached pictures of the shown posts of `feed` to `updates`.
pub fn load_cached_pictures(cache: &NewsCache, feed: &Feed, updates: &Sender<NewsUpdate>) {
    for post in shown_posts(feed, Utc::now()) {
        if post.picture_url.is_some()
            && let Some(picture) = cache.load_picture(post.id)
            && updates.send(NewsUpdate::Picture(post.id, picture)).is_err()
        {
            return;
        }
    }
}

/// A news fetch that was started.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attempt {
    /// The API host it was for; `None` for the cache found at start, which counts for any server.
    server: Option<String>,
    at: Instant,
}

/// The line shown above the posts, when there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The feed shown is fresh, or from the cache before the first refresh.
    Reached,
    /// The last refresh failed.
    Offline,
}

/// The news state of the window: the shown feed, the refresh plan and the running workers.
pub struct NewsState {
    cache: NewsCache,
    feed: Option<Feed>,
    /// When the shown feed was saved.
    saved: Option<SystemTime>,
    reach: Reach,
    last: Option<Attempt>,
    refreshing: Option<Receiver<NewsUpdate>>,
    loading_cache: Option<Receiver<NewsUpdate>>,
}

/// Moves the waiting updates of the worker in `slot` to `updates`; empties the slot when the
/// worker ended.
fn drain(slot: &mut Option<Receiver<NewsUpdate>>, updates: &mut Vec<NewsUpdate>) {
    while let Some(receiver) = slot {
        match receiver.try_recv() {
            Ok(update) => updates.push(update),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => *slot = None,
        }
    }
}

/// Runs `work` in a worker thread; returns the receiver of its updates.
fn spawn(work: impl FnOnce(&Sender<NewsUpdate>) + Send + 'static) -> Receiver<NewsUpdate> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || work(&sender));
    receiver
}

impl NewsState {
    /// The state with the cached feed of `cache` shown at once; its pictures load in a worker. A
    /// cache younger than [`FEED_MIN_AGE`] is not fetched again before that age.
    pub fn open(cache: NewsCache) -> Self {
        let cached = cache.load_feed();
        let last = cached.as_ref().and_then(|cached| {
            let age = SystemTime::now().duration_since(cached.saved).ok()?;
            Some(Attempt {
                server: None,
                at: Instant::now().checked_sub(age)?,
            })
        });
        let loading_cache = cached.as_ref().map(|cached| {
            let (cache, feed) = (cache.clone(), cached.feed.clone());
            spawn(move |updates| load_cached_pictures(&cache, &feed, updates))
        });
        Self {
            saved: cached.as_ref().map(|cached| cached.saved),
            feed: cached.map(|cached| cached.feed),
            cache,
            reach: Reach::Reached,
            last,
            refreshing: None,
            loading_cache,
        }
    }

    /// The feed shown.
    pub fn feed(&self) -> Option<&Feed> {
        self.feed.as_ref()
    }

    /// Whether a worker runs.
    pub fn busy(&self) -> bool {
        self.refreshing.is_some() || self.loading_cache.is_some()
    }

    /// Whether the news of `server` should be fetched at `now`: no refresh runs, and the last one
    /// was for another server or is at least `interval` old.
    pub fn refresh_due(&self, server: &str, now: Instant, interval: Duration) -> bool {
        if self.refreshing.is_some() {
            return false;
        }
        self.last.as_ref().is_none_or(|last| {
            last.server.as_deref().is_some_and(|last| last != server)
                || now.saturating_duration_since(last.at) >= interval
        })
    }

    /// Time until the periodic refresh of `server` is due.
    pub fn next_refresh(&self, server: &str, now: Instant) -> Duration {
        if self.refresh_due(server, now, REFRESH_INTERVAL) {
            return Duration::ZERO;
        }
        self.last.as_ref().map_or(Duration::ZERO, |last| {
            REFRESH_INTERVAL.saturating_sub(now.saturating_duration_since(last.at))
        })
    }

    /// Starts a refresh of the news of the server of `api`.
    pub fn start_refresh(&mut self, api: LauncherApi) {
        self.last = Some(Attempt {
            server: Some(api.host().to_owned()),
            at: Instant::now(),
        });
        let cache = self.cache.clone();
        let previous = self.feed.clone();
        self.refreshing = Some(spawn(move |updates| {
            refresh(&api, &cache, previous.as_ref(), updates);
        }));
    }

    fn apply(&mut self, update: NewsUpdate, pictures: &mut Vec<(i64, RgbImage)>) {
        match update {
            NewsUpdate::Feed(feed) => {
                self.feed = Some(feed);
                self.saved = Some(SystemTime::now());
                self.reach = Reach::Reached;
                // The refresh sends the pictures of the fresh feed; an older cached picture
                // must not arrive after them.
                self.loading_cache = None;
            }
            NewsUpdate::NoNews => {
                self.feed = None;
                self.saved = None;
                self.reach = Reach::Reached;
                self.loading_cache = None;
            }
            NewsUpdate::Offline => self.reach = Reach::Offline,
            NewsUpdate::Picture(id, picture) => pictures.push((id, picture)),
        }
    }

    /// Takes the results of the workers, the cache loader's first; returns the pictures that
    /// arrived, oldest first.
    pub fn poll(&mut self) -> Vec<(i64, RgbImage)> {
        let mut updates = Vec::new();
        for slot in [&mut self.loading_cache, &mut self.refreshing] {
            drain(slot, &mut updates);
        }
        let mut pictures = Vec::new();
        for update in updates {
            self.apply(update, &mut pictures);
        }
        pictures
    }

    /// The line above the posts: the offline note with the date of the shown news, or "No
    /// news"; `None` when fresh or cached news shows.
    pub fn status_line(&self) -> Option<String> {
        match (&self.feed, self.reach) {
            (None, _) => Some(NO_NEWS.to_owned()),
            (Some(_), Reach::Offline) => Some(format!(
                "{OFFLINE_PREFIX}{}",
                self.saved.map(file_date).unwrap_or_default()
            )),
            (Some(_), Reach::Reached) => None,
        }
    }
}

/// The line when no news shows.
pub const NO_NEWS: &str = "No news";
/// The start of the line when the shown news is older than the last refresh.
pub const OFFLINE_PREFIX: &str = "Offline - news from ";

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use nw_protocol::launcher_api::{INFO_PATH, NEWS_SOURCE_PATH, NewsSource};

    use super::*;
    use crate::test_server::serve;

    const FEED_PATH: &str = "/api/news";
    const PICTURE_PATH: &str = "/uploads/a.jpg";
    const PICTURE_SIZE: [u32; 2] = [2000, 1000];
    const NOW: &str = "2026-11-09T12:00:00Z";

    fn post(id: i64, kind: PostKind, created_at: &str) -> Post {
        Post {
            id,
            kind,
            title: format!("Post {id}"),
            body_html: "<p>Body</p>".to_owned(),
            picture_url: None,
            created_at: DateTime::parse_from_rfc3339(created_at).expect("time"),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .expect("time")
            .with_timezone(&Utc)
    }

    fn jpeg() -> Vec<u8> {
        let [width, height] = PICTURE_SIZE;
        encode_jpeg(&RgbImage::from_pixel(
            width,
            height,
            image::Rgb([163, 71, 190]),
        ))
        .expect("jpeg")
    }

    fn feed_json(base: &str, picture: bool) -> String {
        let picture = if picture {
            format!("\"{base}{PICTURE_PATH}\"")
        } else {
            "null".to_owned()
        };
        format!(
            r#"{{"posts":[{{"id":7,"kind":"news","title":"Open test","bodyMarkdown":"x","bodyHtml":"<p>Hi</p>","pictureUrl":{picture},"createdAt":"2026-11-07T15:43:21Z"}},{{"id":8,"kind":"something_new","title":"New kind","bodyMarkdown":"","bodyHtml":"","pictureUrl":null,"createdAt":"2026-11-06T10:00:00+02:00"}}]}}"#
        )
    }

    /// A fake game server and site: the news source names the feed at `feed`, the feed answers
    /// `body`, and the picture route counts its calls.
    fn fake_site(
        source: Option<&'static str>,
        body: fn(&str) -> String,
    ) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let base_cell = Arc::new(std::sync::OnceLock::<String>::new());
        let feed_base = base_cell.clone();
        let source_base = base_cell.clone();
        let base = serve(
            Router::new()
                .route(
                    NEWS_SOURCE_PATH,
                    get(move || {
                        let base = source_base.get().cloned().unwrap_or_default();
                        async move {
                            axum::Json(NewsSource {
                                news_url: source.map(|path| format!("{base}{path}")),
                            })
                        }
                    }),
                )
                .route(
                    FEED_PATH,
                    get(move || {
                        let base = feed_base.get().cloned().unwrap_or_default();
                        async move { body(&base) }
                    }),
                )
                .route(
                    PICTURE_PATH,
                    get(move || {
                        counted.fetch_add(1, Ordering::SeqCst);
                        async { ([(header::CONTENT_TYPE, "image/jpeg")], jpeg()).into_response() }
                    }),
                ),
        );
        base_cell.set(base.clone()).expect("base");
        (base, hits)
    }

    fn run_refresh(base: &str, cache: &NewsCache, previous: Option<&Feed>) -> Vec<NewsUpdate> {
        crate::install_crypto_provider();
        let api = LauncherApi::new(base.to_owned()).expect("api");
        let (sender, receiver) = channel();
        refresh(&api, cache, previous, &sender);
        drop(sender);
        receiver.iter().collect()
    }

    #[test]
    fn the_site_feed_parses_and_unknown_kinds_are_kept() {
        let feed = parse_feed(feed_json("https://example.org", true).as_bytes()).expect("feed");
        assert_eq!(feed.posts.len(), 2);
        assert_eq!(feed.posts[0].kind, PostKind::News);
        assert_eq!(
            feed.posts[0].picture_url.as_deref(),
            Some("https://example.org/uploads/a.jpg")
        );
        assert_eq!(feed.posts[1].kind, PostKind::Other);
        assert_eq!(PostKind::Other.label(), "Post");
        assert!(parse_feed(b"{\"posts\":[{\"id\":1}]}").is_err());
        assert!(parse_feed(b"not json").is_err());
    }

    #[test]
    fn a_fresh_downtime_is_pinned_and_five_posts_show() {
        let mut posts: Vec<Post> = (1..=6)
            .map(|id| post(id, PostKind::News, "2026-11-08T12:00:00Z"))
            .collect();
        posts.push(post(20, PostKind::Downtime, "2026-11-09T02:00:00Z"));
        posts.push(post(21, PostKind::Downtime, "2026-11-07T02:00:00Z"));
        let feed = Feed { posts };
        let shown: Vec<i64> = shown_posts(&feed, now())
            .iter()
            .map(|post| post.id)
            .collect();
        assert_eq!(shown, [20, 1, 2, 3, 4]);
        assert!(is_pinned(&feed.posts[6], now()));
        assert!(!is_pinned(&feed.posts[7], now()), "older than 24 h");
        assert!(!is_pinned(&feed.posts[0], now()), "not a downtime");
    }

    #[test]
    fn only_https_or_local_http_addresses_are_read() {
        for good in [
            "https://example.org/api/news?target=launcher",
            "http://127.0.0.1:3100/api/news",
            "http://localhost/uploads/a.jpg",
        ] {
            assert!(checked_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://example.org/api/news",
            "ftp://example.org/x",
            "file:///etc/passwd",
            "https://user:pass@example.org/x",
            "not a url",
        ] {
            assert!(
                matches!(checked_url(bad), Err(NewsError::BadUrl(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            checked_url("https://d1.cloudfront.net/a.jpg"),
            Err(NewsError::Forbidden(_))
        ));
    }

    #[test]
    fn pictures_are_jpeg_only_and_scaled_to_the_card_width() {
        let picture = scaled_picture(&jpeg()).expect("picture");
        assert_eq!(
            (picture.width(), picture.height()),
            (PICTURE_WIDTH, PICTURE_WIDTH / 2)
        );
        assert!(matches!(
            scaled_picture(b"GIF89a....."),
            Err(NewsError::NotJpeg)
        ));
        let small = encode_jpeg(&RgbImage::new(10, 5)).expect("jpeg");
        assert_eq!(scaled_picture(&small).expect("small").width(), 10);
        let image = color_image(&picture);
        assert_eq!(
            image.size,
            [PICTURE_WIDTH as usize, (PICTURE_WIDTH / 2) as usize]
        );
    }

    #[test]
    fn the_cache_keeps_the_feed_and_prunes_old_pictures_only() {
        let dir = tempfile::tempdir().expect("dir");
        let cache = NewsCache::new(dir.path().to_path_buf());
        assert!(cache.load_feed().is_none());
        let raw = feed_json("https://example.org", false);
        cache.save_feed(raw.as_bytes()).expect("save");
        let cached = cache.load_feed().expect("cached");
        assert_eq!(cached.feed.posts[0].id, 7);
        let picture = encode_jpeg(&RgbImage::new(4, 4)).expect("jpeg");
        for id in [7, 9] {
            cache.save_picture(id, &picture).expect("picture");
        }
        let other = dir.path().join(PICTURES_DIR).join("notes.txt");
        std::fs::write(&other, "keep").expect("other");
        cache.prune_pictures(&HashSet::from([7])).expect("prune");
        assert!(cache.load_picture(7).is_some());
        assert!(cache.load_picture(9).is_none());
        assert!(other.exists(), "only <id>.jpg files are pictures");
        cache.clear().expect("clear");
        assert!(cache.load_feed().is_none() && cache.load_picture(7).is_none());
        cache.clear().expect("clearing twice is fine");
    }

    #[test]
    fn a_refresh_sends_the_feed_then_the_picture_and_reuses_the_cache() {
        let (base, hits) = fake_site(Some(FEED_PATH), |base| feed_json(base, true));
        let dir = tempfile::tempdir().expect("dir");
        let cache = NewsCache::new(dir.path().to_path_buf());
        let updates = run_refresh(&base, &cache, None);
        let feed = match &updates[..] {
            [NewsUpdate::Feed(feed), NewsUpdate::Picture(7, picture)] => {
                assert_eq!(picture.width(), PICTURE_WIDTH);
                feed.clone()
            }
            _ => panic!("expected the feed, then one picture"),
        };
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(cache.load_feed().expect("cached").feed, feed);
        let again = run_refresh(&base, &cache, Some(&feed));
        assert!(matches!(
            &again[..],
            [NewsUpdate::Feed(_), NewsUpdate::Picture(7, _)]
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1, "the cached picture is used");
    }

    #[test]
    fn no_source_clears_the_cache_and_a_bad_feed_is_offline() {
        let dir = tempfile::tempdir().expect("dir");
        let cache = NewsCache::new(dir.path().to_path_buf());
        cache
            .save_feed(feed_json("https://example.org", false).as_bytes())
            .expect("save");
        let (bad, _) = fake_site(Some(FEED_PATH), |_| "not json".to_owned());
        assert!(matches!(
            &run_refresh(&bad, &cache, None)[..],
            [NewsUpdate::Offline]
        ));
        assert!(cache.load_feed().is_some(), "a bad feed keeps the cache");
        assert!(matches!(
            &run_refresh("http://127.0.0.1:9", &cache, None)[..],
            [NewsUpdate::Offline]
        ));
        let (none, _) = fake_site(None, |_| String::new());
        assert!(matches!(
            &run_refresh(&none, &cache, None)[..],
            [NewsUpdate::NoNews]
        ));
        assert!(cache.load_feed().is_none());
        let older = serve(Router::new().route(INFO_PATH, get(|| async { StatusCode::NO_CONTENT })));
        assert!(matches!(
            &run_refresh(&older, &cache, None)[..],
            [NewsUpdate::NoNews]
        ));
    }

    #[test]
    fn the_cache_shows_at_once_and_is_not_fetched_again_within_a_minute() {
        let dir = tempfile::tempdir().expect("dir");
        let cache = NewsCache::new(dir.path().to_path_buf());
        let empty = NewsState::open(cache.clone());
        assert_eq!(empty.status_line().as_deref(), Some(NO_NEWS));
        assert!(empty.refresh_due("login.example.org", Instant::now(), FEED_MIN_AGE));
        cache
            .save_feed(feed_json("https://example.org", false).as_bytes())
            .expect("save");
        let state = NewsState::open(cache);
        assert_eq!(state.feed().expect("cached").posts.len(), 2);
        assert_eq!(state.status_line(), None);
        let now = Instant::now();
        assert!(!state.refresh_due("login.example.org", now, FEED_MIN_AGE));
        assert!(state.refresh_due("login.example.org", now + FEED_MIN_AGE, FEED_MIN_AGE));
        assert!(state.next_refresh("login.example.org", now) <= REFRESH_INTERVAL);
    }

    #[test]
    fn an_offline_refresh_keeps_the_news_and_names_its_date() {
        crate::install_crypto_provider();
        let dir = tempfile::tempdir().expect("dir");
        let cache = NewsCache::new(dir.path().to_path_buf());
        cache
            .save_feed(feed_json("https://example.org", false).as_bytes())
            .expect("save");
        let saved = cache.load_feed().expect("cached").saved;
        let mut state = NewsState::open(cache);
        state.start_refresh(LauncherApi::new("http://127.0.0.1:9".to_owned()).expect("api"));
        assert!(!state.refresh_due("127.0.0.1:9", Instant::now(), FEED_MIN_AGE));
        while state.busy() {
            state.poll();
            std::thread::yield_now();
        }
        assert_eq!(
            state.status_line(),
            Some(format!("{OFFLINE_PREFIX}{}", file_date(saved)))
        );
        assert!(state.feed().is_some());
        assert!(state.refresh_due("other.example.org", Instant::now(), REFRESH_INTERVAL));
    }
}
