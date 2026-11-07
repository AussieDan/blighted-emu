// SPDX-License-Identifier: AGPL-3.0-or-later
//! Movement checks (M4 spec section 7.2, parent spec section 9 item 1). The server is the judge
//! of positions: a refused move keeps the last good position, and the caller sends it back.
//!
//! Speed is checked with a distance budget, not move by move: the budget grows at the speed
//! limit, up to `burst_seconds` of movement, and each move spends its horizontal distance. The
//! average speed stays under the limit, and two honest moves that arrive close together after a
//! network delay are not refused.
//!
//! A climb spends the budget like a horizontal move; a fall is free.
//!
//! The height grid holds the terrain only, not buildings, stairs or bridges. So a position may be
//! at most `ground_tolerance_m` below the ground and at most `max_height_above_ground_m` above it.
//! Where the grid has no height, a fall in one move may be at most `max_height_above_ground_m`.
//!
//! Times are the server's own monotonic receive times in milliseconds, never a time that the
//! client sends: a client time would let a client make its own budget.

use nw_world::WorldFile;
use thiserror::Error;

const MILLIS_PER_SECOND: f32 = 1000.0;

/// A world position in metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
    /// Up.
    pub z: f32,
}

impl Position {
    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// Distance in the ground plane.
    pub fn horizontal_distance(self, other: Self) -> f32 {
        let (east, north) = (self.x - other.x, self.y - other.y);
        // Not `hypot`: its overflow care is not needed in a level and it is several times
        // slower (live 2026-11-01: 5 % of the main thread of a sim of 366 players).
        (east * east + north * north).sqrt()
    }
}

/// Ground height of a level.
pub trait Ground {
    /// Ground height at (`x`, `y`), or `None` where the level has no height data.
    fn height_at(&self, x: f32, y: f32) -> Option<f32>;

    /// The height of the floor at (`x`, `y`) under a body whose feet are at height `_z`: the
    /// ground's, unless the implementation knows other floors there (the level's structures:
    /// [`crate::LevelGround`]).
    fn floor_at(&self, x: f32, y: f32, _z: f32) -> Option<f32> {
        self.height_at(x, y)
    }

    /// Whether a solid stands on the straight line from `_from` to `_to`: a wall between them.
    /// No wall where the implementation knows only the terrain.
    fn blocked(&self, _from: Position, _to: Position) -> bool {
        false
    }

    /// The first wall that the straight line from `_from` to `_to` enters from outside: the
    /// outward unit normal in the ground plane of the first solid surface on it that is too
    /// steep to stand on. A line that starts inside a solid leaves it freely. No wall where the
    /// implementation knows only the terrain.
    fn wall(&self, _from: Position, _to: Position) -> Option<[f32; 2]> {
        None
    }

    /// How much of the ground at (`_x`, `_y`) is road, 0 to 1: the share of the road cells of the
    /// level's tract map in the bilinear blend around the point ([`nw_world::roads::RoadMap`]).
    /// No road where the implementation knows no tract map.
    fn road_share(&self, _x: f32, _y: f32) -> f32 {
        0.0
    }

    /// The water surface at (`_x`, `_y`) when water is there, m: the level's water map's
    /// ([`nw_world::water::WaterMap`]: the sea, lakes, rivers and ponds), or the sea at
    /// `sea_level_m` where the implementation knows no water map.
    fn water_surface(&self, _x: f32, _y: f32, sea_level_m: f32) -> Option<f32> {
        Some(sea_level_m)
    }

    /// The flags of the water at (`_x`, `_y`) when water is there: the kind bits of the level's
    /// water map ([`nw_world::water::WaterMap::flags_at`]: sea `0x0a`, lake or river `0x07`,
    /// pond `0x04`). `None` where the implementation knows no water map.
    fn water_flags(&self, _x: f32, _y: f32) -> Option<u32> {
        None
    }
}

