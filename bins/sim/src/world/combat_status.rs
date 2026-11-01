// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player's combat status (`MB::CombatStatusComponentReplicatedState`, own component 28).
//!
//! Retail (tutorial capture, 9 fights; Observed): about 40 ms after each hit of the player on a
//! mob, the server sends `replicatedCombatLoggedOutTimePoint` = hit + 20 s and
//! `replicatedCombatConcludedTimePoint` = hit + 10 s, both in server ns since 2000-01-01; the first
//! hit of a fight also sends `replicatedInCombatFlag` 1. When every hit of that step killed its
//! mob, the concluded time is hit + 1 s. At the concluded time, with no hit since, it sends the
//! flag 0 alone. A mob's hit on a player in combat changes nothing (83.777 s); a mob's hit on a
//! player out of combat starts it as a player hit does (`newchar-retail-2026-09-18` 94.211 s:
//! the 3601, then 24 ms later the flag 1 with the concluded time the hit + 10.00 s, out of
//! combat since 88.600 s). So a mob that hits a player who does not hit back keeps the
//! out-of-combat regen off. Thorpe's fight set the flag without a player hit (455733), likely
//! the same rule.

use nw_messages::{CombatStatusState, NS_PER_MS};

/// Milliseconds from a hit to the combat logout time point.
const LOGGED_OUT_AFTER_MS: u64 = 20_000;
/// Milliseconds from a hit that did not kill to the combat concluded time point.
const CONCLUDED_AFTER_HIT_MS: u64 = 10_000;
/// Milliseconds from a step whose hits all killed to the combat concluded time point.
const CONCLUDED_AFTER_KILL_MS: u64 = 1_000;

/// The combat timer of one player, in sim milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CombatTimer {
    /// The player is in combat until this time.
    concluded_ms: Option<u64>,
    /// The time of the last hit.
    last_hit_ms: u64,
    /// The time points changed since the last [`CombatTimer::take_change`].
    hit_changed: bool,
    /// The in-combat flag changed since the last [`CombatTimer::take_change`].
    flag_changed: bool,
}

impl CombatTimer {
    /// Whether the player is in combat.
    pub(super) const fn in_combat(&self) -> bool {
        self.concluded_ms.is_some()
    }

    /// The player hit a mob at `now_ms`; `killed` when the hit killed it.
    pub(super) fn hit(&mut self, now_ms: u64, killed: bool) {
        let after_ms = if killed {
            CONCLUDED_AFTER_KILL_MS
        } else {
            CONCLUDED_AFTER_HIT_MS
        };
        let concluded_ms = now_ms.saturating_add(after_ms);
        // Hits of one step: the latest concluded time wins (retail 97575: a kill and a hit, 10 s).
        let concluded_ms = match self.concluded_ms {
            Some(earlier) if self.hit_changed && self.last_hit_ms == now_ms => {
                earlier.max(concluded_ms)
            }
            _ => concluded_ms,
        };
        if self.concluded_ms.is_none() {
            self.flag_changed = true;
        }
        self.concluded_ms = Some(concluded_ms);
        self.last_hit_ms = now_ms;
        self.hit_changed = true;
    }

    /// A mob hit the player at `now_ms`: out of combat it starts combat as a player hit does;
    /// in combat it changes nothing (module notes).
    pub(super) fn hit_taken(&mut self, now_ms: u64) {
        if !self.in_combat() {
            self.hit(now_ms, false);
        }
    }

    /// Ends the combat when its concluded time came at `now_ms`.
    pub(super) fn tick(&mut self, now_ms: u64) {
        if self
            .concluded_ms
            .is_some_and(|concluded| now_ms >= concluded)
        {
            self.concluded_ms = None;
            self.flag_changed = true;
            self.hit_changed = false;
        }
    }

