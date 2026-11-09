// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaderboard report (`POST /api/ingest/leaderboards`): every board of the leaderboard
//! table and the top rows of its current rotation. The website replaces the world's boards with
//! each report, so every report holds the full board list, also boards without rows.
//!
//! The values come from the global database (the sims write them, `edge-http` serves them to
//! the client), the names from the world database. A row whose entity is no character or
//! company of the world (a group, a faction, a deleted character) has no name and is left out;
//! the rows that stay are ranked 1, 2, 3, ... in board order (equal values: the older change
//! first, as the client's board shows them).

use std::collections::{BTreeSet, HashMap};

use nw_content::leaderboard_table::{LeaderboardTable, TableBoard};
use nw_content::leaderboards::{Rotation, RotationCalendar, board_name};
use nw_db::{BoardEntry, DbError, PgPool, display_names, leaderboard_page};
use nw_login::stats::board_order;
use serde::Serialize;
use uuid::Uuid;

/// Most rows of one board the website takes.
pub const MAX_ROWS_PER_BOARD: u32 = 100;
/// The first page of a board.
const FIRST_PAGE: u32 = 1;
/// The first rank.
const FIRST_RANK: u32 = 1;

/// One board, as the website reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteBoard {
    /// The board id (`LeaderboardId`).
    pub board_id: String,
    /// Its English name.
    pub name: String,
    /// Its English group.
    pub group: String,
    /// Its rotation.
    pub rotation: Rotation,
}

/// One row of a board, as the website reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteRow {
    /// The board id.
    pub board_id: String,
    /// The board's rotation.
    pub rotation: Rotation,
    /// The place, 1 to [`MAX_ROWS_PER_BOARD`].
    pub rank: u32,
    /// The value, rounded to a whole number.
    pub value: i64,
    /// The character or company name.
    pub name: String,
}

/// One leaderboard report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// Every board.
    pub boards: Vec<SiteBoard>,
    /// The top rows of every board.
    pub rows: Vec<SiteRow>,
}

/// The board id the client asks for in the rotation of `unix_seconds` in world `world_id`;
/// `None` before the first rotation starts.
pub fn current_stat_id(
    board: &TableBoard,
    calendar: &RotationCalendar,
    world_id: Uuid,
    unix_seconds: i64,
) -> Option<String> {
    let rotation = calendar.name(board.rotation, unix_seconds)?;
    board_name(&board.template, &world_id.to_string(), &rotation)
}

/// A value as a whole number (a float cast saturates; `NaN` gives 0).
fn whole(value: f64) -> i64 {
    value.round() as i64
}

/// The report of `boards`, each with its entries best first, and the entity names `names`.
pub fn snapshot(
    boards: &[(&TableBoard, Vec<BoardEntry>)],
    names: &HashMap<Uuid, String>,
) -> Snapshot {
    let rows = boards
        .iter()
        .flat_map(|(board, entries)| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = Uuid::parse_str(&entry.entity_id).ok()?;
                    Some((names.get(&id)?, entry.value))
                })
                .zip(FIRST_RANK..=MAX_ROWS_PER_BOARD)
                .map(|((name, value), rank)| SiteRow {
                    board_id: board.board_id.clone(),
                    rotation: board.rotation,
                    rank,
                    value: whole(value),
                    name: name.clone(),
                })
        })
        .collect();
    Snapshot {
        boards: boards
            .iter()
            .map(|(board, _)| SiteBoard {
                board_id: board.board_id.clone(),
                name: board.name.clone(),
                group: board.group.clone(),
                rotation: board.rotation,
            })
            .collect(),
        rows,
    }
}

/// The databases the report reads.
#[derive(Debug, Clone)]
pub struct BoardDatabases {
    /// The global database: the leaderboard values.
    pub global: PgPool,
    /// The world database: the character and company names.
    pub world: PgPool,
}

