// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server's own movement of a player's character, from the keys and the camera yaw of each
//! client input record (movement study 2026-09-20). The client never sends where its character
//! stands; retail's server moves it itself: the own place of state `n` is the place of state
//! `n - 1` moved by input record `n` (in the newchar capture the motion of a new key showed in
//! the place of the record that held it, 30 of 41 key starts).
//!
//! The direction follows the keys and the camera yaw at once (a first diagonal record already
//! moved at 45° at full speed); the speed ramps at the action lists' rates. Up and down follow the
//! floor under the body (the ground's: the terrain and the level's structures, decks, roofs,
//! stairs: [`crate::LevelGround`]), and the speed is along it (retail ran 5.50 m/s along slopes,
//! median of 33 steady windows, and less over the ground). A body walks off an edge and falls to
//! the floor below, and a floor too high to step onto is an obstacle it does not climb. A wall
//! stops the body only where a solid still stands at the step-up's highest detection height
//! ([`WALL_HEIGHT_M`]) and the body slides along it: a replay of the retail tutorial that stopped
//! the body at the level's solids a step above its feet erred by a median 0.73 m against 0.22 m
//! without (the client's capsule steps onto and slides over low rocks and rails), and with no
//! wall at all run 15 (2026-09-22) walked the body into the 4 m block under the tutorial
//! beach's stair landing, where its client was pushed into the solid and stuck.
//!
//! The movement modes (movement modes study 2026-09-20, `local-data/scratch-work/movemodes`):
//! - The posture: the crouch and prone toggles (`playeractions_postureinput.actionlist`
//!   `ToggleCrouch`, `ToggleProne`) switch between standing, crouching and lying prone, and the
//!   posture picks the action lists' speed rows (`Crouch`, `Prone`, `Crouch_Walk`, `Prone_Walk`).
//! - The walk key, held: the slow walk.
//!   A posture change holds the body for its transition fragment's time while it slows.
//! - Wading: a body on a floor whose feet are deep enough under the local water (the sea, a
//!   lake, a river: [`Ground::water_surface`]) moves at the `Wade` rows. No jump starts in water
//!   that deep, wading or swimming (`playeractions_traversal.actionlist` `Jump`, `Jump_Diagonal`,
//!   `Standing_Jump`: `Not IfWaterDepth GreaterThanOrEqual Shallow`; run 15: a jump key while
//!   swimming threw the body 0.9 m out of the sea).
//! - The road boost: sprinting on a road for long enough adds the `Traversal_RoadSprintBoost_Haste`
//!   status's `SprintSpeedMod` to the sprint speed; combat on the road ends it.
//! - The land dodge: a dodge press on a floor moves the body by the root motion of the dodge clip
//!   of its equip load class ([`EquipLoad`]); an overburdened body neither dodges nor sprints.

use nw_world::collision_index::FLOOR_SEARCH_ABOVE_M;

use nw_content::mounts::{MountMovement, MountSurface, SurfaceSpeeds};

use crate::equip_load::EquipLoad;
use crate::footing::WaterDepth;
use crate::movement::{Ground, Position};

/// The forward speed by default, m/s (`playeractions_sprint.actionlist`
/// `ServerValidatedCanSprint` `Speed` 5.5; retail newchar forward 5.498 m/s, median of 360 steady
/// windows, and the same with a key to the side).
pub const SPRINT_SPEED_MPS: f32 = 5.5;
/// The speed in every direction after the walk toggle, m/s (`playeractions_nav.actionlist`
/// `ServerValidatedCanRun` `Speed` 3.25; retail clean walking captures 3.235 m/s forward and back).
pub const RUN_SPEED_MPS: f32 = 3.25;
/// The speed with the back key without the walk toggle, m/s (`playeractions_nav.actionlist`
/// `Speed` 0, 4.00, 0; retail clean back captures 3.98 to 4.00 m/s).
pub const BACK_SPEED_MPS: f32 = 4.0;
/// How fast the speed rises and falls, m/s² (`playeractions_sprint.actionlist`
/// `ServerValidatedCanSprint` `Accel` and `Decel` 20; retail positions one third of a second after
/// a key change fit 20 as well as 15 to 30).
pub const ACCELERATION_MPS2: f32 = 20.0;
/// How fast the speed falls when the keys let go, m/s².
pub const DECELERATION_MPS2: f32 = 20.0;
/// The most the floor rises under a walking body in one input record, m (Observed: the largest
/// rise of the retail own place in one record while walking, 28,600 records of the newchar open
/// world). A higher floor is an obstacle: the body keeps its height.
pub const STEP_UP_M: f32 = 0.376;
/// The most the floor falls under a walking body in one input record before it falls, m
/// (Observed: the 99.9th percentile of the retail drops while walking, same records).
pub const STEP_DOWN_M: f32 = 0.282;
/// The radius of the player's capsule, m (`slices/player.dynamicslice`: the character's capsule
/// shape, radius 0.41).
pub const PLAYER_RADIUS_M: f32 = 0.41;
/// How high above the feet a solid is a wall to a walking body, m: the character controller's
/// highest step-up detection (`slices/player.dynamicslice` `m_stepUpMaxDetectionHeight` 1.3); a
/// solid lower than that is stepped onto or slid over. The level collision of the tutorial beach
/// (run 15, 2026-09-22): the block under the stair landing stands 4.0 m over the sand, the rocks
/// beside the path 1.1 to 1.2 m.
pub const WALL_HEIGHT_M: f32 = FLOOR_SEARCH_ABOVE_M;
/// The upward speed of a jump, m/s, and the pull down while in the air, m/s² (Observed: a least
/// squares fit of a retail standing jump on flat ground, newchar record 24187, 22 states, root
/// mean square 0.03 m; two more jumps gave 5.26 and 5.49 m/s, 14.8 and 15.5 m/s²).
pub const JUMP_SPEED_MPS: f32 = 5.48;
/// See [`JUMP_SPEED_MPS`].
pub const GRAVITY_MPS2: f32 = 15.24;
/// The pull down on a body that falls without a jump, m/s² (Observed: a least squares fit of the
/// retail drop into the tutorial sea after the server put the player 7.6 m above it, newchar
/// records 3073 to 3108, 36 places, root mean square 0.007 m; the jump's pull is stronger).
pub const FALL_GRAVITY_MPS2: f32 = 9.76;
/// Input records between a standing jump's key and the lift (Observed: 3 in 7 of the newchar
/// standing jumps; a running jump lifts at once).
pub const STANDING_JUMP_WIND_UP: u8 = 3;
/// How far below the sea a swimming body's feet are, m (Observed: the retail own place at 18.59 m
/// in the tutorial sea of level 20, every record of the swim to shore).
pub const SWIM_DEPTH_M: f32 = 1.41;
/// The speed while the block key is held and a key goes forward, m/s
/// (`playeractions_nav.actionlist` `Block_Nav_Speed` `Speed` 2.0; retail newchar: 5 of the 11
/// steady forward block windows at 1.98 m/s, the others still ramping, and 3 of 3 forward and
/// right at 1.96 m/s).
pub const BLOCK_SPEED_MPS: f32 = 2.0;
/// The speed while the block key is held and no key goes forward, m/s (`playeractions_nav.
/// actionlist` `Nav` `Speed` 1.65 in every direction; retail newchar: back 1.647 m/s, median of 87
/// steady block windows with quartiles 1.647 and 1.674, back and right 1.58 of 19).
pub const BLOCK_BACK_SPEED_MPS: f32 = 1.65;
/// How fast the speed rises and falls while blocking, m/s² (`Block_Nav_Speed` `Accel` and `Decel`
/// 25).
pub const BLOCK_ACCELERATION_MPS2: f32 = 25.0;
/// The speed of a swimming body, m/s (`playeractions_swim.actionlist` `Swim_Nav` `Speed` 3.0;
/// retail newchar tutorial swim: 3.0 m/s after the ramp).
pub const SWIM_SPEED_MPS: f32 = 3.0;
/// How fast a swimming body speeds up, m/s² (`Swim_Nav` `Accel` 3.0: retail swim windows of 10
/// records from 1.25 to 3.0 m/s as it ramps), and slows down (`Decel` 130: retail 6.1 to 3.3 m/s
/// in one record after a dash).
pub const SWIM_ACCELERATION_MPS2: f32 = 3.0;
/// See [`SWIM_ACCELERATION_MPS2`].
pub const SWIM_DECELERATION_MPS2: f32 = 130.0;
/// The speed of a swimming body's dash, a press of the dodge key in the water, m/s
/// (`playeractions_dodge.actionlist` `Dodge` `InheritVelocityMaxSpeed` 6; retail newchar tutorial:
/// 23 of the 25 steady dash windows at 6.0 m/s, from the first record of the press).
pub const SWIM_DASH_SPEED_MPS: f32 = 6.0;
/// How long a dash lasts, s (Observed: 37, 37 and 38 records of 1/30 s in the three tutorial dashes
/// that held the key to their end; the speed went back to the swim speed with the key still
/// held).
pub const SWIM_DASH_S: f32 = 37.0 / 30.0;

