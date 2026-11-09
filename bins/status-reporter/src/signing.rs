// SPDX-License-Identifier: AGPL-3.0-or-later
//! The signature of a report, as the website checks it: lowercase hex HMAC-SHA256 of
//! `<timestamp>.<body>` with the world's secret, sent with the world id and the timestamp in
//! three headers. The website refuses a timestamp more than 60 s from its clock and a second
//! report of the same kind with the same timestamp.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The header with the world id.
pub const WORLD_HEADER: &str = "X-Blighted-World";
/// The header with the Unix seconds of the report.
pub const TIMESTAMP_HEADER: &str = "X-Blighted-Timestamp";
/// The header with the signature.
pub const SIGNATURE_HEADER: &str = "X-Blighted-Signature";
/// The shortest secret the website takes, in characters.
pub const MIN_SECRET_CHARS: usize = 32;
/// Between the timestamp and the body in the signed text.
const SIGNED_SEPARATOR: &[u8] = b".";

/// The secret of one world, shared with the website.
#[derive(Clone)]
pub struct ReportSecret(String);

impl std::fmt::Debug for ReportSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReportSecret(..)")
    }
}

/// A secret the website would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the report secret has {chars} characters; the website needs at least {MIN_SECRET_CHARS}")]
pub struct SecretTooShort {
    /// The characters it has.
    pub chars: usize,
}

impl ReportSecret {
    /// The secret `text`; refused when it is shorter than [`MIN_SECRET_CHARS`].
    ///
    /// # Errors
    ///
    /// Returns [`SecretTooShort`] for a short secret.
    pub fn new(text: String) -> Result<Self, SecretTooShort> {
        let chars = text.chars().count();
        if chars < MIN_SECRET_CHARS {
            return Err(SecretTooShort { chars });
        }
        Ok(Self(text))
    }

    /// The signature of `body` sent at `timestamp` (Unix seconds).
    pub fn sign(&self, timestamp: i64, body: &[u8]) -> String {
        sign(self.0.as_bytes(), timestamp, body)
    }
}

/// Lowercase hex HMAC-SHA256 of `<timestamp>.<body>` with `key`.
fn sign(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    // HMAC takes a key of any length.
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(SIGNED_SEPARATOR);
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The website's known vector: HMAC-SHA256("key", "1.x").
    const VECTOR_SIGNATURE: &str =
        "fc4913c41a7bf793267e2a384f3261c9436ec2cd097bd745e7b0d99113f510ec";
    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn the_signature_matches_the_website_vector() {
        assert_eq!(sign(b"key", 1, b"x"), VECTOR_SIGNATURE);
    }

    #[test]
    fn the_signature_covers_the_timestamp_and_the_body() {
        let secret = ReportSecret::new(SECRET.to_owned()).expect("secret");
        let signed = secret.sign(1, b"x");
        assert_ne!(secret.sign(2, b"x"), signed);
        assert_ne!(secret.sign(1, b"y"), signed);
        assert_eq!(signed, sign(SECRET.as_bytes(), 1, b"x"));
    }

    #[test]
    fn a_short_secret_is_refused() {
        let short = SECRET[1..].to_owned();
        assert_eq!(
            ReportSecret::new(short).err(),
            Some(SecretTooShort {
                chars: MIN_SECRET_CHARS - 1
            })
        );
    }

    #[test]
    fn debug_hides_the_secret() {
        let secret = ReportSecret::new(SECRET.to_owned()).expect("secret");
        assert!(!format!("{secret:?}").contains(SECRET));
    }
}
