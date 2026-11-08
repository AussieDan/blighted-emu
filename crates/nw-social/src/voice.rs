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
//! - A positional channel is named by its place and its layer, `<x><y>_<layer>` with three digits
//!   each; a group channel by a 43-character base64url id.
//!
//! Observed (2026-11-07, the 7,404 retail 405 of 556 ledgers of `captures/`, probe `voicerooms`):
//! - Every 405 answered a 2805 within 1 s (7,387 of 7,404; the rest followed a 2805 the capture
//!   cut): the server sends the channels only when asked. The client asks each time the player
//!   crosses a line of half a place (the positions of its 2805 sit on them).
//! - A 405 lists 1 to 11 channels, mostly 4 (3,526) or 2 (2,786): the places around the player.
//!   The places a box of one place's size centred on the player overlaps hold the list in 5,697
//!   of 6,270 open-world 405 with a known position, and are the list in 2,795 of them (a box of
//!   448 m: 3,185); the own place alone was the list in 496. The exact rule is Unknown (the
//!   lists also keep a place the player came from).
//! - The layer is `0` in the open world and a 16-digit hex id in an instance (a dungeon, a race,
//!   a story instance): 507 ledgers sent `0`, and 113 switched between `0` and an id when the
//!   player went into an instance and out.

use data_encoding::BASE64URL_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
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
/// Bytes of an instance's layer id: 8, which hex makes the retail 16 digits.
const INSTANCE_LAYER_BYTES: usize = 8;
/// Random bytes of a group channel id: 32, which base64url makes 43 characters.
const GROUP_CHANNEL_BYTES: usize = 32;
/// Digits of each place number of a positional channel name.
const PLACE_DIGITS: usize = 3;
/// The largest place number with [`PLACE_DIGITS`] digits.
const MAX_PLACE: i64 = 999;

/// The places on each side of a player's own place whose positional channels it may join.
const NEAR_PLACES: i64 = 1;
/// The share of a place on each side of the player that the listed places cover: the box of one
/// place's size centred on the player (module note).
const LISTED_HALF_SHARE: f32 = 0.5;
/// How far short of its far side the box ends, metres: it covers its near side, not its far one.
const BOX_END_M: f32 = 0.001;

/// The name of the positional channel of place (`east`, `north`) on layer `layer`.
fn place_channel(east: i64, north: i64, layer: &str) -> String {
    format!("{east:0PLACE_DIGITS$}{north:0PLACE_DIGITS$}_{layer}")
}

/// The layer of the positional channels of a player on sim `sim`, `instanced` or not: `0` in
/// the open world, else an id of the sim (an instance runs on a sim of its own), 16 hex digits.
pub fn channel_layer(instanced: bool, sim: &str) -> String {
    if !instanced {
        return OPEN_WORLD_LAYER.to_owned();
    }
    Sha256::digest(sim.as_bytes())
        .iter()
        .take(INSTANCE_LAYER_BYTES)
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

    /// The positional channels listed to a player at `position` (metres east, north) on layer
    /// `layer` (405): its own place first, then the other places that the box of one place's
    /// size centred on it overlaps (module note).
    pub fn positional_channels(&self, position: [f32; 2], layer: &str) -> Vec<String> {
        let own = self.place_of(position);
        let half_m = self.cell_size_m * LISTED_HALF_SHARE;
        // The box covers [position - half, position + half): a box that ends on a line between
        // two places does not reach into the next one.
        let span = |axis: usize| {
            let low = self.place_of_metres(position[axis] - half_m);
            let high = self
                .place_of_metres(position[axis] + half_m - BOX_END_M)
                .max(low);
            low..=high
        };
        let mut places = vec![own];
        for east in span(0) {
            for north in span(1) {
                if (east, north) != own {
                    places.push((east, north));
                }
            }
        }
        places
            .into_iter()
            .map(|(east, north)| place_channel(east, north, layer))
            .collect()
    }

    /// The positional channels a player at `position` on layer `layer` may join: its own place
    /// and the [`NEAR_PLACES`] around it, which hold every listed one. Inferred: a player that
    /// crossed into the next place between the list and the join still joins its old one;
    /// retail's rule is Unknown.
    pub fn positional_channels_near(&self, position: [f32; 2], layer: &str) -> Vec<String> {
        let (east, north) = self.place_of(position);
        let span = |place: i64| (place - NEAR_PLACES).max(0)..=(place + NEAR_PLACES).min(MAX_PLACE);
        span(east)
            .flat_map(|east| span(north).map(move |north| place_channel(east, north, layer)))
            .collect()
    }

    fn place_of(&self, position: [f32; 2]) -> (i64, i64) {
        (
            self.place_of_metres(position[0]),
            self.place_of_metres(position[1]),
        )
    }

    /// The place number of `metres` along one axis.
    fn place_of_metres(&self, metres: f32) -> i64 {
        // A finite position over a positive cell size is a small number.
        ((metres / self.cell_size_m).floor() as i64).clamp(0, MAX_PLACE)
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
        let signature = signature_mac(&self.key, &signed)
            .map(|mac| BASE64URL_NOPAD.encode(&mac.finalize().into_bytes()))
            .unwrap_or_default();
        format!("{signed}.{signature}")
    }
}

