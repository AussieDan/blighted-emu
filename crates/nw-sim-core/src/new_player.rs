// SPDX-License-Identifier: AGPL-3.0-or-later
//! The new-player start (new-player-start note, from the retail "Soul Survivor" capture): a new
//! character starts in the private tutorial level, walks to its exit, and enters the open world
//! at the beach start.

use crate::movement::Position;

/// The world-file folder name of the tutorial level (`FTUE_V2` on the client).
pub const TUTORIAL_FOLDER: &str = "ftue_v2";

/// Where a new character appears in the tutorial: retail's `worldPosAbs` in the first own state
/// (Observed: the entry bundle of the newchar capture of 2026-09-18 and of the Soul Survivor
/// capture). The client's first input point after the tutorial `LevelInfoChangedMsg`, 480.57,
/// 850.16, 91.26, is its camera, 1.67 m higher; a body held there hung over the deck (run 13).
pub const TUTORIAL_START: Position = Position {
    x: 478.5,
    y: 849.7,
    z: 89.586,
};

/// The ship's deck under [`TUTORIAL_START`] (Observed: the newchar own places after the start,
/// 89.508, 89.414 and 89.382 in the first own states that moved it, before any key). The deck is
/// not in the level's height map (the terrain there is at 50 m); the level collision's deck (the
/// galleon's `galleon_body.rnr`) is at 89.38 m here.
pub const TUTORIAL_START_FLOOR: Position = Position {
    z: 89.38,
    ..TUTORIAL_START
};

/// The tutorial's end: the client's last position before the server changed the level to the
/// open world. Retail ends the tutorial by its quest (the client asks to leave after the last
/// video); reaching this point ends it only for a player whose tutorial quest does not run (the
/// content has no tutorial quest).
pub const TUTORIAL_EXIT: Position = Position {
    x: 491.63,
    y: 210.79,
    z: 25.45,
};

/// How close (horizontally, in metres) a player must come to [`TUTORIAL_EXIT`] to leave. Our
/// engineering limit, not a retail value: retail has no exit area (Unknown), and 20 m is a
/// margin around the one exit point the capture holds.
pub const TUTORIAL_EXIT_RADIUS_M: f32 = 20.0;

/// Where a player appears in the open world after the tutorial (the beach start): retail's
/// `worldPosAbs` in the first own state after the open-world `LevelInfoChangedMsg` (Observed:
/// Soul Survivor capture; the terrain there is at 40.125 m). The client's first input point,
/// 7278.91, 3143.20, 41.45, is its camera.
pub const OPEN_WORLD_START: Position = Position {
    x: 7280.4863,
    y: 3141.4854,
    z: 40.121,
};

/// Whether `position` is at the tutorial's exit.
pub fn at_tutorial_exit(position: Position) -> bool {
    let (dx, dy) = (position.x - TUTORIAL_EXIT.x, position.y - TUTORIAL_EXIT.y);
    dx.hypot(dy) <= TUTORIAL_EXIT_RADIUS_M
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_is_near_its_point_and_not_at_the_start() {
        assert!(at_tutorial_exit(TUTORIAL_EXIT));
        assert!(at_tutorial_exit(Position {
            x: TUTORIAL_EXIT.x + TUTORIAL_EXIT_RADIUS_M - 1.0,
            ..TUTORIAL_EXIT
        }));
        assert!(!at_tutorial_exit(Position {
            x: TUTORIAL_EXIT.x + TUTORIAL_EXIT_RADIUS_M + 1.0,
            ..TUTORIAL_EXIT
        }));
        assert!(!at_tutorial_exit(TUTORIAL_START));
    }
}
