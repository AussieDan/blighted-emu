// SPDX-License-Identifier: AGPL-3.0-or-later
//! The projectiles mobs fire. A `CAGE-RangedAttack` clip of the action a mob plays
//! ([`crate::mob_ai::RangedClip`]) fires the attack its `RangedAttackName` names in the ranged
//! profile of the mob's main-hand weapon: the ranged attack database gives its range and radius,
//! the clip's projectile slice its flight. The mob aims at its target, ahead of it when the clip
//! leads (`AILeadTarget`, by at most `AILeadTargetMaxAngle`). The shot lands where it aimed once
//! it flew there, and hits the target when the target's body is then within the projectile's
//! radius of that point.
//!
//! Observed: retail Janszoon threw his anchor (`Officer_AnchorToss`, newchar-retail 1246273 ms,
//! the player 8.7 m away) and the player lost 40 health 1.53 s later, his table's `Anchor_Toss`
//! row (0.7 of the base 58 his axe hits show: `Damned_2H_GreatAxe_Melee_Light` 0.5 for 29,
//! `_Heavy` 1.2 for 69); the clip fires 1.27 s into the throw and the anchor slice flies at
//! 30 m/s. Inferred: the landing test on the body (the client's projectile collision is not
//! read), and a flight time of the slice's speed along the ground (its arc is not drawn).

use nw_sim_core::Position;

use super::Body;
use super::SimWorld;
use super::attack::MobHitStyle;
use super::hit_shape::{PLAYER_CAPSULE, turn};
use super::mob_timeline::yaw_toward;
use crate::combat::AttackOutcome;
use crate::mob_ai::RangedClip;
use crate::mobs::MobAttackIntent;

/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;

/// A projectile a mob fired, on its way to where it aimed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MobShot {
    mob: u64,
    target: u64,
    /// The hit it lands.
    amount: u32,
    style: MobHitStyle,
    /// Where it lands.
    aim: Position,
    /// `m_projectileRadius`.
    radius_m: f32,
    /// When it lands.
    land_ms: u64,
}

/// The point a shot from `from` aims at for a target at `target` that moves at `velocity` (m/s
/// on the ground) and that the shot reaches after `flight_s`: where the target will be, turned
/// back toward the target by what passes `max_turn_rad`.
fn lead(
    from: Position,
    target: Position,
    velocity: [f32; 2],
    flight_s: f32,
    max_turn_rad: f32,
) -> Position {
    let ahead = Position {
        x: target.x + velocity[0] * flight_s,
        y: target.y + velocity[1] * flight_s,
        z: target.z,
    };
    let direct = yaw_toward(from, target);
    let led = turn(direct, yaw_toward(from, ahead));
    if led.abs() <= max_turn_rad {
        return ahead;
    }
    let yaw = direct + led.clamp(-max_turn_rad, max_turn_rad);
    let distance_m = from.horizontal_distance(ahead);
    // Yaw 0 faces +y; yaw grows toward -x (see `yaw_toward`).
    Position {
        x: from.x - yaw.sin() * distance_m,
        y: from.y + yaw.cos() * distance_m,
        z: target.z,
    }
}

