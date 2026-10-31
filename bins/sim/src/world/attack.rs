// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attacks, hits and player health (M6-A spec sections 7 and 8).
//!
//! [`SimWorld::attack`] is the one attack of a player: it checks the attack (parent spec section
//! 9: range and attack rate), rolls the hit, and applies it with [`SimWorld::apply_hit`], which
//! mob attacks and hits from other sims use too. A player at 0 health is dead: it cannot move or
//! attack, every mob forgets it, its character is saved at once, and it lives again with full
//! health where it died when its client asks (M6-B spec section 8).
//!
//! Hits and deaths send messages (M6-B spec section 7): a hit made sends the attacker a 2071, a
//! hit applied on a player sends it a 3601, and a death a 4299. A mob's melee hit on a player
//! sends it a 2218; while the player holds block, the hit does no damage and the 2218 says so
//! (live-tutorial note, section 7). They wait in the world until
//! [`SimWorld::take_messages`] or the tick takes them.
//!
//! [`SimWorld::attack_input`] turns the attack bits of a client's input records into the basic
//! attacks of the weapon's stance ([`nw_content::player_attacks`]): a press during an attack's
//! combo link starts the link's step, else the stance's first; the hold picks the step's light,
//! heavy or charged attack. A melee attack sweeps its `CAGE-Damage` shapes through their windows
//! against every target ([`SimWorld::sweep`]); a projectile or a hit scan fires
//! ([`SimWorld::player_swings_due`]). The stance's variants of status effects
//! ([`nw_content::player_attacks::Stance::variants`]) do not apply: the sim keeps no player status
//! effects.

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use nw_bus::{BusId, ConnectionRef, Hit};
use std::sync::Arc;

use nw_content::combat::{AffixDamage, AttackRow, RangedBlock, WeaponRow};
use nw_content::player_attacks::{AttackHit, AttackKind, BasicAttack, Homing, MeleeShape, Stance};
use nw_content::respawn_points::{nearest_respawn_point, respawn_point};
use nw_content::status_effects::RowKey;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::paperdoll::slot;
use nw_messages::{
    AdjustFxScriptsMsg, CombatEffectMsg, DamageDealtMsg, DamageItem, DeathRecapMsg, FacetHeader,
    HIT_TAG, InputRecord, MeleeDamage, MeleeHitMsg, OnDamageMsg, ServerMessage,
    SpreadshotProcessedMsg, SpreadshotRequestMsg, facet_entity,
};
use nw_sim_core::character::swim_floor;
use nw_sim_core::{Damage, Health, PlayerAttack, TUTORIAL_START, mob_hit, player_hit};

use super::encounter::Tutorial;
use super::gear::{Gear, unix_ns};
use super::hit_shape::{
    MOB_SWING_HALF_ARC_RAD, PLAYER_CAPSULE, SweptShape, Wedge, body_frame, homing_score, turn,
};
use super::mob_timeline::yaw_toward;
use super::projectile::{AimLine, Shot};
use super::stamina::{Dodge, Guard};
use super::{Body, MESSAGE_CHANNEL, Outgoing, Placement, Player, Position, SimWorld};
use crate::combat::{AttackOutcome, AttackRefusal, RemoteHit, RespawnRefusal};
use crate::mobs::{
    Capsule, DEFAULT_BLOCK_STAMINA, DEFAULT_HIT_TAG, MobAttackIntent, MobReach, MoveHit,
    spawn_index,
};
use crate::objectives::Event;

/// How high above the sea a player still swims (Inferred: retail swam at 0.0 to 1.55 m above the
/// tutorial's sea and walked onto the beach at 3.5 m).
const SWIM_HEIGHT_M: f32 = 1.6;

/// The fit of the damage share that a mob's armor takes off a player's hit (xp-potion-exit note,
/// section 4.2, Inferred from two points: retail tutorial hits kept 0.7011 with a gear score 60
/// weapon and 0.7541 with gear score 75, on level 1 mobs of armor rating 109.336):
/// `armor / (armor + SCALE × gear score ^ POWER)`. The client's player attributes
/// (`playerbaseattributes.pbadb`) hold `Physical Armor Scale Factor` 1850, `Armor Mitigation
/// Exponent` 1.2 and `Diminishing Gear Score Threshold` 500; the formula that uses them is
/// Unknown (no function of the client sweep reads them by name), so the fit stays.
const ARMOR_FIT_SCALE: f64 = 1.8748;
const ARMOR_FIT_POWER: f64 = 1.2013;

/// The share of a hit that a mob of armor rating `armor` takes off a weapon of `gear_score`.
fn armor_share(armor: f64, gear_score: u32) -> f64 {
    armor / (armor + ARMOR_FIT_SCALE * f64::from(gear_score).powf(ARMOR_FIT_POWER))
}

/// The visual script of a heal: crc32 of `potionheal` (retail 608 after the potion).
const HEAL_SCRIPT: u32 = 0xf7e1_e0d6;
/// Microseconds per millisecond.
const US_PER_MS: u64 = 1000;
/// A blocked hit that breaks the guard takes half its health (newchar-retail 422387: 17.025 of
/// the same attack's 34.05 unblocked).
const BROKEN_GUARD_DAMAGE_DIVISOR: u32 = 2;

/// A player's health and attack times.
#[derive(Component, Debug, Clone, Copy)]
pub(super) struct Vitals {
    pub(super) health: Health,
    pub(super) dead_since_ms: Option<u64>,
    /// Spawn protection: `false` until the player's client is in the world. A fresh login starts
    /// `false`; the first movement the client sends ([`SimWorld::move_to`]) sets it `true`. While
    /// it is `false`, mobs cannot hit the player ([`SimWorld::mob_hits`]), so an idle character
    /// waiting at the loading screen is not killed before it spawns. A handed-off player keeps
    /// `true`.
    pub(super) spawned: bool,
    last_attack_ms: Option<u64>,
    /// The attack key as the server reads it.
    keys: AttackKeys,
    /// The counter (`u16` after the mask) of the newest input record read.
    input_counter: Option<u16>,
    /// The mob the player locked on in its newest input.
    lock_target: Option<u64>,
    /// The input record (the request's frame) of the player's last spread shot.
    last_shot_frame: Option<u16>,
    /// When the client last asked to fire a spread shot, fired or refused.
    shot_requested_ms: Option<u64>,
    /// The player holds the block key (the newest input record, [`ClientInputMsg::blocking`]).
    ///
    /// [`ClientInputMsg::blocking`]: nw_messages::ClientInputMsg::blocking
    block_key: bool,
    /// A blocked hit took the last stamina: the block stays down until the key is let go.
    guard_broken: bool,
    /// The player held the dodge key in the newest input record.
    dodge_held: bool,
    /// The player pressed quick slot 1 in the newest input record.
    quick_slot_held: bool,
    /// The invulnerability of its dodge roll, from and until: mob hits miss.
    invulnerable_ms: Option<(u64, u64)>,
}

impl Vitals {
    /// Whether the player's block is up: it holds the key (its newest input) and its guard did
    /// not break since it pressed it.
    pub(super) const fn blocking(&self) -> bool {
        self.block_key && !self.guard_broken
    }

    /// The entity the player locks on or homes to in its newest input.
    pub(super) const fn lock_target(&self) -> Option<u64> {
        self.lock_target
    }

    /// Stores the lock from the newest input record. `None` clears it: retail omits row 3 when
    /// the player is not locked on.
    pub(super) const fn set_lock_target(&mut self, target: Option<u64>) {
        self.lock_target = target;
    }

    pub(super) const fn new(health: Health, spawned: bool) -> Self {
        Self {
            health,
            dead_since_ms: None,
            spawned,
            last_attack_ms: None,
            keys: AttackKeys::NONE,
            input_counter: None,
            lock_target: None,
            last_shot_frame: None,
            shot_requested_ms: None,
            block_key: false,
            guard_broken: false,
            dodge_held: false,
            quick_slot_held: false,
            invulnerable_ms: None,
        }
    }
}

/// A combo step whose charge runs ([`nw_content::player_attacks::Schedule`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Charge {
    /// The step: 0 for the stance's own.
    step: usize,
    /// The counter of the record that pressed the attack key.
    pressed_at: u16,
    /// How long after the press the step's charge starts: 0, or until the instant clip after a
    /// buffered press.
    from_ms: u64,
    /// The key held to the charge started the charged attack; the release starts nothing more.
    charged: bool,
}

/// The last basic attack of a player: its step, kind and start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComboState {
    step: usize,
    kind: AttackKind,
    started_ms: u64,
}

/// One attack the attack key starts: its combo step and kind, its start, and the press it came
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyAttack {
    /// The combo step: 0 for the stance's own.
    pub step: usize,
    /// Light, heavy or charged.
    pub kind: AttackKind,
    /// When it starts.
    pub started_ms: u64,
    /// When its attack key was pressed.
    pub pressed_ms: u64,
}

/// One player's attack key as the server reads it: the combo step whose charge waits for the
/// key's release, and the last basic attack, for the press that continues its combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackKeys {
    charge: Option<Charge>,
    combo: Option<ComboState>,
}

impl AttackKeys {
    /// No press read yet.
    pub const NONE: Self = Self {
        charge: None,
        combo: None,
    };

    /// Reads `record` at `record_ms` with `stance`, the stance of the weapon the attack key uses
    /// (`None`: no attack): a press starts its step's charge ([`next_charge`]), a release picks
    /// the light or heavy attack by the hold, a hold to the charge starts the charged attack
    /// ([`Stance::released`]). Gives the attack it starts; that attack continues the combo when
    /// `continues_combo` (the first attack of an input message: the others of it cannot hit).
    pub fn record(
        &mut self,
        stance: Option<&Stance>,
        record: &InputRecord,
        record_ms: u64,
        continues_combo: bool,
    ) -> Option<KeyAttack> {
        let counter = record.u16_after_mask;
        let Some(stance) = stance else {
            if record.attack_released() {
                self.charge = None;
            }
            return None;
        };
        if record.attack_pressed() {
            self.charge = next_charge(stance, self.combo, counter, record_ms);
        }
        let charge = self.charge?;
        let Some(schedule) = stance.schedule(charge.step) else {
            self.charge = None;
            return None;
        };
        // The hold counts from the step's start, in records since the press.
        let since_press_ms = u64::from(
            counter
                .wrapping_sub(charge.pressed_at)
                .saturating_mul(INPUT_RECORD_MS),
        );
        let held_ms = since_press_ms.saturating_sub(charge.from_ms);
        let pressed_ms = record_ms.saturating_sub(since_press_ms);
        // A buffered step starts after the press, maybe after this record.
        let step_ms = pressed_ms.saturating_add(charge.from_ms);
        let (kind, after_ms) = if record.attack_released() {
            self.charge = None;
            if charge.charged {
                return None;
            }
            schedule.released(held_ms)?
        } else if let Some(charged_at_ms) = schedule.charged_at_ms
            && !charge.charged
            && since_press_ms >= charge.from_ms
            && held_ms >= charged_at_ms
            && schedule.charged.is_some()
        {
            // The charged attack starts while the key is held (retail newchar capture: the
            // greatsword's 2071 came 998 and 1110 ms after the press, before the releases at
            // 1181 and 1633 ms).
            self.charge = Some(Charge {
                charged: true,
                ..charge
            });
            (AttackKind::Charged, charged_at_ms)
        } else {
            return None;
        };
        let started_ms = step_ms.saturating_add(after_ms);
        if continues_combo {
            self.combo = Some(ComboState {
                step: charge.step,
                kind,
                started_ms,
            });
        }
        Some(KeyAttack {
            step: charge.step,
            kind,
            started_ms,
            pressed_ms,
        })
    }
}

/// The charge a press of the attack key in the record `pressed_at`, at `pressed_ms`, starts in
/// `stance` after the attack `combo`: while that attack plays, a press in one of its links starts
/// the link's step (at the press, or at the link's instant clip when buffered), and a press
/// before its links starts nothing (retail newchar capture, 2026-09-18: every light attack of a
/// greatsword chain started at least 706 ms after the one before, the instant clip of
/// `Attack_Primary` at 700 ms, though the player pressed every 170 ms); after it, or without
/// one, the stance's first step starts at the press.
fn next_charge(
    stance: &Stance,
    combo: Option<ComboState>,
    pressed_at: u16,
    pressed_ms: u64,
) -> Option<Charge> {
    let first = Charge {
        step: 0,
        pressed_at,
        from_ms: 0,
        charged: false,
    };
    let Some(last) = combo else {
        return Some(first);
    };
    let Some(attack) = stance
        .schedule(last.step)
        .and_then(|schedule| schedule.attack(last.kind))
    else {
        return Some(first);
    };
    let since_ms = pressed_ms.saturating_sub(last.started_ms);
    if since_ms >= attack.length_ms {
        return Some(first);
    }
    attack
        .links
        .iter()
        .find(|link| link.from_ms <= since_ms && since_ms < link.until_ms)
        .map(|link| Charge {
            step: link.step,
            pressed_at,
            from_ms: link
                .starts_at_ms
                .map_or(0, |at| at.saturating_sub(since_ms)),
            charged: false,
        })
}

/// A health amount as the `f32` of the wire (hit messages, the vitals block).
#[allow(clippy::cast_precision_loss, reason = "health stays far below 2^24")]
pub(super) const fn amount_of(amount: u32) -> f32 {
    amount as f32
}

/// The gap a mob's lunge leaves between its body and the player's (Inferred: every retail mob hit
/// landed 0.79 to 1.65 m from the player, middle to middle; a lunge until the bodies touched put
/// mobs in the player on screen, live 2026-09-18; this gap lands a sailor at 1.2 m and Thorpe at
/// 1.5 m, inside every creature's reach).
const LUNGE_GAP_M: f32 = 0.4;

/// The fewest input records between two spread shots of the shot attack `attack`: its next
/// attack's start ([`BasicAttack::next_attack_from_ms`]: the blunderbuss hip fire's
/// `CanTransition` marker at 0.76666665 s, 23 records; the aimed fire's
/// `Firearm_Shoot_Primary_UpperBody` entry leaves for `Firearm_Aim_UpperBody` at the same time).
/// The request's frame is the client's input record counter, so jitter does not count. Retail
/// (2026-09-18 capture, 56 gaps between 57 shots): the shortest gap was 23 records, on 5 shots,
/// none shorter; the ammo (two loads, then a reload) makes the longer ones.
fn shot_records(attack: &BasicAttack) -> u64 {
    attack.next_attack_from_ms() / u64::from(INPUT_RECORD_MS)
}

/// How near an attack key press a spread shot request shows that the attack was a shot. The
/// client sends its 2719 at the press, before the input that carries the key's release (live
/// 2026-09-18: the release came 0.2 to 0.5 s after the shot); the network adds a few ticks
/// (Inferred).
const SHOT_PAIR_MS: u64 = 300;
/// The pellets of a shot that uses the eight-pellet config.
const EIGHT_PELLETS: usize = 8;
/// The pellet config id of the answer on six-pellet shots (ranged note).
const SIX_PELLET_CONFIG: u64 = 230;
/// The pellet config id of the answer on eight-pellet shots (ranged note).
const EIGHT_PELLET_CONFIG: u64 = 231;

/// How a mob's hit on a player lands ([`SimWorld::mob_hit_style`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MobHitStyle {
    /// The 3601 item tag: its row's damage type.
    tag: u8,
    /// Its row's affix: the share of the hit of another type.
    affix: Option<AffixDamage>,
    /// The multiple of the pace's hit.
    damage_scale: f64,
    /// The stamina a block takes.
    block_stamina: f32,
    /// Its row, for the status effects it puts on the player ([`SimWorld::mob_hit_effects`]).
    row: Option<RowKey>,
    /// A projectile's hit ([`SimWorld::defend`]).
    ranged: bool,
}

impl MobHitStyle {
    /// The style of the same hit by a projectile.
    pub(super) const fn fired(self) -> Self {
        Self {
            ranged: true,
            ..self
        }
    }
}

/// A mob swing that has not hit yet.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingMobSwing {
    /// When it hits.
    lands_ms: u64,
    /// The server time it started.
    started_ms: u64,
    swing: MobAttackIntent,
    /// The hit of its move, when the mob has moves.
    hit: Option<MoveHit>,
}

/// Where a player aims an attack.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Aim {
    /// The camera yaw of the input that attacked.
    pub facing: Option<f32>,
    /// The camera pitch of that input (positive looks down).
    pub pitch: Option<f32>,
    /// The mob the player locked on (the input's target row).
    pub lock_target: Option<u64>,
}

/// A player's basic attack that has not landed yet.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlayerSwing {
    attacker: ConnectionRef,
    aim: Aim,
    kind: AttackKind,
    attack: BasicAttack,
    /// When the attack started.
    started_ms: u64,
    /// When its attack key was pressed.
    pressed_ms: u64,
    /// The melee option slot to take in hand when it lands, while the server holds a spread-shot
    /// weapon: unless a shot request came within [`SHOT_PAIR_MS`] of [`PlayerSwing::pressed_ms`],
    /// the attack was a swing of the weapon the client holds. It lands no earlier than
    /// [`SHOT_PAIR_MS`] after it was read, so that a shot request can come.
    hand: Option<u32>,
    /// The earliest time it lands.
    not_before_ms: u64,
    /// The targets its shapes hit.
    hits: Vec<u64>,
    /// When its shapes were last swept.
    swept_ms: Option<u64>,
}

impl PlayerSwing {
    /// When it lands first: its damage window opens, its projectile or hit scan fires.
    fn due_ms(&self) -> u64 {
        let after_ms = match self.attack.hit {
            AttackHit::Melee { window_ms, .. } => window_ms[0],
            AttackHit::Projectile { fire_ms, .. } | AttackHit::HitScan { fire_ms, .. } => fire_ms,
        };
        self.started_ms
            .saturating_add(after_ms)
            .max(self.not_before_ms)
    }

    /// When its damage window closes (its fire time for a shot).
    fn closes_ms(&self) -> u64 {
        match self.attack.hit {
            AttackHit::Melee { window_ms, .. } => self
                .started_ms
                .saturating_add(window_ms[1])
                .max(self.not_before_ms),
            AttackHit::Projectile { .. } | AttackHit::HitScan { .. } => self.due_ms(),
        }
    }
}

/// What a due swing did.
enum Landing {
    /// It landed or missed: its results.
    Done(Vec<Result<AttackOutcome, AttackRefusal>>),
    /// Its damage window is still open: the hits of this tick.
    Waiting(Vec<Result<AttackOutcome, AttackRefusal>>),
}

/// Milliseconds per client input record (retail: 34 ms apart, median; input note).
pub const INPUT_RECORD_MS: u16 = 33;

/// How far behind the last record a record may come and still be an older copy; a record
/// further behind is the client's new count. The client starts its record counter again at 1
/// when it loads a level (live 2026-09-18, our run-9 capture: 11336, then 1 at the open-world
/// load), while older copies in one input trail by a few records (Inferred: 512 records, about
/// 17 s, is far beyond both).
const COUNTER_RESTART_GAP: i16 = 512;

/// Whether input counter `counter` comes after `last`: later, wrapping at `u16::MAX`, or so far
/// behind that the client counts again from the start (a level load). A record seen before a
/// restart must not hide every record after it (live 2026-09-18: no attack on the beach counted).
pub(super) const fn counter_is_newer(counter: u16, last: u16) -> bool {
    let ahead = counter.wrapping_sub(last) as i16;
    ahead > 0 || ahead < -COUNTER_RESTART_GAP
}

/// One attack of a player on another player.
struct PvpStrike<'a> {
    /// The attacker's entity and number.
    entity: Entity,
    number: u64,
    /// The attacker's connection.
    attacker: &'a ConnectionRef,
    /// Where the attacker stands.
    from: Position,
    /// The target's number.
    target: u64,
    /// The attack's row.
    row: &'a AttackRow,
    /// The most distance the hit may reach (`None`: the attack's homing or shot checked it).
    reach_m: Option<f32>,
}

/// The factor of a hit that no balance or style changes.
const PLAIN_DAMAGE_FACTOR: f64 = 1.0;

/// One hit a player lands: who, on what, with which weapon and attack row.
struct Strike<'a> {
    entity: Entity,
    number: u64,
    attacker: &'a ConnectionRef,
    target: u64,
    view: &'a TargetView,
    /// The weapon in the hand and its gear score; `None`: the config's weapon.
    held: Option<(WeaponRow, u32)>,
    row: &'a AttackRow,
}

/// What an attack sees of its target.
struct TargetView {
    position: Position,
    dead: bool,
    returning: bool,
    /// Its level, for its armor.
    level: u32,
    /// The sim of a ghost target; `None` when the target is here.
    owner: Option<BusId>,
}

impl SimWorld {
    /// The player of `attacker` attacks `target` with its weapon's attack `damage_id`.
    ///
    /// # Errors
    ///
    /// Returns [`AttackRefusal`] when a check refuses the attack; nothing changes then.
    pub fn attack(
        &mut self,
        attacker: &ConnectionRef,
        target: u64,
        damage_id: &str,
        now_ms: u64,
    ) -> Result<AttackOutcome, AttackRefusal> {
        let reach_m = self.combat.attack_reach_m + self.combat.reach_margin_m;
        self.hit(attacker, target, damage_id, Some(reach_m), now_ms)
    }

    /// The player of `attacker` hits `target` with its weapon's attack `damage_id`: `reach_m`
    /// caps the distance (`None`: the attack's homing or shot checked it); the attack interval
    /// since its last hit applies.
    pub(super) fn hit(
        &mut self,
        attacker: &ConnectionRef,
        target: u64,
        damage_id: &str,
        reach_m: Option<f32>,
        now_ms: u64,
    ) -> Result<AttackOutcome, AttackRefusal> {
        self.hit_paced(attacker, target, damage_id, reach_m, true, now_ms)
    }

