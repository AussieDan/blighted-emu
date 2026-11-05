// SPDX-License-Identifier: AGPL-3.0-or-later
//! Voice-chat setup in the retail form, for a Vivox-compatible voice service the community runs
//! (this server runs none). Retail (Observed, 13 sessions):
//!
//! - A token is `e30.<claims>.<signature>`: `e30` is the base64url of `{}`, the claims are base64url
//!   JSON in the order `iss`, `exp`, `vxa`, `vxi`, `f`, `t`, and the signature is the base64url
//!   HMAC-SHA256 of `e30.<claims>` (43 characters). `exp` is the issue time plus
//!   [`TOKEN_LIFETIME_S`]; `vxi` is the call's request number; `vxa` is `login`, `join` or `trxn`.
//! - `f` is the player's address `sip:.<issuer>.<character id>.@<domain host>`; `t` is the
//!   channel's address.
//! - A channel's address is `sip:confctl-<kind>-<issuer>.<channel><suffix>@<domain host>`, kind
//!   `d` for a positional channel, `g` for a group channel; the suffix is the same for every
//!   channel of a world.
//! - A positional channel is named by its place, `<x><y>_0` with three digits each; a group
//!   channel by a 43-character base64url id.

use data_encoding::BASE64URL_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// The empty JSON header of every token, base64url.
const TOKEN_HEADER: &str = "e30";
/// Seconds from issue to expiry of every retail token (499.0 to 499.9 s after its call).
pub const TOKEN_LIFETIME_S: u64 = 500;
/// `vxa` of a login token.
const ACTION_LOGIN: &str = "login";
/// `vxa` of a join token.
const ACTION_JOIN: &str = "join";
/// `vxa` of a transcription token.
const ACTION_TRANSCRIPTION: &str = "trxn";
/// The kind letter of a positional channel address.
const KIND_POSITIONAL: &str = "d";
/// The kind letter of a group channel address.
const KIND_GROUP: &str = "g";
/// The layer of an open-world positional channel (every retail open-world name ends `_0`).
const OPEN_WORLD_LAYER: &str = "0";
/// Random bytes of a group channel id: 32, which base64url makes 43 characters.
const GROUP_CHANNEL_BYTES: usize = 32;
/// Digits of each place number of a positional channel name.
const PLACE_DIGITS: usize = 3;
/// The largest place number with [`PLACE_DIGITS`] digits.
const MAX_PLACE: i64 = 999;

/// The places on each side of a player's own place whose positional channels it may join.
const NEAR_PLACES: i64 = 1;

/// The name of the positional channel of place (`east`, `north`).
fn place_channel(east: i64, north: i64) -> String {
    format!("{east:0PLACE_DIGITS$}{north:0PLACE_DIGITS$}_{OPEN_WORLD_LAYER}")
}

/// The voice service a community runs, from the social service's configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceService {
    /// The service's API address (4199 first text).
    pub api_url: String,
    /// The token issuer.
    pub issuer: String,
    /// The SIP domain host (without `@`).
    pub domain: String,
    /// The secret key that signs the tokens.
    pub key: Vec<u8>,
    /// The suffix of every channel name of this world.
    pub channel_suffix: String,
    /// Edge length of the place of a positional channel, in metres.
    pub cell_size_m: f32,
}

/// The `vxa` kind of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAction {
    /// Log in.
    Login,
    /// Join a channel.
    Join,
    /// Transcribe a channel.
    Transcription,
}

impl VoiceService {
    /// The SIP domain as 4199 carries it: with its leading `@`.
    pub fn domain_text(&self) -> String {
        format!("@{}", self.domain)
    }

    /// The address of character `character`.
    pub fn user_uri(&self, character: Uuid) -> String {
        format!(
            "sip:.{}.{}.@{}",
            self.issuer,
            character.hyphenated(),
            self.domain
        )
    }

    /// The address of positional channel `channel`.
    pub fn positional_uri(&self, channel: &str) -> String {
        self.channel_uri(KIND_POSITIONAL, channel)
    }

    /// The address of group channel `channel`.
    pub fn group_uri(&self, channel: &str) -> String {
        self.channel_uri(KIND_GROUP, channel)
    }

    fn channel_uri(&self, kind: &str, channel: &str) -> String {
        format!(
            "sip:confctl-{kind}-{}.{channel}{}@{}",
            self.issuer, self.channel_suffix, self.domain
        )
    }

    /// The name of the positional channel at `position` (metres east, north).
    pub fn positional_channel(&self, position: [f32; 2]) -> String {
        let (east, north) = self.place_of(position);
        place_channel(east, north)
    }

    /// The positional channels a player at `position` may join: its own place and the
    /// [`NEAR_PLACES`] around it. Inferred: the client joins the channel of its channel list
    /// (its own place), and a player that crossed into the next place between the list and the
    /// join still joins its old one; retail's rule is Unknown.
    pub fn positional_channels_near(&self, position: [f32; 2]) -> Vec<String> {
        let (east, north) = self.place_of(position);
        let span = |place: i64| (place - NEAR_PLACES).max(0)..=(place + NEAR_PLACES).min(MAX_PLACE);
        span(east)
            .flat_map(|east| span(north).map(move |north| place_channel(east, north)))
            .collect()
    }

