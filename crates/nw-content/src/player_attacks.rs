// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player attack table (`player-attacks-<build>.json`, made by `content-tool player-attacks`
//! from the game paks; combat audit, 2026-09-18).
//!
//! A player's weapon puts its `MannequinTag` tags on the character (its stance). The stance picks
//! the Mannequin fragments of the basic attacks: `Attack_Start_Charge` starts at the attack key's
//! press and holds the transitions to the light attack (the key let go), the heavy attack (let
//! go after the heavy hold) and the charged attack (held to the end of the charge). Each attack's
//! fragment holds when it hits: a melee attack's `CAGE-Damage` window and its `Homing`, a ranged
//! attack's `CAGE-RangedAttack`, a magic attack's `CAGE-CastSpell` (the projectile of its
//! `SpellData` row). The ranged attack database (`rangedattackdatabase.radb`) gives a
//! projectile's range and radius, its slice how it flies. The stance's `Dodge_Roll` gives the
//! roll's invulnerability, its `Use` fragments when an item takes effect.
//!
//! Melee shapes (build 25810808, 2026-09-18): each `CAGE-Damage` clip casts its shape on the
//! weapon (the attach database's clips play in the weapon's context): the weapon's joint, its
//! offset and rotation, sampled at each clip frame of the attack's animation over the clip's
//! window, as a transform in the character's root (`Xform`) space ([`MeleeShape`]). Combos: an
//! attack's `CAGE-Transition*` clips to a later charge action, on `CanAttack_Heavy` (the attack key
//! pressed), start the next step of the stance's chain ([`ComboLink`], [`ChargeStep`]). A stance
//! tag that a status effect sets (`SetMannequinTag` of an action active while the effect is on,
//! `GenericTagA` for `Greatsword_Stance_Offense`) gives a variant of the stance ([`Stance::variants`]).
//!
//! Motion: an attack action with an `AnimationDrivenMotion` activity moves the player by its
//! animation's root motion, not by the movement keys ([`AttackMotion`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nw_formats::objectstream::lowercase_crc32;
use nw_formats::rigid::Rigid;
use serde::{Deserialize, Serialize};

const FILE_PREFIX: &str = "player-attacks-";
const FILE_SUFFIX: &str = ".json";
/// The separator of a weapon row's `MannequinTag` cell (`2H_Melee,GreatSword`).
const MANNEQUIN_TAG_SEPARATOR: char = ',';
/// The separator of a stance's tags in its key text.
const KEY_SEPARATOR: &str = "+";
/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;
/// Half, for the distance of a constant acceleration.
const HALF: f32 = 0.5;

/// The path of the table of `client_build` in `dir`.
pub fn player_attacks_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!("{FILE_PREFIX}{client_build}{FILE_SUFFIX}"))
}

/// The tags of a weapon row's `MannequinTag` cell, trimmed, in order.
pub fn stance_tags(mannequin_tag: &str) -> Vec<String> {
    mannequin_tag
        .split(MANNEQUIN_TAG_SEPARATOR)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The key of the stance of a weapon whose `MannequinTag` cell is `mannequin_tag`: the CRC-32 of
/// its tags, lower case, sorted, joined by `+` (the tags of the empty cell: no weapon).
pub fn stance_key(mannequin_tag: &str) -> u32 {
    let mut tags: Vec<String> = stance_tags(mannequin_tag)
        .into_iter()
        .map(|tag| tag.to_lowercase())
        .collect();
    tags.sort();
    lowercase_crc32(&tags.join(KEY_SEPARATOR))
}

/// A bad table file.
#[derive(Debug, thiserror::Error)]
pub enum PlayerAttacksError {
    /// The file cannot be read or written.
    #[error("player attack table {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file is not the table's JSON.
    #[error("player attack table: {0}")]
    Json(#[from] serde_json::Error),
}

/// An attack's `Homing` clip: it turns the attacker to the best target in its reach and moves it
/// in (Observed parameters).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Homing {
    /// `Radius`: how far a target may be.
    pub radius_m: f32,
    /// `MaxAngle`: how far off the facing a target may be, degrees.
    pub max_angle_deg: f32,
    /// `Height`: how far apart in height a target may be.
    pub height_m: f32,
    /// `RadiusWeight`: the weight of a target's share of the radius in its score.
    pub radius_weight: f32,
    /// `AngleWeight`: the weight of a target's share of the angle in its score.
    pub angle_weight: f32,
    /// `UseRadiusforAITarget`: the distance to an AI target counts to its body's edge.
    pub use_radius_for_ai_target: bool,
}

/// How a projectile flies (its slice's projectile component, Observed fields).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Flight {
    /// `m_baseSpeed`: its speed when it leaves the weapon.
    pub base_speed_mps: f32,
    /// `m_extraSpeed`: the speed it gains...
    pub extra_speed_mps: f32,
    /// ...over `m_timeForMaxSpeed` (Inferred: evenly).
    pub time_for_max_speed_s: f32,
    /// The downward part of `m_accel` (Inferred: gravity).
    pub gravity_mps2: f32,
    /// `m_flyingLifetime`: how long it flies before it is gone.
    pub lifetime_s: f32,
}

