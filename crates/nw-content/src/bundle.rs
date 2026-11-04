// SPDX-License-Identifier: AGPL-3.0-or-later
//! The content bundle file (M1 spec section 5), format version 2: the shared header of
//! [`crate::container`] with magic `NWCB` and the generated [`SCHEMA_HASH`], then a postcard
//! body (tables and text).
//!
//! Loading checks every header field before it decodes the body. Errors are
//! [`ContainerError`]s.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::container::{
    ContainerError, ContainerSpec, decode_value, encode_value, read_file, write_file,
};
use crate::generated::{SCHEMA_HASH, Tables};

/// First 4 bytes of every bundle.
pub const BUNDLE_MAGIC: [u8; 4] = *b"NWCB";
/// Current bundle format version.
pub const BUNDLE_FORMAT_VERSION: u16 = 2;
/// Header fields of the bundle.
pub const BUNDLE_CONTAINER: ContainerSpec = ContainerSpec {
    magic: BUNDLE_MAGIC,
    format_version: BUNDLE_FORMAT_VERSION,
    schema_hash: SCHEMA_HASH,
};

/// All content that `sim` loads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContentBundle {
    /// Steam build id of the client the content came from. Stored in the header, not the body.
    #[serde(skip)]
    pub client_build: String,
    /// Every datasheet, typed.
    pub tables: Tables,
    /// English text by localization key.
    pub text: BTreeMap<String, String>,
}

impl ContentBundle {
    /// Encodes the bundle with its header.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ContainerError> {
        encode_value(&BUNDLE_CONTAINER, &self.client_build, self)
    }

    /// Checks the header, then decodes the bundle. Refuses another format version, other
    /// generated table code ([`SCHEMA_HASH`]) and another client build before decoding.
    pub fn from_bytes(bytes: &[u8], expected_build: &str) -> Result<Self, ContainerError> {
        let mut bundle: Self = decode_value(&BUNDLE_CONTAINER, bytes, expected_build)?;
        bundle.client_build = expected_build.to_owned();
        Ok(bundle)
    }

    /// Writes the bundle to `path`, creating parent folders.
    pub fn save(&self, path: &Path) -> Result<(), ContainerError> {
        write_file(path, &self.to_bytes()?)
    }

    /// Reads and checks the bundle at `path`.
    pub fn load(path: &Path, expected_build: &str) -> Result<Self, ContainerError> {
        Self::from_bytes(&read_file(path)?, expected_build)
    }
}

/// The first character of a localization key in a datasheet cell (`@ui_riding`).
pub const TEXT_KEY_PREFIX: char = '@';

/// The English text of the datasheet cell `key` (`@ui_riding` or `ui_riding`): by the key as the
/// localization file writes it, else in lower case.
pub fn english<'a>(text: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    let key = key.trim_start_matches(TEXT_KEY_PREFIX);
    text.get(key)
        .or_else(|| text.get(&key.to_lowercase()))
        .map(String::as_str)
}

/// The bundle path of a client build: `<dir>/bundle-<client_build>.bin`.
pub fn bundle_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!("bundle-{client_build}.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_path_names_the_build() {
        assert_eq!(
            bundle_path(Path::new("content"), BUILD),
            Path::new("content").join("bundle-22469132.bin")
        );
    }
    use crate::container::encode_container;

    const BUILD: &str = "22469132";
    const OTHER_BUILD: &str = "1";

    fn bundle() -> ContentBundle {
        let mut bundle = ContentBundle {
            client_build: BUILD.to_owned(),
            ..ContentBundle::default()
        };
        bundle
            .text
            .insert("@ui_hello".to_owned(), "Hello".to_owned());
        bundle
    }

    #[test]
    fn round_trip_keeps_everything() {
        let bytes = bundle().to_bytes().unwrap();
        assert_eq!(&bytes[..BUNDLE_MAGIC.len()], &BUNDLE_MAGIC);
        assert_eq!(ContentBundle::from_bytes(&bytes, BUILD).unwrap(), bundle());
    }

    #[test]
    fn other_build_is_refused() {
        let bytes = bundle().to_bytes().unwrap();
        assert!(matches!(
            ContentBundle::from_bytes(&bytes, OTHER_BUILD),
            Err(ContainerError::BuildMismatch { .. })
        ));
    }

    #[test]
    fn other_generated_code_is_refused() {
        let body = postcard::to_allocvec(&bundle()).unwrap();
        let spec = ContainerSpec {
            schema_hash: SCHEMA_HASH ^ 1,
            ..BUNDLE_CONTAINER
        };
        let bytes = encode_container(&spec, BUILD, &body).unwrap();
        let error = ContentBundle::from_bytes(&bytes, BUILD).expect_err("other schema");
        assert!(matches!(error, ContainerError::SchemaMismatch { .. }));
        assert!(
            error
                .to_string()
                .ends_with("just bundle, just world, then just design"),
            "{error}"
        );
    }

    #[test]
    fn save_creates_folders_and_load_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content/bundle.bin");
        bundle().save(&path).unwrap();
        assert_eq!(ContentBundle::load(&path, BUILD).unwrap(), bundle());
    }
}