/// The ground a reference names.
impl<T: Ground + ?Sized> Ground for &T {
    fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        (**self).height_at(x, y)
    }

    fn floor_at(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        (**self).floor_at(x, y, z)
    }

    fn blocked(&self, from: Position, to: Position) -> bool {
        (**self).blocked(from, to)
    }

    fn wall(&self, from: Position, to: Position) -> Option<[f32; 2]> {
        (**self).wall(from, to)
    }

    fn road_share(&self, x: f32, y: f32) -> f32 {
        (**self).road_share(x, y)
    }

    fn water_surface(&self, x: f32, y: f32, sea_level_m: f32) -> Option<f32> {
        (**self).water_surface(x, y, sea_level_m)
    }

    fn water_flags(&self, x: f32, y: f32) -> Option<u32> {
        (**self).water_flags(x, y)
    }
}

/// The ground a shared value holds.
impl<T: Ground + ?Sized> Ground for std::sync::Arc<T> {
    fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        (**self).height_at(x, y)
    }

    fn floor_at(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        (**self).floor_at(x, y, z)
    }

    fn blocked(&self, from: Position, to: Position) -> bool {
        (**self).blocked(from, to)
    }

    fn wall(&self, from: Position, to: Position) -> Option<[f32; 2]> {
        (**self).wall(from, to)
    }

    fn road_share(&self, x: f32, y: f32) -> f32 {
        (**self).road_share(x, y)
    }

    fn water_surface(&self, x: f32, y: f32, sea_level_m: f32) -> Option<f32> {
        (**self).water_surface(x, y, sea_level_m)
    }

    fn water_flags(&self, x: f32, y: f32) -> Option<u32> {
        (**self).water_flags(x, y)
    }
}

impl Ground for WorldFile {
    fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        Self::height_at(self, x, y)
    }
}

/// The ground area of one cell; a position outside it is refused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellBounds {
    /// Smallest x.
    pub min_x: f32,
    /// Smallest y.
    pub min_y: f32,
    /// Largest x.
    pub max_x: f32,
    /// Largest y.
    pub max_y: f32,
}

impl CellBounds {
    fn contains(&self, position: Position) -> bool {
        (self.min_x..=self.max_x).contains(&position.x)
            && (self.min_y..=self.max_y).contains(&position.y)
    }
}

/// Movement limits from the `sim` configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveLimits {
    /// Highest average speed: the run speed plus the margin.
    pub max_speed_mps: f32,
    /// Seconds of movement the distance budget can hold. One move is never longer than
    /// `max_speed_mps * burst_seconds`, so this must be longer than the longest gap between two
    /// movement messages of an honest client.
    pub burst_seconds: f32,
    /// How far a position may be below the ground.
    pub ground_tolerance_m: f32,
    /// How far a position may be above the ground.
    pub max_height_above_ground_m: f32,
}

/// A movement limit that is not a usable number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("movement limit `{name}` must be a finite number {rule}")]
pub struct MoveLimitsError {
    /// The limit.
    pub name: &'static str,
    /// What it must be.
    pub rule: &'static str,
}

const ABOVE_ZERO: &str = "above 0";
const NOT_NEGATIVE: &str = "of at least 0";

impl MoveLimits {
    /// Limits with `max_speed_mps = run_speed_mps * (1 + speed_margin)`.
    ///
    /// # Errors
    ///
    /// Returns [`MoveLimitsError`] for a value that is not finite, a speed or burst of 0 or less,
    /// or a margin or height below 0. A NaN limit would accept every move.
    pub fn new(
        run_speed_mps: f32,
        speed_margin: f32,
        burst_seconds: f32,
        ground_tolerance_m: f32,
        max_height_above_ground_m: f32,
    ) -> Result<Self, MoveLimitsError> {
        let positive = [
            ("run_speed_mps", run_speed_mps),
            ("burst_seconds", burst_seconds),
        ];
        let not_negative = [
            ("speed_margin", speed_margin),
            ("ground_tolerance_m", ground_tolerance_m),
            ("max_height_above_ground_m", max_height_above_ground_m),
        ];
        let bad = positive
            .into_iter()
            .find(|(_, value)| !(value.is_finite() && *value > 0.0))
            .map(|(name, _)| MoveLimitsError {
                name,
                rule: ABOVE_ZERO,
            })
            .or_else(|| {
                not_negative
                    .into_iter()
                    .find(|(_, value)| !(value.is_finite() && *value >= 0.0))
                    .map(|(name, _)| MoveLimitsError {
                        name,
                        rule: NOT_NEGATIVE,
                    })
            });
        if let Some(error) = bad {
            return Err(error);
        }
        Ok(Self {
            max_speed_mps: run_speed_mps * (1.0 + speed_margin),
            burst_seconds,
            ground_tolerance_m,
            max_height_above_ground_m,
        })
    }