impl Flight {
    /// How far it flies in `time_s`.
    pub fn distance_at(&self, time_s: f32) -> f32 {
        let ramp_s = time_s.min(self.time_for_max_speed_s);
        let ramp_share = if self.time_for_max_speed_s > 0.0 {
            ramp_s / self.time_for_max_speed_s
        } else {
            1.0
        };
        let ramp = self.base_speed_mps * ramp_s + HALF * self.extra_speed_mps * ramp_share * ramp_s;
        let after_s = (time_s - ramp_s).max(0.0);
        ramp + (self.base_speed_mps + self.extra_speed_mps) * after_s
    }

    /// How long it takes to fly `distance_m`; `None` when it is gone before.
    pub fn time_to(&self, distance_m: f32) -> Option<f32> {
        if self.distance_at(self.lifetime_s) < distance_m {
            return None;
        }
        // The distance grows with the time: halve the lifetime down to a millisecond.
        let (mut low, mut high) = (0.0_f32, self.lifetime_s);
        while (high - low) * MS_PER_SECOND > 1.0 {
            let middle = (low + high) * HALF;
            if self.distance_at(middle) < distance_m {
                low = middle;
            } else {
                high = middle;
            }
        }
        Some(high)
    }

    /// How far it fell after `time_s`.
    pub fn drop_at(&self, time_s: f32) -> f32 {
        HALF * self.gravity_mps2 * time_s * time_s
    }
}

/// When and how a basic attack hits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AttackHit {
    /// A melee attack: its `CAGE-Damage` clips are live in this window after the attack starts,
    /// and its homing finds the target.
    Melee {
        /// The first clip's start and the last clip's end, ms.
        window_ms: [u64; 2],
        /// Its `Homing` clip, when it has one.
        homing: Option<Homing>,
        /// Each clip's shape and its path over the clip's window.
        #[serde(default)]
        shapes: Vec<MeleeShape>,
    },
    /// A projectile that leaves the weapon `fire_ms` after the attack starts.
    Projectile {
        /// When it leaves the weapon, ms.
        fire_ms: u64,
        /// `m_maxRange`.
        range_m: f32,
        /// `m_projectileRadius`.
        radius_m: f32,
        /// How it flies.
        flight: Flight,
    },
    /// A hit scan `fire_ms` after the attack starts (`m_isHitScan`).
    HitScan {
        /// When it fires, ms.
        fire_ms: u64,
        /// `m_maxRange`.
        range_m: f32,
    },
}

/// One basic attack of a stance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BasicAttack {
    /// The CAGE action (`Attack_Primary_C1`).
    pub action: String,
    /// Its fragment (`Attack_Primary`).
    pub fragment: String,
    /// The `DamageData` row of its hit.
    pub damage_row: String,
    /// When and how it hits.
    pub hit: AttackHit,
    /// How long its fragment plays: its longest animation layer, ms; 0 when it has none.
    #[serde(default)]
    pub length_ms: u64,
    /// The presses of the attack key that start the next combo step.
    #[serde(default)]
    pub links: Vec<ComboLink>,
    /// The start of its fragment's `CAGE-Named` `CanTransition` clip, ms: the action list's
    /// "Disable AttackAction on CanTransition" unsets `AttackAction` there, and an attack action
    /// starts only without it (`playeractions_firearms.actionlist`,
    /// `Firearm_Shoot_HipFire_FullBody`). `None`: the fragment has no such clip.
    #[serde(default)]
    pub can_transition_ms: Option<u64>,
    /// The windows of its fragment's `CAGE-Named` `CanDodgeCancel` clips, ms: a dodge starts
    /// only in them while the attack plays (`IfCanDodge`; retail tutorial and open world, the
    /// sword light attack: dodges at 17, 566 and 1182 ms in the windows 0-200 and 500-1420 were
    /// taken, the three at 252 to 414 ms in its `CanDodgeBuffer` 200-500 refused). `None`: the
    /// fragment has no such clip, a dodge is free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dodge_cancel_ms: Option<Vec<[u64; 2]>>,
    /// The windows of its fragment's `CAGE-Named` `BlockDisabled` clips, ms: the block does not
    /// rise in them (the action list's `Block_UpperBody` needs `Not IfMannequinMarker
    /// BlockDisabled`; the sword light attack's 200-533 ms). From the data only: no retail block
    /// in an attack was matched to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_disabled_ms: Vec<[u64; 2]>,
    /// How it moves the player; `None`: its action leaves the motion to the movement keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<AttackMotion>,
}

