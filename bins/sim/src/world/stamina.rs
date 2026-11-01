// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's stamina (stamina-wire session note, retail tutorial capture).
//!
//! Only three things take stamina (Observed): a dodge on land (45), a dash in the water (50) and
//! a mob hit on a blocking player (its block stamina). Each starts the regen delay of 1 s: the
//! state's `regen` child counts the seconds left down to 0, then `cur` grows by 33.33 per
//! second to the max. A land dodge starts the delay once more 500 ms after the press (Inferred:
//! the end of the roll), and so does each release of the block key. No regen runs while the
//! block key is held (newchar-retail: `cur` stayed 54.79 from 418195 to 421256 with block held).
//! Raising a block costs nothing (no `cur` drop at the block starts at 410572 and 418156,
//! although `StaminaData` lists `Block` 10). Attacks, sprinting and swimming cost nothing;
//! `winded`, `max` and the rates never change.
//!
//! [`Stamina::tick`] runs once per server tick; [`Stamina::take_change`] gives the children
//! that changed since the last bundle, as retail sends them: `cur` and `regen` at a spend, `regen`
//! alone while the delay runs, `cur` alone while it grows.

use nw_messages::StaminaState;

/// Full stamina of a player (vitals row `Player`, `StaminaBaseMax`).
const MAX_STAMINA: f32 = 100.0;
/// The stamina a dodge on land takes (`StaminaData` `Dodge`; retail 42 of 42 land dodges).
pub(super) const DODGE_STAMINA: f32 = 45.0;
/// The stamina a dash in the water takes (`StaminaData` `Swim_Dash`; retail 3 of 3).
pub(super) const SWIM_DASH_STAMINA: f32 = 50.0;
/// A dodge needs more stamina than this (`playeractions_dodge.actionlist`, action `Dodge`:
/// `IfStaminaValue ComparisonValue="0.0" ComparisonType="GreaterThan"`).
const DODGE_MIN_STAMINA: f32 = 0.0;
/// The regen delay after a spend (vitals row `Player`, `StaminaRegenDelay` 1 s).
const REGEN_DELAY_MS: u64 = 1_000;
/// The regen delay that a blocked hit's first message shows: one tick of the delay is gone
/// (retail `regen` 0.9667 in the first message of all 10 blocked hits).
const BLOCKED_DELAY_MS: u64 = 967;
/// When a land dodge starts the regen delay again, after the press (retail: +501 to +581 ms in
/// 40 of 42 dodges).
const DODGE_RESTART_MS: u64 = 500;
/// Stamina that regen gives per second (retail: 1.111 per 33 ms tick, 1.6665 per 50 ms tick).
const REGEN_PER_SECOND: f32 = 100.0 / 3.0;
/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;

/// What a blocked hit does to the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Guard {
    /// The block holds: stamina is left.
    Held,
    /// The hit took the last stamina: the guard breaks.
    Broken,
}

/// What a dodge press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dodge {
    /// A roll on land.
    Roll,
    /// A dash in the water: no second start of the delay.
    SwimDash,
}

impl Dodge {
    const fn cost(self) -> f32 {
        match self {
            Self::Roll => DODGE_STAMINA,
            Self::SwimDash => SWIM_DASH_STAMINA,
        }
    }
}

/// A player's stamina and what of it the client does not know yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Stamina {
    cur: f32,
    /// What is left of the regen delay.
    delay_ms: u64,
    /// When a land dodge starts the delay again.
    restart_at_ms: Option<u64>,
    /// The time of the last tick.
    last_tick_ms: Option<u64>,
    /// The delay ended and regen has not sent its first step yet.
    regen_starting: bool,
    /// The block key is held: no regen.
    block_held: bool,
    /// The children that changed since the last [`Stamina::take_change`].
    change: StaminaState,
}

impl Default for Stamina {
    fn default() -> Self {
        Self {
            cur: MAX_STAMINA,
            delay_ms: 0,
            restart_at_ms: None,
            last_tick_ms: None,
            regen_starting: false,
            block_held: false,
            change: StaminaState::default(),
        }
    }
}

impl Stamina {
    /// The stamina now.
    pub(super) const fn current(&self) -> f32 {
        self.cur
    }

    /// A dodge press at `now_ms`: while the stamina is above [`DODGE_MIN_STAMINA`] it takes its
    /// stamina, at most all of it, and gives `true`; else it gives `false` and changes nothing.
    /// The action list also refuses a dodge while the `Exhausted` condition holds; when the
    /// client sets it is Unknown, so it is not checked here.
    pub(super) fn dodge(&mut self, dodge: Dodge, now_ms: u64) -> bool {
        if self.cur <= DODGE_MIN_STAMINA {
            return false;
        }
        self.spend(dodge.cost().min(self.cur), REGEN_DELAY_MS);
        self.restart_at_ms =
            (dodge == Dodge::Roll).then(|| now_ms.saturating_add(DODGE_RESTART_MS));
        true
    }

