// SPDX-License-Identifier: AGPL-3.0-or-later
//! `config/blighted-admin.toml` with `BLIGHTED_ADMIN_` overrides (admin tools spec section 6).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use nw_common::config::{ConfigError, load_config};
use nw_db::DatabaseConfig;
use nw_login::config::TlsConfig;

use crate::access::AccessMode;
use serde::Deserialize;
use uuid::Uuid;

/// Prefix of the environment overrides.
pub const ADMIN_ENV_PREFIX: &str = "BLIGHTED_ADMIN_";
/// Shortest scrape and state interval.
pub const MIN_LIVE_INTERVAL_MS: u64 = 100;
const INTERVAL_TOO_SHORT: &str = "is shorter than MIN_LIVE_INTERVAL_MS";
const NOT_LETTERS: &str = "must be one or more letters a to z or A to Z";

/// The admin tools configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// HTTP address: loopback in local mode, the HTTPS address in server mode, absent with
    /// `[access.relay]` (the relay serves the browsers).
    pub listen: Option<SocketAddr>,
    /// `tracing` filter.
    pub log_filter: String,
    /// File with the pinned client build.
    pub client_build_config: PathBuf,
    /// Folder of the world files, the content bundle and `map/`.
    pub content_dir: PathBuf,
    /// Level of the map.
    pub level: String,
    /// Most spawn points one request returns.
    pub max_spawns_per_request: usize,
    /// The capture viewer.
    pub captures: CapturesConfig,
    /// The live panel; without it the live routes are 404.
    pub live: Option<LiveConfig>,
    /// The studio (design files and Apply); without it the studio routes are 404.
    pub studio: Option<StudioConfig>,
    /// Who may use the site; without it, local mode.
    pub access: Option<AccessConfig>,
}

/// The access part of the configuration (admin studio spec sections 4 and 9).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessConfig {
    /// Local (loopback, no login) or server (HTTPS, logins, ranks).
    pub mode: AccessMode,
    /// Server mode: the host names (and ports) the site is reached by, for the request checks.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The global database of the game accounts; server mode needs it, local mode uses it only
    /// for the audit log.
    pub global_database: Option<DatabaseConfig>,
    /// Name of the variable that holds the global database password.
    pub global_database_password_env: Option<String>,
    /// Server mode: the certificate and key.
    pub tls: Option<TlsConfig>,
    /// Server mode: log in with Discord, for accounts made by a Discord login in the game.
    pub discord: Option<AdminDiscordConfig>,
    /// Server mode: no listener; this site connects to an admin relay on another machine
    /// (`blighted-admin relay`), which serves the browsers.
    pub relay: Option<RelayLinkConfig>,
}

/// `[access.relay]`: the relay this site connects to (admin relay spec section 5.2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayLinkConfig {
    /// The relay's agent address, `host:port`.
    pub address: String,
    /// The DNS name of the relay certificate.
    pub server_name: String,
    /// The relay certificate (`relay.pem` of `relay-keys`); the relay must show exactly it.
    pub relay_certificate_pem_path: PathBuf,
    /// This site's certificate (`agent.pem`), which the relay trusts.
    pub certificate_pem_path: PathBuf,
    /// Its private key (`agent-key.pem`).
    pub private_key_pem_path: PathBuf,
}

/// How the browsers reach the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front<'a> {
    /// Local mode: plain HTTP on a loopback address.
    Local(SocketAddr),
    /// Server mode: HTTPS on `listen` for the `[access] hosts`.
    Https(SocketAddr, &'a TlsConfig, &'a [String]),
    /// Server mode through a relay, for the `[access] hosts`.
    Relay(&'a RelayLinkConfig, &'a [String]),
}

