// SPDX-License-Identifier: AGPL-3.0-or-later
//! Discord OAuth2 with PKCE (M2 spec section 5.3). Only the server talks to Discord; it holds the
//! client secret. Scopes: `identify email`, plus `guilds.members.read` when logins need a
//! Discord server (`discord.guild_id`).

use std::future::Future;

use data_encoding::BASE64URL_NOPAD;
use nw_db::DiscordProfile;
use nw_protocol::launcher_api::DISCORD_AUTHORIZE_URL;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Discord token endpoint.
pub const DISCORD_TOKEN_URL: &str = "https://discord.com/api/v10/oauth2/token";
/// Discord current-user endpoint.
pub const DISCORD_ME_URL: &str = "https://discord.com/api/v10/users/@me";
/// Base of the current user's server endpoints: `<base>/<guild id>/member`.
pub const DISCORD_MY_GUILDS_URL: &str = "https://discord.com/api/v10/users/@me/guilds";
/// Scopes when no Discord server is needed: user id, names and email.
pub const DISCORD_SCOPE: &str = "identify email";
/// Scopes when logins need a Discord server: [`DISCORD_SCOPE`] plus the member check.
pub const DISCORD_GUILD_SCOPE: &str = "identify email guilds.members.read";
/// Callback path on `login.<domain>`.
pub const DISCORD_CALLBACK_PATH: &str = "/launcher/discord/callback";
/// Digits of a Discord id of a user, role or server (a snowflake: 17 digits in 2015, at most 20
/// for a u64).
pub const DISCORD_ID_DIGITS: std::ops::RangeInclusive<usize> = 17..=20;

/// True when `id` has the form of a Discord user, role or server id ([`DISCORD_ID_DIGITS`]
/// digits).
pub fn is_discord_id(id: &str) -> bool {
    DISCORD_ID_DIGITS.contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// The Discord user behind a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordUser {
    /// Ids, names, email and the role ids in the required server.
    pub profile: DiscordProfile,
    /// The user is in the required Discord server; `None` when no server was asked for.
    pub guild_member: Option<bool>,
}

/// The scopes of a login: [`DISCORD_GUILD_SCOPE`] when it needs a Discord server.
pub fn login_scope(required_guild_id: Option<&str>) -> &'static str {
    match required_guild_id {
        Some(_) => DISCORD_GUILD_SCOPE,
        None => DISCORD_SCOPE,
    }
}

/// Discord refused the code or could not be reached.
#[derive(Debug, thiserror::Error)]
#[error("Discord request failed")]
pub struct DiscordError(#[source] Box<dyn std::error::Error + Send + Sync>);

/// Exchanges a login code for the Discord user.
pub trait DiscordApi: Clone + Send + Sync + 'static {
    /// The user of an authorization `code` from the callback, and whether the user is in the
    /// server `required_guild_id`.
    fn user_for_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        required_guild_id: Option<&str>,
    ) -> impl Future<Output = Result<DiscordUser, DiscordError>> + Send;
}

/// PKCE S256 challenge of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()))
}

/// The Discord login page for one attempt.
pub fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    verifier: &str,
    scope: &str,
) -> Result<String, DiscordError> {
    let challenge = pkce_challenge(verifier);
    Url::parse_with_params(
        DISCORD_AUTHORIZE_URL,
        [
            ("response_type", "code"),
            ("client_id", client_id),
            ("scope", scope),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
    )
    .map(String::from)
    .map_err(|error| DiscordError(Box::new(error)))
}

/// Discord's HTTP API.
#[derive(Debug, Clone)]
pub struct DiscordHttp {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
}

#[derive(Deserialize)]
struct MeReply {
    id: String,
    username: String,
    global_name: Option<String>,
    email: Option<String>,
    verified: Option<bool>,
}

/// The current user's member record in one server: its role ids.
#[derive(Deserialize)]
struct MemberReply {
    roles: Vec<String>,
}

impl DiscordHttp {
    /// A client for one Discord app. Needs the process crypto provider
    /// ([`crate::install_crypto_provider`]).
    pub fn new(client_id: String, client_secret: String) -> Result<Self, DiscordError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| DiscordError(Box::new(error)))?;
        Ok(Self {
            client,
            client_id,
            client_secret,
        })
    }

    /// The role ids of the owner of `access_token` in the server `guild_id`; `None` when the
    /// user is not in it (Discord answers 404).
    async fn guild_member_roles(
        &self,
        access_token: &str,
        guild_id: &str,
    ) -> Result<Option<Vec<String>>, DiscordError> {
        let reply = self
            .client
            .get(format!("{DISCORD_MY_GUILDS_URL}/{guild_id}/member"))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(failed)?;
        if reply.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let member: MemberReply = reply
            .error_for_status()
            .map_err(failed)?
            .json()
            .await
            .map_err(failed)?;
        Ok(Some(member.roles))
    }
}