/// The speed of a crouching body without the walk toggle, forward and to the side, m/s
/// (`playeractions_sprint.actionlist` `Crouch` `Speed` 2.5, `Accel` and `Decel` 20; retail
/// `walk_running_crouch_prone` captures: 2.50 m/s, median of 118 and 246 steady windows).
pub const CROUCH_SPEED_MPS: f32 = 2.5;
/// The speed of a crouching body with the back key, m/s (`playeractions_nav.actionlist`
/// `Crouch_Unarmed` and `Combat_Nav_Crouch` `Speed` 1.25, `Accel` and `Decel` 25; retail
/// `walk_running_crouch_prone` records 2993 to 3257: 1.23 m/s, median of 30 steady windows).
pub const CROUCH_BACK_SPEED_MPS: f32 = 1.25;
/// How fast a crouching body's back speed rises and falls, m/s² (see [`CROUCH_BACK_SPEED_MPS`]).
pub const CROUCH_BACK_ACCELERATION_MPS2: f32 = 25.0;
/// The speed of a crouching body after the walk toggle, m/s (`playeractions_nav.actionlist`
/// `Crouch` `Speed` 1.65, `Accel` and `Decel` 10; no retail sample).
pub const CROUCH_RUN_SPEED_MPS: f32 = 1.65;
/// How fast a crouching body's speed after the walk toggle rises and falls, m/s².
pub const CROUCH_RUN_ACCELERATION_MPS2: f32 = 10.0;
/// The speed of a crouching body with the walk key held, m/s (`playeractions_nav.actionlist`
/// `Crouch_Walk` `Speed` 0.825, `Accel` and `Decel` 2.5; no retail sample).
pub const CROUCH_WALK_SPEED_MPS: f32 = 0.825;
/// How fast a crouching walk's speed rises and falls, m/s².
pub const CROUCH_WALK_ACCELERATION_MPS2: f32 = 2.5;
/// The speed of a prone body without the walk toggle, forward and to the side, m/s
/// (`playeractions_sprint.actionlist` `Prone` `Speed` 1.25, `Accel` and `Decel` 20; retail
/// `walk_running_crouch_prone` captures: 1.23 to 1.26 m/s, medians of 129 to 322 steady
/// windows).
pub const PRONE_SPEED_MPS: f32 = 1.25;
/// The speed of a prone body with the back key, m/s (Observed: retail `walk_running_crouch_prone`
/// records 1855 to 2992, 0.63 m/s, median of 64 steady windows, 0.61 back and left; no action
/// list row holds it).
pub const PRONE_BACK_SPEED_MPS: f32 = 0.63;
/// The speed of a prone body after the walk toggle, m/s (`playeractions_nav.actionlist` `Prone`
/// `Speed` 0.375, `Accel` 1, `Decel` 5; no retail sample).
pub const PRONE_RUN_SPEED_MPS: f32 = 0.375;
/// The speed of a prone body with the walk key held, m/s (`playeractions_nav.actionlist`
/// `Prone_Walk` `Speed` 0.1875, `Accel` 1, `Decel` 5; no retail sample).
pub const PRONE_WALK_SPEED_MPS: f32 = 0.1875;
/// How fast a prone body's slow speeds rise, m/s² (`Prone` and `Prone_Walk` `Accel` 1).
pub const PRONE_SLOW_ACCELERATION_MPS2: f32 = 1.0;
/// How fast a prone body's slow speeds fall, m/s² (`Prone` and `Prone_Walk` `Decel` 5).
pub const PRONE_SLOW_DECELERATION_MPS2: f32 = 5.0;
/// The speed of a standing body with the walk key held, in every direction, m/s (Observed: retail
/// `walk_running_crouch_prone` captures, 2.00 m/s forward, back and to the side, medians of 7
/// periods of 49 to 321 steady windows; the action lists' `Stand_Walk` row says 1.65).
pub const WALK_SPEED_MPS: f32 = 2.0;
/// How fast a standing walk's speed rises and falls, m/s² (`playeractions_nav.actionlist`
/// `Stand_Walk` `Accel` and `Decel` 25).
pub const WALK_ACCELERATION_MPS2: f32 = 25.0;
/// The feet of a body on a floor at least this deep under the water's surface wade, m
/// (Observed: the own state's water depth went from very shallow to shallow at feet depths of
/// 0.894 to 0.945 m and back at 0.835 to 0.897 m in about 50 changes in six lakes of an
/// open-world capture, two of them both at 0.897 m; in the newchar swim to shore it went to very
/// shallow between 0.874 and 0.842 m, and the sprint started there. No hysteresis).
pub const WADE_DEPTH_M: f32 = 0.895;
/// The speed of a wading body without the walk toggle, forward and to the side, m/s
/// (`playeractions_sprint.actionlist` `Wade` `Speed` 2.5, `Accel` and `Decel` 20; retail newchar
/// swim to shore: 2.2 m/s over the ground up a slope while the feet were 1.42 to 0.89 m deep).
pub const WADE_SPEED_MPS: f32 = 2.5;
/// The speed of a crouching wading body, m/s (`playeractions_sprint.actionlist` `Wade+Crouch`
/// `Speed` 1.75).
pub const WADE_CROUCH_SPEED_MPS: f32 = 1.75;
/// The speed of a wading body with the back key, the walk toggle or the walk key, m/s
/// (`playeractions_nav.actionlist` `Wade` `Speed` 1.65, `Accel` and `Decel` 10).
pub const WADE_SLOW_SPEED_MPS: f32 = 1.65;
/// How fast a wading body's slow speed rises and falls, m/s².
pub const WADE_SLOW_ACCELERATION_MPS2: f32 = 10.0;
/// The share of road under the body from which it is on a road (`playeractions_global.actionlist`
/// `Traversal_Road_Sprint_Speed_Boost`: `IfOnRoad` `GreaterThanOrEqual` 0.1). `IfOnRoad` reads
/// the share at the place as it is (`FUN_142c41000`), not the `RoadStateComponent`'s state with
/// its 3 s on and 2 s off delays; retail `r-ow` records 11215 to 11245: the boost 0.3 to 0.8 s
/// after the own place entered the road, after 28 s of sprint off it.
pub const ROAD_SHARE_MIN: f32 = 0.1;
/// How far above or below the terrain a body may be and still be on its road, m
/// (`ActionConditionIfOnRoad` `HeightThreshold` 1.0, the default the action lists keep:
/// constructor `FUN_142c40cf0`; its check `FUN_145c52f40` gives no road when the place is
/// farther from the terrain height: a bridge, a roof, a cave).
pub const ROAD_HEIGHT_THRESHOLD_M: f32 = 1.0;
/// How long a body sprints before a road speeds it up, s (`Traversal_Road_Sprint_Speed_Boost`:
/// `Sprint_TravelMode` installed for 3 s; retail `walk_running_crouch_prone_2_freedom`: the boost
/// 3.1 to 3.3 s after the motion started, twice).
pub const ROAD_SPRINT_DELAY_S: f32 = 3.0;
/// The road boost's `SprintSpeedMod` (`javelindata_statuseffects_common.datasheet`
/// `Traversal_RoadSprintBoost_Haste`; retail `walk_running_crouch_prone`: 6.32 m/s on a road
/// against 5.50 m/s before the boost, 1.15 times).
pub const ROAD_SPRINT_SPEED_MOD: f32 = 0.15;
/// How long the road boost lasts after its last refresh, s (`Traversal_RoadSprintBoost_Haste`
/// `BaseDuration` 3).
pub const ROAD_HASTE_S: f32 = 3.0;
/// How long the road boost stays off after combat removed it, s
/// (`Traversal_RoadSprintBoost_CombatTimer` `BaseDuration` 5; `Traversal_Road_Sprint_Speed_Boost_
/// CombatRemoval` removes the boost on an attack, a block or a dodge).
pub const ROAD_COMBAT_S: f32 = 5.0;
/// How fast the body slows while it changes its posture, m/s² (Observed: in the retail
/// `walk_running_crouch_prone` capture the prone press at record 1855 slowed the sprint from 5.4
/// to 0.3 m/s in 20 records, 1.96 m, a least squares slope of 8.0 m/s²; the crouch press at
/// record 927 slowed it from 5.5 m/s to 1.5 to 2.0 m/s at 0.5 s, then the crouch speed held).
pub const POSTURE_CHANGE_DECELERATION_MPS2: f32 = 8.0;
/// How long a posture change holds the body before its keys move it again, s
/// (`player.adb` `Sprint_PostureTransition` fragments: the `ResumeSprint` marker of
/// `Crouch+FromStand` at 0.433 s, `Prone+FromStand` and `Prone+FromCrouch` at 0.667 s,
/// `Stand+FromProne` at 0.767 s, `Crouch+FromProne` at 0.8 s; standing up from a crouch has no
/// transition fragment. Retail: a prone press with the forward key held moved the body again
/// after 0.70 s, `walk_running_crouch_prone_2` record 1328).
const POSTURE_CHANGE_S: [(Posture, Posture, f32); 5] = [
    (Posture::Stand, Posture::Crouch, 0.433_333_34),
    (Posture::Stand, Posture::Prone, 0.666_666_7),
    (Posture::Crouch, Posture::Prone, 0.666_666_7),
    (Posture::Prone, Posture::Stand, 0.766_666_65),
    (Posture::Prone, Posture::Crouch, 0.8),
];
/// Frames per second of the dodge clips' root motion (`Xform` bone keys at 30 fps).
const DODGE_CLIP_FPS: f32 = 30.0;
/// How long a dodge holds the body before a movement key takes it over again, s (every dodge
/// fragment of `playeractions_dodge.actionlist`: the `CanTransition` marker from 0.5 s; retail
/// newchar: the run came back 2 to 3 records after 0.5 s).
pub const DODGE_CONTROL_S: f32 = 0.5;
/// How long the dodge's translation multiplier holds, s (every dodge fragment of
/// `playeractions_dodge.actionlist`: the `Dodge_Roll*_Translation` markers from 0 to 0.567 s;
/// after them the clip's root moves the body as it is).
const DODGE_TRANSLATION_S: f32 = 0.566_666_66;
/// The normal dodge's translation multiplier (`playeractions_dodge.actionlist`
/// `AnimationDrivenMotion` `Roll_Translation` 1.28; retail `walk_running_crouch_prone` record
/// 3328, a back dodge of the normal class: the place at 0.5 s within 0.034 m).
pub const NORMAL_DODGE_TRANSLATION: f32 = 1.28;
/// The fast dodge's translation multiplier (`FastRoll_Translation` 1.15; no retail sample).
pub const FAST_DODGE_TRANSLATION: f32 = 1.15;
/// The slow dodge's translation multiplier (Observed: a least squares fit of the 13 clean retail
/// newchar land dodges, a character of the slow class, to the `quickstep_hvy` clips: 1.124 to
/// 1.173 per dodge, median 1.15; the place at 0.5 s erred by a median 0.021 m. The action list's
/// `SlowRoll_Translation` says 1.43; what makes retail's factor smaller is Unknown).
pub const SLOW_DODGE_TRANSLATION: f32 = 1.15;
/// The root of the normal forward dodge (`unarmed_quickstep_f`): frame and distance along the
/// dodge, m; linear between keys.
const NORMAL_DODGE_FORWARD_ROOT: [(u8, f32); 13] = [
    (0, 0.0),
    (15, 2.8),
    (16, 2.786),
    (17, 2.802),
    (18, 2.832),
    (19, 2.853),
    (20, 2.866),
    (21, 2.871),
    (22, 2.871),
    (23, 2.867),
    (24, 2.86),
    (26, 2.855),
    (44, 2.854),
];
/// The root of the normal back dodge (`unarmed_quickstep_b`).
const NORMAL_DODGE_BACK_ROOT: [(u8, f32); 9] = [
    (0, 0.0),
    (15, 2.8),
    (17, 2.963),
    (18, 3.0),
    (21, 2.998),
    (25, 2.994),
    (26, 2.994),
    (27, 3.0),
    (44, 3.0),
];
/// The root of the fast forward dodge (`unarmed_dodgeroll_f_light_to_crch`).
const FAST_DODGE_FORWARD_ROOT: [(u8, f32); 9] = [
    (0, 0.0),
    (2, 0.519),
    (9, 2.326),
    (11, 2.846),
    (13, 3.368),
    (14, 3.631),
    (15, 3.895),
    (16, 4.16),
    (29, 4.16),
];
/// The root of the fast back dodge (`unarmed_dodgeroll_b_light_to_crch`).
const FAST_DODGE_BACK_ROOT: [(u8, f32); 13] = [
    (0, 0.0),
    (3, 0.72),
    (8, 1.915),
    (10, 2.395),
    (12, 2.878),
    (13, 3.121),
    (14, 3.364),
    (15, 3.609),
    (16, 3.856),
    (17, 4.103),
    (20, 4.105),
    (27, 4.114),
    (29, 4.115),
];
/// The root of the slow forward dodge (`unarmed_quickstep_hvy_f`, the same for the one-handed,
/// two-handed and bow stances).
const SLOW_DODGE_FORWARD_ROOT: [(u8, f32); 27] = [
    (0, 0.0),
    (4, 0.646),
    (5, 0.81),
    (6, 0.976),
    (8, 1.316),
    (9, 1.484),
    (10, 1.647),
    (11, 1.803),
    (12, 1.947),
    (13, 2.078),
    (14, 2.202),
    (15, 2.321),
    (16, 2.44),
    (17, 2.549),
    (18, 2.636),
    (19, 2.703),
    (20, 2.762),
    (21, 2.815),
    (22, 2.86),
    (23, 2.898),
    (24, 2.93),
    (25, 2.956),
    (26, 2.976),
    (27, 2.989),
    (28, 2.997),
    (29, 3.0),
    (38, 3.0),
];
/// The root of the slow back dodge (`unarmed_quickstep_hvy_b`, the same for the other stances).
const SLOW_DODGE_BACK_ROOT: [(u8, f32); 30] = [
    (0, 0.0),
    (1, 0.125),
    (2, 0.366),
    (3, 0.603),
    (4, 0.806),
    (5, 1.012),
    (6, 1.204),
    (7, 1.363),
    (8, 1.493),
    (9, 1.609),
    (10, 1.716),
    (11, 1.816),
    (13, 2.011),
    (14, 2.112),
    (15, 2.222),
    (16, 2.327),
    (17, 2.4),
    (21, 2.598),
    (22, 2.646),
    (23, 2.693),
    (24, 2.738),
    (25, 2.781),
    (26, 2.822),
    (27, 2.861),
    (28, 2.896),
    (29, 2.928),
    (30, 2.956),
    (31, 2.98),
    (32, 3.0),
    (38, 3.0),
];

/// What the keys of one input record ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Intent {
    /// Forward (1), back (-1) or neither (0).
    pub forward: i8,
    /// Right (1), left (-1) or neither (0).
    pub right: i8,
    /// The jump key is held.
    pub jump: bool,
    /// The walk toggle is pressed in this record.
    pub walk_toggle: bool,
    /// The block key is held.
    pub block: bool,
    /// The dodge key is held.
    pub dodge: bool,
    /// The crouch toggle is pressed in this record.
    pub crouch_toggle: bool,
    /// The prone toggle is pressed in this record.
    pub prone_toggle: bool,
    /// The walk key is held.
    pub walk: bool,
}

impl Intent {
    /// The intent of the held direction keys, without a jump or a walk toggle.
    pub fn of_keys(forward: bool, back: bool, left: bool, right: bool) -> Self {
        Self {
            forward: i8::from(forward) - i8::from(back),
            right: i8::from(right) - i8::from(left),
            ..Self::default()
        }
    }

    /// Whether a direction key is held.
    pub(crate) const fn moves(self) -> bool {
        self.forward != 0 || self.right != 0
    }

    /// The intent of a rider: the posture toggles, the walk toggle, the block and the dodge do
    /// not reach the body (`playeractions_mount.actionlist`: no posture or block action while
    /// `IsMounted`, and the dodge key is the mount's dash, input 67 held in 99 % of the retail
    /// records while the mount stamina drained against 21 % of the others).
    const fn mounted(self) -> Self {
        Self {
            walk_toggle: false,
            block: false,
            dodge: false,
            crouch_toggle: false,
            prone_toggle: false,
            ..self
        }
    }
}

/// One input record for the character.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterInput {
    /// The keys.
    pub intent: Intent,
    /// The camera yaw (radians; 0 faces north, positive turns west).
    pub yaw: f32,
    /// The record's frame time, s.
    pub dt_s: f32,
}

/// How the body holds itself (`playeractions_postureinput.actionlist`: the mannequin tags
/// `Stand`, `Crouch` and `Prone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Posture {
    /// Standing.
    #[default]
    Stand,
    /// Crouching.
    Crouch,
    /// Lying prone.
    Prone,
}

impl Posture {
    /// The posture after a press of the crouch toggle (`ToggleCrouch`: `Crouch_Desired`, or
    /// `Stand_Desired` when crouching).
    const fn crouch_toggled(self) -> Self {
        match self {
            Self::Crouch => Self::Stand,
            Self::Stand | Self::Prone => Self::Crouch,
        }
    }

    /// The posture after a press of the prone toggle (`ToggleProne`: `Prone_Desired`, or
    /// `Stand_Desired` when prone).
    const fn prone_toggled(self) -> Self {
        match self {
            Self::Prone => Self::Stand,
            Self::Stand | Self::Crouch => Self::Prone,
        }
    }
}

/// A speed and how fast it is reached, from one action list row.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pace {
    speed_mps: f32,
    acceleration_mps2: f32,
    deceleration_mps2: f32,
}

impl Pace {
    /// A row whose speed rises and falls at the same rate.
    const fn even(speed_mps: f32, rate_mps2: f32) -> Self {
        Self {
            speed_mps,
            acceleration_mps2: rate_mps2,
            deceleration_mps2: rate_mps2,
        }
    }
}

/// A mount's speeds on one surface (a `MountMovementData` row's `<Surface>MaxSpeed`,
/// `WalkSpeed`, `SlowWalkSpeed`, `Accel`, `Decel`, `WalkAccel` and `WalkDecel`; mounts study,
/// run 15).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MountPace {
    /// The run, m/s.
    pub run_mps: f32,
    /// With the walk key, m/s.
    pub walk_mps: f32,
    /// With the walk key and the slow walk setting (6271), m/s.
    pub slow_walk_mps: f32,
    /// How fast the run is reached, m/s².
    pub acceleration_mps2: f32,
    /// How fast the speed falls, m/s².
    pub deceleration_mps2: f32,
    /// How fast the walk is reached, m/s².
    pub walk_acceleration_mps2: f32,
    /// How fast the walk's speed falls, m/s².
    pub walk_deceleration_mps2: f32,
}

/// A mount's speeds on each surface (client enum `MountMovementSurfaces`) and its dash.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MountPaces {
    /// On a road.
    pub on_road: MountPace,
    /// Off the roads.
    pub off_road: MountPace,
    /// In very shallow water ([`WaterDepth::VeryShallow`]).
    pub very_shallow_water: MountPace,
    /// In shallow water ([`WaterDepth::Shallow`]).
    pub shallow_water: MountPace,
    /// Swimming.
    pub deep_water: MountPace,
    /// `DashSpeedScaler`: the dash's share of the run.
    pub dash_speed_scale: f32,
    /// `DashAccelScaler`: the dash's share of the acceleration.
    pub dash_acceleration_scale: f32,
}

