// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaderboard report (`POST /api/ingest/leaderboards`): every board of the leaderboard
//! table and the top rows of its current rotation. The website replaces the world's boards with
//! each report, so every report holds the full board list, also boards without rows.
//!
//! The values come from the global database (the sims write them, `edge-http` serves them to
//! the client), the names from the world database. A character or company row has one name; a
//! group row has the names of its members that this world knows. A row without any name (a
//! faction, a deleted character, a player of another world on a cross-world board) is left out,
//! and so is a value that is not a finite number. The ranks are the ranks the client shows:
//! equal values share a rank and the next rank skips (1, 1, 3), so a left-out row leaves a gap.

use std::collections::{BTreeSet, HashMap};

use nw_common::time::iso8601_seconds;
use nw_content::leaderboard_table::{
    EntityKind, GROUP_MEMBER_SEPARATOR, LeaderboardTable, TableBoard,
};
use nw_content::leaderboards::{Rotation, RotationCalendar, WORLD_PLACEHOLDER, board_name};
use nw_db::{BoardEntry, BoardOrder, DbError, PgPool, display_names, leaderboard_page};
use nw_login::stats::board_order;
use serde::Serialize;
use uuid::Uuid;

/// Most rows of one board the report reads (the top of the board).
pub const MAX_ROWS_PER_BOARD: u32 = 100;
/// The last rank the website takes.
pub const MAX_RANK: u64 = 100;
/// The first page of a board.
const FIRST_PAGE: u32 = 1;

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
    /// Its English sub-group (`SecondLevelCategory`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subgroup: Option<String>,
    /// The English label of its value (`ValueString`): `Time` values are seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_kind: Option<String>,
    /// Whether the lowest value ranks first (`min-` boards, such as clear times).
    pub lower_is_better: bool,
    /// Whether the board holds the players of every world (its template has no world).
    pub regional: bool,
    /// What it ranks.
    pub entity_kind: EntityKind,
    /// The current rotation (`w178`, `m41`, `s11`); none before the first rotation starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation_id: Option<String>,
    /// When the current rotation ends, ISO 8601 UTC; none for a season without a known end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// One row of a board, as the website reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteRow {
    /// The board id.
    pub board_id: String,
    /// The board's rotation.
    pub rotation: Rotation,
    /// The place, 1 to [`MAX_RANK`]; equal values share it.
    pub rank: u64,
    /// The value as the client gets it (seconds for `Time` boards).
    pub value: f64,
    /// The character or company name, or the known members of the group; at least one.
    pub names: Vec<String>,
}

/// One leaderboard report.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Snapshot {
    /// Every board.
    pub boards: Vec<SiteBoard>,
    /// The top rows of every board.
    pub rows: Vec<SiteRow>,
}

/// The rotation calendar of the boards and the end of the current season (Unix seconds, from
/// `javelin.leaderboard-season-end-datetime`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardClock {
    /// The rotations.
    pub calendar: RotationCalendar,
    /// The season end; `None` when the config has no valid one.
    pub season_end: Option<i64>,
}

/// One board in its current rotation with its top entries, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentBoard<'a> {
    /// The board.
    pub board: &'a TableBoard,
    /// The current rotation's name.
    pub rotation_id: Option<String>,
    /// When the current rotation ends, Unix seconds.
    pub resets_at: Option<i64>,
    /// The top entries.
    pub entries: Vec<BoardEntry>,
}

/// The board id the client asks for in the rotation `rotation_id` in world `world_id`.
pub fn stat_id(board: &TableBoard, world_id: Uuid, rotation_id: &str) -> Option<String> {
    board_name(&board.template, &world_id.to_string(), rotation_id)
}

/// The ids of the characters or companies that an entity of `kind` stands for: the entity
/// itself, or each member of a group. Ids that are no UUID are left out.
fn entity_members(kind: EntityKind, entity_id: &str) -> Vec<Uuid> {
    match kind {
        EntityKind::Character | EntityKind::Company => {
            Uuid::parse_str(entity_id).ok().into_iter().collect()
        }
        EntityKind::Group => entity_id
            .split(GROUP_MEMBER_SEPARATOR)
            .filter_map(|member| Uuid::parse_str(member).ok())
            .collect(),
    }
}

fn site_board(current: &CurrentBoard<'_>) -> SiteBoard {
    let board = current.board;
    SiteBoard {
        board_id: board.board_id.clone(),
        name: board.name.clone(),
        group: board.group.clone(),
        rotation: board.rotation,
        subgroup: board.subgroup.clone(),
        value_kind: board.value_kind.clone(),
        lower_is_better: board_order(&board.template) == BoardOrder::LowestFirst,
        regional: !board.template.contains(WORLD_PLACEHOLDER),
        entity_kind: board.entity_kind,
        rotation_id: current.rotation_id.clone(),
        resets_at: current
            .resets_at
            .and_then(|seconds| u64::try_from(seconds).ok())
            .map(iso8601_seconds),
    }
}