/// The HMAC-SHA256 of `signed` (`e30.<claims>`) under `key`, ready to finish or verify; `None`
/// for a key the HMAC refuses.
fn signature_mac(key: &[u8], signed: &str) -> Option<Hmac<Sha256>> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).ok()?;
    mac.update(signed.as_bytes());
    Some(mac)
}

/// The claims of a token ([`VoiceService::token`]) that a voice service checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenClaims {
    /// `iss`: the issuer.
    pub issuer: String,
    /// `exp`: Unix seconds after which it is no longer valid.
    pub expires_s: u64,
    /// `vxa`: `login`, `join` or `trxn`.
    pub action: String,
    /// `f`: the player's address.
    pub from: String,
    /// `t`: the channel's address, for a join or a transcription.
    pub target: Option<String>,
}

impl TokenClaims {
    /// Whether these are the claims of a login token from `issuer` for `from` that is still
    /// valid at Unix seconds `now_s`.
    pub fn allow_login(&self, issuer: &str, from: &str, now_s: u64) -> bool {
        self.allows(ACTION_LOGIN, issuer, from, now_s)
    }

    /// Whether these are the claims of a join token from `issuer` for `from` to channel address
    /// `target` that is still valid at Unix seconds `now_s`.
    pub fn allow_join(&self, issuer: &str, from: &str, target: &str, now_s: u64) -> bool {
        self.allows(ACTION_JOIN, issuer, from, now_s) && self.target.as_deref() == Some(target)
    }

    fn allows(&self, action: &str, issuer: &str, from: &str, now_s: u64) -> bool {
        self.action == action
            && self.issuer == issuer
            && self.from == from
            && now_s <= self.expires_s
    }
}

