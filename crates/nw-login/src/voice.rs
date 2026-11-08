// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[voice]` of `config/edge.toml` and the first call of the client's voice SDK on
//! `game.<domain>`: the prelogin, which names the hosts of the voice service.
//!
//! The social service tells the client the voice service's API address (4199, `[voice] api_url`
//! of `config/social.toml`); the client's voice SDK (Vivox 5.23.1) then sends a form `POST` to
//! `<api_url>viv_get_prelogin.php` and reads the XML answer: the SIP realm, the XMPP server that
//! signs the player in and joins its channels, the same over a WebSocket, and the telemetry hosts.
//! Pointing `api_url` at `https://game.<domain>/vivox/api2/` makes this server answer it.
//!
//! Observed (2026-11-07, 418 retail prelogins in the `vivox/` folders of `captures/`, for
//! example `Loadouts` request 55): one `POST` with `Content-Type:
//! application/x-www-form-urlencoded` and `Connection: Close` (its form was not captured), and
//! the XML of [`prelogin_xml`] with the retail hosts. The timeouts, the ping interval, the
//! longest user address and the connection check address are the retail values in every
//! answer.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::post;
use serde::Deserialize;

/// The path of the prelogin under `game.<domain>`: `api_url` is
/// `https://game.<domain>/vivox/api2/`.
pub const PRELOGIN_PATH: &str = "/vivox/api2/viv_get_prelogin.php";
/// Largest prelogin form (our limit; retail's form was not captured).
pub const PRELOGIN_BODY_LIMIT_BYTES: usize = 16 * 1024;
/// The media type of the answer.
const XML_CONTENT_TYPE: &str = "text/xml; charset=utf-8";
/// The retail values every answer carries.
const CONNECTION_CHECK_ADDRESS: &str = "8.8.8.8:53";
const NEVER_RTP_TIMEOUT_S: u32 = 15;
const LOST_RTP_TIMEOUT_S: u32 = 30;
const XMPP_PING_INTERVAL_S: u32 = 10;
const MAX_USER_URI_LENGTH: u32 = 146;
/// Every telemetry host gets every sample (retail: rate 1 for each).
const TELEMETRY_SAMPLE_RATE: u32 = 1;

/// `[voice]`: the hosts of the community's voice service that the prelogin names. Left out,
/// the prelogin is not served.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {
    /// The SIP realm (`DefaultRealm`): the `domain` of the social service's `[voice]`.
    pub realm: String,
    /// The XMPP server that signs the player in and joins its channels (`XMPPServer`).
    pub xmpp_host: String,
    /// Its port.
    pub xmpp_port: u16,
    /// Whether the XMPP connection uses TLS from its start.
    pub xmpp_tls: bool,
    /// The XMPP server over a WebSocket (`WSXMPPServer`, a `wss://` address).
    pub ws_xmpp_url: String,
    /// The telemetry address of the SDK's call statistics (`DefaultWatson`, `DefaultDyson`,
    /// `DefaultEureka`).
    pub telemetry_url: String,
}

/// The XML of the prelogin answer for `config`, in the retail layout.
pub fn prelogin_xml(config: &VoiceConfig) -> String {
    let telemetry = escape(&config.telemetry_url);
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<VCConfiguration>\n\
\x20 <DefaultRealm>{realm}</DefaultRealm>\n\
\x20 <XMPPServer>\n\
\x20   <host>{xmpp_host}</host>\n\
\x20   <port>{xmpp_port}</port>\n\
\x20   <tls>{xmpp_tls}</tls>\n\
\x20 </XMPPServer>\n\
\x20 <WSXMPPServer>{ws}</WSXMPPServer>\n\
\x20 <DefaultWatson>{telemetry}</DefaultWatson>\n\
\x20 <WatsonSampleRate>{TELEMETRY_SAMPLE_RATE}</WatsonSampleRate>\n\
\x20 <DefaultDyson>{telemetry}</DefaultDyson>\n\
\x20 <DysonSampleRate>{TELEMETRY_SAMPLE_RATE}</DysonSampleRate>\n\
\x20 <DefaultEureka>{telemetry}</DefaultEureka>\n\
\x20 <EurekaSampleRate>{TELEMETRY_SAMPLE_RATE}</EurekaSampleRate>\n\
\x20 <ConnectionRecoveryInternetCheckAddresses>{CONNECTION_CHECK_ADDRESS}</ConnectionRecoveryInternetCheckAddresses>\n\
\x20 <NeverRTPTimeout>{NEVER_RTP_TIMEOUT_S}</NeverRTPTimeout>\n\
\x20 <LostRTPTimeout>{LOST_RTP_TIMEOUT_S}</LostRTPTimeout>\n\
\x20 <XmppPingIntervalSeconds>{XMPP_PING_INTERVAL_S}</XmppPingIntervalSeconds>\n\
\x20 <MaxUserUriLength>{MAX_USER_URI_LENGTH}</MaxUserUriLength>\n\
</VCConfiguration>",
        realm = escape(&config.realm),
        xmpp_host = escape(&config.xmpp_host),
        xmpp_port = config.xmpp_port,
        xmpp_tls = config.xmpp_tls,
        ws = escape(&config.ws_xmpp_url),
    )
}

