// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool quick-courses`: the quick course path file ([`nw_world::quick_courses`]) from
//! the game paks: every `.dynamicslice` under the course folders of the asset catalog.

use std::path::{Path, PathBuf};

use nw_formats::asset_catalog::{ASSET_CATALOG_ENTRY, slice_assets_in};
use nw_world::quick_courses::{
    QUICK_COURSE_FOLDERS, QuickCoursePath, QuickCoursePaths, quick_courses_path,
};

use crate::error::ToolError;
use crate::source::GameSource;
use crate::world::{WorldBuildError, stream_of};

/// What `quick-courses` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickCoursesReport {
    /// The file.
    pub path: PathBuf,
    /// The dynamic slices of the course folders in the catalog.
    pub slices: usize,
    /// The courses in them.
    pub courses: usize,
}

/// Writes the quick course path file of `client_build` into `out_dir` from `source`.
///
/// # Errors
///
/// Returns [`ToolError`] when the paks, the catalog or a slice cannot be read, or the file
/// cannot be written.
pub fn build_quick_courses(
    source: &GameSource,
    client_build: &str,
    out_dir: &Path,
) -> Result<QuickCoursesReport, ToolError> {
    let mut catalog = Vec::new();
    source.for_each_entry(
        |path| path == ASSET_CATALOG_ENTRY,
        |_, bytes| {
            catalog = bytes;
            Ok::<(), ToolError>(())
        },
    )?;
    let mut slices = std::collections::HashSet::new();
    for folder in QUICK_COURSE_FOLDERS {
        let assets = slice_assets_in(&catalog, folder).map_err(WorldBuildError::from)?;
        slices.extend(assets.into_keys());
    }
    let mut paths = Vec::new();
    source.for_each_entry(
        |path| slices.contains(path),
        |path, bytes| {
            let root = stream_of(path, &bytes)?;
            paths.extend(QuickCoursePath::all_of(&root));
            Ok::<(), ToolError>(())
        },
    )?;
    let table = QuickCoursePaths::of(paths);
    let path = quick_courses_path(out_dir, client_build);
    table.write(&path)?;
    Ok(QuickCoursesReport {
        path,
        slices: slices.len(),
        courses: table.len(),
    })
}
