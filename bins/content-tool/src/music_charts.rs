// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool music-charts`: the music chart file ([`nw_content::music_charts`]) from the
//! game paks: every `.musicsheetc` of the music sheet folder, by its sheet id.

use std::path::{Path, PathBuf};

use nw_content::music_charts::{
    MUSIC_SHEET_FOLDER, MUSIC_SHEET_SUFFIX, MusicCharts, music_charts_path, parse_chart,
};

use crate::error::ToolError;
use crate::source::GameSource;

/// What `music-charts` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicChartsReport {
    /// The file.
    pub path: PathBuf,
    /// The charts in it.
    pub charts: usize,
    /// The notes over all charts.
    pub notes: usize,
}

/// Writes the music chart file of `client_build` into `out_dir` from `source`.
///
/// # Errors
///
/// Returns [`ToolError`] when the paks cannot be read, a chart is not one, or the file cannot
/// be written.
pub fn build_music_charts(
    source: &GameSource,
    client_build: &str,
    out_dir: &Path,
) -> Result<MusicChartsReport, ToolError> {
    let mut sheets = Vec::new();
    source.for_each_entry(
        |path| path.starts_with(MUSIC_SHEET_FOLDER) && path.ends_with(MUSIC_SHEET_SUFFIX),
        |path, bytes| {
            let sheet = path
                .trim_start_matches(MUSIC_SHEET_FOLDER)
                .trim_end_matches(MUSIC_SHEET_SUFFIX)
                .to_owned();
            sheets.push((sheet, parse_chart(&bytes)?));
            Ok::<(), ToolError>(())
        },
    )?;
    let notes = sheets.iter().map(|(_, notes)| notes.len()).sum();
    let path = music_charts_path(out_dir, client_build);
    let charts = MusicCharts::write(sheets, &path)?;
    Ok(MusicChartsReport {
        path,
        charts,
        notes,
    })
}
