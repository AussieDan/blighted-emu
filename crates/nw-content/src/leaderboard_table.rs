// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaderboard table: every board of one world that the game data defines, with its English
//! name and group. `content-tool leaderboards` writes it from the content bundle for
//! `status-reporter`, which sends the board list and the top rows to the public website.
//!
//! A board is one enabled `LeaderboardData` row whose name template needs at most the world and
//! the rotation (study note `docs/re/2026-09-18-leaderboards-and-analytics.md`). Boards without
//! the world (the cross-world Outpost Rush boards) are in the table too: the players of this
//! world see them. The two influence race boards also need a territory and a race id, so they are
//! not in the table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Tables;
use crate::bundle::english;
use crate::generated::LeaderboardData;
use crate::leaderboards::{Rotation, board_name};

/// File name start of the leaderboard table: `leaderboards-<client build>.json`.
pub const LEADERBOARD_TABLE_PREFIX: &str = "leaderboards-";
/// File name end of the leaderboard table.
pub const LEADERBOARD_TABLE_EXTENSION: &str = ".json";
/// A world id and a rotation name that fill a template, to check it.
const PROBE_WORLD: &str = "world";
const PROBE_ROTATION: &str = "w1";
/// The start and the end of a UI markup tag in English text (`<img src="..."></img>`).
const TAG_START: char = '<';
const TAG_END: char = '>';

/// One board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableBoard {
    /// The board's id (`LeaderboardId`, such as `MonthWarScore`).
    pub board_id: String,
    /// Its English name (`DisplayName`, such as `Score`), or the id when the game data has none.
    pub name: String,
    /// Its English group (`Category`, such as `War`), or `FirstLevelCategory` when the game data
    /// has no text for it.
    pub group: String,
    /// How often it starts again.
    pub rotation: Rotation,
    /// The stats service namespace (`Namespace`), such as `pc`.
    pub namespace: String,
    /// The name template (`LeaderboardDefinitionId`), such as `top-war-score.{world}.{month}`.
    pub template: String,
}

/// Every board, in game data order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderboardTable {
    /// The boards.
    pub boards: Vec<TableBoard>,
}

/// The leaderboard table cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum LeaderboardTableError {
    /// The file cannot be read or written.
    #[error("cannot read or write the leaderboard table {}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is no leaderboard table.
    #[error("the leaderboard table {} is not valid: {source}", .path.display())]
    Json {
        /// The file.
        path: PathBuf,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// The leaderboard table path of a client build: `<dir>/leaderboards-<client_build>.json`.
pub fn leaderboard_table_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!(
        "{LEADERBOARD_TABLE_PREFIX}{client_build}{LEADERBOARD_TABLE_EXTENSION}"
    ))
}

/// `text` without UI markup tags, trimmed.
fn plain_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            TAG_START => in_tag = true,
            TAG_END => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
    }
    plain.trim().to_owned()
}

/// Whether `template` names one board per rotation: it holds no placeholder but the world and
/// the rotation.
fn one_board_per_rotation(template: &str) -> bool {
    board_name(template, PROBE_WORLD, PROBE_ROTATION).is_some()
}

/// The board of `row`, with the English text of `text`; `None` for a disabled row, an unknown
/// rotation or a template that needs more than the world and the rotation.
fn table_board(row: &LeaderboardData, text: &BTreeMap<String, String>) -> Option<TableBoard> {
    if !row.is_enabled || !one_board_per_rotation(&row.leaderboard_definition_id) {
        return None;
    }
    let rotation = Rotation::parse(&row.rotation)?;
    let name = english(text, &row.display_name)
        .map(plain_text)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| row.leaderboard_id.clone());
    let group = english(text, &row.category)
        .map(plain_text)
        .filter(|group| !group.is_empty())
        .unwrap_or_else(|| plain_text(&row.first_level_category));
    Some(TableBoard {
        board_id: row.leaderboard_id.clone(),
        name,
        group,
        rotation,
        namespace: row.namespace.clone(),
        template: row.leaderboard_definition_id.clone(),
    })
}

/// The leaderboard table of the `LeaderboardData` rows `rows`, with the English text of `text`.
pub fn leaderboard_table_of<'a>(
    rows: impl IntoIterator<Item = &'a LeaderboardData>,
    text: &BTreeMap<String, String>,
) -> LeaderboardTable {
    LeaderboardTable {
        boards: rows
            .into_iter()
            .filter_map(|row| table_board(row, text))
            .collect(),
    }
}

