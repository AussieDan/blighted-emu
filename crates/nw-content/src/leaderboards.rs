// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaderboards a stat feeds (study note `docs/re/2026-09-18-leaderboards-and-analytics.md`).
//!
//! A `LeaderboardStatData` row names a stat type, a game mode, its scope, aggregation and
//! rotation; the `LeaderboardData` row with the same id gives the board's name template, for
//! example `top-boss-trial-completions.HercyneRaid_Reward.{world}.{month}`. The client fills
//! `{world}` with the world id and the rotation placeholder with the rotation's name (`w178`,
//! `m41`, `s11` in the retail captures), and asks the stats service for that board.

use std::collections::HashMap;

use nw_common::time::{CivilDate, SECONDS_PER_DAY};
use serde::{Deserialize, Serialize};

use crate::Tables;
use crate::generated::{LeaderboardData, LeaderboardStatData};
use crate::loot_limits::HOURS_PER_DAY;

/// The placeholder of the world id in a board template.
pub const WORLD_PLACEHOLDER: &str = "{world}";
/// The rotation placeholders; the client fills each with the same rotation name.
const ROTATION_PLACEHOLDERS: [&str; 3] = ["{week}", "{month}", "{season}"];
/// The start of a placeholder that a board needs and this server does not fill.
const PLACEHOLDER_START: char = '{';
const WEEK_PREFIX: char = 'w';
const MONTH_PREFIX: char = 'm';
const SEASON_PREFIX: char = 's';
const DAYS_PER_WEEK: i64 = 7;
const SECONDS_PER_HOUR: i64 = 3600;
/// The first rotation's number: the client counts rotations from 1.
const FIRST_ROTATION: i64 = 1;

/// How a stat's values join on its board (`Aggregation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregation {
    /// `Sum`: every value adds to the total.
    Sum,
    /// `Max`: the best value stays.
    Max,
    /// `Min`: the lowest value stays (clear times).
    Min,
}

impl Aggregation {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "Sum" => Some(Self::Sum),
            "Max" => Some(Self::Max),
            "Min" => Some(Self::Min),
            _ => None,
        }
    }
}

/// How often a board starts again (`Rotation`). In JSON: `week`, `month` or `season`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rotation {
    /// `Week`.
    Week,
    /// `Month`.
    Month,
    /// `Season`.
    Season,
}

impl Rotation {
    /// The rotation of a datasheet cell (`Week`, `Month`, `Season`).
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "Week" => Some(Self::Week),
            "Month" => Some(Self::Month),
            "Season" => Some(Self::Season),
            _ => None,
        }
    }
}

/// One board a stat feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatBoard {
    /// The stats service namespace (`Namespace`), such as `pc`.
    pub namespace: String,
    /// `Character`, `Company`, `Group` or `Global` (`Scope`).
    pub scope: String,
    /// How the values join.
    pub aggregation: Aggregation,
    /// The board's rotation.
    pub rotation: Rotation,
    /// The board's name template (`LeaderboardDefinitionId`).
    pub template: String,
}

/// The boards of every stat, by stat type and game mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeaderboardStats {
    by_stat: HashMap<(String, String), Vec<StatBoard>>,
}

/// A table key: the stat type and the lowercase game mode (empty for none).
fn stat_key(stat_type: &str, game_mode: &str) -> (String, String) {
    (stat_type.to_owned(), game_mode.to_lowercase())
}