    fn max_budget_m(&self) -> f32 {
        self.max_speed_mps * self.burst_seconds
    }

    /// Whether the height `z` is at most `ground_tolerance_m` below the ground `height` and at
    /// most `max_height_above_ground_m` above it. A height that is not finite is not.
    fn near_ground(&self, height: f32, z: f32) -> bool {
        (height - self.ground_tolerance_m..=height + self.max_height_above_ground_m).contains(&z)
    }

    /// `position` standing where an arriving player starts on `ground`, with the sea at
    /// `sea_level_m`: on the floor under it (the terrain, or a structure's floor), or at the
    /// swim height where water is over that floor; from the terrain when it is under the
    /// terrain or too far above it. Unchanged where the level has no height data.
    ///
    /// Observed (2026-11-07): retail's first own position at a login is 0.00 to 0.08 m over the
    /// floor (`general-other-01` at 501 ms, a store-horse capture at 4302 ms on a structure 9.7 m
    /// over the terrain), while the saved spot is the last client position, in the air in 930 of
    /// 75,478 on-foot samples of `general-other-01` (up to 8.7 m): live, a login dropped the
    /// player a few feet.
    pub fn onto_ground(
        &self,
        ground: &(impl Ground + ?Sized),
        position: Position,
        sea_level_m: f32,
    ) -> Position {
        let Some(terrain) = ground.height_at(position.x, position.y) else {
            return position;
        };
        let from = if self.near_ground(terrain, position.z) {
            position.z
        } else {
            terrain
        };
        let floor = ground
            .floor_at(position.x, position.y, from)
            .unwrap_or(terrain);
        let z = crate::character::swim_floor_at(ground, position.x, position.y, sea_level_m)
            .map_or(floor, |swim| swim.max(floor));
        Position { z, ..position }
    }
}

/// Why a move was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRefusal {
    /// A coordinate is not a finite number.
    NotFinite,
    /// The position is outside the cell.
    OutOfBounds,
    /// The move time is before the last good move.
    TimeWentBack,
    /// The move is longer than the distance budget.
    TooFast,
    /// The position is under the ground or too far above it, or, without height data, falls too
    /// far in one move.
    OffGround,
}

impl MoveRefusal {
    /// Metric label.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NotFinite => "not_finite",
            Self::OutOfBounds => "out_of_bounds",
            Self::TimeWentBack => "time_went_back",
            Self::TooFast => "too_fast",
            Self::OffGround => "off_ground",
        }
    }
}

/// Whether the ground check ran for an accepted move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundCheck {
    /// The level has height data at the position, and the check passed.
    Checked,
    /// The level has no height data at the position; the check was skipped.
    NoGround,
}

/// The movement state that crosses a handoff (M5 spec section 7.4). Each `sim` has its own
/// monotonic clock, so the time of the last good move travels as an age.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionState {
    /// The last good position.
    pub position: Position,
    /// Milliseconds since the last good move.
    pub move_age_ms: u64,
    /// The distance budget left, in metres.
    pub budget_m: f32,
}

/// The checked movement of one entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    position: Position,
    time_ms: u64,
    budget_m: f32,
    /// The ground-plane velocity of the last good move, m/s.
    velocity: [f32; 2],
}

impl Motion {
    /// An entity at `position` at server time `received_ms`, with a full distance budget.
    pub fn new(position: Position, received_ms: u64, limits: &MoveLimits) -> Self {
        Self {
            position,
            time_ms: received_ms,
            budget_m: limits.max_budget_m(),
            velocity: [0.0; 2],
        }
    }

    /// The last good position.
    pub const fn position(&self) -> Position {
        self.position
    }

    /// Server time of the last good move (or of the start).
    pub const fn moved_ms(&self) -> u64 {
        self.time_ms
    }

    /// The ground-plane velocity of the last good move (m/s); still at the start and after an
    /// import.
    pub const fn velocity(&self) -> [f32; 2] {
        self.velocity
    }