/// How an attack moves its player (its action's `AnimationDrivenMotion`): the root motion of its
/// animation (the fragment's first shown clip, the `Xform` joint), times the multiplier of the
/// row its markers pick, and the velocity it keeps from the attack's start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttackMotion {
    /// The time between two offsets, ms: one frame of the animation at its speed.
    pub step_ms: f32,
    /// The player's offset from its place at the attack's start, in its frame then (right and
    /// forward, m), one per step from the start to the fragment's end.
    pub offsets: Vec<[f32; 2]>,
    /// It keeps the player's velocity at the start (`InheritVelocity`).
    #[serde(default)]
    pub inherit_velocity: bool,
    /// The most speed it keeps, m/s (`InheritVelocityMaxSpeed`); `None`: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit_max_mps: Option<f32>,
    /// How fast the kept velocity falls, right and forward, m/s² (`Decel`).
    pub decel_mps2: [f32; 2],
}

impl AttackMotion {
    /// The offset `since_start_ms` after the attack starts, between the two offsets around it;
    /// the last after them, none without them.
    pub fn offset_at(&self, since_start_ms: f32) -> [f32; 2] {
        let Some(last) = self.offsets.last() else {
            return [0.0; 2];
        };
        if self.step_ms <= 0.0 {
            return *last;
        }
        let steps = (since_start_ms / self.step_ms).max(0.0);
        // `steps` is at least 0 and an offset count is small.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = steps.floor() as usize;
        match (self.offsets.get(index), self.offsets.get(index + 1)) {
            (Some(from), Some(to)) => {
                let share = steps - steps.floor();
                [
                    from[0] + (to[0] - from[0]) * share,
                    from[1] + (to[1] - from[1]) * share,
                ]
            }
            _ => *last,
        }
    }

    /// How long its offsets run, ms.
    pub fn length_ms(&self) -> f32 {
        // An offset count is small.
        #[allow(clippy::cast_precision_loss)]
        let steps = self.offsets.len().saturating_sub(1) as f32;
        steps * self.step_ms
    }
}

impl BasicAttack {
    /// How long after its start the next attack action may start: at its `CanTransition`
    /// marker, else when its fragment ends (the action holds `AttackAction` until it ends).
    pub fn next_attack_from_ms(&self) -> u64 {
        self.can_transition_ms.unwrap_or(self.length_ms)
    }
}

/// A melee hit shape (`MeleeAttackShapeCastType`, numbered as the client's enum in
/// `FUN_1400b3890`), centred on its pose; the long axis of a capsule or cylinder is the pose's z
/// (the server's cast puts the ends at plus and minus half the height on z: `FUN_144860cd0`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ShapeKind {
    /// `MeleeAttackShapeCastTypeSphere`.
    Sphere {
        /// `MeleeAttackShapeRadius`.
        radius_m: f32,
    },
    /// `MeleeAttackShapeCastTypeCapsule`.
    Capsule {
        /// `MeleeAttackShapeRadius`.
        radius_m: f32,
        /// `MeleeAttackCapsuleHalfHeight`: from the middle to each round end's centre.
        half_height_m: f32,
    },
    /// `MeleeAttackShapeCastTypeBox`.
    Box {
        /// Half of `MeleeAttackBoxDimensions{X,Y,Z}` (the server's cast halves them).
        half_extents_m: [f32; 3],
    },
    /// `MeleeAttackShapeCastTypeCylinder`.
    Cylinder {
        /// `MeleeAttackShapeRadius`.
        radius_m: f32,
        /// `MeleeAttackCapsuleHalfHeight`: from the middle to each flat end.
        half_height_m: f32,
    },
}

/// One `CAGE-Damage` clip's shape and where it is over the clip's window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeleeShape {
    /// The clip's start and end, ms after the attack starts.
    pub window_ms: [u64; 2],
    /// The shape.
    pub shape: ShapeKind,
    /// The time between two poses, ms: one frame of the animation at its speed.
    pub step_ms: f32,
    /// The shape's pose in the character's root space from the window's start, one per step,
    /// the last at or after the window's end.
    pub poses: Vec<Rigid>,
}

impl MeleeShape {
    /// The shape's pose `since_start_ms` after the attack starts, between the two poses around
    /// it; the first pose before the window, the last after it; `None` without poses.
    pub fn pose_at(&self, since_start_ms: u64) -> Option<Rigid> {
        let first = self.poses.first()?;
        // Attack times are a few seconds of milliseconds: exact in an f32.
        #[allow(clippy::cast_precision_loss)]
        let into_ms = since_start_ms.saturating_sub(self.window_ms[0]) as f32;
        if self.step_ms <= 0.0 {
            return Some(*first);
        }
        let steps = into_ms / self.step_ms;
        // `steps` is at least 0 and a pose count is small.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = steps.floor() as usize;
        match (self.poses.get(index), self.poses.get(index + 1)) {
            (Some(from), Some(to)) => Some(from.blend(to, steps - steps.floor())),
            (Some(only), None) => Some(*only),
            (None, _) => self.poses.last().copied(),
        }
    }
}