impl MountPace {
    /// The speeds of one surface's `speeds`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "mount speeds of a few metres per second"
    )]
    pub const fn of(speeds: SurfaceSpeeds) -> Self {
        Self {
            run_mps: speeds.max_speed as f32,
            walk_mps: speeds.walk_speed as f32,
            slow_walk_mps: speeds.slow_walk_speed as f32,
            acceleration_mps2: speeds.acceleration as f32,
            deceleration_mps2: speeds.deceleration as f32,
            walk_acceleration_mps2: speeds.walk_acceleration as f32,
            walk_deceleration_mps2: speeds.walk_deceleration as f32,
        }
    }
}

impl MountPaces {
    /// The fastest its rider can go: the fastest surface's run with the dash, m/s.
    pub fn top_speed_mps(&self) -> f32 {
        [
            self.on_road,
            self.off_road,
            self.very_shallow_water,
            self.shallow_water,
            self.deep_water,
        ]
        .iter()
        .map(|pace| pace.run_mps)
        .fold(0.0, f32::max)
            * self.dash_speed_scale.max(1.0)
    }

    /// The speeds of the movement row `movement`.
    #[allow(clippy::cast_possible_truncation, reason = "dash scales near 1")]
    pub fn of(movement: &MountMovement) -> Self {
        Self {
            on_road: MountPace::of(movement.on(MountSurface::OnRoad)),
            off_road: MountPace::of(movement.on(MountSurface::OffRoad)),
            very_shallow_water: MountPace::of(movement.on(MountSurface::InVeryShallowWater)),
            shallow_water: MountPace::of(movement.on(MountSurface::InShallowWater)),
            deep_water: MountPace::of(movement.on(MountSurface::InDeepWater)),
            dash_speed_scale: movement.dash_speed_scale as f32,
            dash_acceleration_scale: movement.dash_acceleration_scale as f32,
        }
    }
}

/// A body on a mount: the mount's speeds, the surface under it, and the rider's dash and slow
/// walk.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mounted {
    paces: MountPaces,
    /// The speeds of the surface under the mount now.
    pace: MountPace,
    dashing: bool,
    slow_walk: bool,
}

impl Mounted {
    /// The speed the keys of `intent` ask for (`playeractions_mount.actionlist` `MountWalk`:
    /// the walk key walks, slowly with the slow walk setting; `MountDash`: the dash scales the
    /// run).
    fn pace(&self, intent: Intent) -> Pace {
        let pace = self.pace;
        if intent.walk {
            return Pace {
                speed_mps: if self.slow_walk {
                    pace.slow_walk_mps
                } else {
                    pace.walk_mps
                },
                acceleration_mps2: pace.walk_acceleration_mps2,
                deceleration_mps2: pace.walk_deceleration_mps2,
            };
        }
        let (speed, acceleration) = if self.dashing {
            (
                pace.run_mps * self.paces.dash_speed_scale,
                pace.acceleration_mps2 * self.paces.dash_acceleration_scale,
            )
        } else {
            (pace.run_mps, pace.acceleration_mps2)
        };
        Pace {
            speed_mps: speed,
            acceleration_mps2: acceleration,
            deceleration_mps2: pace.deceleration_mps2,
        }
    }
}

/// The clip that moves a dodging body and its translation multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DodgeRoot {
    keys: &'static [(u8, f32)],
    translation: f32,
}

impl DodgeRoot {
    /// The dodge of a body of equip load `load`, back or forward (`playeractions_dodge.
    /// actionlist`: the fragment `Dodge_Roll_Fast` for the fast class, `Dodge_Roll_Slow` for the
    /// slow one, `Dodge_Roll` for the others).
    const fn of(load: EquipLoad, back: bool) -> Self {
        let (keys, translation): (&'static [(u8, f32)], f32) = match (load, back) {
            (EquipLoad::Fast, false) => (&FAST_DODGE_FORWARD_ROOT, FAST_DODGE_TRANSLATION),
            (EquipLoad::Fast, true) => (&FAST_DODGE_BACK_ROOT, FAST_DODGE_TRANSLATION),
            (EquipLoad::Slow, false) => (&SLOW_DODGE_FORWARD_ROOT, SLOW_DODGE_TRANSLATION),
            (EquipLoad::Slow, true) => (&SLOW_DODGE_BACK_ROOT, SLOW_DODGE_TRANSLATION),
            (EquipLoad::Normal | EquipLoad::Overburdened, false) => {
                (&NORMAL_DODGE_FORWARD_ROOT, NORMAL_DODGE_TRANSLATION)
            }
            (EquipLoad::Normal | EquipLoad::Overburdened, true) => {
                (&NORMAL_DODGE_BACK_ROOT, NORMAL_DODGE_TRANSLATION)
            }
        };
        Self { keys, translation }
    }

    /// The clip's root at `at_s` seconds, m: linear between its keys, its last key after them.
    fn root(self, at_s: f32) -> f32 {
        let frame = at_s * DODGE_CLIP_FPS;
        let keys = self.keys;
        keys.windows(2)
            .find(|pair| frame < f32::from(pair[1].0))
            .map_or(keys[keys.len() - 1].1, |pair| {
                let ((from, start), (to, end)) = (pair[0], pair[1]);
                let share = (frame - f32::from(from)) / f32::from(to - from);
                start + (end - start) * share.max(0.0)
            })
    }

    /// How far the body has gone at `at_s` seconds, m: the root times the multiplier while the
    /// translation marker holds, the root alone after it.
    fn distance(self, at_s: f32) -> f32 {
        let held = at_s.min(DODGE_TRANSLATION_S);
        self.root(held) * self.translation + self.root(at_s) - self.root(held)
    }

    /// How long the clip moves, s.
    fn length_s(self) -> f32 {
        f32::from(self.keys[self.keys.len() - 1].0) / DODGE_CLIP_FPS
    }
}

/// A land dodge under way.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Dodge {
    root: DodgeRoot,
    /// The direction of the dodge (a unit vector in the ground plane).
    direction: [f32; 2],
    /// Seconds since the press.
    at_s: f32,
}

/// Where the character is between the floors.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Vertical {
    /// On a floor.
    Grounded,
    /// A standing jump before the lift: records left.
    WindUp(u8),
    /// In a jump, rising at `rise` m/s (falling below 0).
    Jumping(f32),
    /// Falling off a floor, rising at this speed (falling below 0).
    Falling(f32),
}

/// A player's character as the server moves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Character {
    position: Position,
    speed_mps: f32,
    /// The direction of its motion (a unit vector in the ground plane).
    heading: [f32; 2],
    walking: bool,
    vertical: Vertical,
    jump_held: bool,
    /// It swims: it ended the record before at the swim depth.
    swimming: bool,
    dodge_held: bool,
    /// Seconds left of a swim dash.
    dash_s: f32,
    posture: Posture,
    /// The land dodge under way.
    dodge: Option<Dodge>,
    /// Seconds it has sprinted standing (`Sprint_TravelMode` installed).
    sprint_s: f32,
    /// Seconds left of a posture change ([`POSTURE_CHANGE_S`]).
    posture_change_s: f32,
    /// Its equip load category.
    equip_load: EquipLoad,
    /// A held status effect keeps it from sprinting (`NoSprint`).
    no_sprint: bool,
    /// Seconds left of the road boost.
    road_haste_s: f32,
    /// The road boost's action is on (`Traversal_Road_Sprint_Speed_Boost` installed): the
    /// body is on a road and refreshes the boost.
    road_boost_on: bool,
    /// Seconds left while combat keeps the road boost off.
    combat_s: f32,
    /// The mount it rides.
    mount: Option<Mounted>,
}

/// The height of a swimming body's feet in the sea at `sea_level_m`.
pub fn swim_floor(sea_level_m: f32) -> f32 {
    sea_level_m - SWIM_DEPTH_M
}

/// The height of a swimming body's feet at (`x`, `y`) of `ground`, with the sea at
/// `sea_level_m`, when water is there (the local surface: a lake's, a river's, the sea's).
pub fn swim_floor_at(
    ground: &(impl Ground + ?Sized),
    x: f32,
    y: f32,
    sea_level_m: f32,
) -> Option<f32> {
    ground.water_surface(x, y, sea_level_m).map(swim_floor)
}

/// The unit vectors in the ground plane of camera yaw `yaw`: forward and right.
pub fn yaw_axes(yaw: f32) -> ([f32; 2], [f32; 2]) {
    let (sin, cos) = yaw.sin_cos();
    ([-sin, cos], [cos, sin])
}

/// The wall of `ground` that a body at `from` meets on its way to `to` in the ground plane: the
/// outward unit normal of the first solid it enters at [`WALL_HEIGHT_M`] over its feet, with
/// its capsule's [`PLAYER_RADIUS_M`] ahead of it. `None` for a free way or no motion.
pub fn wall_between(
    ground: &(impl Ground + ?Sized),
    from: Position,
    to: Position,
) -> Option<[f32; 2]> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = dx.hypot(dy);
    if length <= f32::EPSILON {
        return None;
    }
    let height = from.z + WALL_HEIGHT_M;
    let reach = (length + PLAYER_RADIUS_M) / length;
    ground.wall(
        Position { z: height, ..from },
        Position {
            x: from.x + dx * reach,
            y: from.y + dy * reach,
            z: height,
        },
    )
}

/// Where a body at `from` that moves to `to` stops for the walls of `ground`: at `to` when the
/// way is free; else slid along the wall it meets (its motion less the part into the wall), or
/// at `from` when the slide meets a wall too. The height stays `to`'s.
pub fn stop_at_walls(ground: &(impl Ground + ?Sized), from: Position, to: Position) -> Position {
    let Some(normal) = wall_between(ground, from, to) else {
        return to;
    };
    let step = [to.x - from.x, to.y - from.y];
    let into = (step[0] * normal[0] + step[1] * normal[1]).min(0.0);
    let slid = Position {
        x: to.x - into * normal[0],
        y: to.y - into * normal[1],
        ..to
    };
    if wall_between(ground, from, slid).is_some() {
        Position { z: to.z, ..from }
    } else {
        slid
    }
}

/// The unit vector in the ground plane of the keys of `intent` with camera yaw `yaw`, when they
/// ask for a direction.
pub(crate) fn key_direction(intent: Intent, yaw: f32) -> Option<[f32; 2]> {
    if !intent.moves() {
        return None;
    }
    let (forward, right) = yaw_axes(yaw);
    let (along, side) = (f32::from(intent.forward), f32::from(intent.right));
    let length = along.hypot(side);
    Some([
        (along * forward[0] + side * right[0]) / length,
        (along * forward[1] + side * right[1]) / length,
    ])
}

impl Character {
    /// A character standing at `position`.
    pub const fn new(position: Position) -> Self {
        Self {
            position,
            speed_mps: 0.0,
            heading: [0.0, 1.0],
            walking: false,
            vertical: Vertical::Grounded,
            jump_held: false,
            swimming: false,
            dodge_held: false,
            dash_s: 0.0,
            posture: Posture::Stand,
            dodge: None,
            sprint_s: 0.0,
            posture_change_s: 0.0,
            equip_load: EquipLoad::Normal,
            no_sprint: false,
            road_haste_s: 0.0,
            road_boost_on: false,
            combat_s: 0.0,
            mount: None,
        }
    }

    /// It gets on a mount with `paces`.
    pub const fn mount(&mut self, paces: MountPaces) {
        self.mount = Some(Mounted {
            paces,
            pace: paces.off_road,
            dashing: false,
            slow_walk: false,
        });
        self.dodge = None;
        self.posture = Posture::Stand;
        self.posture_change_s = 0.0;
    }

    /// It gets off its mount.
    pub const fn dismount(&mut self) {
        self.mount = None;
    }

    /// Whether it rides a mount.
    pub const fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// The fastest its mount can go ([`MountPaces::top_speed_mps`]); `None` on foot.
    pub fn mount_top_speed_mps(&self) -> Option<f32> {
        self.mount.map(|mounted| mounted.paces.top_speed_mps())
    }

    /// Its mount dashes, or stops dashing.
    pub const fn set_mount_dash(&mut self, dashing: bool) {
        if let Some(mounted) = self.mount.as_mut() {
            mounted.dashing = dashing;
        }
    }

    /// The walk key gives its mount's slow walk, or its walk.
    pub const fn set_mount_slow_walk(&mut self, slow_walk: bool) {
        if let Some(mounted) = self.mount.as_mut() {
            mounted.slow_walk = slow_walk;
        }
    }

    /// Whether it swims: it ended the last record at the swim depth.
    pub const fn is_swimming(&self) -> bool {
        self.swimming
    }

    /// Where it is.
    pub const fn position(&self) -> Position {
        self.position
    }

    /// Its velocity in the ground plane, m/s.
    pub fn velocity(&self) -> [f32; 2] {
        [
            self.heading[0] * self.speed_mps,
            self.heading[1] * self.speed_mps,
        ]
    }

    /// Whether it stands still: no speed on the ground plane, not in a jump and not in a dodge
    /// (a fall moves it only up and down).
    pub fn is_resting(&self) -> bool {
        self.speed_mps == 0.0 && !self.is_jumping() && self.dodge.is_none()
    }

    /// The direction of its motion (a unit vector in the ground plane).
    pub const fn heading(&self) -> [f32; 2] {
        self.heading
    }

    /// How it holds itself.
    pub const fn posture(&self) -> Posture {
        self.posture
    }

    /// Whether it is in a jump (the client's camera does not follow a jump up and down).
    pub const fn is_jumping(&self) -> bool {
        matches!(
            self.vertical,
            Vertical::WindUp(_) | Vertical::Jumping { .. }
        )
    }

    /// Whether it stands on a floor: not in a jump and not falling.
    pub const fn is_grounded(&self) -> bool {
        matches!(self.vertical, Vertical::Grounded)
    }

    /// Its equip load category changes to `load` (the server weighed its equipped items).
    pub const fn set_equip_load(&mut self, load: EquipLoad) {
        self.equip_load = load;
    }