impl SimWorld {
    /// The mob of `intent` at `from` fires the projectile of `clip` at its target at `now_ms`,
    /// to land `amount` in `style` ([`SimWorld::mob_shots_due`]). A shot whose attack, profile or
    /// flight the data does not have is not fired; a target beyond its range is missed.
    pub(super) fn fire_mob_shot(
        &mut self,
        intent: &MobAttackIntent,
        clip: &RangedClip,
        amount: u32,
        style: MobHitStyle,
        from: Position,
        now_ms: u64,
    ) {
        let data = self.mobs.ai().data();
        let weapon = intent
            .strike
            .as_ref()
            .and_then(|strike| strike.weapon.as_deref());
        let attack = weapon
            .and_then(|weapon| self.combat.data.ranged_profile_of_item(weapon))
            .and_then(|profile| data.ranged_attack(profile, &clip.attack))
            .copied();
        let Some(attack) = attack else {
            tracing::warn!(
                mob = intent.mob,
                ?weapon,
                ?clip,
                "mob ranged attack missing"
            );
            return;
        };
        let flight = if attack.hit_scan {
            None
        } else {
            let flight = clip
                .slice
                .as_deref()
                .and_then(|slice| data.projectile_flight(slice));
            if flight.is_none() {
                tracing::warn!(mob = intent.mob, ?clip, "mob projectile flight missing");
                return;
            }
            flight
        };
        let Some((target_at, velocity)) = self.shot_target(intent.target) else {
            return;
        };
        let flight_to =
            |distance_m: f32| flight.map_or(Some(0.0), |flight| flight.time_to(distance_m));
        let aim = match flight_to(from.horizontal_distance(target_at)) {
            Some(flight_s) if clip.lead => {
                lead(from, target_at, velocity, flight_s, clip.lead_max_rad)
            }
            _ => target_at,
        };
        let distance_m = from.horizontal_distance(aim);
        let flight_s = flight_to(distance_m).filter(|_| distance_m <= attack.range_m);
        let Some(flight_s) = flight_s else {
            tracing::debug!(
                mob = intent.mob,
                distance = distance_m,
                "mob shot missed: out of range"
            );
            return;
        };
        // A flight of at most its lifetime, a few seconds.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a flight of at most a few seconds"
        )]
        let flight_ms = (flight_s * MS_PER_SECOND).round() as u64;
        tracing::debug!(
            mob = intent.mob,
            attack = clip.attack,
            distance = distance_m,
            flight_ms,
            "mob shot"
        );
        self.mob_shots.push(MobShot {
            mob: intent.mob,
            target: intent.target,
            amount,
            style: style.fired(),
            aim,
            radius_m: attack.radius_m,
            land_ms: now_ms.saturating_add(flight_ms),
        });
    }

    /// Lands the mob shots that got where they aimed at `now_ms`: each hits its target when the
    /// target's body is within its radius of that point.
    pub(super) fn mob_shots_due(&mut self, now_ms: u64) -> Vec<AttackOutcome> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.mob_shots)
            .into_iter()
            .partition(|shot| now_ms >= shot.land_ms);
        self.mob_shots = later;
        due.into_iter()
            .filter_map(|shot| {
                let (body, _) = self.shot_target(shot.target)?;
                let wide_m = shot.radius_m + PLAYER_CAPSULE.radius_m;
                let level = (shot.aim.z - shot.radius_m..=shot.aim.z + PLAYER_CAPSULE.height_m)
                    .contains(&body.z);
                if body.horizontal_distance(shot.aim) > wide_m || !level {
                    tracing::debug!(
                        mob = shot.mob,
                        aside = body.horizontal_distance(shot.aim),
                        dz = body.z - shot.aim.z,
                        "mob shot missed"
                    );
                    return None;
                }
                self.land_mob_hit(shot.target, shot.mob, shot.amount, shot.style, now_ms)
            })
            .collect()
    }

    /// Where the player `target` is and how it moves (m/s on the ground): a player of this sim
    /// by its body, a player ghost by its last place, standing.
    fn shot_target(&mut self, target: u64) -> Option<(Position, [f32; 2])> {
        if let Some(entity) = self.player_by_number(target) {
            let motion = self.world.get::<Body>(entity)?.motion;
            return Some((motion.position(), motion.velocity()));
        }
        let (_, ghost) = self.ghost(target)?;
        ghost.mob.is_none().then_some((ghost.position, [0.0; 2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: Position = Position {
        x: 100.0,
        y: 100.0,
        z: 20.0,
    };
    const TOLERANCE: f32 = 1e-4;
    /// A target 8 m north of [`FROM`].
    const TARGET: Position = Position { y: 108.0, ..FROM };
    const FLIGHT_S: f32 = 0.5;

    #[test]
    fn a_lead_aims_where_the_target_will_be_within_its_angle() {
        // Running east at 4 m/s: 2 m ahead in the flight, 14 degrees off the direct line.
        let east = [4.0, 0.0];
        let wide = lead(FROM, TARGET, east, FLIGHT_S, 20_f32.to_radians());
        assert!((wide.x - 102.0).abs() < TOLERANCE && (wide.y - 108.0).abs() < TOLERANCE);
        // A 10 degree limit turns the aim back to 10 degrees east of the line, as far out.
        let narrow = lead(FROM, TARGET, east, FLIGHT_S, 10_f32.to_radians());
        let distance = FROM.horizontal_distance(wide);
        assert!((FROM.horizontal_distance(narrow) - distance).abs() < TOLERANCE);
        let off = turn(yaw_toward(FROM, TARGET), yaw_toward(FROM, narrow));
        assert!((off.abs() - 10_f32.to_radians()).abs() < TOLERANCE);
        assert!(narrow.x > FROM.x, "east of the line");
        assert_eq!(
            lead(FROM, TARGET, [0.0; 2], FLIGHT_S, 0.0),
            TARGET,
            "a still target"
        );
    }

    use nw_sim_core::Motion;

    use crate::combat::test_support::TEST_RANGED_ITEM;
    use crate::mob_ai::Strike;
    use crate::mob_ai::test_support::{
        TEST_MOB_ROW, TEST_MOB_TABLE, TEST_RANGED_ATTACK, TEST_SHOT_RANGE_M, TEST_SHOT_SLICE,
        TEST_SHOT_SPEED_MPS,
    };
    use crate::mobs::{MobReach, mob_entity};
    use crate::world::Vitals;
    use crate::world::tests::{NOW, join, table, test_spawn, world_with_spawns};

    const MOB_X: f32 = 1024.0;
    /// The player stands this far east of the mob.
    const SHOT_M: f32 = 6.0;

    /// A world with one mob and player 1 [`SHOT_M`] east of it.
    fn range() -> SimWorld {
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + SHOT_M);
        world
    }

    /// The test mob's ranged clip at player 1, from the main-hand item `weapon`.
    fn shot_at(weapon: Option<&str>) -> MobAttackIntent {
        MobAttackIntent {
            mob: mob_entity(0),
            target: 1,
            target_at: Position {
                x: MOB_X + SHOT_M,
                y: 10.0,
                z: 50.0,
            },
            mob_move: None,
            strike: Some(Strike {
                table: TEST_MOB_TABLE.to_owned(),
                row: TEST_MOB_ROW.to_owned(),
                weapon: weapon.map(str::to_owned),
                reach: MobReach {
                    lunge_max_m: 0.0,
                    lunge_turn_rad: 0.0,
                    reach_m: 0.0,
                },
                ranged: Some(RangedClip {
                    attack: TEST_RANGED_ATTACK.to_owned(),
                    slice: Some(TEST_SHOT_SLICE.to_owned()),
                    lead: false,
                    lead_max_rad: 0.0,
                }),
            }),
        }
    }

    fn health_of(world: &mut SimWorld) -> u32 {
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .current()
    }

    /// Puts player 1 at `x` on the mob's line.
    fn move_player(world: &mut SimWorld, x: f32, y: f32) {
        let entity = world.player_by_number(1).expect("player");
        let limits = world.settings.limits;
        world.world.get_mut::<Body>(entity).expect("body").motion =
            Motion::new(Position { x, y, z: 50.0 }, NOW, &limits);
    }

    #[test]
    fn a_mob_shot_flies_and_hits_the_player_where_it_aimed() {
        let mut world = range();
        let full = health_of(&mut world);
        assert!(
            world
                .mob_hits(vec![(shot_at(Some(TEST_RANGED_ITEM)), None)], NOW)
                .is_empty(),
            "on its way"
        );
        // 6 m at 20 m/s.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "300 ms"
        )]
        let flight_ms = (SHOT_M / TEST_SHOT_SPEED_MPS * MS_PER_SECOND).round() as u64;
        assert!(world.mob_shots_due(NOW + flight_ms - 1).is_empty());
        assert_eq!(world.mob_shots_due(NOW + flight_ms).len(), 1);
        assert!(health_of(&mut world) < full);
        assert!(world.mob_shots.is_empty());
    }

    #[test]
    fn a_mob_shot_misses_a_player_who_stepped_aside() {
        let mut world = range();
        let full = health_of(&mut world);
        world.mob_hits(vec![(shot_at(Some(TEST_RANGED_ITEM)), None)], NOW);
        // 1 m aside: beyond the 0.5 m projectile and the body's radius.
        move_player(&mut world, MOB_X + SHOT_M, 11.0);
        assert!(world.mob_shots_due(NOW + 1_000).is_empty());
        assert_eq!(health_of(&mut world), full);
    }

    #[test]
    fn a_mob_fires_no_shot_beyond_its_range_or_without_its_profile() {
        let mut world = range();
        move_player(&mut world, MOB_X + TEST_SHOT_RANGE_M + 1.0, 10.0);
        world.mob_hits(vec![(shot_at(Some(TEST_RANGED_ITEM)), None)], NOW);
        assert!(world.mob_shots.is_empty(), "out of range");
        move_player(&mut world, MOB_X + SHOT_M, 10.0);
        world.mob_hits(vec![(shot_at(None), None)], NOW);
        assert!(world.mob_shots.is_empty(), "no weapon, no ranged profile");
    }
}
