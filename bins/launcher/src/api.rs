// SPDX-License-Identifier: GPL-3.0-or-later
//! Client of the server's launcher API (M2 spec section 5). Calls block; the window runs them
//! on a worker thread.

use std::time::Duration;

use nw_protocol::launcher_api::{
    DISCORD_AUTHORIZE_URL, DISCORD_POLL_PATH, DISCORD_START_PATH, DiscordPoll, DiscordPollReply,
    DiscordStart, ErrorBody, INFO_PATH, LOGIN_PATH, LOGOUT_PATH, LauncherInfo, LauncherSession,
    PLAY_PATH, PasswordCredentials, PlayReply, REGISTER_PATH,
};
use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder};
use serde::de::DeserializeOwned;

use crate::server::{api_host, api_host_name, api_port};

/// Time limit of one call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// A failed call.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The server could not be reached.
    #[error("server unreachable: {0}")]
    Unreachable(#[source] reqwest::Error),
    /// The server refused the call.
    #[error("{message}")]
    Server {
        /// HTTP status.
        status: u16,
        /// Error code from the body.
        code: String,
        /// Error text from the body.
        message: String,
    },
    /// The reply is not the expected JSON.
    #[error("server reply is not valid: {0}")]
    BadReply(#[source] reqwest::Error),
    /// The server sent a login link that is not Discord's login page.
    #[error("the server sent a login link that is not Discord's login page: {0}")]
    NotDiscord(String),
}

/// The Discord login page from a server reply. Only `https://discord.com/oauth2/authorize` with a
/// query is accepted, so a server cannot make the launcher open another program or page (review
/// finding 1).
pub fn discord_login_page(authorize_url: &str) -> Result<Url, ApiError> {
    let not_discord = || ApiError::NotDiscord(authorize_url.to_owned());
    let page = Url::parse(authorize_url).map_err(|_| not_discord())?;
    let mut without_query = page.clone();
    without_query.set_query(None);
    if page.fragment().is_none() && without_query.as_str() == DISCORD_AUTHORIZE_URL {
        Ok(page)
    } else {
        Err(not_discord())
    }
}

/// The launcher API of one server.
#[derive(Debug, Clone)]
pub struct LauncherApi {
    base: String,
    client: Client,
}

impl LauncherApi {
    /// A client for the API at `base` (for example `https://login.example.org`). Needs the
    /// process crypto provider ([`crate::install_crypto_provider`]).
    pub fn new(base: String) -> Result<Self, ApiError> {
        // A redirect could send the launcher, and its token, to another host (review finding 10).
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CALL_TIMEOUT)
            .build()
            .map_err(ApiError::Unreachable)?;
        Ok(Self { base, client })
    }

    /// The API host and port as typed, for example `login.example.org` or
    /// `login.example.org:8443`.
    pub fn host(&self) -> &str {
        api_host(&self.base)
    }

    /// The API host name without the port, for example `login.example.org`.
    pub fn host_name(&self) -> &str {
        api_host_name(&self.base)
    }

    /// The API's HTTPS port. The server answers its Amazon names on it too.
    pub fn port(&self) -> u16 {
        api_port(&self.base)
    }

    /// Server name, login mode and launch arguments.
    pub fn info(&self) -> Result<LauncherInfo, ApiError> {
        self.call(self.client.get(self.url(INFO_PATH)))
    }

    /// Makes a password account and logs in.
    pub fn register(&self, credentials: &PasswordCredentials) -> Result<LauncherSession, ApiError> {
        self.call(self.client.post(self.url(REGISTER_PATH)).json(credentials))
    }

    /// Logs in with a password.
    pub fn login(&self, credentials: &PasswordCredentials) -> Result<LauncherSession, ApiError> {
        self.call(self.client.post(self.url(LOGIN_PATH)).json(credentials))
    }

    /// A game session key for one client start.
    pub fn play(&self, launcher_token: &str) -> Result<PlayReply, ApiError> {
        self.call(
            self.client
                .post(self.url(PLAY_PATH))
                .bearer_auth(launcher_token),
        )
    }

    /// Ends the launcher session.
    pub fn logout(&self, launcher_token: &str) -> Result<(), ApiError> {
        let response = self
            .client
            .post(self.url(LOGOUT_PATH))
            .bearer_auth(launcher_token)
            .send()
            .map_err(ApiError::Unreachable)?;
        check_status(response).map(drop)
    }

    /// Starts a Discord login.
    pub fn discord_start(&self) -> Result<DiscordStart, ApiError> {
        self.call(self.client.post(self.url(DISCORD_START_PATH)))
    }

    /// Asks whether a Discord login finished.
    pub fn discord_poll(&self, state: &str) -> Result<DiscordPollReply, ApiError> {
        let body = DiscordPoll {
            state: state.to_owned(),
        };
        self.call(self.client.post(self.url(DISCORD_POLL_PATH)).json(&body))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn call<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ApiError> {
        let response = request.send().map_err(ApiError::Unreachable)?;
        check_status(response)?.json().map_err(ApiError::BadReply)
    }
}

fn check_status(
    response: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body: ErrorBody = response.json().map_err(ApiError::BadReply)?;
    Err(ApiError::Server {
        status: status.as_u16(),
        code: body.code,
        message: body.message,
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::Redirect;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use nw_protocol::aws_mirror::{AwsMirror, CLIENT_PING_PORT};
    use nw_protocol::launcher_api::{AuthMode, DISCORD_AUTHORIZE_URL};

    use super::*;

    const TOKEN: &str = "launcher-token";
    const KEY: &str = "game-session-key";
    const EXPIRES_AT: i64 = 1_760_400_000;
    /// Route that the redirect test sends the launcher to.
    const ELSEWHERE_PATH: &str = "/launcher/elsewhere";

    fn session() -> LauncherSession {
        LauncherSession {
            launcher_token: TOKEN.to_owned(),
            expires_at: EXPIRES_AT,
        }
    }

    fn fake_info() -> Json<LauncherInfo> {
        Json(LauncherInfo {
            server_name: "Fake".to_owned(),
            auth_mode: AuthMode::Password,
            allow_registration: true,
            launch_arguments: Vec::new(),
            aws_mirror: AwsMirror {
                hosts: Vec::new(),
                certificate_pem: String::new(),
                ping_port: CLIENT_PING_PORT,
            },
        })
    }

    /// Serves `app` on a free port in its own thread and returns its base URL.
    fn serve(app: Router) -> String {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                    .await
                    .expect("bind");
                sender
                    .send(listener.local_addr().expect("address"))
                    .expect("send");
                axum::serve(listener, app).await.expect("serve");
            });
        });
        let address = receiver.recv().expect("address");
        format!("http://{address}")
    }

    /// A fake launcher API; returns its base URL.
    fn fake_server() -> String {
        serve(
            Router::new()
                .route(INFO_PATH, get(|| async { fake_info() }))
                .route(LOGIN_PATH, post(|| async { Json(session()) }))
                .route(
                    PLAY_PATH,
                    post(|headers: HeaderMap| async move {
                        let bearer = format!("Bearer {TOKEN}");
                        if headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            == Some(bearer.as_str())
                        {
                            Ok(Json(PlayReply {
                                game_session_key: KEY.to_owned(),
                            }))
                        } else {
                            Err((
                                StatusCode::UNAUTHORIZED,
                                Json(ErrorBody {
                                    code: "no_session".to_owned(),
                                    message: "log in again".to_owned(),
                                }),
                            ))
                        }
                    }),
                ),
        )
    }

    #[test]
    fn calls_parse_replies_and_errors() {
        crate::install_crypto_provider();
        let api = LauncherApi::new(fake_server()).expect("client");
        assert_eq!(api.info().expect("info").server_name, "Fake");
        let credentials = PasswordCredentials {
            username: "ada".to_owned(),
            password: "password-ok".to_owned(),
        };
        assert_eq!(api.login(&credentials).expect("login"), session());
        assert_eq!(api.play(TOKEN).expect("play").game_session_key, KEY);
        match api.play("other") {
            Err(ApiError::Server { status, code, .. }) => {
                assert_eq!((status, code.as_str()), (401, "no_session"));
            }
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn redirects_are_not_followed() {
        crate::install_crypto_provider();
        let base = serve(
            Router::new()
                .route(
                    INFO_PATH,
                    get(|| async { Redirect::temporary(ELSEWHERE_PATH) }),
                )
                .route(ELSEWHERE_PATH, get(|| async { fake_info() })),
        );
        let api = LauncherApi::new(base).expect("client");
        assert!(api.info().is_err());
    }

    #[test]
    fn only_discord_login_pages_are_opened() {
        let good = format!("{DISCORD_AUTHORIZE_URL}?response_type=code&state=abc");
        assert_eq!(discord_login_page(&good).expect("discord").as_str(), good);
        for bad in [
            r"\\host\share\x.exe",
            "file:///C:/Windows/System32/calc.exe",
            "http://discord.com/oauth2/authorize?state=abc",
            "https://discord.com.example.org/oauth2/authorize",
            "https://user@discord.com/oauth2/authorize",
            "https://discord.com:8443/oauth2/authorize",
            "https://discord.com/oauth2/authorize/../../evil",
            "https://discord.com/oauth2/authorize#fragment",
        ] {
            assert!(
                matches!(discord_login_page(bad), Err(ApiError::NotDiscord(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn unreachable_server_is_reported() {
        crate::install_crypto_provider();
        let api = LauncherApi::new("http://127.0.0.1:9".to_owned()).expect("client");
        assert!(matches!(api.info(), Err(ApiError::Unreachable(_))));
    }
}
