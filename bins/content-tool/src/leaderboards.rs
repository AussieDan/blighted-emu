// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool leaderboards`: the leaderboard table ([`nw_content::leaderboard_table`]) from the
//! content bundle, for `status-reporter`. It needs no game install.

use std::path::{Path, PathBuf};

use nw_content::ContentBundle;
use nw_content::bundle::bundle_path;
use nw_content::leaderboard_table::{leaderboard_table, leaderboard_table_path};

use crate::error::ToolError;

/// What `leaderboards` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderboardsReport {
    /// The file.
    pub path: PathBuf,
    /// The boards in it.
    pub boards: usize,
}

/// Writes the leaderboard table of `client_build` into `content_dir`, from the bundle in it.
///
/// # Errors
///
/// Returns [`ToolError`] when the bundle cannot be read or the table cannot be written.
pub fn write_leaderboard_table(
    content_dir: &Path,
    client_build: &str,
) -> Result<LeaderboardsReport, ToolError> {
    let bundle = ContentBundle::load(&bundle_path(content_dir, client_build), client_build)?;
    let table = leaderboard_table(&bundle.tables, &bundle.text);
    let path = leaderboard_table_path(content_dir, client_build);
    table.write(&path)?;
    Ok(LeaderboardsReport {
        path,
        boards: table.boards.len(),
    })
}

#[cfg(test)]
mod tests {
    use nw_content::leaderboard_table::LeaderboardTable;
    use nw_content::leaderboards::test_rows::board_row;
    use nw_content::table::Table;

    use super::*;

    const BUILD: &str = "25810808";

    #[test]
    fn the_table_holds_the_boards_of_the_bundle() {
        let dir = tempfile::tempdir().expect("dir");
        let mut bundle = ContentBundle {
            client_build: BUILD.to_owned(),
            ..ContentBundle::default()
        };
        bundle.tables.leaderboard_data.push(Table {
            source_path: "leaderboarddata.datasheet".to_owned(),
            unique_id: String::new(),
            rows: vec![nw_content::generated::LeaderboardData {
                rotation: "Season".to_owned(),
                ..board_row("SeasonWarScore", "top-war-score.{world}.{season}")
            }],
        });
        bundle
            .save(&bundle_path(dir.path(), BUILD))
            .expect("bundle");
        let report = write_leaderboard_table(dir.path(), BUILD).expect("table");
        assert_eq!(report.boards, 1);
        let table = LeaderboardTable::load(&report.path).expect("load");
        assert_eq!(table.boards[0].board_id, "SeasonWarScore");
    }
}