impl AdminConfig {
    /// How the browsers reach the site, from `listen` and `[access]`.
    ///
    /// # Errors
    ///
    /// Returns [`AdminConfigError`] when `listen` and `[access]` do not fit together.
    pub fn front(&self) -> Result<Front<'_>, AdminConfigError> {
        let server = |access: &AccessConfig| {
            access.global_database.is_some()
                && access.global_database_password_env.is_some()
                && !access.hosts.is_empty()
        };
        match self.access.as_ref() {
            Some(access) if access.mode == AccessMode::Server => {
                match (&access.relay, &access.tls, self.listen) {
                    (Some(relay), None, None) if server(access) => {
                        Ok(Front::Relay(relay, &access.hosts))
                    }
                    (Some(_), ..) => Err(AdminConfigError::Invalid {
                        key: "access.relay",
                        reason: "relay mode needs global_database, global_database_password_env and hosts, and takes no tls and no listen",
                    }),
                    (None, Some(tls), Some(listen)) if server(access) => {
                        Ok(Front::Https(listen, tls, &access.hosts))
                    }
                    (None, ..) => Err(AdminConfigError::Invalid {
                        key: "access",
                        reason: "server mode needs listen, tls, global_database, global_database_password_env and hosts",
                    }),
                }
            }
            Some(access) if access.discord.is_some() => Err(AdminConfigError::Invalid {
                key: "access.discord",
                reason: "Discord login needs server mode",
            }),
            Some(access) if access.relay.is_some() => Err(AdminConfigError::Invalid {
                key: "access.relay",
                reason: "a relay needs server mode",
            }),
            _ => match self.listen {
                Some(listen) if listen.ip().is_loopback() => Ok(Front::Local(listen)),
                Some(listen) => Err(AdminConfigError::NotLoopback(listen)),
                None => Err(AdminConfigError::Invalid {
                    key: "listen",
                    reason: "local mode needs a loopback listen address",
                }),
            },
        }
    }
}

/// `[access.discord]`: the Discord app of the game login (the same client id and secret).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminDiscordConfig {
    /// Discord application (client) id.
    pub client_id: String,
    /// Name of the variable that holds the client secret.
    pub client_secret_env: String,
}

impl AccessConfig {
    /// The mode, and local mode when the section is absent.
    pub fn mode_of(access: Option<&Self>) -> AccessMode {
        access.map_or(AccessMode::Local, |access| access.mode)
    }
}

/// The studio part of the configuration (admin studio spec sections 5.2 to 5.4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    /// The design files (`content/`).
    pub design_dir: PathBuf,
    /// The retail bundle and world files from `content-tool build` and `world`.
    pub retail_dir: PathBuf,
    /// The control FIFO of `scripts/live-stack.sh`.
    pub control_fifo: PathBuf,
    /// Longest wait for every sim to start again after Apply restarts them.
    pub restart_timeout_s: u64,
    /// The GitHub API the to-do import reads issues from.
    #[serde(default = "default_github_api_url")]
    pub github_api_url: String,
    /// The environment variable with a GitHub token, for issues of a private repository.
    #[serde(default)]
    pub github_token_env: Option<String>,
}

/// The public GitHub API.
pub const GITHUB_API_URL: &str = "https://api.github.com";

fn default_github_api_url() -> String {
    GITHUB_API_URL.to_owned()
}

/// The live panel part of the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveConfig {
    /// The bus.
    pub nats_url: String,
    /// The world, as in the world-director configuration.
    pub world_id: Uuid,
    /// The metrics pages of the servers.
    pub metrics_urls: Vec<String>,
    /// Time between scrapes (also the CPU sample time).
    pub scrape_interval_ms: u64,
    /// Time between two states sent to the page.
    pub state_interval_ms: u64,
    /// Program names shown in the CPU tile.
    pub processes: Vec<String>,
    /// The `nw-testclient` program of the bot batches.
    pub testclient_bin: PathBuf,
    /// Its configuration.
    pub testclient_config: PathBuf,
    /// `KEY=VALUE` file; only its `NW_SECRET_*` lines go to the batches.
    pub testclient_env_file: PathBuf,
    /// Account prefix of the batches; the batch letters follow it.
    pub bot_account_prefix: String,
    /// Character prefix of the batches (letters only); the batch letters follow it.
    pub bot_character_prefix: String,
    /// Character prefixes (letters only) of bots started outside this program, such as
    /// `nw-testclient` runs; a player whose name starts with one is shown as a bot.
    pub other_bot_prefixes: Vec<String>,
    /// Most bots of all batches together.
    pub max_bots: u32,
}