    /// The children that changed since the last call, with the time points at server time
    /// `server_ms` (ms since 2000-01-01) for sim time `now_ms`; `None` when nothing changed.
    pub(super) fn take_change(&mut self, now_ms: u64, server_ms: u64) -> Option<CombatStatusState> {
        let server_ns = |at_ms: u64| {
            server_ms
                .saturating_add(at_ms)
                .saturating_sub(now_ms)
                .saturating_mul(NS_PER_MS)
        };
        let in_combat = self.concluded_ms.is_some();
        let change = CombatStatusState {
            in_combat: self.flag_changed.then_some(in_combat),
            in_pvp_combat: None,
            logged_out_time: (self.hit_changed && in_combat)
                .then(|| server_ns(self.last_hit_ms.saturating_add(LOGGED_OUT_AFTER_MS))),
            concluded_time: self
                .concluded_ms
                .filter(|_| self.hit_changed)
                .map(server_ns),
        };
        self.flag_changed = false;
        self.hit_changed = false;
        (change != CombatStatusState::default()).then_some(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIT_MS: u64 = 78_108;
    /// Server time = sim time + this, for the test.
    const SERVER_OFFSET_MS: u64 = 845_730_000_000;

    fn take(timer: &mut CombatTimer, now_ms: u64) -> Option<CombatStatusState> {
        timer.take_change(now_ms, now_ms + SERVER_OFFSET_MS)
    }

    fn server_ns(at_ms: u64) -> u64 {
        (at_ms + SERVER_OFFSET_MS) * NS_PER_MS
    }

    #[test]
    fn a_first_hit_starts_the_combat_with_both_time_points() {
        let mut timer = CombatTimer::default();
        assert_eq!(take(&mut timer, HIT_MS), None, "no hit, nothing to send");
        timer.hit(HIT_MS, false);
        // Retail 78148: `01 0d 01 <hit + 20 s> <hit + 10 s>`.
        assert_eq!(
            take(&mut timer, HIT_MS + 40),
            Some(CombatStatusState {
                in_combat: Some(true),
                in_pvp_combat: None,
                logged_out_time: Some(server_ns(HIT_MS + LOGGED_OUT_AFTER_MS)),
                concluded_time: Some(server_ns(HIT_MS + CONCLUDED_AFTER_HIT_MS)),
            })
        );
        assert_eq!(take(&mut timer, HIT_MS + 80), None, "sent once");
    }

    #[test]
    fn a_mob_hit_starts_combat_only_out_of_combat() {
        // Retail newchar 94.211 s: out of combat, the mob's hit started it (concluded hit + 10 s).
        let mut timer = CombatTimer::default();
        timer.hit_taken(HIT_MS);
        assert!(timer.in_combat());
        assert_eq!(
            take(&mut timer, HIT_MS + 40).and_then(|state| state.concluded_time),
            Some(server_ns(HIT_MS + CONCLUDED_AFTER_HIT_MS))
        );
        // Retail 83.777 s: in combat, a mob's hit moves nothing.
        timer.hit_taken(HIT_MS + 5_000);
        assert_eq!(take(&mut timer, HIT_MS + 5_040), None);
        timer.tick(HIT_MS + CONCLUDED_AFTER_HIT_MS);
        assert!(!timer.in_combat(), "ends at the first hit's time");
    }

    #[test]
    fn later_hits_move_the_time_points_without_the_flag() {
        let mut timer = CombatTimer::default();
        timer.hit(HIT_MS, false);
        take(&mut timer, HIT_MS);
        let later = HIT_MS + 1_000;
        timer.hit(later, false);
        let change = take(&mut timer, later).expect("change");
        assert_eq!(change.in_combat, None, "retail 79148: `01 0c`");
        assert_eq!(
            change.concluded_time,
            Some(server_ns(later + CONCLUDED_AFTER_HIT_MS))
        );
    }

    #[test]
    fn a_kill_ends_the_combat_a_second_later() {
        let mut timer = CombatTimer::default();
        timer.hit(HIT_MS, false);
        take(&mut timer, HIT_MS);
        let kill = HIT_MS + 3_466;
        timer.hit(kill, true);
        assert_eq!(
            take(&mut timer, kill).and_then(|change| change.concluded_time),
            Some(server_ns(kill + CONCLUDED_AFTER_KILL_MS)),
            "retail 81612: concluded 1 s after the kill"
        );
        timer.tick(kill + CONCLUDED_AFTER_KILL_MS - 1);
        assert_eq!(take(&mut timer, kill + CONCLUDED_AFTER_KILL_MS - 1), None);
        timer.tick(kill + CONCLUDED_AFTER_KILL_MS);
        // Retail 82649: `01 01 00`.
        assert_eq!(
            take(&mut timer, kill + CONCLUDED_AFTER_KILL_MS),
            Some(CombatStatusState {
                in_combat: Some(false),
                ..CombatStatusState::default()
            })
        );
    }

    #[test]
    fn a_kill_and_a_hit_in_one_step_keep_the_longer_time() {
        let mut timer = CombatTimer::default();
        timer.hit(HIT_MS, false);
        timer.hit(HIT_MS, true);
        assert_eq!(
            take(&mut timer, HIT_MS).and_then(|change| change.concluded_time),
            Some(server_ns(HIT_MS + CONCLUDED_AFTER_HIT_MS)),
            "retail 97575"
        );
    }
}