impl LeaderboardStats {
    /// The boards of `LeaderboardStatData` and `LeaderboardData` in `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .leaderboard_stat_data
                .iter()
                .flat_map(|table| &table.rows),
            tables.leaderboard_data.iter().flat_map(|table| &table.rows),
        )
    }

    /// The boards of `stats` whose id has a row in `boards` with an aggregation and a rotation
    /// this server knows.
    pub fn build<'a>(
        stats: impl IntoIterator<Item = &'a LeaderboardStatData>,
        boards: impl IntoIterator<Item = &'a LeaderboardData>,
    ) -> Self {
        let templates: HashMap<&str, &str> = boards
            .into_iter()
            .map(|board| {
                (
                    board.leaderboard_id.as_str(),
                    board.leaderboard_definition_id.as_str(),
                )
            })
            .collect();
        let mut by_stat: HashMap<(String, String), Vec<StatBoard>> = HashMap::new();
        for stat in stats {
            let Some(template) = templates.get(stat.leaderboard_stat_id.as_str()) else {
                continue;
            };
            let (Some(aggregation), Some(rotation)) = (
                Aggregation::parse(&stat.aggregation),
                Rotation::parse(&stat.rotation),
            ) else {
                continue;
            };
            by_stat
                .entry(stat_key(
                    &stat.stat_type,
                    stat.game_mode.as_deref().unwrap_or_default(),
                ))
                .or_default()
                .push(StatBoard {
                    namespace: stat.namespace.clone(),
                    scope: stat.scope.clone(),
                    aggregation,
                    rotation,
                    template: (*template).to_owned(),
                });
        }
        Self { by_stat }
    }

    /// The boards that stat type `stat_type` of game mode `game_mode` (any case) feeds.
    pub fn boards(&self, stat_type: &str, game_mode: &str) -> &[StatBoard] {
        self.by_stat
            .get(&stat_key(stat_type, game_mode))
            .map_or(&[], Vec::as_slice)
    }

    /// The number of stats with a board.
    pub fn len(&self) -> usize {
        self.by_stat.len()
    }

    /// Whether no stat has a board.
    pub fn is_empty(&self) -> bool {
        self.by_stat.is_empty()
    }
}

/// The rotation names of one server, from the keys the client reads
/// (`javelin.leaderboard-rotation-startDate`, `javelin.weekly-leaderboard-reset-hour`,
/// `javelin.leaderboard-season-rotation`). Times are UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationCalendar {
    /// The first week and month start on this day at the reset hour.
    pub start: CivilDate,
    /// The hour of the day when a week starts.
    pub reset_hour: u32,
    /// The season's number.
    pub season: u32,
}

impl RotationCalendar {
    /// The name of the rotation that holds `unix_seconds`: `w<week>`, `m<month>` or
    /// `s<season>`, counted from 1. `None` before the first rotation starts.
    pub fn name(&self, rotation: Rotation, unix_seconds: i64) -> Option<String> {
        let start_seconds =
            self.start.to_days() * SECONDS_PER_DAY + i64::from(self.reset_hour) * SECONDS_PER_HOUR;
        if unix_seconds < start_seconds {
            return None;
        }
        Some(match rotation {
            Rotation::Week => {
                let week = (unix_seconds - start_seconds) / (DAYS_PER_WEEK * SECONDS_PER_DAY);
                format!("{WEEK_PREFIX}{}", week + FIRST_ROTATION)
            }
            Rotation::Month => {
                let now = CivilDate::of_unix_seconds(unix_seconds);
                let month = now.month_number() - self.start.month_number();
                format!("{MONTH_PREFIX}{}", month + FIRST_ROTATION)
            }
            Rotation::Season => format!("{SEASON_PREFIX}{}", self.season),
        })
    }
}

/// The rotation keys of a server config (`[leaderboards]` of `config/sim.toml`): the values the
/// config layers give the client (`javelin.leaderboard-rotation-startDate`,
/// `javelin.weekly-leaderboard-reset-hour`, `javelin.leaderboard-season-rotation`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationSettings {
    /// `javelin.leaderboard-rotation-startDate`, `year-month-day`.
    pub rotation_start_date: String,
    /// `javelin.weekly-leaderboard-reset-hour`, UTC.
    pub weekly_reset_hour: u32,
    /// `javelin.leaderboard-season-rotation`.
    pub season: u32,
}