/// A press of the attack key in a live attack that starts the next combo step: a
/// `CAGE-TransitionInstant` or `CAGE-TransitionBuffered` clip on `CanAttack_Heavy` (the attack key
/// pressed, `playeractions_attack.actionlist`) whose new action is a charge action of the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboLink {
    /// A press counts from this long after the attack starts...
    pub from_ms: u64,
    /// ...until this long.
    pub until_ms: u64,
    /// A buffered press starts the step at the start of the instant clip after it (ms after the
    /// attack starts); `None`: the step starts at the press.
    pub starts_at_ms: Option<u64>,
    /// The step it starts: 0 for the stance's own, `n` for `Stance::combo[n - 1]`.
    pub step: usize,
}

/// A later step of a stance's combo: its charge action's schedule and attacks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChargeStep {
    /// The charge action (`Attack_Start_Charge_C2`).
    pub action: String,
    /// The light attack.
    pub light: Option<BasicAttack>,
    /// When a release starts the light attack at the earliest, after the step starts.
    pub light_from_ms: u64,
    /// A release this long after the step starts or later makes the heavy attack...
    pub heavy_from_ms: Option<u64>,
    /// ...this one.
    pub heavy: Option<BasicAttack>,
    /// The key still held this long after the step starts makes the charged attack...
    pub charged_at_ms: Option<u64>,
    /// ...this one.
    pub charged: Option<BasicAttack>,
}

impl ChargeStep {
    /// Its schedule.
    pub fn schedule(&self) -> Schedule<'_> {
        Schedule {
            light: self.light.as_ref(),
            light_from_ms: self.light_from_ms,
            heavy_from_ms: self.heavy_from_ms,
            heavy: self.heavy.as_ref(),
            charged_at_ms: self.charged_at_ms,
            charged: self.charged.as_ref(),
        }
    }
}

/// A stance of another tag set: the stance with the tag that a status effect sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StanceVariant {
    /// The status effect (`Greatsword_Stance_Offense`).
    pub status_effect: String,
    /// The Mannequin tag it sets (`GenericTagA`).
    pub tag: String,
    /// The stance with the tag.
    pub stance: Stance,
}

/// The light, heavy and charged attacks of one combo step and when each starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Schedule<'a> {
    /// The light attack.
    pub light: Option<&'a BasicAttack>,
    /// When a release starts the light attack at the earliest.
    pub light_from_ms: u64,
    /// A release this long after the step starts or later makes the heavy attack.
    pub heavy_from_ms: Option<u64>,
    /// The heavy attack.
    pub heavy: Option<&'a BasicAttack>,
    /// The key still held this long after the step starts makes the charged attack.
    pub charged_at_ms: Option<u64>,
    /// The charged attack.
    pub charged: Option<&'a BasicAttack>,
}

impl<'a> Schedule<'a> {
    /// The attack of `kind`.
    pub const fn attack(&self, kind: AttackKind) -> Option<&'a BasicAttack> {
        match kind {
            AttackKind::Light => self.light,
            AttackKind::Heavy => self.heavy,
            AttackKind::Charged => self.charged,
        }
    }

    /// The attack a key let go `held_ms` after the step starts makes, and when it starts after
    /// the step's start: the charged attack when it was held to the charge, the heavy attack when
    /// it was held past the heavy hold, else the light attack; `None` when the step has none.
    pub fn released(&self, held_ms: u64) -> Option<(AttackKind, u64)> {
        if self.charged_at_ms.is_some_and(|at| held_ms >= at) {
            return self.charged_at_ms.map(|at| (AttackKind::Charged, at));
        }
        if self.heavy_from_ms.is_some_and(|from| held_ms >= from) && self.heavy.is_some() {
            return Some((AttackKind::Heavy, held_ms));
        }
        self.light
            .map(|_| (AttackKind::Light, held_ms.max(self.light_from_ms)))
    }
}

/// When a used item of one `MannequinTag` takes effect (the `UsePaperdollItem` clip of the
/// stance's `Use` fragment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemUse {
    /// The consumable's `MannequinTag` (`Potion`).
    pub tag: String,
    /// How long after the use starts, ms.
    pub effect_ms: u64,
}

