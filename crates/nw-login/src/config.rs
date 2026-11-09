// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/edge.toml` (M2 spec section 9). Secrets are not in this file; see [`crate::secrets`].

use std::collections::{BTreeMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use nw_common::config::{ConfigError, load_config};
pub use nw_db::DatabaseConfig;
use nw_protocol::aws_mirror::{
    CMS_HOST, CMS_IMAGE_HOST, MIRROR_HOST_KINDS, REMOTE_CONFIG_BUCKETS, dynamodb_host,
    is_aws_mirror_host, remote_config_host,
};
use nw_protocol::launcher_api::{AuthMode, SESSION_PLACEHOLDER, is_forbidden_host};
use serde::Deserialize;
use uuid::Uuid;

use crate::analytics::AnalyticsConfig;
use crate::discord::is_discord_id;
use crate::experience::ExperienceConfig;
use crate::fast_travel::FastTravelConfig;
use crate::game::API_REGION;
use crate::secrets::SECRET_ENV_PREFIX;
use crate::voice::VoiceConfig;

/// Environment variable prefix for overrides of `config/edge.toml`.
pub const EDGE_ENV_PREFIX: &str = "NW_EDGE_";

/// Host name label of the launcher API and the channel config: `login.<domain>`.
pub const LOGIN_HOST_LABEL: &str = "login";
/// Host name label of the game routes: `game.<domain>`.
pub const GAME_HOST_LABEL: &str = "game";
/// The HTTPS port a link implies when it names none.
pub const DEFAULT_HTTPS_PORT: u16 = 443;
/// The port the client pings when nothing sends its ping elsewhere.
pub const DEFAULT_PING_PORT: u16 = 80;
/// Separator between a host and its port in a link.
const PORT_SEPARATOR: char = ':';

/// Window of `queue_requests_per_minute`, in seconds.
pub const QUEUE_LIMIT_WINDOW_SECONDS: u64 = 60;
/// Queue requests of one client besides its refreshes in one window: the join.
const QUEUE_JOIN_REQUESTS: u64 = 1;

/// Launch argument that sets the channel base URL (endpoint RE note).
pub const CHANNEL_SERVICE_ARGUMENT: &str = "-ChannelService";
/// Launch argument that sets the channel key (endpoint RE note).
pub const LOGIN_CHANNEL_OVERRIDE_ARGUMENT: &str = "-LoginChannelOverride";
/// Launch argument that selects the client's auth backend (unknowns note, fifth pass).
pub const AUTH_BACKEND_ARGUMENT: &str = "-AuthBackend";
/// Auth backend `nw` (index 0): the only one that sends the Steam ticket to our `/steam` route.
/// The client compares the name with exact case; its built-in default is `omni`, which skips
/// `/steam` (unknowns note, fifth pass).
pub const AUTH_BACKEND_NW: &str = "nw";
/// Launch argument that sets `client-connection.authorization.cognito-pool-id`
/// (`FUN_146467680`). The client names its two `CognitoId` remote-config layers after it and
/// skips them when it is empty (character-select note); it also sends it in `ListWorlds`.
pub const COGNITO_POOL_ARGUMENT: &str = "-CognitoPool";

/// The whole `edge-http` configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    /// Name shown in the launcher.
    pub server_name: String,
    /// The community's own domain; `login.<domain>` is the launcher API host and `game.<domain>`
    /// the game route host.
    pub domain: String,
    /// Public HTTPS listener.
    pub https_listen: SocketAddr,
    /// Plain HTTP listener of the region ping ([`crate::region_ping`]). The client pings port 80.
    pub ping_listen: SocketAddr,
    /// The HTTPS port players reach `https_listen` on. It differs from the listener port when a
    /// router or proxy forwards another port to it. Links to `login.<domain>` and
    /// `game.<domain>` name it when it is not [`DEFAULT_HTTPS_PORT`], the default.
    #[serde(default = "default_public_https_port")]
    pub public_https_port: u16,
    /// The plain HTTP port players reach `ping_listen` on. The launcher's proxy sends the region
    /// ping there. Default [`DEFAULT_PING_PORT`].
    #[serde(default = "default_public_ping_port")]
    pub public_ping_port: u16,
    /// Internal Prometheus listener (review finding I1).
    pub metrics_listen: SocketAddr,
    /// Time limit for one HTTP request.
    pub request_timeout_seconds: u64,
    /// Most requests in progress at one time (spec amendment 6). More requests wait for a free
    /// slot, within `request_timeout_seconds`.
    pub max_connections: usize,
    /// `tracing` filter, for example `info`.
    pub log_filter: String,
    /// Certificate files.
    pub tls: TlsConfig,
    /// Account and session settings.
    pub auth: AuthConfig,
    /// Discord app; used when `auth.mode = "discord"`.
    #[serde(default)]
    pub discord: DiscordConfig,
    /// Global database.
    pub global_database: DatabaseConfig,
    /// Valkey.
    pub valkey: ValkeyConfig,
    /// Our own remote config sets.
    pub remote_config: RemoteConfigConfig,
    /// Our own news documents and images.
    pub cms: CmsConfig,
    /// The Amazon names this server answers for its game clients.
    pub aws_mirror: AwsMirrorConfig,
    /// The experience multipliers; normal experience when left out.
    #[serde(default)]
    pub experience: ExperienceConfig,
    /// Whether fast travel costs azoth; the retail cost when left out.
    #[serde(default)]
    pub fast_travel: FastTravelConfig,
    /// The store; an empty store when left out.
    #[serde(default)]
    pub shop: ShopConfig,
    /// The limits and retention of the analytics stream; the defaults when left out.
    #[serde(default)]
    pub analytics: AnalyticsConfig,
    /// The hosts of the community's voice service that the voice prelogin names
    /// ([`crate::voice`]); left out, the prelogin is not served.
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    /// The community's public website; left out, the launcher gets no news and the message of
    /// the day stays the `[cms]` file.
    #[serde(default)]
    pub website: Option<WebsiteConfig>,
    /// Worlds, one `[[world]]` block each.
    #[serde(rename = "world")]
    pub worlds: Vec<WorldConfig>,
}