/// `text` with the XML special characters escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The prelogin route, merged into the routes of `game.<domain>`.
pub fn voice_router(config: VoiceConfig) -> Router {
    Router::new()
        .route(PRELOGIN_PATH, post(prelogin))
        .layer(DefaultBodyLimit::max(PRELOGIN_BODY_LIMIT_BYTES))
        .with_state(config)
}

/// Answers a prelogin with the hosts of `config`. The form's field names are logged: retail's
/// form was not captured, and the next steps of the voice service need them.
async fn prelogin(State(config): State<VoiceConfig>, body: Bytes) -> impl IntoResponse {
    let fields: Vec<String> = form_fields(&body);
    tracing::info!(?fields, bytes = body.len(), "voice prelogin");
    tracing::debug!(form = %String::from_utf8_lossy(&body), "voice prelogin form");
    ([(CONTENT_TYPE, XML_CONTENT_TYPE)], prelogin_xml(&config))
}

/// The field names of the form `body` (`name=value&...`), in order.
fn form_fields(body: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            pair.split_once('=')
                .map_or(pair, |(name, _)| name)
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::test_http::{request, send};

    fn config() -> VoiceConfig {
        VoiceConfig {
            realm: "voice.example.org".to_owned(),
            xmpp_host: "game.example.org".to_owned(),
            xmpp_port: 5223,
            xmpp_tls: true,
            ws_xmpp_url: "wss://game.example.org:443/ws-xmpp".to_owned(),
            telemetry_url: "https://game.example.org".to_owned(),
        }
    }

    /// The retail answer of `Loadouts` request 55 with the hosts of [`config`].
    const RETAIL_LAYOUT: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>
<VCConfiguration>
  <DefaultRealm>voice.example.org</DefaultRealm>
  <XMPPServer>
    <host>game.example.org</host>
    <port>5223</port>
    <tls>true</tls>
  </XMPPServer>
  <WSXMPPServer>wss://game.example.org:443/ws-xmpp</WSXMPPServer>
  <DefaultWatson>https://game.example.org</DefaultWatson>
  <WatsonSampleRate>1</WatsonSampleRate>
  <DefaultDyson>https://game.example.org</DefaultDyson>
  <DysonSampleRate>1</DysonSampleRate>
  <DefaultEureka>https://game.example.org</DefaultEureka>
  <EurekaSampleRate>1</EurekaSampleRate>
  <ConnectionRecoveryInternetCheckAddresses>8.8.8.8:53</ConnectionRecoveryInternetCheckAddresses>
  <NeverRTPTimeout>15</NeverRTPTimeout>
  <LostRTPTimeout>30</LostRTPTimeout>
  <XmppPingIntervalSeconds>10</XmppPingIntervalSeconds>
  <MaxUserUriLength>146</MaxUserUriLength>
</VCConfiguration>";

    #[test]
    fn the_answer_has_the_retail_layout() {
        assert_eq!(prelogin_xml(&config()), RETAIL_LAYOUT);
        let odd = VoiceConfig {
            realm: "a&b<c>".to_owned(),
            ..config()
        };
        assert!(prelogin_xml(&odd).contains("<DefaultRealm>a&amp;b&lt;c&gt;</DefaultRealm>"));
    }

    #[test]
    fn a_form_post_gets_the_xml_and_its_fields_are_read() {
        assert_eq!(
            form_fields(b"userid=x&mode=y&flag"),
            ["userid", "mode", "flag"]
        );
        let app = voice_router(config());
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (status, body) = rt.block_on(send(&app, request("POST", PRELOGIN_PATH, "a=1")));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(String::from_utf8_lossy(&body), RETAIL_LAYOUT);
        let (status, _) = rt.block_on(send(&app, request("GET", PRELOGIN_PATH, "")));
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }
}