/// One stance: the basic attacks of a weapon's tags, its dodge and its item uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stance {
    /// The weapon rows' `MannequinTag` cell (empty: no weapon).
    pub tags: String,
    /// The light attack: the key let go before the heavy hold...
    pub light: Option<BasicAttack>,
    /// ...which starts at the release, at the earliest this long after the press.
    pub light_from_ms: u64,
    /// The heavy attack: the key let go this long after the press or later...
    pub heavy_from_ms: Option<u64>,
    /// ...starts at the release.
    pub heavy: Option<BasicAttack>,
    /// The charged attack: the key still held this long after the press...
    pub charged_at_ms: Option<u64>,
    /// ...starts then.
    pub charged: Option<BasicAttack>,
    /// The `Invulnerability` window of its dodge roll, ms after the roll starts.
    pub dodge_invulnerable_ms: Option<[u64; 2]>,
    /// When its sheathe action puts the weapon away, ms after the action starts: the marker of
    /// the sheathe fragment's entry for a standing character ([`crate::sheathe`]); `None` when
    /// no entry has the marker.
    #[serde(default)]
    pub sheathe_ms: Option<u64>,
    /// When its item uses take effect, by consumable tag.
    pub uses: Vec<ItemUse>,
    /// The later steps of its combo, in chain order ([`ComboLink::step`]).
    #[serde(default)]
    pub combo: Vec<ChargeStep>,
    /// The stance while a status effect sets one more Mannequin tag.
    #[serde(default)]
    pub variants: Vec<StanceVariant>,
    /// The shot an attack fires while the character aims (the right-click held with a gun or a
    /// bow): the fragment with a ranged attack that the aim action's fragment changes to
    /// (`Firearm_AimShoot_SubState`: `Firearm_Shoot_Primary_UpperBody`, row `MusketAttack1`;
    /// `Bow_AimChargeShoot_SubState`: `Bow_Shoot_UpperBody`, row `BowAttack1`); `None` for a
    /// stance that does not aim.
    #[serde(default)]
    pub aimed: Option<BasicAttack>,
    /// The shot of a drawn charge while it aims, through a charge fragment of the aim
    /// (`Bow_Charge_UpperBody` to `Bow_Shoot_Charged`, row `BowAttack2`).
    #[serde(default)]
    pub aimed_charged: Option<BasicAttack>,
}

/// Which basic attack a press and a hold of the attack key make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttackKind {
    /// The light attack.
    Light,
    /// The heavy attack.
    Heavy,
    /// The charged attack.
    Charged,
}

impl Stance {
    /// The schedule of combo step `step` (0: the stance's own); `None` past the chain.
    pub fn schedule(&self, step: usize) -> Option<Schedule<'_>> {
        match step.checked_sub(1) {
            None => Some(Schedule {
                light: self.light.as_ref(),
                light_from_ms: self.light_from_ms,
                heavy_from_ms: self.heavy_from_ms,
                heavy: self.heavy.as_ref(),
                charged_at_ms: self.charged_at_ms,
                charged: self.charged.as_ref(),
            }),
            Some(later) => self.combo.get(later).map(ChargeStep::schedule),
        }
    }

    /// The attack of `kind` of the first combo step.
    pub const fn attack(&self, kind: AttackKind) -> Option<&BasicAttack> {
        match kind {
            AttackKind::Light => self.light.as_ref(),
            AttackKind::Heavy => self.heavy.as_ref(),
            AttackKind::Charged => self.charged.as_ref(),
        }
    }

    /// The shot an attack of `kind` fires while the character aims: the charged attack fires
    /// the drawn charge's shot when the stance has one; `None` for a stance that does not aim.
    pub fn aimed_attack(&self, kind: AttackKind) -> Option<&BasicAttack> {
        match kind {
            AttackKind::Charged => self.aimed_charged.as_ref().or(self.aimed.as_ref()),
            AttackKind::Light | AttackKind::Heavy => self.aimed.as_ref(),
        }
    }

    /// The attack a key let go `held_ms` after its press makes in the first combo step
    /// ([`Schedule::released`]).
    pub fn released(&self, held_ms: u64) -> Option<(AttackKind, u64)> {
        self.schedule(0)?.released(held_ms)
    }

    /// The stance of a character with the status effects `effects` (any case): the first
    /// variant whose effect is among them, else itself.
    pub fn with_status_effects(&self, effects: &[&str]) -> &Self {
        self.variants
            .iter()
            .find(|variant| {
                effects
                    .iter()
                    .any(|effect| effect.eq_ignore_ascii_case(&variant.status_effect))
            })
            .map_or(self, |variant| &variant.stance)
    }

    /// When an item use of consumable tag `tag` takes effect after it starts.
    pub fn use_effect_ms(&self, tag: &str) -> Option<u64> {
        self.uses
            .iter()
            .find(|item| item.tag.eq_ignore_ascii_case(tag))
            .map(|item| item.effect_ms)
    }
}

/// Every stance in the table, by key ([`stance_key`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerAttacks {
    by_key: HashMap<u32, Stance>,
}