const fn default_public_https_port() -> u16 {
    DEFAULT_HTTPS_PORT
}

const fn default_public_ping_port() -> u16 {
    DEFAULT_PING_PORT
}

/// `[tls]`: PEM files from an ACME tool such as `certbot` or `lego`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate chain.
    pub certificate_pem_path: PathBuf,
    /// Private key.
    pub private_key_pem_path: PathBuf,
}

/// `[auth]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Password or Discord mode.
    pub mode: AuthMode,
    /// Whether password sign-up is open.
    pub allow_registration: bool,
    /// Parallel Argon2id hashes; 0 = the number of CPUs.
    pub argon2_parallel: usize,
    /// Failed logins allowed per IP and per username in one window.
    pub login_fail_limit: u32,
    /// Length of the failed-login window.
    pub login_fail_window_seconds: u64,
    /// Lifetime of a game session after each use.
    pub game_session_ttl_seconds: u64,
}

/// `[discord]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    /// Discord application (client) id.
    pub client_id: String,
    /// Id of the Discord server a player must be in to log in; empty: no server check.
    #[serde(default)]
    pub guild_id: String,
    /// When true, only the Discord users of [`Self::allowed_user_ids`] and the holders of a role
    /// of [`Self::allowed_role_ids`] may log in and make accounts. Off by default: every Discord
    /// user may.
    #[serde(default)]
    pub allow_list: bool,
    /// The Discord user ids that may log in while [`Self::allow_list`] is on.
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    /// The role ids in the server [`Self::guild_id`] whose holders may log in while
    /// [`Self::allow_list`] is on. A Play checks the roles of the last login.
    #[serde(default)]
    pub allowed_role_ids: Vec<String>,
}

impl DiscordConfig {
    /// The Discord server a login needs, if any.
    pub fn required_guild_id(&self) -> Option<&str> {
        (!self.guild_id.is_empty()).then_some(self.guild_id.as_str())
    }

    /// True when the Discord user `user_id` with the server roles `role_ids` may log in:
    /// [`Self::allow_list`] is off, [`Self::allowed_user_ids`] names it, or it holds a role of
    /// [`Self::allowed_role_ids`].
    pub fn allows(&self, user_id: &str, role_ids: &[String]) -> bool {
        !self.allow_list
            || self.allowed_user_ids.iter().any(|id| id == user_id)
            || role_ids
                .iter()
                .any(|role| self.allowed_role_ids.contains(role))
    }
}

/// `[valkey]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValkeyConfig {
    /// For example `redis://127.0.0.1:6379`.
    pub url: String,
}

/// `[remote_config]`: our own remote config sets (study note
/// `docs/re/2026-09-18-remote-config-layers.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfigConfig {
    /// Folder of the config sets: one file per layer, for example `public-ProductId.json`
    /// ([`crate::remote_config::layer_file_name`]).
    pub directory: PathBuf,
    /// The bucket the client asks for; one of [`REMOTE_CONFIG_BUCKETS`].
    pub bucket: String,
    /// Our Cognito pool id: the client's [`COGNITO_POOL_ARGUMENT`] and the id of the
    /// `CognitoId` layers. It names no Amazon pool.
    pub cognito_pool_id: Uuid,
}

/// `[shop]`: the in-game store: its mounts (the store table of `content-tool store`,
/// [`nw_protocol::store_api`]), whether it sells, and whether its mounts are free or cost Marks of
/// Fortune from the account's wallet.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopConfig {
    /// The store table file; no file, or none set, gives an empty store.
    pub store_table: Option<PathBuf>,
    /// Whether the store sells. Off, the catalog is empty and purchases are refused; the mounts an
    /// account got stay its own.
    #[serde(default = "switched_on")]
    pub enabled: bool,
    /// Whether every mount costs 0 (retail's last weeks). Off, a mount costs its price from the
    /// account's wallet.
    #[serde(default = "switched_on")]
    pub free: bool,
    /// The price of a mount, in Marks of Fortune (Observed: 20 of retail's 31 mount offers cost
    /// 20000, 10 cost 15000, 1 cost 10000).
    #[serde(default = "default_mount_price")]
    pub mount_price: u32,
    /// The price of single mounts, by the entitlement that unlocks them
    /// (`Ent_Mount_MTX_1_wolf`); the others cost [`ShopConfig::mount_price`].
    #[serde(default)]
    pub mount_prices: BTreeMap<String, u32>,
}