    /// The state at server time `now_ms`, for another `sim`.
    pub const fn export(&self, now_ms: u64) -> MotionState {
        MotionState {
            position: self.position,
            move_age_ms: now_ms.saturating_sub(self.time_ms),
            budget_m: self.budget_m,
        }
    }

    /// The motion of `state` on this `sim`'s clock at `now_ms`. A budget above the limit is
    /// lowered to it, and a budget that is not a number is 0, so no state can buy a longer move.
    pub fn import(state: MotionState, now_ms: u64, limits: &MoveLimits) -> Self {
        let budget_m = if state.budget_m.is_finite() {
            state.budget_m.clamp(0.0, limits.max_budget_m())
        } else {
            0.0
        };
        Self {
            position: state.position,
            time_ms: now_ms.saturating_sub(state.move_age_ms),
            budget_m,
            velocity: [0.0; 2],
        }
    }

    /// Checks a move to `next` that the server received at `received_ms` (its own monotonic clock,
    /// never the client's). An accepted move becomes the last good position; a refused move
    /// changes nothing.
    ///
    /// # Errors
    ///
    /// Returns the [`MoveRefusal`] of the first check that fails.
    pub fn try_move(
        &mut self,
        limits: &MoveLimits,
        ground: &(impl Ground + ?Sized),
        bounds: &CellBounds,
        next: Position,
        received_ms: u64,
    ) -> Result<GroundCheck, MoveRefusal> {
        if !next.is_finite() {
            return Err(MoveRefusal::NotFinite);
        }
        if !bounds.contains(next) {
            return Err(MoveRefusal::OutOfBounds);
        }
        let Some(elapsed_ms) = received_ms.checked_sub(self.time_ms) else {
            return Err(MoveRefusal::TimeWentBack);
        };
        let earned = limits.max_speed_mps * elapsed_ms as f32 / MILLIS_PER_SECOND;
        let budget = (self.budget_m + earned).min(limits.max_budget_m());
        let climb = (next.z - self.position.z).max(0.0);
        let cost = self.position.horizontal_distance(next) + climb;
        if cost > budget {
            return Err(MoveRefusal::TooFast);
        }
        let check = match ground.height_at(next.x, next.y) {
            Some(height) if !limits.near_ground(height, next.z) => {
                return Err(MoveRefusal::OffGround);
            }
            Some(_) => GroundCheck::Checked,
            None if self.position.z - next.z > limits.max_height_above_ground_m => {
                return Err(MoveRefusal::OffGround);
            }
            None => GroundCheck::NoGround,
        };
        let velocity = if elapsed_ms == 0 {
            self.velocity
        } else {
            let seconds = elapsed_ms as f32 / MILLIS_PER_SECOND;
            [
                (next.x - self.position.x) / seconds,
                (next.y - self.position.y) / seconds,
            ]
        };
        *self = Self {
            position: next,
            time_ms: received_ms,
            budget_m: budget - cost,
            velocity,
        };
        Ok(check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_SPEED: f32 = 6.0;
    const MARGIN: f32 = 0.2;
    const BURST: f32 = 1.0;
    const BELOW: f32 = 2.0;
    const ABOVE: f32 = 64.0;
    const GROUND_HEIGHT: f32 = 100.0;
    const CELL_SIZE: f32 = 2048.0;
    const STEP_MS: u64 = 1000;
    const START_MS: u64 = 10_000;
    const TOLERANCE: f32 = 1e-5;

    /// Flat ground at [`GROUND_HEIGHT`] for x below 1000; no data elsewhere.
    struct Flat;

    impl Ground for Flat {
        fn height_at(&self, x: f32, _: f32) -> Option<f32> {
            (x < 1000.0).then_some(GROUND_HEIGHT)
        }
    }

    fn limits() -> MoveLimits {
        MoveLimits::new(RUN_SPEED, MARGIN, BURST, BELOW, ABOVE).expect("limits")
    }

    fn bounds() -> CellBounds {
        CellBounds {
            min_x: 0.0,
            min_y: 0.0,
            max_x: CELL_SIZE,
            max_y: CELL_SIZE,
        }
    }

    fn at(x: f32, z: f32) -> Position {
        Position { x, y: 10.0, z }
    }

    fn start() -> Motion {
        Motion::new(at(10.0, GROUND_HEIGHT), START_MS, &limits())
    }

    /// A clock of another `sim`, far from [`START_MS`].
    const OTHER_CLOCK_MS: u64 = 50_000;
    const EXPORT_DELAY_MS: u64 = 100;
    const HUGE_BUDGET_M: f32 = 1000.0;

    #[test]
    fn a_motion_continues_after_export_and_import() {
        let mut motion = start();
        let half = START_MS + STEP_MS / 2;
        let first = at(13.0, GROUND_HEIGHT);
        motion
            .try_move(&limits(), &Flat, &bounds(), first, half)
            .expect("first");
        let state = motion.export(half + EXPORT_DELAY_MS);
        assert_eq!(state.position, first);
        assert_eq!(state.move_age_ms, EXPORT_DELAY_MS);
        // Left: 7.2 - 3 m; earned since: 0.72 m per 100 ms.
        let reach = 13.0 + (7.2 - 3.0) + 0.72;
        let fresh = || Motion::import(state, OTHER_CLOCK_MS, &limits());
        let inside = at(reach - 0.05, GROUND_HEIGHT);
        assert_eq!(
            fresh().try_move(&limits(), &Flat, &bounds(), inside, OTHER_CLOCK_MS),
            Ok(GroundCheck::Checked)
        );
        let outside = at(reach + 0.05, GROUND_HEIGHT);
        assert_eq!(
            fresh().try_move(&limits(), &Flat, &bounds(), outside, OTHER_CLOCK_MS),
            Err(MoveRefusal::TooFast)
        );
    }

    #[test]
    fn an_imported_budget_is_clamped_and_an_old_age_saturates() {
        let state = MotionState {
            position: at(10.0, GROUND_HEIGHT),
            move_age_ms: u64::MAX,
            budget_m: HUGE_BUDGET_M,
        };
        let motion = Motion::import(state, OTHER_CLOCK_MS, &limits());
        assert_eq!(
            motion.export(OTHER_CLOCK_MS).budget_m,
            limits().max_speed_mps * BURST
        );
        assert_eq!(motion.export(OTHER_CLOCK_MS).move_age_ms, OTHER_CLOCK_MS);
        let broken = MotionState {
            budget_m: f32::NAN,
            ..state
        };
        assert_eq!(
            Motion::import(broken, OTHER_CLOCK_MS, &limits())
                .export(OTHER_CLOCK_MS)
                .budget_m,
            0.0
        );
    }

    #[test]
    fn the_limit_is_run_speed_plus_the_margin() {
        assert!((limits().max_speed_mps - 7.2).abs() < TOLERANCE);
    }

    #[test]
    fn a_run_is_accepted_and_moves_the_entity() {
        let mut motion = start();
        let next = at(16.0, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), next, START_MS + STEP_MS),
            Ok(GroundCheck::Checked)
        );
        assert_eq!(motion.position(), next);
    }