impl PlayerAttacks {
    /// Loads the table of `client_build` from `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerAttacksError`] when the file is missing or bad.
    pub fn load(dir: &Path, client_build: &str) -> Result<Self, PlayerAttacksError> {
        let path = player_attacks_path(dir, client_build);
        let text = std::fs::read_to_string(&path)
            .map_err(|source| PlayerAttacksError::Io { path, source })?;
        Self::parse(&text)
    }

    /// The table in `text`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerAttacksError::Json`] when the text is not the table.
    pub fn parse(text: &str) -> Result<Self, PlayerAttacksError> {
        let stances: Vec<Stance> = serde_json::from_str(text)?;
        Ok(Self::of_stances(stances))
    }

    /// The table of `stances`.
    pub fn of_stances(stances: Vec<Stance>) -> Self {
        Self {
            by_key: stances
                .into_iter()
                .map(|stance| (stance_key(&stance.tags), stance))
                .collect(),
        }
    }

    /// The stance of key `key`.
    pub fn stance(&self, key: u32) -> Option<&Stance> {
        self.by_key.get(&key)
    }

    /// Stances in the table.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

/// Writes `stances` as the table file at `path`, creating parent folders.
///
/// # Errors
///
/// Returns [`PlayerAttacksError`] when the file cannot be written.
pub fn write_stances(path: &Path, stances: &[Stance]) -> Result<(), PlayerAttacksError> {
    let io = |source| PlayerAttacksError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(io)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(stances)?).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retail arrow (`basic_arrow.dynamicslice`, build 25810808): 100 m/s, 25 more over
    /// 1 s, `m_accel` z -9.8, 10 s.
    const ARROW: Flight = Flight {
        base_speed_mps: 100.0,
        extra_speed_mps: 25.0,
        time_for_max_speed_s: 1.0,
        gravity_mps2: 9.8,
        lifetime_s: 10.0,
    };
    const CLOSE: f32 = 0.01;

    fn melee(action: &str) -> BasicAttack {
        BasicAttack {
            action: action.to_owned(),
            fragment: action.to_owned(),
            damage_row: action.to_owned(),
            hit: AttackHit::Melee {
                window_ms: [0, 1],
                homing: None,
                shapes: Vec::new(),
            },
            length_ms: 0,
            links: Vec::new(),
            can_transition_ms: None,
            dodge_cancel_ms: None,
            block_disabled_ms: Vec::new(),
            motion: None,
        }
    }

    /// An attack whose press from 433 ms (buffered to 700 ms) starts combo step `step`, as the
    /// greatsword's `Attack_Primary` (`player_greatsword.adb`).
    fn linked(action: &str, step: usize) -> BasicAttack {
        BasicAttack {
            length_ms: 1_990,
            links: vec![
                ComboLink {
                    from_ms: 433,
                    until_ms: 700,
                    starts_at_ms: Some(700),
                    step,
                },
                ComboLink {
                    from_ms: 700,
                    until_ms: 1_990,
                    starts_at_ms: None,
                    step,
                },
            ],
            ..melee(action)
        }
    }

    /// The greatsword's charge schedule (`player_greatsword.adb`): heavy from 329 ms, charged at
    /// 667 ms; put away 833 ms into its sheathe (`player.adb` `From_Greatsword`).
    fn greatsword() -> Stance {
        Stance {
            tags: "2H_Melee,GreatSword".to_owned(),
            light: Some(melee("Attack_Primary_C1")),
            light_from_ms: 0,
            heavy_from_ms: Some(329),
            heavy: Some(melee("Attack_Heavy")),
            charged_at_ms: Some(667),
            charged: Some(melee("Attack_Heavy_Charged")),
            dodge_invulnerable_ms: Some([0, 500]),
            sheathe_ms: Some(833),
            uses: vec![ItemUse {
                tag: "Potion".to_owned(),
                effect_ms: 333,
            }],
            combo: vec![ChargeStep {
                action: "Attack_Start_Charge_C2".to_owned(),
                light: Some(linked("Attack_Primary_C2", 0)),
                light_from_ms: 0,
                heavy_from_ms: Some(333),
                heavy: Some(melee("Attack_Heavy_C2")),
                charged_at_ms: None,
                charged: None,
            }],
            variants: Vec::new(),
            aimed: None,
            aimed_charged: None,
        }
    }

    #[test]
    fn a_stance_key_does_not_care_for_case_order_or_spaces() {
        assert_eq!(
            stance_key("2H_Melee,GreatSword"),
            stance_key(" greatsword , 2H_MELEE")
        );
        assert_ne!(stance_key("2H_Melee,GreatSword"), stance_key("2H_Melee"));
        assert_eq!(
            stance_tags("1H_Melee, Axe,Throwable"),
            ["1H_Melee", "Axe", "Throwable"]
        );
    }