/// The retail price of most store mounts, in Marks of Fortune.
pub const DEFAULT_MOUNT_PRICE: u32 = 20_000;

const fn switched_on() -> bool {
    true
}

const fn default_mount_price() -> u32 {
    DEFAULT_MOUNT_PRICE
}

impl Default for ShopConfig {
    fn default() -> Self {
        Self {
            store_table: None,
            enabled: switched_on(),
            free: switched_on(),
            mount_price: DEFAULT_MOUNT_PRICE,
            mount_prices: BTreeMap::new(),
        }
    }
}

impl ShopConfig {
    /// The listed price of the mount that `entitlement` unlocks.
    pub fn list_price(&self, entitlement: &str) -> u32 {
        self.mount_prices
            .get(entitlement)
            .copied()
            .unwrap_or(self.mount_price)
    }

    /// What the mount that `entitlement` unlocks costs: 0 in a free store, else its listed price.
    pub fn price(&self, entitlement: &str) -> u32 {
        if self.free {
            0
        } else {
            self.list_price(entitlement)
        }
    }
}

/// `[cms]`: our own news documents and images, answered as the client's news host and its image
/// host (study note `docs/re/2026-11-02-cms-news-hosts.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CmsConfig {
    /// Folder with one subfolder per news host, named after it; each file in it is served at its
    /// path ([`crate::cms::CmsObjects::load`]).
    pub directory: PathBuf,
}

/// The website's launcher news feed, after [`WebsiteConfig::site_url`].
pub const LAUNCHER_NEWS_PATH: &str = "/api/news?target=launcher";
/// The website's message of the day in the client's retail shape, after
/// [`WebsiteConfig::site_url`].
pub const SITE_MOTD_PATH: &str = "/api/game/cms/motd";
const HTTP_SCHEME: &str = "http://";
const HTTPS_SCHEME: &str = "https://";
const URL_PATH_SEPARATOR: char = '/';
const URL_PORT_SEPARATOR: char = ':';

/// `[website]`: the community's public website. The launcher reads its news feed there
/// (`GET /launcher/news-source` names it), and `edge-http` gets the message of the day from it
/// once a minute. The website opens no connection to this server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebsiteConfig {
    /// The website, such as `https://example.org`, with no trailing `/`.
    pub site_url: String,
}

impl WebsiteConfig {
    /// The launcher news feed.
    pub fn launcher_news_url(&self) -> String {
        format!("{}{LAUNCHER_NEWS_PATH}", self.site_url)
    }

    /// The message of the day.
    pub fn motd_url(&self) -> String {
        format!("{}{SITE_MOTD_PATH}", self.site_url)
    }

    /// The host of [`Self::site_url`]; `None` when it does not start with `http://` or
    /// `https://`.
    fn host(&self) -> Option<&str> {
        let rest = self
            .site_url
            .strip_prefix(HTTPS_SCHEME)
            .or_else(|| self.site_url.strip_prefix(HTTP_SCHEME))?;
        let authority = rest.split(URL_PATH_SEPARATOR).next()?;
        authority.split(URL_PORT_SEPARATOR).next()
    }
}

/// `[aws_mirror]`: the Amazon names our launcher maps to this server for the game process, and
/// the self-signed certificate this server shows for them (`edge-http aws-mirror-certificate`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwsMirrorConfig {
    /// Host names in lower case; each passes [`is_aws_mirror_host`], and the bucket host, the
    /// region ping host of [`API_REGION`] and both news hosts are among them.
    pub hosts: Vec<String>,
    /// Certificate for exactly these names.
    pub certificate_pem_path: PathBuf,
    /// Its private key.
    pub private_key_pem_path: PathBuf,
}

/// One `[[world]]` block.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldConfig {
    /// World id.
    pub id: Uuid,
    /// Name shown in the world list.
    pub name: String,
    /// Region text shown in the world list.
    pub region: String,
    /// `Token.RepAddress`: `IPv4:port` of the `rep-gateway` that serves this world.
    pub rep_address: String,
    /// The world database.
    pub database: DatabaseConfig,
    /// Environment variable that holds the world database password.
    pub database_password_env: String,
    /// Live entry tickets allowed at one time; `world-director` also limits by free player slots.
    pub capacity: u32,
    /// Characters one account may have in this world.
    pub max_characters_per_account: u32,
    /// Seconds between queue refreshes of the client.
    pub refresh_interval_seconds: u64,
    /// Queue joins and refreshes allowed per account per minute.
    pub queue_requests_per_minute: u32,
}

