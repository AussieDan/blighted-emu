// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attacks of ranged and magic weapons (staffs, gauntlets, bows, muskets): the server casts
//! their projectile. A retail client sends no hit request for them, only its inputs (Observed:
//! the fire staff and bow capture holds 293 hits and no client message but inputs around them),
//! and the projectiles are server data: the attack's `CAGE-RangedAttack` clip or its spell
//! (`SpellData`), the ranged attack database's range and radius, the projectile slice's flight
//! ([`nw_content::player_attacks`]). So an attack flies along the player's aim: the first mob on
//! its line, where the client shows it ([`super::mob_trail`]), is hit once the projectile got
//! there.

use nw_bus::ConnectionRef;
use nw_content::player_attacks::Flight;
use nw_sim_core::Position;

use super::SimWorld;
use super::hit_shape::PLAYER_CAPSULE;
use super::mob_timeline::yaw_toward;
use crate::combat::{AttackOutcome, AttackRefusal};
use crate::mobs::Capsule;

/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;

/// How a projectile or hit scan of one attack meets bodies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Shot {
    /// `m_maxRange`.
    range_m: f32,
    /// `m_projectileRadius`; 0 for a hit scan.
    radius_m: f32,
    /// How it flies; `None` for a hit scan, which is there at once.
    flight: Option<Flight>,
    /// The radius around the body it meets in which every mob is hit; 0: that body only.
    splash_m: f32,
}

impl Shot {
    /// A projectile of `range_m` and `radius_m` that flies as `flight`
    /// ([`AttackHit::Projectile`]).
    pub(super) const fn projectile(range_m: f32, radius_m: f32, flight: Flight) -> Self {
        Self {
            range_m,
            radius_m,
            flight: Some(flight),
            splash_m: 0.0,
        }
    }

    /// A hit scan of `range_m` ([`AttackHit::HitScan`]).
    pub(super) const fn hit_scan(range_m: f32) -> Self {
        Self {
            range_m,
            radius_m: 0.0,
            flight: None,
            splash_m: 0.0,
        }
    }

    /// How far it goes when it meets nothing, and when it ends there (seconds): its range, or
    /// where its flight's lifetime ends before it.
    pub(super) fn end(&self) -> (f32, f32) {
        match &self.flight {
            None => (self.range_m, 0.0),
            Some(flight) => {
                let distance_m = self.range_m.min(flight.distance_at(flight.lifetime_s));
                let time_s = flight.time_to(distance_m).unwrap_or(flight.lifetime_s);
                (distance_m, time_s)
            }
        }
    }

    /// The shot, hitting every mob within `splash_m` of the body it meets.
    pub(super) const fn with_splash(self, splash_m: f32) -> Self {
        Self { splash_m, ..self }
    }
}

/// What a shot does to the mobs it hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShotHit<'a> {
    /// The `DamageData` row of the hit; `None` for a spell of status effects alone, which hurts
    /// no one.
    pub(super) damage_id: Option<&'a str>,
    /// The status effects it puts on a mob it hurts, with the number of the player they come
    /// from (an ability spell's `StatusEffects`).
    pub(super) effects: Option<(&'a [String], u64)>,
}

impl<'a> ShotHit<'a> {
    /// A hit of the row `damage_id` and nothing more.
    pub(super) const fn plain(damage_id: &'a str) -> Self {
        Self {
            damage_id: Some(damage_id),
            effects: None,
        }
    }
}

/// A projectile on its way to the mob it hits.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProjectileFlight {
    attacker: ConnectionRef,
    target: u64,
    /// [`ShotHit::damage_id`].
    damage_id: Option<String>,
    /// When it gets there.
    land_ms: u64,
    /// [`Shot::with_splash`].
    splash_m: f32,
    /// [`ShotHit::effects`].
    effects: Option<(Vec<String>, u64)>,
}

/// A flight of `flight_s` seconds in whole milliseconds.
pub(super) fn flight_ms(flight_s: f32) -> u64 {
    // A flight of at most its lifetime, a few seconds.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a flight of at most a few seconds"
    )]
    let flight_ms = (flight_s.max(0.0) * MS_PER_SECOND) as u64;
    flight_ms
}

/// Where an attack is aimed: the camera's yaw and pitch (the input's view angles; a positive
/// pitch looks down, Observed: the retail `lookDir` z is `-sin(pitch)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct AimLine {
    pub(super) yaw: f32,
    pub(super) pitch: f32,
}

