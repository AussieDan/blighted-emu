// SPDX-License-Identifier: AGPL-3.0-or-later
//! `blighted-admin relay-keys`: the two self-signed certificates of the agent connection (admin
//! relay spec section 5.3). Each side trusts exactly the other side's certificate.

use std::path::Path;

use nw_login::certificate::{CertificateError, CertificateUse, self_signed_certificate, write_new};

/// Days the relay and agent certificates are valid.
pub const RELAY_KEYS_VALID_DAYS: u32 = 825;
/// The relay certificate, for the relay and the agent.
pub const RELAY_CERTIFICATE_FILE: &str = "relay.pem";
/// The relay key, for the relay only.
pub const RELAY_KEY_FILE: &str = "relay-key.pem";
/// The agent certificate, for the agent and the relay.
pub const AGENT_CERTIFICATE_FILE: &str = "agent.pem";
/// The agent key, for the agent only.
pub const AGENT_KEY_FILE: &str = "agent-key.pem";
const RELAY_COMMON_NAME: &str = "Blighted admin relay";
const AGENT_COMMON_NAME: &str = "Blighted admin agent";

/// Writes the relay certificate for `relay_name`, the agent certificate and their keys into
/// `out` (made when missing). Nothing is written when one of the four files already exists.
///
/// # Errors
///
/// Returns [`CertificateError`] when a file exists or cannot be written, or OpenSSL fails.
pub fn write_relay_keys(relay_name: &str, out: &Path) -> Result<(), CertificateError> {
    let files = [
        RELAY_CERTIFICATE_FILE,
        RELAY_KEY_FILE,
        AGENT_CERTIFICATE_FILE,
        AGENT_KEY_FILE,
    ]
    .map(|file| out.join(file));
    if let Some(existing) = files.iter().find(|path| path.exists()) {
        return Err(CertificateError::Io {
            path: existing.clone(),
            source: std::io::ErrorKind::AlreadyExists.into(),
        });
    }
    std::fs::create_dir_all(out).map_err(|source| CertificateError::Io {
        path: out.to_path_buf(),
        source,
    })?;
    let (relay, relay_key) = self_signed_certificate(
        RELAY_COMMON_NAME,
        &[relay_name.to_owned()],
        CertificateUse::Server,
        RELAY_KEYS_VALID_DAYS,
    )?;
    let (agent, agent_key) = self_signed_certificate(
        AGENT_COMMON_NAME,
        &[],
        CertificateUse::Client,
        RELAY_KEYS_VALID_DAYS,
    )?;
    let [relay_path, relay_key_path, agent_path, agent_key_path] = &files;
    write_new(relay_key_path, &relay_key)?;
    write_new(relay_path, &relay)?;
    write_new(agent_key_path, &agent_key)?;
    write_new(agent_path, &agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_files_are_written_once() {
        let dir = tempfile::tempdir().expect("dir");
        let out = dir.path().join("admin-relay");
        write_relay_keys("relay.example.test", &out).expect("keys");
        for file in [
            RELAY_CERTIFICATE_FILE,
            RELAY_KEY_FILE,
            AGENT_CERTIFICATE_FILE,
            AGENT_KEY_FILE,
        ] {
            assert!(out.join(file).is_file(), "{file}");
        }
        let first = std::fs::read(out.join(RELAY_KEY_FILE)).expect("read");
        assert!(matches!(
            write_relay_keys("relay.example.test", &out),
            Err(CertificateError::Io { .. })
        ));
        assert_eq!(
            std::fs::read(out.join(RELAY_KEY_FILE)).expect("read"),
            first
        );
    }
}