fn site_rows<'a>(
    current: &'a CurrentBoard<'_>,
    names: &'a HashMap<Uuid, String>,
) -> impl Iterator<Item = SiteRow> + 'a {
    let board = current.board;
    current.entries.iter().filter_map(move |entry| {
        let names: Vec<String> = entity_members(board.entity_kind, &entry.entity_id)
            .iter()
            .filter_map(|id| names.get(id).cloned())
            .collect();
        (!names.is_empty() && entry.value.is_finite() && entry.rank <= MAX_RANK).then(|| SiteRow {
            board_id: board.board_id.clone(),
            rotation: board.rotation,
            rank: entry.rank,
            value: entry.value,
            names,
        })
    })
}

/// The report of `boards` and the entity names `names`.
pub fn snapshot(boards: &[CurrentBoard<'_>], names: &HashMap<Uuid, String>) -> Snapshot {
    Snapshot {
        boards: boards.iter().map(site_board).collect(),
        rows: boards
            .iter()
            .flat_map(|current| site_rows(current, names))
            .collect(),
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
    clock: &BoardClock,
    world_id: Uuid,
    unix_seconds: i64,
) -> Result<Snapshot, DbError> {
    let mut boards = Vec::with_capacity(table.boards.len());
    for board in &table.boards {
        let rotation_id = clock.calendar.name(board.rotation, unix_seconds);
        let entries = match rotation_id
            .as_deref()
            .and_then(|rotation_id| stat_id(board, world_id, rotation_id))
        {
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
        boards.push(CurrentBoard {
            board,
            resets_at: clock
                .calendar
                .ends_at(board.rotation, unix_seconds, clock.season_end),
            rotation_id,
            entries,
        });
    }
    let ids: Vec<Uuid> = boards
        .iter()
        .flat_map(|current| {
            current
                .entries
                .iter()
                .flat_map(|entry| entity_members(current.board.entity_kind, &entry.entity_id))
        })
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
    /// A character this world does not know (deleted, or of another world).
    const STRANGER: Uuid = Uuid::from_u128(99);
    /// 2026-06-30 09:53:59 UTC: rotations `w178`, `m41`, `s11`.
    const NOW: i64 = 1_782_813_239;
    /// 2026-07-01 00:00:00 UTC: the end of `m41`.
    const MONTH_END: i64 = 1_782_864_000;
    const CALENDAR: RotationCalendar = RotationCalendar {
        start: CivilDate {
            year: 2023,
            month: 2,
            day: 7,
        },
        reset_hour: 2,
        season: 11,
    };

    fn board(id: &str, template: &str, rotation: Rotation, kind: EntityKind) -> TableBoard {
        TableBoard {
            board_id: id.to_owned(),
            name: "Score".to_owned(),
            group: "War".to_owned(),
            subgroup: Some("War".to_owned()),
            value_kind: Some("Score".to_owned()),
            entity_kind: kind,
            rotation,
            namespace: "pc".to_owned(),
            template: template.to_owned(),
        }
    }

    fn current(board: &TableBoard, entries: Vec<BoardEntry>) -> CurrentBoard<'_> {
        CurrentBoard {
            board,
            rotation_id: Some("m41".to_owned()),
            resets_at: Some(MONTH_END),
            entries,
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

    fn rows(report: &Snapshot) -> Vec<(&str, u64, f64, Vec<&str>)> {
        report
            .rows
            .iter()
            .map(|row| {
                (
                    row.board_id.as_str(),
                    row.rank,
                    row.value,
                    row.names.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn the_stat_id_fills_the_world_and_the_rotation() {
        let month = board(
            "MonthWarScore",
            "top-war-score.{world}.{month}",
            Rotation::Month,
            EntityKind::Character,
        );
        let rotation = CALENDAR.name(month.rotation, NOW).expect("rotation");
        assert_eq!(
            stat_id(&month, WORLD, &rotation).as_deref(),
            Some("top-war-score.8d0f4c2e-0000-4000-8000-000000000001.m41")
        );
    }

    #[test]
    fn rows_keep_the_shared_ranks_and_decimals_and_empty_boards_stay() {
        let score = board(
            "MonthWarScore",
            "t.{world}.{month}",
            Rotation::Month,
            EntityKind::Character,
        );
        let wins = board(
            "SeasonCompanyWarWins",
            "w.{world}.{season}",
            Rotation::Season,
            EntityKind::Company,
        );
        let empty = board(
            "WeekExpedition",
            "e.{world}.{week}",
            Rotation::Week,
            EntityKind::Group,
        );
        let boards = [
            current(
                &score,
                vec![
                    entry(&ANN.to_string(), 9.6, 1),
                    entry(&BOB.to_string(), 9.6, 1),
                    entry(&STRANGER.to_string(), 8.0, 3),
                    entry(&ALPHA.to_string(), f64::NAN, 4),
                    entry("no-uuid", 7.0, 5),
                    entry(&ALPHA.to_string(), 1.25, 6),
                ],
            ),
            current(&wins, vec![entry(&ALPHA.to_string(), 2.0, 1)]),
            current(&empty, Vec::new()),
        ];
        let report = snapshot(&boards, &names());
        assert_eq!(report.boards.len(), 3, "the empty board stays in the list");
        assert_eq!(
            rows(&report),
            [
                ("MonthWarScore", 1, 9.6, vec!["Ann"]),
                ("MonthWarScore", 1, 9.6, vec!["Bob"]),
                ("MonthWarScore", 6, 1.25, vec!["Alpha"]),
                ("SeasonCompanyWarWins", 1, 2.0, vec!["Alpha"]),
            ]
        );
    }

    #[test]
    fn a_group_row_names_the_members_this_world_knows() {
        let clear = board(
            "WeekExpeditionClearTime",
            "min-dungeon-group-gold-medal-expedition-clear-time.D.1.{world}.{week}",
            Rotation::Week,
            EntityKind::Group,
        );
        let group = |members: &[Uuid]| {
            members
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(&GROUP_MEMBER_SEPARATOR.to_string())
        };
        let boards = [current(
            &clear,
            vec![
                entry(&group(&[ANN, STRANGER, BOB]), 1046.5, 1),
                entry(&group(&[STRANGER]), 1100.0, 2),
                entry(&group(&[ALPHA]), 1200.0, 3),
            ],
        )];
        let report = snapshot(&boards, &names());
        assert_eq!(
            rows(&report),
            [
                ("WeekExpeditionClearTime", 1, 1046.5, vec!["Ann", "Bob"]),
                ("WeekExpeditionClearTime", 3, 1200.0, vec!["Alpha"]),
            ]
        );
        let site = &report.boards[0];
        assert!(site.lower_is_better && !site.regional);
    }

    #[test]
    fn ranks_past_the_website_limit_are_left_out() {
        let score = board(
            "MonthWarScore",
            "t.{world}.{month}",
            Rotation::Month,
            EntityKind::Character,
        );
        let boards = [current(
            &score,
            vec![
                entry(&ANN.to_string(), 2.0, MAX_RANK),
                entry(&BOB.to_string(), 1.0, MAX_RANK + 1),
            ],
        )];
        let report = snapshot(&boards, &names());
        assert_eq!(
            rows(&report),
            [("MonthWarScore", MAX_RANK, 2.0, vec!["Ann"])]
        );
    }

    #[test]
    fn the_report_has_the_website_json_shape() {
        let score = board(
            "SeasonRegionalScore",
            "top-outpost-rush-score.{season}",
            Rotation::Season,
            EntityKind::Character,
        );
        let mut without_end = current(&score, vec![entry(&ANN.to_string(), 3.5, 1)]);
        without_end.rotation_id = Some("s11".to_owned());
        without_end.resets_at = None;
        let mut no_text = board(
            "MonthWarScore",
            "t.{world}.{month}",
            Rotation::Month,
            EntityKind::Company,
        );
        no_text.subgroup = None;
        no_text.value_kind = None;
        let report = snapshot(&[without_end, current(&no_text, Vec::new())], &names());
        assert_eq!(
            serde_json::to_value(report).expect("json"),
            serde_json::json!({
                "boards": [{
                    "boardId": "SeasonRegionalScore",
                    "name": "Score",
                    "group": "War",
                    "rotation": "season",
                    "subgroup": "War",
                    "valueKind": "Score",
                    "lowerIsBetter": false,
                    "regional": true,
                    "entityKind": "character",
                    "rotationId": "s11"
                }, {
                    "boardId": "MonthWarScore",
                    "name": "Score",
                    "group": "War",
                    "rotation": "month",
                    "lowerIsBetter": false,
                    "regional": false,
                    "entityKind": "company",
                    "rotationId": "m41",
                    "resetsAt": "2026-07-01T00:00:00Z"
                }],
                "rows": [{
                    "boardId": "SeasonRegionalScore",
                    "rotation": "season",
                    "rank": 1,
                    "value": 3.5,
                    "names": ["Ann"]
                }]
            })
        );
    }
}
