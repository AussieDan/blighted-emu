// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's body as the server keeps it: moved by its keys ([`crate::character`]) over the
//! level's floors, and held to the client's own view ([`crate::camera_check`]) on the ground
//! plane.
//!
//! The rule, per input record: the character moves; the camera check solves where the client's
//! camera puts the body; on the ground plane the body goes toward where the camera puts it, at
//! most [`FOLLOW_STEP_M`] in one record (a wall stopped the client, a dodge or an attack moved
//! it, the server's speed was not the client's). The client moves its own player, and every
//! difference the server keeps pulls the client back. The replay of the retail tutorial captures
//! against retail's own places (`content-tool` test `own_replay`, 2026-09-24) measured it: with a
//! pull past 0.15 m by half the distance and at most 0.15 m a record, the client would have been
//! corrected (past 0.5 m) in 40% of the open-world states, with this follow in 36%, and the open
//! world's errors were 0.31 and 2.51 m (median, 90th percentile) against 0.25 and 1.71 m. A full
//! snap in one record stepped the sent place by 0.5 to 0.8 m (live 2026-09-22, the tutorial
//! ship), which the client showed as judder. When that pulls the body back against its motion,
//! the character's speed drops to the camera body's own speed along it since the record before (a
//! client held at a wall stays there instead of being pulled back record after record; replay of
//! the newchar capture, 2026-09-20: 1,366 pull-backs instead of 1,486 in the tutorial and 1,610
//! instead of 1,945 in the open world, a median of 0.34 m instead of 0.29 m in the tutorial and
//! 0.16 m instead of 0.14 m in the open world).
//!
//! The height is the level's floor, not the camera's. The ground knows the level's floors: the
//! terrain and the structures of the level collision file (`crate::LevelGround`; the tutorial deck
//! at the retail own place, 89.38 m at the start). Out of a jump, the camera's height only picks
//! which floor under the body the client stands on: the highest one at most a step-up detection
//! height above the higher of the camera's body and the body
//! ([`nw_world::collision_index::FLOOR_SEARCH_ABOVE_M`]; in an action the camera's body went
//! 1.5 m under the tutorial deck, run 19, and a search from it missed the deck); the
//! deck and not the sea under it, a building's upper floor and not its cellar. A camera's height
//! errs by up to half a metre, the floors of one place lie metres apart. A body that falls lands
//! on that floor once it is down at it. Where the level has neither height data nor a structure
//! the body takes the camera's height. So does a body whose camera stands more than
//! [`CAMERA_FLOOR_SPREAD_M`] above the highest floor the level knows there while it sinks no
//! faster than a walk down the steepest walkable slope: it stands on a floor the level lacks (the
//! steps of the tutorial cave, run 17: the level collision has no surface between 23.13 m and
//! 33 m there, and the body stayed at the foot while the client climbed); in an action it keeps
//! its own height there, as the camera's height is off in an action. A body the server dropped
//! (a move into the air) falls by the physics until it lands.
//!
//! The camera does not carry the body through a wall ([`wall_between`]) from close by: the
//! client's capsule does not pass one, so a camera body just beyond it is the solve's error, a
//! boom the level shortened (run 15: by the beach stairs the camera's body stood 1.2 m from the
//! camera and resyncs of 0.7 to 1.3 m carried the body into the solid block under the stair
//! landing). The wall is the one at the higher body's feet, so a client that climbed onto a ledge
//! takes the body along. From farther than [`WALL_HOLD_M`], a camera's body that stands in the
//! open on the far side (the way back to the body meets the wall too) shows a client that went
//! around or over the wall where the level's collision differs from the client's, and the camera
//! carries the body again: in the tutorial replay a wall held the body behind the beach block for
//! the rest of the run, 127 m from retail's place (median); with this limit 0.40 m (1.5 m and 3 m
//! gave the same within 0.03 m). A camera's body inside a solid (the way back enters no wall)
//! never carries it, from any distance (run 15's stayed in the block for a second).
//!
//! A camera does not give the body's height itself: the boom is never sent, the camera check
//! measures it from the server's own motion of the body while the view turns, and an action
//! moves the body by its animation, which the solve reads as a change of the boom. Run 14
//! (2026-09-20), the first sailor fight on the tutorial deck: in a fast turn of the view the
//! solve's boom grew from 4.4 to 5.25 m without a boom key, the camera's body went 0.55 m under
//! the deck, and a body that took the camera's height went down through the deck, 89.39 to 82.1 m
//! in two seconds. The client's camera also shortens its boom where it meets the level
//! (`libs/camera/gamecamera.xml`: `CameraCollides`); on the retail tutorial deck the camera's body
//! stood 0.48 m below to 0.73 m above the retail own place while moving (5% to 95%). Replay of
//! the retail newchar tutorial with the level collision (level-collision note 2026-09-20): the
//! body's height erred by a median 0.022 m and 0.117 m at the 90th percentile, against 0.124 m and
//! 0.654 m with the camera's height. In a jump the body keeps its own height and the camera moves
//! it only on the ground plane (a jump that dropped the camera's pull-back went on 0.58 m beside
//! retail's place over the Fury Bay ledge gap, open-world replay records 25,304 to 25,321). A record whose camera does not follow the body changes
//! nothing.
//!
//! An attack whose action's animation drives the motion (`AnimationDrivenMotion`,
//! [`AttackMotion`]) moves the body by the animation's root, not by the keys: toward the keys of
//! the record it starts in, else the camera's forward (Observed, retail newchar tutorial: the
//! first attacks after a pause without a movement key went 1 to 24 degrees from the camera's
//! forward; with a key held the place kept 0.84 to 0.97 m/s from 0.5 to 1 s after the start,
//! against 3.5 to 5 m/s free), with the velocity it keeps, until a movement key ends it from its
//! `CanTransition` marker on, a dodge or a block ends it, or its motion is over. The sim learns
//! of an attack after the records of its message moved the body: those records are taken again
//! with the attack's motion ([`TrackedBody::start_attack`]).

use std::collections::VecDeque;
use std::sync::Arc;

use nw_content::player_attacks::AttackMotion;

use crate::camera_check::{CameraCheck, CameraView, MOUNTED_CAMERA_LAG_S};
use crate::character::{
    Character, CharacterInput, MountPaces, SPRINT_SPEED_MPS, key_direction, swim_floor_at,
    wall_between, yaw_axes,
};
use crate::equip_load::EquipLoad;
use crate::movement::{Ground, Position};
use nw_world::collision_index::MAX_WALKABLE_SLOPE_DEG;

/// How far the server's body may be from the camera's body and stay, m: the own place's step on
/// the wire (4/255 m), below which the client cannot see the difference.
pub const FOLLOW_FROM_M: f32 = 4.0 / 255.0;
/// The longest step toward the camera's body in one record, m (Observed: retail's own places step
/// at most 0.36 m from one record to the next in the open-world walk replay, motion included).
pub const FOLLOW_STEP_M: f32 = 0.36;
/// How far the camera's body may be from the server's body and a wall between them still hold the
/// body, m (the patched client's full-correction distance; see the module notes).
pub const WALL_HOLD_M: f32 = 2.0;
/// Records after the client starts an action (an attack, a dodge, an ability) while its moves may
/// carry the body without the movement keys, so the camera check does not hold it still
/// (Observed: after the 20 actions the newchar tutorial started without a movement key, the own
/// place last moved 25 to 60 records later, median 52, by up to 8.5 m; each new action starts
/// the count again).
pub const ACTION_RECORDS: u8 = 52;
/// Records in a row whose game camera does not look at the body after which the body goes to
/// where the camera puts it at its last boom (our choice: about one second). The camera check
/// takes no camera that passes more than 1 m (`CAMERA_FIT_M`) beside the body,
/// so a body that got that far from its client (a server move the client did not take, a client
/// move the server did not see) was never found again: live 2026-09-26, Thorpe's first grab pull
/// left the body 7 m from the client and it drifted to 21 m for the rest of the fight, where
/// Thorpe fought it. Cutscene and video cameras never come here (the sim drops their views).
pub const RECOVER_AFTER_RECORDS: u16 = 30;
/// How far above the highest known floor the camera's body may stand and still stand on it, m
/// (Observed: on the retail tutorial deck the camera's body stood 0.48 m below to 0.73 m above the
/// retail own place while moving, 5% to 95%).
pub const CAMERA_FLOOR_SPREAD_M: f32 = 1.0;

/// The records a body remembers, to take them again with the motion of an attack that started
/// in them (our choice: an input message holds three records, a resent one up to six).
pub const ATTACK_REWIND_RECORDS: usize = 8;
/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;

/// The fastest a walking body comes down, m/s: a sprint down the steepest walkable slope. A
/// camera that sinks faster shows a fall.
fn walking_descent_mps() -> f32 {
    SPRINT_SPEED_MPS * MAX_WALKABLE_SLOPE_DEG.to_radians().sin()
}