    #[test]
    fn the_hold_picks_the_light_heavy_or_charged_attack() {
        let stance = greatsword();
        assert_eq!(stance.released(100), Some((AttackKind::Light, 100)));
        assert_eq!(stance.released(328), Some((AttackKind::Light, 328)));
        assert_eq!(stance.released(329), Some((AttackKind::Heavy, 329)));
        assert_eq!(stance.released(500), Some((AttackKind::Heavy, 500)));
        assert_eq!(stance.released(667), Some((AttackKind::Charged, 667)));
        assert_eq!(stance.released(2_000), Some((AttackKind::Charged, 667)));
        let knife = Stance {
            light_from_ms: 167,
            heavy_from_ms: None,
            heavy: None,
            ..greatsword()
        };
        assert_eq!(
            knife.released(50),
            Some((AttackKind::Light, 167)),
            "a light attack starts no earlier than its transition"
        );
        assert_eq!(
            knife.released(400),
            Some((AttackKind::Light, 400)),
            "no heavy"
        );
    }

    #[test]
    fn a_table_finds_a_stance_by_its_key_and_an_item_use() {
        let text = serde_json::to_string(&[greatsword()]).expect("json");
        let table = PlayerAttacks::parse(&text).expect("table");
        let stance = table
            .stance(stance_key("2H_Melee,GreatSword"))
            .expect("stance");
        assert_eq!(stance.use_effect_ms("potion"), Some(333));
        assert_eq!(stance.use_effect_ms("Bandage"), None);
        assert_eq!(table.stance(stance_key("Bow")), None);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_combo_step_has_its_own_schedule() {
        let stance = greatsword();
        let second = stance.schedule(1).expect("step 2");
        assert_eq!(
            second.light.map(|attack| attack.action.as_str()),
            Some("Attack_Primary_C2")
        );
        assert_eq!(second.released(400), Some((AttackKind::Heavy, 400)));
        assert_eq!(
            second.released(700),
            Some((AttackKind::Heavy, 700)),
            "no charge in this step: held on, still the heavy attack"
        );
        assert_eq!(stance.schedule(2), None, "past the chain");
        let first = stance.schedule(0).expect("step 1");
        assert_eq!(
            first.attack(AttackKind::Charged),
            stance.attack(AttackKind::Charged)
        );
        let link = second.light.expect("light").links[0];
        assert_eq!(
            (link.from_ms, link.starts_at_ms, link.step),
            (433, Some(700), 0)
        );
    }

    #[test]
    fn an_aiming_stance_fires_its_aimed_shots_and_an_old_file_has_none() {
        /// The bow's aimed rows (`player_bow.adb`: `Bow_Shoot_UpperBody`, `Bow_Shoot_Charged`).
        const SHOT_ROW: &str = "BowAttack1";
        const CHARGED_ROW: &str = "BowAttack2";
        let shot = |row: &str| BasicAttack {
            damage_row: row.to_owned(),
            ..melee("Bow_AimChargeShoot_SubState")
        };
        let bow = Stance {
            aimed: Some(shot(SHOT_ROW)),
            aimed_charged: Some(shot(CHARGED_ROW)),
            ..greatsword()
        };
        let row = |kind| {
            bow.aimed_attack(kind)
                .map(|attack| attack.damage_row.as_str())
        };
        assert_eq!(row(AttackKind::Light), Some(SHOT_ROW));
        assert_eq!(row(AttackKind::Heavy), Some(SHOT_ROW));
        assert_eq!(row(AttackKind::Charged), Some(CHARGED_ROW));
        let gun = Stance {
            aimed_charged: None,
            ..bow.clone()
        };
        assert_eq!(
            gun.aimed_attack(AttackKind::Charged)
                .map(|attack| attack.damage_row.as_str()),
            Some(SHOT_ROW),
            "no charge: the charged attack fires the shot"
        );
        assert_eq!(greatsword().aimed_attack(AttackKind::Light), None);
        let mut json = serde_json::to_value(greatsword()).expect("json");
        let fields = json.as_object_mut().expect("an object");
        fields.remove("aimed");
        fields.remove("aimed_charged");
        let old: Stance = serde_json::from_value(json).expect("a file from before the aim");
        assert_eq!(old, greatsword());
    }

    #[test]
    fn a_status_effect_picks_the_stance_variant_with_its_tag() {
        let base = greatsword();
        let offense = Stance {
            charged_at_ms: Some(500),
            ..greatsword()
        };
        let stance = Stance {
            variants: vec![StanceVariant {
                status_effect: "Greatsword_Stance_Offense".to_owned(),
                tag: "GenericTagA".to_owned(),
                stance: offense,
            }],
            ..base.clone()
        };
        assert_eq!(stance.with_status_effects(&[]).charged_at_ms, Some(667));
        assert_eq!(
            stance
                .with_status_effects(&["greatsword_stance_offense"])
                .charged_at_ms,
            Some(500)
        );
        assert_eq!(
            stance.with_status_effects(&["Greatsword_Stance_Defense"]),
            &stance
        );
    }

    #[test]
    fn a_shape_pose_blends_between_its_frames() {
        let shape = MeleeShape {
            window_ms: [400, 500],
            shape: ShapeKind::Box {
                half_extents_m: [0.6, 0.25, 1.0],
            },
            step_ms: 50.0,
            poses: vec![
                Rigid::translation([0.0, 0.0, 0.0]),
                Rigid::translation([1.0, 0.0, 0.0]),
                Rigid::translation([3.0, 0.0, 0.0]),
            ],
        };
        let at = |ms| shape.pose_at(ms).expect("pose").translation[0];
        assert!((at(425) - 0.5).abs() < 1e-5);
        assert!((at(475) - 2.0).abs() < 1e-5);
        assert_eq!(at(300), 0.0, "before the window: the first pose");
        assert_eq!(at(600), 3.0, "after it: the last");
        let empty = MeleeShape {
            poses: Vec::new(),
            ..shape
        };
        assert_eq!(empty.pose_at(450), None);
    }

    #[test]
    fn an_older_table_without_shapes_or_combos_still_reads() {
        let text = r#"[{"tags":"1H_Melee,Sword","light":{"action":"Attack_Primary_C1","fragment":"Attack_Primary","damage_row":"1H_Sword_Attack1","hit":{"kind":"Melee","window_ms":[333,433],"homing":null}},"light_from_ms":0,"heavy_from_ms":null,"heavy":null,"charged_at_ms":null,"charged":null,"dodge_invulnerable_ms":null,"uses":[]}]"#;
        let table = PlayerAttacks::parse(text).expect("table");
        let stance = table.stance(stance_key("1H_Melee,Sword")).expect("stance");
        assert!(stance.combo.is_empty() && stance.variants.is_empty());
        let light = stance.attack(AttackKind::Light).expect("light");
        assert!(matches!(&light.hit, AttackHit::Melee { shapes, .. } if shapes.is_empty()));
        assert_eq!(
            light.can_transition_ms, None,
            "an older table has no marker"
        );
    }

    #[test]
    fn an_attack_moves_its_player_between_its_offsets_and_stops_at_the_last() {
        let motion = AttackMotion {
            step_ms: 100.0,
            offsets: vec![[0.0, 0.0], [0.0, 0.4], [0.2, 1.0]],
            inherit_velocity: false,
            inherit_max_mps: None,
            decel_mps2: [9.0, 9.0],
        };
        let close = |at: [f32; 2], expected: [f32; 2]| {
            (at[0] - expected[0]).abs() < CLOSE && (at[1] - expected[1]).abs() < CLOSE
        };
        assert!(close(motion.offset_at(0.0), [0.0, 0.0]));
        assert!(close(motion.offset_at(50.0), [0.0, 0.2]));
        assert!(close(motion.offset_at(150.0), [0.1, 0.7]));
        assert!(close(motion.offset_at(900.0), [0.2, 1.0]), "the last");
        assert!((motion.length_ms() - 200.0).abs() < CLOSE);
        let file = serde_json::to_string(&melee("Attack_Primary_C1")).expect("json");
        assert!(
            !file.contains("motion"),
            "an attack without motion leaves it out"
        );
    }

    #[test]
    fn the_next_attack_starts_at_the_can_transition_marker_else_at_the_end() {
        // The blunderbuss hip fire (`player_blunderbuss.adb`, `2H_Ranged+Blunderbuss`): fragment
        // 900 ms, `CanTransition` at 0.76666665 s.
        let hip_fire = BasicAttack {
            length_ms: 900,
            can_transition_ms: Some(767),
            dodge_cancel_ms: None,
            block_disabled_ms: Vec::new(),
            ..melee("Firearm_Shoot_HipFire_FullBody")
        };
        assert_eq!(hip_fire.next_attack_from_ms(), 767);
        let without_marker = BasicAttack {
            can_transition_ms: None,
            dodge_cancel_ms: None,
            block_disabled_ms: Vec::new(),
            ..hip_fire
        };
        assert_eq!(without_marker.next_attack_from_ms(), 900);
    }

    #[test]
    fn an_arrow_gains_speed_then_flies_on_and_falls() {
        // 1 s: 100 + 12.5; then 125 m/s.
        assert!((ARROW.distance_at(1.0) - 112.5).abs() < CLOSE);
        assert!((ARROW.distance_at(2.0) - 237.5).abs() < CLOSE);
        let time = ARROW.time_to(50.0).expect("in flight");
        assert!((ARROW.distance_at(time) - 50.0).abs() < 0.2);
        assert!((ARROW.drop_at(1.0) - 4.9).abs() < CLOSE);
        let gone = Flight {
            lifetime_s: 0.5,
            ..ARROW
        };
        assert_eq!(gone.time_to(100.0), None, "gone before 100 m");
    }
}