/// The claims of `token` when `key` signed it (`e30.<claims>.<signature>`); `None` for another
/// form, a wrong signature or claims without `iss`, `exp`, `vxa` and `f`.
pub fn verified_claims(key: &[u8], token: &str) -> Option<TokenClaims> {
    let (signed, signature) = token.rsplit_once('.')?;
    let (header, claims) = signed.split_once('.')?;
    if header != TOKEN_HEADER {
        return None;
    }
    let signature = BASE64URL_NOPAD.decode(signature.as_bytes()).ok()?;
    signature_mac(key, signed)?.verify_slice(&signature).ok()?;
    let claims = BASE64URL_NOPAD.decode(claims.as_bytes()).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&claims).ok()?;
    let text = |name: &str| claims.get(name)?.as_str().map(str::to_owned);
    Some(TokenClaims {
        issuer: text("iss")?,
        expires_s: claims.get("exp")?.as_u64()?,
        action: text("vxa")?,
        from: text("f")?,
        target: text("t"),
    })
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
        let near = service.positional_channels_near([6700.0, 3600.0], OPEN_WORLD_LAYER);
        assert_eq!(near.len(), 9);
        assert!(near.contains(&"012006_0".to_owned()) && near.contains(&"014008_0".to_owned()));
        assert!(!near.contains(&"015007_0".to_owned()));
        assert_eq!(
            service.positional_channels_near([0.0, 0.0], OPEN_WORLD_LAYER),
            ["000000_0", "000001_0", "001000_0", "001001_0"],
            "no place below 0"
        );
        assert_eq!(service.domain_text(), "@voice.example");
        assert_eq!(new_group_channel().len(), 43);
    }

    #[test]
    fn the_listed_channels_are_the_places_around_the_player_as_retail() {
        let service = service();
        // Retail `MSQ22-lv67` at 1.461 s: the player at (9219.44, 9057.035), 405 `017018_0`,
        // `017017_0`, `018017_0`, `018018_0`.
        let listed = service.positional_channels([9_219.44, 9_057.035], OPEN_WORLD_LAYER);
        assert_eq!(listed[0], "018017_0", "the own place first");
        let mut sorted = listed.clone();
        sorted.sort();
        assert_eq!(sorted, ["017017_0", "017018_0", "018017_0", "018018_0"]);
        // In the middle of a place on one axis: two places.
        let two = service.positional_channels([18.1 * 512.0, 17.5 * 512.0], OPEN_WORLD_LAYER);
        assert_eq!(two, ["018017_0", "017017_0"]);
        let near = service.positional_channels_near([9_219.44, 9_057.035], OPEN_WORLD_LAYER);
        assert!(listed.iter().all(|channel| near.contains(channel)));
        assert_eq!(
            service.positional_channels([10.0, 10.0], "a5b5ccc46aa972b9"),
            ["000000_a5b5ccc46aa972b9"],
            "no place below 0, and the layer goes in the name"
        );
    }

    #[test]
    fn the_open_world_is_layer_0_and_an_instance_has_its_own() {
        assert_eq!(channel_layer(false, "sim-1"), OPEN_WORLD_LAYER);
        let dungeon = channel_layer(true, "instance-1");
        assert_eq!(dungeon.len(), 2 * INSTANCE_LAYER_BYTES);
        assert!(dungeon.chars().all(|digit| digit.is_ascii_hexdigit()));
        assert_ne!(dungeon, channel_layer(true, "instance-2"));
        assert_eq!(dungeon, channel_layer(true, "instance-1"));
    }

    #[test]
    fn a_signed_login_token_verifies_and_a_changed_one_does_not() {
        let service = service();
        let character = Uuid::from_u128(1);
        let token = service.token(TokenAction::Login, character, 1, None, 1_000);
        let claims = verified_claims(b"secret", &token).expect("verified");
        let from = service.user_uri(character);
        assert!(claims.allow_login("issuer-1", &from, 1_000 + TOKEN_LIFETIME_S));
        assert!(
            !claims.allow_login("issuer-1", &from, 1_001 + TOKEN_LIFETIME_S),
            "expired"
        );
        assert!(
            !claims.allow_login("issuer-2", &from, 1_000),
            "another issuer"
        );
        assert!(verified_claims(b"other key", &token).is_none());
        let mut forged = token.clone();
        forged.insert(10, 'x');
        assert!(verified_claims(b"secret", &forged).is_none());
        let join = service.token(TokenAction::Join, character, 2, Some("sip:t"), 1_000);
        let join = verified_claims(b"secret", &join).expect("verified");
        assert!(
            !join.allow_login("issuer-1", &from, 1_000),
            "a join token is no login"
        );
        assert_eq!(join.target.as_deref(), Some("sip:t"));
        assert!(join.allow_join("issuer-1", &from, "sip:t", 1_000));
        assert!(
            !join.allow_join("issuer-1", &from, "sip:other", 1_000),
            "another channel"
        );
        assert!(
            !claims.allow_join("issuer-1", &from, "sip:t", 1_000),
            "a login token joins nothing"
        );
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