    /// A held status effect keeps it from sprinting (`NoSprint`), or no longer does.
    pub const fn set_no_sprint(&mut self, no_sprint: bool) {
        self.no_sprint = no_sprint;
    }

    /// Whether it may sprint: neither overburdened (`SprintDisabled`: `Overburdened`) nor held
    /// by a `NoSprint` effect.
    fn may_sprint(&self) -> bool {
        self.equip_load != EquipLoad::Overburdened && !self.no_sprint
    }

    /// Whether the road boost speeds up its sprint.
    pub fn has_road_boost(&self) -> bool {
        self.road_haste_s > 0.0
    }

    /// Combat (an attack of the player, a block, a dodge, or a hit on it) while the road boost's
    /// action is on ends the boost and keeps it off for [`ROAD_COMBAT_S`]
    /// (`Traversal_Road_Sprint_Speed_Boost_CombatRemoval`: it fires only while
    /// `Traversal_Road_Sprint_Speed_Boost` is installed, on the frame of the hit, `IfDamaged`).
    /// Off the road the boost's last seconds stay.
    pub const fn enter_combat(&mut self) {
        if self.road_boost_on {
            self.road_boost_on = false;
            self.road_haste_s = 0.0;
            self.combat_s = ROAD_COMBAT_S;
        }
    }

    /// Puts it at `at`, keeping its motion (the camera check found it elsewhere in a jump).
    pub const fn place(&mut self, at: Position) {
        self.position = at;
    }

    /// Puts it on a floor at `at`, keeping its speed (the camera check found its floor).
    pub const fn land_at(&mut self, at: Position) {
        self.position = at;
        self.vertical = Vertical::Grounded;
    }

    /// Lowers its speed to at most `speed_mps` (the client's body went slower than the keys).
    pub const fn slow_to(&mut self, speed_mps: f32) {
        if speed_mps < self.speed_mps {
            self.speed_mps = speed_mps;
        }
    }

    /// Puts it at `at` standing on its floor (a server move: a spawn, a teleport, a push).
    pub const fn stand_at(&mut self, at: Position) {
        self.position = at;
        self.speed_mps = 0.0;
        self.vertical = Vertical::Grounded;
        self.dodge = None;
    }

    /// Puts it at `at` falling, keeping its speed (the camera check found it past an edge).
    pub const fn fall_from(&mut self, at: Position) {
        self.position = at;
        self.vertical = Vertical::Falling(0.0);
    }

    /// Puts it at `at` in the air: from there it falls to the floor it finds (a server move that
    /// drops the player: retail's move into the tutorial sea placed it 7.6 m above the water, and
    /// it fell).
    pub const fn drop_at(&mut self, at: Position) {
        self.position = at;
        self.speed_mps = 0.0;
        self.vertical = Vertical::Falling(0.0);
        self.dodge = None;
    }

    /// Moves it by one input record over `ground` with the sea at `sea_level_m` (the water of
    /// `ground` where it knows its water: [`Ground::water_surface`]); gives its motion in the
    /// ground plane.
    pub fn step(
        &mut self,
        input: CharacterInput,
        ground: &(impl Ground + ?Sized),
        sea_level_m: f32,
    ) -> [f32; 2] {
        self.advance(input, None, ground, sea_level_m)
    }

    /// Moves it by one input record whose motion an action's animation drives (an attack's root
    /// motion: `AnimationDrivenMotion`), `driven` m on the ground plane, over the walls and floors
    /// of `ground` as a key step goes; the keys start no dodge and no jump. Gives its motion.
    pub fn step_driven(
        &mut self,
        input: CharacterInput,
        driven: [f32; 2],
        ground: &(impl Ground + ?Sized),
        sea_level_m: f32,
    ) -> [f32; 2] {
        self.advance(input, Some(driven), ground, sea_level_m)
    }

    /// One input record: by the keys, or by `driven` when an action's animation drives it.
    fn advance(
        &mut self,
        input: CharacterInput,
        driven: Option<[f32; 2]>,
        ground: &(impl Ground + ?Sized),
        sea_level_m: f32,
    ) -> [f32; 2] {
        let CharacterInput { intent, yaw, dt_s } = input;
        let intent = if self.mount.is_some() {
            intent.mounted()
        } else {
            intent
        };
        if intent.walk_toggle {
            self.walking = !self.walking;
        }
        let surface = ground.water_surface(self.position.x, self.position.y, sea_level_m);
        // Where no water is, no floor of water holds the body.
        let swim = surface.map_or(f32::NEG_INFINITY, swim_floor);
        let wading = self.is_wading(surface);
        self.update_mount_pace(ground, surface);
        let posture = self.posture;
        self.change_posture(intent, wading);
        if self.posture != posture {
            self.posture_change_s = POSTURE_CHANGE_S
                .iter()
                .find(|(from, to, _)| *from == posture && *to == self.posture)
                .map_or(0.0, |(_, _, seconds)| *seconds);
        }
        let dodge_pressed = intent.dodge && !self.dodge_held;
        self.dodge_held = intent.dodge;
        if self.swimming && dodge_pressed {
            self.dash_s = SWIM_DASH_S;
        }
        if dodge_pressed || intent.block {
            self.enter_combat();
        }
        let can_dodge = self.equip_load != EquipLoad::Overburdened
            && self.is_grounded()
            && !self.swimming
            && !wading;
        if driven.is_none() && dodge_pressed && self.dodge.is_none() && can_dodge {
            self.dodge = Some(self.dodge_of(intent, yaw));
        }
        // A record is in the dash when its middle is.
        let dashing = self.swimming && self.dash_s > dt_s / 2.0;
        self.dash_s = (self.dash_s - dt_s).max(0.0);
        let distance = match (driven, self.dodge) {
            (Some(motion), _) => self.driven_step(motion, dt_s),
            (None, Some(dodge)) => self.dodge_step(dodge, intent, dt_s),
            (None, None) => self.key_step(intent, yaw, dt_s, dashing, wading),
        };
        self.update_road_boost(ground, dt_s);
        if driven.is_none() {
            self.start_jump(intent.jump, self.swimming || wading);
        }
        let start = self.position;
        let free = Position {
            x: start.x + self.heading[0] * distance,
            y: start.y + self.heading[1] * distance,
            z: start.z,
        };
        let mut next = stop_at_walls(ground, start, free);
        if driven.is_some() && next != free {
            // An attack's motion halts on what it meets (`SetHaltMotionOnCollision`, the attack
            // actions of `playeractions_attack.actionlist`: `MaxAngleToHaltMotion` 170).
            next = Position { z: free.z, ..start };
            self.speed_mps = 0.0;
        } else if next != free {
            // The wall takes the motion into it: the body goes on at the speed it kept.
            let kept = start.horizontal_distance(next);
            if kept > f32::EPSILON {
                self.heading = [(next.x - start.x) / kept, (next.y - start.y) / kept];
            }
            self.speed_mps = kept / dt_s;
        }
        let distance = start.horizontal_distance(next);
        match self.vertical {
            Vertical::Grounded => self.walk(&mut next, distance, ground, swim),
            Vertical::WindUp(left) => {
                next = start;
                self.vertical = match left.saturating_sub(1) {
                    0 => Vertical::Jumping(JUMP_SPEED_MPS),
                    left => Vertical::WindUp(left),
                };
            }
            Vertical::Jumping(rise) => {
                self.vertical = self
                    .fly(&mut next, rise, GRAVITY_MPS2, dt_s, ground)
                    .map_or(Vertical::Grounded, Vertical::Jumping);
            }
            Vertical::Falling(rise) => {
                self.vertical = self
                    .fly(&mut next, rise, FALL_GRAVITY_MPS2, dt_s, ground)
                    .map_or(Vertical::Grounded, Vertical::Falling);
            }
        }
        if next.z < swim {
            next.z = swim;
            self.vertical = Vertical::Grounded;
        }
        self.swimming = next.z <= swim;
        if !self.swimming {
            self.dash_s = 0.0;
        }
        self.position = next;
        [next.x - start.x, next.y - start.y]
    }

    /// Whether it wades: on a floor out of the swim, its feet at least [`WADE_DEPTH_M`] under
    /// the water `surface` there.
    fn is_wading(&self, surface: Option<f32>) -> bool {
        !self.swimming
            && self.is_grounded()
            && surface.is_some_and(|surface| surface - self.position.z >= WADE_DEPTH_M)
    }

    /// The posture after the record's toggles (`ToggleCrouch`, `ToggleProne`: no change while
    /// blocking; no prone in wading water) and the water (`ToggleCrouch` fires by itself for a
    /// prone body in water deeper than very shallow and for a crouching one deeper than
    /// shallow: the prone body crouches, the crouching swimmer stands).
    fn change_posture(&mut self, intent: Intent, wading: bool) {
        // No toggle while blocking, nor before the posture change's marker (`ToggleCrouch` and
        // `ToggleProne`: `CanTransition`, or no `Idle_PostureTransition` playing).
        if !intent.block && self.posture_change_s <= 0.0 {
            if intent.crouch_toggle {
                self.posture = self.posture.crouch_toggled();
            }
            if intent.prone_toggle && !wading && !self.swimming {
                self.posture = self.posture.prone_toggled();
            }
        }
        if self.swimming {
            self.posture = Posture::Stand;
        } else if wading && self.posture == Posture::Prone {
            self.posture = Posture::Crouch;
        }
    }

    /// The dodge of a press with the keys of `intent` and camera yaw `yaw`
    /// (`playeractions_dodge.actionlist`: forward along the keys; back with the back key or with
    /// no key, straight back from the camera without one).
    fn dodge_of(&self, intent: Intent, yaw: f32) -> Dodge {
        let (forward, _) = yaw_axes(yaw);
        let back = intent.forward < 0 || !intent.moves();
        let direction = key_direction(intent, yaw).unwrap_or([-forward[0], -forward[1]]);
        Dodge {
            root: DodgeRoot::of(self.equip_load, back),
            direction,
            at_s: 0.0,
        }
    }

    /// One record of the dodge `dodge`: the clip's distance in the record. After
    /// [`DODGE_CONTROL_S`] a movement key ends it and the keys move the body on from the
    /// dodge's speed; without one the clip runs to its end.
    fn dodge_step(&mut self, dodge: Dodge, intent: Intent, dt_s: f32) -> f32 {
        let end_s = dodge.at_s + dt_s;
        let distance = dodge.root.distance(end_s) - dodge.root.distance(dodge.at_s);
        self.heading = dodge.direction;
        self.speed_mps = distance / dt_s;
        let taken_over = end_s >= DODGE_CONTROL_S && intent.moves();
        self.dodge = (!taken_over && end_s < dodge.root.length_s()).then_some(Dodge {
            at_s: end_s,
            ..dodge
        });
        distance
    }

    /// One record of an animation's motion `motion` (m on the ground plane): its heading and
    /// speed; gives the distance.
    fn driven_step(&mut self, motion: [f32; 2], dt_s: f32) -> f32 {
        let distance = motion[0].hypot(motion[1]);
        if distance > f32::EPSILON {
            self.heading = [motion[0] / distance, motion[1] / distance];
        }
        self.speed_mps = distance / dt_s;
        self.sprint_s = 0.0;
        distance
    }

    /// One record of the keys' motion: the new speed and heading; gives the distance.
    fn key_step(
        &mut self,
        intent: Intent,
        yaw: f32,
        dt_s: f32,
        dashing: bool,
        wading: bool,
    ) -> f32 {
        if self.posture_change_s > 0.0 {
            // The posture change holds the body: it slows in its own direction.
            self.posture_change_s = (self.posture_change_s - dt_s).max(0.0);
            self.speed_mps = (self.speed_mps - POSTURE_CHANGE_DECELERATION_MPS2 * dt_s).max(0.0);
            self.sprint_s = 0.0;
            return self.speed_mps * dt_s;
        }
        let heading = key_direction(intent, yaw);
        self.heading = heading.unwrap_or(self.heading);
        let pace = self.pace(intent, dashing, wading);
        let target = if heading.is_some() {
            pace.speed_mps
        } else {
            0.0
        };
        self.speed_mps = if dashing && heading.is_some() {
            // The dash takes its speed at once (the dodge inherits it).
            target
        } else if target >= self.speed_mps {
            (self.speed_mps + pace.acceleration_mps2 * dt_s).min(target)
        } else {
            (self.speed_mps - pace.deceleration_mps2 * dt_s).max(target)
        };
        let travel_sprint = heading.is_some()
            && self.posture == Posture::Stand
            && !(dashing || self.swimming || wading || intent.block || intent.walk)
            && !self.walking
            && self.may_sprint()
            && intent.forward >= 0;
        if travel_sprint {
            self.sprint_s += dt_s;
        } else if self.is_grounded() {
            self.sprint_s = 0.0;
        }
        self.speed_mps * dt_s
    }

