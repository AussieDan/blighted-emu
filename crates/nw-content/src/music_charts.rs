// SPDX-License-Identifier: AGPL-3.0-or-later
//! The note charts of the music sheets: one `sharedassets/musicsheets/<sheet>.musicsheetc` file
//! per sheet of the song book (655 in build 25810808). A file is a version byte, a
//! little-endian `u32` note count, then per note its key (`u8`, the 1-to-7 key the client names
//! in a 4892), its lane (`i8`: -1 for keys 2 and 6, 0 for 3, 1 for 4 and 7) and its time
//! (little-endian `f32`, seconds into the song). Observed: every file of the build is that size
//! exactly; retail `20261010-113829-464` played `set_azothflute_part1_easy`, and its 4892 calls
//! name each chart note by index with that note's key.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use nw_formats::objectstream::lowercase_crc32;
use serde::{Deserialize, Serialize};

/// The pak folder of the charts.
pub const MUSIC_SHEET_FOLDER: &str = "sharedassets/musicsheets/";
/// The file suffix of a chart; the sheet id is the file name before it.
pub const MUSIC_SHEET_SUFFIX: &str = ".musicsheetc";
const FILE_PREFIX: &str = "music-charts-";
const FILE_SUFFIX: &str = ".json";
/// Bytes before the first note: the version and the count.
const HEADER_LEN: usize = 5;
/// Bytes of one note: key, lane and time.
const NOTE_LEN: usize = 6;

/// The chart file of `client_build` in `dir`.
pub fn music_charts_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!("{FILE_PREFIX}{client_build}{FILE_SUFFIX}"))
}

/// Why a chart or the chart file cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum MusicChartError {
    /// A chart's size does not fit its note count.
    #[error("music sheet chart of {len} bytes")]
    Size {
        /// Its size.
        len: usize,
    },
    /// The file cannot be read or written.
    #[error("music chart file {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file is not the charts' JSON.
    #[error("music chart file {path}: {source}")]
    Json {
        /// The file.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
}

/// One note of a chart.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChartNote {
    /// The key, 1 to 7.
    pub key: u8,
    /// The lane: -1, 0 or 1.
    pub lane: i8,
    /// Seconds into the song.
    pub time_s: f32,
}

/// The notes of the chart `bytes`.
///
/// # Errors
///
/// Returns [`MusicChartError::Size`] when the size does not fit the note count.
pub fn parse_chart(bytes: &[u8]) -> Result<Vec<ChartNote>, MusicChartError> {
    let size = || MusicChartError::Size { len: bytes.len() };
    let count = bytes
        .get(1..HEADER_LEN)
        .and_then(|count| <[u8; 4]>::try_from(count).ok())
        .map(u32::from_le_bytes)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(size)?;
    if count.checked_mul(NOTE_LEN).and_then(|notes| notes.checked_add(HEADER_LEN))
        != Some(bytes.len())
    {
        return Err(size());
    }
    Ok(bytes[HEADER_LEN..]
        .chunks_exact(NOTE_LEN)
        .map(|note| ChartNote {
            key: note[0],
            lane: i8::from_le_bytes([note[1]]),
            time_s: f32::from_le_bytes([note[2], note[3], note[4], note[5]]),
        })
        .collect())
}

/// Every chart, by sheet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MusicCharts {
    /// The charts by the CRC-32 of their lowercase sheet id, the sheet's wire id (4879).
    by_sheet: HashMap<u32, Vec<ChartNote>>,
}

/// The file form: the charts by lowercase sheet id.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ChartFile {
    sheets: BTreeMap<String, Vec<ChartNote>>,
}

impl MusicCharts {
    /// The charts of `sheets` (sheet id, notes).
    pub fn of(sheets: impl IntoIterator<Item = (String, Vec<ChartNote>)>) -> Self {
        Self {
            by_sheet: sheets
                .into_iter()
                .map(|(sheet, notes)| (lowercase_crc32(&sheet), notes))
                .collect(),
        }
    }

    /// The chart of sheet `sheet` (its wire id).
    pub fn chart(&self, sheet: u32) -> Option<&[ChartNote]> {
        self.by_sheet.get(&sheet).map(Vec::as_slice)
    }

    /// Charts it holds.
    pub fn len(&self) -> usize {
        self.by_sheet.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.by_sheet.is_empty()
    }

    /// Writes `sheets` (sheet id, notes) to `path` as the chart file.
    ///
    /// # Errors
    ///
    /// Returns [`MusicChartError`] when the file cannot be written.
    pub fn write(
        sheets: impl IntoIterator<Item = (String, Vec<ChartNote>)>,
        path: &Path,
    ) -> Result<usize, MusicChartError> {
        let file = ChartFile {
            sheets: sheets
                .into_iter()
                .map(|(sheet, notes)| (sheet.to_lowercase(), notes))
                .collect(),
        };
        let text = serde_json::to_string(&file).map_err(|source| MusicChartError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(|source| MusicChartError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(file.sheets.len())
    }

    /// The chart file of `client_build` in `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`MusicChartError`] when the file cannot be read or is not the charts.
    pub fn load(dir: &Path, client_build: &str) -> Result<Self, MusicChartError> {
        let path = music_charts_path(dir, client_build);
        let text = std::fs::read_to_string(&path).map_err(|source| MusicChartError::Io {
            path: path.clone(),
            source,
        })?;
        let file: ChartFile =
            serde_json::from_str(&text).map_err(|source| MusicChartError::Json { path, source })?;
        Ok(Self::of(file.sheets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first two notes of retail `set_azothflute_part1_easy`, as the file holds them.
    fn retail_head() -> Vec<u8> {
        let mut bytes = vec![0x01, 0x02, 0x00, 0x00, 0x00];
        bytes.extend([0x03, 0x00]);
        bytes.extend(2.400_041_6_f32.to_le_bytes());
        bytes.extend([0x06, 0xff]);
        bytes.extend(3.100_046_2_f32.to_le_bytes());
        bytes
    }

    #[test]
    fn a_chart_is_its_count_of_six_byte_notes() {
        let notes = parse_chart(&retail_head()).expect("chart");
        assert_eq!(
            notes,
            [
                ChartNote {
                    key: 3,
                    lane: 0,
                    time_s: 2.400_041_6,
                },
                ChartNote {
                    key: 6,
                    lane: -1,
                    time_s: 3.100_046_2,
                },
            ]
        );
        let mut short = retail_head();
        short.pop();
        assert!(parse_chart(&short).is_err(), "a size that does not fit");
    }

    #[test]
    fn the_charts_are_found_by_the_sheet_wire_id_after_a_round_trip() {
        let dir = tempfile::tempdir().expect("dir");
        let path = music_charts_path(dir.path(), "1");
        let notes = parse_chart(&retail_head()).expect("chart");
        let written = MusicCharts::write(
            [("Set_AzothFlute_Part1_Easy".to_owned(), notes.clone())],
            &path,
        )
        .expect("write");
        assert_eq!(written, 1);
        let charts = MusicCharts::load(dir.path(), "1").expect("load");
        assert_eq!(
            charts.chart(lowercase_crc32("set_azothflute_part1_easy")),
            Some(notes.as_slice())
        );
    }
}