    /// A mob hit on the blocking player takes `loss`, at most all the stamina; the hit that
    /// takes the last of it breaks the guard (newchar-retail 422387: `cur` 24.265 to 0).
    pub(super) fn blocked_hit(&mut self, loss: f32) -> Guard {
        let broken = loss >= self.cur;
        self.spend(loss.min(self.cur), BLOCKED_DELAY_MS);
        if broken { Guard::Broken } else { Guard::Held }
    }

    /// Whether the block key is held (the newest input).
    pub(super) const fn hold_block(&mut self, held: bool) {
        self.block_held = held;
    }

    /// A used consumable gives `amount` (its status effect's `Stamina`), at most to the max.
    pub(super) fn gain(&mut self, amount: f32) {
        let gained = (self.cur + amount).min(MAX_STAMINA);
        if gained > self.cur {
            self.cur = gained;
            self.change.cur = Some(self.cur);
        }
    }

    /// The player let go of the block key: the regen delay starts again.
    pub(super) fn block_released(&mut self) {
        self.delay_ms = REGEN_DELAY_MS;
        self.regen_starting = false;
        self.change.regen = Some(seconds(self.delay_ms));
    }

    fn spend(&mut self, amount: f32, delay_ms: u64) {
        self.cur -= amount;
        self.delay_ms = delay_ms;
        self.regen_starting = false;
        self.change.cur = Some(self.cur);
        self.change.regen = Some(seconds(self.delay_ms));
    }

    /// One server tick at `now_ms`: the second start of a dodge's delay, the delay counting down,
    /// or regen at `regen_scale` times the plain rate (the held effects' `StaminaRate`).
    pub(super) fn tick(&mut self, now_ms: u64, regen_scale: f32) {
        let elapsed_ms = self
            .last_tick_ms
            .map_or(0, |last| now_ms.saturating_sub(last));
        self.last_tick_ms = Some(now_ms);
        if self.restart_at_ms.is_some_and(|at| now_ms >= at) {
            self.restart_at_ms = None;
            self.block_released();
            return;
        }
        if self.delay_ms > 0 {
            self.delay_ms = self.delay_ms.saturating_sub(elapsed_ms);
            self.regen_starting = self.delay_ms == 0;
            self.change.regen = Some(seconds(self.delay_ms));
            return;
        }
        if self.cur < MAX_STAMINA && !self.block_held {
            #[allow(clippy::cast_precision_loss, reason = "a tick is a few ms")]
            let step = REGEN_PER_SECOND * regen_scale * elapsed_ms as f32 / MS_PER_SECOND;
            self.cur = (self.cur + step).min(MAX_STAMINA);
            self.change.cur = Some(self.cur);
            // Retail sends `regen` 0 once more with the first step.
            if std::mem::take(&mut self.regen_starting) {
                self.change.regen = Some(0.0);
            }
        }
    }

    /// The children that changed since the last call; `None` when nothing changed.
    pub(super) fn take_change(&mut self) -> Option<StaminaState> {
        let change = std::mem::take(&mut self.change);
        (change != StaminaState::default()).then_some(change)
    }
}