    #[test]
    fn a_good_move_sets_the_velocity_and_a_refused_one_keeps_it() {
        let mut motion = start();
        assert_eq!(motion.velocity(), [0.0; 2], "still at the start");
        motion
            .try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(16.0, GROUND_HEIGHT),
                START_MS + STEP_MS,
            )
            .expect("run");
        // 6 m east in one second.
        assert_eq!(motion.velocity(), [RUN_SPEED, 0.0]);
        let too_far = at(100.0, GROUND_HEIGHT);
        assert!(
            motion
                .try_move(&limits(), &Flat, &bounds(), too_far, START_MS + 2 * STEP_MS)
                .is_err()
        );
        assert_eq!(motion.velocity(), [RUN_SPEED, 0.0]);
        motion
            .try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(16.0, GROUND_HEIGHT),
                START_MS + 3 * STEP_MS,
            )
            .expect("stand");
        assert_eq!(motion.velocity(), [0.0; 2], "standing still");
    }

    #[test]
    fn a_move_beyond_the_budget_is_refused_and_keeps_the_last_good_position() {
        let mut motion = start();
        let far = at(30.0, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), far, START_MS + STEP_MS),
            Err(MoveRefusal::TooFast)
        );
        assert_eq!(motion.position(), at(10.0, GROUND_HEIGHT));
    }

    #[test]
    fn moves_that_arrive_close_together_use_the_saved_budget() {
        let mut motion = start();
        let half = STEP_MS / 2;
        let first = at(13.0, GROUND_HEIGHT);
        motion
            .try_move(&limits(), &Flat, &bounds(), first, START_MS + half)
            .expect("first half");
        let second = at(16.0, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), second, START_MS + half + 1),
            Ok(GroundCheck::Checked)
        );
        let third = at(30.0, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), third, START_MS + half + 2),
            Err(MoveRefusal::TooFast)
        );
    }

    #[test]
    fn under_the_ground_and_high_above_it_are_refused() {
        let time = START_MS + STEP_MS;
        let mut low = start();
        assert_eq!(
            low.try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(11.0, GROUND_HEIGHT - BELOW - 0.5),
                time
            ),
            Err(MoveRefusal::OffGround)
        );
        let roof = GROUND_HEIGHT + ABOVE - 1.0;
        let mut high = Motion::new(at(10.0, roof), START_MS, &limits());
        assert_eq!(
            high.try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(11.0, GROUND_HEIGHT + ABOVE + 0.5),
                time
            ),
            Err(MoveRefusal::OffGround)
        );
        assert_eq!(
            high.try_move(&limits(), &Flat, &bounds(), at(11.0, roof), time),
            Ok(GroundCheck::Checked)
        );
    }

    /// Flat ground with a roof over part of it, and the sea over the ground east of it.
    struct Roofed;

    /// The roof's height over [`GROUND_HEIGHT`], and where the roof and the sea start.
    const ROOF_M: f32 = 9.7;
    const ROOF_FROM_X: f32 = 20.0;
    const SEA_FROM_X: f32 = 40.0;
    /// The sea's level over [`GROUND_HEIGHT`].
    const SEA_DEPTH_M: f32 = 5.0;

    impl Ground for Roofed {
        fn height_at(&self, x: f32, y: f32) -> Option<f32> {
            Flat.height_at(x, y)
        }

        fn floor_at(&self, x: f32, y: f32, z: f32) -> Option<f32> {
            let roof = GROUND_HEIGHT + ROOF_M;
            if (ROOF_FROM_X..SEA_FROM_X).contains(&x) && z >= roof {
                Some(roof)
            } else {
                self.height_at(x, y)
            }
        }

        fn water_surface(&self, x: f32, _y: f32, sea_level_m: f32) -> Option<f32> {
            (x >= SEA_FROM_X).then_some(sea_level_m)
        }
    }

    #[test]
    fn an_arrival_stands_on_the_floor_under_it() {
        let limits = limits();
        let sea = GROUND_HEIGHT + SEA_DEPTH_M;
        let fallen = at(10.0, GROUND_HEIGHT - BELOW - 0.5);
        assert_eq!(
            limits.onto_ground(&Roofed, fallen, sea),
            at(10.0, GROUND_HEIGHT)
        );
        let far_up = at(10.0, GROUND_HEIGHT + ABOVE + 0.5);
        assert_eq!(
            limits.onto_ground(&Roofed, far_up, sea),
            at(10.0, GROUND_HEIGHT)
        );
        let not_finite = at(10.0, f32::NAN);
        assert_eq!(
            limits.onto_ground(&Roofed, not_finite, sea),
            at(10.0, GROUND_HEIGHT)
        );
        let jumped = at(10.0, GROUND_HEIGHT + 1.0);
        assert_eq!(
            limits.onto_ground(&Roofed, jumped, sea),
            at(10.0, GROUND_HEIGHT),
            "a save during a jump lands"
        );
        let over_the_roof = at(ROOF_FROM_X + 1.0, GROUND_HEIGHT + ROOF_M + 1.0);
        assert_eq!(
            limits.onto_ground(&Roofed, over_the_roof, sea),
            at(ROOF_FROM_X + 1.0, GROUND_HEIGHT + ROOF_M),
            "a roof or bridge holds it"
        );
        let swimming = at(SEA_FROM_X + 1.0, sea);
        assert_eq!(
            limits.onto_ground(&Roofed, swimming, sea),
            at(SEA_FROM_X + 1.0, crate::character::swim_floor(sea)),
            "in the sea it floats"
        );
        let no_data = at(1000.0, GROUND_HEIGHT + ABOVE * 2.0);
        assert_eq!(limits.onto_ground(&Roofed, no_data, sea), no_data);
    }

    #[test]
    fn no_height_data_skips_the_ground_check() {
        let mut motion = Motion::new(at(1000.0, 0.0), START_MS, &limits());
        assert_eq!(
            motion.try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(1001.0, 3.0),
                START_MS + STEP_MS
            ),
            Ok(GroundCheck::NoGround)
        );
    }

    #[test]
    fn climbing_spends_the_budget_and_falling_does_not() {
        let mut motion = start();
        let time = START_MS + STEP_MS;
        let cliff = at(11.0, GROUND_HEIGHT + 10.0);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), cliff, time),
            Err(MoveRefusal::TooFast)
        );
        let mut roof = Motion::new(at(10.0, GROUND_HEIGHT + 50.0), START_MS, &limits());
        let jump_down = at(16.0, GROUND_HEIGHT);
        assert_eq!(
            roof.try_move(&limits(), &Flat, &bounds(), jump_down, time),
            Ok(GroundCheck::Checked)
        );
    }

    #[test]
    fn without_height_data_heights_stay_near_the_last_good_one() {
        let time = START_MS + STEP_MS;
        let mut motion = Motion::new(at(1000.0, 0.0), START_MS, &limits());
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), at(1000.0, 500.0), time),
            Err(MoveRefusal::TooFast)
        );
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), at(1000.0, -ABOVE - 1.0), time),
            Err(MoveRefusal::OffGround)
        );
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), at(1000.0, -ABOVE + 1.0), time),
            Ok(GroundCheck::NoGround)
        );
    }

    #[test]
    fn the_budget_holds_at_most_one_burst() {
        let mut motion = start();
        let idle = START_MS + 10 * STEP_MS;
        let longer_than_a_burst = at(10.0 + limits().max_speed_mps * BURST + 0.5, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), longer_than_a_burst, idle),
            Err(MoveRefusal::TooFast)
        );
    }

    #[test]
    fn a_refused_move_keeps_the_budget() {
        let mut motion = start();
        let far = at(30.0, GROUND_HEIGHT);
        assert!(
            motion
                .try_move(&limits(), &Flat, &bounds(), far, START_MS)
                .is_err()
        );
        let run = at(17.0, GROUND_HEIGHT);
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), run, START_MS),
            Ok(GroundCheck::Checked)
        );
    }

    #[test]
    fn heights_that_are_not_finite_are_refused() {
        let mut motion = start();
        for z in [f32::NAN, f32::INFINITY] {
            assert_eq!(
                motion.try_move(&limits(), &Flat, &bounds(), at(11.0, z), START_MS + STEP_MS),
                Err(MoveRefusal::NotFinite)
            );
        }
    }

    #[test]
    fn limits_must_be_finite_and_in_range() {
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert!(MoveLimits::new(bad, MARGIN, BURST, BELOW, ABOVE).is_err());
            assert!(MoveLimits::new(RUN_SPEED, MARGIN, bad, BELOW, ABOVE).is_err());
        }
        assert!(MoveLimits::new(RUN_SPEED, f32::NAN, BURST, BELOW, ABOVE).is_err());
        assert!(MoveLimits::new(RUN_SPEED, MARGIN, BURST, -1.0, ABOVE).is_err());
        assert!(MoveLimits::new(RUN_SPEED, 0.0, BURST, 0.0, 0.0).is_ok());
    }

    #[test]
    fn bad_numbers_outside_and_old_times_are_refused() {
        let mut motion = start();
        let time = START_MS + STEP_MS;
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), at(f32::NAN, 0.0), time),
            Err(MoveRefusal::NotFinite)
        );
        assert_eq!(
            motion.try_move(&limits(), &Flat, &bounds(), at(-1.0, GROUND_HEIGHT), time),
            Err(MoveRefusal::OutOfBounds)
        );
        assert_eq!(
            motion.try_move(
                &limits(),
                &Flat,
                &bounds(),
                at(11.0, GROUND_HEIGHT),
                START_MS - 1
            ),
            Err(MoveRefusal::TimeWentBack)
        );
        assert_eq!(MoveRefusal::TooFast.reason(), "too_fast");
    }
}