fn failed(error: reqwest::Error) -> DiscordError {
    DiscordError(Box::new(error))
}

impl DiscordApi for DiscordHttp {
    async fn user_for_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        required_guild_id: Option<&str>,
    ) -> Result<DiscordUser, DiscordError> {
        let token: TokenReply = self
            .client
            .post(DISCORD_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
            ])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?
            .json()
            .await
            .map_err(failed)?;
        let me: MeReply = self
            .client
            .get(DISCORD_ME_URL)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?
            .json()
            .await
            .map_err(failed)?;
        let member_roles = match required_guild_id {
            Some(guild_id) => Some(
                self.guild_member_roles(&token.access_token, guild_id)
                    .await?,
            ),
            None => None,
        };
        let guild_member = member_roles.as_ref().map(Option::is_some);
        Ok(DiscordUser {
            profile: DiscordProfile {
                user_id: me.id,
                display_name: me.global_name.unwrap_or_else(|| me.username.clone()),
                username: me.username,
                email: me.email,
                email_verified: me.verified.unwrap_or(false),
                guild_role_ids: member_roles.flatten().unwrap_or_default(),
            },
            guild_member,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Discord stand-in: the code `good` belongs to one fixed user, who is only in the
    /// server [`FAKE_GUILD_ID`].
    #[derive(Debug, Clone)]
    pub(crate) struct FakeDiscord;

    /// The code [`FakeDiscord`] accepts.
    pub(crate) const GOOD_CODE: &str = "good";
    /// The user of [`GOOD_CODE`].
    pub(crate) const FAKE_USER_ID: &str = "80351110224678912";
    /// The Discord username of [`FAKE_USER_ID`].
    pub(crate) const FAKE_USERNAME: &str = "nelly";
    /// The email of [`FAKE_USER_ID`].
    pub(crate) const FAKE_EMAIL: &str = "nelly@example.org";
    /// The only server [`FAKE_USER_ID`] is in.
    pub(crate) const FAKE_GUILD_ID: &str = "1234567890123456789";
    /// The one role [`FAKE_USER_ID`] holds in [`FAKE_GUILD_ID`].
    pub(crate) const FAKE_ROLE_ID: &str = "1000000000000000003";

    impl DiscordApi for FakeDiscord {
        async fn user_for_code(
            &self,
            code: &str,
            _verifier: &str,
            _redirect_uri: &str,
            required_guild_id: Option<&str>,
        ) -> Result<DiscordUser, DiscordError> {
            if code == GOOD_CODE {
                Ok(DiscordUser {
                    profile: DiscordProfile {
                        user_id: FAKE_USER_ID.to_owned(),
                        username: FAKE_USERNAME.to_owned(),
                        display_name: "Nelly".to_owned(),
                        email: Some(FAKE_EMAIL.to_owned()),
                        email_verified: true,
                        guild_role_ids: if required_guild_id == Some(FAKE_GUILD_ID) {
                            vec![FAKE_ROLE_ID.to_owned()]
                        } else {
                            Vec::new()
                        },
                    },
                    guild_member: required_guild_id.map(|guild_id| guild_id == FAKE_GUILD_ID),
                })
            } else {
                Err(DiscordError(Box::new(std::io::Error::other("bad code"))))
            }
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_has_every_parameter() {
        let url = authorize_url(
            "123",
            "https://login.example.org/launcher/discord/callback",
            "st",
            "ver",
            login_scope(None),
        )
        .expect("url");
        assert!(
            url.starts_with(
                "https://discord.com/oauth2/authorize?response_type=code&client_id=123"
            )
        );
        assert!(url.contains("scope=identify+email&"));
        assert!(url.contains(
            "redirect_uri=https%3A%2F%2Flogin.example.org%2Flauncher%2Fdiscord%2Fcallback"
        ));
        assert!(url.contains("state=st"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn a_required_server_adds_the_member_scope() {
        assert_eq!(login_scope(None), DISCORD_SCOPE);
        assert_eq!(login_scope(Some(FAKE_GUILD_ID)), DISCORD_GUILD_SCOPE);
        let url = authorize_url(
            "123",
            "https://login.example.org/launcher/discord/callback",
            "st",
            "ver",
            login_scope(Some(FAKE_GUILD_ID)),
        )
        .expect("url");
        assert!(url.contains("scope=identify+email+guilds.members.read&"));
    }

    #[test]
    fn a_discord_id_is_17_to_20_digits() {
        assert!(is_discord_id("80351110224678912"));
        assert!(is_discord_id("18446744073709551615"));
        for bad in [
            "8035111022467891",
            "184467440737095516150",
            "nelly",
            "8035111022467891x",
        ] {
            assert!(!is_discord_id(bad), "{bad}");
        }
    }
}