    /// The speed its keys ask for in its mode now, and how fast it gets there; `dashing` in a
    /// swim dash, `wading` in shallow water. The order is the action lists' (`Wade` first, then
    /// `Block_Nav_Speed`, the walk rows, the posture rows).
    fn pace(&self, intent: Intent, dashing: bool, wading: bool) -> Pace {
        if let Some(mounted) = &self.mount {
            return mounted.pace(intent);
        }
        let back = intent.forward < 0;
        // A body that may not sprint ([`Self::may_sprint`]) runs.
        let running = self.walking || !self.may_sprint();
        let slow = back || running || intent.walk || intent.block;
        if dashing {
            return Pace::even(SWIM_DASH_SPEED_MPS, SWIM_DECELERATION_MPS2);
        }
        if self.swimming {
            return Pace {
                speed_mps: SWIM_SPEED_MPS,
                acceleration_mps2: SWIM_ACCELERATION_MPS2,
                deceleration_mps2: SWIM_DECELERATION_MPS2,
            };
        }
        if wading {
            return match (slow, self.posture) {
                (true, _) => Pace::even(WADE_SLOW_SPEED_MPS, WADE_SLOW_ACCELERATION_MPS2),
                (false, Posture::Stand) => Pace::even(WADE_SPEED_MPS, ACCELERATION_MPS2),
                (false, Posture::Crouch | Posture::Prone) => {
                    Pace::even(WADE_CROUCH_SPEED_MPS, ACCELERATION_MPS2)
                }
            };
        }
        if intent.block {
            let speed = if intent.forward > 0 {
                BLOCK_SPEED_MPS
            } else {
                BLOCK_BACK_SPEED_MPS
            };
            return Pace::even(speed, BLOCK_ACCELERATION_MPS2);
        }
        let prone_slow = |speed_mps| Pace {
            speed_mps,
            acceleration_mps2: PRONE_SLOW_ACCELERATION_MPS2,
            deceleration_mps2: PRONE_SLOW_DECELERATION_MPS2,
        };
        match self.posture {
            Posture::Stand if intent.walk => Pace::even(WALK_SPEED_MPS, WALK_ACCELERATION_MPS2),
            Posture::Crouch if intent.walk => {
                Pace::even(CROUCH_WALK_SPEED_MPS, CROUCH_WALK_ACCELERATION_MPS2)
            }
            Posture::Prone if intent.walk => prone_slow(PRONE_WALK_SPEED_MPS),
            Posture::Stand if running => Pace::even(RUN_SPEED_MPS, ACCELERATION_MPS2),
            Posture::Crouch if running => {
                Pace::even(CROUCH_RUN_SPEED_MPS, CROUCH_RUN_ACCELERATION_MPS2)
            }
            Posture::Prone if running => prone_slow(PRONE_RUN_SPEED_MPS),
            Posture::Stand if back => Pace::even(BACK_SPEED_MPS, ACCELERATION_MPS2),
            Posture::Crouch if back => {
                Pace::even(CROUCH_BACK_SPEED_MPS, CROUCH_BACK_ACCELERATION_MPS2)
            }
            Posture::Prone if back => Pace::even(PRONE_BACK_SPEED_MPS, ACCELERATION_MPS2),
            Posture::Stand => Pace {
                speed_mps: SPRINT_SPEED_MPS * self.sprint_speed_mod(),
                acceleration_mps2: ACCELERATION_MPS2,
                deceleration_mps2: DECELERATION_MPS2,
            },
            Posture::Crouch => Pace::even(CROUCH_SPEED_MPS, ACCELERATION_MPS2),
            Posture::Prone => Pace::even(PRONE_SPEED_MPS, ACCELERATION_MPS2),
        }
    }

    /// The sprint speed's factor: 1 plus the road boost's `SprintSpeedMod` while it lasts.
    fn sprint_speed_mod(&self) -> f32 {
        if self.has_road_boost() {
            1.0 + ROAD_SPRINT_SPEED_MOD
        } else {
            1.0
        }
    }

    /// The road boost after a record of `dt_s` (`Traversal_Road_Sprint_Speed_Boost`): on a road
    /// of `ground` after [`ROAD_SPRINT_DELAY_S`] of standing sprint and out of combat it lasts
    /// [`ROAD_HASTE_S`] from now; else it runs down.
    fn update_road_boost(&mut self, ground: &(impl Ground + ?Sized), dt_s: f32) {
        self.combat_s = (self.combat_s - dt_s).max(0.0);
        let on_road = self.on_road(ground);
        self.road_boost_on =
            on_road && self.sprint_s >= ROAD_SPRINT_DELAY_S && self.combat_s == 0.0;
        if self.road_boost_on {
            self.road_haste_s = ROAD_HASTE_S;
        } else {
            self.road_haste_s = (self.road_haste_s - dt_s).max(0.0);
        }
    }

    /// Whether it is on a road of `ground`: near the terrain where the road share is at least
    /// [`ROAD_SHARE_MIN`].
    fn on_road(&self, ground: &(impl Ground + ?Sized)) -> bool {
        let Position { x, y, z } = self.position;
        let near_terrain = ground
            .height_at(x, y)
            .is_some_and(|terrain| (z - terrain).abs() <= ROAD_HEIGHT_THRESHOLD_M);
        near_terrain && ground.road_share(x, y) >= ROAD_SHARE_MIN
    }

    /// The speeds of the surface under its mount, with the water `surface` there: deep water
    /// while it swims, else the depth over its feet ([`WaterDepth`]), else a road
    /// ([`Self::on_road`], Inferred: the client's road surface, as for the sprint's road boost)
    /// or off the roads.
    fn update_mount_pace(&mut self, ground: &(impl Ground + ?Sized), surface: Option<f32>) {
        let Some(mounted) = self.mount else {
            return;
        };
        let depth = surface.map_or(WaterDepth::None, |surface| {
            WaterDepth::of(surface - self.position.z)
        });
        let paces = mounted.paces;
        let pace = match depth {
            _ if self.swimming => paces.deep_water,
            WaterDepth::Shallow => paces.shallow_water,
            WaterDepth::VeryShallow => paces.very_shallow_water,
            WaterDepth::None if self.on_road(ground) => paces.on_road,
            WaterDepth::None => paces.off_road,
        };
        self.mount = Some(Mounted { pace, ..mounted });
    }

    /// Starts a jump on a new press of the jump key while it stands on a floor out of water
    /// `deep_water` (wading or swimming).
    fn start_jump(&mut self, held: bool, deep_water: bool) {
        let pressed = held && !self.jump_held;
        self.jump_held = held;
        if pressed && self.vertical == Vertical::Grounded && !deep_water {
            self.vertical = if self.speed_mps == 0.0 {
                Vertical::WindUp(STANDING_JUMP_WIND_UP)
            } else {
                Vertical::Jumping(JUMP_SPEED_MPS)
            };
        }
    }

    /// A walk to `next` on the floor (the floor of `ground`, or the water at `swim` where that is
    /// higher): along a floor within a step, the distance is along its slope; a floor further
    /// down starts a fall; a floor further up is an obstacle and the body keeps its height; with
    /// no floor known (no height data) it keeps its height.
    ///
    /// On the terrain the slope is the floor's rise in the record (module comment). A structure's
    /// physics mesh is no smooth slope (a rock pile's triangles rise and fall by up to a step from
    /// one record to the next), so its slope is the rise across the body's capsule
    /// ([`PLAYER_RADIUS_M`] ahead and behind), and only a climb shortens the step. Replay of the
    /// retail captures with the level collision (level-collision note 2026-09-20): with the
    /// per-record rise on structures too, a median of 0.59 m from retail's own place in the
    /// tutorial; with no slope on structures 0.221 m and 1,370 pull-backs; with the climb across
    /// the capsule 0.226 m and 1,314 pull-backs (open world 1,611 and 1,580, 1,495 and 1,460; a
    /// climb or descent across the capsule: 1,283 in the tutorial but medians up to 0.026 m
    /// worse).
    fn walk(
        &mut self,
        next: &mut Position,
        distance: f32,
        ground: &(impl Ground + ?Sized),
        swim: f32,
    ) {
        let start = self.position;
        let floor_at = |at: &Position| {
            ground
                .floor_at(at.x, at.y, at.z)
                .map(|floor| floor.max(swim))
        };
        let Some(floor) = floor_at(next) else {
            return;
        };
        let rise = floor - next.z;
        if rise > STEP_UP_M {
            return;
        }
        if -rise > STEP_DOWN_M {
            self.vertical = Vertical::Falling(0.0);
            return;
        }
        let on_terrain = ground
            .height_at(next.x, next.y)
            .is_some_and(|terrain| (terrain - floor).abs() <= f32::EPSILON);
        let slope_rise = if on_terrain {
            Some(rise)
        } else {
            let floor_along = |offset: f32| {
                ground
                    .floor_at(
                        next.x + self.heading[0] * offset,
                        next.y + self.heading[1] * offset,
                        floor,
                    )
                    .map(|floor| floor.max(swim))
            };
            floor_along(PLAYER_RADIUS_M)
                .zip(floor_along(-PLAYER_RADIUS_M))
                .map(|(ahead, behind)| (ahead - behind) / (2.0 * PLAYER_RADIUS_M) * distance)
                .filter(|climb| *climb > 0.0)
        };
        if let Some(rise) = slope_rise.filter(|_| distance > 0.0) {
            let along = distance / distance.hypot(rise);
            next.x = start.x + (next.x - start.x) * along;
            next.y = start.y + (next.y - start.y) * along;
        }
        next.z = floor_at(next).unwrap_or(floor);
    }

