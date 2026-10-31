// SPDX-License-Identifier: AGPL-3.0-or-later
//! World simulation rules of M4 (M4 spec section 7), with no I/O and no clock reads: the caller
//! passes positions and times in. `sim` runs them.

pub mod among;
pub mod brain;
pub mod camera_check;
pub mod cells;
pub mod character;
pub mod combat;
pub mod equip_load;
pub mod footing;
pub mod grid;
pub mod interest;
pub mod level_ground;
pub mod movement;
pub mod new_player;
pub mod steering;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod tick;
pub mod tracking;

pub use among::{AmongCharacters, CharacterBody};
pub use brain::{BrainAction, ChaseBrain, ChaseSettings, Gait, MobState, Seen, walk_within};
pub use camera_check::{CameraBody, CameraCheck, CameraView};
pub use cells::{
    CellGrid, CellGridError, CellId, CellOwners, MAX_CELLS, assign_columns, level_bounds,
};
pub use character::{Character, CharacterInput, Intent, MountPace, MountPaces, Posture};
pub use combat::{
    Damage, Dice, Health, Hit, MIN_HIT, MobAttack, PlayerAttack, Threat, mob_hit, mob_max_health,
    player_hit, player_max_health,
};
pub use equip_load::{EquipLoad, EquipLoadLimits};
pub use footing::{Footing, WaterDepth};
pub use grid::SpatialGrid;
pub use interest::{Interest, InterestChanges};
pub use level_ground::LevelGround;
pub use movement::{
    CellBounds, Ground, GroundCheck, Motion, MotionState, MoveLimits, MoveLimitsError, MoveRefusal,
    Position,
};
pub use new_player::{
    OPEN_WORLD_START, TUTORIAL_EXIT, TUTORIAL_EXIT_RADIUS_M, TUTORIAL_FOLDER, TUTORIAL_START,
    TUTORIAL_START_FLOOR, at_tutorial_exit,
};
pub use steering::{Mover, Steering, turn_toward, yaw_gap, yaw_toward};
pub use tick::{DueTicks, TICKS_PER_SECOND, TickClock};
pub use tracking::{TrackInput, Tracked, TrackedBody};