/// One input record for a tracked body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackInput {
    /// The keys, the yaw and the frame time.
    pub character: CharacterInput,
    /// The record's camera, when the client's camera carries the body.
    pub view: Option<CameraView>,
    /// The record pressed a key that changes the camera's boom.
    pub boom_key: bool,
    /// The client starts an action in this record (an attack or an ability: combat, which ends
    /// the road boost).
    pub action_started: bool,
}

/// What one record did to a tracked body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tracked {
    /// Where the body is.
    pub position: Position,
    /// The camera put the body elsewhere than the keys.
    pub resynced: bool,
    /// The camera followed the body.
    pub consistent: bool,
    /// The body stands on a floor: not in a jump or a fall.
    pub on_floor: bool,
}

/// An attack whose animation moves the body.
#[derive(Debug, Clone, PartialEq)]
struct AttackPlay {
    motion: Arc<AttackMotion>,
    /// Its forward on the ground plane, once it started (a unit vector).
    forward: Option<[f32; 2]>,
    /// Ms since it started (below 0: it starts that much later).
    at_ms: f32,
    /// From this long after its start a movement key ends it, ms.
    keys_from_ms: f32,
    /// The velocity it keeps, along its right and its forward, m/s.
    kept_mps: [f32; 2],
}

/// One record the body took: where it stood before it, the record's input and its velocity then.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TakenRecord {
    before: Position,
    input: CharacterInput,
    velocity: [f32; 2],
}

/// A player's body.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedBody {
    character: Character,
    camera: CameraCheck,
    /// The attack whose animation moves it.
    attack: Option<AttackPlay>,
    /// Its last records, oldest first ([`ATTACK_REWIND_RECORDS`]).
    taken: VecDeque<TakenRecord>,
    /// Where the camera put the body in the record before, when it did.
    last_seen: Option<Position>,
    /// Records left while an action the client started may move the body.
    acting_records: u8,
    /// The server dropped it and it has not landed yet.
    dropping: bool,
    /// The last record stood it on a floor the level lacks, at its camera's height: the physics
    /// finds no floor under it, but it does not fall.
    on_lacking_floor: bool,
    /// Records in a row whose camera did not look at it.
    unfit_records: u16,
}

impl TrackedBody {
    /// A body standing at `at`.
    pub fn new(at: Position) -> Self {
        Self {
            character: Character::new(at),
            camera: CameraCheck::default(),
            attack: None,
            taken: VecDeque::with_capacity(ATTACK_REWIND_RECORDS),
            last_seen: None,
            acting_records: 0,
            dropping: false,
            on_lacking_floor: false,
            unfit_records: 0,
        }
    }

    /// Where it is.
    pub const fn position(&self) -> Position {
        self.character.position()
    }

    /// Its velocity in the ground plane, m/s.
    pub fn velocity(&self) -> [f32; 2] {
        self.character.velocity()
    }

    /// Whether it stands still: no speed on the ground plane, not in a jump, and no action of
    /// the client moving it.
    pub fn is_resting(&self) -> bool {
        self.character.is_resting() && self.acting_records == 0 && self.attack.is_none()
    }

    /// The server started an attack of motion `motion` `since_ms` ago (below 0: it starts that
    /// much later), whose movement keys may end it from `keys_from_ms` after its start: its
    /// animation moves the body from then on. The records the body took since its start are
    /// taken again with its motion, over `ground` with the sea at `sea_level_m`.
    pub fn start_attack(
        &mut self,
        motion: Arc<AttackMotion>,
        keys_from_ms: f32,
        since_ms: f32,
        ground: &(impl Ground + ?Sized),
        sea_level_m: f32,
    ) {
        let mut rewind = 0;
        let mut rewound_ms = 0.0;
        for record in self.taken.iter().rev() {
            if rewound_ms >= since_ms {
                break;
            }
            rewound_ms += record.input.dt_s * MS_PER_SECOND;
            rewind += 1;
        }
        let again: Vec<TakenRecord> = self.taken.drain(self.taken.len() - rewind..).collect();
        self.attack = Some(AttackPlay {
            motion,
            forward: None,
            at_ms: since_ms - rewound_ms,
            keys_from_ms,
            kept_mps: [0.0; 2],
        });
        let Some(first) = again.first() else {
            return;
        };
        self.character.land_at(first.before);
        for record in again {
            let driven = self.attack_motion(record.input, record.velocity);
            self.remember(record.input);
            match driven {
                Some(driven) => {
                    self.character
                        .step_driven(record.input, driven, ground, sea_level_m);
                }
                None => {
                    self.character.step(record.input, ground, sea_level_m);
                }
            }
        }
    }

    /// Keeps `input` among the records it took, with where it stands and its velocity now.
    fn remember(&mut self, input: CharacterInput) {
        if self.taken.len() == ATTACK_REWIND_RECORDS {
            self.taken.pop_front();
        }
        self.taken.push_back(TakenRecord {
            before: self.character.position(),
            input,
            velocity: self.character.velocity(),
        });
    }

    /// The motion of the attack under way in the record of `input` (m on the ground plane),
    /// when its animation moves the body; a body of velocity `velocity` that starts it keeps
    /// what its motion keeps of that. `None` when no attack moves it (a dodge, a block, a
    /// movement key past its marker or the motion's end ends it).
    fn attack_motion(&mut self, input: CharacterInput, velocity: [f32; 2]) -> Option<[f32; 2]> {
        let play = self.attack.as_mut()?;
        let CharacterInput { intent, yaw, dt_s } = input;
        let dt_ms = dt_s * MS_PER_SECOND;
        let over = play.at_ms >= play.motion.length_ms()
            || (play.at_ms >= play.keys_from_ms && intent.moves());
        if intent.dodge || intent.block || over {
            self.attack = None;
            return None;
        }
        let to_ms = play.at_ms + dt_ms;
        if to_ms <= 0.0 {
            play.at_ms = to_ms;
            return None;
        }
        let forward = *play.forward.get_or_insert_with(|| {
            let forward = key_direction(intent, yaw).unwrap_or_else(|| yaw_axes(yaw).0);
            let right = [forward[1], -forward[0]];
            let along = |axis: [f32; 2]| velocity[0] * axis[0] + velocity[1] * axis[1];
            let kept = [along(right), along(forward)];
            let speed = kept[0].hypot(kept[1]);
            let limit = play.motion.inherit_max_mps.unwrap_or(f32::INFINITY);
            let share = if play.motion.inherit_velocity && speed > f32::EPSILON {
                (limit / speed).min(1.0)
            } else {
                0.0
            };
            play.kept_mps = [kept[0] * share, kept[1] * share];
            forward
        });
        let right = [forward[1], -forward[0]];
        let from_ms = play.at_ms.max(0.0);
        let (from, to) = (play.motion.offset_at(from_ms), play.motion.offset_at(to_ms));
        let run_s = (to_ms - from_ms) / MS_PER_SECOND;
        let mut local = [0.0; 2];
        for axis in 0..2 {
            local[axis] = to[axis] - from[axis] + play.kept_mps[axis] * run_s;
            let fall = play.motion.decel_mps2[axis] * run_s;
            play.kept_mps[axis] = if play.kept_mps[axis] > 0.0 {
                (play.kept_mps[axis] - fall).max(0.0)
            } else {
                (play.kept_mps[axis] + fall).min(0.0)
            };
        }
        play.at_ms = to_ms;
        Some([
            right[0] * local[0] + forward[0] * local[1],
            right[1] * local[0] + forward[1] * local[1],
        ])
    }

    /// Its equip load category changes to `load` ([`Character::set_equip_load`]).
    pub const fn set_equip_load(&mut self, load: EquipLoad) {
        self.character.set_equip_load(load);
    }

    /// A held `NoSprint` effect keeps it from sprinting ([`Character::set_no_sprint`]).
    pub const fn set_no_sprint(&mut self, no_sprint: bool) {
        self.character.set_no_sprint(no_sprint);
    }

    /// Whether the road boost speeds up its sprint.
    pub fn has_road_boost(&self) -> bool {
        self.character.has_road_boost()
    }

    /// A hit on it: combat, which ends the road boost while its action is on
    /// ([`Character::enter_combat`]).
    pub const fn enter_combat(&mut self) {
        self.character.enter_combat();
    }

    /// It gets on a mount with `paces` ([`Character::mount`]).
    pub const fn mount(&mut self, paces: MountPaces) {
        self.character.mount(paces);
    }

    /// It gets off its mount ([`Character::dismount`]).
    pub const fn dismount(&mut self) {
        self.character.dismount();
    }

    /// Whether it rides a mount.
    pub const fn is_mounted(&self) -> bool {
        self.character.is_mounted()
    }