    /// One record in the air: up by `rise`, pulled down by `gravity`, and onto the floor of
    /// `ground` when it reaches it. Where no floor is known at all (no height data), it stays at
    /// its height: nothing says it may fall. Gives the rise after the record, or `None` when it
    /// landed.
    fn fly(
        &self,
        next: &mut Position,
        rise: f32,
        gravity: f32,
        dt_s: f32,
        ground: &(impl Ground + ?Sized),
    ) -> Option<f32> {
        next.z += rise * dt_s - gravity * dt_s * dt_s / 2.0;
        match ground.floor_at(next.x, next.y, self.position.z) {
            Some(floor) if next.z <= floor => {
                next.z = floor;
                None
            }
            Some(_) => Some(rise - gravity * dt_s),
            None => {
                next.z = self.position.z;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One input record at 30 Hz.
    const DT: f32 = 1.0 / 30.0;
    const SEA: f32 = -100.0;
    const CLOSE: f32 = 1e-4;

    /// Flat ground at height 0.
    struct Flat;

    impl Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(0.0)
        }
    }

    /// Ground that rises 1 m per metre east.
    struct Ramp;

    impl Ground for Ramp {
        fn height_at(&self, x: f32, _: f32) -> Option<f32> {
            Some(x)
        }
    }

    fn origin() -> Position {
        Position {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }
    }

    fn input(forward: i8, right: i8) -> CharacterInput {
        CharacterInput {
            intent: Intent {
                forward,
                right,
                ..Intent::default()
            },
            yaw: 0.0,
            dt_s: DT,
        }
    }

    fn run(character: &mut Character, records: usize, forward: i8, right: i8) {
        for _ in 0..records {
            character.step(input(forward, right), &Flat, SEA);
        }
    }

    #[test]
    fn the_keys_move_it_along_the_camera_yaw_at_the_action_list_speeds() {
        // A second of each key: the speed reaches its top within 0.28 s.
        let cases = [
            (1, 0, [0.0, 1.0], SPRINT_SPEED_MPS),
            (-1, 0, [0.0, -1.0], BACK_SPEED_MPS),
            (0, 1, [1.0, 0.0], SPRINT_SPEED_MPS),
            (0, -1, [-1.0, 0.0], SPRINT_SPEED_MPS),
        ];
        for (forward, right, direction, speed) in cases {
            let mut character = Character::new(origin());
            run(&mut character, 30, forward, right);
            let velocity = character.velocity();
            assert!(
                (velocity[0] - direction[0] * speed).abs() < CLOSE,
                "{velocity:?}"
            );
            assert!(
                (velocity[1] - direction[1] * speed).abs() < CLOSE,
                "{velocity:?}"
            );
        }
        // West of north at yaw 90°: forward is -x.
        let mut west = Character::new(origin());
        for _ in 0..30 {
            west.step(
                CharacterInput {
                    yaw: std::f32::consts::FRAC_PI_2,
                    ..input(1, 0)
                },
                &Flat,
                SEA,
            );
        }
        assert!(west.position().x < -4.0 && west.position().y.abs() < CLOSE);
    }

    #[test]
    fn a_diagonal_turns_at_once_and_keeps_the_speed() {
        let mut character = Character::new(origin());
        run(&mut character, 30, 1, 0);
        character.step(input(1, 1), &Flat, SEA);
        let [x, y] = character.velocity();
        assert!((x - y).abs() < CLOSE, "45°: {x} {y}");
        assert!((x.hypot(y) - SPRINT_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn the_speed_ramps_up_and_down_at_twenty() {
        let mut character = Character::new(origin());
        character.step(input(1, 0), &Flat, SEA);
        assert!((character.velocity()[1] - ACCELERATION_MPS2 * DT).abs() < CLOSE);
        run(&mut character, 30, 1, 0);
        character.step(input(0, 0), &Flat, SEA);
        assert!(
            (character.velocity()[1] - (SPRINT_SPEED_MPS - DECELERATION_MPS2 * DT)).abs() < CLOSE
        );
        run(&mut character, 30, 0, 0);
        assert!(character.is_resting());
    }

    #[test]
    fn the_walk_toggle_switches_to_the_run_speed_in_every_direction() {
        let mut character = Character::new(origin());
        character.step(
            CharacterInput {
                intent: Intent {
                    forward: 1,
                    walk_toggle: true,
                    ..Intent::default()
                },
                ..input(1, 0)
            },
            &Flat,
            SEA,
        );
        run(&mut character, 30, 1, 0);
        assert!((character.velocity()[1] - RUN_SPEED_MPS).abs() < CLOSE);
        run(&mut character, 30, -1, 0);
        assert!((character.velocity()[1] + RUN_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn on_a_slope_the_speed_is_along_the_ground() {
        let mut character = Character::new(origin());
        // East up a 45° ramp.
        for _ in 0..60 {
            character.step(input(0, 1), &Ramp, SEA);
        }
        let before = character.position();
        character.step(input(0, 1), &Ramp, SEA);
        let after = character.position();
        let along = (after.x - before.x).hypot(after.z - before.z);
        assert!((along - SPRINT_SPEED_MPS * DT).abs() < 1e-3, "{along}");
        assert!((after.z - after.x).abs() < CLOSE, "on the ramp");
    }

    #[test]
    fn on_a_structure_a_climb_is_along_it_and_a_descent_is_not() {
        /// A 45° rock ramp up to the east, 50 m over the terrain.
        struct RockRamp;
        impl Ground for RockRamp {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                Some(-50.0)
            }
            fn floor_at(&self, x: f32, _: f32, z: f32) -> Option<f32> {
                Some(if x <= z + 1.0 { x } else { -50.0 })
            }
        }
        let step = |character: &mut Character, right: i8| {
            let before = character.position();
            character.step(input(0, right), &RockRamp, SEA);
            (character.position().x - before.x).abs()
        };
        let mut character = Character::new(origin());
        for _ in 0..60 {
            step(&mut character, 1);
        }
        let climb = step(&mut character, 1);
        assert!(
            (climb - SPRINT_SPEED_MPS * DT / 2.0_f32.sqrt()).abs() < 1e-3,
            "{climb}"
        );
        for _ in 0..60 {
            step(&mut character, -1);
        }
        let descent = step(&mut character, -1);
        assert!((descent - SPRINT_SPEED_MPS * DT).abs() < 1e-3, "{descent}");
        assert!(
            (character.position().z - character.position().x).abs() < CLOSE,
            "on the ramp"
        );
    }

    #[test]
    fn a_standing_jump_waits_three_records_then_rises_and_lands() {
        let mut character = Character::new(origin());
        let jump = CharacterInput {
            intent: Intent {
                jump: true,
                ..Intent::default()
            },
            ..input(0, 0)
        };
        for _ in 0..STANDING_JUMP_WIND_UP {
            character.step(jump, &Flat, SEA);
            assert!(character.position().z.abs() < CLOSE, "wind-up");
        }
        let mut peak: f32 = 0.0;
        for _ in 0..40 {
            character.step(jump, &Flat, SEA);
            peak = peak.max(character.position().z);
        }
        // The retail standing jump peaked at 1.01 m.
        assert!((peak - 1.0).abs() < 0.05, "{peak}");
        assert!(
            character.is_resting(),
            "landed, and holding the key does not jump again"
        );
    }

    #[test]
    fn a_floor_far_below_is_a_fall_and_one_far_above_is_not_climbed() {
        struct Cliff;
        impl Ground for Cliff {
            fn height_at(&self, x: f32, _: f32) -> Option<f32> {
                Some(if x < 0.1 { 0.0 } else { -10.0 })
            }
        }
        let mut character = Character::new(origin());
        run(&mut character, 1, 0, 1);
        for _ in 0..30 {
            character.step(input(0, 1), &Cliff, SEA);
        }
        assert!(
            character.position().z < 0.0 && character.position().z > -10.0,
            "falling"
        );
        for _ in 0..60 {
            character.step(input(0, 0), &Cliff, SEA);
        }
        assert!((character.position().z + 10.0).abs() < CLOSE, "landed");
        struct Wall;
        impl Ground for Wall {
            fn height_at(&self, x: f32, _: f32) -> Option<f32> {
                Some(if x < 0.1 { 0.0 } else { 3.0 })
            }
        }
        let mut walker = Character::new(origin());
        run(&mut walker, 30, 0, 1);
        for _ in 0..30 {
            walker.step(input(0, 1), &Wall, SEA);
        }
        assert!(walker.position().z.abs() < CLOSE, "keeps its height");
    }

    #[test]
    fn in_deep_water_it_swims_below_the_sea() {
        const TUTORIAL_SEA: f32 = 20.0;
        struct SeaBed;
        impl Ground for SeaBed {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                Some(5.0)
            }
        }
        let mut character = Character::new(Position {
            x: 0.0,
            y: 0.0,
            z: TUTORIAL_SEA - SWIM_DEPTH_M,
        });
        run(&mut character, 1, 0, 0);
        for _ in 0..30 {
            character.step(input(1, 0), &SeaBed, TUTORIAL_SEA);
        }
        assert!((character.position().z - (TUTORIAL_SEA - SWIM_DEPTH_M)).abs() < CLOSE);
    }

    fn moving(forward: i8, right: i8, block: bool, dodge: bool) -> CharacterInput {
        CharacterInput {
            intent: Intent {
                block,
                dodge,
                ..input(forward, right).intent
            },
            ..input(forward, right)
        }
    }

    #[test]
    fn blocking_moves_at_the_block_speeds() {
        let mut character = Character::new(origin());
        character.step(moving(1, 0, true, false), &Flat, SEA);
        assert!(
            (character.velocity()[1] - BLOCK_ACCELERATION_MPS2 * DT).abs() < CLOSE,
            "the block ramp"
        );
        for _ in 0..30 {
            character.step(moving(1, 0, true, false), &Flat, SEA);
        }
        assert!((character.velocity()[1] - BLOCK_SPEED_MPS).abs() < CLOSE);
        for _ in 0..30 {
            character.step(moving(-1, 0, true, false), &Flat, SEA);
        }
        assert!((character.velocity()[1] + BLOCK_BACK_SPEED_MPS).abs() < CLOSE);
        for _ in 0..30 {
            character.step(moving(0, 1, true, false), &Flat, SEA);
        }
        assert!((character.velocity()[0] - BLOCK_BACK_SPEED_MPS).abs() < CLOSE);
    }

    /// Water 5 m deep everywhere, the sea at the tutorial's 20 m.
    const TUTORIAL_SEA: f32 = 20.0;
    struct SeaBed;
    impl Ground for SeaBed {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(5.0)
        }
    }

    fn swimmer() -> Character {
        let mut character = Character::new(Position {
            x: 0.0,
            y: 0.0,
            z: TUTORIAL_SEA - SWIM_DEPTH_M,
        });
        character.step(input(0, 0), &SeaBed, TUTORIAL_SEA);
        character
    }

    #[test]
    fn a_swimmer_ramps_to_the_swim_speed_and_dashes_on_a_dodge_press() {
        let mut character = swimmer();
        character.step(input(1, 0), &SeaBed, TUTORIAL_SEA);
        assert!((character.velocity()[1] - SWIM_ACCELERATION_MPS2 * DT).abs() < CLOSE);
        for _ in 0..60 {
            character.step(input(1, 0), &SeaBed, TUTORIAL_SEA);
        }
        assert!((character.velocity()[1] - SWIM_SPEED_MPS).abs() < CLOSE);
        character.step(moving(1, 0, false, true), &SeaBed, TUTORIAL_SEA);
        assert!(
            (character.velocity()[1] - SWIM_DASH_SPEED_MPS).abs() < CLOSE,
            "the dash at once"
        );
        let mut records = 1;
        loop {
            character.step(moving(1, 0, false, true), &SeaBed, TUTORIAL_SEA);
            if (character.velocity()[1] - SWIM_DASH_SPEED_MPS).abs() >= CLOSE {
                break;
            }
            records += 1;
        }
        // 37 records of 1/30 s, held to the end.
        assert_eq!(records, 37);
        assert!(
            (character.velocity()[1] - SWIM_SPEED_MPS).abs() < CLOSE,
            "back to the swim speed with the key held"
        );
    }

    fn keyed(forward: i8, right: i8, change: impl Fn(&mut Intent)) -> CharacterInput {
        let mut keys = input(forward, right);
        change(&mut keys.intent);
        keys
    }

    /// The speed along the ground plane, m/s.
    fn speed(character: &Character) -> f32 {
        let [x, y] = character.velocity();
        x.hypot(y)
    }

    /// Records of 1/30 s in the dodge's hold ([`DODGE_CONTROL_S`]).
    const DODGE_RECORDS: usize = 15;
    /// Records after the hold until the run is back at its speed (retail: 2 to 3; one more for
    /// the record in which the hold ends).
    const RUN_BACK_RECORDS: usize = 4;
    /// Records of 1/30 s of the back dodge clip (frame 38).
    const BACK_CLIP_RECORDS: usize = 38;

    #[test]
    fn a_land_dodge_moves_the_body_by_the_clip_root_then_the_run_takes_over() {
        // Retail newchar: forward dodges with the key held were 2.61 to 2.69 m from the press at
        // 0.5 s, and the run was back at 5.4 to 5.5 m/s 2 to 3 records later.
        const RETAIL_AT_HOLD_END_M: [f32; 2] = [2.61, 2.69];
        let mut character = Character::new(origin());
        character.set_equip_load(EquipLoad::Slow);
        run(&mut character, 30, 1, 0);
        let pressed_at = character.position().y;
        let dodge = keyed(1, 0, |keys| keys.dodge = true);
        for _ in 0..DODGE_RECORDS {
            character.step(dodge, &Flat, SEA);
        }
        let gone = character.position().y - pressed_at;
        assert!(
            (RETAIL_AT_HOLD_END_M[0]..=RETAIL_AT_HOLD_END_M[1]).contains(&gone),
            "{gone}"
        );
        assert!(!character.is_resting());
        for _ in 0..RUN_BACK_RECORDS {
            character.step(dodge, &Flat, SEA);
        }
        assert!(
            (speed(&character) - SPRINT_SPEED_MPS).abs() < CLOSE,
            "the run"
        );
    }

    #[test]
    fn a_dodge_without_keys_goes_back_from_the_camera_for_the_whole_clip() {
        /// Past the end of every clip, s.
        const AFTER_CLIP_S: f32 = 2.0;
        let mut character = Character::new(origin());
        character.set_equip_load(EquipLoad::Slow);
        let dodge = keyed(0, 0, |keys| keys.dodge = true);
        for _ in 0..BACK_CLIP_RECORDS {
            character.step(dodge, &Flat, SEA);
        }
        // The root times the multiplier to the marker's end, the root alone after it.
        let back = DodgeRoot::of(EquipLoad::Slow, true).distance(AFTER_CLIP_S);
        let root_end = SLOW_DODGE_BACK_ROOT[SLOW_DODGE_BACK_ROOT.len() - 1].1;
        assert!(back > root_end && back < root_end * SLOW_DODGE_TRANSLATION);
        assert!(
            (character.position().y + back).abs() < 1e-3,
            "{:?}",
            character.position()
        );
        assert!(character.position().x.abs() < CLOSE);
        character.step(input(0, 0), &Flat, SEA);
        assert!(
            character.is_resting(),
            "the clip ended, holding the key starts no other"
        );
    }

    #[test]
    fn a_normal_load_dodges_the_quick_step_and_an_overburdened_one_neither_dodges_nor_sprints() {
        // Retail `walk_running_crouch_prone` record 3328, a back dodge of the normal class:
        // 3.55 m at 0.5 s.
        const RETAIL_NORMAL_BACK_M: f32 = 3.55;
        const RETAIL_TOLERANCE_M: f32 = 0.06;
        let mut normal = Character::new(origin());
        let dodge = keyed(-1, 0, |keys| keys.dodge = true);
        for _ in 0..DODGE_RECORDS {
            normal.step(dodge, &Flat, SEA);
        }
        let back = -normal.position().y;
        assert!(
            (back - RETAIL_NORMAL_BACK_M).abs() < RETAIL_TOLERANCE_M,
            "{back}"
        );
        let mut fast = Character::new(origin());
        fast.set_equip_load(EquipLoad::Fast);
        for _ in 0..DODGE_RECORDS {
            fast.step(keyed(1, 0, |keys| keys.dodge = true), &Flat, SEA);
        }
        assert!(fast.position().y > back, "the light roll goes farther");
        let mut heavy = Character::new(origin());
        heavy.set_equip_load(EquipLoad::Overburdened);
        assert!((settle(&mut heavy, input(1, 0)) - RUN_SPEED_MPS).abs() < CLOSE);
        heavy.step(keyed(1, 0, |keys| keys.dodge = true), &Flat, SEA);
        assert!((speed(&heavy) - RUN_SPEED_MPS).abs() < CLOSE, "no dodge");
    }

    #[test]
    fn a_no_sprint_effect_makes_the_sprint_a_run_until_it_ends() {
        let mut slowed = Character::new(origin());
        slowed.set_no_sprint(true);
        assert!((settle(&mut slowed, input(1, 0)) - RUN_SPEED_MPS).abs() < CLOSE);
        slowed.set_no_sprint(false);
        assert!((settle(&mut slowed, input(1, 0)) - SPRINT_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn a_dodge_in_wading_water_or_in_the_air_moves_nothing() {
        let mut wader = Character::new(Position {
            z: SHALLOW_FLOOR_M,
            ..origin()
        });
        wader.step(
            keyed(0, 0, |keys| keys.dodge = true),
            &Shallows,
            TUTORIAL_SEA,
        );
        assert!(wader.is_resting());
        let mut jumper = Character::new(origin());
        run(&mut jumper, 30, 1, 0);
        jumper.step(keyed(1, 0, |keys| keys.jump = true), &Flat, SEA);
        jumper.step(keyed(1, 0, |keys| keys.dodge = true), &Flat, SEA);
        assert!(
            (speed(&jumper) - SPRINT_SPEED_MPS).abs() < CLOSE,
            "no dodge"
        );
    }

    /// Records of 1/30 s to reach any speed from any other.
    const SETTLE: usize = 120;

    fn settle(character: &mut Character, keys: CharacterInput) -> f32 {
        for _ in 0..SETTLE {
            character.step(keys, &Flat, SEA);
        }
        speed(character)
    }

    #[test]
    fn the_crouch_and_prone_toggles_pick_the_posture_speeds() {
        let mut character = Character::new(origin());
        character.step(keyed(0, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert_eq!(character.posture(), Posture::Crouch);
        assert!((settle(&mut character, input(1, 0)) - CROUCH_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, input(0, 1)) - CROUCH_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, input(-1, 0)) - CROUCH_BACK_SPEED_MPS).abs() < CLOSE);
        let walk = keyed(1, 0, |keys| keys.walk = true);
        assert!((settle(&mut character, walk) - CROUCH_WALK_SPEED_MPS).abs() < CLOSE);
        character.step(keyed(1, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        assert_eq!(character.posture(), Posture::Prone);
        assert!((settle(&mut character, input(1, 0)) - PRONE_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, input(-1, 0)) - PRONE_BACK_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, walk) - PRONE_WALK_SPEED_MPS).abs() < CLOSE);
        character.step(keyed(1, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert_eq!(character.posture(), Posture::Crouch, "prone to crouch");
        settle(&mut character, input(1, 0));
        character.step(keyed(1, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        assert_eq!(character.posture(), Posture::Prone);
        character.step(keyed(1, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert_eq!(
            character.posture(),
            Posture::Prone,
            "no toggle in the change"
        );
        settle(&mut character, input(1, 0));
        character.step(keyed(1, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        assert_eq!(character.posture(), Posture::Stand, "prone to standing");
        assert!((settle(&mut character, walk) - WALK_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, input(1, 0)) - SPRINT_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn going_prone_from_a_sprint_slows_the_body_to_rest_then_it_crawls() {
        // Retail `walk_running_crouch_prone` record 1855: 1.96 m from the press to rest at 0.63 s.
        const RETAIL_DIVE_M: f32 = 1.96;
        const DIVE_TOLERANCE_M: f32 = 0.25;
        /// Retail's speed at the marker, record 1875: 0.6 m/s.
        const RETAIL_RESUME_MPS: f32 = 0.6;
        /// Records of 1/30 s to the `ResumeSprint` marker of `Prone+FromStand` (0.667 s).
        const PRONE_CHANGE_RECORDS: usize = 20;
        let mut character = Character::new(origin());
        run(&mut character, 30, 1, 0);
        let pressed_at = character.position().y;
        character.step(keyed(1, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        for _ in 1..PRONE_CHANGE_RECORDS {
            character.step(input(1, 0), &Flat, SEA);
        }
        let dive = character.position().y - pressed_at;
        assert!((dive - RETAIL_DIVE_M).abs() < DIVE_TOLERANCE_M, "{dive}");
        assert!(speed(&character) < RETAIL_RESUME_MPS, "near rest");
        assert!((settle(&mut character, input(1, 0)) - PRONE_SPEED_MPS).abs() < CLOSE);
        // From rest with the key held, the body waits for the marker.
        let mut lying = Character::new(origin());
        lying.step(keyed(1, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        for _ in 1..PRONE_CHANGE_RECORDS - 1 {
            lying.step(input(1, 0), &Flat, SEA);
        }
        assert!(lying.position().y.abs() < CLOSE, "still in the change");
        for _ in 0..3 {
            lying.step(input(1, 0), &Flat, SEA);
        }
        assert!(lying.position().y > 0.0, "it crawls");
    }

    #[test]
    fn standing_up_from_a_crouch_has_no_hold() {
        let mut character = Character::new(origin());
        character.step(keyed(0, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert!((settle(&mut character, input(1, 0)) - CROUCH_SPEED_MPS).abs() < CLOSE);
        character.step(keyed(1, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert!(speed(&character) > CROUCH_SPEED_MPS, "it speeds up at once");
    }

    #[test]
    fn the_walk_toggle_in_a_posture_picks_its_run_row_and_block_keeps_the_posture() {
        let mut character = Character::new(origin());
        character.step(keyed(0, 0, |keys| keys.walk_toggle = true), &Flat, SEA);
        character.step(keyed(0, 0, |keys| keys.crouch_toggle = true), &Flat, SEA);
        assert!((settle(&mut character, input(1, 0)) - CROUCH_RUN_SPEED_MPS).abs() < CLOSE);
        assert!((settle(&mut character, input(-1, 0)) - CROUCH_RUN_SPEED_MPS).abs() < CLOSE);
        character.step(keyed(0, 0, |keys| keys.prone_toggle = true), &Flat, SEA);
        assert!((settle(&mut character, input(1, 0)) - PRONE_RUN_SPEED_MPS).abs() < CLOSE);
        let blocked = keyed(0, 0, |keys| {
            keys.block = true;
            keys.crouch_toggle = true;
        });
        character.step(blocked, &Flat, SEA);
        assert_eq!(
            character.posture(),
            Posture::Prone,
            "no toggle while blocking"
        );
    }

    /// The tutorial sea's floor 1 m under the sea east of x 0 and 0.5 m under it west of it.
    struct Shallows;

    /// The feet's depth of [`Shallows`] east and west, m.
    const SHALLOW_FLOOR_M: f32 = TUTORIAL_SEA - 1.0;
    const BEACH_FLOOR_M: f32 = TUTORIAL_SEA - 0.5;

    impl Ground for Shallows {
        fn height_at(&self, x: f32, _: f32) -> Option<f32> {
            Some(if x >= 0.0 {
                SHALLOW_FLOOR_M
            } else {
                BEACH_FLOOR_M
            })
        }
    }

    fn settle_in(character: &mut Character, keys: CharacterInput) -> f32 {
        for _ in 0..SETTLE {
            character.step(keys, &Shallows, TUTORIAL_SEA);
        }
        speed(character)
    }

    #[test]
    fn a_body_in_wading_water_moves_at_the_wade_speeds_and_rises_from_prone() {
        // North along x 10: 1 m deep the whole way.
        let mut character = Character::new(Position {
            x: 10.0,
            y: 0.0,
            z: SHALLOW_FLOOR_M,
        });
        assert!((settle_in(&mut character, input(1, 0)) - WADE_SPEED_MPS).abs() < CLOSE);
        assert!((settle_in(&mut character, input(-1, 0)) - WADE_SLOW_SPEED_MPS).abs() < CLOSE);
        character.step(
            keyed(0, 0, |keys| keys.prone_toggle = true),
            &Shallows,
            TUTORIAL_SEA,
        );
        assert_eq!(
            character.posture(),
            Posture::Stand,
            "no prone in wading water"
        );
        character.step(
            keyed(0, 0, |keys| keys.crouch_toggle = true),
            &Shallows,
            TUTORIAL_SEA,
        );
        assert!((settle_in(&mut character, input(1, 0)) - WADE_CROUCH_SPEED_MPS).abs() < CLOSE);
        // West of x 0 the feet are 0.5 m deep: no wading, and a prone body walks into the deeper
        // water and crouches.
        let mut beach = Character::new(Position {
            x: -10.0,
            y: 0.0,
            z: BEACH_FLOOR_M,
        });
        assert!((settle_in(&mut beach, input(1, 0)) - SPRINT_SPEED_MPS).abs() < CLOSE);
        beach.step(
            keyed(0, 0, |keys| keys.prone_toggle = true),
            &Shallows,
            TUTORIAL_SEA,
        );
        assert_eq!(beach.posture(), Posture::Prone);
        for _ in 0..SETTLE * 4 {
            beach.step(input(0, 1), &Shallows, TUTORIAL_SEA);
        }
        assert!(beach.position().x > 0.0);
        assert_eq!(beach.posture(), Posture::Crouch);
    }

    /// A lake 1 m deep over flat ground at 99 m, its surface at 100 m, far above the sea.
    struct Lake;
    const LAKE_BED_M: f32 = 99.0;
    const LAKE_SURFACE_M: f32 = 100.0;

    impl Ground for Lake {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(LAKE_BED_M)
        }

        fn water_surface(&self, _: f32, _: f32, _: f32) -> Option<f32> {
            Some(LAKE_SURFACE_M)
        }
    }

    #[test]
    fn a_lake_s_surface_makes_the_body_wade_and_a_dry_map_place_does_not() {
        let mut wader = Character::new(Position {
            z: LAKE_BED_M,
            ..origin()
        });
        for _ in 0..SETTLE {
            wader.step(input(1, 0), &Lake, SEA);
        }
        assert!((speed(&wader) - WADE_SPEED_MPS).abs() < CLOSE);
        /// A water map that knows no water here, over ground under the sea's level.
        struct Dry;
        impl Ground for Dry {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                Some(0.0)
            }

            fn water_surface(&self, _: f32, _: f32, _: f32) -> Option<f32> {
                None
            }
        }
        const HIGH_SEA_M: f32 = 10.0;
        let mut walker = Character::new(origin());
        for _ in 0..SETTLE {
            walker.step(input(1, 0), &Dry, HIGH_SEA_M);
        }
        assert!((speed(&walker) - SPRINT_SPEED_MPS).abs() < CLOSE);
        assert!(
            walker.position().z.abs() < CLOSE,
            "on the ground, not swimming"
        );
    }

    /// Flat ground that is road east of x 0.
    struct Road;

    impl Ground for Road {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(0.0)
        }

        fn road_share(&self, x: f32, _: f32) -> f32 {
            if x >= 0.0 { 1.0 } else { 0.0 }
        }
    }

    /// Records of 1/30 s in [`ROAD_SPRINT_DELAY_S`] and [`ROAD_HASTE_S`].
    const ROAD_DELAY_RECORDS: usize = 90;
    /// Records of 1/30 s in [`ROAD_COMBAT_S`].
    const COMBAT_RECORDS: usize = 150;
    /// Records west at the boosted sprint from x 1 to off the road (0.21 m a record).
    const OFF_ROAD_RECORDS: usize = 10;

    #[test]
    fn a_sprint_on_a_road_speeds_up_after_the_delay_and_the_boost_outlasts_the_road() {
        let boosted = SPRINT_SPEED_MPS * (1.0 + ROAD_SPRINT_SPEED_MOD);
        let mut character = Character::new(Position { x: 1.0, ..origin() });
        let north = input(1, 0);
        for _ in 0..ROAD_DELAY_RECORDS - 1 {
            character.step(north, &Road, SEA);
        }
        assert!(!character.has_road_boost(), "not before the delay");
        assert!((speed(&character) - SPRINT_SPEED_MPS).abs() < CLOSE);
        for _ in 0..2 {
            character.step(north, &Road, SEA);
        }
        assert!(character.has_road_boost());
        for _ in 0..3 {
            character.step(north, &Road, SEA);
        }
        assert!(
            (speed(&character) - boosted).abs() < CLOSE,
            "{}",
            speed(&character)
        );
        // West off the road: the boost lasts its 3 s.
        let west = input(0, -1);
        for _ in 0..ROAD_DELAY_RECORDS - 2 {
            character.step(west, &Road, SEA);
        }
        assert!(character.position().x < 0.0);
        assert!((speed(&character) - boosted).abs() < CLOSE);
        for _ in 0..ROAD_DELAY_RECORDS {
            character.step(west, &Road, SEA);
        }
        assert!((speed(&character) - SPRINT_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn combat_ends_the_road_boost_and_keeps_it_off() {
        let mut character = Character::new(Position { x: 1.0, ..origin() });
        let north = input(1, 0);
        for _ in 0..ROAD_DELAY_RECORDS + 1 {
            character.step(north, &Road, SEA);
        }
        assert!(character.has_road_boost());
        character.enter_combat();
        assert!(!character.has_road_boost());
        for _ in 0..COMBAT_RECORDS - 1 {
            character.step(north, &Road, SEA);
        }
        assert!(!character.has_road_boost(), "the combat timer");
        for _ in 0..2 {
            character.step(north, &Road, SEA);
        }
        assert!(
            character.has_road_boost(),
            "it sprinted on the road all along"
        );
        character.step(keyed(1, 0, |keys| keys.block = true), &Road, SEA);
        assert!(!character.has_road_boost(), "a block");
    }

    #[test]
    fn combat_off_the_road_or_before_the_boost_leaves_it_alone() {
        // Off the road the boost's action is off: a hit keeps the boost's last seconds.
        let mut character = Character::new(Position { x: 1.0, ..origin() });
        for _ in 0..ROAD_DELAY_RECORDS + 1 {
            character.step(input(1, 0), &Road, SEA);
        }
        for _ in 0..OFF_ROAD_RECORDS {
            character.step(input(0, -1), &Road, SEA);
        }
        assert!(character.position().x < 0.0);
        character.enter_combat();
        assert!(character.has_road_boost(), "the tail stays");
        // On the road before the delay: no combat timer, the boost comes on time.
        let mut early = Character::new(Position { x: 1.0, ..origin() });
        early.step(input(1, 0), &Road, SEA);
        early.enter_combat();
        for _ in 0..ROAD_DELAY_RECORDS {
            early.step(input(1, 0), &Road, SEA);
        }
        assert!(early.has_road_boost());
    }

    #[test]
    fn a_road_far_under_the_body_gives_no_boost() {
        /// Road under a bridge 5 m over the terrain.
        struct Bridge;
        const BRIDGE_M: f32 = 5.0;
        impl Ground for Bridge {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                Some(0.0)
            }
            fn floor_at(&self, _: f32, _: f32, _: f32) -> Option<f32> {
                Some(BRIDGE_M)
            }
            fn road_share(&self, _: f32, _: f32) -> f32 {
                1.0
            }
        }
        let mut character = Character::new(Position {
            z: BRIDGE_M,
            ..origin()
        });
        for _ in 0..ROAD_DELAY_RECORDS * 2 {
            character.step(input(1, 0), &Bridge, SEA);
        }
        assert!(!character.has_road_boost());
    }

    #[test]
    fn a_crouched_sprint_on_a_road_gets_no_boost() {
        let mut character = Character::new(Position { x: 1.0, ..origin() });
        character.step(keyed(0, 0, |keys| keys.crouch_toggle = true), &Road, SEA);
        for _ in 0..ROAD_DELAY_RECORDS * 2 {
            character.step(input(1, 0), &Road, SEA);
        }
        assert!(!character.has_road_boost());
        assert!((speed(&character) - CROUCH_SPEED_MPS).abs() < CLOSE);
    }

    /// The `ftue_v2` terrain under the tutorial's move into the water, m.
    const TUTORIAL_SEABED_M: f32 = 10.82;

    /// The tutorial sea where the server moved the player into it.
    struct TutorialSea;

    impl Ground for TutorialSea {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(TUTORIAL_SEABED_M)
        }
    }

    /// Retail's place before its drop into the tutorial sea, held to newchar record 3072, m.
    const RETAIL_DROP_FROM_M: f32 = 26.155;
    /// Retail's own places in that drop: records after 3072 and the place's height (newchar
    /// records 3073 to 3108; 3079 carried no place).
    const RETAIL_DROP: [(u32, f32); 35] = [
        (1, 26.140),
        (2, 26.124),
        (3, 26.093),
        (4, 26.046),
        (5, 25.999),
        (6, 25.920),
        (8, 25.763),
        (9, 25.669),
        (10, 25.559),
        (11, 25.434),
        (12, 25.308),
        (13, 25.167),
        (14, 25.010),
        (15, 24.854),
        (16, 24.681),
        (17, 24.493),
        (18, 24.289),
        (19, 24.074),
        (20, 23.855),
        (21, 23.635),
        (22, 23.384),
        (23, 23.133),
        (24, 22.882),
        (25, 22.600),
        (26, 22.317),
        (27, 22.026),
        (28, 21.728),
        (29, 21.399),
        (30, 21.085),
        (31, 20.740),
        (32, 20.395),
        (33, 20.034),
        (34, 19.677),
        (35, 19.300),
        (36, 18.908),
    ];
    /// The record after 3072 in which retail's place was in the water (3109: 18.50 m).
    const RETAIL_DROP_IN_WATER_RECORD: u32 = 37;
    /// How far the server's drop may be from retail's place, m: retail's fall was 0.5 m/s fast
    /// at its first record (the fit), the server's starts from rest.
    const RETAIL_DROP_TOLERANCE_M: f32 = 0.25;

    #[test]
    fn a_dropped_body_falls_into_the_sea_as_retail_s_did() {
        let mut character = Character::new(origin());
        character.drop_at(Position {
            z: RETAIL_DROP_FROM_M,
            ..origin()
        });
        let mut in_water = None;
        for record in 1..=RETAIL_DROP_IN_WATER_RECORD + 2 {
            character.step(input(0, 0), &TutorialSea, TUTORIAL_SEA);
            let z = character.position().z;
            if let Some((_, retail)) = RETAIL_DROP.iter().find(|(at, _)| *at == record) {
                assert!(
                    (z - retail).abs() < RETAIL_DROP_TOLERANCE_M,
                    "record {record}: {z} against retail {retail}"
                );
            }
            if in_water.is_none() && z <= swim_floor(TUTORIAL_SEA) {
                in_water = Some(record);
            }
        }
        assert!(
            in_water.is_some_and(|record| record.abs_diff(RETAIL_DROP_IN_WATER_RECORD) <= 1),
            "{in_water:?}"
        );
        assert!(character.is_grounded(), "it swims");
        assert!((character.position().z - swim_floor(TUTORIAL_SEA)).abs() < CLOSE);
    }

    #[test]
    fn a_drop_where_no_floor_is_known_stays_where_it_was_put() {
        struct NoHeights;
        impl Ground for NoHeights {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                None
            }
        }
        let mut character = Character::new(origin());
        character.drop_at(origin());
        character.step(input(0, 0), &NoHeights, SEA);
        assert!(character.is_grounded());
        assert!(character.position().z.abs() < CLOSE);
    }

    /// The `ftue_v2` terrain under the tutorial ship, m.
    const SHIP_TERRAIN_M: f32 = 50.0;
    /// The ship's deck (the galleon's physics asset at the tutorial start; retail own place
    /// 89.38 m), m.
    const DECK_M: f32 = 89.38;
    /// The deck spans y from -10 to 10 m.
    const DECK_HALF_M: f32 = 10.0;

    /// The tutorial ship as the level's ground knows it: its deck over the terrain.
    struct Ship;

    impl Ground for Ship {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(SHIP_TERRAIN_M)
        }

        fn floor_at(&self, _: f32, y: f32, z: f32) -> Option<f32> {
            let on_deck = y.abs() <= DECK_HALF_M && z + 1.3 >= DECK_M;
            Some(if on_deck { DECK_M } else { SHIP_TERRAIN_M })
        }
    }

    fn on_deck() -> Position {
        Position {
            z: DECK_M,
            ..origin()
        }
    }

    #[test]
    fn a_body_on_a_known_deck_walks_on_it_and_jumps_back_onto_it() {
        let mut character = Character::new(on_deck());
        for _ in 0..30 {
            character.step(input(0, 1), &Ship, TUTORIAL_SEA);
            assert!(character.is_grounded());
            assert!((character.position().z - DECK_M).abs() < CLOSE);
        }
        assert!(character.position().x > 1.0, "it walked");
        let jump = CharacterInput {
            intent: Intent {
                jump: true,
                ..Intent::default()
            },
            ..input(0, 1)
        };
        character.step(jump, &Ship, TUTORIAL_SEA);
        assert!(character.is_jumping());
        let mut peak = DECK_M;
        for _ in 0..60 {
            character.step(input(0, 1), &Ship, TUTORIAL_SEA);
            peak = peak.max(character.position().z);
        }
        assert!(peak > DECK_M + 0.5, "it jumped: {peak}");
        assert!(character.is_grounded());
        assert!((character.position().z - DECK_M).abs() < CLOSE);
    }

    #[test]
    fn it_walks_off_the_deck_s_edge_and_falls_to_the_floor_below() {
        let mut character = Character::new(on_deck());
        for _ in 0..150 {
            character.step(input(1, 0), &Ship, TUTORIAL_SEA);
        }
        assert!(character.position().y > DECK_HALF_M);
        assert!(character.is_grounded());
        assert!((character.position().z - SHIP_TERRAIN_M).abs() < CLOSE);
    }

    /// The jump key held for one record.
    fn jump() -> CharacterInput {
        CharacterInput {
            intent: Intent {
                jump: true,
                ..Intent::default()
            },
            ..input(0, 0)
        }
    }

    #[test]
    fn no_jump_starts_while_swimming_or_wading() {
        // Run 15: a jump key in the tutorial sea threw the swimming body 0.9 m out of the water.
        let mut swimming = swimmer();
        let swim = swimming.position().z;
        swimming.step(jump(), &SeaBed, TUTORIAL_SEA);
        for _ in 0..30 {
            swimming.step(input(0, 0), &SeaBed, TUTORIAL_SEA);
            assert!(
                (swimming.position().z - swim).abs() < CLOSE,
                "it stays in the water"
            );
        }
        let floor = SHALLOW_FLOOR_M;
        let mut wading = Character::new(Position {
            z: floor,
            ..origin()
        });
        wading.step(input(0, 0), &Shallows, TUTORIAL_SEA);
        wading.step(jump(), &Shallows, TUTORIAL_SEA);
        for _ in 0..30 {
            wading.step(input(0, 0), &Shallows, TUTORIAL_SEA);
            assert!(
                (wading.position().z - floor).abs() < CLOSE,
                "it stays on the bottom"
            );
        }
        let mut dry = Character::new(origin());
        dry.step(jump(), &Flat, SEA);
        let mut peak: f32 = 0.0;
        for _ in 0..30 {
            dry.step(input(0, 0), &Flat, SEA);
            peak = peak.max(dry.position().z);
        }
        assert!(peak > 0.5, "on land it jumps: {peak}");
    }

    /// Flat ground at 0 with a box `height` m high whose south face runs along y = `face_y`
    /// over x from -5 to 5, turned by `turn` radians about the up axis at (0, `face_y`).
    fn walled(
        height: f32,
        face_y: f32,
        turn: f32,
    ) -> crate::LevelGround<crate::test_support::FlatGround> {
        use nw_world::collision::{Body, Geometry, LevelCollision};
        use nw_world::collision_index::CollisionIndex;
        const HALF_DEPTH_M: f32 = 1.0;
        const HALF_WIDTH_M: f32 = 5.0;
        let (sin, cos) = turn.sin_cos();
        // The box's centre is half its depth north of the face's middle, turned with it.
        let centre = [-sin * HALF_DEPTH_M, face_y + cos * HALF_DEPTH_M];
        let place = [
            [cos, -sin, 0.0, centre[0]],
            [sin, cos, 0.0, centre[1]],
            [0.0, 0.0, 1.0, height / 2.0],
        ];
        let collision = CollisionIndex::new(LevelCollision {
            level: "ftue_v2".to_owned(),
            geometries: vec![Geometry::Box {
                half_extents: [HALF_WIDTH_M, HALF_DEPTH_M, height / 2.0],
            }],
            bodies: vec![Body { geometry: 0, place }],
            ..LevelCollision::default()
        });
        crate::LevelGround::new(
            crate::test_support::FlatGround(0.0),
            std::sync::Arc::new(collision),
        )
    }

    #[test]
    fn a_solid_taller_than_a_step_up_stops_the_body_a_capsule_short_of_it() {
        // The block under the tutorial beach's stair landing stands 4 m over the sand (run 15).
        const BLOCK_M: f32 = 4.0;
        const FACE_Y: f32 = 3.0;
        let ground = walled(BLOCK_M, FACE_Y, 0.0);
        let mut character = Character::new(origin());
        for _ in 0..90 {
            character.step(input(1, 0), &ground, SEA);
            assert!(
                character.position().y <= FACE_Y - PLAYER_RADIUS_M + CLOSE,
                "{:?}",
                character.position()
            );
        }
        /// One record of sprint, m.
        const RECORD_M: f32 = SPRINT_SPEED_MPS * DT;
        assert!(
            character.position().y > FACE_Y - PLAYER_RADIUS_M - RECORD_M,
            "it got to the wall"
        );
        assert!(
            character.velocity()[1].abs() < CLOSE,
            "no speed into the wall"
        );
    }

    #[test]
    fn a_body_slides_along_a_slanted_wall() {
        const BLOCK_M: f32 = 4.0;
        const FACE_Y: f32 = 3.0;
        /// The face turned 30 degrees.
        const TURN: f32 = std::f32::consts::FRAC_PI_6;
        let ground = walled(BLOCK_M, FACE_Y, TURN);
        let mut character = Character::new(origin());
        for _ in 0..90 {
            character.step(input(1, 0), &ground, SEA);
        }
        let at = character.position();
        // The face's outward normal is (sin, -cos) of the turn. The probe reaches a capsule
        // ahead along the motion, so the body stops a capsule short of the face along the
        // keys' north: radius × cos(turn) off it.
        let (sin, cos) = TURN.sin_cos();
        let off_face = at.x * sin - (at.y - FACE_Y) * cos;
        assert!(off_face >= PLAYER_RADIUS_M * cos - CLOSE, "{at:?}");
        assert!(at.x > 1.0, "it slid east along the face: {at:?}");
    }

    #[test]
    fn a_solid_lower_than_a_step_up_is_no_wall() {
        // The rocks beside the beach path stand 1.1 to 1.2 m over the sand (run 15).
        const ROCK_M: f32 = 1.15;
        const FACE_Y: f32 = 3.0;
        let ground = walled(ROCK_M, FACE_Y, 0.0);
        let mut character = Character::new(origin());
        for _ in 0..90 {
            character.step(input(1, 0), &ground, SEA);
        }
        assert!(
            character.position().y > FACE_Y,
            "{:?}",
            character.position()
        );
    }

    /// `ConfigMountRiding_0` (`javelindata_mount_movement`): the run and walk of each surface,
    /// the slow walk, the rates and the dash scalers.
    fn riding_paces() -> MountPaces {
        let pace = |run_mps, walk_mps| MountPace {
            run_mps,
            walk_mps,
            slow_walk_mps: 2.0,
            acceleration_mps2: 13.5,
            deceleration_mps2: 9.0,
            walk_acceleration_mps2: 13.5,
            walk_deceleration_mps2: 12.0,
        };
        MountPaces {
            on_road: pace(9.25, 6.3),
            off_road: pace(7.0, 5.5),
            very_shallow_water: pace(6.65, 5.5),
            shallow_water: pace(5.55, 5.0),
            deep_water: pace(4.8, 2.5),
            dash_speed_scale: 1.12,
            dash_acceleration_scale: 1.15,
        }
    }

    /// Flat ground at height 0 that is road everywhere.
    struct WholeRoad;

    impl Ground for WholeRoad {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(0.0)
        }

        fn road_share(&self, _: f32, _: f32) -> f32 {
            1.0
        }
    }

    fn ride(rider: &mut Character, intent: Intent, ground: &impl Ground, sea_level_m: f32) {
        for _ in 0..60 {
            rider.step(
                CharacterInput {
                    intent,
                    yaw: 0.0,
                    dt_s: DT,
                },
                ground,
                sea_level_m,
            );
        }
    }

    #[test]
    fn a_rider_moves_at_its_mount_s_speeds() {
        let paces = riding_paces();
        let forward = Intent {
            forward: 1,
            ..Intent::default()
        };
        let walk = Intent {
            walk: true,
            ..forward
        };
        let speed = |rider: &Character| rider.velocity()[1];
        let mut rider = Character::new(origin());
        rider.mount(paces);
        assert!(rider.is_mounted());
        ride(&mut rider, forward, &Flat, SEA);
        assert!((speed(&rider) - paces.off_road.run_mps).abs() < CLOSE);
        ride(&mut rider, forward, &WholeRoad, SEA);
        assert!((speed(&rider) - paces.on_road.run_mps).abs() < CLOSE);
        rider.set_mount_dash(true);
        ride(&mut rider, forward, &WholeRoad, SEA);
        let dash = paces.on_road.run_mps * paces.dash_speed_scale;
        assert!((speed(&rider) - dash).abs() < CLOSE);
        rider.set_mount_dash(false);
        ride(&mut rider, walk, &WholeRoad, SEA);
        assert!((speed(&rider) - paces.on_road.walk_mps).abs() < CLOSE);
        rider.set_mount_slow_walk(true);
        ride(&mut rider, walk, &WholeRoad, SEA);
        assert!((speed(&rider) - paces.on_road.slow_walk_mps).abs() < CLOSE);
        rider.dismount();
        assert!(!rider.is_mounted());
        ride(&mut rider, forward, &Flat, SEA);
        assert!((speed(&rider) - SPRINT_SPEED_MPS).abs() < CLOSE);
    }

    #[test]
    fn a_rider_neither_dodges_nor_changes_its_posture() {
        let paces = riding_paces();
        let mut rider = Character::new(origin());
        rider.mount(paces);
        let keys = Intent {
            forward: 1,
            dodge: true,
            crouch_toggle: true,
            prone_toggle: true,
            ..Intent::default()
        };
        rider.step(
            CharacterInput {
                intent: keys,
                yaw: 0.0,
                dt_s: DT,
            },
            &Flat,
            SEA,
        );
        assert_eq!(rider.posture(), Posture::Stand);
        assert!(
            (rider.velocity()[1] - paces.off_road.acceleration_mps2 * DT).abs() < CLOSE,
            "the keys' motion, not a dodge roll"
        );
    }

    #[test]
    fn a_rider_in_water_takes_the_water_s_speeds() {
        let paces = riding_paces();
        let forward = Intent {
            forward: 1,
            ..Intent::default()
        };
        // The water 0.5 m over the feet is very shallow, 1 m shallow (the wade depth 0.895 m).
        const VERY_SHALLOW_SEA_M: f32 = 0.5;
        const SHALLOW_SEA_M: f32 = 1.0;
        let mut rider = Character::new(origin());
        rider.mount(paces);
        ride(&mut rider, forward, &Flat, VERY_SHALLOW_SEA_M);
        assert!((rider.velocity()[1] - paces.very_shallow_water.run_mps).abs() < CLOSE);
        ride(&mut rider, forward, &Flat, SHALLOW_SEA_M);
        assert!((rider.velocity()[1] - paces.shallow_water.run_mps).abs() < CLOSE);
        assert!(!rider.is_swimming());
    }
}
