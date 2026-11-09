// SPDX-License-Identifier: AGPL-3.0-or-later
//! JSON between the `launcher` and `edge-http` (M2 spec sections 5 and 6). This API is ours, not
//! the game client's, so its keys use snake_case.

use serde::{Deserialize, Serialize};

use crate::aws_mirror::AwsMirror;

/// `GET` server name, login mode and launch arguments.
pub const INFO_PATH: &str = "/launcher/info";
/// `POST` password sign-up.
pub const REGISTER_PATH: &str = "/launcher/register";
/// `POST` password login.
pub const LOGIN_PATH: &str = "/launcher/login";
/// `POST` new game session key.
pub const PLAY_PATH: &str = "/launcher/play";
/// `POST` end the launcher session.
pub const LOGOUT_PATH: &str = "/launcher/logout";
/// `GET` where the launcher reads the server's news ([`NewsSource`]). Old servers answer `404`:
/// then there is no news.
pub const NEWS_SOURCE_PATH: &str = "/launcher/news-source";
/// `POST` start a Discord login.
pub const DISCORD_START_PATH: &str = "/launcher/discord/start";
/// `POST` ask whether a Discord login finished.
pub const DISCORD_POLL_PATH: &str = "/launcher/discord/poll";

/// Discord login page. The server builds its login links on it; the launcher opens only links
/// to this page.
pub const DISCORD_AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";

/// Placeholder in [`LauncherInfo::launch_arguments`] that the launcher replaces with the game
/// session key.
pub const SESSION_PLACEHOLDER: &str = "{session}";

/// Domains that a community server, and the launcher, must never use (M2 spec section 3.4). A
/// host equal to one of these or ending in `.<domain>` is refused.
pub const FORBIDDEN_DOMAINS: [&str; 5] = [
    "amazon.com",
    "amazonaws.com",
    "amazongames.com",
    "cloudfront.net",
    "a2z.com",
];

/// True when `host` is a forbidden domain or a sub-domain of one.
pub fn is_forbidden_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    FORBIDDEN_DOMAINS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// How players log in on a server. One mode per server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Username and password, made in the launcher.
    Password,
    /// Discord login in the browser.
    Discord,
}

/// Reply of `GET /launcher/info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherInfo {
    /// Name shown in the launcher.
    pub server_name: String,
    /// Login mode of this server.
    pub auth_mode: AuthMode,
    /// Whether password sign-up is open.
    pub allow_registration: bool,
    /// Game launch arguments; [`SESSION_PLACEHOLDER`] stands for the game session key.
    pub launch_arguments: Vec<String>,
    /// The Amazon names this server answers for the game process, with its certificate.
    pub aws_mirror: AwsMirror,
}

/// Body of `POST /launcher/register` and `POST /launcher/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordCredentials {
    /// Username as typed; the server stores it in lower case.
    pub username: String,
    /// Password.
    pub password: String,
}

/// Reply of a successful login: a launcher session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherSession {
    /// Bearer token for later launcher calls.
    pub launcher_token: String,
    /// Expiry, Unix seconds.
    pub expires_at: i64,
}

/// Reply of `POST /launcher/play`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayReply {
    /// One game session key for one client start.
    pub game_session_key: String,
}

/// Reply of `POST /launcher/discord/start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordStart {
    /// Discord login page to open in the browser.
    pub authorize_url: String,
    /// Login attempt id for [`DiscordPoll`].
    pub state: String,
}

/// Body of `POST /launcher/discord/poll`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordPoll {
    /// Login attempt id from [`DiscordStart`].
    pub state: String,
}

/// Reply of `POST /launcher/discord/poll`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiscordPollReply {
    /// The player has not finished the Discord login yet.
    Pending,
    /// Login finished.
    Ready(LauncherSession),
}

/// Reply of `GET /launcher/news-source`: the site feed the launcher reads its news from. It is a
/// path of its own, so [`LauncherInfo`] stays the same for older launchers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewsSource {
    /// The feed (`https://<site>/api/news?target=launcher`); `None` when the server has no site.
    pub news_url: Option<String>,
}

/// Error body of every `edge-http` route that has no client-defined error shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    /// Stable snake_case error code.
    pub code: String,
    /// Text for people.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use crate::aws_mirror::CLIENT_PING_PORT;

    use super::*;

    const TOKEN: &str = "launcher-token";
    const EXPIRES_AT: i64 = 1_760_400_000;

    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("json object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    fn json<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).expect("serialize")
    }

    #[test]
    fn forbidden_hosts_are_detected() {
        assert!(is_forbidden_host("cloudfront.net"));
        assert!(is_forbidden_host("d2c74t4zimux3r.CloudFront.net."));
        assert!(is_forbidden_host("client.amazongames.com"));
        assert!(!is_forbidden_host("notcloudfront.net"));
        assert!(!is_forbidden_host("example.org"));
    }

    #[test]
    fn news_source_has_the_documented_keys() {
        let source = NewsSource {
            news_url: Some("https://example.org/api/news?target=launcher".to_owned()),
        };
        assert_eq!(keys(&json(&source)), ["news_url"]);
        let none: NewsSource = serde_json::from_str(r#"{"news_url":null}"#).expect("none");
        assert_eq!(none.news_url, None);
    }

    #[test]
    fn info_has_the_documented_keys() {
        let info = LauncherInfo {
            server_name: "Test".to_owned(),
            auth_mode: AuthMode::Discord,
            allow_registration: false,
            launch_arguments: vec![
                "-LoginChannelOverride".to_owned(),
                SESSION_PLACEHOLDER.to_owned(),
            ],
            aws_mirror: AwsMirror {
                hosts: vec!["ags-javelin-remote-config.s3.amazonaws.com".to_owned()],
                certificate_pem: "-----BEGIN CERTIFICATE-----".to_owned(),
                ping_port: CLIENT_PING_PORT,
            },
        };
        let value = json(&info);
        assert_eq!(
            keys(&value),
            [
                "allow_registration",
                "auth_mode",
                "aws_mirror",
                "launch_arguments",
                "server_name"
            ]
        );
        assert_eq!(
            keys(&value["aws_mirror"]),
            ["certificate_pem", "hosts", "ping_port"]
        );
        assert_eq!(value["auth_mode"], "discord");
        assert_eq!(
            serde_json::from_value::<LauncherInfo>(value).expect("parse"),
            info
        );
    }

    #[test]
    fn poll_reply_is_tagged_by_status() {
        let ready = DiscordPollReply::Ready(LauncherSession {
            launcher_token: TOKEN.to_owned(),
            expires_at: EXPIRES_AT,
        });
        let value = json(&ready);
        assert_eq!(keys(&value), ["expires_at", "launcher_token", "status"]);
        assert_eq!(value["status"], "ready");
        assert_eq!(
            json(&DiscordPollReply::Pending),
            serde_json::json!({"status": "pending"})
        );
        assert_eq!(
            serde_json::from_value::<DiscordPollReply>(value).expect("parse"),
            ready
        );
    }

    #[test]
    fn unknown_keys_are_refused() {
        let text = r#"{"username":"ada","password":"secret-1","extra":1}"#;
        assert!(serde_json::from_str::<PasswordCredentials>(text).is_err());
    }

    #[test]
    fn error_body_round_trip() {
        let error = ErrorBody {
            code: "bad_session".to_owned(),
            message: "log in again".to_owned(),
        };
        assert_eq!(keys(&json(&error)), ["code", "message"]);
        assert_eq!(
            serde_json::from_value::<ErrorBody>(json(&error)).expect("parse"),
            error
        );
    }
}