    fn place_of(&self, position: [f32; 2]) -> (i64, i64) {
        let place = |metres: f32| -> i64 {
            // A finite position over a positive cell size is a small number.
            ((metres / self.cell_size_m).floor() as i64).clamp(0, MAX_PLACE)
        };
        (place(position[0]), place(position[1]))
    }

    /// A token of `action` for `character` with request number `request` at Unix seconds
    /// `now_s`, to channel address `target` for a join or a transcription.
    pub fn token(
        &self,
        action: TokenAction,
        character: Uuid,
        request: u16,
        target: Option<&str>,
        now_s: u64,
    ) -> String {
        let name = match action {
            TokenAction::Login => ACTION_LOGIN,
            TokenAction::Join => ACTION_JOIN,
            TokenAction::Transcription => ACTION_TRANSCRIPTION,
        };
        let mut claims = format!(
            "{{\"iss\":{},\"exp\":{},\"vxa\":\"{name}\",\"vxi\":{request},\"f\":{}",
            json_text(&self.issuer),
            now_s + TOKEN_LIFETIME_S,
            json_text(&self.user_uri(character)),
        );
        if let Some(target) = target {
            claims.push_str(&format!(",\"t\":{}", json_text(target)));
        }
        claims.push('}');
        let signed = format!(
            "{TOKEN_HEADER}.{}",
            BASE64URL_NOPAD.encode(claims.as_bytes())
        );
        let signature = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key)
            .map(|mut mac| {
                mac.update(signed.as_bytes());
                BASE64URL_NOPAD.encode(&mac.finalize().into_bytes())
            })
            .unwrap_or_default();
        format!("{signed}.{signature}")
    }
}

/// A JSON string of `text`.
fn json_text(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// A new group channel id: 32 random bytes as base64url.
///
/// # Panics
///
/// Panics when the operating system has no random source.
pub fn new_group_channel() -> String {
    let mut bytes = [0; GROUP_CHANNEL_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system's random source");
    BASE64URL_NOPAD.encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> VoiceService {
        VoiceService {
            api_url: "https://voice.example/api2/".to_owned(),
            issuer: "issuer-1".to_owned(),
            domain: "voice.example".to_owned(),
            key: b"secret".to_vec(),
            channel_suffix: "SUFFIX".to_owned(),
            cell_size_m: 512.0,
        }
    }

    #[test]
    fn addresses_have_the_retail_shape() {
        let service = service();
        let character = Uuid::from_u128(1);
        assert_eq!(
            service.user_uri(character),
            "sip:.issuer-1.00000000-0000-0000-0000-000000000001.@voice.example"
        );
        assert_eq!(
            service.positional_uri("013007_0"),
            "sip:confctl-d-issuer-1.013007_0SUFFIX@voice.example"
        );
        assert!(
            service
                .group_uri("abc")
                .starts_with("sip:confctl-g-issuer-1.abc")
        );
        assert_eq!(service.positional_channel([6700.0, 3600.0]), "013007_0");
        let near = service.positional_channels_near([6700.0, 3600.0]);
        assert_eq!(near.len(), 9);
        assert!(near.contains(&"012006_0".to_owned()) && near.contains(&"014008_0".to_owned()));
        assert!(!near.contains(&"015007_0".to_owned()));
        assert_eq!(
            service.positional_channels_near([0.0, 0.0]),
            ["000000_0", "000001_0", "001000_0", "001001_0"],
            "no place below 0"
        );
        assert_eq!(service.domain_text(), "@voice.example");
        assert_eq!(new_group_channel().len(), 43);
    }

    #[test]
    fn a_token_has_the_retail_parts_and_signature() {
        let service = service();
        let token = service.token(
            TokenAction::Join,
            Uuid::from_u128(1),
            6,
            Some("sip:t"),
            1_000,
        );
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], TOKEN_HEADER);
        assert_eq!(parts[2].len(), 43, "a base64url HMAC-SHA256");
        let claims = BASE64URL_NOPAD
            .decode(parts[1].as_bytes())
            .expect("base64url");
        let claims: serde_json::Value = serde_json::from_slice(&claims).expect("json");
        assert_eq!(claims["exp"], 1_000 + TOKEN_LIFETIME_S);
        assert_eq!(claims["vxa"], "join");
        assert_eq!(claims["vxi"], 6);
        assert_eq!(claims["t"], "sip:t");
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(b"secret").expect("key");
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        assert!(
            mac.verify_slice(
                &BASE64URL_NOPAD
                    .decode(parts[2].as_bytes())
                    .expect("base64url")
            )
            .is_ok()
        );
        let login = service.token(TokenAction::Login, Uuid::from_u128(1), 1, None, 1_000);
        let claims = login.split('.').nth(1).expect("claims");
        let claims = BASE64URL_NOPAD
            .decode(claims.as_bytes())
            .expect("base64url");
        let claims: serde_json::Value = serde_json::from_slice(&claims).expect("json");
        assert!(claims.get("t").is_none(), "a login token names no channel");
    }
}