/// The first body of `bodies` (mob, place, capsule) that `shot` from a shooter standing at `from`
/// along `aim` meets, with how long it flies to it (seconds); `None` when it meets none. It meets
/// a body whose edge is within its range and its radius of its line, before its flight ends,
/// while it is level with the body: it leaves within the shooter's body height, climbs or sinks
/// with the aim's pitch and falls by its gravity on the way.
pub(super) fn first_on_line(
    shot: &Shot,
    from: Position,
    aim: AimLine,
    bodies: &[(u64, Position, Capsule)],
) -> Option<(u64, f32)> {
    let yaw = aim.yaw;
    // Yaw 0 faces +y; yaw grows toward -x (see `yaw_toward`).
    let (east, north) = (-yaw.sin(), yaw.cos());
    bodies
        .iter()
        .filter_map(|(mob, at, body)| {
            let (dx, dy) = (at.x - from.x, at.y - from.y);
            let along = dx * east + dy * north;
            let aside = (dx * north - dy * east).abs();
            if along < 0.0
                || along > shot.range_m + body.radius_m
                || aside > shot.radius_m + body.radius_m
            {
                return None;
            }
            let (time_s, drop_m) = match &shot.flight {
                Some(flight) => {
                    let time_s = flight.time_to(along)?;
                    (time_s, flight.drop_at(time_s))
                }
                None => (0.0, 0.0),
            };
            let rise_m = along * (-aim.pitch).tan() - drop_m;
            let low = from.z + rise_m - shot.radius_m;
            let high = from.z + PLAYER_CAPSULE.height_m + rise_m + shot.radius_m;
            let level = low < at.z + body.height_m && at.z < high;
            level.then_some((*mob, along, time_s))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(mob, _, time_s)| (mob, time_s))
}

impl SimWorld {
    /// The player of `attacker` at `position` fires `shot` of the attack whose hit is `hit` at
    /// `now_ms`, along `aim`. The mob it meets of `places` is hit when the shot gets there
    /// ([`SimWorld::projectile_flights_due`]).
    pub(super) fn cast(
        &mut self,
        attacker: &ConnectionRef,
        (shot, hit): (Shot, ShotHit<'_>),
        (position, aim): (Position, AimLine),
        places: &[(u64, Position, Capsule)],
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let met = first_on_line(&shot, position, aim, places);
        tracing::debug!(attacker = ?attacker, damage_id = ?hit.damage_id, ?aim, met = ?met, "player cast");
        let Some((target, flight_s)) = met else {
            return vec![Ok(AttackOutcome::Missed)];
        };
        self.launch(attacker, (shot, hit), target, flight_s, now_ms);
        Vec::new()
    }

    /// Sends `shot` of `attacker` with the hit `hit` to mob `target`, which it gets to after
    /// `flight_s` from `now_ms`.
    pub(super) fn launch(
        &mut self,
        attacker: &ConnectionRef,
        (shot, hit): (Shot, ShotHit<'_>),
        target: u64,
        flight_s: f32,
        now_ms: u64,
    ) {
        self.projectile_flights.push(ProjectileFlight {
            attacker: attacker.clone(),
            target,
            damage_id: hit.damage_id.map(str::to_owned),
            land_ms: now_ms.saturating_add(flight_ms(flight_s)),
            splash_m: shot.splash_m,
            effects: hit
                .effects
                .map(|(effects, source)| (effects.to_vec(), source)),
        });
    }

    /// The yaw of an attack from `from` with the aim of a lock on `lock_target` (toward it when it
    /// is among `places`) or the camera yaw `facing`.
    pub(super) fn aim_yaw(
        lock_target: Option<u64>,
        facing: Option<f32>,
        from: Position,
        places: &[(u64, Position, Capsule)],
    ) -> Option<f32> {
        lock_target
            .and_then(|target| {
                places
                    .iter()
                    .find(|(mob, _, _)| *mob == target)
                    .map(|(_, at, _)| yaw_toward(from, *at))
            })
            .or(facing)
    }

    /// Lands the projectiles that got to their mob at `now_ms`: a splash also hits the other
    /// living mobs within its radius of that mob, and each mob it hurts takes its effects; a
    /// projectile of effects alone puts them on each living mob it reaches.
    pub(super) fn projectile_flights_due(
        &mut self,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.projectile_flights)
            .into_iter()
            .partition(|flight| now_ms >= flight.land_ms);
        self.projectile_flights = later;
        let mut results = Vec::new();
        for flight in due {
            let splashed: Vec<u64> = if flight.splash_m > 0.0 {
                let centre = self
                    .mob_states()
                    .find(|(mob, _, _)| *mob == flight.target)
                    .map(|(_, at, _)| at);
                centre.map_or_else(Vec::new, |centre| {
                    self.mob_states()
                        .filter(|(mob, at, state)| {
                            *mob != flight.target
                                && state.health > 0
                                && at.horizontal_distance(centre) <= flight.splash_m
                        })
                        .map(|(mob, _, _)| mob)
                        .collect()
                })
            } else {
                Vec::new()
            };
            let first = std::iter::once((flight.target, true));
            for (mob, paced) in first.chain(splashed.into_iter().map(|mob| (mob, false))) {
                let Some(damage_id) = &flight.damage_id else {
                    let alive = self
                        .mob_states()
                        .any(|(own, _, state)| own == mob && state.health > 0);
                    if let (true, Some((effects, source))) = (alive, &flight.effects) {
                        self.spell_effects_on_mob(mob, effects, *source, false);
                    }
                    continue;
                };
                let result = self.hit_paced(&flight.attacker, mob, damage_id, None, paced, now_ms);
                if let (Ok(AttackOutcome::Hit { target, .. }), Some((effects, source))) =
                    (&result, &flight.effects)
                {
                    self.spell_effects_on_mob(*target, effects, *source, false);
                }
                results.push(result);
            }
        }
        results
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
    const BODY: Capsule = Capsule {
        radius_m: 0.5,
        height_m: 1.8,
    };
    const NORTH: AimLine = AimLine {
        yaw: 0.0,
        pitch: 0.0,
    };
    /// The ice gauntlet's light bolt (`IceMagic` / `Primary`: 50 m, radius 0.15;
    /// `icemagic_primary_proj`: 100 m/s, 25 more over 1 s, no gravity, 0.5 s).
    const BOLT: Shot = Shot::projectile(
        50.0,
        0.15,
        Flight {
            base_speed_mps: 100.0,
            extra_speed_mps: 25.0,
            time_for_max_speed_s: 1.0,
            gravity_mps2: 0.0,
            lifetime_s: 0.5,
        },
    );
    /// The bow's hip shot (`Bow` / `Basic`: 100 m, radius 0.15; `basic_arrow`: gravity 9.8, 10 s).
    const ARROW: Shot = Shot::projectile(
        100.0,
        0.15,
        Flight {
            base_speed_mps: 100.0,
            extra_speed_mps: 25.0,
            time_for_max_speed_s: 1.0,
            gravity_mps2: 9.8,
            lifetime_s: 10.0,
        },
    );

    fn at(dx: f32, dy: f32) -> Position {
        Position {
            x: FROM.x + dx,
            y: FROM.y + dy,
            ..FROM
        }
    }

    #[test]
    fn a_projectile_meets_the_first_body_on_its_line() {
        let bodies = [
            (1, at(0.0, 15.0), BODY),
            (2, at(0.3, 8.0), BODY),
            (3, at(3.0, 5.0), BODY),
            (4, at(0.0, -5.0), BODY),
        ];
        assert_eq!(
            first_on_line(&BOLT, FROM, NORTH, &bodies).map(|(mob, _)| mob),
            Some(2),
            "the nearer one on the line; not the one aside nor behind"
        );
        let east = [(7, at(10.0, 0.0), BODY)];
        let yaw_east = AimLine {
            yaw: yaw_toward(FROM, at(10.0, 0.0)),
            pitch: 0.0,
        };
        assert_eq!(
            first_on_line(&BOLT, FROM, yaw_east, &east).map(|(mob, _)| mob),
            Some(7),
            "the yaw convention of yaw_toward"
        );
    }

    #[test]
    fn a_projectile_flies_until_its_lifetime_ends() {
        // 0.5 s of the bolt: 50 m plus 3.125 m of its gained speed.
        let (_, time_s) =
            first_on_line(&BOLT, FROM, NORTH, &[(1, at(0.0, 40.0), BODY)]).expect("in reach");
        // 100 t + 12.5 t^2 = 40: t = 0.382 s.
        assert!((time_s - 0.382).abs() < 0.002, "{time_s}");
        assert_eq!(
            first_on_line(&BOLT, FROM, NORTH, &[(1, at(0.0, 54.0), BODY)]),
            None,
            "beyond its flight"
        );
    }

    #[test]
    fn a_falling_projectile_passes_under_a_high_body() {
        // An arrow at 80 m flies about 0.73 s and falls about 2.6 m: aimed level it passes under
        // a level body, aimed up by 2.6 m over 80 m it meets it.
        let up = AimLine {
            yaw: 0.0,
            pitch: -(2.6_f32 / 80.0).atan(),
        };
        let level = [(1, at(0.0, 80.0), BODY)];
        assert_eq!(first_on_line(&ARROW, FROM, NORTH, &level), None);
        assert!(first_on_line(&ARROW, FROM, up, &level).is_some());
        let high = [(
            1,
            Position {
                z: FROM.z + PLAYER_CAPSULE.height_m + BODY.radius_m,
                ..at(0.0, 80.0)
            },
            BODY,
        )];
        let below = [(
            1,
            Position {
                z: FROM.z - 3.0,
                ..at(0.0, 80.0)
            },
            BODY,
        )];
        assert_eq!(first_on_line(&ARROW, FROM, up, &high), None);
        assert!(
            first_on_line(&ARROW, FROM, NORTH, &below).is_some(),
            "it fell to the body"
        );
        let bolt_below = [(
            1,
            Position {
                z: FROM.z - 3.0,
                ..at(0.0, 40.0)
            },
            BODY,
        )];
        assert_eq!(
            first_on_line(&BOLT, FROM, NORTH, &bolt_below),
            None,
            "a bolt does not fall"
        );
    }

    #[test]
    fn a_hit_scan_is_there_at_once_within_its_range() {
        // The blunderbuss's spread (`BlunderbussPrimaryFire_Spread`: hit scan, 14 m).
        let spread = Shot::hit_scan(14.0);
        assert_eq!(
            first_on_line(&spread, FROM, NORTH, &[(1, at(0.0, 10.0), BODY)]),
            Some((1, 0.0))
        );
        assert_eq!(
            first_on_line(&spread, FROM, NORTH, &[(1, at(0.0, 15.0), BODY)]),
            None
        );
    }
}