/// The report of every board of `table` in the rotations of `unix_seconds`, read from
/// `databases`.
///
/// # Errors
///
/// Returns [`DbError`] when a database fails; then there is no report (a partial one would
/// empty the website's boards).
pub async fn read_snapshot(
    databases: &BoardDatabases,
    table: &LeaderboardTable,
    calendar: &RotationCalendar,
    world_id: Uuid,
    unix_seconds: i64,
) -> Result<Snapshot, DbError> {
    let mut boards = Vec::with_capacity(table.boards.len());
    for board in &table.boards {
        let entries = match current_stat_id(board, calendar, world_id, unix_seconds) {
            Some(stat_id) => {
                leaderboard_page(
                    &databases.global,
                    &board.namespace,
                    &stat_id,
                    board_order(&stat_id),
                    FIRST_PAGE,
                    MAX_ROWS_PER_BOARD,
                )
                .await?
                .entries
            }
            None => Vec::new(),
        };
        boards.push((board, entries));
    }
    let ids: Vec<Uuid> = boards
        .iter()
        .flat_map(|(_, entries)| entries)
        .filter_map(|entry| Uuid::parse_str(&entry.entity_id).ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let names = if ids.is_empty() {
        HashMap::new()
    } else {
        display_names(&databases.world, &ids)
            .await?
            .into_iter()
            .collect()
    };
    Ok(snapshot(&boards, &names))
}

#[cfg(test)]
mod tests {
    use nw_common::time::CivilDate;

    use super::*;

    const WORLD: Uuid = Uuid::from_u128(0x8d0f_4c2e_0000_4000_8000_0000_0000_0001);
    const ANN: Uuid = Uuid::from_u128(1);
    const BOB: Uuid = Uuid::from_u128(2);
    const ALPHA: Uuid = Uuid::from_u128(3);
    /// 2026-06-30 09:53:59 UTC: rotations `w178`, `m41`, `s11`.
    const NOW: i64 = 1_782_813_239;
    /// 2023-02-07 01:59:59 UTC: before the first rotation.
    const BEFORE_START: i64 = 1_675_735_199;
    const CALENDAR: RotationCalendar = RotationCalendar {
        start: CivilDate {
            year: 2023,
            month: 2,
            day: 7,
        },
        reset_hour: 2,
        season: 11,
    };

    fn board(id: &str, template: &str, rotation: Rotation) -> TableBoard {
        TableBoard {
            board_id: id.to_owned(),
            name: "Score".to_owned(),
            group: "War".to_owned(),
            rotation,
            namespace: "pc".to_owned(),
            template: template.to_owned(),
        }
    }

    fn entry(entity: &str, value: f64, rank: u64) -> BoardEntry {
        BoardEntry {
            entity_id: entity.to_owned(),
            value,
            rank,
        }
    }

    fn names() -> HashMap<Uuid, String> {
        HashMap::from([
            (ANN, "Ann".to_owned()),
            (BOB, "Bob".to_owned()),
            (ALPHA, "Alpha".to_owned()),
        ])
    }

    #[test]
    fn the_current_stat_id_fills_the_world_and_the_rotation() {
        let month = board(
            "MonthWarScore",
            "top-war-score.{world}.{month}",
            Rotation::Month,
        );
        assert_eq!(
            current_stat_id(&month, &CALENDAR, WORLD, NOW).as_deref(),
            Some("top-war-score.8d0f4c2e-0000-4000-8000-000000000001.m41")
        );
        assert_eq!(
            current_stat_id(&month, &CALENDAR, WORLD, BEFORE_START),
            None
        );
    }

    #[test]
    fn rows_get_names_and_unique_ranks_and_empty_boards_stay() {
        let score = board("MonthWarScore", "t.{world}.{month}", Rotation::Month);
        let wins = board(
            "SeasonCompanyWarWins",
            "w.{world}.{season}",
            Rotation::Season,
        );
        let empty = board("WeekExpedition", "e.{world}.{week}", Rotation::Week);
        let boards = [
            (
                &score,
                vec![
                    entry(&ANN.to_string(), 9.6, 1),
                    entry("a-group-id", 9.0, 2),
                    entry(&Uuid::from_u128(99).to_string(), 8.0, 3),
                    entry(&BOB.to_string(), 9.6, 1),
                ],
            ),
            (&wins, vec![entry(&ALPHA.to_string(), 2.0, 1)]),
            (&empty, Vec::new()),
        ];
        let report = snapshot(&boards, &names());
        assert_eq!(report.boards.len(), 3, "the empty board stays in the list");
        let rows: Vec<(&str, u32, i64, &str)> = report
            .rows
            .iter()
            .map(|row| {
                (
                    row.board_id.as_str(),
                    row.rank,
                    row.value,
                    row.name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("MonthWarScore", 1, 10, "Ann"),
                ("MonthWarScore", 2, 10, "Bob"),
                ("SeasonCompanyWarWins", 1, 2, "Alpha"),
            ]
        );
    }

    #[test]
    fn a_board_keeps_at_most_the_website_rows() {
        let score = board("MonthWarScore", "t.{world}.{month}", Rotation::Month);
        let many: Vec<BoardEntry> = (0..=MAX_ROWS_PER_BOARD)
            .map(|place| entry(&ANN.to_string(), f64::from(place), u64::from(place)))
            .collect();
        let report = snapshot(&[(&score, many)], &names());
        assert_eq!(report.rows.len(), MAX_ROWS_PER_BOARD as usize);
        assert_eq!(
            report.rows.last().map(|row| row.rank),
            Some(MAX_ROWS_PER_BOARD)
        );
    }

    #[test]
    fn the_report_has_the_website_json_shape() {
        let score = board("MonthWarScore", "t.{world}.{month}", Rotation::Month);
        let report = snapshot(&[(&score, vec![entry(&ANN.to_string(), 3.0, 1)])], &names());
        assert_eq!(
            serde_json::to_value(report).expect("json"),
            serde_json::json!({
                "boards": [{
                    "boardId": "MonthWarScore",
                    "name": "Score",
                    "group": "War",
                    "rotation": "month"
                }],
                "rows": [{
                    "boardId": "MonthWarScore",
                    "rotation": "month",
                    "rank": 1,
                    "value": 3,
                    "name": "Ann"
                }]
            })
        );
    }
}