impl LiveConfig {
    /// Every character prefix of bots: the batches' and the others.
    pub fn bot_prefixes(&self) -> Vec<String> {
        std::iter::once(self.bot_character_prefix.clone())
            .chain(self.other_bot_prefixes.iter().cloned())
            .collect()
    }
}

/// The capture viewer part of the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturesConfig {
    /// Folder searched for `ledger.bin` files.
    pub captures_dir: PathBuf,
    /// Saved Discord threads of the open-world sessions.
    pub discord_dir: PathBuf,
    /// The local Azoth catalog archive.
    pub azoth_dir: PathBuf,
    /// The client type table, relative to the install folder.
    pub type_index: PathBuf,
    /// Aeternum-World class names.
    pub type_names: PathBuf,
}

/// A configuration that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum AdminConfigError {
    /// The file or an override cannot be read.
    #[error(transparent)]
    Load(#[from] ConfigError),
    /// `listen` is not a loopback address: the tools have no login.
    #[error("listen address {0} is not a loopback address")]
    NotLoopback(SocketAddr),
    /// A value is out of range.
    #[error("config value `{key}` is not valid: {reason}")]
    Invalid {
        /// Key.
        key: &'static str,
        /// Why.
        reason: &'static str,
    },
}

/// Reads and checks `path` with `BLIGHTED_ADMIN_` overrides.
///
/// # Errors
///
/// Returns [`AdminConfigError`] when the file cannot be read or a value is not allowed.
pub fn load_admin_config(path: &Path) -> Result<AdminConfig, AdminConfigError> {
    let config: AdminConfig = load_config(path, ADMIN_ENV_PREFIX)?;
    config.front()?;
    if config.max_spawns_per_request == 0 {
        return Err(AdminConfigError::Invalid {
            key: "max_spawns_per_request",
            reason: "must be at least 1",
        });
    }
    if config
        .studio
        .as_ref()
        .is_some_and(|studio| studio.restart_timeout_s == 0)
    {
        return Err(AdminConfigError::Invalid {
            key: "studio.restart_timeout_s",
            reason: "must be at least 1",
        });
    }
    if let Some(live) = &config.live {
        for (key, interval) in [
            ("live.scrape_interval_ms", live.scrape_interval_ms),
            ("live.state_interval_ms", live.state_interval_ms),
        ] {
            if interval < MIN_LIVE_INTERVAL_MS {
                return Err(AdminConfigError::Invalid {
                    key,
                    reason: INTERVAL_TOO_SHORT,
                });
            }
        }
        let letters = |prefix: &String| {
            !prefix.is_empty() && prefix.chars().all(|letter| letter.is_ascii_alphabetic())
        };
        if !letters(&live.bot_character_prefix) {
            return Err(AdminConfigError::Invalid {
                key: "live.bot_character_prefix",
                reason: NOT_LETTERS,
            });
        }
        if !live.other_bot_prefixes.iter().all(letters) {
            return Err(AdminConfigError::Invalid {
                key: "live.other_bot_prefixes",
                reason: NOT_LETTERS,
            });
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_committed_config_loads() {
        let config =
            load_admin_config(&repo_root().join("config/blighted-admin.toml")).expect("load");
        assert!(matches!(config.front(), Ok(Front::Local(listen)) if listen.ip().is_loopback()));
        assert!(config.max_spawns_per_request > 0);
    }

    fn load_changed(from: &str, to: &str) -> Result<AdminConfig, AdminConfigError> {
        let committed =
            std::fs::read_to_string(repo_root().join("config/blighted-admin.toml")).expect("read");
        let changed = committed.replace(from, to);
        assert_ne!(changed, committed);
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("blighted-admin.toml");
        std::fs::write(&path, changed).expect("write");
        load_admin_config(&path)
    }

    #[test]
    fn the_committed_config_has_a_live_section() {
        let config =
            load_admin_config(&repo_root().join("config/blighted-admin.toml")).expect("load");
        let live = config.live.expect("live section");
        assert!(!live.metrics_urls.is_empty());
        assert!(live.scrape_interval_ms >= MIN_LIVE_INTERVAL_MS);
        assert!(live.state_interval_ms >= MIN_LIVE_INTERVAL_MS);
        assert!(live.max_bots > 0);
    }

    #[test]
    fn the_committed_config_has_a_studio_section() {
        let config =
            load_admin_config(&repo_root().join("config/blighted-admin.toml")).expect("load");
        let studio = config.studio.expect("studio section");
        assert_eq!(studio.design_dir, Path::new("content"));
        assert_eq!(studio.retail_dir, Path::new("local-data/content/retail"));
        assert_eq!(
            studio.control_fifo,
            Path::new("local-data/logs/live/control")
        );
        assert_eq!(studio.restart_timeout_s, 600);
    }

    #[test]
    fn a_zero_restart_timeout_is_refused() {
        assert!(matches!(
            load_changed("restart_timeout_s = 600", "restart_timeout_s = 0"),
            Err(AdminConfigError::Invalid {
                key: "studio.restart_timeout_s",
                ..
            })
        ));
    }

    #[test]
    fn a_live_interval_below_the_minimum_is_refused() {
        for (from, to, key) in [
            (
                "scrape_interval_ms = 1000",
                "scrape_interval_ms = 10",
                "live.scrape_interval_ms",
            ),
            (
                "state_interval_ms = 250",
                "state_interval_ms = 0",
                "live.state_interval_ms",
            ),
        ] {
            assert!(
                matches!(load_changed(from, to), Err(AdminConfigError::Invalid { key: found, .. }) if found == key),
                "{key}"
            );
        }
    }

    #[test]
    fn another_bot_prefix_with_digits_is_refused() {
        assert!(matches!(
            load_changed(
                "other_bot_prefixes = [\"Swarm\"",
                "other_bot_prefixes = [\"Swarm1\""
            ),
            Err(AdminConfigError::Invalid {
                key: "live.other_bot_prefixes",
                ..
            })
        ));
    }

    #[test]
    fn a_character_prefix_with_digits_is_refused() {
        assert!(matches!(
            load_changed(
                "bot_character_prefix = \"Adminbot\"",
                "bot_character_prefix = \"Admin1\""
            ),
            Err(AdminConfigError::Invalid {
                key: "live.bot_character_prefix",
                ..
            })
        ));
    }

    const SERVER_ACCESS: &str = "mode = \"server\"\nhosts = [\"admin.example.test\"]\nglobal_database_password_env = \"NW_SECRET_GLOBAL_DATABASE_PASSWORD\"\nglobal_database = { url = \"postgres://nw@127.0.0.1:5432/nw_global\", max_connections = 2 }\ntls = { certificate_pem_path = \"cert.pem\", private_key_pem_path = \"key.pem\" }\n";

    #[test]
    fn the_committed_config_is_local_mode() {
        let config =
            load_admin_config(&repo_root().join("config/blighted-admin.toml")).expect("load");
        assert_eq!(
            config.access.map(|access| access.mode),
            Some(AccessMode::Local)
        );
    }

    #[test]
    fn server_mode_needs_tls_a_database_and_hosts() {
        assert!(matches!(
            load_changed("mode = \"local\"", "mode = \"server\""),
            Err(AdminConfigError::Invalid { key: "access", .. })
        ));
    }

    #[test]
    fn server_mode_may_listen_off_loopback() {
        let committed =
            std::fs::read_to_string(repo_root().join("config/blighted-admin.toml")).expect("read");
        let changed = committed
            .replace("mode = \"local\"\n", SERVER_ACCESS)
            .replace("listen = \"127.0.0.1:", "listen = \"0.0.0.0:");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("blighted-admin.toml");
        std::fs::write(&path, changed).expect("write");
        let config = load_admin_config(&path).expect("server config");
        let access = config.access.expect("access");
        assert_eq!(access.mode, AccessMode::Server);
        assert_eq!(access.hosts, ["admin.example.test"]);
    }

    /// `[access.discord]` of the tests.
    const DISCORD_ACCESS: &str = "[access.discord]\nclient_id = \"1567248535328522280\"\nclient_secret_env = \"NW_SECRET_DISCORD_CLIENT_SECRET\"\n";

    #[test]
    fn discord_login_is_read_in_server_mode_and_refused_in_local_mode() {
        let committed =
            std::fs::read_to_string(repo_root().join("config/blighted-admin.toml")).expect("read");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("blighted-admin.toml");
        let server = committed.replace(
            "mode = \"local\"\n",
            &format!("{SERVER_ACCESS}{DISCORD_ACCESS}"),
        );
        std::fs::write(&path, server).expect("write");
        let discord = load_admin_config(&path)
            .expect("server config")
            .access
            .and_then(|access| access.discord)
            .expect("discord");
        assert_eq!(discord.client_id, "1567248535328522280");
        assert_eq!(discord.client_secret_env, "NW_SECRET_DISCORD_CLIENT_SECRET");
        assert!(matches!(
            load_changed(
                "mode = \"local\"\n",
                &format!("mode = \"local\"\n{DISCORD_ACCESS}")
            ),
            Err(AdminConfigError::Invalid {
                key: "access.discord",
                ..
            })
        ));
    }

    #[test]
    fn a_listen_address_off_loopback_is_refused() {
        assert!(matches!(
            load_changed("listen = \"127.0.0.1:", "listen = \"0.0.0.0:"),
            Err(AdminConfigError::NotLoopback(_))
        ));
    }
    /// Server mode without tls, and the `[access.relay]` of the tests.
    const RELAY_ACCESS: &str = "mode = \"server\"\nhosts = [\"admin.example.test\"]\nglobal_database_password_env = \"NW_SECRET_GLOBAL_DATABASE_PASSWORD\"\nglobal_database = { url = \"postgres://nw@127.0.0.1:5432/nw_global\", max_connections = 2 }\n[access.relay]\naddress = \"relay.example.test:9191\"\nserver_name = \"relay.example.test\"\nrelay_certificate_pem_path = \"relay.pem\"\ncertificate_pem_path = \"agent.pem\"\nprivate_key_pem_path = \"agent-key.pem\"\n";
    const COMMITTED_LISTEN: &str = "listen = \"127.0.0.1:9180\"\n";

    fn load_text(text: &str) -> Result<AdminConfig, AdminConfigError> {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("blighted-admin.toml");
        std::fs::write(&path, text).expect("write");
        load_admin_config(&path)
    }

    fn committed() -> String {
        std::fs::read_to_string(repo_root().join("config/blighted-admin.toml")).expect("read")
    }

    #[test]
    fn relay_mode_has_no_listener_and_names_the_hosts() {
        let text = committed()
            .replace("mode = \"local\"\n", RELAY_ACCESS)
            .replace(COMMITTED_LISTEN, "");
        let config = load_text(&text).expect("relay config");
        let Ok(Front::Relay(relay, hosts)) = config.front() else {
            panic!("not relay mode");
        };
        assert_eq!(relay.address, "relay.example.test:9191");
        assert_eq!(relay.server_name, "relay.example.test");
        assert_eq!(hosts, ["admin.example.test"]);
    }

    #[test]
    fn relay_mode_with_a_listener_or_tls_is_refused() {
        let with_listen = committed().replace("mode = \"local\"\n", RELAY_ACCESS);
        let relay_access_with_tls = RELAY_ACCESS.replace(
            "[access.relay]",
            "tls = { certificate_pem_path = \"cert.pem\", private_key_pem_path = \"key.pem\" }\n[access.relay]",
        );
        let with_tls = committed()
            .replace("mode = \"local\"\n", &relay_access_with_tls)
            .replace(COMMITTED_LISTEN, "");
        for text in [with_listen, with_tls] {
            assert!(matches!(
                load_text(&text),
                Err(AdminConfigError::Invalid {
                    key: "access.relay",
                    ..
                })
            ));
        }
    }

    #[test]
    fn a_relay_in_local_mode_is_refused() {
        let text = committed().replace(
            "mode = \"local\"\n",
            &RELAY_ACCESS.replace("mode = \"server\"", "mode = \"local\""),
        );
        assert!(matches!(
            load_text(&text),
            Err(AdminConfigError::Invalid {
                key: "access.relay",
                ..
            })
        ));
    }

    #[test]
    fn local_mode_needs_a_listen_address() {
        assert!(matches!(
            load_text(&committed().replace(COMMITTED_LISTEN, "")),
            Err(AdminConfigError::Invalid { key: "listen", .. })
        ));
    }
}
