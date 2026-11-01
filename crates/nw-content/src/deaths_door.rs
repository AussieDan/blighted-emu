// SPDX-License-Identifier: AGPL-3.0-or-later
//! Death's door (status effects note 2026-10-31, death's door study 2026-11-01): a player whose
//! health a hit takes to 0 first goes down for `DeathsDoorTime` (`VitalsBaseData` row `Player`:
//! 120 s), where a hit after `DeathsDoorDelay` (1 s) kills it, a revive brings it back, and the
//! end of the time kills it. Game modes with `DisableDeathsDoor` (outpost rush, capture the flag,
//! the 3v3 arena, battle royale) kill at once, and so does a killer `SkipDeathsDoor` levels
//! above (`LevelDisparityData`: 10 and more).
//!
//! **Observed** (75 community ledgers): 107 of 281 own-player deaths went through it, in quests
//! and catacombs, never in an outpost rush, a capture the flag, a 3v3 or an invasion; at its end
//! the dead `vitalsData` holds the ms spent in it (`NH-MSQ-5.1`: 2430 after a hit, else about
//! 120,033: the full time).

use std::collections::HashSet;

use crate::Tables;

/// The `VitalsBaseData` row of the players.
const PLAYER_VITALS: &str = "Player";
/// A text cell that is on (`SkipDeathsDoor`).
const CELL_TRUE: &str = "TRUE";
/// Milliseconds per second.
const MS_PER_S: f64 = 1_000.0;

/// The death's door rules of the players.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeathsDoorRules {
    /// `DeathsDoorTime`, ms; 0: no death's door.
    pub time_ms: u64,
    /// `DeathsDoorDelay`, ms: how long a downed player takes no hit.
    pub delay_ms: u64,
    /// The game modes with `DisableDeathsDoor`, lowercase.
    disabled_modes: HashSet<String>,
    /// The smallest killer-minus-player level gap with `SkipDeathsDoor`.
    skip_from_gap: Option<i64>,
}

/// Whole milliseconds of `seconds`; negative is 0.
fn ms_of(seconds: f64) -> u64 {
    // A few minutes.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = (seconds.max(0.0) * MS_PER_S).round() as u64;
    ms
}

impl DeathsDoorRules {
    /// The rules of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let player = tables
            .vitals_base_data
            .iter()
            .flat_map(|table| &table.rows)
            .find(|row| row.vitals_id.eq_ignore_ascii_case(PLAYER_VITALS));
        Self {
            time_ms: player.and_then(|row| row.deaths_door_time).map_or(0, ms_of),
            delay_ms: player
                .and_then(|row| row.deaths_door_delay)
                .map_or(0, ms_of),
            disabled_modes: tables
                .game_mode_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter(|row| row.disable_deaths_door == Some(true))
                .map(|row| row.game_mode_id.to_lowercase())
                .collect(),
            skip_from_gap: tables
                .level_disparity_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter(|row| row.skip_deaths_door.eq_ignore_ascii_case(CELL_TRUE))
                .map(|row| row.level_disparity)
                .min(),
        }
    }

    /// Rules of `time_ms` and `delay_ms`, with the `disabled_modes` and the smallest skipping
    /// level gap `skip_from_gap`.
    pub fn new(
        (time_ms, delay_ms): (u64, u64),
        disabled_modes: &[&str],
        skip_from_gap: Option<i64>,
    ) -> Self {
        Self {
            time_ms,
            delay_ms,
            disabled_modes: disabled_modes
                .iter()
                .map(|mode| mode.to_lowercase())
                .collect(),
            skip_from_gap,
        }
    }

    /// Whether a player killed in game mode `mode` (`None`: the open world) by a killer of level
    /// gap `gap` (killer level less the player's; `None` when Unknown) goes down first.
    pub fn takes(&self, mode: Option<&str>, gap: Option<i64>) -> bool {
        self.time_ms > 0
            && mode.is_none_or(|mode| !self.disabled_modes.contains(&mode.to_lowercase()))
            && gap
                .zip(self.skip_from_gap)
                .is_none_or(|(gap, skip)| gap < skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_goes_down_but_not_in_a_disabled_mode_or_to_a_far_higher_killer() {
        let rules = DeathsDoorRules::new((120_000, 1_000), &["OutpostRush"], Some(10));
        assert!(rules.takes(None, Some(3)), "the open world");
        assert!(rules.takes(Some("DungeonAmrine"), None));
        assert!(!rules.takes(Some("outpostrush"), Some(0)), "disabled");
        assert!(!rules.takes(None, Some(10)), "ten levels above");
        assert!(rules.takes(None, Some(9)));
        assert!(!DeathsDoorRules::default().takes(None, None), "no time");
        assert_eq!(ms_of(1.0), 1_000);
    }
}