/// The leaderboard table of `tables`, with the English text of `text`.
pub fn leaderboard_table(tables: &Tables, text: &BTreeMap<String, String>) -> LeaderboardTable {
    leaderboard_table_of(
        tables.leaderboard_data.iter().flat_map(|table| &table.rows),
        text,
    )
}

impl LeaderboardTable {
    /// Reads the table at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`LeaderboardTableError`] when the file cannot be read or is no leaderboard table.
    pub fn load(path: &Path) -> Result<Self, LeaderboardTableError> {
        let bytes = std::fs::read(path).map_err(|source| LeaderboardTableError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| LeaderboardTableError::Json {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Writes the table to `path`, creating parent folders.
    ///
    /// # Errors
    ///
    /// Returns [`LeaderboardTableError`] when the file cannot be written.
    pub fn write(&self, path: &Path) -> Result<(), LeaderboardTableError> {
        let io = |source| LeaderboardTableError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(io)?;
        }
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|source| LeaderboardTableError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        std::fs::write(path, bytes).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leaderboards::test_rows::board_row;

    const MONTH_TEMPLATE: &str = "top-war-score.{world}.{month}";
    const BUILD: &str = "25810808";

    fn row(id: &str, template: &str, rotation: &str) -> LeaderboardData {
        LeaderboardData {
            display_name: "@ui_leaderboard_display_war_score".to_owned(),
            category: "@ui_leaderboard_category_or".to_owned(),
            first_level_category: "Faction War".to_owned(),
            rotation: rotation.to_owned(),
            ..board_row(id, template)
        }
    }

    fn text() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "ui_leaderboard_display_war_score".to_owned(),
                "Score".to_owned(),
            ),
            (
                "ui_leaderboard_category_or".to_owned(),
                "Outpost Rush <img src=\"lyshineui/images/icon\" scale=\"1.0\"></img>".to_owned(),
            ),
        ])
    }

    #[test]
    fn a_board_gets_its_english_name_and_group_without_markup() {
        let rows = [row("MonthWarScore", MONTH_TEMPLATE, "Month")];
        assert_eq!(
            leaderboard_table_of(&rows, &text()).boards,
            [TableBoard {
                board_id: "MonthWarScore".to_owned(),
                name: "Score".to_owned(),
                group: "Outpost Rush".to_owned(),
                rotation: Rotation::Month,
                namespace: "pc".to_owned(),
                template: MONTH_TEMPLATE.to_owned(),
            }]
        );
    }

    #[test]
    fn a_board_without_text_keeps_its_id_and_first_category() {
        let rows = [row("MonthWarScore", MONTH_TEMPLATE, "Month")];
        let board = &leaderboard_table_of(&rows, &BTreeMap::new()).boards[0];
        assert_eq!(
            (board.name.as_str(), board.group.as_str()),
            ("MonthWarScore", "Faction War")
        );
    }

    #[test]
    fn boards_that_need_more_than_world_and_rotation_are_left_out() {
        let disabled = LeaderboardData {
            is_enabled: false,
            ..row("Off", MONTH_TEMPLATE, "Month")
        };
        let rows = [
            row(
                "InfluenceRace",
                "top-company-influence-race-influence.{territory}.{world}.{influenceRaceId}",
                "Season",
            ),
            row("NoRotation", MONTH_TEMPLATE, "Day"),
            disabled,
            row("SeasonWarScore", "top-war-score.{world}.{season}", "Season"),
            row("CrossWorld", "top-outpost-rush-wins.{season}", "Season"),
        ];
        let ids: Vec<String> = leaderboard_table_of(&rows, &text())
            .boards
            .into_iter()
            .map(|board| board.board_id)
            .collect();
        assert_eq!(ids, ["SeasonWarScore", "CrossWorld"]);
    }

    #[test]
    fn the_table_round_trips_through_its_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = leaderboard_table_path(dir.path(), BUILD);
        assert!(path.ends_with("leaderboards-25810808.json"));
        let rows = [row("MonthWarScore", MONTH_TEMPLATE, "Month")];
        let table = leaderboard_table_of(&rows, &text());
        table.write(&path).expect("write");
        assert_eq!(LeaderboardTable::load(&path).expect("load"), table);
    }

    #[test]
    fn a_missing_table_is_an_error() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(matches!(
            LeaderboardTable::load(&leaderboard_table_path(dir.path(), BUILD)),
            Err(LeaderboardTableError::Io { .. })
        ));
    }
}