/// A value of [`RotationSettings`] that is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("config value `{key}` is not valid: {reason}")]
pub struct RotationSettingsError {
    /// The key path, such as `leaderboards.season`.
    pub key: &'static str,
    /// Why the value is refused.
    pub reason: &'static str,
}

impl RotationSettings {
    /// The calendar of these keys.
    ///
    /// # Errors
    ///
    /// Returns [`RotationSettingsError`] for a start date the calendar does not have or a reset
    /// hour past the day.
    pub fn calendar(&self) -> Result<RotationCalendar, RotationSettingsError> {
        let start = CivilDate::parse(&self.rotation_start_date).ok_or(RotationSettingsError {
            key: "leaderboards.rotation_start_date",
            reason: "must be a year-month-day date",
        })?;
        if self.weekly_reset_hour >= HOURS_PER_DAY {
            return Err(RotationSettingsError {
                key: "leaderboards.weekly_reset_hour",
                reason: "must be below 24",
            });
        }
        Ok(RotationCalendar {
            start,
            reset_hour: self.weekly_reset_hour,
            season: self.season,
        })
    }
}

/// The board name of `template` in world `world` and rotation `rotation_name`; `None` when the
/// template has another placeholder.
pub fn board_name(template: &str, world: &str, rotation_name: &str) -> Option<String> {
    let mut name = template.replace(WORLD_PLACEHOLDER, world);
    for placeholder in ROTATION_PLACEHOLDERS {
        name = name.replace(placeholder, rotation_name);
    }
    (!name.contains(PLACEHOLDER_START)).then_some(name)
}

/// Rows for tests in this and other crates (`test-util` feature).
#[cfg(any(test, feature = "test-util"))]
pub mod test_rows {
    use crate::generated::{LeaderboardData, LeaderboardStatData};

    /// The namespace of every retail row.
    const NAMESPACE: &str = "pc";

    /// A `LeaderboardStatData` row with these values; the others are empty.
    pub fn stat_row(
        id: &str,
        stat_type: &str,
        game_mode: &str,
        aggregation: &str,
        rotation: &str,
        scope: &str,
    ) -> LeaderboardStatData {
        LeaderboardStatData {
            aggregation: aggregation.to_owned(),
            categorical_progression_id: None,
            game_mode: Some(game_mode.to_owned()),
            leaderboard_stat_id: id.to_owned(),
            namespace: NAMESPACE.to_owned(),
            notes: None,
            personal_best: None,
            rotation: rotation.to_owned(),
            scope: scope.to_owned(),
            service_stat_id: String::new(),
            stat_type: stat_type.to_owned(),
        }
    }