impl WorldConfig {
    /// `rep_address` as a socket address.
    pub fn rep_socket_address(&self) -> Result<SocketAddrV4, EdgeConfigError> {
        let address: SocketAddrV4 =
            self.rep_address
                .parse()
                .map_err(|_| EdgeConfigError::Invalid {
                    key: "world.rep_address",
                    reason: "must be IPv4:port".to_owned(),
                })?;
        if address.port() == 0 {
            return Err(EdgeConfigError::Invalid {
                key: "world.rep_address",
                reason: "port must not be 0".to_owned(),
            });
        }
        Ok(address)
    }
}

/// Error from [`load_edge_config`].
#[derive(Debug, thiserror::Error)]
pub enum EdgeConfigError {
    /// The file cannot be read or has a wrong key or type.
    #[error(transparent)]
    Load(#[from] ConfigError),
    /// A value is not allowed.
    #[error("config value `{key}` is not valid: {reason}")]
    Invalid {
        /// Key path, for example `world.capacity`.
        key: &'static str,
        /// Why the value is refused.
        reason: String,
    },
}

/// Reads and checks `config/edge.toml` (with `NW_EDGE_` overrides).
pub fn load_edge_config(path: &Path) -> Result<EdgeConfig, EdgeConfigError> {
    let config: EdgeConfig = load_config(path, EDGE_ENV_PREFIX)?;
    config.validate()?;
    Ok(config)
}

impl EdgeConfig {
    /// `login.<domain>`.
    pub fn login_host(&self) -> String {
        format!("{LOGIN_HOST_LABEL}.{}", self.domain)
    }

    /// `game.<domain>`.
    pub fn game_host(&self) -> String {
        format!("{GAME_HOST_LABEL}.{}", self.domain)
    }

    /// `host` with [`Self::public_https_port`], left out when it is [`DEFAULT_HTTPS_PORT`].
    fn public_authority(&self, host: String) -> String {
        if self.public_https_port == DEFAULT_HTTPS_PORT {
            host
        } else {
            format!("{host}{PORT_SEPARATOR}{}", self.public_https_port)
        }
    }

    /// `login.<domain>` as players reach it, with the port when it is not the default.
    pub fn login_authority(&self) -> String {
        self.public_authority(self.login_host())
    }

    /// `game.<domain>` as players reach it, with the port when it is not the default.
    pub fn game_authority(&self) -> String {
        self.public_authority(self.game_host())
    }

    /// The launch arguments the launcher passes to the game (M2 spec section 6).
    pub fn launch_arguments(&self) -> Vec<String> {
        vec![
            CHANNEL_SERVICE_ARGUMENT.to_owned(),
            format!("https://{}", self.login_authority()),
            LOGIN_CHANNEL_OVERRIDE_ARGUMENT.to_owned(),
            SESSION_PLACEHOLDER.to_owned(),
            AUTH_BACKEND_ARGUMENT.to_owned(),
            AUTH_BACKEND_NW.to_owned(),
            COGNITO_POOL_ARGUMENT.to_owned(),
            self.remote_config.cognito_pool_id.to_string(),
        ]
    }

    /// The world with this id.
    pub fn world(&self, id: Uuid) -> Option<&WorldConfig> {
        self.worlds.iter().find(|world| world.id == id)
    }

