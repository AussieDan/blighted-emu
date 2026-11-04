// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool store`: the store table ([`nw_protocol::store_api`]) from the content bundle,
//! for `edge-http`'s store catalog. It needs no game install.

use std::path::{Path, PathBuf};

use nw_content::ContentBundle;
use nw_content::bundle::bundle_path;
use nw_content::store::store_table;
use nw_protocol::store_api::store_table_path;

use crate::error::ToolError;

/// What `store` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreReport {
    /// The file.
    pub path: PathBuf,
    /// The mounts the store sells.
    pub mounts: usize,
    /// The entitlements every account holds.
    pub held_by_all: usize,
}

/// Writes the store table of `client_build` into `content_dir`, from the bundle in it.
///
/// # Errors
///
/// Returns [`ToolError`] when the bundle cannot be read or the table cannot be written.
pub fn write_store_table(content_dir: &Path, client_build: &str) -> Result<StoreReport, ToolError> {
    let bundle = ContentBundle::load(&bundle_path(content_dir, client_build), client_build)?;
    let table = store_table(&bundle.tables, &bundle.text);
    let path = store_table_path(content_dir, client_build);
    table.write(&path)?;
    Ok(StoreReport {
        path,
        mounts: table.mounts.len(),
        held_by_all: table.held_by_all.len(),
    })
}