    /// Its mount dashes, or stops dashing ([`Character::set_mount_dash`]).
    pub const fn set_mount_dash(&mut self, dashing: bool) {
        self.character.set_mount_dash(dashing);
    }

    /// The walk key gives its mount's slow walk, or its walk.
    pub const fn set_mount_slow_walk(&mut self, slow_walk: bool) {
        self.character.set_mount_slow_walk(slow_walk);
    }

    /// Whether it stands on a floor: not in a jump or a fall (the landing of a fall, for its
    /// damage).
    pub const fn is_grounded(&self) -> bool {
        self.character.is_grounded()
    }

    /// Whether it swims.
    pub const fn is_swimming(&self) -> bool {
        self.character.is_swimming()
    }

    /// Whether it falls and is not in a jump or a swim.
    pub const fn is_falling(&self) -> bool {
        !self.character.is_grounded()
            && !self.character.is_jumping()
            && !self.character.is_swimming()
    }

    /// The camera boom it holds, once a camera fitted.
    pub const fn boom_m(&self) -> Option<f32> {
        self.camera.boom_m()
    }

    /// The server puts it at `at` (a spawn, a teleport, a push): it stands there, and the camera
    /// that follows it starts again.
    pub fn stand_at(&mut self, at: Position) {
        self.character.stand_at(at);
        self.attack = None;
        self.taken.clear();
        self.camera.forget();
        self.last_seen = None;
        self.dropping = false;
        self.on_lacking_floor = false;
    }

    /// The server puts it at `at` in the air (a teleport, a respawn): it falls from there to the
    /// floor it finds, and the camera that follows it starts again.
    pub fn drop_at(&mut self, at: Position) {
        self.character.drop_at(at);
        self.attack = None;
        self.taken.clear();
        self.camera.forget();
        self.last_seen = None;
        self.dropping = true;
        self.on_lacking_floor = false;
    }

    /// Moves it by one input record over `ground`, with the sea at `sea_level_m`.
    pub fn record(
        &mut self,
        input: TrackInput,
        ground: &(impl Ground + ?Sized),
        sea_level_m: f32,
    ) -> Tracked {
        if input.action_started {
            self.acting_records = ACTION_RECORDS;
            self.character.enter_combat();
        }
        let before = self.character.position();
        let driven = self.attack_motion(input.character, self.character.velocity());
        self.remember(input.character);
        let moved = match driven {
            Some(driven) => {
                self.character
                    .step_driven(input.character, driven, ground, sea_level_m)
            }
            None => self.character.step(input.character, ground, sea_level_m),
        };
        let mut position = self.character.position();
        let resting = self.is_resting();
        let acting = self.acting_records > 0;
        self.acting_records = self.acting_records.saturating_sub(1);
        let seen = input.view.and_then(|view| {
            // A rider's camera trails its mount farther than a walker's.
            let view = if self.character.is_mounted() {
                view.trailing(MOUNTED_CAMERA_LAG_S)
            } else {
                view
            };
            self.camera.observe(
                view,
                position,
                moved,
                self.character.velocity(),
                resting,
                input.boom_key,
            )
        });
        // A camera that has not looked at the body for [`RECOVER_AFTER_RECORDS`] shows where
        // the client is: the body goes there.
        let recovering = seen.is_none() && input.view.is_some() && {
            self.unfit_records = self.unfit_records.saturating_add(1);
            self.unfit_records >= RECOVER_AFTER_RECORDS
        };
        let seen = if recovering {
            input
                .view
                .and_then(|view| self.camera.body_at_last_boom(view))
        } else {
            seen
        };
        if seen.is_some() {
            self.unfit_records = 0;
        }
        let last_seen = std::mem::replace(&mut self.last_seen, seen.map(|seen| seen.at));
        let Some(seen) = seen else {
            return Tracked {
                position,
                resynced: false,
                consistent: false,
                on_floor: self.character.is_grounded(),
            };
        };
        // The wall is the one at the higher of the two bodies' feet: a client that climbed onto
        // a ledge (a mantle, a vault) shows its body on top, and the line passes over the ledge.
        let over = Position {
            z: position.z.max(seen.at.z),
            ..position
        };
        let distance = position.horizontal_distance(seen.at);
        // A fall keeps the keys' place: the server's physics brings it down. A swim and a jump
        // follow the camera on the ground plane (the tutorial replay: a swim that kept the keys
        // erred by 3.21 m at the 90th percentile in the sea, one that followed by 2.27 m).
        let falling = self.is_falling();
        // A body on a floor the level lacks has no floor under it in the physics, but its
        // client walks: the camera still carries it.
        let walled = wall_between(ground, over, seen.at).is_some() && {
            // From far, a camera's body on the open far side of the wall carries the body; one
            // inside a solid (the way back to the body enters no wall) does not.
            let back = Position {
                z: over.z,
                ..seen.at
            };
            distance <= WALL_HOLD_M || wall_between(ground, back, position).is_none()
        };
        let resynced = recovering
            || ((!falling || self.on_lacking_floor) && distance > FOLLOW_FROM_M && !walled);
        if resynced {
            let heading = self.character.heading();
            let along = |from: Position, to: Position| {
                (to.x - from.x) * heading[0] + (to.y - from.y) * heading[1]
            };
            if along(position, seen.at) < 0.0
                && let Some(last_seen) = last_seen
            {
                self.character
                    .slow_to((along(last_seen, seen.at) / input.character.dt_s).max(0.0));
            }
            // Toward the camera's body, the record's whole step at most a retail step from where
            // the body was: a camera that jumps (a new boom, the stop of its trail) does not throw
            // the body the others see, and the step stays inside the sim's speed limit (10.8 m/s
            // against 13 m/s).
            let (dx, dy) = (seen.at.x - before.x, seen.at.y - before.y);
            // A rider's step is its mount's: the walking step held a horse at 0.36 m a record,
            // 7.2 m/s at 20 records a second, below its 9.25 m/s road run (replay of retail
            // Hermit's Rally: the ridden body fell 3 m behind retail's place, median).
            let step_m = self
                .character
                .mount_top_speed_mps()
                .map_or(FOLLOW_STEP_M, |top_mps| {
                    FOLLOW_STEP_M.max(top_mps * input.character.dt_s)
                });
            // A recovery goes the whole way at once.
            let share = if recovering {
                1.0
            } else {
                (step_m / dx.hypot(dy)).min(1.0)
            };
            position.x = before.x + dx * share;
            position.y = before.y + dy * share;
        }
        if self.character.is_jumping() {
            // A jump keeps its own height; the camera only moves it on the ground plane.
            self.character.place(position);
        } else {
            let seen_at = Position {
                z: seen.at.z,
                ..position
            };
            let walking_height = last_seen.is_none_or(|last| {
                (last.z - seen.at.z) / input.character.dt_s <= walking_descent_mps()
            });
            // An action moves the camera's body off the body's height (run 14: 0.55 m under
            // the deck; run 19: 1.5 m, past the step-up search, and the body went down through
            // the deck to the sea). The floor is searched from the higher of the two bodies'
            // feet, and a body on a floor the level lacks keeps its own height in an action.
            let held = std::mem::take(&mut self.on_lacking_floor) && acting;
            let height = if held { position.z } else { seen.at.z };
            // A rider's camera sits on the saddle, about 2 m above a walker's: its height picks
            // no floor and gives the body none (replay of retail Hermit's Rally: with it the
            // ridden body stood 2.0 m above retail's place, median).
            let mounted = self.character.is_mounted();
            let search_at = Position {
                z: if mounted {
                    position.z
                } else {
                    position.z.max(seen.at.z)
                },
                ..position
            };
            match floor_under(ground, search_at, sea_level_m) {
                // A floor the level lacks.
                Some(floor)
                    if !self.dropping
                        && !self.is_swimming()
                        && !mounted
                        && (held || walking_height)
                        && height - floor > CAMERA_FLOOR_SPREAD_M =>
                {
                    self.character.land_at(Position {
                        z: height,
                        ..position
                    });
                    self.on_lacking_floor = true;
                }
                Some(floor) if !falling || position.z <= floor => {
                    position.z = floor;
                    self.character.land_at(position);
                }
                Some(_) => self.character.place(position),
                // No height data and no structure here: only the camera gives a height.
                None => self.character.land_at(seen_at),
            }
        }
        self.dropping &= !self.character.is_grounded();
        Tracked {
            position: self.character.position(),
            resynced,
            consistent: true,
            on_floor: self.character.is_grounded(),
        }
    }
}