    /// A hit on another player (`PvP_Flag` toggles 2525; both players must be flagged, a player
    /// who is not is no target). The damage rolls as a hit on a mob does ([`player_hit`]: the
    /// weapon's base damage, the row's coefficient and the crit), times the open-world PvP
    /// balance of the weapon's class ([`nw_content::combat::CombatData::pvp_damage_factor`]) and
    /// the attacker's status effects; the target's armor takes the share it takes of a mob's hit
    /// ([`crate::combat::CombatSettings::player_armor_share`]), its status effects their share.
    /// It lands as any hit on a player does ([`SimWorld::apply_hit_with`]: 3601 to the target,
    /// its death and recap), and the attacker gets its 2071.
    fn pvp_hit(&mut self, pvp: PvpStrike<'_>, now_ms: u64) -> Result<AttackOutcome, AttackRefusal> {
        let PvpStrike {
            entity,
            number,
            attacker,
            from,
            target,
            row,
            reach_m,
        } = pvp;
        let flagged = |world: &Self, entity: Entity| {
            world
                .world
                .get::<Player>(entity)
                .is_some_and(|player| player.pvp_flagged)
        };
        if !flagged(self, entity) {
            return Err(AttackRefusal::UnknownTarget);
        }
        let target_entity = self
            .player_by_number(target)
            .ok_or(AttackRefusal::UnknownTarget)?;
        if target_entity == entity || !flagged(self, target_entity) {
            return Err(AttackRefusal::UnknownTarget);
        }
        let at = self
            .world
            .get::<Body>(target_entity)
            .ok_or(AttackRefusal::UnknownTarget)?
            .motion
            .position();
        if reach_m.is_some_and(|reach_m| at.horizontal_distance(from) > reach_m) {
            return Err(AttackRefusal::OutOfReach);
        }
        if self
            .world
            .get::<Vitals>(target_entity)
            .is_none_or(|vitals| vitals.health.is_dead())
        {
            return Err(AttackRefusal::TargetDead);
        }
        let (weapon, pvp_factor) = self
            .world
            .get::<Player>(entity)
            .and_then(|player| {
                player
                    .gear
                    .as_ref()?
                    .main_weapon()
                    .map(|(item, _)| item.to_owned())
            })
            .and_then(|item| {
                let data = &self.combat.data;
                Some((*data.weapon_of_item(&item)?, data.pvp_damage_factor(&item)))
            })
            .unwrap_or((*self.combat.data.weapon(), PLAIN_DAMAGE_FACTOR));
        let attack = PlayerAttack {
            base_damage: weapon.base_damage,
            coefficient: row.coefficient,
            can_crit: row.can_crit,
            crit_chance: weapon.crit_chance,
            crit_multiplier: weapon.crit_multiplier,
        };
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            vitals.last_attack_ms = Some(now_ms);
        }
        let armor_share = self.combat.player_armor_share;
        let tag = row.damage_type.unwrap_or(HIT_TAG);
        let mut hit = player_hit(&attack, armor_share, &mut self.dice);
        let scale = pvp_factor * self.player_hit_scale(entity, target, tag);
        // A hit of a few thousand points.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a scaled hit stays a few thousand points"
        )]
        {
            hit.amount = (f64::from(hit.amount) * scale).round() as u32;
        }
        let style = MobHitStyle {
            tag,
            affix: row.affix,
            damage_scale: PLAIN_DAMAGE_FACTOR,
            block_stamina: 0.0,
            row: Some(RowKey::of(&row.table, &row.damage_id)),
            ranged: false,
        };
        let outcome = self
            .apply_hit_with(target, number, hit.amount, 0, now_ms, Some(&style))
            .ok_or(AttackRefusal::TargetDead)?;
        let killed = matches!(outcome, AttackOutcome::Killed { .. });
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.combat.hit(now_ms, killed);
        }
        let item = DamageItem {
            tag,
            amount: amount_of(hit.amount),
            scale: (1.0 - armor_share) as f32,
        };
        self.send_reliable(
            attacker.clone(),
            &ServerMessage::DamageDealt(DamageDealtMsg::melee(
                number, target, item, hit.crit, killed,
            )),
        );
        Ok(outcome)
    }

    /// As [`SimWorld::hit`]; the attack interval applies only when `paced` (a second target of
    /// one swing is not a second attack).
    fn hit_paced(
        &mut self,
        attacker: &ConnectionRef,
        target: u64,
        damage_id: &str,
        reach_m: Option<f32>,
        paced: bool,
        now_ms: u64,
    ) -> Result<AttackOutcome, AttackRefusal> {
        let entity = *self
            .by_connection
            .get(attacker)
            .ok_or(AttackRefusal::NotInWorld)?;
        let number = self
            .world
            .get::<Player>(entity)
            .ok_or(AttackRefusal::NotInWorld)?
            .number;
        let position = self
            .world
            .get::<Body>(entity)
            .ok_or(AttackRefusal::NotInWorld)?
            .motion
            .position();
        let vitals = *self
            .world
            .get::<Vitals>(entity)
            .ok_or(AttackRefusal::NotInWorld)?;
        if vitals.health.is_dead() {
            return Err(AttackRefusal::Dead);
        }
        if spawn_index(target).is_none() {
            let row = self
                .combat
                .data
                .attack(damage_id)
                .ok_or(AttackRefusal::NotWeaponAttack)?
                .clone();
            if paced && self.too_fast(&vitals, now_ms) {
                return Err(AttackRefusal::TooFast);
            }
            let pvp = PvpStrike {
                entity,
                number,
                attacker,
                from: position,
                target,
                row: &row,
                reach_m,
            };
            return self.pvp_hit(pvp, now_ms);
        }
        let view = self
            .target_view(target)
            .ok_or(AttackRefusal::UnknownTarget)?;
        if view.dead {
            return Err(AttackRefusal::TargetDead);
        }
        if view.returning {
            return Err(AttackRefusal::Returning);
        }
        let row = self
            .combat
            .data
            .attack(damage_id)
            .ok_or(AttackRefusal::NotWeaponAttack)?;
        if reach_m.is_some_and(|reach_m| view.position.horizontal_distance(position) > reach_m) {
            return Err(AttackRefusal::OutOfReach);
        }
        if paced && self.too_fast(&vitals, now_ms) {
            return Err(AttackRefusal::TooFast);
        }
        // The weapon in the hand, else the config's; its gear score sets the armor share.
        let held = self.held_weapon(entity);
        let row = row.clone();
        let outcome = self.strike(
            Strike {
                entity,
                number,
                attacker,
                target,
                view: &view,
                held,
                row: &row,
            },
            now_ms,
            |amount, crit, killed, scale| {
                let item = DamageItem {
                    tag: row.damage_type.unwrap_or(HIT_TAG),
                    amount,
                    scale,
                };
                DamageDealtMsg::melee(number, target, item, crit, killed)
            },
        )?;
        Ok(outcome)
    }

    /// Whether an attack at `now_ms` comes sooner than the attack interval after the last one
    /// of `vitals`.
    fn too_fast(&self, vitals: &Vitals, now_ms: u64) -> bool {
        vitals
            .last_attack_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < self.combat.min_attack_interval_ms)
    }

    /// Lands one hit of `strike` at `now_ms`: rolls the damage of its weapon (else the config's)
    /// and row against the target's level, applies it here or sends it to the target's sim, and
    /// tells the attacker with the message `dealt` makes of the amount, the crit, the kill and the
    /// share of the damage the armor let through. A mob that absorbs hits (Thorpe in his
    /// intermission) takes none.
    fn strike(
        &mut self,
        strike: Strike<'_>,
        now_ms: u64,
        dealt: impl Fn(f32, bool, bool, f32) -> DamageDealtMsg,
    ) -> Result<AttackOutcome, AttackRefusal> {
        let Strike {
            entity,
            number,
            attacker,
            target,
            view,
            held,
            row,
        } = strike;
        let weapon = held.map_or_else(|| *self.combat.data.weapon(), |(weapon, _)| weapon);
        let armor_share = held
            .zip(self.combat.data.level(view.level))
            .map_or(self.combat.armor_share, |((_, gear_score), level)| {
                armor_share(level.physical_armor_rating, gear_score)
            });
        let attack = PlayerAttack {
            base_damage: weapon.base_damage,
            coefficient: row.coefficient,
            can_crit: row.can_crit,
            crit_chance: weapon.crit_chance,
            crit_multiplier: weapon.crit_multiplier,
        };
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            vitals.last_attack_ms = Some(now_ms);
        }
        if view.owner.is_none() && self.mobs.absorbing(target, now_ms) {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.combat.hit(now_ms, false);
            }
            self.send_reliable(
                attacker.clone(),
                &ServerMessage::DamageDealt(DamageDealtMsg::absorbed(number, target)),
            );
            self.send_reliable(
                attacker.clone(),
                &ServerMessage::CombatEffect(CombatEffectMsg::DisplayImmuneText {
                    header: FacetHeader::of(nw_messages::facet::component::DAMAGE_RECEIVER, target),
                    attacker: facet_entity(number),
                }),
            );
            return Ok(AttackOutcome::Hit { target, amount: 0 });
        }
        let mut hit = player_hit(&attack, armor_share, &mut self.dice);
        // The status effects of the player and of the mob scale the hit (`Dmg*`, `Abs*`).
        let effects_scale =
            self.player_hit_scale(entity, target, row.damage_type.unwrap_or(HIT_TAG));
        // A hit of a few thousand points.
        hit.amount = (f64::from(hit.amount) * effects_scale).round() as u32;
        let threat = row.threat.saturating_add(u64::from(hit.amount));
        let outcome = match &view.owner {
            None => self
                .apply_hit(target, number, hit.amount, threat, now_ms)
                .ok_or(AttackRefusal::TargetDead)?,
            Some(owner) => AttackOutcome::Remote(self.remote_hit(
                owner.clone(),
                target,
                number,
                hit.amount,
                threat,
                None,
            )),
        };
        let killed = matches!(outcome, AttackOutcome::Killed { .. });
        if matches!(outcome, AttackOutcome::Hit { .. }) {
            self.mob_timelines.hurt(target);
            if view.owner.is_none() {
                self.player_hit_effects(
                    target,
                    number,
                    RowKey::of(&row.table, &row.damage_id),
                    hit.amount,
                );
            }
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.combat.hit(now_ms, killed);
        }
        self.send_reliable(
            attacker.clone(),
            &ServerMessage::DamageDealt(dealt(
                amount_of(hit.amount),
                hit.crit,
                killed,
                (1.0 - armor_share) as f32,
            )),
        );
        Ok(outcome)
    }

    /// The player of `shooter` fires the spread shot `request` at `now_ms` with the spread-shot
    /// weapon it wears (ranged note): the answer goes to the shooter at once, then each pellet that
    /// hit a living mob in range lands as one hit. Retail trusts the targets the client names.
    pub fn spreadshot(
        &mut self,
        shooter: &ConnectionRef,
        request: &SpreadshotRequestMsg,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let refused = |refusal| vec![Err(refusal)];
        let Some(entity) = self.by_connection.get(shooter).copied() else {
            return refused(AttackRefusal::NotInWorld);
        };
        let (Some(player), Some(body), Some(vitals)) = (
            self.world.get::<Player>(entity),
            self.world.get::<Body>(entity),
            self.world.get::<Vitals>(entity),
        ) else {
            return refused(AttackRefusal::NotInWorld);
        };
        if vitals.health.is_dead() {
            return refused(AttackRefusal::Dead);
        }
        let last_shot_frame = vitals.last_shot_frame;
        let number = player.number;
        let position = body.motion.position();
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            vitals.shot_requested_ms = Some(now_ms);
        }
        let data = Arc::clone(&self.combat.data);
        // A shot shows the spread-shot weapon in the client's hand: the server's hand follows
        // when it holds another option (a weapon key it missed, or a handoff).
        self.take_in_hand(entity, number, |weapon| weapon.spread_shot);
        let Some(player) = self.world.get::<Player>(entity) else {
            return refused(AttackRefusal::NotInWorld);
        };
        let held = player.gear.as_ref().and_then(|gear| {
            let (item, gear_score) = gear.main_weapon()?;
            let weapon = *data.weapon_of_item(item)?;
            weapon.spread_shot.then_some((weapon, gear_score))
        });
        let Some(held) = held else {
            return refused(AttackRefusal::NotWeaponAttack);
        };
        // The stance's light attack is the shot: its hit scan's range and its damage row
        // (`BlunderbussPrimaryFire_Spread`, 14 m; `Blunderbuss_Primary_Attack`).
        let shot = data
            .stance(held.0.stance)
            .and_then(|stance| stance.attack(AttackKind::Light))
            .and_then(|attack| match attack.hit {
                AttackHit::HitScan { range_m, .. } => {
                    Some((data.attack(&attack.damage_row)?, range_m, attack))
                }
                AttackHit::Melee { .. } | AttackHit::Projectile { .. } => None,
            });
        let Some((row, range_m, attack)) = shot else {
            return refused(AttackRefusal::NotWeaponAttack);
        };
        if last_shot_frame
            .is_some_and(|last| u64::from(request.frame.wrapping_sub(last)) < shot_records(attack))
        {
            return refused(AttackRefusal::TooFast);
        }
        let fired = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| Some(player.gear.as_mut()?.fire(now_ms)))
            .unwrap_or(false);
        if !fired {
            return refused(AttackRefusal::NoAmmo);
        }
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            vitals.last_shot_frame = Some(request.frame);
        }
        let config = if request.pellets.len() == EIGHT_PELLETS {
            EIGHT_PELLET_CONFIG
        } else {
            SIX_PELLET_CONFIG
        };
        self.send_reliable(
            shooter.clone(),
            &ServerMessage::SpreadshotProcessed(SpreadshotProcessedMsg {
                request: request.clone(),
                damage: lowercase_crc32(&row.damage_id),
                config,
            }),
        );
        request
            .pellet_targets()
            .into_iter()
            .map(|target| {
                if spawn_index(target).is_none() {
                    return Err(AttackRefusal::UnknownTarget);
                }
                let view = self
                    .target_view(target)
                    .ok_or(AttackRefusal::UnknownTarget)?;
                if view.dead {
                    return Err(AttackRefusal::TargetDead);
                }
                if view.returning {
                    return Err(AttackRefusal::Returning);
                }
                if view.position.horizontal_distance(position) > range_m {
                    return Err(AttackRefusal::OutOfReach);
                }
                self.strike(
                    Strike {
                        entity,
                        number,
                        attacker: shooter,
                        target,
                        view: &view,
                        held: Some(held),
                        row,
                    },
                    now_ms,
                    |amount, crit, killed, scale| {
                        DamageDealtMsg::pellet(number, target, amount, scale, crit, killed)
                    },
                )
            })
            .collect()
    }

    /// The hit and death messages since the last call.
    pub fn take_messages(&mut self) -> Vec<Outgoing> {
        std::mem::take(&mut self.combat_messages)
    }

    /// Queues `message` for `connection`, reliable on the message channel.
    fn send_reliable(&mut self, connection: ConnectionRef, message: &ServerMessage) {
        match message.envelope() {
            Ok(envelope) => self.combat_messages.push(Outgoing {
                connection,
                channel: MESSAGE_CHANNEL,
                reliable: true,
                envelopes: vec![envelope],
            }),
            Err(error) => tracing::error!(%error, "combat message not encoded"),
        }
    }

    /// Drinks the potions whose press is due at `now_ms`, at server time `server_ms` (ms since
    /// 2000-01-01).
    pub(super) fn drink_due(&mut self, now_ms: u64, server_ms: u64) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.drinks)
            .into_iter()
            .partition(|(_, at)| now_ms >= *at);
        self.drinks = later;
        for (number, _) in due {
            self.drink(number, now_ms, server_ms);
        }
    }

    /// The player `number` uses the item of its quick slot 1 at `now_ms`. A healing potion is
    /// drunk unless the slot is empty, holds a disabled item (`javelin.disabled-item-classes`),
    /// or the item's cooldown runs (xp-potion-exit note, section 2.5): one less in the slot, the
    /// cooldown, the heal (`health` + `health share` x max, at most to the max) and the heal's
    /// visual script. Another consumable (food: live 2026-10-29, the light ration gave no buff
    /// from the quick slot) is used as from the bag ([`SimWorld::consume`]).
    fn drink(&mut self, number: u64, now_ms: u64, server_ms: u64) {
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let worn = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref()?.worn_item(slot::QUICK_SLOT_1))
            .map(str::to_owned);
        // Food and other consumables that do not heal at once take effect as a use from the bag.
        if let Some(item) = worn
            && self.combat.data.heal_of_item(&item).is_none()
        {
            self.consume(
                entity,
                number,
                &item,
                super::item_calls::UsedFrom::Worn(slot::QUICK_SLOT_1),
                now_ms,
            );
            return;
        }
        let data = Arc::clone(&self.combat.data);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        let Some(gear) = player.gear.as_mut() else {
            tracing::debug!(player = number, "drink: no gear");
            return;
        };
        let Some(item) = gear.worn_item(slot::QUICK_SLOT_1).map(str::to_owned) else {
            tracing::debug!(player = number, "drink: quick slot 1 is empty");
            return;
        };
        if self.content.is_item_disabled(&item) {
            tracing::debug!(player = number, item, "drink: the item's class is disabled");
            return;
        }
        let Some(heal) = data.heal_of_item(&item) else {
            tracing::debug!(player = number, item, "drink: the item does not heal");
            return;
        };
        let now_us = server_ms.saturating_mul(US_PER_MS);
        let cooldown = Some((heal.cooldown_id.as_str(), heal.cooldown_s));
        if !super::item_calls::start_cooldown(&mut player.cooldowns, &item, cooldown, now_us) {
            tracing::debug!(player = number, item, "drink: the cooldown runs");
            return;
        }
        gear.use_one(slot::QUICK_SLOT_1);
        player.cooldowns_changed = true;
        let connection = player.connection.clone();
        let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) else {
            return;
        };
        let max = f64::from(vitals.health.max());
        // The heal fits a `u32`: it is below the max health and a flat amount.
        let amount = (heal.health + heal.health_share * max).round() as u32;
        let healed = vitals.health.heal(amount);
        tracing::debug!(player = number, item, amount, healed, "drink");
        self.send_reliable(
            connection.clone(),
            &ServerMessage::AdjustFxScripts(AdjustFxScriptsMsg::on(number, HEAL_SCRIPT)),
        );
        if healed > 0 {
            self.send_reliable(
                connection,
                &ServerMessage::OnHealed(nw_messages::OnHealedMsg::of(
                    number,
                    number,
                    healed as f32,
                )),
            );
        }
    }

    /// The dead player of `connection` lives again with full health, when `min_respawn_ms` have
    /// passed since its death (M6-B spec section 8.2), at a respawn place, never where it died
    /// (respawn note, 2026-09-18: retail moved its three open-world deaths 305 to 541 m): in the
    /// tutorial at the start of its newest fight ([`Tutorial::respawn_at`]); in the open world
    /// at the respawn point `point` its client chose, else at the known point nearest its death
    /// ([`nw_content::respawn_points`]). No mob keeps it as a target, a respawn point is walked
    /// floor, and the body falls from its new place to the floor it finds (in the tutorial after
    /// the move into the water: into the sea). Gives where it came back.
    ///
    /// # Errors
    ///
    /// Returns [`RespawnRefusal`] when there is no player, it is alive, or it died too recently.
    pub fn request_respawn(
        &mut self,
        connection: &ConnectionRef,
        point: Option<u64>,
        now_ms: u64,
    ) -> Result<Position, RespawnRefusal> {
        let min_ms = self.combat.min_respawn_ms;
        let entity = self
            .by_connection
            .get(connection)
            .copied()
            .ok_or(RespawnRefusal::NotInWorld)?;
        let mut vitals = self
            .world
            .get_mut::<Vitals>(entity)
            .ok_or(RespawnRefusal::NotInWorld)?;
        let since = vitals.dead_since_ms.ok_or(RespawnRefusal::NotDead)?;
        if now_ms.saturating_sub(since) < min_ms {
            return Err(RespawnRefusal::TooSoon);
        }
        vitals.health.refill();
        vitals.dead_since_ms = None;
        let number = self
            .world
            .get::<Player>(entity)
            .map(|player| player.number)
            .ok_or(RespawnRefusal::NotInWorld)?;
        let died_at = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
            .ok_or(RespawnRefusal::NotInWorld)?;
        let to = if self.is_tutorial() {
            self.tutorials
                .get(&number)
                .map_or(TUTORIAL_START, Tutorial::respawn_at)
        } else {
            // A point no capture shows is a capital of the level: an inn or a respawn camp the
            // player bound (travel study: retail's 1938 named the bound ids).
            let at = point
                .and_then(|point| {
                    respawn_point(point)
                        .or_else(|| self.content.travel.spawn_point(point))
                        .or_else(|| self.own_spawn_point(entity, point))
                })
                .unwrap_or_else(|| nearest_respawn_point([died_at.x, died_at.y, died_at.z]));
            let at = Position {
                x: at[0],
                y: at[1],
                z: at[2],
            };
            // A respawn point stands on a floor. The tutorial's respawn places are not all
            // floors: the one after the move into the water is in the air over the sea.
            self.walked_floor.walked(at);
            at
        };
        self.mobs.forget(number);
        self.scripted_swings.retain(|swing| swing.target != number);
        self.place_player(entity, to, None, Placement::Drop, now_ms);
        Ok(to)
    }

    /// Keeps whether the player holds block, starts a dodge roll on a new press of the dodge key
    /// and a drink on a new press of quick slot 1 (the first, newest record), then reads the
    /// attacks (a press and its release) in the records of `records` not read before, oldest
    /// first, with the stance of the weapon the attack key uses: a release picks the light or
    /// heavy attack by the hold, a hold to the charge starts the charged attack
    /// ([`Stance::released`]). The first attack starts ([`SimWorld::player_swings_due`] lands
    /// it); the others of the same message are [`AttackRefusal::TooFast`], which this returns.
    /// The counter of the newest record carries over to the next message. Nothing for a
    /// connection without a player.
    pub fn attack_input(
        &mut self,
        connection: &ConnectionRef,
        records: &[InputRecord],
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let Some(entity) = self.by_connection.get(connection).copied() else {
            return Vec::new();
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return Vec::new();
        };
        let swimming = self.world.get::<Body>(entity).is_some_and(|body| {
            body.motion.position().z <= self.settings.ocean_level_m + SWIM_HEIGHT_M
        });
        if let Some(newest) = records.first() {
            self.stamina_input(entity, number, newest, swimming, now_ms);
        }
        // The weapon the attack key uses: the one in the hand, else, while the hand holds a
        // spread-shot weapon (which fires through its own request), the melee option.
        let data = Arc::clone(&self.combat.data);
        let held = self.held_weapon(entity).map(|(weapon, _)| weapon);
        let spread = held.is_some_and(|weapon| weapon.spread_shot);
        let (stance, hand) = if spread {
            let melee = self.world.get::<Player>(entity).and_then(|player| {
                let gear = player.gear.as_ref()?;
                let slot_id = gear.option_with(|item| {
                    data.weapon_of_item(item)
                        .and_then(|weapon| data.stance(weapon.stance))
                        .is_some_and(Self::swings)
                })?;
                let weapon = data.weapon_of_item(gear.worn_item(slot_id)?)?;
                Some((data.stance(weapon.stance)?, slot_id))
            });
            match melee {
                Some((stance, slot_id)) => (Some(stance), Some(slot_id)),
                None => (None, None),
            }
        } else {
            let weapon = held.unwrap_or(*data.weapon());
            (data.stance(weapon.stance), None)
        };
        let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) else {
            return Vec::new();
        };
        // The client sends the newest record first, then older copies with fewer fields (swing
        // note: counters 7290, 7289, 7288); each record counts once, oldest first, at its own
        // time: one record time before the newer one. An attack is a press and its release, or a
        // hold to the charge, of a combo step.
        let mut attacks: Vec<KeyAttack> = Vec::new();
        let mut weapon_key = None;
        // A new press of the attack key, which draws a sheathed weapon (`Gear::draw_to_attack`).
        let mut attack_pressed = false;
        // A new press of the sheathe key ([`Gear::draw_selected`]).
        let mut sheathe_pressed = false;
        let newest_counter = records.first().map(|record| record.u16_after_mask);
        for record in records.iter().rev() {
            let counter = record.u16_after_mask;
            if vitals
                .input_counter
                .is_some_and(|last| !counter_is_newer(counter, last))
            {
                continue;
            }
            vitals.input_counter = Some(counter);
            weapon_key = record.weapon_key_pressed().or(weapon_key);
            attack_pressed |= record.attack_pressed();
            sheathe_pressed |= record.sheathe_pressed();
            let behind = newest_counter.map_or(0, |newest| newest.wrapping_sub(counter));
            let record_ms =
                now_ms.saturating_sub(u64::from(behind.saturating_mul(INPUT_RECORD_MS)));
            if let Some(attack) = vitals
                .keys
                .record(stance, record, record_ms, attacks.is_empty())
            {
                attacks.push(attack);
            }
        }
        let lock_target = records.first().and_then(InputRecord::lock_target);
        if std::mem::replace(&mut vitals.lock_target, lock_target) != lock_target {
            tracing::info!(player = number, target = ?lock_target, "lock target");
        }
        let time_ns = unix_ns(self.server_ms);
        let sheathe_ms = self.sheathe_ms(entity);
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
        {
            match weapon_key {
                Some(key) => {
                    if gear.draw(key, now_ms, time_ns) {
                        tracing::debug!(player = number, key, "weapon drawn");
                    }
                }
                // The sheathe key with a weapon drawn starts the sheathe action.
                None if sheathe_pressed && gear.drawn().is_some() => {
                    if let Some(after_ms) = sheathe_ms
                        && gear.sheathe(now_ms, after_ms)
                    {
                        tracing::debug!(player = number, after_ms, "weapon sheathed by the key");
                    }
                }
                None => {
                    if sheathe_pressed && gear.draw_selected(now_ms, time_ns) {
                        tracing::debug!(player = number, "weapon drawn by the sheathe key");
                    } else if attack_pressed && gear.draw_to_attack(time_ns) {
                        tracing::debug!(player = number, "weapon drawn to attack");
                    }
                }
            }
        }
        // Every attack of one message has the same receive time, so only the first can hit: the
        // others are too fast, and a client cannot make the loop search for targets per record.
        let Some(&KeyAttack {
            step,
            kind,
            started_ms,
            pressed_ms,
        }) = attacks.first()
        else {
            return Vec::new();
        };
        if spread {
            // An attack with the spread-shot weapon empty is the client's reload, not a swing:
            // the client sends no shot then, and taking the melee option in hand made the client
            // swap weapons when the reload ended (live 2026-09-18: after every second shot).
            let empty = self
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref().map(Gear::held_empty))
                .unwrap_or(false);
            if empty {
                tracing::debug!(player = number, "attack with an empty spread-shot weapon");
                return Vec::new();
            }
            if hand.is_none() {
                return Vec::new();
            }
        }
        // With the right-click held, a gun or a bow fires the aimed shot of its aim fragment
        // (`MusketAttack1`, `BowAttack1`, the drawn charge's `BowAttack2`), not the hip fire
        // (`MusketAttack_Hipfire`, `BowAttack_Hipshot`; `player_2h_ranged.adb`,
        // `player_bow.adb`).
        let aiming = records.first().is_some_and(InputRecord::blocking);
        let aimed = stance
            .filter(|_| aiming)
            .and_then(|stance| stance.aimed_attack(kind));
        let Some(attack) = aimed
            .or_else(|| {
                stance
                    .and_then(|stance| stance.schedule(step))
                    .and_then(|schedule| schedule.attack(kind))
            })
            .cloned()
        else {
            tracing::debug!(
                player = number,
                step,
                ?kind,
                "attack of a stance without it"
            );
            return vec![Err(AttackRefusal::NotWeaponAttack)];
        };
        if !spread && held.is_some_and(|weapon| weapon.max_loaded_ammo > 0) {
            // A weapon with loaded shots that fires through the server's attack (`MaxLoadedAmmo`
            // of a musket) takes one; with none loaded the attack is the client's reload, as
            // the spread-shot weapon's above.
            let fired = self
                .world
                .get_mut::<Player>(entity)
                .and_then(|mut player| Some(player.gear.as_mut()?.fire(now_ms)))
                .unwrap_or(false);
            if !fired {
                tracing::debug!(player = number, "attack with an empty weapon");
                return Vec::new();
            }
        }
        let view = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.view);
        let newest_view = records.first().and_then(|newest| newest.bit_6);
        self.player_swings.push(PlayerSwing {
            attacker: connection.clone(),
            aim: Aim {
                facing: newest_view
                    .map(|[yaw, _]| yaw)
                    .or(view.map(|view| view.yaw)),
                pitch: newest_view
                    .map(|[_, pitch]| pitch)
                    .or(view.map(|view| view.pitch)),
                lock_target,
            },
            kind,
            attack,
            started_ms,
            pressed_ms,
            hand,
            not_before_ms: if hand.is_some() {
                now_ms.saturating_add(SHOT_PAIR_MS)
            } else {
                now_ms
            },
            hits: Vec::new(),
            swept_ms: None,
        });
        attacks[1..]
            .iter()
            .map(|_| Err(AttackRefusal::TooFast))
            .collect()
    }

    /// Whether `stance` has a melee basic attack.
    fn swings(stance: &Stance) -> bool {
        [AttackKind::Light, AttackKind::Heavy, AttackKind::Charged]
            .into_iter()
            .filter_map(|kind| stance.attack(kind))
            .any(|attack| matches!(attack.hit, AttackHit::Melee { .. }))
    }

    /// Lands the player attacks due at `now_ms` ([`SimWorld::land`]), then the projectiles that
    /// got to their mob ([`SimWorld::projectile_flights_due`]). A melee attack waits through its
    /// damage window for a target.
    pub(super) fn player_swings_due(
        &mut self,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let mut results = Vec::new();
        let mut later = Vec::new();
        for mut swing in std::mem::take(&mut self.player_swings) {
            if now_ms < swing.due_ms() {
                later.push(swing);
                continue;
            }
            if let Some(slot_id) = swing.hand.take()
                && !self.melee_hand(&swing, slot_id)
            {
                continue;
            }
            match self.land(&mut swing, now_ms) {
                Landing::Done(landed) => results.extend(landed),
                Landing::Waiting(landed) => {
                    results.extend(landed);
                    later.push(swing);
                }
            }
        }
        self.player_swings = later;
        results.extend(self.projectile_flights_due(now_ms));
        results
    }

    /// Whether `swing`, made while the server held a spread-shot weapon, was a swing of the melee
    /// option `slot_id`: then the server's hand takes that weapon. A shot request from
    /// [`SHOT_PAIR_MS`] before the press on shows it was a shot.
    fn melee_hand(&mut self, swing: &PlayerSwing, slot_id: u32) -> bool {
        let Some(entity) = self.by_connection.get(&swing.attacker).copied() else {
            return false;
        };
        let shot = self
            .world
            .get::<Vitals>(entity)
            .and_then(|vitals| vitals.shot_requested_ms)
            .is_some_and(|requested| requested.saturating_add(SHOT_PAIR_MS) >= swing.pressed_ms);
        if shot {
            return false;
        }
        let time_ns = unix_ns(self.server_ms);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            let number = player.number;
            if let Some(gear) = player.gear.as_mut() {
                gear.take_in_hand(slot_id, time_ns);
                tracing::debug!(
                    player = number,
                    slot_id,
                    "melee attack with no shot: hand follows"
                );
            }
        }
        true
    }

    /// The server's hand of player `entity` (`number`) takes the lowest main-hand option whose
    /// weapon `wanted` picks, when it holds another one.
    fn take_in_hand(&mut self, entity: Entity, number: u64, wanted: impl Fn(&WeaponRow) -> bool) {
        if self
            .held_weapon(entity)
            .is_some_and(|(weapon, _)| wanted(&weapon))
        {
            return;
        }
        let data = Arc::clone(&self.combat.data);
        let time_ns = unix_ns(self.server_ms);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(gear) = player.gear.as_mut() else {
            return;
        };
        let Some(slot_id) = gear.option_with(|item| data.weapon_of_item(item).is_some_and(&wanted))
        else {
            return;
        };
        gear.take_in_hand(slot_id, time_ns);
        tracing::debug!(player = number, slot_id, "hand follows the client");
    }

    /// The stance of the weapon in the hand of player `entity`, else of the config's weapon.
    fn hand_stance(&self, entity: Entity) -> Option<&Stance> {
        let data = &self.combat.data;
        let weapon = self
            .held_weapon(entity)
            .map_or(*data.weapon(), |(weapon, _)| weapon);
        data.stance(weapon.stance)
    }

    /// The block, dodge and quick slot keys of the newest input record `newest` of player
    /// `number`: a release of block starts its stamina's regen delay again; a new press of dodge
    /// starts a roll, or a dash when `swimming`, if its stamina allows it, and a roll keeps mob
    /// hits off in its stance's invulnerability window; a new press of quick slot 1 drinks when
    /// the stance's item use of the slot's item takes effect.
    fn stamina_input(
        &mut self,
        entity: Entity,
        number: u64,
        newest: &InputRecord,
        swimming: bool,
        now_ms: u64,
    ) {
        let stance = self.hand_stance(entity).cloned();
        // A potion takes effect at its stance's `Potion` use (333 ms), food at `Eat` (1,000 ms).
        let drink_ms = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref()?.worn_item(slot::QUICK_SLOT_1))
            .and_then(|item| self.content.item_rules.consumable(item))
            .and_then(|consumable| consumable.use_tag.as_deref())
            .and_then(|tag| stance.as_ref()?.use_effect_ms(tag));
        let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) else {
            return;
        };
        let block_key = newest.blocking();
        let block_released = vitals.block_key && !block_key;
        if block_key != vitals.block_key {
            tracing::debug!(player = number, block_key, "block key");
        }
        vitals.block_key = block_key;
        if block_released {
            vitals.guard_broken = false;
        }
        let dodge = newest.dodging();
        let dodge_pressed = dodge && !vitals.dodge_held;
        vitals.dodge_held = dodge;
        let quick = newest.using_quick_slot_1();
        if quick
            && !vitals.quick_slot_held
            && let Some(drink_ms) = drink_ms
        {
            self.drinks.push((number, now_ms.saturating_add(drink_ms)));
        }
        vitals.quick_slot_held = quick;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player.stamina.hold_block(block_key);
        if block_released {
            player.stamina.block_released();
        }
        let kind = if swimming {
            Dodge::SwimDash
        } else {
            Dodge::Roll
        };
        if dodge_pressed
            && player.stamina.dodge(kind, now_ms)
            && kind == Dodge::Roll
            && let Some([from_ms, until_ms]) =
                stance.and_then(|stance| stance.dodge_invulnerable_ms)
            && let Some(mut vitals) = self.world.get_mut::<Vitals>(entity)
        {
            vitals.invulnerable_ms = Some((
                now_ms.saturating_add(from_ms),
                now_ms.saturating_add(until_ms),
            ));
        }
    }

    /// Runs the stamina of every player one tick at `now_ms`.
    pub(super) fn tick_stamina(&mut self, now_ms: u64) {
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            player.stamina.tick(now_ms);
        }
    }

    /// Moves every player's hands on to `now_ms` at server time `server_ms`: a new weapon in a
    /// main-hand option gets its shots, a drawn weapon reaches the hand, an empty one is loaded.
    pub(super) fn tick_hands(&mut self, now_ms: u64, server_ms: u64) {
        let data = Arc::clone(&self.combat.data);
        let weapon_of = |item: &str| {
            let weapon = data.weapon_of_item(item)?;
            Some((weapon.max_loaded_ammo, weapon.reload_ms))
        };
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            if let Some(gear) = player.gear.as_mut() {
                gear.arm(weapon_of, unix_ns(server_ms));
                gear.tick_hands(now_ms, unix_ns(server_ms));
            }
        }
    }

    /// How long the sheathe action of the weapon in the hand of player `entity` takes to put it
    /// away ([`Stance::sheathe_ms`]); `None` when the weapon or its stance's marker is not known.
    fn sheathe_ms(&self, entity: Entity) -> Option<u64> {
        let (weapon, _) = self.held_weapon(entity)?;
        self.combat.data.stance(weapon.stance)?.sheathe_ms
    }

    /// Player `entity` stands at `at` at `now_ms`: in deep water (its body at the swim floor, as
    /// [`nw_sim_core::character`] swims) the sheathe action starts and puts the drawn weapon away
    /// (`playeractions_sheathe.actionlist`: `Sheathe_MainHand_*_UB` starts on `IfWaterDepth`
    /// `Deep`, the depth at which `playeractions_swim.actionlist` starts the swim).
    pub(super) fn sheathe_in_deep_water(&mut self, entity: Entity, at: Position, now_ms: u64) {
        if at.z > swim_floor(self.settings.ocean_level_m) {
            return;
        }
        let Some(after_ms) = self.sheathe_ms(entity) else {
            return;
        };
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
            && gear.sheathe(now_ms, after_ms)
        {
            tracing::debug!(
                player = player.number,
                after_ms,
                "weapon sheathed in deep water"
            );
        }
    }

    /// The weapon in the hand of the player `entity` with its gear score, when its row is known.
    /// How the player `entity` blocks a projectile: with the shield worn with its weapon in the
    /// hand when that has `CanBlockRanged`, else with that weapon (Inferred: a shield's block is
    /// the shield's); `None` when neither blocks one.
    fn ranged_block_of(&self, entity: Entity) -> Option<RangedBlock> {
        let gear = self.world.get::<Player>(entity)?.gear.as_ref()?;
        let block = |item: &str| self.combat.data.weapon_of_item(item)?.ranged_block;
        gear.off_hand_weapon()
            .and_then(block)
            .or_else(|| gear.main_weapon().and_then(|(item, _)| block(item)))
    }

    fn held_weapon(&self, entity: Entity) -> Option<(WeaponRow, u32)> {
        let (item, gear_score) = self
            .world
            .get::<Player>(entity)?
            .gear
            .as_ref()?
            .main_weapon()?;
        Some((*self.combat.data.weapon_of_item(item)?, gear_score))
    }

    /// Every living, not returning mob, here or a ghost, with its body, at each place where it
    /// stood in the look-back ([`super::mob_trail`]): the client shows it a little behind the
    /// server.
    fn target_places(&mut self) -> Vec<(u64, Position, Capsule)> {
        let candidates: Vec<(u64, Position, Capsule)> = self
            .mob_states()
            .filter(|(_, _, state)| state.health > 0 && !state.returning)
            .map(|(mob, at, _)| (mob, at, self.mobs.capsule(mob)))
            .collect();
        candidates
            .iter()
            .flat_map(|(mob, at, body)| {
                std::iter::once(*at)
                    .chain(self.mob_trails.recent(*mob))
                    .map(move |place| (*mob, place, *body))
            })
            .collect()
    }

    /// The target of the homing `homing` of a player at `from` with the aim `aim` among `places`:
    /// the locked target when it is in reach, else the best one in reach at the aim's yaw
    /// ([`homing_score`]); `None` when none is.
    fn homing_target(
        homing: &Homing,
        from: Position,
        aim: Aim,
        places: &[(u64, Position, Capsule)],
    ) -> Option<u64> {
        let best = |facing: Option<f32>, wanted: Option<u64>| {
            places
                .iter()
                .filter(|(mob, _, _)| wanted.is_none_or(|wanted| wanted == *mob))
                .filter_map(|(mob, at, body)| {
                    Some((
                        homing_score(homing, from, facing, *at, body.radius_m)?,
                        *mob,
                    ))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, mob)| mob)
        };
        aim.lock_target
            .and_then(|lock| best(None, Some(lock)))
            .or_else(|| best(Some(aim.facing?), None))
    }

    /// Lands `swing` at `now_ms`, when it is due. A melee attack sweeps its shapes from their
    /// poses of its last sweep to their poses now ([`SimWorld::sweep`]); it misses when its window
    /// closes without a hit. A shot flies, or hits at once, along the aim ([`SimWorld::cast`]).
    fn land(&mut self, swing: &mut PlayerSwing, now_ms: u64) -> Landing {
        let done = |refusal| Landing::Done(vec![Err(refusal)]);
        let Some(entity) = self.by_connection.get(&swing.attacker).copied() else {
            return done(AttackRefusal::NotInWorld);
        };
        if self
            .world
            .get::<Vitals>(entity)
            .is_some_and(|vitals| vitals.health.is_dead())
        {
            return done(AttackRefusal::Dead);
        }
        let Some(position) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return done(AttackRefusal::NotInWorld);
        };
        let places = self.target_places();
        match swing.attack.hit.clone() {
            AttackHit::Melee { homing, shapes, .. } => {
                let landed = self.sweep(swing, homing.as_ref(), &shapes, position, &places, now_ms);
                if now_ms < swing.closes_ms() {
                    Landing::Waiting(landed)
                } else if swing.hits.is_empty() {
                    Landing::Done(vec![Ok(AttackOutcome::Missed)])
                } else {
                    Landing::Done(landed)
                }
            }
            AttackHit::Projectile {
                range_m,
                radius_m,
                flight,
                ..
            } => self.fire(
                swing,
                Shot::projectile(range_m, radius_m, flight),
                position,
                &places,
                now_ms,
            ),
            AttackHit::HitScan { range_m, .. } => {
                self.fire(swing, Shot::hit_scan(range_m), position, &places, now_ms)
            }
        }
    }

    /// The yaw a player at `from` faces in a melee attack with `aim`: toward the target of the
    /// attack's homing when one is in its reach (the clip turns the attacker,
    /// `ApplyRotationDuringCameraLock` toward a locked one), else the aim's.
    fn melee_facing(
        homing: Option<&Homing>,
        from: Position,
        aim: Aim,
        places: &[(u64, Position, Capsule)],
    ) -> Option<f32> {
        homing
            .and_then(|homing| Self::homing_target(homing, from, aim, places))
            .and_then(|target| places.iter().find(|(mob, _, _)| *mob == target))
            .map(|(_, at, _)| yaw_toward(from, *at))
            .or(aim.facing)
    }

    /// Sweeps the shapes of `swing` (a melee attack of homing `homing`) at `now_ms`: each shape
    /// whose window meets the time since the last sweep goes from its pose then to its pose now,
    /// on the body at `position` facing [`SimWorld::melee_facing`], and hits every target of
    /// `places` it touches that it has not hit yet. The first hit of a swing keeps the attack
    /// interval; the others of the same swing do not.
    fn sweep(
        &mut self,
        swing: &mut PlayerSwing,
        homing: Option<&Homing>,
        shapes: &[MeleeShape],
        position: Position,
        places: &[(u64, Position, Capsule)],
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let to_ms = now_ms.saturating_sub(swing.started_ms);
        let from_ms = swing
            .swept_ms
            .map_or(0, |swept| swept.saturating_sub(swing.started_ms));
        swing.swept_ms = Some(now_ms);
        let Some(facing) = Self::melee_facing(homing, position, swing.aim, places) else {
            return Vec::new();
        };
        let frame = body_frame(position, facing);
        let mut touched: Vec<u64> = Vec::new();
        for shape in shapes {
            let [open_ms, close_ms] = shape.window_ms;
            if to_ms < open_ms || from_ms > close_ms {
                continue;
            }
            let (Some(from), Some(to)) = (
                shape.pose_at(from_ms.max(open_ms)),
                shape.pose_at(to_ms.min(close_ms)),
            ) else {
                continue;
            };
            let swept = SweptShape {
                shape: shape.shape,
                from: frame.then_local(&from),
                to: frame.then_local(&to),
            };
            for (mob, at, body) in places {
                if !swing.hits.contains(mob) && !touched.contains(mob) && swept.touches(*at, *body)
                {
                    touched.push(*mob);
                }
            }
        }
        tracing::debug!(
            attacker = ?swing.attacker,
            x = position.x,
            y = position.y,
            z = position.z,
            facing,
            kind = ?swing.kind,
            action = swing.attack.action,
            ?touched,
            targets = ?places
                .iter()
                .map(|(mob, at, _)| (*mob, at.x, at.y, at.z))
                .collect::<Vec<_>>(),
            "player swing"
        );
        let damage_id = swing.attack.damage_row.clone();
        touched
            .into_iter()
            .map(|target| {
                let paced = swing.hits.is_empty();
                swing.hits.push(target);
                self.hit_paced(&swing.attacker, target, &damage_id, None, paced, now_ms)
            })
            .collect()
    }

    /// Fires `shot` of `swing` from `position` along its aim at `now_ms`.
    fn fire(
        &mut self,
        swing: &PlayerSwing,
        shot: Shot,
        position: Position,
        places: &[(u64, Position, Capsule)],
        now_ms: u64,
    ) -> Landing {
        let yaw = Self::aim_yaw(swing.aim.lock_target, swing.aim.facing, position, places);
        let (Some(yaw), Some(pitch)) = (yaw, swing.aim.pitch) else {
            return Landing::Done(vec![Err(AttackRefusal::NoAim)]);
        };
        Landing::Done(self.cast(
            &swing.attacker,
            (shot, swing.attack.damage_row.as_str()),
            (position, AimLine { yaw, pitch }),
            places,
            now_ms,
        ))
    }

    /// What an attack sees of the mob `target`: here, or a ghost of its owner.
    fn target_view(&mut self, target: u64) -> Option<TargetView> {
        if let Some(mob) = self.mobs.get(target) {
            let view = TargetView {
                position: mob.position,
                dead: mob.health.is_dead(),
                returning: mob.returning(self.mobs.ai()),
                level: 0,
                owner: None,
            };
            return Some(TargetView {
                level: self.mobs.level(target).unwrap_or_default(),
                ..view
            });
        }
        let (owner, ghost) = self.ghost(target)?;
        let state = ghost.mob?;
        Some(TargetView {
            position: ghost.position,
            dead: state.health == 0,
            returning: state.returning,
            level: state.level,
            owner: Some(owner.clone()),
        })
    }

    /// A hit for `owner`, the sim of `target`; a mob's hit on a player carries the stamina its
    /// block takes and whether it is a projectile's.
    fn remote_hit(
        &self,
        owner: BusId,
        target: u64,
        attacker: u64,
        amount: u32,
        threat: u64,
        mob_blow: Option<(f32, bool)>,
    ) -> RemoteHit {
        RemoteHit {
            owner,
            hit: Hit {
                target,
                attacker,
                attacker_sim: self.me.id.clone(),
                amount,
                threat,
                block_stamina_milli: mob_blow.map(|(stamina, _)| Hit::milli_of(stamina)),
                ranged: mob_blow.is_some_and(|(_, ranged)| ranged),
            },
        }
    }

    /// Applies a hit of `amount` with `threat` from `attacker` on `target`, a mob or a player of
    /// this sim. Returns `None` when the target is not here, is dead, (a mob) is returning, or
    /// (a player) is frozen for a handoff, whose offer already carries its health.
    pub fn apply_hit(
        &mut self,
        target: u64,
        attacker: u64,
        amount: u32,
        threat: u64,
        now_ms: u64,
    ) -> Option<AttackOutcome> {
        self.apply_hit_with(target, attacker, amount, threat, now_ms, None)
    }

    /// Mob `mob`, killed by player `killer`, counts for the kill tasks of the killer's quest
    /// (`TaskKillContribution`; the tutorial counts its kills in its encounter).
    fn quest_kill(&mut self, mob: u64, killer: u64, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let Some((vitals_id, categories)) = self
            .mobs
            .vitals_of(mob)
            .and_then(|vitals| content.kill_types.of(vitals))
        else {
            return;
        };
        let spawn_categories = content
            .kill_types
            .spawn_categories(self.mobs.category_of(mob));
        self.quest_event(
            killer,
            Event::Killed {
                vitals_id,
                categories,
                spawn_categories: &spawn_categories,
            },
            now_ms,
        );
    }

    /// [`SimWorld::apply_hit`] of a mob hit of `style`: a player's 3601 carries its damage type
    /// and its affix's part as a second item (else [`DEFAULT_HIT_TAG`]).
    fn apply_hit_with(
        &mut self,
        target: u64,
        attacker: u64,
        amount: u32,
        threat: u64,
        now_ms: u64,
        style: Option<&MobHitStyle>,
    ) -> Option<AttackOutcome> {
        if spawn_index(target).is_some() {
            let [x, y, z] = self.attacker_position(attacker);
            if !self
                .mobs
                .hurt(target, attacker, Position { x, y, z }, threat, now_ms)
            {
                return None;
            }
            let damage = self.mobs.get_mut(target)?.health.take(amount);
            let outcome = match damage {
                Damage::Killed => {
                    self.mobs.died(target, now_ms);
                    AttackOutcome::Killed { target }
                }
                Damage::Hurt | Damage::AlreadyDead => AttackOutcome::Hit { target, amount },
            };
            if damage != Damage::AlreadyDead {
                self.wear_weapon(attacker);
            }
            if matches!(outcome, AttackOutcome::Killed { .. }) {
                self.drop_loot(target, attacker, now_ms);
                self.quest_kill(target, attacker, now_ms);
                self.note_kill(target, attacker);
                self.reward_kill(target, attacker);
            }
            return Some(outcome);
        }
        let entity = self.player_by_number(target)?;
        let player = self.world.get::<Player>(entity)?;
        if player.frozen || self.is_god(entity) {
            return None;
        }
        let connection = player.connection.clone();
        let attacker_at = self.attacker_position(attacker);
        let scale = self
            .mobs
            .hit_scale(attacker, (1.0 - self.combat.player_armor_share) as f32);
        // The player's status effects take their share of the hit (`Abs*`).
        let tag = style.map_or(DEFAULT_HIT_TAG, |style| style.tag);
        let amount = self.player_absorbs(entity, tag, amount);
        self.player_hit_taken(entity);
        let mut vitals = self.world.get_mut::<Vitals>(entity)?;
        let max_health = amount_of(vitals.health.max());
        let damage = vitals.health.take(amount);
        if damage == Damage::Killed {
            vitals.dead_since_ms = Some(now_ms);
        }
        if damage == Damage::AlreadyDead {
            return None;
        }
        // A hit is combat: it ends the road boost on the frame of the hit
        // (`Traversal_Road_Sprint_Speed_Boost_CombatRemoval` `IfDamaged`).
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.tracked.enter_combat();
        }
        // A hit ends a summon and takes mount stamina; a death ends the ride ([`super::mount`]).
        self.mount_hit(
            entity,
            amount_of(amount),
            max_health,
            damage == Damage::Killed,
            now_ms,
        );
        let total = amount_of(amount);
        let affix = style.and_then(|style| style.affix);
        // A share of a hit: f32 holds it.
        let affix_share = affix.map_or(0.0, |affix| affix.share as f32);
        let melee = MeleeDamage {
            attacker,
            amount: total * (1.0 - affix_share),
            max_health,
            attacker_at,
            tag: style.map_or(DEFAULT_HIT_TAG, |style| style.tag),
            scale,
            fire: affix.map(|affix| (affix.damage_type, total * affix_share)),
        };
        self.send_reliable(
            connection.clone(),
            &ServerMessage::OnDamage(OnDamageMsg::melee(target, &melee)),
        );
        if damage == Damage::Hurt {
            return Some(AttackOutcome::Hit { target, amount });
        }
        self.mobs.forget(target);
        self.meta_event(entity, super::meta_achievements::MetaEvent::Died);
        // A death wears the worn items (durability loss, [`super::repair`]); a player killer
        // makes it a PvP death.
        let by_player =
            spawn_index(attacker).is_none() && self.player_by_number(attacker).is_some();
        self.wear_at_death(entity, by_player);
        // A death puts the drawn weapon away (retail: 161 put-aways at the 4299).
        let time_ns = unix_ns(self.server_ms);
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
        {
            gear.put_away(time_ns, None);
        }
        if let Some(departure) = self.departure(entity) {
            self.deaths.push(departure);
        }
        self.send_reliable(
            connection,
            &ServerMessage::DeathRecap(DeathRecapMsg::of(target)),
        );
        Some(AttackOutcome::Killed { target })
    }

    /// Where `attacker` stands: a mob or a player, here or as a ghost; the origin when it is
    /// none of them (retail always names the creature's position in a hit on a player).
    fn attacker_position(&mut self, attacker: u64) -> [f32; 3] {
        let at = match self.mobs.get(attacker) {
            Some(mob) => Some(mob.position),
            None => self
                .player_by_number(attacker)
                .and_then(|entity| self.world.get::<Body>(entity))
                .map(|body| body.motion.position())
                .or_else(|| self.ghost(attacker).map(|(_, ghost)| ghost.position)),
        };
        at.map_or([0.0; 3], |at| [at.x, at.y, at.z])
    }

    /// The player with entity number `number`.
    pub(super) fn player_by_number(&mut self, number: u64) -> Option<Entity> {
        let mut query = self.world.query::<(Entity, &Player)>();
        query
            .iter(&self.world)
            .find(|(_, player)| player.number == number)
            .map(|(entity, _)| entity)
    }

    /// Starts the swings `started` at `now_ms`, server time `server_ms` (their animation) and
    /// gives the swings that hit now: each lands its mob's hit delay after its start (retail: the
    /// hit came a fixed time after the attack started, per creature; creature-animation note,
    /// section 5.1).
    pub(super) fn swings_landing(
        &mut self,
        started: Vec<MobAttackIntent>,
        now_ms: u64,
        server_ms: u64,
    ) -> Vec<(MobAttackIntent, Option<MoveHit>)> {
        for swing in started {
            let pending = |lands_after_ms: u64, hit| PendingMobSwing {
                lands_ms: now_ms.saturating_add(lands_after_ms),
                started_ms: server_ms,
                swing: swing.clone(),
                hit,
            };
            match (swing.mob_move, &swing.strike) {
                (Some(mob_move), _) => {
                    self.mob_timelines
                        .play(swing.mob, mob_move.fragment, mob_move.length_ms);
                    for hit in mob_move.hits {
                        self.swings.push(pending(hit.at_ms, Some(*hit)));
                    }
                }
                // A damage window hits when it comes; its tree plays the attack.
                (None, Some(_)) => self.swings.push(pending(0, None)),
                (None, None) => {
                    self.mob_timelines.attacked(swing.mob);
                    self.swings
                        .push(pending(self.mobs.hit_delay_ms(swing.mob), None));
                }
            }
        }
        let (landing, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.swings)
            .into_iter()
            .partition(|pending| now_ms >= pending.lands_ms);
        self.swings = later;
        landing
            .into_iter()
            // A mob that died or reeled from a player's hit before its swing landed does not hit.
            .filter(|pending| {
                self.mobs
                    .health(pending.swing.mob)
                    .is_some_and(|health| health > 0)
                    && !self
                        .mob_timelines
                        .reeled_since(pending.swing.mob, pending.started_ms)
            })
            .map(|pending| (pending.swing, pending.hit))
            .collect()
    }

    /// Whether the swing `intent` of a mob at `at` reaches its target's body at `body`. A mob
    /// with a reach (its damage window's, or its pace's) lunges first, when the target is within
    /// its lunge turn of where it aimed: toward the body, up to its longest lunge, to where it
    /// stops or the bodies touch; then its wedge must touch the body (collision note: every
    /// retail mob hit landed 0.79 to 1.65 m from the player). A paced mob without one hits
    /// within its gait's swing start plus [`crate::mobs::SWING_HIT_MARGIN_M`].
    fn mob_swing_reaches(
        &mut self,
        intent: &MobAttackIntent,
        at: Position,
        body: Position,
    ) -> bool {
        let capsule = self.mobs.capsule(intent.mob);
        // A damage window's shape reaches from the front of the body (`mob_ai::window_reach`);
        // the wedge's reach starts at the body's middle.
        let reach = intent
            .strike
            .as_ref()
            .map(|strike| MobReach {
                reach_m: strike.reach.reach_m + capsule.radius_m,
                ..strike.reach
            })
            .or_else(|| self.mobs.reach(intent.mob));
        let Some(reach) = reach else {
            return self
                .mobs
                .hit_reach_m(intent.mob)
                .is_some_and(|reach_m| body.horizontal_distance(at) <= reach_m);
        };
        let aim = yaw_toward(at, intent.target_at);
        let toward = yaw_toward(at, body);
        let (from, facing) = if turn(aim, toward).abs() <= reach.lunge_turn_rad {
            let stop_m = capsule.radius_m + PLAYER_CAPSULE.radius_m + LUNGE_GAP_M;
            let lunge_m = (body.horizontal_distance(at) - stop_m).clamp(0.0, reach.lunge_max_m);
            let from = self
                .mobs
                .lunge_over(intent.mob, body, lunge_m, &*self.ground)
                .unwrap_or(at);
            (from, toward)
        } else {
            (at, aim)
        };
        Wedge {
            from,
            own: capsule,
            facing,
            reach_m: reach.reach_m,
            half_arc_rad: MOB_SWING_HALF_ARC_RAD,
        }
        .touches(body, PLAYER_CAPSULE)
    }

    /// How the hit `intent` of a mob, of the move hit `move_hit` when it has one, lands on the
    /// player `defender`, from its damage rows: the row's damage type and affix, its multiple, and
    /// the stamina a block takes ([`AttackRow::block_stamina`], with the weapon's `BlockStability`
    /// of a defender of this sim, else of the config's weapon: a player of another sim blocks
    /// there). The row is the damage window's (its multiple is the row's coefficient,
    /// [`crate::mobs::ROW_CARRIES_ATTACK`]), or a paced mob's move row over its attack row (which
    /// the pace's attack factor fits), times its second phase's multiple. A mob without rows the
    /// tables have hits with [`DEFAULT_HIT_TAG`] and takes [`DEFAULT_BLOCK_STAMINA`].
    fn mob_hit_style(
        &self,
        intent: &MobAttackIntent,
        move_hit: Option<MoveHit>,
        defender: Option<Entity>,
    ) -> MobHitStyle {
        let data = &self.combat.data;
        let plain = MobHitStyle {
            tag: DEFAULT_HIT_TAG,
            affix: None,
            damage_scale: 1.0,
            block_stamina: DEFAULT_BLOCK_STAMINA,
            row: None,
            ranged: false,
        };
        let stability = defender
            .and_then(|defender| self.held_weapon(defender))
            .map_or(data.weapon().block_stability, |(weapon, _)| {
                weapon.block_stability
            });
        if let Some(strike) = &intent.strike {
            let Some(row) = data.table_attack(&strike.table, &strike.row) else {
                tracing::warn!(mob = intent.mob, ?strike, "mob damage row missing");
                return plain;
            };
            let block_stamina = strike
                .weapon
                .as_deref()
                .and_then(|weapon| data.weapon_of_item(weapon))
                // A few dozen stamina points.
                .map_or(DEFAULT_BLOCK_STAMINA, |weapon| {
                    row.block_stamina(weapon, stability) as f32
                });
            return MobHitStyle {
                tag: row.damage_type.unwrap_or(DEFAULT_HIT_TAG),
                affix: row.affix,
                damage_scale: row.coefficient,
                block_stamina,
                row: Some(RowKey::of(&row.table, &row.damage_id)),
                ranged: false,
            };
        }
        let mob = intent.mob;
        let Some(attack) = self.mobs.attack(mob) else {
            return plain;
        };
        let attack_row = data.table_attack(attack.table, attack.row);
        let row = match move_hit {
            Some(hit) => data.table_attack(attack.table, hit.row),
            None => attack_row,
        };
        let weapon = data.weapon_of_item(attack.weapon);
        let (Some(row), Some(attack_row), Some(weapon)) = (row, attack_row, weapon) else {
            tracing::warn!(mob, ?attack, "mob attack rows missing");
            return plain;
        };
        let phase_mult = self
            .mobs
            .phase_two(mob)
            .map_or(1.0, |phase| phase.damage_mult);
        MobHitStyle {
            tag: row.damage_type.unwrap_or(DEFAULT_HIT_TAG),
            affix: row.affix,
            damage_scale: row.coefficient / attack_row.coefficient * phase_mult,
            // A few dozen stamina points.
            block_stamina: row.block_stamina(weapon, stability) as f32,
            row: Some(RowKey::of(&row.table, &row.damage_id)),
            ranged: false,
        }
    }

    /// How the player `entity` (number `target`) takes a mob's hit of `amount` whose block takes
    /// `block_stamina`: the health it takes, or `None` when it takes none.
    ///
    /// - A player whose client has not spawned into the world yet takes no hit (spawn
    ///   protection: it would die at the loading screen otherwise).
    /// - A swing into a dodge roll misses: retail sends no hit message then.
    /// - A held block with stamina takes the hit's block stamina and no health; in the tutorial
    ///   retail sends 2218 `blocked` 1 with no 3601, in the open world only the stamina change
    ///   (newchar-retail 1353915: `cur` 100 to 86.8, no 2218, no 3601).
    /// - The blocked hit that takes the last stamina breaks the guard: it takes half its health
    ///   and the block ends until the key is let go (newchar-retail 422387: `cur` 24.265 to 0,
    ///   2218 `blocked` 1 and a 3601 of 17.025, the same attack unblocked 34.05; `isBlockActive`
    ///   0 at 422423 while the key stayed held).
    /// - In the tutorial every other hit comes with 2218 `blocked` 0; retail sent no 2218 in the
    ///   open world (0 of 34 hits in general-other-01, none in newchar-retail after 764894).
    /// - A projectile's hit (`blow`'s flag) is blocked only by a weapon with `CanBlockRanged`
    ///   ([`SimWorld::ranged_block_of`]): its block takes the block stamina times
    ///   `RangedBlockStaminaDamageScaling`, and `RangedBlockHealthDamageScaling` of the health
    ///   passes it (Inferred from the data: no retail ranged block was captured).
    fn defend(
        &mut self,
        entity: Entity,
        target: u64,
        mob: u64,
        amount: u32,
        (block_stamina, ranged): (f32, bool),
        now_ms: u64,
    ) -> Option<u32> {
        let vitals = *self.world.get::<Vitals>(entity)?;
        if !vitals.spawned {
            return None;
        }
        if vitals
            .invulnerable_ms
            .is_some_and(|(from, until)| from <= now_ms && now_ms < until)
        {
            tracing::debug!(mob, target, "mob swing missed: dodge");
            return None;
        }
        let tutorial = self.is_tutorial();
        let ranged_block = ranged.then(|| self.ranged_block_of(entity));
        let (can_block, block_stamina, passes) = match ranged_block {
            None => (true, block_stamina, 0.0),
            Some(None) => (false, block_stamina, 1.0),
            Some(Some(block)) => (
                true,
                // A few dozen stamina points.
                block_stamina * block.stamina_scale as f32,
                block.health_scale,
            ),
        };
        let mut player = self.world.get_mut::<Player>(entity)?;
        let guard = (can_block && vitals.blocking() && player.stamina.current() > 0.0)
            .then(|| player.stamina.blocked_hit(block_stamina));
        let stamina = player.stamina.current();
        let connection = player.connection.clone();
        tracing::debug!(
            mob,
            target,
            block_key = vitals.block_key,
            guard_broken = vitals.guard_broken,
            ?guard,
            stamina,
            amount,
            "mob hit on a player"
        );
        if tutorial {
            self.send_reliable(
                connection,
                &ServerMessage::MeleeHit(MeleeHitMsg::on(target, guard.is_some())),
            );
        }
        match guard {
            // A hit of a few hundred points.
            Some(Guard::Held) => {
                Some((f64::from(amount) * passes).round() as u32).filter(|passed| *passed > 0)
            }
            Some(Guard::Broken) => {
                self.world.get_mut::<Vitals>(entity)?.guard_broken = true;
                Some(amount / BROKEN_GUARD_DAMAGE_DIVISOR)
            }
            None => Some(amount),
        }
    }

    /// A mob hit from another sim on player `target` of this one, with `blow` (its block stamina
    /// and whether it is a projectile's): it takes its block and dodge here, where the player's
    /// state is, then lands. Returns `None` when it takes no health
    /// (see [`SimWorld::defend`]) or the player is not here.
    pub fn remote_mob_hit(
        &mut self,
        target: u64,
        attacker: u64,
        amount: u32,
        blow: (f32, bool),
        now_ms: u64,
    ) -> Option<AttackOutcome> {
        let entity = self.player_by_number(target)?;
        let amount = self.defend(entity, target, attacker, amount, blow, now_ms)?;
        self.apply_hit(target, attacker, amount, 0, now_ms)
    }

    /// Applies the mobs' hits on the players of this sim; a hit on a player ghost goes to its
    /// sim.
    pub(super) fn mob_hits(
        &mut self,
        intents: Vec<(MobAttackIntent, Option<MoveHit>)>,
        now_ms: u64,
    ) -> Vec<AttackOutcome> {
        let armor_share = self.combat.player_armor_share;
        intents
            .into_iter()
            .filter_map(|(intent, move_hit)| {
                let mob = self.mobs.get(intent.mob)?;
                let at = mob.position;
                let mut hit = mob_hit(&mob.attack, armor_share);
                let defender = self.player_by_number(intent.target);
                // The same hit for a player of this sim and one of another (a ghost).
                let style = self.mob_hit_style(&intent, move_hit, defender);
                let scaled = f64::from(hit.amount)
                    * style.damage_scale
                    * self.mob_hit_scale(intent.mob, style.tag);
                // A hit of a few hundred points.
                hit.amount = scaled.round() as u32;
                tracing::debug!(
                    mob = intent.mob,
                    target = intent.target,
                    x = at.x,
                    y = at.y,
                    z = at.z,
                    amount = hit.amount,
                    "mob attack"
                );
                if let Some(clip) = intent
                    .strike
                    .as_ref()
                    .and_then(|strike| strike.ranged.as_ref())
                {
                    self.fire_mob_shot(&intent, clip, hit.amount, style, at, now_ms);
                    return None;
                }
                if let Some(entity) = defender {
                    // A swing at a player who moved away misses: retail sends no hit message.
                    let body = self.world.get::<Body>(entity)?.motion.position();
                    if !self.mob_swing_reaches(&intent, at, body) {
                        tracing::debug!(
                            mob = intent.mob,
                            distance = at.horizontal_distance(body),
                            dz = body.z - at.z,
                            "mob swing missed: out of reach"
                        );
                        return None;
                    }
                }
                self.land_mob_hit(intent.target, intent.mob, hit.amount, style, now_ms)
            })
            .collect()
    }

    /// Lands a mob's hit of `amount` in `style` on the player `target`: on a player of this sim
    /// it takes the block and dodge ([`SimWorld::defend`]) and its row's status effects; a hit
    /// on a player ghost goes to its sim.
    pub(super) fn land_mob_hit(
        &mut self,
        target: u64,
        mob: u64,
        amount: u32,
        style: MobHitStyle,
        now_ms: u64,
    ) -> Option<AttackOutcome> {
        if let Some(entity) = self.player_by_number(target) {
            let amount = self.defend(
                entity,
                target,
                mob,
                amount,
                (style.block_stamina, style.ranged),
                now_ms,
            )?;
            let outcome = self.apply_hit_with(target, mob, amount, 0, now_ms, Some(&style));
            if outcome.is_some() {
                self.mob_hit_effects(entity, mob, style.row, amount);
            }
            return outcome;
        }
        let (owner, ghost) = self.ghost(target)?;
        let owner = owner.clone();
        ghost.mob.is_none().then(|| {
            AttackOutcome::Remote(self.remote_hit(
                owner,
                target,
                mob,
                amount,
                0,
                Some((style.block_stamina, style.ranged)),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use nw_sim_core::MobState;

    use super::*;
    use crate::combat::RespawnRefusal;
    use crate::combat::test_support::TEST_PLAYER_HEALTH;
    use crate::combat::tests::test_combat;
    use crate::config::tests::committed;
    use crate::mobs::{VitalsSpawn, mob_entity};
    use crate::world::gear::{self, Gear};
    use crate::world::tests::{
        BORDER_X, CELL_M, MANY, MAX_PLAYERS, NOW, SERVER_TIME, admit_at, connection, join,
        messages_to, settings, slot, table, test_spawn, tick, world_with_spawns,
        world_with_spawns_in,
    };
    use crate::world::{MoveError, WorldSettings};
    use nw_bus::{Ghost, MobGhost, Point};
    use nw_content::combat::{CombatData, CombatRows};
    use nw_content::player_attacks::stance_key;
    use nw_messages::{
        BodyCodec, CombatEffectMsg, DamageDealtMsg, DeathRecapMsg, FacetHeader, RAW_ID_LEN,
        ServerMessage, SlotBits, SpreadHit,
    };

    const LIGHT: &str = "1H_Sword_Attack1";
    /// Yaw toward -x: player 1 of [`fight`] stands east of the mob and faces it.
    const FACING_WEST: f32 = std::f32::consts::FRAC_PI_2;
    const FACING_EAST: f32 = -std::f32::consts::FRAC_PI_2;
    const MOB_X: f32 = CELL_M / 2.0;
    /// 64 x 1.0 x (1 - 0.35), rounded.
    const SWORD_HIT: u32 = 42;
    const MOB_HEALTH: u32 = 1000;
    const INTERVAL_MS: u64 = crate::config::DEFAULT_MIN_ATTACK_INTERVAL_MS;
    const TICK_MS: u64 = 33;
    const MOB_RESPAWN_MS: u64 = 360_000;
    /// The committed `min_respawn_ms`.
    const MIN_RESPAWN_MS: u64 = 2_000;
    /// Far beyond any wait.
    const LONG_AFTER_MS: u64 = 60_000;
    const LETHAL: u32 = 100_000;
    /// A distance from the mob's middle that the sword's light swing reaches: its capsule's
    /// middle passes 1.06 m ahead of the root, 0.3 m long each way and 0.3 m round, and the test
    /// mob is 0.5 m round (`player-attacks-25810808.json`).
    const SWORD_REACH_M: f32 = 1.2;

    fn mob() -> u64 {
        mob_entity(0)
    }

    /// A world with one mob and player 1 two metres from it.
    fn fight() -> SimWorld {
        fight_at(MOB_X + 2.0)
    }

    /// A world with one mob and player 1 at `x`.
    fn fight_at(x: f32) -> SimWorld {
        fight_in(world_with_spawns(vec![test_spawn(MOB_X)]), x)
    }

    /// [`fight`] on the tutorial level.
    fn tutorial_fight() -> SimWorld {
        let mut tutorial = settings(MANY, MAX_PLAYERS);
        tutorial.level = nw_messages::TUTORIAL_LEVEL;
        fight_in(
            world_with_spawns_in(tutorial, vec![test_spawn(MOB_X)]),
            MOB_X + 2.0,
        )
    }

    /// `world` with player 1 at `x`.
    fn fight_in(mut world: SimWorld, x: f32) -> SimWorld {
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x);
        world
    }

    fn health_of(world: &mut SimWorld, number: u64) -> Health {
        let entity = world.player_by_number(number).expect("player");
        world.world.get::<Vitals>(entity).expect("vitals").health
    }

    fn mob_health(world: &mut SimWorld) -> Health {
        world.mobs.get_mut(mob()).expect("mob").health
    }

    /// What one input record of a test does with the attack key.
    #[derive(Clone, Copy)]
    enum Key {
        Press,
        Hold,
        Release,
    }

    /// The records of one message in the client's order, newest first, from `keys` oldest first,
    /// with counters from `first_counter` up (retail press: value 2 bit 40; release: value 2
    /// bit 31).
    fn message(first_counter: u16, keys: &[Key]) -> Vec<InputRecord> {
        const ACTION_VALUES: usize = 10;
        let mut records: Vec<InputRecord> = keys
            .iter()
            .zip(first_counter..)
            .map(|(key, counter)| {
                let mut values = vec![0; ACTION_VALUES];
                values[2] = match key {
                    Key::Press => 1 << 40,
                    Key::Hold => 0,
                    Key::Release => 1 << 31,
                };
                InputRecord {
                    u16_after_mask: counter,
                    slots: nw_messages::InputSlots {
                        count: 19,
                        values,
                        bytes: [128; 4],
                    },
                    bit_6: Some([FACING_WEST, 0.0]),
                    ..InputRecord::default()
                }
            })
            .collect();
        records.reverse();
        records
    }

    /// The test sword's stance ([`crate::combat::test_support::TEST_SWORD_STANCE`]).
    fn sword() -> Stance {
        crate::combat::test_support::test_player_attacks()
            .stance(nw_content::player_attacks::stance_key(
                crate::combat::test_support::TEST_SWORD_STANCE,
            ))
            .cloned()
            .expect("sword stance")
    }

    /// The damage window of the sword's attack `kind`.
    fn window(kind: AttackKind) -> [u64; 2] {
        match sword().attack(kind).expect("attack").hit {
            AttackHit::Melee { window_ms, .. } => window_ms,
            AttackHit::Projectile { .. } | AttackHit::HitScan { .. } => panic!("a melee attack"),
        }
    }

    /// When a sword light attack's damage window opens after its start.
    fn light_hit_ms() -> u64 {
        window(AttackKind::Light)[0]
    }

    /// The results of the player attacks due at each tick from `from` to `to`.
    fn through(
        world: &mut SimWorld,
        from: u64,
        to: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let mut results = Vec::new();
        let mut now = from;
        loop {
            results.extend(world.player_swings_due(now));
            if now >= to {
                return results;
            }
            now = (now + TICK_MS).min(to);
        }
    }

    /// The results of the sword's light attack started at `start`, through its window.
    fn light_through(
        world: &mut SimWorld,
        start: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let [open, close] = window(AttackKind::Light);
        through(world, start + open, start + close)
    }

    /// A fight after one tick with player 1 [`SWORD_REACH_M`] east of the mob.
    fn close_fight() -> SimWorld {
        let mut world = fight_at(MOB_X + SWORD_REACH_M);
        tick(&mut world, NOW);
        world
    }

    /// The sword attack `kind` of player `number` with `aim`, started at `now`: its results
    /// through its window.
    fn swing(
        world: &mut SimWorld,
        number: u64,
        aim: Aim,
        kind: AttackKind,
        now: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let attack = sword().attack(kind).cloned().expect("attack");
        world.player_swings.push(PlayerSwing {
            attacker: connection(number),
            aim,
            kind,
            attack,
            started_ms: now,
            pressed_ms: now,
            hand: None,
            not_before_ms: now,
            hits: Vec::new(),
            swept_ms: None,
        });
        let [open, close] = window(kind);
        through(world, now + open, now + close)
    }

    fn facing(yaw: f32) -> Aim {
        Aim {
            facing: Some(yaw),
            pitch: Some(0.0),
            lock_target: None,
        }
    }

    /// A fight after one tick, which wakes the mob.
    fn awake_fight() -> SimWorld {
        let mut world = fight();
        tick(&mut world, NOW);
        world
    }

    #[test]
    fn a_tap_is_one_light_attack_that_hits_in_its_damage_window() {
        let mut world = close_fight();
        let refused = world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        assert!(refused.is_empty());
        // The light attack starts at the release (`Attack_Heavy_NotHeld` of the sword's
        // `Attack_Start_Charge`), which is the newest record of the message.
        assert!(
            world.player_swings_due(NOW + light_hit_ms() - 1).is_empty(),
            "still swinging"
        );
        assert_eq!(
            light_through(&mut world, NOW),
            vec![Ok(AttackOutcome::Hit {
                target: mob(),
                amount: SWORD_HIT
            })]
        );
        assert!(
            through(&mut world, NOW, NOW + LONG_AFTER_MS).is_empty(),
            "it hits once"
        );
    }

    #[test]
    fn a_hold_to_the_charge_starts_the_charged_attack_while_the_key_is_held() {
        // Retail newchar capture: the greatsword's charged hit (2071) came 998 ms after the
        // press, before the release at 1181 ms.
        let charged_at = sword().charged_at_ms.expect("charge");
        let hold_records = charged_at.div_ceil(u64::from(INPUT_RECORD_MS));
        let mut world = awake_fight();
        let mut keys = vec![Key::Press];
        keys.extend(std::iter::repeat_n(
            Key::Hold,
            usize::try_from(hold_records).expect("records"),
        ));
        world.attack_input(&connection(1), &message(1, &keys), NOW);
        assert_eq!(world.player_swings[0].kind, AttackKind::Charged);
        // It starts at the charge, after the press of the oldest record.
        let held = hold_records * u64::from(INPUT_RECORD_MS);
        let due = NOW - held + charged_at + window(AttackKind::Charged)[0];
        assert!(world.player_swings_due(due - 1).is_empty());
        assert_eq!(world.player_swings_due(due).len(), 1, "before the release");
        let next = u16::try_from(hold_records).expect("counter") + 2;
        world.attack_input(
            &connection(1),
            &message(next, &[Key::Release]),
            NOW + INTERVAL_MS,
        );
        assert!(
            world.player_swings_due(NOW + LONG_AFTER_MS).is_empty(),
            "the release starts nothing more"
        );
    }

    #[test]
    fn the_hold_picks_the_light_or_the_heavy_attack() {
        // The sword's `Attack_Start_Charge`: a release before 333 ms is the light attack, from
        // 333 ms the heavy one.
        let heavy_from = sword().heavy_from_ms.expect("heavy");
        let records = |hold_ms: u64| hold_ms.div_ceil(u64::from(INPUT_RECORD_MS));
        let kind_of = |hold_records: u64| {
            let mut world = awake_fight();
            let mut keys = vec![Key::Press];
            keys.extend(std::iter::repeat_n(
                Key::Hold,
                usize::try_from(hold_records).expect("records") - 1,
            ));
            keys.push(Key::Release);
            world.attack_input(&connection(1), &message(1, &keys), NOW);
            world.player_swings[0].kind
        };
        assert_eq!(kind_of(records(heavy_from) - 1), AttackKind::Light);
        assert_eq!(kind_of(records(heavy_from)), AttackKind::Heavy);
    }

    #[test]
    fn a_press_attacks_at_its_release_in_a_later_message() {
        let mut world = close_fight();
        let pressed = message(1, &[Key::Press, Key::Hold]);
        world.attack_input(&connection(1), &pressed, NOW);
        assert!(light_through(&mut world, NOW).is_empty());
        let released = message(3, &[Key::Release]);
        world.attack_input(&connection(1), &released, NOW + INTERVAL_MS);
        assert_eq!(light_through(&mut world, NOW + INTERVAL_MS).len(), 1);
    }

    #[test]
    fn a_counter_that_starts_again_after_a_level_load_is_read() {
        /// The client's count before the open-world load (run-9 capture: 11336), then 1.
        const BEFORE_LOAD: u16 = 11_336;
        assert!(counter_is_newer(1, BEFORE_LOAD), "the count starts again");
        assert!(
            !counter_is_newer(BEFORE_LOAD - 3, BEFORE_LOAD),
            "an older copy"
        );
        assert!(counter_is_newer(0, u16::MAX), "the count wraps");
        let mut world = close_fight();
        world.attack_input(&connection(1), &message(BEFORE_LOAD, &[Key::Hold]), NOW);
        let restarted = message(1, &[Key::Press, Key::Release]);
        world.attack_input(&connection(1), &restarted, NOW + INTERVAL_MS);
        assert_eq!(
            light_through(&mut world, NOW + INTERVAL_MS).len(),
            1,
            "the attack after the load swings"
        );
    }

    #[test]
    fn an_older_copy_is_not_read_again() {
        let mut world = close_fight();
        let tap = message(5, &[Key::Press, Key::Release]);
        world.attack_input(&connection(1), &tap, NOW);
        assert_eq!(light_through(&mut world, NOW).len(), 1);
        // The next message repeats records 5 and 6 after the new record 7.
        let mut next = message(7, &[Key::Hold]);
        next.extend(tap);
        world.attack_input(&connection(1), &next, NOW + INTERVAL_MS);
        assert!(light_through(&mut world, NOW + INTERVAL_MS).is_empty());
    }

    #[test]
    fn presses_before_the_attacks_links_start_nothing_and_a_swing_out_of_reach_misses() {
        let mut world = awake_fight();
        join(&mut world, 2, MOB_X + 10.0);
        let records = message(
            1,
            &[
                Key::Press,
                Key::Release,
                Key::Press,
                Key::Release,
                Key::Press,
                Key::Release,
            ],
        );
        assert!(world.attack_input(&connection(2), &records, NOW).is_empty());
        assert_eq!(
            world.player_swings.len(),
            1,
            "one attack: the others came too soon"
        );
        // The first release, four records before the newest, starts it.
        let start = NOW - 4 * u64::from(INPUT_RECORD_MS);
        assert_eq!(world.player_swings[0].started_ms, start);
        let opened = world.player_swings_due(start + light_hit_ms());
        assert!(opened.is_empty(), "it waits through its window: {opened:?}");
        assert_eq!(
            world.player_swings_due(start + window(AttackKind::Light)[1]),
            vec![Ok(AttackOutcome::Missed)],
            "and misses when it closes"
        );
    }

    /// The records of one message: a tap, holds up to `link_ms` after its release, and another
    /// tap.
    fn tap_then_tap(link_ms: u64) -> Vec<InputRecord> {
        let holds = usize::try_from(link_ms.div_ceil(u64::from(INPUT_RECORD_MS))).expect("records");
        let mut keys = vec![Key::Press, Key::Release];
        keys.extend(std::iter::repeat_n(Key::Hold, holds));
        keys.extend([Key::Press, Key::Release]);
        message(1, &keys)
    }

    #[test]
    fn a_second_attack_in_one_message_is_too_fast() {
        let link = sword().light.expect("light").links[1];
        let mut world = awake_fight();
        let records = tap_then_tap(link.from_ms);
        assert_eq!(
            world.attack_input(&connection(1), &records, NOW),
            vec![Err(AttackRefusal::TooFast)],
            "the second light attack of the combo came in the same message"
        );
        assert_eq!(world.player_swings.len(), 1);
    }

    #[test]
    fn a_press_in_a_link_starts_the_next_combo_step_and_after_the_attack_the_first() {
        let sword = sword();
        let light = sword.light.clone().expect("light");
        let buffered = light.links[0];
        let second = sword.combo[buffered.step - 1]
            .light
            .clone()
            .expect("second light");
        let mut world = awake_fight();
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        // A press 2 records into the buffered window, released at once: the second step starts
        // at the window's instant clip and attacks at once (the key is no longer held).
        let pressed_ms = NOW + buffered.from_ms + 2 * u64::from(INPUT_RECORD_MS);
        world.attack_input(
            &connection(1),
            &message(3, &[Key::Press, Key::Release]),
            pressed_ms + u64::from(INPUT_RECORD_MS),
        );
        let swing = world.player_swings.last().expect("second swing");
        assert_eq!(swing.attack.action, second.action);
        assert_eq!(
            swing.started_ms,
            NOW + buffered.starts_at_ms.expect("buffered")
        );
        // A press before the second attack's links starts nothing...
        let second_start = swing.started_ms;
        world.attack_input(
            &connection(1),
            &message(5, &[Key::Press, Key::Release]),
            second_start + u64::from(INPUT_RECORD_MS),
        );
        assert_eq!(world.player_swings.len(), 2);
        // ...and one after it starts the stance's first step at the press.
        let after = second_start + second.length_ms + u64::from(INPUT_RECORD_MS);
        world.attack_input(
            &connection(1),
            &message(7, &[Key::Press, Key::Release]),
            after,
        );
        let third = world.player_swings.last().expect("third swing");
        assert_eq!(third.attack.action, light.action);
        assert_eq!(third.started_ms, after);
    }

    #[test]
    fn a_swing_with_no_mob_in_reach_still_swings_and_misses() {
        let mut world = awake_fight();
        join(&mut world, 2, MOB_X + 10.0);
        assert_eq!(
            swing(&mut world, 2, facing(FACING_WEST), AttackKind::Light, NOW),
            vec![Ok(AttackOutcome::Missed)]
        );
        assert_eq!(
            swing(&mut world, 7, facing(FACING_WEST), AttackKind::Light, NOW),
            vec![Err(AttackRefusal::NotInWorld)]
        );
    }

    #[test]
    fn a_swing_at_nothing_is_still_a_swing() {
        // Run 13: 34 swings at nothing went to the log as refused. The client plays them: its
        // action goes to its own state and the other players' views, attacks cost no stamina,
        // and only the hit is absent.
        /// A light attack sequence the client started in the retail newchar capture (`r1`
        /// 5051:ffff000a), and its states.
        const LIGHT_SEQUENCE: u64 = 5051;
        const LIGHT_STATES: u64 = 0xffff_000a;
        let mut world = awake_fight();
        join(&mut world, 2, MOB_X + 10.0);
        let entity = world.player_by_number(2).expect("player");
        let mut records = message(1, &[Key::Press, Key::Release]);
        let press = records.last_mut().expect("the press, oldest");
        press.row_1 = Some([LIGHT_SEQUENCE, LIGHT_STATES, 0]);
        let press_counter = press.u16_after_mask;
        let _ = world.move_from_input(&connection(2), &records, NOW);
        assert!(world.attack_input(&connection(2), &records, NOW).is_empty());
        let started = world.player_swings.first().expect("a swing").started_ms;
        assert_eq!(
            world.player_swings_due(started + window(AttackKind::Light)[1]),
            vec![Ok(AttackOutcome::Missed)]
        );
        let mut player = world.world.get_mut::<Player>(entity).expect("player");
        let pressed_in = crate::world::members::own_state_counter(press_counter);
        player.slayer.advance(pressed_in, NOW);
        let due = player.slayer.take_due().state.expect("the action goes out");
        assert_eq!(due.sequence, LIGHT_SEQUENCE);
        assert_eq!(due.started, Some(pressed_in));
        assert!(player.stamina.take_change().is_none(), "no stamina spent");
    }

    #[test]
    fn a_swing_turns_to_its_homing_target_and_misses_what_its_shape_does_not_reach() {
        /// 50 degrees off the facing: inside the sword homing's 60 degrees.
        const OFF_DEG: f32 = 50.0;
        let mut world = close_fight();
        assert!(matches!(
            swing(&mut world, 1, facing(FACING_WEST - OFF_DEG.to_radians()), AttackKind::Light, NOW).as_slice(),
            [Ok(AttackOutcome::Hit { target, .. })] if *target == mob()
        ));
        assert_eq!(
            swing(
                &mut world,
                1,
                facing(FACING_EAST),
                AttackKind::Light,
                NOW + LONG_AFTER_MS
            ),
            vec![Ok(AttackOutcome::Missed)],
            "no homing to a mob behind, and the swing is in front"
        );
        // Inside the homing's 3.75 m radius, beyond the blade.
        let mut far = fight_at(MOB_X + 3.0);
        tick(&mut far, NOW);
        assert_eq!(
            swing(&mut far, 1, facing(FACING_WEST), AttackKind::Light, NOW),
            vec![Ok(AttackOutcome::Missed)],
            "the homing turns, the client moves the body: the shape does not reach"
        );
    }

    /// A world after a tick at `NOW` with the mob, at first 1 m west of player 1, lunged 4 m away
    /// behind it on the server, and where it stands now; the client still shows it in front.
    fn lunged_away() -> (SimWorld, Position) {
        const LUNGE_M: f32 = 4.0;
        let mut world = fight_at(MOB_X + 1.0);
        tick(&mut world, NOW);
        let entity = world.by_connection[&connection(1)];
        let from = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let away = Position {
            x: MOB_X - LUNGE_M,
            ..from
        };
        let away = world.mobs.lunge(mob(), away, LUNGE_M).expect("lunged");
        (world, away)
    }

    #[test]
    fn a_swing_finds_a_mob_where_it_stood_in_the_look_back() {
        // Retail confirms a melee hit up to 0.2 s back
        // (`javelin.server-melee-hit-confirmation-max-lookback-time-sec`).
        let (mut world, _) = lunged_away();
        let window = window(AttackKind::Light);
        let start = NOW - window[0];
        assert!(matches!(
            swing(&mut world, 1, facing(FACING_WEST), AttackKind::Light, start).as_slice(),
            [Ok(AttackOutcome::Hit { target, .. })] if *target == mob()
        ));
        // Past the look-back the old place is gone.
        let (mut world, away) = lunged_away();
        let late_ms = NOW + 7 * TICK_MS;
        world
            .mob_trails
            .record(late_ms, [(mob(), away)].into_iter());
        let late = late_ms - window[0];
        assert_eq!(
            swing(&mut world, 1, facing(FACING_WEST), AttackKind::Light, late),
            vec![Ok(AttackOutcome::Missed)]
        );
    }

    /// Two mobs, at `MOB_X` and 2 m east, and player 1 between them; the targets of a light swing
    /// with the aim `aim`.
    fn two_mobs_aside(aim: Aim) -> Vec<u64> {
        let mut world = world_with_spawns(vec![test_spawn(MOB_X), test_spawn(MOB_X + 2.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + 1.0);
        tick(&mut world, NOW);
        swing(&mut world, 1, aim, AttackKind::Light, NOW)
            .into_iter()
            .filter_map(|result| match result {
                Ok(AttackOutcome::Hit { target, .. }) => Some(target),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_swing_hits_every_mob_its_shape_passes() {
        // Two mobs side by side, 0.9 m apart, in front of player 1 (retail: 4 of the 84 melee
        // hits of 2026-09-18 were second targets of one swing).
        let beside = |dy: f32| VitalsSpawn {
            position: [MOB_X, 10.0 + dy, 50.0],
            ..test_spawn(MOB_X)
        };
        let mut world = world_with_spawns(vec![beside(-0.45), beside(0.45)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + SWORD_REACH_M);
        tick(&mut world, NOW);
        let mut hit: Vec<u64> = swing(&mut world, 1, facing(FACING_WEST), AttackKind::Light, NOW)
            .into_iter()
            .filter_map(|result| match result {
                Ok(AttackOutcome::Hit { target, .. }) => Some(target),
                _ => None,
            })
            .collect();
        hit.sort_unstable();
        assert_eq!(
            hit,
            vec![mob_entity(0), mob_entity(1)],
            "the second is no second attack"
        );
    }

    #[test]
    fn the_client_names_a_target_only_by_its_lock_and_the_server_checks_it() {
        // Retail 2026-09-18: around 84 melee hits the client sent nothing but inputs and time
        // syncs; row 3 of the input named the hit's target on the 49 hits during a lock, from
        // 734 to 799 ms before them. A lock turns the swing to its target at any angle...
        let locked = |target: u64| Aim {
            lock_target: Some(target),
            ..facing(FACING_EAST)
        };
        assert_eq!(two_mobs_aside(locked(mob_entity(0))), vec![mob_entity(0)]);
        // ...but its target must be in the homing reach.
        let mut world = world_with_spawns(vec![test_spawn(MOB_X), test_spawn(MOB_X - 6.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + 1.0);
        tick(&mut world, NOW);
        let far_locked = Aim {
            lock_target: Some(mob_entity(1)),
            ..facing(FACING_WEST)
        };
        assert!(
            matches!(
                swing(&mut world, 1, far_locked, AttackKind::Light, NOW).as_slice(),
                [Ok(AttackOutcome::Hit { target, .. })] if *target == mob_entity(0)
            ),
            "the far locked mob is out of reach: the homing takes the near one"
        );
        // Without a yaw, only the lock aims.
        let no_yaw = Aim {
            facing: None,
            pitch: None,
            lock_target: None,
        };
        assert_eq!(two_mobs_aside(no_yaw), Vec::<u64>::new());
    }

    #[test]
    fn a_ghost_mob_in_reach_is_a_target() {
        /// In the sword's reach east of player 1 of [`border`].
        const GHOST_X: f32 = BORDER_X - 1.5 + SWORD_REACH_M;
        let mut world = border();
        world.set_ghosts(&sim_b(), vec![mob_ghost(GHOST_X, MOB_HEALTH, true)], NOW);
        assert_eq!(
            swing(&mut world, 1, facing(FACING_EAST), AttackKind::Light, NOW),
            vec![Ok(AttackOutcome::Missed)],
            "a returning mob is no target"
        );
        world.set_ghosts(&sim_b(), vec![mob_ghost(GHOST_X, MOB_HEALTH, false)], NOW);
        let results = swing(&mut world, 1, facing(FACING_EAST), AttackKind::Light, NOW);
        let [Ok(AttackOutcome::Remote(remote))] = results.as_slice() else {
            panic!("a remote hit");
        };
        assert_eq!(remote.hit.target, mob());
    }

    fn amount(hit: u32) -> f32 {
        f32::from(u16::try_from(hit).expect("small"))
    }

    /// The 2071 of a light sword hit on the mob: the config's attack rows name no damage type,
    /// and the config's armor share.
    fn light_hit_dealt(world: &SimWorld) -> DamageDealtMsg {
        let item = DamageItem {
            tag: HIT_TAG,
            amount: amount(SWORD_HIT),
            scale: (1.0 - world.combat.armor_share) as f32,
        };
        DamageDealtMsg::melee(1, mob(), item, false, false)
    }

    #[test]
    fn a_hit_sends_the_attacker_a_2071() {
        let mut world = fight();
        world
            .attack(&connection(1), mob(), LIGHT, NOW)
            .expect("hit");
        assert_eq!(
            messages_to(&world.take_messages(), 1),
            vec![ServerMessage::DamageDealt(light_hit_dealt(&world))]
        );
        assert!(world.take_messages().is_empty(), "taken once");
        let entity = world.by_connection[&connection(1)];
        let mut player = world.world.get_mut::<Player>(entity).expect("player");
        let status = player.combat.take_change(NOW, NOW).expect("in combat");
        assert_eq!(status.in_combat, Some(true), "the hit starts the combat");
    }

    #[test]
    fn a_hit_on_an_absorbing_mob_sends_the_attacker_a_2071_and_a_1924() {
        /// How long the mob absorbs hits, from `NOW`.
        const ABSORB_MS: u64 = 1_000;
        let mut world = fight();
        let mob = world
            .mobs
            .spawn_personal(test_spawn(MOB_X), 1, false, None, None, NOW)
            .expect("rows");
        world.mobs.absorb(mob, NOW + ABSORB_MS);
        world.attack(&connection(1), mob, LIGHT, NOW).expect("hit");
        assert_eq!(
            messages_to(&world.take_messages(), 1),
            vec![
                ServerMessage::DamageDealt(DamageDealtMsg::absorbed(1, mob)),
                ServerMessage::CombatEffect(CombatEffectMsg::DisplayImmuneText {
                    header: FacetHeader::of(nw_messages::facet::component::DAMAGE_RECEIVER, mob,),
                    attacker: nw_messages::facet_entity(1),
                }),
            ]
        );
        assert_eq!(
            world.mobs.health(mob),
            Some(MOB_HEALTH),
            "an absorbed hit does no damage"
        );
    }

    /// The shots and reload time of the test blunderbuss (retail `BlunderbussT2`: 2, 1.46 s).
    const BLUNDERBUSS_SHOTS: u8 = 2;
    const BLUNDERBUSS_RELOAD_MS: u64 = 1_460;
    /// The blunderbuss's `MannequinTag`.
    const BLUNDERBUSS_STANCE: &str = "2H_Ranged,Blunderbuss";

    /// The combat data of `source` with the retail stances.
    fn test_data_of(source: nw_content::combat::CombatSource) -> CombatData {
        CombatData::build(source, &CombatRows::default())
            .expect("data")
            .with_player_attacks(crate::combat::test_support::test_player_attacks())
    }

    /// [`fight_at`] `x` with player 1 wearing the kit sword, whose stats are a spread-shot weapon.
    fn shooter_at(x: f32) -> SimWorld {
        const BLUNDERBUSS: &str = "BlunderbussT2";
        let mut source = crate::combat::test_support::test_source();
        let sword = source.weapons[0].1;
        source.weapons.push((
            BLUNDERBUSS.to_owned(),
            WeaponRow {
                spread_shot: true,
                max_loaded_ammo: BLUNDERBUSS_SHOTS,
                reload_ms: BLUNDERBUSS_RELOAD_MS,
                stance: stance_key(BLUNDERBUSS_STANCE),
                ..sword
            },
        ));
        source
            .item_stats
            .push((gear::tests::SWORD.to_owned(), BLUNDERBUSS.to_owned()));
        let data = test_data_of(source);
        let mut world = fight_at(x);
        world.combat = committed().combat_settings(
            Arc::new(data),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        let entity = world.by_connection[&connection(1)];
        world.world.get_mut::<Player>(entity).expect("player").gear = Gear::starter(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            Some(gear::tests::BACKSTORY),
        );
        world.tick_hands(NOW, SERVER_TIME);
        world
    }

    /// [`fight_at`] `x` with player 1 holding a blunderbuss in main-hand option 1 (the kit sword
    /// with spread-shot stats) and a melee weapon in option 2 (the kit staff with the test
    /// sword's stats).
    fn gun_and_blade_at(x: f32) -> SimWorld {
        const BLUNDERBUSS: &str = "BlunderbussT2";
        let mut source = crate::combat::test_support::test_source();
        let (blade, sword) = source.weapons[0].clone();
        source.weapons.push((
            BLUNDERBUSS.to_owned(),
            WeaponRow {
                spread_shot: true,
                max_loaded_ammo: BLUNDERBUSS_SHOTS,
                reload_ms: BLUNDERBUSS_RELOAD_MS,
                stance: stance_key(BLUNDERBUSS_STANCE),
                ..sword
            },
        ));
        source
            .item_stats
            .push((gear::tests::SWORD.to_owned(), BLUNDERBUSS.to_owned()));
        source
            .item_stats
            .push((gear::tests::STAFF.to_owned(), blade));
        let data = test_data_of(source);
        let mut world = fight_at(x);
        world.combat = committed().combat_settings(
            Arc::new(data),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        let entity = world.by_connection[&connection(1)];
        let mut gear = Gear::starter(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            Some(gear::tests::BACKSTORY),
        )
        .expect("gear");
        let kits = gear::tests::kits();
        assert!(gear.add(kits.item(gear::tests::STAFF).expect("staff"), 1));
        let staff = gear::instance_id(gear::tests::CHARACTER, gear::tests::STAFF);
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_2,
                1,
                staff,
                gear::tests::none_disabled
            ),
            Ok(gear::tests::STAFF.to_owned())
        );
        world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
        world.tick_hands(NOW, SERVER_TIME);
        tick(&mut world, NOW);
        world
    }

    /// When a tap at [`NOW`] made while the server holds the blunderbuss lands: after its hit
    /// time and the wait for a shot request.
    fn lands_at() -> u64 {
        NOW + light_hit_ms().max(SHOT_PAIR_MS)
    }

    fn drawn(world: &SimWorld) -> Option<u32> {
        let entity = world.by_connection[&connection(1)];
        world.world.get::<Player>(entity)?.gear.as_ref()?.drawn()
    }

    #[test]
    fn a_tap_with_every_weapon_sheathed_draws_the_weapon_and_the_client_is_told() {
        let mut world = caster_at(
            MOB_X + SWORD_REACH_M,
            crate::combat::test_support::TEST_SWORD_STANCE,
        );
        let entity = world.by_connection[&connection(1)];
        let gear_change = |world: &mut SimWorld| {
            world
                .world
                .get_mut::<Player>(entity)
                .and_then(|mut player| player.gear.as_mut()?.take_change())
        };
        gear_change(&mut world);
        assert_eq!(drawn(&world), None, "every weapon sheathed at login");
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_1));
        let hands = gear_change(&mut world)
            .and_then(|states| states.paperdoll.public)
            .and_then(|public| public.attached);
        assert_eq!(
            hands,
            Some(SlotBits::of([slot::MAIN_HAND_OPTION_1])),
            "the client is told the weapon is in the hand"
        );
    }

    #[test]
    fn a_melee_attack_with_no_shot_takes_the_melee_weapon_in_hand() {
        let mut world = gun_and_blade_at(MOB_X + SWORD_REACH_M);
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        let early = world.player_swings_due(NOW + SHOT_PAIR_MS - 1);
        assert!(early.is_empty(), "it waits for a shot request: {early:?}");
        let landed = through(&mut world, lands_at(), NOW + window(AttackKind::Light)[1]);
        assert!(
            matches!(landed.as_slice(), [Ok(AttackOutcome::Hit { target, .. })] if *target == mob()),
            "{landed:?}"
        );
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_2));
    }

    #[test]
    fn an_attack_with_a_shot_is_the_shot() {
        let mut world = gun_and_blade_at(MOB_X + 2.0);
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        world.spreadshot(&connection(1), &shot(2), NOW + INPUT_RECORD_MS as u64);
        assert!(world.player_swings_due(lands_at()).is_empty());
        assert_ne!(drawn(&world), Some(slot::MAIN_HAND_OPTION_2));
    }

    #[test]
    fn a_shot_before_the_attack_input_is_the_shot_and_nothing_switches() {
        let mut world = gun_and_blade_at(MOB_X + 2.0);
        let entity = world.by_connection[&connection(1)];
        let gear_change = |world: &mut SimWorld| {
            world
                .world
                .get_mut::<Player>(entity)
                .and_then(|mut player| player.gear.as_mut()?.take_change())
        };
        gear_change(&mut world);
        // The client sends the shot at the press; the release comes in a later input.
        world.spreadshot(&connection(1), &shot(2), NOW);
        let released_ms = NOW + SHOT_PAIR_MS;
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            released_ms,
        );
        assert!(
            world
                .player_swings_due(released_ms + light_hit_ms() + SHOT_PAIR_MS)
                .is_empty()
        );
        assert_ne!(drawn(&world), Some(slot::MAIN_HAND_OPTION_2));
        // The shot's ammo change goes out; then a swing with no shot follows the client's hand.
        gear_change(&mut world);
        world.attack_input(
            &connection(1),
            &message(3, &[Key::Press, Key::Release]),
            released_ms + LONG_AFTER_MS,
        );
        world.player_swings_due(released_ms + LONG_AFTER_MS + light_hit_ms() + SHOT_PAIR_MS);
        assert_eq!(
            drawn(&world),
            Some(slot::MAIN_HAND_OPTION_2),
            "a swing with no shot"
        );
        assert_eq!(
            gear_change(&mut world),
            None,
            "the client is not told to switch"
        );
    }

    #[test]
    fn an_attack_with_the_gun_empty_reloads_and_swaps_nothing() {
        let mut world = gun_and_blade_at(MOB_X + 2.0);
        for (frame, shot_at) in [(0, NOW), (SHOT_RECORDS, NOW + SHOT_GAP_MS)] {
            world.spreadshot(&connection(1), &shot_at_frame(2, frame), shot_at);
        }
        let empty_at = NOW + 2 * SHOT_GAP_MS;
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            empty_at,
        );
        assert!(
            world.player_swings_due(empty_at + LONG_AFTER_MS).is_empty(),
            "no swing"
        );
        assert_ne!(
            drawn(&world),
            Some(slot::MAIN_HAND_OPTION_2),
            "the blunderbuss stays in hand"
        );
    }

    #[test]
    fn a_shot_takes_the_spread_shot_weapon_in_hand() {
        let mut world = gun_and_blade_at(MOB_X + 2.0);
        world.attack_input(
            &connection(1),
            &message(1, &[Key::Press, Key::Release]),
            NOW,
        );
        world.player_swings_due(lands_at());
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_2), "the blade");
        let later = lands_at() + SHOT_GAP_MS;
        let results = world.spreadshot(&connection(1), &shot(2), later);
        assert!(
            matches!(results.as_slice(), [Ok(AttackOutcome::Hit { .. })]),
            "{results:?}"
        );
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_1));
    }

    /// [`fight_at`] `x` with player 1 holding a weapon of the stance `tags` (the kit sword with
    /// that stance).
    fn caster_at(x: f32, tags: &str) -> SimWorld {
        const CASTER: &str = "CasterT2";
        let mut source = crate::combat::test_support::test_source();
        let sword = source.weapons[0].1;
        source.weapons.push((
            CASTER.to_owned(),
            WeaponRow {
                stance: stance_key(tags),
                ..sword
            },
        ));
        source
            .item_stats
            .push((gear::tests::SWORD.to_owned(), CASTER.to_owned()));
        let data = test_data_of(source);
        let mut world = fight_at(x);
        world.combat = committed().combat_settings(
            Arc::new(data),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        let entity = world.by_connection[&connection(1)];
        world.world.get_mut::<Player>(entity).expect("player").gear = Gear::starter(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            Some(gear::tests::BACKSTORY),
        );
        tick(&mut world, NOW);
        world
    }

    #[test]
    fn ranged_and_magic_attacks_fly_to_a_mob_far_away() {
        /// Out of every melee reach, inside every projectile's flight.
        const AWAY_M: f32 = 15.0;
        // The retail stances whose light attack is a projectile (the life staff's is Unknown).
        for tags in [
            "MagicStaff,Fire",
            "MagicGauntlet,VoidGauntlet",
            "MagicGauntlet,Ice",
            "Bow,LongBow",
            "2H_Ranged,Rifle",
        ] {
            let mut world = caster_at(MOB_X + AWAY_M, tags);
            let attack = world
                .combat
                .data
                .stance(stance_key(tags))
                .and_then(|stance| stance.attack(AttackKind::Light))
                .cloned()
                .expect("a light attack");
            let AttackHit::Projectile {
                fire_ms, flight, ..
            } = attack.hit
            else {
                panic!("{tags}: a projectile");
            };
            world.attack_input(
                &connection(1),
                &message(1, &[Key::Press, Key::Release]),
                NOW,
            );
            let fired = NOW + fire_ms.max(window_start(&attack));
            assert!(
                world.player_swings_due(fired).is_empty(),
                "{tags}: the projectile is on its way"
            );
            // The cast flies to the mob's middle on its line.
            let flight_s = flight.time_to(AWAY_M).expect("in flight");
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a flight of a few hundred ms"
            )]
            let flight_ms = (flight_s * 1_000.0) as u64;
            assert!(
                world.player_swings_due(fired + flight_ms / 2).is_empty(),
                "{tags}: still flying"
            );
            let landed = world.player_swings_due(fired + flight_ms + 1);
            assert!(
                matches!(landed.as_slice(), [Ok(AttackOutcome::Hit { target, .. })] if *target == mob()),
                "{tags}: {landed:?}"
            );
        }
        let mut world = caster_at(MOB_X + AWAY_M, "MagicGauntlet,Ice");
        let mut away = message(1, &[Key::Press, Key::Release]);
        for record in &mut away {
            record.bit_6 = Some([FACING_EAST, 0.0]);
        }
        world.attack_input(&connection(1), &away, NOW + LONG_AFTER_MS);
        assert_eq!(
            world.player_swings_due(NOW + 2 * LONG_AFTER_MS),
            vec![Ok(AttackOutcome::Missed)],
            "a cast away from the mob meets nothing"
        );
    }

    #[test]
    fn a_gun_fires_its_aimed_shot_while_aiming_and_waits_for_its_load() {
        /// The musket's stance and its aimed shot's row (`player_2h_ranged.adb`).
        const MUSKET: &str = "2H_Ranged,Rifle";
        const MUSKET_STATS: &str = "MusketT2";
        const AIMED_ROW: &str = "MusketAttack1";
        /// The aimed row hits twice as hard as the hip fire's in this test.
        const AIMED_COEFFICIENT: f64 = 2.0;
        /// One load, loaded again 1 s after the shot (`MaxLoadedAmmo`, `AutoReloadAmmoSeconds`).
        const LOADS: u8 = 1;
        const RELOAD_MS: u64 = 1_000;
        const AWAY_M: f32 = 15.0;
        /// Right-click held: input slot value 1, bit 62.
        const AIM_VALUE: usize = 1;
        const AIM_BIT: u32 = 62;
        let mut stances: Vec<Stance> =
            serde_json::from_str(crate::combat::test_support::TEST_PLAYER_ATTACKS)
                .expect("player attack table");
        let musket = stances
            .iter_mut()
            .find(|stance| stance.tags == MUSKET)
            .expect("the musket's stance");
        let hip = musket.light.clone().expect("the hip fire");
        musket.aimed = Some(BasicAttack {
            damage_row: AIMED_ROW.to_owned(),
            ..hip.clone()
        });
        let mut source = crate::combat::test_support::test_source();
        let sword = source.weapons[0].1;
        source.weapons.push((
            MUSKET_STATS.to_owned(),
            WeaponRow {
                stance: stance_key(MUSKET),
                max_loaded_ammo: LOADS,
                reload_ms: RELOAD_MS,
                ..sword
            },
        ));
        source
            .item_stats
            .push((gear::tests::SWORD.to_owned(), MUSKET_STATS.to_owned()));
        let hip_row = source
            .attacks
            .iter()
            .find(|row| row.damage_id == hip.damage_row)
            .cloned()
            .expect("the hip fire's row");
        source.attacks.push(AttackRow {
            damage_id: AIMED_ROW.to_owned(),
            coefficient: AIMED_COEFFICIENT,
            ..hip_row
        });
        let data = CombatData::build(source, &CombatRows::default())
            .expect("data")
            .with_player_attacks(nw_content::player_attacks::PlayerAttacks::of_stances(
                stances,
            ));
        let mut world = fight_at(MOB_X + AWAY_M);
        world.combat = committed().combat_settings(
            Arc::new(data),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        let entity = world.by_connection[&connection(1)];
        world.world.get_mut::<Player>(entity).expect("player").gear = Gear::starter(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            Some(gear::tests::BACKSTORY),
        );
        tick(&mut world, NOW);
        let amount = |landed: Vec<Result<AttackOutcome, AttackRefusal>>| match landed.as_slice() {
            [Ok(AttackOutcome::Hit { amount, .. })] => *amount,
            other => panic!("one hit: {other:?}"),
        };
        /// Long enough for the musket bolt to reach a mob 15 m away.
        const PROJECTILE_FLIGHT_MS: u64 = 1_000;
        let mut aimed = message(1, &[Key::Press, Key::Release]);
        for record in &mut aimed {
            record.slots.values[AIM_VALUE] |= 1 << AIM_BIT;
        }
        let shot = |world: &mut SimWorld, fired_ms: u64| {
            let _ = world.player_swings_due(fired_ms);
            amount(world.player_swings_due(fired_ms + PROJECTILE_FLIGHT_MS))
        };
        world.attack_input(&connection(1), &aimed, NOW);
        let aimed_hit = shot(&mut world, NOW + LONG_AFTER_MS);
        world.attack_input(
            &connection(1),
            &message(3, &[Key::Press, Key::Release]),
            NOW + LONG_AFTER_MS,
        );
        assert!(
            world.player_swings_due(NOW + 2 * LONG_AFTER_MS).is_empty(),
            "no load: the client reloads"
        );
        let loaded_ms = NOW + 2 * LONG_AFTER_MS;
        world.tick_hands(loaded_ms, loaded_ms);
        world.attack_input(
            &connection(1),
            &message(5, &[Key::Press, Key::Release]),
            loaded_ms,
        );
        let hip_hit = shot(&mut world, loaded_ms + LONG_AFTER_MS);
        assert!(
            aimed_hit > hip_hit,
            "the aimed row hits harder: {aimed_hit} against {hip_hit}"
        );
    }

    /// When `attack` lands at the earliest, after it starts.
    fn window_start(attack: &BasicAttack) -> u64 {
        match attack.hit {
            AttackHit::Melee { window_ms, .. } => window_ms[0],
            AttackHit::Projectile { fire_ms, .. } | AttackHit::HitScan { fire_ms, .. } => fire_ms,
        }
    }

    /// A shot of `pellets` pellets on [`mob`], whose last pellet missed.
    fn shot(pellets: usize) -> SpreadshotRequestMsg {
        const HIT: [u8; 2] = [0x00, 0x10];
        const MISS: [u8; 2] = [0xff, 0xfe];
        let mut pellets = vec![HIT; pellets];
        if let Some(last) = pellets.last_mut() {
            *last = MISS;
        }
        SpreadshotRequestMsg {
            header: FacetHeader::of(0, 1),
            frame: 0,
            flag: 1,
            seed: 0,
            origin: [MOB_X + 2.0, 0.0, 0.0],
            direction: [0; 6],
            pellets,
            hits: vec![SpreadHit {
                raw_id: [0; RAW_ID_LEN],
                facet_entity: nw_messages::facet_entity(mob()),
                extra: 0,
            }],
            times: vec![0],
        }
    }

    /// The input records between two hip-fire shots: the blunderbuss hip fire's `CanTransition`
    /// marker at 0.76666665 s (`player_blunderbuss.adb`), the shortest retail gap (23 records on
    /// 5 of 56 gaps, 2026-09-18).
    const SHOT_RECORDS: u16 = 23;
    /// [`SHOT_RECORDS`] in ms.
    const SHOT_GAP_MS: u64 = SHOT_RECORDS as u64 * INPUT_RECORD_MS as u64;

    /// [`shot`] in the client's input record `frame`.
    fn shot_at_frame(pellets: usize, frame: u16) -> SpreadshotRequestMsg {
        SpreadshotRequestMsg {
            frame,
            ..shot(pellets)
        }
    }

    #[test]
    fn the_shot_gap_is_the_hip_fire_can_transition_marker_in_records() {
        let stance = crate::combat::test_support::test_player_attacks()
            .stance(stance_key(BLUNDERBUSS_STANCE))
            .cloned()
            .expect("blunderbuss stance");
        let hip_fire = stance.attack(AttackKind::Light).expect("hip fire");
        assert_eq!(hip_fire.can_transition_ms, Some(767));
        assert_eq!(shot_records(hip_fire), u64::from(SHOT_RECORDS));
    }

    #[test]
    fn the_shot_gap_counts_the_client_records_not_the_arrival() {
        let mut world = shooter_at(MOB_X + 2.0);
        let first = world.spreadshot(&connection(1), &shot_at_frame(2, 100), NOW);
        assert!(first.iter().all(Result::is_ok), "{first:?}");
        // Both arrive in one tick: the network held the first back.
        let second = world.spreadshot(&connection(1), &shot_at_frame(2, 100 + SHOT_RECORDS), NOW);
        assert!(
            matches!(second.as_slice(), [Ok(AttackOutcome::Hit { .. })]),
            "{second:?}"
        );
        // The counter wraps.
        let mut world = shooter_at(MOB_X + 2.0);
        world.spreadshot(&connection(1), &shot_at_frame(2, u16::MAX), NOW);
        assert_eq!(
            world
                .spreadshot(
                    &connection(1),
                    &shot_at_frame(2, SHOT_RECORDS - 2),
                    NOW + SHOT_GAP_MS
                )
                .as_slice(),
            [Err(AttackRefusal::TooFast)]
        );
    }

    #[test]
    fn each_pellet_that_hit_is_one_hit_after_the_answer() {
        const SIX: usize = 6;
        const EIGHT: usize = 8;
        let mut world = shooter_at(MOB_X + 2.0);
        let request = shot(SIX);
        let results = world.spreadshot(&connection(1), &request, NOW);
        assert_eq!(results.len(), SIX - 1, "the missed pellet does nothing");
        assert!(results.iter().all(|result| matches!(
            result,
            Ok(AttackOutcome::Hit { target, .. }) if *target == mob()
        )));
        let messages = messages_to(&world.take_messages(), 1);
        // The blunderbuss stance's light attack row (`Blunderbuss_Primary_Attack`).
        let expected = SpreadshotProcessedMsg {
            request,
            damage: lowercase_crc32("Blunderbuss_Primary_Attack"),
            config: SIX_PELLET_CONFIG,
        };
        let ServerMessage::SpreadshotProcessed(answer) = &messages[0] else {
            panic!("the answer comes first: {:?}", messages[0]);
        };
        // The answer does not carry the frame, origin and times, so compare its bytes.
        assert_eq!(answer.encode(), expected.encode());
        // 64 x 1.0: the held weapon's gear score against armor rating 0 lets all of it through.
        const PELLET_HIT: u32 = 64;
        const ALL_THROUGH: f32 = 1.0;
        let pellet =
            DamageDealtMsg::pellet(1, mob(), amount(PELLET_HIT), ALL_THROUGH, false, false);
        assert_eq!(
            messages[1..],
            vec![ServerMessage::DamageDealt(pellet); SIX - 1]
        );
        assert_eq!(
            world
                .spreadshot(
                    &connection(1),
                    &shot_at_frame(EIGHT, SHOT_RECORDS - 1),
                    NOW + SHOT_GAP_MS
                )
                .as_slice(),
            [Err(AttackRefusal::TooFast)]
        );
        world.spreadshot(
            &connection(1),
            &shot_at_frame(EIGHT, SHOT_RECORDS),
            NOW + SHOT_GAP_MS,
        );
        assert!(matches!(
            &messages_to(&world.take_messages(), 1)[0],
            ServerMessage::SpreadshotProcessed(answer) if answer.config == EIGHT_PELLET_CONFIG
        ));
    }

    #[test]
    fn a_shot_without_a_spread_shot_weapon_is_refused() {
        let mut world = fight();
        assert_eq!(
            world.spreadshot(&connection(1), &shot(1), NOW).as_slice(),
            [Err(AttackRefusal::NotWeaponAttack)]
        );
        assert!(messages_to(&world.take_messages(), 1).is_empty());
    }

    #[test]
    fn a_pellet_out_of_range_is_refused() {
        // `BlunderbussPrimaryFire_Spread`: a hit scan of 14 m.
        const SPREAD_RANGE_M: f32 = 14.0;
        let mut world = shooter_at(MOB_X + SPREAD_RANGE_M - 1.0);
        assert!(matches!(
            world.spreadshot(&connection(1), &shot(2), NOW).as_slice(),
            [Ok(AttackOutcome::Hit { .. })]
        ));
        let mut world = shooter_at(MOB_X + SPREAD_RANGE_M + 1.0);
        assert_eq!(
            world.spreadshot(&connection(1), &shot(2), NOW).as_slice(),
            [Err(AttackRefusal::OutOfReach)]
        );
    }

    #[test]
    fn a_remote_hit_sends_the_attacker_a_2071() {
        let mut world = border();
        world.set_ghosts(
            &sim_b(),
            vec![mob_ghost(BORDER_X + 1.0, MOB_HEALTH, false)],
            NOW,
        );
        world
            .attack(&connection(1), mob(), LIGHT, NOW)
            .expect("remote hit");
        assert_eq!(
            messages_to(&world.take_messages(), 1),
            vec![ServerMessage::DamageDealt(light_hit_dealt(&world))]
        );
    }

    /// An input record of a player who holds the block key.
    fn block_held() -> InputRecord {
        InputRecord {
            slots: nw_messages::InputSlots {
                count: 19,
                values: vec![0, 1 << 62],
                bytes: [0; 4],
            },
            ..InputRecord::default()
        }
    }

    /// Ticks `world` until the mob swings at player 1: that tick's output.
    fn until_swing(world: &mut SimWorld, from_ms: u64) -> (u64, crate::world::TickOutput) {
        let mut now = from_ms;
        loop {
            let before = player_stamina(world, 1);
            let output = tick(world, now);
            if !output.mob_hits.is_empty() || player_stamina(world, 1) < before {
                return (now, output);
            }
            now += TICK_MS;
            assert!(now < from_ms + LONG_AFTER_MS, "the mob never swung");
        }
    }

    #[test]
    fn a_blocked_mob_hit_does_no_damage_and_sends_no_2218_in_the_open_world() {
        let mut world = fight();
        assert!(
            world
                .attack_input(&connection(1), &[block_held()], NOW)
                .is_empty()
        );
        let (_, output) = until_swing(&mut world, NOW);
        assert!(output.mob_hits.is_empty(), "a blocked hit did damage");
        assert_eq!(health_of(&mut world, 1).current(), TEST_PLAYER_HEALTH);
        let stamina = player_stamina(&mut world, 1);
        assert!(stamina < 100.0, "the block took stamina: {stamina}");
        assert!(
            messages_to(&output.messages, 1)
                .iter()
                .all(|message| !matches!(message, ServerMessage::MeleeHit(_))),
            "retail sends no 2218 in the open world"
        );
    }

    #[test]
    fn a_blocked_mob_hit_sends_2218_blocked_in_the_tutorial() {
        let mut world = tutorial_fight();
        world.attack_input(&connection(1), &[block_held()], NOW);
        let (_, output) = until_swing(&mut world, NOW);
        assert!(output.mob_hits.is_empty(), "a blocked hit did damage");
        assert!(
            messages_to(&output.messages, 1)
                .contains(&ServerMessage::MeleeHit(MeleeHitMsg::on(1, true)))
        );
    }

    #[test]
    fn the_blocked_hit_that_takes_the_last_stamina_breaks_the_guard() {
        // Retail newchar 422387: the block took the last 24.265 stamina, the hit did half its
        // damage and `isBlockActive` went 0 while the key stayed held.
        const LOW_STAMINA: f32 = 1.0;
        let mut world = fight();
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .stamina
            .blocked_hit(100.0 - LOW_STAMINA);
        world.attack_input(&connection(1), &[block_held()], NOW);
        let (now, output) = until_swing(&mut world, NOW);
        let [AttackOutcome::Hit { amount, .. }] = output.mob_hits.as_slice() else {
            panic!(
                "the broken guard lets half the hit through: {:?}",
                output.mob_hits
            );
        };
        let full = mob_hit(
            &world.mobs.get(mob()).expect("mob").attack,
            world.combat.player_armor_share,
        )
        .amount;
        assert_eq!(*amount, full / BROKEN_GUARD_DAMAGE_DIVISOR);
        assert_eq!(player_stamina(&mut world, 1), 0.0);
        let vitals = |world: &mut SimWorld| *world.world.get::<Vitals>(entity).expect("vitals");
        assert!(
            !vitals(&mut world).blocking(),
            "the block is down while the key is held"
        );
        world.attack_input(&connection(1), &[block_held()], now + TICK_MS);
        assert!(
            !vitals(&mut world).blocking(),
            "holding the key does not raise it again"
        );
        world.attack_input(&connection(1), &[InputRecord::default()], now + 2 * TICK_MS);
        world.attack_input(&connection(1), &[block_held()], now + 3 * TICK_MS);
        assert!(vitals(&mut world).blocking(), "a new press raises it");
    }

    #[test]
    fn a_projectile_passes_a_block_without_can_block_ranged_and_a_sword_halves_it() {
        const BLOCK_STAMINA: f32 = 16.5;
        const AMOUNT: u32 = 40;
        // Build 25810808 swords: `RangedBlockHealthDamageScaling` and
        // `RangedBlockStaminaDamageScaling` 0.5.
        const SWORD_SCALE: f64 = 0.5;
        let blocked = |world: &mut SimWorld, ranged: bool| {
            let entity = world.player_by_number(1).expect("player");
            world.defend(entity, 1, mob(), AMOUNT, (BLOCK_STAMINA, ranged), NOW)
        };
        let mut world = fight();
        world.attack_input(&connection(1), &[block_held()], NOW);
        assert_eq!(blocked(&mut world, false), None, "a swing is blocked");
        let stamina = player_stamina(&mut world, 1);
        assert_eq!(
            blocked(&mut world, true),
            Some(AMOUNT),
            "the test sword cannot block a projectile"
        );
        assert!((player_stamina(&mut world, 1) - stamina).abs() < f32::EPSILON);
        let held = {
            let entity = world.player_by_number(1).expect("player");
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.insert(gear::tests::gear_with_staff());
            gear.main_weapon().expect("a weapon").0.to_owned()
        };
        let mut source = crate::combat::test_support::test_source();
        let (sword_id, sword) = source.weapons[0].clone();
        source.item_stats.push((held, sword_id));
        source.weapons[0].1 = WeaponRow {
            ranged_block: Some(RangedBlock {
                health_scale: SWORD_SCALE,
                stamina_scale: SWORD_SCALE,
            }),
            ..sword
        };
        world.combat = committed().combat_settings(
            Arc::new(test_data_of(source)),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        assert_eq!(blocked(&mut world, true), Some(AMOUNT / 2), "half passes");
        let taken = stamina - player_stamina(&mut world, 1);
        assert!((taken - BLOCK_STAMINA * SWORD_SCALE as f32).abs() < 0.01);
    }

    #[test]
    fn a_mob_hit_from_another_sim_takes_the_block_here() {
        const BLOCK_STAMINA: f32 = 16.5;
        const AMOUNT: u32 = 40;
        let mut world = fight();
        world.attack_input(&connection(1), &[block_held()], NOW);
        assert!(
            world
                .remote_mob_hit(1, mob(), AMOUNT, (BLOCK_STAMINA, false), NOW)
                .is_none(),
            "a blocked hit takes no health"
        );
        assert_eq!(health_of(&mut world, 1).current(), TEST_PLAYER_HEALTH);
        assert!((player_stamina(&mut world, 1) - (100.0 - BLOCK_STAMINA)).abs() < 0.01);
        world.attack_input(&connection(1), &[InputRecord::default()], NOW + TICK_MS);
        assert!(
            world
                .remote_mob_hit(1, mob(), AMOUNT, (BLOCK_STAMINA, false), NOW + TICK_MS)
                .is_some(),
            "an open player takes it"
        );
        assert_eq!(
            health_of(&mut world, 1).current(),
            TEST_PLAYER_HEALTH - AMOUNT
        );
    }

    fn player_stamina(world: &mut SimWorld, number: u64) -> f32 {
        let entity = world.player_by_number(number).expect("player");
        world
            .world
            .get::<Player>(entity)
            .expect("player")
            .stamina
            .current()
    }

    #[test]
    fn a_block_without_stamina_lets_the_hit_through() {
        let mut world = fight();
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .stamina
            .blocked_hit(LETHAL as f32);
        let blocking = InputRecord {
            slots: nw_messages::InputSlots {
                count: 19,
                values: vec![0, 1 << 62],
                bytes: [0; 4],
            },
            ..InputRecord::default()
        };
        world.attack_input(&connection(1), &[blocking], NOW);
        let mut now = NOW;
        while now < NOW + LONG_AFTER_MS {
            let output = tick(&mut world, now);
            let messages = messages_to(&output.messages, 1);
            assert!(
                !messages.contains(&ServerMessage::MeleeHit(MeleeHitMsg::on(1, true))),
                "no block at 0 stamina"
            );
            if !output.mob_hits.is_empty() {
                assert!(health_of(&mut world, 1).current() < TEST_PLAYER_HEALTH);
                return;
            }
            now += TICK_MS;
        }
        panic!("the mob never hit");
    }

    #[test]
    fn the_armor_fit_gives_the_retail_tutorial_shares() {
        const LEVEL_1_ARMOR: f64 = 109.336;
        const KEPT_GS_60: f64 = 0.7011;
        const KEPT_GS_75: f64 = 0.7541;
        assert!((1.0 - armor_share(LEVEL_1_ARMOR, 60) - KEPT_GS_60).abs() < 1e-3);
        assert!((1.0 - armor_share(LEVEL_1_ARMOR, 75) - KEPT_GS_75).abs() < 1e-3);
    }

    #[test]
    fn a_paced_swing_lands_after_its_hit_delay() {
        const DELAY_MS: u64 = 456;
        let mut world = fight();
        let pace = crate::mobs::Pace {
            attack_factor: 1.0,
            swing_gap_ms: (2_500, 4_000),
            speed_mps: 5.5,
            hit_delay_ms: DELAY_MS,
            attack: BOSS_ATTACK,
            gait: nw_sim_core::Gait {
                swing_start_m: 3.0,
                run_beyond_m: 3.0,
                walk_speed_mps: 5.5,
            },
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: crate::mobs::DEFAULT_MOB_CAPSULE,
            reach: None,
            moves: &[],
            phase_two: None,
        };
        let mob = world
            .mobs
            .spawn_personal(test_spawn(MOB_X), 1, false, Some(pace), None, NOW)
            .expect("rows");
        let swing = MobAttackIntent {
            mob,
            target: 1,
            target_at: Position {
                x: MOB_X + 2.0,
                y: 10.0,
                z: 50.0,
            },
            mob_move: None,
            strike: None,
        };
        assert!(
            world
                .swings_landing(vec![swing.clone()], NOW, SERVER_TIME)
                .is_empty()
        );
        assert!(
            world
                .swings_landing(Vec::new(), NOW + DELAY_MS - 1, SERVER_TIME)
                .is_empty()
        );
        assert_eq!(
            world.swings_landing(Vec::new(), NOW + DELAY_MS, SERVER_TIME),
            [(swing, None)]
        );
        assert!(
            world
                .swings_landing(Vec::new(), NOW + DELAY_MS + 1, SERVER_TIME)
                .is_empty(),
            "once"
        );
    }

    /// The strike of the test tree's damage window (`mob_ai::test_support`): it lunges up to
    /// its homing radius and its shape reaches 0.8 m in front of the body.
    fn test_strike() -> crate::mob_ai::Strike {
        use crate::mob_ai::test_support::{TEST_HOMING_RADIUS_M, TEST_MOB_ROW, TEST_MOB_TABLE};
        const SHAPE_REACH_M: f32 = 0.8;
        crate::mob_ai::Strike {
            table: TEST_MOB_TABLE.to_owned(),
            row: TEST_MOB_ROW.to_owned(),
            weapon: None,
            reach: MobReach {
                lunge_max_m: TEST_HOMING_RADIUS_M.parse().expect("radius"),
                lunge_turn_rad: std::f32::consts::PI,
                reach_m: SHAPE_REACH_M,
            },
            ranged: None,
        }
    }

    #[test]
    fn a_swing_at_a_player_out_of_reach_misses() {
        let mut world = fight();
        join(&mut world, 2, MOB_X + 10.0);
        let swing = |target, x| MobAttackIntent {
            mob: mob(),
            target,
            target_at: Position {
                x,
                y: 10.0,
                z: 50.0,
            },
            mob_move: None,
            strike: Some(test_strike()),
        };
        assert!(
            world
                .mob_hits(vec![(swing(2, MOB_X + 10.0), None)], NOW)
                .is_empty(),
            "10 m away"
        );
        assert!(messages_to(&world.take_messages(), 2).is_empty());
        assert_eq!(
            world
                .mob_hits(vec![(swing(1, MOB_X + 2.0), None)], NOW)
                .len(),
            1,
            "2 m away"
        );
    }

    #[test]
    fn a_mob_with_a_reach_lunges_to_the_player_and_hits() {
        const REACH: crate::mobs::MobReach = crate::mobs::MobReach {
            lunge_max_m: 2.0,
            lunge_turn_rad: std::f32::consts::FRAC_PI_4,
            reach_m: 1.4,
        };
        let pace = crate::mobs::Pace {
            attack_factor: 1.0,
            swing_gap_ms: (2_500, 4_000),
            speed_mps: 5.5,
            hit_delay_ms: 0,
            attack: BOSS_ATTACK,
            gait: nw_sim_core::Gait {
                swing_start_m: 3.0,
                run_beyond_m: 3.0,
                walk_speed_mps: 5.5,
            },
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: crate::mobs::DEFAULT_MOB_CAPSULE,
            reach: Some(REACH),
            moves: &[],
            phase_two: None,
        };
        // Player 1 stands 3 m east of the paced mob: out of its 1.4 m reach until it lunges.
        let mut world = fight_at(MOB_X + 3.0);
        let mob = world
            .mobs
            .spawn_personal(test_spawn(MOB_X), 1, false, Some(pace), None, NOW)
            .expect("rows");
        let at = |x| Position {
            x,
            y: 10.0,
            z: 50.0,
        };
        let swing = |aimed_x| MobAttackIntent {
            mob,
            target: 1,
            target_at: at(aimed_x),
            mob_move: None,
            strike: None,
        };
        assert_eq!(
            world.mob_hits(vec![(swing(MOB_X + 3.0), None)], NOW).len(),
            1
        );
        // It lunges until a gap is left between the bodies: 0.5 + 0.4 + the gap.
        let stop_m = crate::mobs::DEFAULT_MOB_CAPSULE.radius_m
            + crate::world::hit_shape::PLAYER_CAPSULE.radius_m
            + LUNGE_GAP_M;
        let lunged = world.mobs.get_mut(mob).expect("mob").position;
        assert!((at(MOB_X + 3.0).horizontal_distance(lunged) - stop_m).abs() < 1e-3);
        // Already there, it stays: it never steps into the player.
        assert_eq!(
            world.mob_hits(vec![(swing(MOB_X + 3.0), None)], NOW).len(),
            1
        );
        let again = world.mobs.get_mut(mob).expect("mob").position;
        assert!((at(MOB_X + 3.0).horizontal_distance(again) - stop_m).abs() < 1e-3);
        // Aimed west of itself, the player east is beyond its lunge turn and behind its wedge.
        assert!(
            world
                .mob_hits(vec![(swing(MOB_X - 10.0), None)], NOW)
                .is_empty()
        );
    }

    #[test]
    fn a_swing_into_a_dodge_roll_misses() {
        let mut world = fight();
        let dodge = InputRecord {
            slots: nw_messages::InputSlots {
                count: 19,
                values: vec![0, 1 << 3],
                bytes: [0; 4],
            },
            ..InputRecord::default()
        };
        let mut now = NOW;
        assert!(
            world
                .attack_input(&connection(1), std::slice::from_ref(&dodge), now)
                .is_empty()
        );
        let [_, invulnerable_until] = sword().dodge_invulnerable_ms.expect("a roll");
        while now < NOW + invulnerable_until {
            let output = tick(&mut world, now);
            assert!(output.mob_hits.is_empty(), "a hit landed in the roll");
            assert!(
                messages_to(&output.messages, 1)
                    .iter()
                    .all(|message| !matches!(message, ServerMessage::MeleeHit(_)))
            );
            now += TICK_MS;
        }
        // Holding the key does not roll again; only a new press does.
        assert!(world.attack_input(&connection(1), &[dodge], now).is_empty());
        while now < NOW + LONG_AFTER_MS {
            if !tick(&mut world, now).mob_hits.is_empty() {
                return;
            }
            now += TICK_MS;
        }
        panic!("the mob never hit after the roll");
    }

    #[test]
    fn a_mob_hit_sends_the_player_a_3601() {
        let mut world = fight();
        let mut now = NOW;
        loop {
            let output = tick(&mut world, now);
            if let Some(AttackOutcome::Hit {
                target,
                amount: hit,
            }) = output.mob_hits.first()
            {
                assert_eq!(*target, 1);
                let messages = messages_to(&output.messages, 1);
                assert!(
                    messages.iter().any(|message| matches!(
                        message,
                        ServerMessage::OnDamage(damage)
                            if damage.items[0].amount == amount(*hit)
                                && damage.u64_c == nw_messages::facet_entity(mob())
                    )),
                    "{messages:?}"
                );
                assert!(
                    messages
                        .iter()
                        .all(|message| !matches!(message, ServerMessage::MeleeHit(_))),
                    "retail sends no 2218 in the open world: {messages:?}"
                );
                return;
            }
            now += TICK_MS;
            assert!(now < NOW + LONG_AFTER_MS, "the mob never hit");
        }
    }

    #[test]
    fn a_mob_hit_sends_2218_unblocked_in_the_tutorial() {
        let mut world = tutorial_fight();
        let (_, output) = until_swing(&mut world, NOW);
        assert_eq!(output.mob_hits.len(), 1);
        assert!(
            messages_to(&output.messages, 1)
                .contains(&ServerMessage::MeleeHit(MeleeHitMsg::on(1, false)))
        );
    }

    /// The damage table, rows and weapon of the test paces: the plain swing (slash, block mod
    /// 0.2), a hit of half its coefficient, and a hit with a half fire affix.
    const BOSS_TABLE: &str = "BossDamageTable";
    const BOSS_WEAPON: &str = "BossSword";
    const BOSS_ATTACK: crate::mobs::PaceAttack = crate::mobs::PaceAttack {
        table: BOSS_TABLE,
        row: "Boss_Swing",
        weapon: BOSS_WEAPON,
    };
    /// `DamageTypeData`: Slash 4, Fire 9.
    const SLASH: u8 = 4;
    const FIRE: u8 = 9;

    /// [`fight`] with the rows of [`BOSS_ATTACK`]: its weapon's `BlockStaminaDamage`
    /// `block_damage` and the player's sword's `BlockStability` `stability`.
    fn boss_rows_fight(block_damage: f64, stability: f64) -> SimWorld {
        let mut source = crate::combat::test_support::test_source();
        let sword = source.weapons[0].1;
        source.weapons[0].1 = WeaponRow {
            block_stability: stability,
            ..sword
        };
        source.weapons.push((
            BOSS_WEAPON.to_owned(),
            WeaponRow {
                block_stamina_damage: block_damage,
                ..sword
            },
        ));
        source
            .item_stats
            .push((BOSS_WEAPON.to_owned(), BOSS_WEAPON.to_owned()));
        for (row, coefficient, affix) in [
            ("Boss_Swing", 1.0, None),
            ("Boss_Half", 0.5, None),
            (
                "Boss_Fire",
                1.0,
                Some(nw_content::combat::AffixDamage {
                    share: 0.5,
                    damage_type: FIRE,
                }),
            ),
        ] {
            source.attacks.push(nw_content::combat::AttackRow {
                damage_id: row.to_owned(),
                coefficient,
                can_crit: false,
                threat: 0,
                damage_type: Some(SLASH),
                table: BOSS_TABLE.to_owned(),
                block_stamina_mod: 0.2,
                affix,
            });
        }
        let mut world = fight();
        world.combat = committed().combat_settings(
            Arc::new(test_data_of(source)),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        );
        world
    }

    /// A fight with a boss beside player 1: one move of two hits, the second of half the
    /// coefficient, and a second phase with double damage whose move's row has a fire half.
    fn boss_fight() -> (SimWorld, u64, MobAttackIntent) {
        const HITS: [MoveHit; 2] = [
            MoveHit {
                at_ms: 300,
                row: "Boss_Swing",
            },
            MoveHit {
                at_ms: 900,
                row: "Boss_Half",
            },
        ];
        const MOVE: crate::mobs::MobMove = crate::mobs::MobMove {
            fragment: crate::mobs::MoveFragment {
                fragment: 53,
                action: 19,
                tail: 0x8e,
            },
            length_ms: 2_800,
            hits: &HITS,
        };
        const FIRE_HITS: [MoveHit; 1] = [MoveHit {
            at_ms: 300,
            row: "Boss_Fire",
        }];
        const FIRE_MOVE: crate::mobs::MobMove = crate::mobs::MobMove {
            hits: &FIRE_HITS,
            ..MOVE
        };
        const MOVES: [crate::mobs::MobMove; 1] = [MOVE];
        const FIRE_MOVES: [crate::mobs::MobMove; 1] = [FIRE_MOVE];
        let pace = crate::mobs::Pace {
            attack_factor: 1.0,
            swing_gap_ms: (1_000, 1_000),
            speed_mps: 5.5,
            hit_delay_ms: 0,
            attack: BOSS_ATTACK,
            gait: nw_sim_core::Gait {
                swing_start_m: 3.0,
                run_beyond_m: 3.0,
                walk_speed_mps: 5.5,
            },
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: crate::mobs::DEFAULT_MOB_CAPSULE,
            reach: None,
            moves: &MOVES,
            phase_two: Some(crate::mobs::PhaseTwo {
                moves: &FIRE_MOVES,
                damage_mult: 2.0,
            }),
        };
        let mut world = boss_rows_fight(0.0, 0.0);
        let boss = world
            .mobs
            .spawn_personal(test_spawn(MOB_X), 1, false, Some(pace), None, NOW)
            .expect("rows");
        let swing = MobAttackIntent {
            mob: boss,
            target: 1,
            target_at: Position {
                x: MOB_X + 2.0,
                y: 10.0,
                z: 50.0,
            },
            mob_move: Some(MOVE),
            strike: None,
        };
        (world, boss, swing)
    }

    #[test]
    fn each_hit_of_a_move_lands_at_its_time() {
        let (mut world, _, swing) = boss_fight();
        assert!(
            world
                .swings_landing(vec![swing], NOW, SERVER_TIME)
                .is_empty()
        );
        let first = world.swings_landing(Vec::new(), NOW + 300, SERVER_TIME);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1.map(|hit| hit.at_ms), Some(300));
        assert!(
            world
                .swings_landing(Vec::new(), NOW + 899, SERVER_TIME)
                .is_empty()
        );
        let second = world.swings_landing(Vec::new(), NOW + 900, SERVER_TIME);
        assert_eq!(second[0].1.map(|hit| hit.at_ms), Some(900));
    }

    #[test]
    fn a_second_phase_hit_is_larger_and_half_fire() {
        let (mut world, boss, swing) = boss_fight();
        let hit = swing.mob_move.expect("move").hits[0];
        let half_hit = swing.mob_move.expect("move").hits[1];
        let halved = world.mob_hits(vec![(swing.clone(), Some(half_hit))], NOW);
        world.take_messages();
        let first_phase = world.mob_hits(vec![(swing.clone(), Some(hit))], NOW);
        let [AttackOutcome::Hit { amount: base, .. }] = first_phase.as_slice() else {
            panic!("a hit: {first_phase:?}");
        };
        let damage_items = |world: &mut SimWorld| -> Vec<(u8, f32)> {
            messages_to(&world.take_messages(), 1)
                .into_iter()
                .find_map(|message| match message {
                    ServerMessage::OnDamage(damage) => Some(
                        damage
                            .items
                            .iter()
                            .map(|item| (item.tag, item.amount))
                            .collect(),
                    ),
                    _ => None,
                })
                .expect("a 3601")
        };
        assert_eq!(damage_items(&mut world), vec![(SLASH, amount(*base))]);
        assert!(
            matches!(halved.as_slice(), [AttackOutcome::Hit { amount, .. }] if 2 * amount == *base),
            "the coefficient over the attack row's: {halved:?}"
        );
        world.mobs.enter_phase_two(boss);
        let fire_hit = MoveHit {
            at_ms: 300,
            row: "Boss_Fire",
        };
        let second_phase = world.mob_hits(vec![(swing, Some(fire_hit))], NOW);
        let [
            AttackOutcome::Hit {
                amount: doubled, ..
            },
        ] = second_phase.as_slice()
        else {
            panic!("a hit: {second_phase:?}");
        };
        assert_eq!(*doubled, 2 * base);
        let half = amount(*doubled) / 2.0;
        assert_eq!(damage_items(&mut world), vec![(SLASH, half), (FIRE, half)]);
    }

    #[test]
    fn a_blocked_hit_takes_the_attackers_weapon_and_row_stamina() {
        // Retail tutorial: the legionnaire's `Legionnaire_LightStab` (block mod 0.2) with
        // `Legion_1HSpear` (`BlockStaminaDamage` 100) on a warhammer of `BlockStability` 27 took
        // 14.6 stamina.
        const RETAIL_LOSS: f32 = 14.6;
        let mut world = boss_rows_fight(100.0, 27.0);
        let pace = crate::mobs::Pace {
            attack_factor: 1.0,
            swing_gap_ms: (1_000, 1_000),
            speed_mps: 5.5,
            hit_delay_ms: 0,
            attack: BOSS_ATTACK,
            gait: nw_sim_core::Gait {
                swing_start_m: 3.0,
                run_beyond_m: 3.0,
                walk_speed_mps: 5.5,
            },
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: crate::mobs::DEFAULT_MOB_CAPSULE,
            reach: None,
            moves: &[],
            phase_two: None,
        };
        let mob = world
            .mobs
            .spawn_personal(test_spawn(MOB_X), 1, false, Some(pace), None, NOW)
            .expect("rows");
        let blocking = InputRecord {
            slots: nw_messages::InputSlots {
                count: 19,
                values: vec![0, 1 << 62],
                bytes: [0; 4],
            },
            ..InputRecord::default()
        };
        world.attack_input(&connection(1), &[blocking], NOW);
        let swing = MobAttackIntent {
            mob,
            target: 1,
            target_at: Position {
                x: MOB_X + 2.0,
                y: 10.0,
                z: 50.0,
            },
            mob_move: None,
            strike: None,
        };
        assert!(
            world.mob_hits(vec![(swing, None)], NOW).is_empty(),
            "blocked"
        );
        let loss = 100.0 - player_stamina(&mut world, 1);
        assert!((loss - RETAIL_LOSS).abs() < 1e-3, "{loss}");
    }

    #[test]
    fn spawn_protection_stops_mobs_until_the_client_spawns() {
        let hit_on_one = |outcome: &AttackOutcome| {
            matches!(outcome,
                AttackOutcome::Hit { target, .. } | AttackOutcome::Killed { target } if *target == 1)
        };
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        // A fresh login (not a handoff): spawn-protected until its client spawns.
        world
            .admit(&admit_at(1, MOB_X + 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let mut now = NOW;
        while now < NOW + LONG_AFTER_MS {
            let output = tick(&mut world, now);
            assert!(
                !output.mob_hits.iter().any(hit_on_one),
                "a mob hit the player before it spawned",
            );
            now += TICK_MS;
        }
        assert_eq!(
            health_of(&mut world, 1).current(),
            TEST_PLAYER_HEALTH,
            "the protected player kept full health",
        );
        // The spawn (the client's first time sync request after the card) ends the
        // protection, so the same mob can now hit it. A move would not: the client
        // sends movement before the level has loaded (live 2026-09-22).
        world.heard_from(&connection(1));
        loop {
            let output = tick(&mut world, now);
            if output.mob_hits.iter().any(hit_on_one) {
                return;
            }
            now += TICK_MS;
            assert!(
                now < NOW + 2 * LONG_AFTER_MS,
                "the mob never hit after the spawn"
            );
        }
    }

    #[test]
    fn a_remote_mob_hit_sends_the_player_a_3601() {
        let mut world = border();
        assert!(world.apply_hit(1, mob(), SWORD_HIT, 0, NOW).is_some());
        let messages = messages_to(&world.take_messages(), 1);
        let [ServerMessage::OnDamage(damage)] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(damage.items[0].amount, amount(SWORD_HIT));
        assert_eq!(
            damage.u64_c,
            nw_messages::facet_entity(mob()),
            "the attacker's facet entity"
        );
        assert_eq!(damage.raw_2, [0x02, 0x00], "a direct hit");
    }

    #[test]
    fn a_hit_ends_the_player_s_road_boost() {
        /// Flat ground that is road everywhere.
        struct Road;
        impl nw_sim_core::Ground for Road {
            fn height_at(&self, _: f32, _: f32) -> Option<f32> {
                Some(0.0)
            }
            fn road_share(&self, _: f32, _: f32) -> f32 {
                1.0
            }
        }
        /// Records of 1/30 s past the road boost's sprint delay (3 s).
        const SPRINT_RECORDS: usize = 95;
        const FRAME_S: f32 = 1.0 / 30.0;
        const SEA_M: f32 = -100.0;
        let mut body = nw_sim_core::TrackedBody::new(Position {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        });
        let sprint = nw_sim_core::TrackInput {
            character: nw_sim_core::CharacterInput {
                intent: nw_sim_core::Intent {
                    forward: 1,
                    ..nw_sim_core::Intent::default()
                },
                yaw: 0.0,
                dt_s: FRAME_S,
            },
            view: None,
            boom_key: false,
            action_started: false,
        };
        for _ in 0..SPRINT_RECORDS {
            body.record(sprint, &Road, SEA_M);
        }
        assert!(body.has_road_boost());
        let mut world = border();
        let entity = world.player_by_number(1).expect("player 1");
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .tracked = body;
        assert!(world.apply_hit(1, mob(), SWORD_HIT, 0, NOW).is_some());
        let player = world.world.get::<Player>(entity).expect("player");
        assert!(!player.tracked.has_road_boost());
    }

    #[test]
    fn a_death_sends_a_4299() {
        let mut world = fight();
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        let messages = messages_to(&world.take_messages(), 1);
        assert_eq!(
            messages.last(),
            Some(&ServerMessage::DeathRecap(DeathRecapMsg::of(1)))
        );
        assert!(matches!(messages[0], ServerMessage::OnDamage(_)));
    }

    #[test]
    fn a_death_puts_the_drawn_weapon_away() {
        let mut world = caster_at(
            MOB_X + SWORD_REACH_M,
            crate::combat::test_support::TEST_SWORD_STANCE,
        );
        let entity = world.by_connection[&connection(1)];
        let gear = |world: &mut SimWorld| {
            world
                .world
                .get_mut::<Player>(entity)
                .and_then(|mut player| player.gear.as_mut().map(|gear| gear.draw_to_attack(1)))
        };
        assert_eq!(gear(&mut world), Some(true), "drawn");
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        assert_eq!(drawn(&world), None, "put away at the death");
    }

    /// An input message of one record whose sheathe key is pressed (the Azoth catalog input 428:
    /// slot value 6, bit 44).
    fn sheathe_message() -> Vec<InputRecord> {
        const SHEATHE_VALUE: usize = 6;
        const SHEATHE_BIT: u32 = 44;
        let mut records = message(1, &[Key::Hold]);
        records[0].slots.values[SHEATHE_VALUE] = 1 << SHEATHE_BIT;
        records
    }

    #[test]
    fn the_sheathe_key_draws_the_selected_weapon() {
        let mut world = caster_at(
            MOB_X + SWORD_REACH_M,
            crate::combat::test_support::TEST_SWORD_STANCE,
        );
        world.attack_input(&connection(1), &sheathe_message(), NOW);
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_1));
    }

    /// A sword fight with the sword drawn: the world, the player and the sword's put-away time.
    fn drawn_sword() -> (SimWorld, Entity, u64) {
        /// `Sheathe_UpperBody` `From_1H_Melee_Sword` of `player.adb`: `sheathe_weapon` at 0.4 s.
        const SWORD_SHEATHE_MS: u64 = 400;
        let mut world = caster_at(
            MOB_X + SWORD_REACH_M,
            crate::combat::test_support::TEST_SWORD_STANCE,
        );
        let entity = world.by_connection[&connection(1)];
        let player = world.world.get_mut::<Player>(entity);
        assert!(
            player
                .and_then(|mut player| player.gear.as_mut().map(|gear| gear.draw_to_attack(1)))
                .expect("gear")
        );
        assert_eq!(sword().sheathe_ms, Some(SWORD_SHEATHE_MS));
        (world, entity, SWORD_SHEATHE_MS)
    }

    #[test]
    fn the_sheathe_key_puts_the_drawn_weapon_away_at_the_marker_of_its_stance() {
        let (mut world, _, marker_ms) = drawn_sword();
        world.attack_input(&connection(1), &sheathe_message(), NOW);
        world.tick_hands(NOW + marker_ms - 1, SERVER_TIME);
        assert_eq!(
            drawn(&world),
            Some(slot::MAIN_HAND_OPTION_1),
            "in the hand until the marker"
        );
        world.tick_hands(NOW + marker_ms, SERVER_TIME);
        assert_eq!(drawn(&world), None, "put away at the marker");
    }

    #[test]
    fn deep_water_puts_the_drawn_weapon_away_at_the_marker() {
        /// Above the swim floor: wading, not swimming.
        const WADE_M: f32 = 0.5;
        let (mut world, entity, marker_ms) = drawn_sword();
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let swim = swim_floor(world.settings.ocean_level_m);
        world.sheathe_in_deep_water(
            entity,
            Position {
                z: swim + WADE_M,
                ..at
            },
            NOW,
        );
        world.tick_hands(NOW + marker_ms, SERVER_TIME);
        assert_eq!(drawn(&world), Some(slot::MAIN_HAND_OPTION_1), "not deep");
        let later = NOW + marker_ms;
        world.sheathe_in_deep_water(entity, Position { z: swim, ..at }, later);
        world.tick_hands(later + marker_ms, SERVER_TIME);
        assert_eq!(drawn(&world), None, "put away while swimming");
    }

    /// A fight whose player died at `NOW`.
    fn dead_fight() -> SimWorld {
        let mut world = fight();
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        world.take_messages();
        world
    }

    #[test]
    fn a_request_after_the_wait_respawns() {
        let mut world = dead_fight();
        assert!(
            world
                .request_respawn(&connection(1), None, NOW + MIN_RESPAWN_MS)
                .is_ok()
        );
        assert_eq!(health_of(&mut world, 1).current(), TEST_PLAYER_HEALTH);
    }

    #[test]
    fn a_request_too_soon_is_refused() {
        let mut world = dead_fight();
        assert_eq!(
            world.request_respawn(&connection(1), None, NOW + MIN_RESPAWN_MS - 1),
            Err(RespawnRefusal::TooSoon)
        );
        assert!(health_of(&mut world, 1).is_dead());
    }

    #[test]
    fn a_request_while_alive_is_refused() {
        let mut world = fight();
        assert_eq!(
            world.request_respawn(&connection(1), None, NOW),
            Err(RespawnRefusal::NotDead)
        );
        assert_eq!(
            world.request_respawn(&connection(7), None, NOW),
            Err(RespawnRefusal::NotInWorld)
        );
    }

    #[test]
    fn a_second_request_does_not_respawn_twice() {
        let mut world = dead_fight();
        let later = NOW + MIN_RESPAWN_MS;
        world
            .request_respawn(&connection(1), None, later)
            .expect("respawn");
        world.apply_hit(1, mob(), SWORD_HIT, 0, later);
        assert_eq!(
            world.request_respawn(&connection(1), None, later),
            Err(RespawnRefusal::NotDead)
        );
        assert_eq!(
            health_of(&mut world, 1).current(),
            TEST_PLAYER_HEALTH - SWORD_HIT,
            "no refill"
        );
    }

    fn flag(world: &mut SimWorld, number: u64) {
        let entity = world.player_by_number(number).expect("player");
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .pvp_flagged = true;
    }

    #[test]
    fn a_flagged_player_hurts_another_flagged_player_with_the_rolled_hit() {
        const PLAYER_2: u64 = 2;
        let mut world = fight();
        join(&mut world, PLAYER_2, MOB_X + 3.0);
        assert_eq!(
            world.attack(&connection(1), PLAYER_2, LIGHT, NOW),
            Err(AttackRefusal::UnknownTarget),
            "no PvP without the flag"
        );
        flag(&mut world, 1);
        assert_eq!(
            world.attack(&connection(1), PLAYER_2, LIGHT, NOW),
            Err(AttackRefusal::UnknownTarget),
            "an unflagged target is no target"
        );
        flag(&mut world, PLAYER_2);
        let outcome = world
            .attack(&connection(1), PLAYER_2, LIGHT, NOW)
            .expect("a PvP hit");
        let AttackOutcome::Hit { amount, .. } = outcome else {
            panic!("a hit: {outcome:?}");
        };
        assert!(amount > 1, "the weapon's damage, not one point: {amount}");
        assert_eq!(
            health_of(&mut world, PLAYER_2).current(),
            TEST_PLAYER_HEALTH - amount
        );
        let messages = world.take_messages();
        let to = |who: u64| {
            messages
                .iter()
                .filter(|out| out.connection == connection(who))
                .count()
        };
        assert_eq!(to(1), 1, "the attacker gets its 2071");
        assert_eq!(to(PLAYER_2), 1, "the target gets its 3601");
        assert_eq!(
            world.attack(&connection(1), PLAYER_2, LIGHT, NOW + 1),
            Err(AttackRefusal::TooFast),
            "the attack interval applies"
        );
    }

    #[test]
    fn a_refused_attack_sends_no_hit_message() {
        let mut world = fight();
        join(&mut world, 2, MOB_X + 4.5);
        assert_eq!(
            world.attack(&connection(2), mob(), LIGHT, NOW),
            Err(AttackRefusal::OutOfReach)
        );
        world.apply_hit(mob(), 9, LETHAL, 0, NOW);
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW),
            Err(AttackRefusal::TargetDead)
        );
        assert!(world.take_messages().is_empty());
    }

    #[test]
    fn a_dead_player_cannot_attack() {
        let mut world = fight();
        assert!(matches!(
            world.apply_hit(1, mob(), LETHAL, 0, NOW),
            Some(AttackOutcome::Killed { .. })
        ));
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW),
            Err(AttackRefusal::Dead)
        );
    }

    #[test]
    fn an_unknown_target_is_refused() {
        let mut world = fight();
        assert_eq!(
            world.attack(&connection(1), mob_entity(99), LIGHT, NOW),
            Err(AttackRefusal::UnknownTarget)
        );
        assert_eq!(
            world.attack(&connection(7), mob(), LIGHT, NOW),
            Err(AttackRefusal::NotInWorld)
        );
    }

    #[test]
    fn a_returning_mob_is_refused() {
        // Past its leash the test tree walks home under `ResetAI` (as DistanceLeash).
        const PAST_LEASH_M: f32 = crate::mob_ai::test_support::TEST_LEASH_M + 5.0;
        let mut world = fight();
        world.mobs.get_mut(mob()).expect("mob").position.x += PAST_LEASH_M;
        join(&mut world, 2, MOB_X + PAST_LEASH_M + 1.0);
        tick(&mut world, NOW + TICK_MS);
        assert_eq!(world.mobs.state(mob()), Some(MobState::Return));
        assert_eq!(
            world.attack(&connection(2), mob(), LIGHT, NOW + TICK_MS),
            Err(AttackRefusal::Returning)
        );
    }

    #[test]
    fn an_attack_not_of_the_weapon_is_refused() {
        let mut world = fight();
        assert_eq!(
            world.attack(&connection(1), mob(), "Spoon_Attack", NOW),
            Err(AttackRefusal::NotWeaponAttack)
        );
    }

    #[test]
    fn an_attack_out_of_reach_is_refused() {
        let mut world = fight();
        join(&mut world, 2, MOB_X + 4.5);
        join(&mut world, 3, MOB_X + 3.9);
        assert_eq!(
            world.attack(&connection(2), mob(), LIGHT, NOW),
            Err(AttackRefusal::OutOfReach)
        );
        assert!(
            world.attack(&connection(3), mob(), LIGHT, NOW).is_ok(),
            "reach plus margin"
        );
    }

    #[test]
    fn attacks_faster_than_the_interval_are_refused() {
        let mut world = fight();
        assert!(world.attack(&connection(1), mob(), LIGHT, NOW).is_ok());
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW + INTERVAL_MS - 1),
            Err(AttackRefusal::TooFast)
        );
        assert!(
            world
                .attack(&connection(1), mob(), LIGHT, NOW + INTERVAL_MS)
                .is_ok()
        );
    }

    #[test]
    fn a_player_cannot_attack_a_player() {
        let mut world = fight();
        join(&mut world, 2, MOB_X + 1.0);
        assert_eq!(
            world.attack(&connection(1), 2, LIGHT, NOW),
            Err(AttackRefusal::UnknownTarget)
        );
        assert_eq!(
            world.attack(&connection(1), 1, LIGHT, NOW),
            Err(AttackRefusal::UnknownTarget)
        );
    }

    /// Player `number` hits the mob every interval from `from` until it dies; returns the time
    /// of the killing hit and the number of hits.
    fn kill(world: &mut SimWorld, number: u64, from: u64) -> (u64, u32) {
        let mut now = from;
        for hits in 1..=MOB_HEALTH {
            match world
                .attack(&connection(number), mob(), LIGHT, now)
                .expect("hit")
            {
                AttackOutcome::Killed { target } => {
                    assert_eq!(target, mob());
                    return (now, hits);
                }
                AttackOutcome::Hit { amount, .. } => assert_eq!(amount, SWORD_HIT),
                other => panic!("a hit on a local mob, not {other:?}"),
            }
            now += INTERVAL_MS;
        }
        panic!("the mob never died");
    }

    #[test]
    fn a_player_kills_a_mob_and_it_respawns() {
        let mut world = fight();
        let (killed_at, hits) = kill(&mut world, 1, NOW);
        assert_eq!(hits, MOB_HEALTH.div_ceil(SWORD_HIT));
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, killed_at + INTERVAL_MS),
            Err(AttackRefusal::TargetDead)
        );
        tick(&mut world, killed_at + MOB_RESPAWN_MS - 1);
        assert!(mob_health(&mut world).is_dead());
        tick(&mut world, killed_at + MOB_RESPAWN_MS);
        assert_eq!(mob_health(&mut world).current(), MOB_HEALTH);
    }

    #[test]
    fn hits_on_a_dying_mob_kill_it_once() {
        let mut world = fight();
        join(&mut world, 2, MOB_X - 2.0);
        let mut now = NOW;
        while mob_health(&mut world).current() > SWORD_HIT {
            world
                .attack(&connection(1), mob(), LIGHT, now)
                .expect("hit");
            now += INTERVAL_MS;
        }
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, now),
            Ok(AttackOutcome::Killed { target: mob() })
        );
        assert_eq!(
            world.attack(&connection(2), mob(), LIGHT, now),
            Err(AttackRefusal::TargetDead)
        );
        assert_eq!(
            world.apply_hit(mob(), 2, SWORD_HIT, 0, now),
            None,
            "a late remote hit too"
        );
    }

    #[test]
    fn a_mob_kills_a_player_who_then_respawns_at_a_respawn_point_not_where_it_died() {
        let mut world = fight();
        let mut deaths = Vec::new();
        let mut now = NOW;
        while deaths.is_empty() && now < NOW + 60_000 {
            deaths = tick(&mut world, now).deaths;
            now += TICK_MS;
        }
        assert_eq!(deaths.len(), 1, "killed once");
        assert_eq!(deaths[0].connection, connection(1));
        assert_eq!(deaths[0].position.x, MOB_X + 2.0, "saved where it died");
        let died_at = now - TICK_MS;
        let later = died_at + LONG_AFTER_MS;
        tick(&mut world, later);
        assert!(
            health_of(&mut world, 1).is_dead(),
            "no respawn without a request"
        );
        // The client named no point: the known respawn point nearest the death.
        let at = world
            .request_respawn(&connection(1), None, later)
            .expect("respawn");
        assert_eq!(health_of(&mut world, 1).current(), TEST_PLAYER_HEALTH);
        let entity = world.player_by_number(1).expect("player");
        let body = world.world.get::<Body>(entity).expect("body");
        assert_eq!(body.motion.position(), at);
        let nearest = nearest_respawn_point([MOB_X + 2.0, 0.0, 0.0]);
        assert_eq!([at.x, at.y, at.z], nearest, "not where it died");
        assert!(world.world.get::<Player>(entity).expect("player").moved);
    }

    #[test]
    fn a_respawn_goes_to_the_point_the_client_chose() {
        /// A retail respawn point (FFA_LightningStrikeDeath).
        const POINT: u64 = 14_639_843_741_699_618_923;
        let mut world = dead_fight();
        let at = world
            .request_respawn(&connection(1), Some(POINT), NOW + MIN_RESPAWN_MS)
            .expect("respawn");
        assert_eq!(Some([at.x, at.y, at.z]), respawn_point(POINT));
    }

    #[test]
    fn a_dead_player_cannot_move() {
        let mut world = fight();
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        let next = Position {
            x: MOB_X + 2.5,
            y: 10.0,
            z: 50.0,
        };
        assert_eq!(
            world.move_to(&connection(1), next, NOW + 100),
            Err(MoveError::Dead)
        );
    }

    #[test]
    fn a_dead_player_is_forgotten_by_mobs() {
        let mut world = fight();
        world
            .attack(&connection(1), mob(), LIGHT, NOW)
            .expect("hit");
        tick(&mut world, NOW + TICK_MS);
        assert_eq!(world.mobs.target(mob()), Some(1), "its attacker");
        world.apply_hit(1, mob(), LETHAL, 0, NOW + TICK_MS);
        tick(&mut world, NOW + 2 * TICK_MS);
        assert_eq!(world.mobs.target(mob()), None);
        assert_eq!(world.mobs.state(mob()), Some(MobState::Idle));
    }

    #[test]
    fn a_death_is_saved_at_once() {
        let mut world = fight();
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        let deaths = tick(&mut world, NOW + TICK_MS).deaths;
        assert_eq!(deaths.len(), 1);
        assert_eq!(deaths[0].connection, connection(1));
        assert!(
            tick(&mut world, NOW + 2 * TICK_MS).deaths.is_empty(),
            "once"
        );
    }

    fn sim_b() -> BusId {
        slot("sim-b").id
    }

    /// A mob ghost of sim-b at `x`.
    fn mob_ghost(x: f32, health: u32, returning: bool) -> Ghost {
        Ghost {
            entity: mob(),
            position: Point {
                x,
                y: 10.0,
                z: 50.0,
            },
            mob: Some(MobGhost {
                health,
                max_health: MOB_HEALTH,
                returning,
                vitals_id: 0,
                category_id: 0,
                level: 0,
            }),
            player: None,
        }
    }

    /// A world of sim-a with player 1 just west of the border.
    fn border() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 1.5);
        world
    }

    #[test]
    fn an_attack_on_a_ghost_goes_to_its_owner() {
        let mut world = border();
        world.set_ghosts(
            &sim_b(),
            vec![mob_ghost(BORDER_X + 1.0, MOB_HEALTH, false)],
            NOW,
        );
        let outcome = world
            .attack(&connection(1), mob(), LIGHT, NOW)
            .expect("remote hit");
        let AttackOutcome::Remote(remote) = outcome else {
            panic!("not remote: {outcome:?}");
        };
        assert_eq!(remote.owner, sim_b());
        assert_eq!((remote.hit.target, remote.hit.attacker), (mob(), 1));
        assert_eq!(remote.hit.amount, SWORD_HIT);
        assert_eq!(remote.hit.attacker_sim, slot("sim-a").id);
    }

    #[test]
    fn a_ghost_mob_is_checked_like_a_local_one() {
        let mut world = border();
        world.set_ghosts(
            &sim_b(),
            vec![mob_ghost(BORDER_X + 1.0, MOB_HEALTH, true)],
            NOW,
        );
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW),
            Err(AttackRefusal::Returning)
        );
        world.set_ghosts(&sim_b(), vec![mob_ghost(BORDER_X + 1.0, 0, false)], NOW);
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW),
            Err(AttackRefusal::TargetDead)
        );
        world.set_ghosts(
            &sim_b(),
            vec![mob_ghost(BORDER_X + 9.0, MOB_HEALTH, false)],
            NOW,
        );
        assert_eq!(
            world.attack(&connection(1), mob(), LIGHT, NOW),
            Err(AttackRefusal::OutOfReach)
        );
    }

    #[test]
    fn a_mob_hits_a_ghost_player_through_its_sim() {
        /// The coefficient of the test mob's damage row: a ghost player takes it too.
        const ROW_FACTOR: f64 = 3.0;
        let mut world = world_with_spawns(vec![test_spawn(BORDER_X - 1.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let mut source = crate::combat::test_support::test_source();
        for row in &mut source.attacks {
            if row.damage_id == crate::mob_ai::test_support::TEST_MOB_ROW {
                row.coefficient = ROW_FACTOR;
            }
        }
        world.combat.data = Arc::new(test_data_of(source));
        let player = Ghost {
            entity: 9,
            position: Point {
                x: BORDER_X + 1.0,
                y: 10.0,
                z: 50.0,
            },
            mob: None,
            player: None,
        };
        // The test tree's damage window comes this long into its attack.
        let window_ticks = (crate::mob_ai::test_support::TEST_HIT_S * 1000.0) as u64 / TICK_MS + 2;
        let mut remote = None;
        for tick_number in 0..window_ticks {
            let now = NOW + tick_number * TICK_MS;
            world.set_ghosts(&sim_b(), vec![player], now);
            remote = tick(&mut world, now)
                .mob_hits
                .into_iter()
                .find_map(|outcome| match outcome {
                    AttackOutcome::Remote(remote) => Some(remote),
                    _ => None,
                });
            if remote.is_some() {
                break;
            }
        }
        let remote = remote.expect("the mob hit the ghost player");
        assert_eq!(remote.owner, sim_b());
        assert_eq!((remote.hit.target, remote.hit.attacker), (9, mob()));
        let unscaled = (crate::combat::test_support::TEST_LEVEL_DAMAGE
            * (1.0 - world.combat.player_armor_share))
            .round();
        assert_eq!(
            f64::from(remote.hit.amount),
            (unscaled * ROW_FACTOR).round(),
            "the same hit as on a player of this sim"
        );
    }

    #[test]
    fn a_hit_for_a_mob_of_a_lost_cell_is_dropped() {
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world.set_table(&table(&["sim-b", "sim-a"])).expect("table");
        assert_eq!(world.apply_hit(mob(), 1, SWORD_HIT, 0, NOW), None);
    }

    #[test]
    fn a_hit_on_a_dead_mob_is_dropped() {
        let mut world = fight();
        world.apply_hit(mob(), 1, LETHAL, 0, NOW);
        assert_eq!(world.apply_hit(mob(), 1, SWORD_HIT, 0, NOW), None);
    }

    /// Ground at one height everywhere.
    struct Flat;

    impl nw_sim_core::Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(STRESS_HEIGHT)
        }
    }

    const STRESS_HEIGHT: f32 = 50.0;
    const STRESS_FIGHTS: u32 = 1000;
    /// Fights along one row, this far apart: a mob sees the players of its neighbours too.
    const STRESS_SPACING_M: f32 = 14.0;
    const STRESS_TICKS: u64 = 1800;
    const STRESS_THREADS: usize = 4;
    const STRESS_MOB_RESPAWN_MS: u64 = 5_000;
    const STRESS_VISIBLE: usize = 47;
    const ONE_TICK_MS: f64 = 33.0;
    const P99: f64 = 0.99;

    /// M6-A spec section 12.2: run with
    /// `cargo test --release -p sim fight_stress -- --ignored --nocapture`.
    #[test]
    #[ignore = "release-mode stress test of the M6-A exit"]
    fn fight_stress_keeps_ticks_under_one_tick() {
        let mut combat = test_combat();
        combat.mob_respawn_ms = STRESS_MOB_RESPAWN_MS;
        let x_of = |fight: u32| STRESS_SPACING_M * (1.0 + fight as f32);
        combat.spawns = (0..STRESS_FIGHTS)
            .map(|fight| test_spawn(x_of(fight)))
            .collect();
        let mut world = SimWorld::new(
            WorldSettings {
                tick_threads: STRESS_THREADS,
                ..crate::world::tests::settings(STRESS_VISIBLE, STRESS_FIGHTS * 2)
            },
            crate::login::WorldIds::random(0).expect("ids"),
            std::sync::Arc::new(Flat),
            slot("sim-a"),
            combat,
            std::sync::Arc::default(),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        for fight in 0..STRESS_FIGHTS {
            join(&mut world, u64::from(fight) + 1, x_of(fight) + 2.0);
        }
        let mut next_attack = vec![NOW; STRESS_FIGHTS as usize];
        let mut times: Vec<f64> = Vec::new();
        let (mut hits, mut kills, mut respawns) = (0_u64, 0_u64, 0_u64);
        for tick_number in 0..STRESS_TICKS {
            let now = NOW + tick_number * TICK_MS;
            let started = std::time::Instant::now();
            for fight in 0..STRESS_FIGHTS {
                if next_attack[fight as usize] > now {
                    continue;
                }
                next_attack[fight as usize] = now + INTERVAL_MS;
                let attacker = connection(u64::from(fight) + 1);
                match world.attack(&attacker, mob_entity(fight), LIGHT, now) {
                    Ok(AttackOutcome::Killed { .. }) => kills += 1,
                    Ok(_) => hits += 1,
                    Err(AttackRefusal::Dead) => {
                        respawns += u64::from(world.request_respawn(&attacker, None, now).is_ok());
                    }
                    Err(_) => {}
                }
            }
            world.tick(now, 0).expect("tick");
            world.take_messages();
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        let at = |share: f64| times[((times.len() - 1) as f64 * share) as usize];
        let (p50, p99, max) = (at(0.5), at(P99), times[times.len() - 1]);
        println!(
            "fight stress: {STRESS_FIGHTS} fights, {STRESS_TICKS} ticks, {hits} hits, {kills} kills, {respawns} respawns; tick ms p50 {p50:.2}, p99 {p99:.2}, max {max:.2}"
        );
        assert!(kills > 0, "mobs died and respawned");
        assert!(p99 < ONE_TICK_MS, "tick p99 {p99:.2} ms");
    }

    #[test]
    fn a_dead_player_is_not_handed_off() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X + 50.0);
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        assert!(
            tick(&mut world, NOW).handoffs.is_empty(),
            "it stays until its respawn"
        );
        world
            .request_respawn(&connection(1), None, NOW + MIN_RESPAWN_MS)
            .expect("respawn");
        // The respawn is a server move of three states. The handoff waits until that move
        // has gone out. The nearest known point is still past this sim's margin.
        /// The spawn card can take the first tick, then the move is three states.
        const SERVER_MOVE_TICKS: u64 = 4;
        let respawned = NOW + MIN_RESPAWN_MS;
        assert!(
            tick(&mut world, respawned).handoffs.is_empty(),
            "the server move goes before the handoff"
        );
        let mut handed_off = 0;
        for step in 1..=SERVER_MOVE_TICKS {
            handed_off = tick(&mut world, respawned + step).handoffs.len();
        }
        assert_eq!(handed_off, 1);
    }

    #[test]
    fn a_hit_on_a_frozen_player_is_dropped() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X + 50.0);
        let offer = tick(&mut world, NOW).handoffs[0].offer.clone();
        assert_eq!(
            world.apply_hit(1, mob(), LETHAL, 0, NOW),
            None,
            "the offer has its health"
        );
        assert_eq!(health_of(&mut world, 1).current(), offer.health);
    }

    #[test]
    fn a_dead_player_is_no_ghost() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 50.0);
        assert_eq!(tick(&mut world, NOW).ghosts[0].1.len(), 1);
        world.apply_hit(1, mob(), LETHAL, 0, NOW);
        let ghosts = tick(&mut world, NOW + TICK_MS).ghosts;
        assert!(
            ghosts.iter().all(|(_, entities)| entities.is_empty()),
            "{ghosts:?}"
        );
    }

    #[test]
    fn a_handoff_keeps_the_players_health() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X + 50.0);
        world.apply_hit(1, mob(), SWORD_HIT, 0, NOW);
        let offer = tick(&mut world, NOW).handoffs[0].offer.clone();
        assert_eq!(offer.health, TEST_PLAYER_HEALTH - SWORD_HIT);
        let mut next = world_with_spawns(Vec::new());
        next.take(&offer, NOW, SERVER_TIME).expect("take");
        assert_eq!(
            health_of(&mut next, 1).current(),
            TEST_PLAYER_HEALTH - SWORD_HIT
        );
    }

    #[test]
    fn food_in_the_quick_slot_is_eaten_as_from_the_bag() {
        // Live 2026-10-29: the light ration (`FoodHealthRecoveryT1`, `MannequinTag` `Eat`, no
        // heal) in quick slot 1 gave no buff and stayed. Here flint stands in for the food.
        use nw_content::item_rules::{
            ConsumableRow, EffectRow, ItemRuleRow, ItemRuleSource, ItemRules,
        };
        const FOOD: &str = gear::tests::FLINT;
        const RATIONS: u32 = 2;
        let mut world = fight_at(MOB_X);
        Arc::make_mut(&mut world.content).item_rules = ItemRules::build(ItemRuleSource {
            items: vec![ItemRuleRow {
                item_id: FOOD.to_owned(),
                stats_ref: None,
                consume_on_use: true,
                not_droppable: false,
                nonremovable: false,
            }],
            consumables: vec![ConsumableRow {
                id: FOOD.to_owned(),
                add_status_effects: Some("FoodRecoveryT1".to_owned()),
                potency: Some(1.0),
                duration_override_s: Some(0.0),
                cooldown_id: None,
                cooldown_s: None,
                disabled_in_combat: false,
                use_tag: Some("Eat".to_owned()),
            }],
            effects: vec![EffectRow {
                status_id: "FoodRecoveryT1".to_owned(),
                base_duration_s: Some(1_200.0),
                tick_s: Some(2.0),
                delay_s: None,
                health: Some(10.0),
                health_share: Some(0.0),
                stamina: None,
                remove_on_death: true,
                has_fx: false,
            }],
        });
        let kits = gear::tests::kits();
        let mut gear = Gear::starter(&kits, gear::tests::CHARACTER, Some(gear::tests::BACKSTORY))
            .expect("gear");
        gear.renew(
            kits.item(FOOD).expect("food"),
            RATIONS,
            slot::QUICK_SLOT_1,
            0,
        );
        let entity = world.by_connection[&connection(1)];
        world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
        world.drink(1, NOW, SERVER_TIME);
        let rations: u32 = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(Gear::carried)
            .unwrap_or_default()
            .iter()
            .filter(|stack| stack.item_id == FOOD)
            .map(|stack| stack.quantity)
            .sum();
        assert_eq!(rations, RATIONS - 1, "one ration eaten");
    }

    #[test]
    fn a_potion_of_a_disabled_class_is_not_drunk() {
        const HEAL_EFFECT: &str = "TestHeal";
        const POTIONS: u32 = 2;
        for (disabled, left) in [(false, POTIONS - 1), (true, POTIONS)] {
            let mut source = crate::combat::test_support::test_source();
            source.consumables.push(nw_content::combat::ConsumableRow {
                id: gear::tests::POTION.to_owned(),
                cooldown_s: 1,
                cooldown_id: gear::tests::POTION.to_owned(),
                status_effects: HEAL_EFFECT.to_owned(),
                use_tag: String::new(),
            });
            source.heals.push((HEAL_EFFECT.to_owned(), 1.0, 0.0));
            let mut world = fight_at(MOB_X);
            world.combat = committed().combat_settings(
                Arc::new(test_data_of(source)),
                Arc::from([]),
                crate::combat::tests::test_mob_ai(),
            );
            if disabled {
                Arc::make_mut(&mut world.content)
                    .disabled_items
                    .insert(gear::tests::POTION.to_lowercase());
            }
            let kits = gear::tests::kits();
            let mut gear =
                Gear::starter(&kits, gear::tests::CHARACTER, Some(gear::tests::BACKSTORY))
                    .expect("gear");
            gear.renew(
                kits.item(gear::tests::POTION).expect("potion"),
                POTIONS,
                slot::QUICK_SLOT_1,
                0,
            );
            let entity = world.by_connection[&connection(1)];
            world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
            world.drink(1, NOW, SERVER_TIME);
            let potions: u32 = world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|stack| stack.item_id == gear::tests::POTION)
                .map(|stack| stack.quantity)
                .sum();
            assert_eq!(potions, left, "disabled: {disabled}");
        }
    }
}