#[allow(clippy::cast_precision_loss, reason = "the delay is at most a second")]
fn seconds(ms: u64) -> f32 {
    ms as f32 / MS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 10_000;
    const TICK: u64 = 33;
    const PLAIN_REGEN: f32 = 1.0;
    const CLOSE: f32 = 0.01;

    fn change(cur: Option<f32>, regen: Option<f32>) -> Option<StaminaState> {
        Some(StaminaState {
            cur,
            regen,
            ..StaminaState::default()
        })
    }

    /// Ticks from `from` until `to`, one per [`TICK`], and gives the time after the last.
    fn run(stamina: &mut Stamina, from: u64, to: u64) -> u64 {
        let mut now = from;
        while now < to {
            stamina.tick(now, PLAIN_REGEN);
            stamina.take_change();
            now += TICK;
        }
        now
    }

    #[test]
    fn a_land_dodge_takes_45_and_starts_the_delay_twice() {
        let mut stamina = Stamina::default();
        stamina.tick(NOW, PLAIN_REGEN);
        assert!(stamina.dodge(Dodge::Roll, NOW));
        assert_eq!(
            stamina.take_change(),
            change(Some(55.0), Some(1.0)),
            "retail 01 09"
        );
        stamina.tick(NOW + TICK, PLAIN_REGEN);
        assert_eq!(
            stamina.take_change(),
            change(None, Some(0.967)),
            "retail 01 08"
        );
        let now = run(&mut stamina, NOW + 2 * TICK, NOW + DODGE_RESTART_MS);
        stamina.tick(now, PLAIN_REGEN);
        assert_eq!(
            stamina.take_change(),
            change(None, Some(1.0)),
            "the second start"
        );
        let now = run(&mut stamina, now + TICK, now + REGEN_DELAY_MS);
        assert_eq!(stamina.current(), 55.0, "no regen during the delay");
        stamina.tick(now, PLAIN_REGEN);
        assert_eq!(
            stamina.take_change(),
            change(None, Some(0.0)),
            "retail 01 08 0"
        );
        stamina.tick(now + TICK, PLAIN_REGEN);
        let first = stamina.take_change().expect("first step");
        assert_eq!(first.regen, Some(0.0), "retail 01 09 cur 0");
        assert!((first.cur.expect("cur") - 56.1).abs() < CLOSE);
        stamina.tick(now + 2 * TICK, PLAIN_REGEN);
        assert_eq!(stamina.take_change().and_then(|state| state.regen), None);
        run(&mut stamina, now + 3 * TICK, now + 3 * MS_PER_SECOND as u64);
        assert_eq!(stamina.current(), MAX_STAMINA, "full and no higher");
        stamina.tick(now + 4 * MS_PER_SECOND as u64, PLAIN_REGEN);
        assert_eq!(stamina.take_change(), None, "nothing to send when full");
    }

    #[test]
    fn an_exhaust_effect_slows_the_regen_by_its_stamina_rate() {
        const EXHAUSTED: f32 = 0.85;
        let mut plain = Stamina::default();
        let mut exhausted = Stamina::default();
        for stamina in [&mut plain, &mut exhausted] {
            stamina.tick(NOW, PLAIN_REGEN);
            assert!(stamina.dodge(Dodge::Roll, NOW));
        }
        let after_delay = NOW + DODGE_RESTART_MS + REGEN_DELAY_MS + 2 * TICK;
        let end = after_delay + MS_PER_SECOND as u64;
        let mut now = NOW + TICK;
        while now < end {
            plain.tick(now, PLAIN_REGEN);
            exhausted.tick(now, EXHAUSTED);
            now += TICK;
        }
        let gained = |stamina: &Stamina| stamina.current() - (MAX_STAMINA - DODGE_STAMINA);
        assert!(gained(&plain) > 0.0);
        assert!((gained(&exhausted) - gained(&plain) * EXHAUSTED).abs() < CLOSE);
    }

    #[test]
    fn a_swim_dash_takes_50_and_starts_the_delay_once() {
        let mut stamina = Stamina::default();
        stamina.tick(NOW, PLAIN_REGEN);
        assert!(stamina.dodge(Dodge::SwimDash, NOW));
        assert_eq!(stamina.take_change(), change(Some(50.0), Some(1.0)));
        let now = run(&mut stamina, NOW + TICK, NOW + REGEN_DELAY_MS + TICK);
        stamina.tick(now, PLAIN_REGEN);
        assert!(stamina.current() > 50.0, "regen runs after one delay only");
    }

    #[test]
    fn a_dodge_needs_stamina_above_zero_and_takes_at_most_all() {
        let mut stamina = Stamina::default();
        stamina.tick(NOW, PLAIN_REGEN);
        assert!(stamina.dodge(Dodge::Roll, NOW));
        stamina.take_change();
        assert!(stamina.dodge(Dodge::Roll, NOW + TICK), "55 is enough");
        stamina.take_change();
        assert!(
            stamina.dodge(Dodge::Roll, NOW + 2 * TICK),
            "10 is above zero too"
        );
        assert_eq!(stamina.take_change(), change(Some(0.0), Some(1.0)));
        assert!(!stamina.dodge(Dodge::Roll, NOW + 3 * TICK), "0 is not");
        assert_eq!(stamina.take_change(), None);
        assert_eq!(stamina.current(), 0.0);
    }

    #[test]
    fn a_blocked_hit_takes_its_loss_and_a_release_starts_the_delay() {
        let mut stamina = Stamina::default();
        stamina.tick(NOW, PLAIN_REGEN);
        assert_eq!(stamina.blocked_hit(25.55), Guard::Held);
        let hit = stamina.take_change().expect("hit");
        assert!((hit.cur.expect("cur") - 74.45).abs() < CLOSE, "retail");
        assert_eq!(hit.regen, Some(0.967), "retail");
        assert_eq!(
            stamina.blocked_hit(1_000.0),
            Guard::Broken,
            "the last stamina"
        );
        assert_eq!(stamina.current(), 0.0, "never below 0");
        stamina.take_change();
        stamina.block_released();
        assert_eq!(
            stamina.take_change(),
            change(None, Some(1.0)),
            "retail 01 08 3f800000"
        );
    }

    #[test]
    fn no_regen_runs_while_the_block_key_is_held() {
        // Retail newchar: `cur` stayed 54.79 from 418195 to 421256 with block held.
        let mut stamina = Stamina::default();
        stamina.tick(NOW, PLAIN_REGEN);
        stamina.blocked_hit(45.0);
        stamina.hold_block(true);
        let now = run(&mut stamina, NOW + TICK, NOW + 3 * REGEN_DELAY_MS);
        assert_eq!(stamina.current(), 55.0, "held: no regen");
        stamina.hold_block(false);
        stamina.block_released();
        run(&mut stamina, now, now + 2 * REGEN_DELAY_MS);
        assert!(
            stamina.current() > 55.0,
            "regen after the release and its delay"
        );
    }
}