    fn validate_aws_mirror(&self) -> Result<(), EdgeConfigError> {
        let invalid = |reason: &str| {
            Err(EdgeConfigError::Invalid {
                key: "aws_mirror.hosts",
                reason: reason.to_owned(),
            })
        };
        let mut hosts = HashSet::new();
        for host in &self.aws_mirror.hosts {
            if host.to_ascii_lowercase() != *host || !is_aws_mirror_host(host) {
                return invalid(&format!("{host} is not a lower-case {MIRROR_HOST_KINDS}"));
            }
            if !hosts.insert(host.as_str()) {
                return invalid(&format!("{host} is listed twice"));
            }
        }
        let bucket_host = remote_config_host(&self.remote_config.bucket);
        if !hosts.contains(bucket_host.as_str()) {
            return invalid(&format!("must contain the bucket host {bucket_host}"));
        }
        let ping_host = dynamodb_host(API_REGION);
        if !hosts.contains(ping_host.as_str()) {
            return invalid(&format!("must contain the region ping host {ping_host}"));
        }
        for news_host in [CMS_HOST, CMS_IMAGE_HOST] {
            if !hosts.contains(news_host) {
                return invalid(&format!("must contain the news host {news_host}"));
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), EdgeConfigError> {
        let invalid = |key: &'static str, reason: &str| {
            Err(EdgeConfigError::Invalid {
                key,
                reason: reason.to_owned(),
            })
        };
        if self.domain.is_empty() || self.domain.contains('/') || self.domain.contains(':') {
            return invalid("domain", "must be a plain domain name");
        }
        if is_forbidden_host(&self.domain) {
            return invalid("domain", "must not be an Amazon or CloudFront domain");
        }
        if self.public_https_port == 0 {
            return invalid("public_https_port", "must be above 0");
        }
        if self.public_ping_port == 0 {
            return invalid("public_ping_port", "must be above 0");
        }
        if self.request_timeout_seconds == 0 {
            return invalid("request_timeout_seconds", "must be above 0");
        }
        let analytics = &self.analytics;
        if analytics.max_body_bytes == 0
            || analytics.requests_per_minute == 0
            || analytics.bytes_per_minute == 0
            || analytics.retention_days == 0
        {
            return invalid("analytics", "every limit must be above 0");
        }
        if self.max_connections == 0 {
            return invalid("max_connections", "must be above 0");
        }
        if self.auth.login_fail_limit == 0 || self.auth.login_fail_window_seconds == 0 {
            return invalid("auth.login_fail_limit", "limit and window must be above 0");
        }
        if self.auth.game_session_ttl_seconds == 0 {
            return invalid("auth.game_session_ttl_seconds", "must be above 0");
        }
        if self.auth.mode == AuthMode::Discord && self.discord.client_id.is_empty() {
            return invalid("discord.client_id", "is required in Discord mode");
        }
        if !self.discord.guild_id.chars().all(|c| c.is_ascii_digit()) {
            return invalid(
                "discord.guild_id",
                "must be a Discord server id (digits only)",
            );
        }
        if !self
            .discord
            .allowed_user_ids
            .iter()
            .all(|id| is_discord_id(id))
        {
            return invalid(
                "discord.allowed_user_ids",
                "must be Discord user ids (17 to 20 digits)",
            );
        }
        if !self
            .discord
            .allowed_role_ids
            .iter()
            .all(|id| is_discord_id(id))
        {
            return invalid(
                "discord.allowed_role_ids",
                "must be Discord role ids (17 to 20 digits)",
            );
        }
        if !self.discord.allowed_role_ids.is_empty() && self.discord.guild_id.is_empty() {
            return invalid(
                "discord.allowed_role_ids",
                "needs discord.guild_id: the roles are read in that server",
            );
        }
        if self.discord.allow_list
            && self.discord.allowed_user_ids.is_empty()
            && self.discord.allowed_role_ids.is_empty()
        {
            return invalid(
                "discord.allowed_user_ids",
                "needs at least one Discord user id or role id while discord.allow_list is on",
            );
        }
        if !REMOTE_CONFIG_BUCKETS.contains(&self.remote_config.bucket.as_str()) {
            return invalid(
                "remote_config.bucket",
                "must be a bucket the client asks for",
            );
        }
        if let Err(error) = self.experience.validate() {
            return invalid("experience", &error.to_string());
        }
        self.validate_aws_mirror()?;
        if let Some(website) = &self.website {
            let url = website.site_url.as_str();
            match website.host() {
                Some(host)
                    if !host.is_empty()
                        && !url.ends_with(URL_PATH_SEPARATOR)
                        && !is_forbidden_host(host) => {}
                _ => {
                    return invalid(
                        "website.site_url",
                        "must start with http:// or https://, have no trailing / and no Amazon \
                         or CloudFront host",
                    );
                }
            }
        }
        if self.worlds.is_empty() {
            return invalid("world", "at least one [[world]] is required");
        }
        let mut ids = HashSet::new();
        let mut rep_addresses = HashSet::new();
        for world in &self.worlds {
            if !ids.insert(world.id) {
                return invalid("world.id", "world ids must be unique");
            }
            // One rep-gateway serves one world, so worlds cannot share its address.
            if !rep_addresses.insert(world.rep_socket_address()?) {
                return invalid("world.rep_address", "each world needs its own rep-gateway");
            }
            if world.capacity == 0 || world.refresh_interval_seconds == 0 {
                return invalid(
                    "world.capacity",
                    "capacity and refresh interval must be above 0",
                );
            }
            if world.max_characters_per_account == 0 {
                return invalid("world.max_characters_per_account", "must be above 0");
            }
            // A normal client refreshes once per interval and joins once (review minor m4).
            let needed = QUEUE_LIMIT_WINDOW_SECONDS.div_ceil(world.refresh_interval_seconds)
                + QUEUE_JOIN_REQUESTS;
            if u64::from(world.queue_requests_per_minute) < needed {
                return invalid(
                    "world.queue_requests_per_minute",
                    &format!(
                        "must be at least {needed} for refresh_interval_seconds = {}",
                        world.refresh_interval_seconds
                    ),
                );
            }
            if !world.database_password_env.starts_with(SECRET_ENV_PREFIX) {
                return invalid(
                    "world.database_password_env",
                    &format!("must start with {SECRET_ENV_PREFIX}"),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Discord server id and two role ids in it (made-up snowflakes).
    const GUILD: &str = "1234567890123456789";
    const ROLE: &str = "1000000000000000001";
    const OTHER_ROLE: &str = "1000000000000000002";

    /// A valid configuration in TOML.
    pub(crate) const VALID: &str = r#"
server_name = "Test server"
domain = "example.org"
https_listen = "0.0.0.0:443"
ping_listen = "0.0.0.0:80"
public_https_port = 443
public_ping_port = 80
metrics_listen = "127.0.0.1:9100"
request_timeout_seconds = 10
max_connections = 4096
log_filter = "info"

[tls]
certificate_pem_path = "local-data/edge-http/fullchain.pem"
private_key_pem_path = "local-data/edge-http/privkey.pem"

[auth]
mode = "password"
allow_registration = true
argon2_parallel = 0
login_fail_limit = 10
login_fail_window_seconds = 600
game_session_ttl_seconds = 7200

[global_database]
url = "postgres://nw@127.0.0.1:5432/nw_global"
max_connections = 5

[valkey]
url = "redis://127.0.0.1:6379"

[remote_config]
directory = "config/remote-config"
bucket = "ags-javelin-remote-config"
cognito_pool_id = "b04fbfcd-942c-472d-88c0-c762c4076b2f"

[cms]
directory = "config/cms"

[aws_mirror]
hosts = [
    "ags-javelin-remote-config.s3.amazonaws.com",
    "kinesis.us-east-1.amazonaws.com",
    "sts.us-east-1.amazonaws.com",
    "dynamodb.us-east-1.amazonaws.com",
    "d1hkbwzm1bktgo.cloudfront.net",
    "client.content-service.amazongames.com",
]
certificate_pem_path = "local-data/edge-http/aws-mirror.pem"
private_key_pem_path = "local-data/edge-http/aws-mirror-key.pem"

[[world]]
id = "8d0f4c2e-0000-4000-8000-000000000001"
name = "Aeternum"
region = "us-east"
rep_address = "127.0.0.1:33435"
database_password_env = "NW_SECRET_WORLD_1_DATABASE_PASSWORD"
capacity = 100
max_characters_per_account = 2
refresh_interval_seconds = 5
queue_requests_per_minute = 30

[world.database]
url = "postgres://nw@127.0.0.1:5432/nw_world_1"
max_connections = 5
"#;

    /// The id of the world in [`VALID`].
    const FIRST_WORLD_ID: &str = "8d0f4c2e-0000-4000-8000-000000000001";
    /// The id of the second world in [`two_worlds`].
    const SECOND_WORLD_ID: &str = "8d0f4c2e-0000-4000-8000-000000000002";
    /// The rep-gateway of the world in [`VALID`].
    const FIRST_REP_ADDRESS: &str = "127.0.0.1:33435";
    /// The rep-gateway of the second world in [`two_worlds`].
    const SECOND_REP_ADDRESS: &str = "127.0.0.1:33436";

    /// [`VALID`] with a second world that has its own id and rep-gateway.
    pub(crate) fn two_worlds() -> String {
        let start = VALID.find("[[world]]").expect("world block");
        let second = VALID[start..]
            .replace(FIRST_WORLD_ID, SECOND_WORLD_ID)
            .replace(FIRST_REP_ADDRESS, SECOND_REP_ADDRESS);
        format!("{VALID}{second}")
    }

    /// The valid two-world configuration as a value.
    pub(crate) fn valid_two_worlds() -> EdgeConfig {
        load(&two_worlds()).expect("valid two-world config")
    }

    fn load(text: &str) -> Result<EdgeConfig, EdgeConfigError> {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("edge.toml");
        std::fs::write(&path, text).expect("write");
        load_edge_config(&path)
    }

    /// The valid configuration as a value.
    pub(crate) fn valid() -> EdgeConfig {
        load(VALID).expect("valid config")
    }

    fn refused(text: &str, key: &str) {
        match load(text) {
            Err(EdgeConfigError::Invalid { key: found, .. }) => assert_eq!(found, key),
            other => panic!("expected {key} to be refused, got {other:?}"),
        }
    }

    #[test]
    fn valid_file_loads() {
        let config = valid();
        assert_eq!(config.experience, ExperienceConfig::default());
        assert_eq!(config.fast_travel, FastTravelConfig::default());
        assert_eq!(config.shop, ShopConfig::default());
        assert!(config.shop.enabled && config.shop.free);
        assert_eq!(config.login_host(), "login.example.org");
        assert_eq!(config.game_host(), "game.example.org");
        assert_eq!(config.login_authority(), "login.example.org");
        assert_eq!(config.game_authority(), "game.example.org");
        assert_eq!(config.worlds.len(), 1);
        assert_eq!(config.auth.mode, AuthMode::Password);
        assert_eq!(
            config.launch_arguments(),
            [
                "-ChannelService",
                "https://login.example.org",
                "-LoginChannelOverride",
                SESSION_PLACEHOLDER,
                "-AuthBackend",
                "nw",
                "-CognitoPool",
                "b04fbfcd-942c-472d-88c0-c762c4076b2f"
            ]
        );
    }

    #[test]
    fn the_public_ports_default_to_443_and_80() {
        let config = load(
            &VALID
                .replace("public_https_port = 443\n", "")
                .replace("public_ping_port = 80\n", ""),
        )
        .expect("valid config");
        assert_eq!(config.public_https_port, DEFAULT_HTTPS_PORT);
        assert_eq!(config.public_ping_port, DEFAULT_PING_PORT);
    }

    #[test]
    fn another_public_port_is_named_in_the_links() {
        let config = load(&VALID.replace("public_https_port = 443", "public_https_port = 8443"))
            .expect("valid config");
        assert_eq!(config.login_host(), "login.example.org");
        assert_eq!(config.login_authority(), "login.example.org:8443");
        assert_eq!(config.game_authority(), "game.example.org:8443");
        assert_eq!(
            config.launch_arguments()[1],
            "https://login.example.org:8443"
        );
    }

    #[test]
    fn a_store_mount_costs_its_price_unless_the_store_is_free() {
        const WOLF: &str = "Ent_Mount_MTX_1_wolf";
        const BEAR: &str = "Ent_Mount_MTX_7_bear_Ronin";
        const MOUNT_PRICE: u32 = 15_000;
        const WOLF_PRICE: u32 = 10_000;
        let shop = load(&VALID.replace(
            "[cms]",
            &format!(
                "[shop]\nfree = false\nmount_price = {MOUNT_PRICE}\n\
                 [shop.mount_prices]\n{WOLF} = {WOLF_PRICE}\n\n[cms]"
            ),
        ))
        .expect("config")
        .shop;
        assert!(shop.enabled);
        assert_eq!(shop.price(WOLF), WOLF_PRICE);
        assert_eq!(shop.price(BEAR), MOUNT_PRICE);
        let free = ShopConfig {
            free: true,
            ..shop.clone()
        };
        assert_eq!(free.price(WOLF), 0);
        assert_eq!(free.list_price(WOLF), WOLF_PRICE);
    }

    #[test]
    fn bad_values_are_refused() {
        refused(
            &VALID.replace("public_https_port = 443", "public_https_port = 0"),
            "public_https_port",
        );
        refused(
            &VALID.replace("public_ping_port = 80", "public_ping_port = 0"),
            "public_ping_port",
        );
        refused(
            &VALID.replace("domain = \"example.org\"", "domain = \"x.cloudfront.net\""),
            "domain",
        );
        refused(
            &VALID.replace("\"127.0.0.1:33435\"", "\"rep.example.org:33435\""),
            "world.rep_address",
        );
        refused(
            &VALID.replace("\"127.0.0.1:33435\"", "\"127.0.0.1:0\""),
            "world.rep_address",
        );
        refused(
            &VALID.replace("mode = \"password\"", "mode = \"discord\""),
            "discord.client_id",
        );
        refused(
            &format!("{VALID}\n[discord]\nclient_id = \"123\"\nguild_id = \"blighted\"\n"),
            "discord.guild_id",
        );
        refused(
            &format!("{VALID}\n[discord]\nclient_id = \"123\"\nallowed_user_ids = [\"nelly\"]\n"),
            "discord.allowed_user_ids",
        );
        refused(
            &format!("{VALID}\n[discord]\nclient_id = \"123\"\nallow_list = true\n"),
            "discord.allowed_user_ids",
        );
        refused(
            &format!(
                "{VALID}\n[discord]\nclient_id = \"123\"\nguild_id = \"{GUILD}\"\n\
                 allowed_role_ids = [\"mods\"]\n"
            ),
            "discord.allowed_role_ids",
        );
        refused(
            &format!("{VALID}\n[discord]\nclient_id = \"123\"\nallowed_role_ids = [\"{ROLE}\"]\n"),
            "discord.allowed_role_ids",
        );
        refused(
            &VALID.replace("capacity = 100", "capacity = 0"),
            "world.capacity",
        );
        refused(
            &VALID.replace(
                "max_characters_per_account = 2",
                "max_characters_per_account = 0",
            ),
            "world.max_characters_per_account",
        );
    }

    #[test]
    fn the_website_section_names_the_feed_and_the_motd() {
        assert_eq!(valid().website, None);
        let with = |url: &str| {
            load(&VALID.replace(
                "[cms]",
                &format!("[website]\nsite_url = \"{url}\"\n\n[cms]"),
            ))
        };
        let website = with("https://example.org")
            .expect("website")
            .website
            .expect("set");
        assert_eq!(
            website.launcher_news_url(),
            "https://example.org/api/news?target=launcher"
        );
        assert_eq!(website.motd_url(), "https://example.org/api/game/cms/motd");
        assert!(with("http://127.0.0.1:3100").is_ok());
        for bad in [
            "example.org",
            "https://example.org/",
            "https://",
            "https://d1hkbwzm1bktgo.cloudfront.net",
        ] {
            match with(bad) {
                Err(EdgeConfigError::Invalid { key, .. }) => {
                    assert_eq!(key, "website.site_url", "{bad}");
                }
                other => panic!("expected {bad} to be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_voice_section_is_read_when_set() {
        assert_eq!(valid().voice, None);
        let voice = load(&VALID.replace(
            "[cms]",
            "[voice]\nrealm = \"voice.example.org\"\nxmpp_host = \"game.example.org\"\n\
             xmpp_port = 5223\nxmpp_tls = true\n\
             ws_xmpp_url = \"wss://game.example.org:443/ws-xmpp\"\n\
             telemetry_url = \"https://game.example.org\"\n\n[cms]",
        ))
        .expect("voice");
        assert_eq!(
            voice.voice.map(|voice| (voice.realm, voice.xmpp_port)),
            Some(("voice.example.org".to_owned(), 5223))
        );
    }

    #[test]
    fn fast_travel_is_free_only_when_set() {
        assert!(!valid().fast_travel.free);
        let free = load(&VALID.replace("[cms]", "[fast_travel]\nfree = true\n\n[cms]"))
            .expect("free fast travel");
        assert!(free.fast_travel.free);
    }

    #[test]
    fn remote_config_and_mirror_values_are_checked() {
        refused(
            &VALID.replace(
                "bucket = \"ags-javelin-remote-config\"",
                "bucket = \"my-bucket\"",
            ),
            "remote_config.bucket",
        );
        refused(
            &VALID.replace("[cms]", "[experience.trade_skills]\nridin = 2.0\n\n[cms]"),
            "experience",
        );
        refused(
            &VALID.replace(
                "\"sts.us-east-1.amazonaws.com\",",
                "\"cognito-identity.us-east-1.amazonaws.com\",",
            ),
            "aws_mirror.hosts",
        );
        refused(
            &VALID.replace(
                "\"sts.us-east-1.amazonaws.com\",",
                "\"STS.us-east-1.amazonaws.com\",",
            ),
            "aws_mirror.hosts",
        );
        refused(
            &VALID.replace(
                "\"sts.us-east-1.amazonaws.com\",",
                "\"kinesis.us-east-1.amazonaws.com\",",
            ),
            "aws_mirror.hosts",
        );
        refused(
            &VALID.replace(
                "\"ags-javelin-remote-config.s3.amazonaws.com\",",
                "\"ags-javelin-dev-remote-config.s3.amazonaws.com\",",
            ),
            "aws_mirror.hosts",
        );
        for required in [CMS_HOST, CMS_IMAGE_HOST, "dynamodb.us-east-1.amazonaws.com"] {
            refused(
                &VALID.replace(&format!("    \"{required}\",\n"), ""),
                "aws_mirror.hosts",
            );
        }
        refused(
            &VALID.replace(
                "\"client.content-service.amazongames.com\",",
                "\"client.catalogservice.amazongames.com\",",
            ),
            "aws_mirror.hosts",
        );
    }

    #[test]
    fn queue_limit_must_allow_normal_refreshes() {
        // A refresh every second needs 61 requests per minute (60 refreshes and the join).
        refused(
            &VALID.replace(
                "refresh_interval_seconds = 5",
                "refresh_interval_seconds = 1",
            ),
            "world.queue_requests_per_minute",
        );
        assert!(
            load(&VALID.replace(
                "queue_requests_per_minute = 30",
                "queue_requests_per_minute = 13"
            ))
            .is_ok()
        );
        refused(
            &VALID.replace(
                "queue_requests_per_minute = 30",
                "queue_requests_per_minute = 12",
            ),
            "world.queue_requests_per_minute",
        );
    }

    #[test]
    fn world_password_must_come_from_a_secret_variable() {
        refused(
            &VALID.replace("\"NW_SECRET_WORLD_1_DATABASE_PASSWORD\"", "\"HOME\""),
            "world.database_password_env",
        );
    }

    #[test]
    fn duplicate_world_ids_are_refused() {
        let start = VALID.find("[[world]]").expect("world block");
        let twice = format!("{VALID}{}", &VALID[start..]);
        refused(&twice, "world.id");
    }

    #[test]
    fn worlds_cannot_share_a_rep_gateway() {
        let start = VALID.find("[[world]]").expect("world block");
        let second = VALID[start..].replace(FIRST_WORLD_ID, SECOND_WORLD_ID);
        refused(&format!("{VALID}{second}"), "world.rep_address");
    }

    #[test]
    fn two_worlds_with_their_own_rep_gateways_load() {
        let config = load(&two_worlds()).expect("two worlds");
        assert_eq!(config.worlds.len(), 2);
        assert_ne!(config.worlds[0].rep_address, config.worlds[1].rep_address);
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(matches!(
            load(&format!("{VALID}\n[extra]\nkey = 1\n")),
            Err(EdgeConfigError::Load(_))
        ));
    }

    #[test]
    fn the_allow_list_is_off_by_default_and_names_who_may_log_in_when_on() {
        let mut discord = DiscordConfig::default();
        assert!(!discord.allow_list);
        assert!(
            discord.allows("80351110224678912", &[]),
            "off: every Discord user"
        );
        discord.allow_list = true;
        discord.allowed_user_ids = vec!["80351110224678912".to_owned()];
        assert!(discord.allows("80351110224678912", &[]));
        assert!(!discord.allows("175928847299117063", &[]));
        discord.allowed_role_ids = vec![ROLE.to_owned()];
        assert!(
            discord.allows(
                "175928847299117063",
                &[OTHER_ROLE.to_owned(), ROLE.to_owned()]
            ),
            "a holder of an allowed role"
        );
        assert!(!discord.allows("175928847299117063", &[OTHER_ROLE.to_owned()]));
    }
}