/// The floor under the feet of a body at `at`: the floor of `ground` there, or the swim floor of
/// the water there (the sea at `sea_level_m` where `ground` knows no water) when that is higher.
fn floor_under(ground: &(impl Ground + ?Sized), at: Position, sea_level_m: f32) -> Option<f32> {
    let swim = swim_floor_at(ground, at.x, at.y, sea_level_m);
    match ground.floor_at(at.x, at.y, at.z) {
        Some(floor) => Some(swim.map_or(floor, |swim| floor.max(swim))),
        None => ground.height_at(at.x, at.y).and(swim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_check::{CAMERA_LAG_S, CAMERA_RISE_M, SHOULDER_RIGHT_M};
    use crate::character::{ACCELERATION_MPS2, Intent, yaw_axes};
    use crate::character::{CharacterInput, swim_floor};

    const DT: f32 = 1.0 / 30.0;
    const SEA: f32 = -100.0;
    const BOOM: f32 = 4.0;
    const PITCH: f32 = 0.2;
    /// The height of the flat ground.
    const GROUND_Z: f32 = 10.0;

    struct Flat;

    impl Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z)
        }
    }

    /// The camera of a body at `body` looking north with boom `boom`, trailing `velocity`.
    fn view_of(body: Position, boom: f32, velocity: [f32; 2]) -> CameraView {
        let (forward, right) = yaw_axes(0.0);
        let level = boom * PITCH.cos();
        CameraView::new(
            Position {
                x: body.x - level * forward[0] + SHOULDER_RIGHT_M * right[0]
                    - CAMERA_LAG_S * velocity[0],
                y: body.y - level * forward[1] + SHOULDER_RIGHT_M * right[1]
                    - CAMERA_LAG_S * velocity[1],
                z: body.z + CAMERA_RISE_M + boom * PITCH.sin(),
            },
            0.0,
            PITCH,
        )
    }

    fn input(forward: i8, view: CameraView) -> TrackInput {
        TrackInput {
            character: CharacterInput {
                intent: Intent {
                    forward,
                    ..Intent::default()
                },
                yaw: 0.0,
                dt_s: DT,
            },
            view: Some(view),
            boom_key: false,
            action_started: false,
        }
    }

    fn start() -> Position {
        Position {
            x: 100.0,
            y: 100.0,
            z: GROUND_Z,
        }
    }

    #[test]
    fn a_body_its_camera_lost_goes_back_to_the_camera_after_a_second() {
        // The client's body: 7 m to the side of the server's (live 2026-09-26 after Thorpe's
        // first grab pull), standing still.
        const GAP_M: f32 = 7.0;
        let client = Position {
            x: start().x + GAP_M,
            ..start()
        };
        let mut body = TrackedBody::new(start());
        // The camera learns its boom while it looks at the body.
        for _ in 0..10 {
            body.record(input(0, view_of(start(), BOOM, [0.0; 2])), &Flat, SEA);
        }
        body.stand_at(start());
        let away = view_of(client, BOOM, [0.0; 2]);
        for record in 1..RECOVER_AFTER_RECORDS {
            let tracked = body.record(input(0, away), &Flat, SEA);
            assert!(
                tracked.position.horizontal_distance(start()) < 0.1,
                "record {record}: a camera that does not look at the body moves nothing yet"
            );
        }
        let tracked = body.record(input(0, away), &Flat, SEA);
        assert!(tracked.resynced);
        assert!(
            tracked.position.horizontal_distance(client) < 0.1,
            "{:?}: the body goes to its client",
            tracked.position
        );
    }

    /// An attack whose animation moves its player 1 m forward over 300 ms.
    fn lunge() -> Arc<AttackMotion> {
        Arc::new(AttackMotion {
            step_ms: 100.0,
            offsets: vec![[0.0, 0.0], [0.0, 0.2], [0.0, 0.6], [0.0, 1.0]],
            inherit_velocity: false,
            inherit_max_mps: None,
            decel_mps2: [0.0; 2],
        })
    }

    /// A record of keys alone (no camera) with camera yaw `yaw`.
    fn keys(forward: i8, yaw: f32) -> TrackInput {
        TrackInput {
            character: CharacterInput {
                intent: Intent {
                    forward,
                    ..Intent::default()
                },
                yaw,
                dt_s: DT,
            },
            view: None,
            boom_key: false,
            action_started: false,
        }
    }

    /// How close two places count as the same, m.
    const CLOSE: f32 = 1e-3;
    /// Records of 1/30 s in the lunge's 300 ms.
    const LUNGE_RECORDS: usize = 9;
    /// Records of 1/30 s in its first 100 ms.
    const FIRST_STEP_RECORDS: usize = 3;
    /// How long an attack may run before a movement key ends it, ms: longer than the lunge.
    const NO_KEYS_MS: f32 = 1_000.0;

    #[test]
    fn an_attack_moves_the_body_by_its_animation_toward_the_camera_not_by_the_keys() {
        // A quarter turn: the camera looks along -x.
        let yaw = std::f32::consts::FRAC_PI_2;
        let mut body = TrackedBody::new(start());
        body.start_attack(lunge(), NO_KEYS_MS, 0.0, &Flat, SEA);
        for _ in 0..LUNGE_RECORDS {
            body.record(keys(0, yaw), &Flat, SEA);
        }
        let at = body.position();
        assert!(
            (at.x - (start().x - 1.0)).abs() < CLOSE && (at.y - start().y).abs() < CLOSE,
            "{at:?}"
        );
        body.stand_at(start());
        body.record(keys(0, yaw), &Flat, SEA);
        assert_eq!(body.position(), start(), "a server move ends the attack");
    }

    #[test]
    fn a_movement_key_takes_the_body_back_from_the_attack_s_marker_on() {
        let mut keyed = TrackedBody::new(start());
        let mut still = TrackedBody::new(start());
        keyed.start_attack(lunge(), 100.0, 0.0, &Flat, SEA);
        still.start_attack(lunge(), 100.0, 0.0, &Flat, SEA);
        for _ in 0..FIRST_STEP_RECORDS {
            keyed.record(keys(1, 0.0), &Flat, SEA);
            still.record(keys(0, 0.0), &Flat, SEA);
        }
        assert!(
            (keyed.position().y - (start().y + 0.2)).abs() < CLOSE,
            "the animation, not the keys, before the marker: {:?}",
            keyed.position()
        );
        for _ in FIRST_STEP_RECORDS..LUNGE_RECORDS {
            keyed.record(keys(1, 0.0), &Flat, SEA);
            still.record(keys(0, 0.0), &Flat, SEA);
        }
        assert!((still.position().y - (start().y + 1.0)).abs() < CLOSE);
        assert!(
            (keyed.position().y - still.position().y).abs() > CLOSE,
            "the keys moved the body from the marker on"
        );
    }

    #[test]
    fn an_attack_halts_at_the_capsule_of_the_character_it_meets() {
        // The camera looks along +y; a character's 0.4 m capsule stands 1.2 m ahead.
        let characters = [crate::CharacterBody {
            x: start().x,
            y: start().y + 1.2,
            radius_m: 0.4,
        }];
        let ground = crate::AmongCharacters {
            ground: &Flat,
            characters: &characters,
        };
        let mut body = TrackedBody::new(start());
        body.start_attack(lunge(), NO_KEYS_MS, 0.0, &ground, SEA);
        for _ in 0..LUNGE_RECORDS {
            body.record(keys(0, 0.0), &ground, SEA);
        }
        let gone = body.position().y - start().y;
        assert!(
            gone < 1.2 - 0.4 - crate::character::PLAYER_RADIUS_M + CLOSE && gone > 0.0,
            "the capsules touch, the body goes no further: {gone}"
        );
    }

    #[test]
    fn an_attack_older_than_the_body_s_records_goes_on_from_where_it_is() {
        // Started 200 ms ago, before any record the body took: 0.6 m of its 1 m are behind it.
        let mut body = TrackedBody::new(start());
        body.start_attack(lunge(), NO_KEYS_MS, 200.0, &Flat, SEA);
        for _ in 0..FIRST_STEP_RECORDS {
            body.record(keys(0, 0.0), &Flat, SEA);
        }
        assert!(
            (body.position().y - (start().y + 0.4)).abs() < CLOSE,
            "{:?}",
            body.position()
        );
    }

    #[test]
    fn an_attack_that_starts_later_waits_for_its_start() {
        let mut body = TrackedBody::new(start());
        body.start_attack(lunge(), NO_KEYS_MS, -100.0, &Flat, SEA);
        for _ in 0..FIRST_STEP_RECORDS {
            body.record(keys(0, 0.0), &Flat, SEA);
        }
        assert_eq!(body.position(), start(), "not yet started");
        for _ in 0..FIRST_STEP_RECORDS {
            body.record(keys(0, 0.0), &Flat, SEA);
        }
        assert!(
            (body.position().y - (start().y + 0.2)).abs() < CLOSE,
            "{:?}",
            body.position()
        );
    }

    #[test]
    fn an_attack_the_server_learns_late_takes_its_records_again() {
        let mut late = TrackedBody::new(start());
        for _ in 0..FIRST_STEP_RECORDS {
            late.record(keys(1, 0.0), &Flat, SEA);
        }
        late.start_attack(lunge(), NO_KEYS_MS, 100.0, &Flat, SEA);
        let mut early = TrackedBody::new(start());
        early.start_attack(lunge(), NO_KEYS_MS, 0.0, &Flat, SEA);
        for _ in 0..FIRST_STEP_RECORDS {
            early.record(keys(1, 0.0), &Flat, SEA);
        }
        assert!(
            late.position().horizontal_distance(early.position()) < CLOSE,
            "{:?} against {:?}",
            late.position(),
            early.position()
        );
    }

    #[test]
    fn a_client_running_free_keeps_the_body_of_the_keys() {
        // The client runs north as the server does: its camera follows the same body.
        let mut body = TrackedBody::new(start());
        let mut client = Character::new(start());
        for _ in 0..90 {
            client.step(
                CharacterInput {
                    intent: Intent {
                        forward: 1,
                        ..Intent::default()
                    },
                    yaw: 0.0,
                    dt_s: DT,
                },
                &Flat,
                SEA,
            );
            let tracked = body.record(
                input(1, view_of(client.position(), BOOM, client.velocity())),
                &Flat,
                SEA,
            );
            assert!(!tracked.resynced, "{tracked:?}");
            assert!(tracked.position.horizontal_distance(client.position()) < 0.05);
            assert!(
                (tracked.position.z - GROUND_Z).abs() < 1e-3,
                "on the terrain"
            );
        }
    }

    #[test]
    fn a_rider_keeps_its_mount_s_speed_and_the_floor_under_a_saddle_camera() {
        use crate::camera_check::MOUNTED_CAMERA_LAG_S;
        use crate::character::{MountPace, MountPaces};
        /// Retail Moonshadow's road run, and retail's 20 input records a second.
        const RUN_MPS: f32 = 9.25;
        const RECORD_S: f32 = 0.05;
        /// A rider's camera above a walker's (retail Hermit's Rally: 3.91 m against 1.685 m).
        const SADDLE_M: f32 = 2.2;
        const RECORDS: u32 = 60;
        let pace = MountPace {
            run_mps: RUN_MPS,
            walk_mps: 5.0,
            slow_walk_mps: 2.0,
            acceleration_mps2: 1000.0,
            deceleration_mps2: 9.0,
            walk_acceleration_mps2: 15.0,
            walk_deceleration_mps2: 12.0,
        };
        let mut body = TrackedBody::new(start());
        body.mount(MountPaces {
            on_road: pace,
            off_road: pace,
            very_shallow_water: pace,
            shallow_water: pace,
            deep_water: pace,
            dash_speed_scale: 1.0,
            dash_acceleration_scale: 1.0,
        });
        let mut client = start();
        // `view_of` trails by the walker's lag: scale the motion to the rider's.
        let trail = [0.0, RUN_MPS * MOUNTED_CAMERA_LAG_S / CAMERA_LAG_S];
        for _ in 0..RECORDS {
            client.y += RUN_MPS * RECORD_S;
            let camera = Position {
                z: client.z + SADDLE_M,
                ..client
            };
            let mut record = input(1, view_of(camera, BOOM, trail));
            record.character.dt_s = RECORD_S;
            body.record(record, &Flat, SEA);
        }
        let at = body.position();
        assert!(
            (at.z - GROUND_Z).abs() < 1e-3,
            "on the floor, not the saddle: {at:?}"
        );
        assert!(
            at.horizontal_distance(client) < 0.5,
            "at the mount's speed, past the walking step of 0.36 m a record: {at:?} {client:?}"
        );
    }

    #[test]
    fn a_client_stopped_by_a_wall_holds_the_body_at_the_wall() {
        // The client's body stops at a wall 1 m north while the key stays down; the server's
        // runs on until the camera shows the body is not there.
        let mut body = TrackedBody::new(start());
        let wall = Position {
            y: start().y + 1.0,
            ..start()
        };
        let mut client = Character::new(start());
        const RECORDS: u32 = 120;
        let mut resyncs = 0;
        let mut last = start();
        let mut max_step = 0.0_f32;
        for _ in 0..RECORDS {
            if client.position().y < wall.y {
                client.step(
                    CharacterInput {
                        intent: Intent {
                            forward: 1,
                            ..Intent::default()
                        },
                        yaw: 0.0,
                        dt_s: DT,
                    },
                    &Flat,
                    SEA,
                );
            }
            let stopped = client.position().y >= wall.y;
            let velocity = if stopped { [0.0; 2] } else { client.velocity() };
            let tracked = body.record(
                input(1, view_of(client.position(), BOOM, velocity)),
                &Flat,
                SEA,
            );
            resyncs += u32::from(tracked.resynced);
            max_step = max_step.max(tracked.position.horizontal_distance(last));
            last = tracked.position;
        }
        // The server's body stays within the threshold, the camera's trail of a running body and
        // one record of running of the wall: it never runs away into the wall. Each pull-back
        // stops the character, so it takes records to drift past the threshold again; while it
        // is held there the corrections run on, each capped, so no step of the body is larger
        // than one record of running plus one correction.
        let bound = FOLLOW_FROM_M + (CAMERA_LAG_S + DT) * SPRINT_SPEED_MPS;
        assert!(last.horizontal_distance(wall) <= bound, "{last:?}");
        assert!(resyncs > 0, "{resyncs}");
        assert!(
            max_step <= SPRINT_SPEED_MPS * DT + FOLLOW_STEP_M + 0.01,
            "{max_step}"
        );
    }

    #[test]
    fn a_camera_that_holds_a_jumping_body_moves_it_on_the_ground_plane_only() {
        // Both run north and jump; the client's body stops at a wall 1 m ahead in the air, the
        // server's flies on until the camera puts it back.
        let keys = |jump: bool| CharacterInput {
            intent: Intent {
                forward: 1,
                jump,
                ..Intent::default()
            },
            yaw: 0.0,
            dt_s: DT,
        };
        let mut body = TrackedBody::new(start());
        let mut client = Character::new(start());
        for _ in 0..30 {
            client.step(keys(false), &Flat, SEA);
            body.record(
                input(1, view_of(client.position(), BOOM, client.velocity())),
                &Flat,
                SEA,
            );
        }
        let wall_y = client.position().y + 1.0;
        let bound = FOLLOW_FROM_M + (CAMERA_LAG_S + DT) * SPRINT_SPEED_MPS;
        let mut highest: f32 = GROUND_Z;
        for record in 0..15 {
            if client.position().y < wall_y {
                client.step(keys(record == 0), &Flat, SEA);
            }
            let stopped = client.position().y >= wall_y;
            let velocity = if stopped { [0.0; 2] } else { client.velocity() };
            let tracked = body.record(
                TrackInput {
                    character: keys(record == 0),
                    ..input(1, view_of(client.position(), BOOM, velocity))
                },
                &Flat,
                SEA,
            );
            assert!(
                tracked.position.horizontal_distance(client.position()) <= bound,
                "record {record}: {tracked:?}"
            );
            highest = highest.max(tracked.position.z);
        }
        assert!(highest > GROUND_Z + 0.5, "the jump keeps its own height");
    }

    #[test]
    fn a_client_on_steps_the_level_lacks_takes_the_camera_height() {
        // Run 17, the tutorial cave: the client climbs steps that the level collision does not
        // have; the server's body climbs with its camera instead of staying at the foot.
        const RISE_PER_RECORD_M: f32 = 0.1;
        const RECORDS: u32 = 40;
        let mut body = TrackedBody::new(start());
        let mut client = start();
        for _ in 0..RECORDS {
            client.y += SPRINT_SPEED_MPS * DT;
            client.z += RISE_PER_RECORD_M;
            let velocity = [0.0, SPRINT_SPEED_MPS];
            body.record(input(1, view_of(client, BOOM, velocity)), &Flat, SEA);
        }
        let at = body.position();
        assert!(
            (at.z - client.z).abs() < CAMERA_FLOOR_SPREAD_M,
            "the body climbed with the client: {at:?} {client:?}"
        );
    }

    /// A ledge: a floor [`LEDGE_M`] above the flat ground south of [`start`]'s y plus 1 m.
    struct Ledge;

    /// The height of [`Ledge`] above the ground, m.
    const LEDGE_M: f32 = 15.0;

    impl Ground for Ledge {
        fn height_at(&self, _: f32, y: f32) -> Option<f32> {
            Some(if y < start().y + 1.0 {
                GROUND_Z + LEDGE_M
            } else {
                GROUND_Z
            })
        }
    }

    #[test]
    fn a_client_that_runs_off_a_ledge_falls_by_the_physics() {
        // A real fall must stay a fall (its landing is the fall damage's drop), not a climb down
        // the camera.
        let top = Position {
            z: GROUND_Z + LEDGE_M,
            ..start()
        };
        let mut body = TrackedBody::new(top);
        let mut client = Character::new(top);
        let mut airborne = false;
        for _ in 0..90 {
            client.step(
                CharacterInput {
                    intent: Intent {
                        forward: 1,
                        ..Intent::default()
                    },
                    yaw: 0.0,
                    dt_s: DT,
                },
                &Ledge,
                SEA,
            );
            let tracked = body.record(
                input(1, view_of(client.position(), BOOM, client.velocity())),
                &Ledge,
                SEA,
            );
            airborne |= !tracked.on_floor;
        }
        assert!(airborne, "the body fell");
        assert!(
            (body.position().z - GROUND_Z).abs() < 1e-3,
            "it landed: {:?}",
            body.position()
        );
    }

    /// Stairs: a floor that rises 0.25 m per 0.5 m north of [`start`], over the flat ground.
    struct Stairs;

    impl Ground for Stairs {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(GROUND_Z)
        }

        fn floor_at(&self, _: f32, y: f32, z: f32) -> Option<f32> {
            const RISER_M: f32 = 0.25;
            const TREAD_M: f32 = 0.5;
            let steps = ((y - start().y) / TREAD_M).floor().max(0.0);
            let step = GROUND_Z + steps * RISER_M;
            Some(if step <= z + 1.3 { step } else { GROUND_Z })
        }
    }

    #[test]
    fn the_body_climbs_known_stairs_whatever_height_the_camera_gives() {
        // The camera's body 0.5 m under the stairs: the floor is the ground's, not the camera's.
        const CAMERA_ERROR_M: f32 = -0.5;
        let mut body = TrackedBody::new(start());
        let mut runner = Character::new(start());
        for _ in 0..40 {
            let keys = input(1, view_of(start(), BOOM, [0.0; 2]));
            runner.step(keys.character, &Stairs, SEA);
            let client = runner.position();
            let shown = Position {
                z: client.z + CAMERA_ERROR_M,
                ..client
            };
            let tracked = body.record(
                input(1, view_of(shown, BOOM, runner.velocity())),
                &Stairs,
                SEA,
            );
            let floor = Stairs
                .floor_at(tracked.position.x, tracked.position.y, tracked.position.z)
                .expect("floor");
            assert!((tracked.position.z - floor).abs() < 1e-3, "{tracked:?}");
        }
        assert!(body.position().z > GROUND_Z + 1.0, "{:?}", body.position());
    }

    /// The tutorial galleon's upper deck ([`deck_triangles`]) over the `ftue_v2` terrain.
    fn galleon() -> crate::LevelGround<crate::test_support::FlatGround> {
        use nw_world::collision::{Body, Geometry, IDENTITY_AFFINE, LevelCollision};
        use nw_world::collision_index::CollisionIndex;
        /// The `ftue_v2` terrain under the ship, m.
        const SHIP_TERRAIN_M: f32 = 50.0;
        let triangles = deck_triangles();
        let points: Vec<[f32; 3]> = triangles.iter().flatten().copied().collect();
        let corners = (0..triangles.len())
            .map(|index| {
                let first = u16::try_from(index * 3).expect("a few triangles");
                [first, first + 1, first + 2]
            })
            .collect();
        let collision = CollisionIndex::new(LevelCollision {
            level: "ftue_v2".to_owned(),
            geometries: vec![Geometry::Mesh {
                points,
                triangles: corners,
            }],
            bodies: vec![Body {
                geometry: 0,
                place: IDENTITY_AFFINE,
            }],
            ..LevelCollision::default()
        });
        crate::LevelGround::new(
            crate::test_support::FlatGround(SHIP_TERRAIN_M),
            std::sync::Arc::new(collision),
        )
    }

    /// The triangles of `testdata/ftue-galleon-deck.txt`.
    fn deck_triangles() -> Vec<[[f32; 3]; 3]> {
        include_str!("testdata/ftue-galleon-deck.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| {
                let v: Vec<f32> = line
                    .split_whitespace()
                    .map(|value| value.parse().expect("number"))
                    .collect();
                [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]
            })
            .collect()
    }

    /// The place of the block under the tutorial beach's stair landing: body 6 of the `ftue_v2`
    /// level collision, a box of half size 1 (4 m high from 19.39 m; retail walks onto its top
    /// at 23.39 m from the stairs).
    const BEACH_BLOCK: [[f32; 4]; 3] = [
        [-8.812_316, 1.654_394, 0.0, 719.742_25],
        [-4.726_837, -3.084_312, 0.0, 368.216_64],
        [0.0, 0.0, 2.0, 21.390_278],
    ];
    /// The beach sand under run 15's own places by the block, m.
    const BEACH_SAND_M: f32 = 19.945;

    /// The block over the sand.
    fn beach_block() -> crate::LevelGround<crate::test_support::FlatGround> {
        use nw_world::collision::{Body, Geometry, LevelCollision};
        use nw_world::collision_index::CollisionIndex;
        let collision = CollisionIndex::new(LevelCollision {
            level: "ftue_v2".to_owned(),
            geometries: vec![Geometry::Box {
                half_extents: [1.0; 3],
            }],
            bodies: vec![Body {
                geometry: 0,
                place: BEACH_BLOCK,
            }],
            ..LevelCollision::default()
        });
        crate::LevelGround::new(
            crate::test_support::FlatGround(BEACH_SAND_M),
            std::sync::Arc::new(collision),
        )
    }

    /// Whether `at` stands in the block's ground-plane outline.
    fn in_beach_block(at: Position) -> bool {
        let [x_row, y_row, _] = BEACH_BLOCK;
        let offset = [at.x - x_row[3], at.y - y_row[3]];
        [0, 1].into_iter().all(|axis| {
            let column = [x_row[axis], y_row[axis]];
            let share = (offset[0] * column[0] + offset[1] * column[1])
                / (column[0] * column[0] + column[1] * column[1]);
            share.abs() <= 1.0
        })
    }

    #[test]
    fn the_run_15_camera_does_not_carry_the_body_into_the_beach_block() {
        // Run 15 (2026-09-22, 03:49:22Z): by the beach stairs the camera's body ran north into
        // the block, 0.7 to 1.3 m ahead of the server's, and the resyncs put the body inside it;
        // its client was pushed into the solid and stuck. These are the server's own places of
        // those states, from 715.55, 359.17 on; the client's capsule cannot be inside the block.
        const RUN_15_PLACES: [[f32; 2]; 9] = [
            [715.008, 359.864],
            [715.729, 360.947],
            [716.043, 361.653],
            [715.887, 362.202],
            [715.746, 362.924],
            [715.966, 363.065],
            [715.887, 364.053],
            [716.039, 365.316],
            [715.673, 366.556],
        ];
        /// Records each place shows for (the states came 33 to 65 ms apart).
        const RECORDS_EACH: usize = 2;
        /// Records the last place holds after the run.
        const HOLD_RECORDS: usize = 30;
        let start = Position {
            x: 715.551,
            y: 359.173,
            z: BEACH_SAND_M,
        };
        let ground = beach_block();
        assert!(in_beach_block(Position {
            x: RUN_15_PLACES[8][0],
            y: RUN_15_PLACES[8][1],
            ..start
        }));
        assert!(!in_beach_block(start));
        let mut body = TrackedBody::new(start);
        let shown = RUN_15_PLACES
            .iter()
            .flat_map(|place| std::iter::repeat_n(*place, RECORDS_EACH))
            .chain(std::iter::repeat_n(RUN_15_PLACES[8], HOLD_RECORDS));
        let mut last = start;
        for (record, [x, y]) in shown.enumerate() {
            let client = Position { x, y, ..start };
            let tracked = body.record(input(1, view_of(client, BOOM, [0.0; 2])), &ground, SEA);
            assert!(
                !in_beach_block(tracked.position),
                "record {record}: {tracked:?}"
            );
            last = tracked.position;
        }
        assert!(last.y > start.y + 2.0, "it went up to the block: {last:?}");
    }

    #[test]
    fn a_client_that_climbed_onto_the_block_takes_the_body_onto_its_top() {
        // A mantle or a vault: the client shows its body on the block's top, past its side.
        /// The block's top, m.
        const TOP_M: f32 = 23.39;
        /// Records at rest, then records on the top.
        const REST_RECORDS: usize = 10;
        const TOP_RECORDS: usize = 20;
        let below = Position {
            x: 716.043,
            y: 361.653,
            z: BEACH_SAND_M,
        };
        let on_top = Position {
            x: 715.746,
            y: 362.924,
            z: TOP_M,
        };
        assert!(!in_beach_block(below) && in_beach_block(on_top));
        let ground = beach_block();
        let mut body = TrackedBody::new(below);
        for _ in 0..REST_RECORDS {
            body.record(input(0, view_of(below, BOOM, [0.0; 2])), &ground, SEA);
        }
        let mut last = below;
        for _ in 0..TOP_RECORDS {
            last = body
                .record(input(1, view_of(on_top, BOOM, [0.0; 2])), &ground, SEA)
                .position;
        }
        assert!(in_beach_block(last), "{last:?}");
        assert!((last.z - TOP_M).abs() < 0.01, "on the top: {last:?}");
    }

    #[test]
    fn where_the_level_has_no_data_the_body_takes_the_camera_height() {
        let place = Position { z: 40.0, ..start() };
        let mut body = TrackedBody::new(start());
        let tracked = body.record(
            input(0, view_of(place, BOOM, [0.0; 2])),
            &crate::test_support::NoGround,
            SEA,
        );
        assert!(tracked.consistent);
        assert!((tracked.position.z - place.z).abs() < 0.01, "{tracked:?}");
    }

    /// The tutorial start: retail's own place before any key.
    const RETAIL_START: Position = Position {
        x: 478.5,
        y: 849.7,
        z: 89.382,
    };

    #[test]
    fn the_retail_start_stands_on_the_galleon_deck() {
        /// The own place's step: 4/255 m.
        const PLACE_STEP_M: f32 = 4.0 / 255.0;
        let deck = galleon()
            .floor_at(RETAIL_START.x, RETAIL_START.y, RETAIL_START.z)
            .expect("deck");
        assert!((deck - RETAIL_START.z).abs() < PLACE_STEP_M, "{deck}");
    }

    #[test]
    fn the_run_14_ship_fight_keeps_the_body_on_the_deck() {
        // Run 14 (2026-09-20): in the first sailor fight the solve's boom drifted 1.1 m long in
        // a fast turn of the view, the camera's body went down to 0.55 m under the deck, and a
        // body that took the camera's height went through the deck (89.38 to 88.84 m in these
        // records, then 82.1 m). The deck of the level's collision is the floor.
        const TUTORIAL_SEA_M: f32 = 20.0;
        /// The deck's lowest point in the fight's area, m.
        const DECK_LOW_M: f32 = 89.27;
        let replay = parse_replay(include_str!("testdata/run14-ship-fight.txt"));
        let ground = galleon();
        let mut body = TrackedBody::new(replay.start);
        for (record, (input, _)) in replay.records.iter().enumerate() {
            let tracked = body.record(*input, &ground, TUTORIAL_SEA_M);
            let deck = ground
                .floor_at(tracked.position.x, tracked.position.y, tracked.position.z)
                .expect("deck");
            assert!(
                (tracked.position.z - deck).abs() < 1e-3 && deck >= DECK_LOW_M,
                "record {record}: {tracked:?}"
            );
        }
    }

    #[test]
    fn the_run_19_attack_camera_under_the_deck_keeps_the_body_on_the_deck() {
        // Run 19 (2026-09-24), the second sailor fight: while the player attacked, the camera's
        // body went 1.5 m under the deck, past the step-up search from it; the body took the
        // floor under the deck and went down to the sea (89.46 to 50 m in 14 s).
        const TUTORIAL_SEA_M: f32 = 20.0;
        const CAMERA_UNDER_DECK_M: f32 = 1.5;
        const WALK_RECORDS: u32 = 15;
        const ATTACK_RECORDS: u32 = 30;
        /// The back key: south from the start, as run 19 went, where the deck reaches 13 m.
        const BACK: i8 = -1;
        let ground = galleon();
        let start = Position {
            z: ground
                .floor_at(RETAIL_START.x, RETAIL_START.y, RETAIL_START.z)
                .expect("deck"),
            ..RETAIL_START
        };
        let mut body = TrackedBody::new(start);
        let mut client = Character::new(start);
        for record in 0..WALK_RECORDS + ATTACK_RECORDS {
            let attacking = record >= WALK_RECORDS;
            client.step(
                CharacterInput {
                    intent: Intent {
                        forward: BACK,
                        ..Intent::default()
                    },
                    yaw: 0.0,
                    dt_s: DT,
                },
                &ground,
                TUTORIAL_SEA_M,
            );
            let on_deck = client.position();
            assert_eq!(
                ground.floor_at(on_deck.x, on_deck.y, on_deck.z),
                Some(on_deck.z),
                "record {record}: the client walks on the deck"
            );
            let mut seen = on_deck;
            if attacking {
                seen.z -= CAMERA_UNDER_DECK_M;
            }
            let tracked = body.record(
                TrackInput {
                    action_started: record == WALK_RECORDS,
                    ..input(BACK, view_of(seen, BOOM, client.velocity()))
                },
                &ground,
                TUTORIAL_SEA_M,
            );
            assert!(
                (tracked.position.z - client.position().z).abs() < 1e-3,
                "record {record}: {tracked:?} client {:?}",
                client.position()
            );
        }
    }

    #[test]
    fn a_long_boom_still_puts_the_body_on_the_terrain() {
        // Run 12 (2026-09-18): the client's zoom held an 8.1 m boom and the old camera fit put
        // the body 0.7 to 1.6 m above the boar's terrain. The terrain is within reach of the
        // camera's height, so the body stands on it.
        const RUN_12_BOOM_M: f32 = 8.1;
        let mut body = TrackedBody::new(start());
        for _ in 0..10 {
            let tracked = body.record(
                input(0, view_of(start(), RUN_12_BOOM_M, [0.0; 2])),
                &Flat,
                SEA,
            );
            assert!((tracked.position.z - GROUND_Z).abs() < 1e-3, "{tracked:?}");
        }
        assert!((body.boom_m().expect("fitted") - RUN_12_BOOM_M).abs() < 0.01);
    }

    #[test]
    fn a_camera_that_does_not_follow_leaves_the_keys_alone() {
        let mut body = TrackedBody::new(start());
        body.record(input(0, view_of(start(), BOOM, [0.0; 2])), &Flat, SEA);
        let far = Position {
            x: start().x + 50.0,
            ..start()
        };
        let mut cutscene = view_of(far, BOOM, [0.0; 2]);
        cutscene.yaw = 2.0;
        let tracked = body.record(input(1, cutscene), &Flat, SEA);
        assert!(!tracked.consistent && !tracked.resynced);
        assert!((tracked.position.y - start().y - ACCELERATION_MPS2 * DT * DT).abs() < 1e-4);
    }

    #[test]
    fn a_server_move_starts_the_camera_again() {
        let mut body = TrackedBody::new(start());
        body.record(input(0, view_of(start(), BOOM, [0.0; 2])), &Flat, SEA);
        let moved = Position {
            x: 500.0,
            ..start()
        };
        body.stand_at(moved);
        assert_eq!(body.boom_m(), None);
        let tracked = body.record(input(0, view_of(moved, 6.0, [0.0; 2])), &Flat, SEA);
        assert!(tracked.consistent && !tracked.resynced);
        assert!(tracked.position.horizontal_distance(moved) < 0.01);
    }

    /// The tutorial sea (the `ftue_v2` ocean level) and the terrain under the move into it, m.
    const TUTORIAL_SEA_M: f32 = 20.0;
    const TUTORIAL_SEABED_M: f32 = 10.82;
    /// Records that the test client shows the server's place late: the client shows the
    /// server's own place, a few records after the server moved it.
    const CLIENT_LAG_RECORDS: usize = 3;
    /// Where the camera's body was above the landed retail place in the records after its drop
    /// into the tutorial sea (newchar records 3110 to 3114), m.
    const RETAIL_CAMERA_TRAIL_M: [f32; 5] = [0.75, 0.48, 0.36, 0.20, 0.15];

    struct TutorialSea;

    impl Ground for TutorialSea {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(TUTORIAL_SEABED_M)
        }
    }

    /// A client at rest that shows the server's places `CLIENT_LAG_RECORDS` late.
    struct LateClient {
        shown: std::collections::VecDeque<Position>,
    }

    impl LateClient {
        fn at(place: Position) -> Self {
            Self {
                shown: std::iter::repeat_n(place, CLIENT_LAG_RECORDS).collect(),
            }
        }

        /// The camera of the place it shows now; it gets `server` for later.
        fn camera(&mut self, server: Position) -> CameraView {
            self.shown.push_back(server);
            let shown = self.shown.pop_front().expect("lagged places");
            view_of(shown, BOOM, [0.0; 2])
        }
    }

    #[test]
    fn a_dropped_body_falls_while_its_client_s_camera_shows_it_late() {
        // Run 13: after the move into the water the server's body took the height of the camera
        // of a client that showed the server's place, and both hung 7.6 m above the sea.
        let placed = Position {
            z: 26.16,
            ..start()
        };
        let mut body = TrackedBody::new(start());
        body.drop_at(placed);
        let mut free = Character::new(placed);
        free.drop_at(placed);
        let mut client = LateClient::at(placed);
        let mut server = placed;
        for record in 0..60 {
            let tracked = body.record(
                input(0, client.camera(server)),
                &TutorialSea,
                TUTORIAL_SEA_M,
            );
            free.step(
                input(0, view_of(placed, BOOM, [0.0; 2])).character,
                &TutorialSea,
                TUTORIAL_SEA_M,
            );
            assert!(
                (tracked.position.z - free.position().z).abs() < 1e-4,
                "record {record}: the camera held the drop at {:?}",
                tracked.position
            );
            assert_eq!(tracked.on_floor, free.is_grounded(), "no floor in the air");
            server = tracked.position;
        }
        assert!(
            (server.z - swim_floor(TUTORIAL_SEA_M)).abs() < 1e-4,
            "{server:?}"
        );
    }

    #[test]
    fn a_camera_settling_over_the_sea_keeps_the_landed_body_on_the_swim_floor() {
        let swim = swim_floor(TUTORIAL_SEA_M);
        let placed = Position {
            z: swim + 3.0,
            ..start()
        };
        let mut body = TrackedBody::new(start());
        body.drop_at(placed);
        let mut client = LateClient::at(placed);
        let mut server = placed;
        while !body.character.is_grounded() {
            server = body
                .record(
                    input(0, client.camera(server)),
                    &TutorialSea,
                    TUTORIAL_SEA_M,
                )
                .position;
        }
        let trail = RETAIL_CAMERA_TRAIL_M.into_iter().chain([0.0]);
        for above in trail {
            let camera = view_of(
                Position {
                    z: swim + above,
                    ..server
                },
                BOOM,
                [0.0; 2],
            );
            let tracked = body.record(input(0, camera), &TutorialSea, TUTORIAL_SEA_M);
            assert!(
                (tracked.position.z - swim).abs() < 1e-4,
                "{above} m above: {tracked:?}"
            );
        }
    }

    #[test]
    fn a_swimming_body_does_not_snap_back_to_a_trailing_camera() {
        // Live 2026-09-24: after the sink the player swam forward and the camera
        // trail pulled the body backward each record.
        let swim = swim_floor(TUTORIAL_SEA_M);
        let start = Position {
            x: 793.84,
            y: 337.42,
            z: swim,
        };
        let mut body = TrackedBody::new(start);
        body.drop_at(start);
        let mut at = start;
        for _ in 0..8 {
            at = body
                .record(
                    input(0, view_of(at, BOOM, [0.0; 2])),
                    &TutorialSea,
                    TUTORIAL_SEA_M,
                )
                .position;
        }
        assert!(body.is_swimming() || (at.z - swim).abs() < 1e-3, "{at:?}");
        let behind = Position {
            y: at.y - 4.0,
            ..at
        };
        let tracked = body.record(
            input(1, view_of(behind, BOOM, [0.0; 2])),
            &TutorialSea,
            TUTORIAL_SEA_M,
        );
        assert!(!tracked.resynced, "{tracked:?}");
        assert!(
            tracked.position.y > at.y,
            "the camera pulled it back: {at:?} -> {:?}",
            tracked.position
        );
    }

    /// The retail replay data: a 1 m terrain grid and input records with the retail places.
    struct Replay {
        x0: f32,
        y0: f32,
        columns: usize,
        heights: Vec<f32>,
        start: Position,
        records: Vec<(TrackInput, Option<Position>)>,
    }

    impl Ground for Replay {
        fn height_at(&self, x: f32, y: f32) -> Option<f32> {
            let (column, row) = (x - self.x0, y - self.y0);
            if column < 0.0 || row < 0.0 {
                return None;
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a grid of a few dozen metres"
            )]
            let (i, j) = (column.floor() as usize, row.floor() as usize);
            let height = |i: usize, j: usize| self.heights.get(j * self.columns + i).copied();
            if i + 1 >= self.columns {
                return None;
            }
            let (u, w) = (column.fract(), row.fract());
            let south = height(i, j)? * (1.0 - u) + height(i + 1, j)? * u;
            let north = height(i, j + 1)? * (1.0 - u) + height(i + 1, j + 1)? * u;
            Some(south * (1.0 - w) + north * w)
        }
    }

    fn parse_replay(text: &str) -> Replay {
        let mut lines = text.lines().filter(|line| !line.starts_with('#'));
        let numbers = |line: &str| -> Vec<f32> {
            line.split_whitespace()
                .filter_map(|word| word.parse().ok())
                .collect()
        };
        let grid = numbers(lines.next().expect("grid"));
        let [x0, y0, columns, rows] = grid[..] else {
            panic!("grid line: {grid:?}");
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a grid of a few dozen metres"
        )]
        let (columns, rows) = (columns as usize, rows as usize);
        let heights: Vec<f32> = (0..rows)
            .flat_map(|_| numbers(lines.next().expect("heights")))
            .collect();
        let start = numbers(lines.next().expect("start"));
        let start = Position {
            x: start[0],
            y: start[1],
            z: start[2],
        };
        let flag = |value: f32| value > 0.5;
        let records = lines
            .map(|line| {
                let v = numbers(line);
                let (keys, place) = v[6..].split_at(8);
                let intent = Intent {
                    jump: flag(keys[4]),
                    walk_toggle: flag(keys[5]),
                    ..Intent::of_keys(flag(keys[0]), flag(keys[1]), flag(keys[2]), flag(keys[3]))
                };
                let input = TrackInput {
                    character: CharacterInput {
                        intent,
                        yaw: v[1],
                        dt_s: v[0],
                    },
                    view: Some(CameraView::new(
                        Position {
                            x: v[3],
                            y: v[4],
                            z: v[5],
                        },
                        v[1],
                        v[2],
                    )),
                    boom_key: flag(keys[6]),
                    action_started: flag(keys[7]),
                };
                let place = (place.len() == 3).then(|| Position {
                    x: place[0],
                    y: place[1],
                    z: place[2],
                });
                (input, place)
            })
            .collect();
        Replay {
            x0,
            y0,
            columns,
            heights,
            start,
            records,
        }
    }

    #[test]
    fn a_retail_open_world_walk_replays_within_the_camera_models_accuracy() {
        // Retail newchar capture, open world, 1,417 records (47 s): running, turning, stops,
        // zooms and actions. The server's body from the retail keys and cameras against retail's
        // own places (movement study 2026-09-20: the whole open world part replayed with a median
        // of 0.16 m; the body of the camera fit erred by a median 0.22 m while moving).
        const OPEN_WORLD_SEA_M: f32 = 40.0;
        const MEDIAN_M: f32 = 0.2;
        const NINETIETH_M: f32 = 1.2;
        let replay = parse_replay(include_str!("testdata/retail-open-world-walk.txt"));
        let mut body = TrackedBody::new(replay.start);
        let mut truth = replay.start;
        let mut errors: Vec<f32> = replay
            .records
            .iter()
            .map(|(input, place)| {
                let tracked = body.record(*input, &replay, OPEN_WORLD_SEA_M);
                truth = place.unwrap_or(truth);
                tracked.position.horizontal_distance(truth)
            })
            .collect();
        errors.sort_by(f32::total_cmp);
        let median = errors[errors.len() / 2];
        let ninetieth = errors[errors.len() * 9 / 10];
        assert!(median < MEDIAN_M, "median {median} m");
        assert!(ninetieth < NINETIETH_M, "90th percentile {ninetieth} m");
    }

    #[test]
    fn the_sent_places_of_a_retail_walk_step_like_retails_own() {
        // The place the server sends is the body: it must step like retail's own places
        // step (at most 0.36 m in this replay), never in one snap. With the whole-way
        // snap this replay sent steps of 0.45 m and the live tutorial ship (threshold
        // 0.57 m) sent 0.8 m (live 2026-09-22).
        const OPEN_WORLD_SEA_M: f32 = 40.0;
        /// The largest step of a retail own place in this replay, m, with headroom.
        const RETAIL_STEP_M: f32 = 0.40;
        const MEDIAN_M: f32 = 0.2;
        let replay = parse_replay(include_str!("testdata/retail-open-world-walk.txt"));
        let mut body = TrackedBody::new(replay.start);
        let mut truth = replay.start;
        let mut before_body = replay.start;
        let mut body_steps: Vec<f32> = Vec::new();
        let mut errors: Vec<f32> = Vec::new();
        for (input, place) in &replay.records {
            let tracked = body.record(*input, &replay, OPEN_WORLD_SEA_M);
            truth = place.unwrap_or(truth);
            body_steps.push(tracked.position.horizontal_distance(before_body));
            before_body = tracked.position;
            errors.push(tracked.position.horizontal_distance(truth));
        }
        let largest = body_steps.iter().copied().fold(0.0_f32, f32::max);
        assert!(largest < RETAIL_STEP_M, "largest step {largest} m");
        errors.sort_by(f32::total_cmp);
        let median = errors[errors.len() / 2];
        assert!(median < MEDIAN_M, "median {median} m");
    }
}
