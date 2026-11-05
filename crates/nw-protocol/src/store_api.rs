// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store table: what this server's store sells, for free, and the entitlements every account
//! holds. `content-tool store` writes it from the game data (`nw_content::store`) for
//! `edge-http`, which serves the catalog and the entitlement list from it; the sims build the same
//! table from the content bundle and grant the purchases.
//!
//! Retail sold its store mounts for 0 in its last weeks (`mount-menu-buying-all-store-horses`,
//! 2026-11-04: 35 mounts unlocked for 0 Marks of Fortune). This server keeps that price.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The store currency: its alias and id (retail `currencyAlias`, `currencyId`).
pub const STORE_CURRENCY: &str = "Marks of Fortune";
/// The store currency's name (retail `currencyName`).
pub const STORE_CURRENCY_NAME: &str = "New World - Marks of Fortune";

/// File name start of the store table: `store-<client build>.json`.
pub const STORE_TABLE_PREFIX: &str = "store-";
/// File name end of the store table.
pub const STORE_TABLE_EXTENSION: &str = ".json";

/// An entitlement and its English name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreEntitlement {
    /// The entitlement: the game data's `UniqueTagID` (`Ent_Mount_MTX_7_bear_Ronin`). The store's
    /// offer and entitlement ids are this text too.
    pub entitlement: String,
    /// Its English name (`Ronin Bear Mount`), or the entitlement when the game data has none.
    pub name: String,
}

/// One mount the store sells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreMount {
    /// The entitlement that unlocks the mount.
    #[serde(flatten)]
    pub unlock: StoreEntitlement,
    /// The mount it unlocks: its `MountId` (`Mount_MTX_7_bear_Ronin`).
    pub mount: String,
    /// Its store product: the `UniqueTagID` of `javelindata_storeproducts`
    /// (`Mount_Bear_MTX7_Ronin`), the offer's `alias` (retail: the client finds an offer's product
    /// by its alias, and leaves out an offer without one).
    pub product: String,
}

/// The store table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreTable {
    /// Every mount the store sells.
    pub mounts: Vec<StoreMount>,
    /// The entitlements every account holds: those the store mounts require (the expansion,
    /// `Ent_Xpack_RiseOfTheAngryEarth_Owner`).
    pub held_by_all: Vec<StoreEntitlement>,
}

/// The store table cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum StoreTableError {
    /// The file cannot be read.
    #[error("cannot read the store table {}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is no store table.
    #[error("the store table {} is not valid: {source}", .path.display())]
    Json {
        /// The file.
        path: PathBuf,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// The store table file of `client_build` in `dir`.
pub fn store_table_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!(
        "{STORE_TABLE_PREFIX}{client_build}{STORE_TABLE_EXTENSION}"
    ))
}

impl StoreTable {
    /// Reads the table at `path`; an empty table when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`StoreTableError`] when the file cannot be read or is no store table.
    pub fn load(path: &Path) -> Result<Self, StoreTableError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| StoreTableError::Json {
                path: path.to_path_buf(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StoreTableError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Writes the table to `path`, creating its folder.
    ///
    /// # Errors
    ///
    /// Returns [`StoreTableError::Io`] when the file cannot be written.
    pub fn write(&self, path: &Path) -> Result<(), StoreTableError> {
        let io = |source| StoreTableError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(io)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| StoreTableError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, bytes).map_err(io)
    }

    /// The mount that `entitlement` unlocks.
    pub fn mount_of(&self, entitlement: &str) -> Option<&StoreMount> {
        self.mounts
            .iter()
            .find(|mount| mount.unlock.entitlement == entitlement)
    }

    /// The store mount whose `MountId` is `mount` (any case).
    pub fn unlock_of(&self, mount: &str) -> Option<&StoreMount> {
        self.mounts
            .iter()
            .find(|store| store.mount.eq_ignore_ascii_case(mount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> StoreTable {
        StoreTable {
            mounts: vec![StoreMount {
                unlock: StoreEntitlement {
                    entitlement: "Ent_Mount_MTX_7_bear_Ronin".to_owned(),
                    name: "Ronin Bear Mount".to_owned(),
                },
                mount: "Mount_MTX_7_bear_Ronin".to_owned(),
                product: "Mount_Bear_MTX7_Ronin".to_owned(),
            }],
            held_by_all: vec![StoreEntitlement {
                entitlement: "Ent_Xpack_RiseOfTheAngryEarth_Owner".to_owned(),
                name: "Rise of the Angry Earth".to_owned(),
            }],
        }
    }

    #[test]
    fn a_table_is_written_and_read_back_and_finds_its_mounts() {
        let dir = std::env::temp_dir().join(format!("store-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("folder");
        let path = store_table_path(&dir, "25810808");
        assert!(path.ends_with("store-25810808.json"));
        assert_eq!(
            StoreTable::load(&path).expect("absent"),
            StoreTable::default()
        );
        table().write(&path).expect("write");
        let read = StoreTable::load(&path).expect("read");
        assert_eq!(read, table());
        assert_eq!(
            read.mount_of("Ent_Mount_MTX_7_bear_Ronin")
                .map(|m| m.mount.as_str()),
            Some("Mount_MTX_7_bear_Ronin")
        );
        assert!(read.unlock_of("mount_mtx_7_bear_ronin").is_some());
        assert!(read.mount_of("Ent_Nothing").is_none());
        std::fs::write(&path, b"[]").expect("write");
        assert!(matches!(
            StoreTable::load(&path),
            Err(StoreTableError::Json { .. })
        ));
        std::fs::remove_dir_all(&dir).expect("removed");
    }
}