    /// A `LeaderboardData` row with this id and template; the others are empty.
    pub fn board_row(id: &str, template: &str) -> LeaderboardData {
        LeaderboardData {
            category: String::new(),
            category_additional_header: None,
            category_description: None,
            category_priority: None,
            character_leaderboard: true,
            company_leaderboard: false,
            data_sheet_category: String::new(),
            data_sheet_lb_scope: String::new(),
            display_name: String::new(),
            display_name_priority: None,
            entitlement_rewards: None,
            faction: None,
            faction_leaderboard: false,
            faction_leaderboard_definition_id: None,
            first_level_category: String::new(),
            game_mode: None,
            group_leaderboard: false,
            is_enabled: true,
            item_rewards: None,
            leaderboard_definition_id: template.to_owned(),
            leaderboard_id: id.to_owned(),
            name_string: None,
            namespace: NAMESPACE.to_owned(),
            reward_type: None,
            rewards: None,
            rotation: String::new(),
            second_level_category: String::new(),
            second_level_category_priority: None,
            value_string: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_rows::{board_row, stat_row};
    use super::*;

    const HERCYNE: &str = "HercyneRaid_Reward";
    const MONTH_TEMPLATE: &str = "top-boss-trial-completions.HercyneRaid_Reward.{world}.{month}";
    const WORLD: &str = "8d0f4c2e-0000-4000-8000-000000000001";
    /// The retail start date and the iad-prod reset hour.
    const CALENDAR: RotationCalendar = RotationCalendar {
        start: CivilDate {
            year: 2023,
            month: 2,
            day: 7,
        },
        reset_hour: 2,
        season: 11,
    };
    /// 2026-06-30 09:53:59 UTC, the time of the retail `Hive leaderboards` capture: it asked
    /// for `w178`, `m41` and `s11`.
    const CAPTURE_SECONDS: i64 = 1_782_813_239;
    /// 2023-02-07 01:59:59 and 02:00:00 UTC.
    const BEFORE_START: i64 = 1_675_735_199;
    const AT_START: i64 = 1_675_735_200;
    const WEEK_SECONDS: i64 = 604_800;

    #[test]
    fn a_stat_feeds_the_board_of_its_id() {
        let stat = |id, stat_type, mode, aggregation| {
            stat_row(id, stat_type, mode, aggregation, "Month", "Character")
        };
        let stats = [
            stat("MonthRaidCompletionsHercyne", "GameEvent", HERCYNE, "Sum"),
            stat(
                "MonthDarkness",
                "DarknessBreachesCompleted",
                "Darkness",
                "Sum",
            ),
            stat("Odd", "GameEvent", "Other", "Average"),
        ];
        let boards = [
            board_row("MonthRaidCompletionsHercyne", MONTH_TEMPLATE),
            board_row("Odd", "top-odd.{world}.{month}"),
        ];
        let table = LeaderboardStats::build(&stats, &boards);
        assert_eq!(
            table.len(),
            1,
            "no board, or an unknown aggregation, feeds nothing"
        );
        assert_eq!(
            table.boards("GameEvent", "hercyneraid_reward"),
            [StatBoard {
                namespace: "pc".to_owned(),
                scope: "Character".to_owned(),
                aggregation: Aggregation::Sum,
                rotation: Rotation::Month,
                template: MONTH_TEMPLATE.to_owned(),
            }]
        );
        assert!(
            table
                .boards("DarknessBreachesCompleted", "Darkness")
                .is_empty()
        );
    }

    #[test]
    fn rotation_names_match_the_retail_capture() {
        assert_eq!(
            CALENDAR.name(Rotation::Week, CAPTURE_SECONDS).as_deref(),
            Some("w178")
        );
        assert_eq!(
            CALENDAR.name(Rotation::Month, CAPTURE_SECONDS).as_deref(),
            Some("m41")
        );
        assert_eq!(
            CALENDAR.name(Rotation::Season, CAPTURE_SECONDS).as_deref(),
            Some("s11")
        );
    }

    #[test]
    fn rotations_start_at_the_reset_hour() {
        assert_eq!(CALENDAR.name(Rotation::Week, BEFORE_START), None);
        assert_eq!(
            CALENDAR.name(Rotation::Week, AT_START).as_deref(),
            Some("w1")
        );
        assert_eq!(
            CALENDAR
                .name(Rotation::Week, AT_START + WEEK_SECONDS - 1)
                .as_deref(),
            Some("w1")
        );
        assert_eq!(
            CALENDAR
                .name(Rotation::Week, AT_START + WEEK_SECONDS)
                .as_deref(),
            Some("w2")
        );
        assert_eq!(
            CALENDAR.name(Rotation::Month, AT_START).as_deref(),
            Some("m1")
        );
    }

    #[test]
    fn board_names_fill_the_world_and_the_rotation() {
        assert_eq!(
            board_name(MONTH_TEMPLATE, WORLD, "m41").as_deref(),
            Some(
                "top-boss-trial-completions.HercyneRaid_Reward.8d0f4c2e-0000-4000-8000-000000000001.m41"
            )
        );
        assert_eq!(
            board_name("top-outpost-rush-wins.{season}", WORLD, "s11").as_deref(),
            Some("top-outpost-rush-wins.s11")
        );
        assert_eq!(
            board_name("top-x.{territory}.{world}.{week}", WORLD, "w1"),
            None
        );
    }
}
