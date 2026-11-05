// SPDX-License-Identifier: AGPL-3.0-or-later
//! Combat rules (M6-A spec section 4): health, the hit formulas, crits and threat. The formulas
//! are Inferred from the datasheet field names; the client holds none of them. Every random roll
//! comes from [`Dice`], seeded by the caller, so a run repeats exactly.

use std::collections::HashMap;

/// The smallest hit: a hit that lands always costs health.
pub const MIN_HIT: u32 = 1;

/// SplitMix64 increment.
const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
/// SplitMix64 mixing constants.
const SPLITMIX_MIX_1: u64 = 0xBF58_476D_1CE4_E5B9;
const SPLITMIX_MIX_2: u64 = 0x94D0_49BB_1331_11EB;
/// Bits of a roll that make a fraction from 0 to 1 (the mantissa of an `f64`).
const FRACTION_BITS: u32 = 53;

/// Seeded random rolls (SplitMix64).
#[derive(Debug, Clone)]
pub struct Dice {
    state: u64,
}

impl Dice {
    /// Dice that give the same rolls for the same `seed`.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(SPLITMIX_GAMMA);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(SPLITMIX_MIX_1);
        value = (value ^ (value >> 27)).wrapping_mul(SPLITMIX_MIX_2);
        value ^ (value >> 31)
    }

    /// `true` with chance `p` (0 never, 1 always).
    pub fn chance(&mut self, p: f64) -> bool {
        self.fraction() < p
    }

    /// A fraction from 0 (included) to 1 (excluded).
    pub fn fraction(&mut self) -> f64 {
        (self.next() >> (u64::BITS - FRACTION_BITS)) as f64 / (1_u64 << FRACTION_BITS) as f64
    }

    /// A roll from 0 to `n - 1`; 0 when `n` is 0.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        // The remainder of a u64 by a u32 fits a u32.
        (self.next() % u64::from(n)) as u32
    }
}

/// What a hit did to a health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// Health is lower, above 0.
    Hurt,
    /// This hit took the last health.
    Killed,
    /// The health was already 0.
    AlreadyDead,
}

/// Health from 0 to its max; at 0 the entity is dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    current: u32,
    max: u32,
}

impl Health {
    /// Health at its `max`.
    pub const fn full(max: u32) -> Self {
        Self { current: max, max }
    }

    /// Health at `current`, at most `max` (a player that arrives from another sim).
    pub fn at(current: u32, max: u32) -> Self {
        Self {
            current: current.min(max),
            max,
        }
    }

    /// Takes `amount` of health.
    pub fn take(&mut self, amount: u32) -> Damage {
        if self.current == 0 {
            return Damage::AlreadyDead;
        }
        self.current = self.current.saturating_sub(amount);
        if self.current == 0 {
            Damage::Killed
        } else {
            Damage::Hurt
        }
    }

    /// Whether the health is 0.
    pub const fn is_dead(&self) -> bool {
        self.current == 0
    }

    /// Back to the max.
    pub const fn refill(&mut self) {
        self.current = self.max;
    }

    /// Adds `amount`, at most to the max; the health after it.
    pub fn heal(&mut self, amount: u32) -> u32 {
        self.current = self.current.saturating_add(amount).min(self.max);
        self.current
    }

    /// A new max: a rise adds to the health too, a fall takes it down to the max at most
    /// (Inferred: retail shows the player full after each level up of newchar 2026-09-18).
    pub fn set_max(&mut self, max: u32) {
        if max > self.max && self.current > 0 {
            self.current = self.current.saturating_add(max - self.max);
        }
        self.max = max;
        self.current = self.current.min(max);
    }

    /// The health now.
    pub const fn current(&self) -> u32 {
        self.current
    }

    /// The max.
    pub const fn max(&self) -> u32 {
        self.max
    }
}

/// Health each zero-based level squared adds to a player's max (module notes of
/// [`player_max_health`]).
const PLAYER_HEALTH_PER_LEVEL_SQUARED: f64 = 1.75;

/// Max health of a player of zero-based `level`: its vitals' base health (`Player`
/// `HealthInitial`, 856), 1.75 x level squared, and the `Health` its Constitution gives, without
/// the fraction. Observed: the own `healthMax` of newchar-retail-2026-09-18 at levels 0 and 2 to 7
/// (856, 863, 871.75, 884, 899.75, 919, 941.75; 200 more after 5 points in Constitution, its
/// row 10) and of general-other-01 at level 64 with Constitution 200 (856 + 7168 + 6419 = 14443).
pub fn player_max_health(base: u32, level: u32, constitution_health: u32) -> u32 {
    let level = f64::from(level);
    let health = f64::from(base)
        + PLAYER_HEALTH_PER_LEVEL_SQUARED * level * level
        + f64::from(constitution_health);
    to_whole(health.floor())
}

/// Max health of a mob: the level's base health, its `HealthMod`, then its creature type's
/// `CategoryHealthMod`, each product without its fraction (the base too). Observed on all 14
/// kinds and levels of the retail cards of newchar-retail-2026-09-18, among them the tutorial
/// sailor (337.5, 1.05, 1.0: 353, where the plain product gives 354), the level-3 boar (412.5,
/// 1.75, 0.6: 432) and the named level-4 sailor (466.55, 1.3, 2.5: 605 x 2.5 = 1512).
pub fn mob_max_health(base_max_health: f64, health_mod: f64, category_mod: f64) -> u32 {
    let typed = (base_max_health.floor() * health_mod).floor();
    to_whole((typed * category_mod).floor())
}

/// A rounded, finite, non-negative value as a whole number.
fn to_whole(value: f64) -> u32 {
    if value.is_finite() && value > 0.0 {
        // Clamped to the u32 range first, so the cast only drops the fraction.
        value.round().min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

/// One hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Health it takes, at least [`MIN_HIT`].
    pub amount: u32,
    /// Whether it was a crit.
    pub crit: bool,
}

/// A player's attack with its weapon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerAttack {
    /// The weapon's base damage.
    pub base_damage: f64,
    /// The attack's damage coefficient.
    pub coefficient: f64,
    /// Whether the attack can crit.
    pub can_crit: bool,
    /// The weapon's crit chance, 0 to 1.
    pub crit_chance: f64,
    /// The weapon's crit damage multiplier.
    pub crit_multiplier: f64,
}

/// A mob's attack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobAttack {
    /// The level's base damage.
    pub base_damage: f64,
    /// The type's damage mod.
    pub damage_mod: f64,
    /// The config factor for the attack row, which lives in the mob's action list.
    pub factor: f64,
}

/// `Armor Set Rating Exponent` of `playerbaseattributes.pbadb` (field `armor set rating
/// exponent`): a full armor set of gear score `gs` rates `gs ^ 1.2`. Observed: the
/// `VitalsLevelData` armor ratings are exactly their gear score to this power (level 1, gear score
/// 50: 109.3362; level 60, gear score 600: 2156.659).
pub const ARMOR_SET_RATING_EXPONENT: f64 = 1.2;
/// `Armor Mitigation Exponent` of `playerbaseattributes.pbadb`: the attacker's gear score enters
/// the mitigation at this power ([`armor_share`]).
pub const ARMOR_MITIGATION_EXPONENT: f64 = 1.2;
/// The scale of the attacker's gear score in the mitigation ([`armor_share`]). Observed: the
/// retail tutorial hits of gear score 60 and 75 weapons on level 1 mobs (armor 109.3362) kept
/// 0.70114803 and 0.7540894 of their damage (newchar-retail-2026-09-18, 2071 scale); both give
/// this value to 8 digits with the exponent 1.2. How it comes from the file's `Physical Armor
/// Scale Factor` 1850 is Unknown.
pub const ARMOR_MITIGATION_SCALE: f64 = 1.885_107_35;
/// `Min Possible Weapon Gear Score` of `playerbaseattributes.pbadb`: the gear score from which a
/// weapon's base damage compounds ([`weapon_damage_scale`]).
pub const MIN_WEAPON_GEAR_SCORE: u32 = 100;
/// `Base Damage Compound Increase` of `playerbaseattributes.pbadb`: the base damage grows by this
/// share per `Base Damage Gear Score Interval`.
pub const BASE_DAMAGE_COMPOUND_INCREASE: f64 = 0.0112;
/// `Base Damage Gear Score Interval` of `playerbaseattributes.pbadb`.
pub const BASE_DAMAGE_GEAR_SCORE_INTERVAL: u32 = 5;
/// `Diminishing Gear Score Threshold` of `playerbaseattributes.pbadb`: above it each interval adds
/// the increase times [`COMPOUND_DIMINISHING_MULTIPLIER`].
pub const DIMINISHING_GEAR_SCORE_THRESHOLD: u32 = 500;
/// `Compound Increase Diminishing Multiplier` of `playerbaseattributes.pbadb`.
pub const COMPOUND_DIMINISHING_MULTIPLIER: f64 = 0.6667;

/// The armor rating of a full set of gear score `gear_score` ([`ARMOR_SET_RATING_EXPONENT`]).
pub fn armor_rating(gear_score: u32) -> f64 {
    f64::from(gear_score).powf(ARMOR_SET_RATING_EXPONENT)
}

/// The share of a hit that `armor` (a rating) takes off an attacker of gear score `gear_score`:
/// `armor / (armor + scale × gear score ^ 1.2)` ([`ARMOR_MITIGATION_SCALE`],
/// [`ARMOR_MITIGATION_EXPONENT`]). The form is Inferred from the two retail points it fits; the
/// file's `Min Armor Mitigation` 0 and `Max Armor Mitigation` 2 are not applied (their use is
/// Unknown; 2 is no share).
pub fn armor_share(armor: f64, gear_score: u32) -> f64 {
    let against = ARMOR_MITIGATION_SCALE * f64::from(gear_score).powf(ARMOR_MITIGATION_EXPONENT);
    if armor <= 0.0 || against <= 0.0 {
        return 0.0;
    }
    armor / (armor + against)
}

/// The multiple of a weapon's `BaseDamage` at gear score `gear_score`: 1 at and below
/// [`MIN_WEAPON_GEAR_SCORE`], then compounding by [`BASE_DAMAGE_COMPOUND_INCREASE`] per
/// [`BASE_DAMAGE_GEAR_SCORE_INTERVAL`], by the diminished increase above
/// [`DIMINISHING_GEAR_SCORE_THRESHOLD`]. Inferred from the file's field names; the floor at the
/// minimum fits the retail tutorial hits of gear score 60 and 75 weapons (`1hSwordT2_FTUE`, base
/// 80: 68.000 before armor, a whole share of its base); the compound above 100 is not checked
/// against a capture.
pub fn weapon_damage_scale(gear_score: u32) -> f64 {
    let interval = f64::from(BASE_DAMAGE_GEAR_SCORE_INTERVAL);
    let full = gear_score
        .clamp(MIN_WEAPON_GEAR_SCORE, DIMINISHING_GEAR_SCORE_THRESHOLD)
        .saturating_sub(MIN_WEAPON_GEAR_SCORE);
    let diminished = gear_score.saturating_sub(DIMINISHING_GEAR_SCORE_THRESHOLD);
    (1.0 + BASE_DAMAGE_COMPOUND_INCREASE).powf(f64::from(full) / interval)
        * (1.0 + BASE_DAMAGE_COMPOUND_INCREASE * COMPOUND_DIMINISHING_MULTIPLIER)
            .powf(f64::from(diminished) / interval)
}

/// A player's hit on a target with `armor_share` of the damage taken off.
pub fn player_hit(attack: &PlayerAttack, armor_share: f64, dice: &mut Dice) -> Hit {
    let crit = attack.can_crit && dice.chance(attack.crit_chance);
    let multiplier = if crit { attack.crit_multiplier } else { 1.0 };
    let raw = attack.base_damage * attack.coefficient * multiplier * (1.0 - armor_share);
    Hit {
        amount: to_whole(raw).max(MIN_HIT),
        crit,
    }
}

/// A mob's hit on a target with `armor_share` of the damage taken off. Mob attacks do not crit.
pub fn mob_hit(attack: &MobAttack, armor_share: f64) -> Hit {
    let raw = attack.base_damage * attack.damage_mod * attack.factor * (1.0 - armor_share);
    Hit {
        amount: to_whole(raw).max(MIN_HIT),
        crit: false,
    }
}

/// Threat of each attacker on one target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Threat {
    by_attacker: HashMap<u64, u64>,
}

impl Threat {
    /// Adds `amount` to `attacker`'s threat.
    pub fn add(&mut self, attacker: u64, amount: u64) {
        let threat = self.by_attacker.entry(attacker).or_default();
        *threat = threat.saturating_add(amount);
    }

    /// Removes `attacker`.
    pub fn forget(&mut self, attacker: u64) {
        self.by_attacker.remove(&attacker);
    }

    /// The attacker with the most threat; a tie goes to the lower entity number.
    pub fn top(&self) -> Option<u64> {
        self.by_attacker
            .iter()
            .max_by(|(one, a), (two, b)| a.cmp(b).then(two.cmp(one)))
            .map(|(attacker, _)| *attacker)
    }

    /// Removes every attacker.
    pub fn clear(&mut self) {
        self.by_attacker.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: u64 = 7;
    const LEVEL_20_HEALTH: f64 = 1872.2;
    const HALF: f64 = 0.5;
    const SWORD_BASE: f64 = 64.0;
    const HEAVY_COEFFICIENT: f64 = 1.2;
    const ARMOR_SHARE: f64 = 0.35;
    const CRIT_MULTIPLIER: f64 = 1.3;
    const ROLLS: usize = 1000;

    fn sword(can_crit: bool, crit_chance: f64) -> PlayerAttack {
        PlayerAttack {
            base_damage: SWORD_BASE,
            coefficient: HEAVY_COEFFICIENT,
            can_crit,
            crit_chance,
            crit_multiplier: CRIT_MULTIPLIER,
        }
    }

    /// The armor rules give the retail tutorial shares: gear score 60 and 75 weapons on a level 1
    /// mob (armor 50 ^ 1.2) keep 0.70114803 and 0.7540894 (newchar-retail-2026-09-18).
    #[test]
    fn armor_takes_the_retail_tutorial_shares() {
        const MOB_GEAR_SCORE: u32 = 50;
        const MOB_ARMOR: f64 = 109.336_207;
        assert!((armor_rating(MOB_GEAR_SCORE) - MOB_ARMOR).abs() < 1e-3);
        for (gear_score, kept) in [(60, 0.701_148_03), (75, 0.754_089_4)] {
            let share = armor_share(armor_rating(MOB_GEAR_SCORE), gear_score);
            assert!((1.0 - share - kept).abs() < 1e-6, "{gear_score}: {share}");
        }
        assert_eq!(armor_share(0.0, 60), 0.0, "no armor");
    }

    #[test]
    fn a_weapon_compounds_its_damage_from_gear_score_100() {
        assert_eq!(weapon_damage_scale(60), 1.0, "below the minimum");
        assert_eq!(weapon_damage_scale(MIN_WEAPON_GEAR_SCORE), 1.0);
        let at_500 = 1.0112_f64.powi(80);
        assert!((weapon_damage_scale(500) - at_500).abs() < 1e-9);
        let at_600 = at_500 * (1.0 + 0.0112 * 0.6667_f64).powi(20);
        assert!((weapon_damage_scale(600) - at_600).abs() < 1e-9);
    }

    #[test]
    fn a_heal_stops_at_the_max() {
        let mut health = Health::at(500, 856);
        assert_eq!(health.heal(300), 800);
        assert_eq!(health.heal(486), 856);
    }

    #[test]
    fn player_health_fits_the_retail_levels_and_constitution() {
        /// `Player` `HealthInitial`.
        const BASE: u32 = 856;
        /// Constitution `Health` of rows 10 and 200.
        const CONSTITUTION_10: u32 = 200;
        const CONSTITUTION_200: u32 = 6_419;
        let retail = [
            (0, 856),
            (2, 863),
            (3, 871),
            (4, 884),
            (5, 899),
            (6, 919),
            (7, 941),
        ];
        for (level, health) in retail {
            assert_eq!(player_max_health(BASE, level, 0), health, "level {level}");
        }
        assert_eq!(player_max_health(BASE, 5, CONSTITUTION_10), 1_099);
        assert_eq!(player_max_health(BASE, 64, CONSTITUTION_200), 14_443);
    }

    #[test]
    fn a_new_max_moves_the_health_with_a_rise_and_caps_it_on_a_fall() {
        let mut health = Health::full(856);
        health.set_max(863);
        assert_eq!((health.current(), health.max()), (863, 863));
        let mut hurt = Health::at(500, 856);
        hurt.set_max(1_056);
        assert_eq!(hurt.current(), 700);
        hurt.set_max(600);
        assert_eq!((hurt.current(), hurt.max()), (600, 600));
        let mut dead = Health::at(0, 856);
        dead.set_max(900);
        assert!(dead.is_dead(), "a rise does not raise the dead");
    }

    #[test]
    fn mob_health_is_the_whole_base_times_mod_without_the_fraction() {
        /// A product just below 371.
        const NEAR_371: f64 = 370.9;
        assert_eq!(mob_max_health(LEVEL_20_HEALTH, HALF, 1.0), 936);
        assert_eq!(mob_max_health(LEVEL_20_HEALTH, 1.0, 1.0), 1872);
        assert_eq!(mob_max_health(NEAR_371, 1.0, 1.0), 370);
    }

    /// The retail cards of newchar-retail-2026-09-18 (vitals `maxHealth`; `VitalsLevelData`
    /// `BaseMaxHealth`, `HealthMod`, `CategoryHealthMod` of the creature type).
    #[test]
    fn mob_health_matches_the_retail_cards() {
        const LEVEL_1: f64 = 337.5;
        const LEVEL_2: f64 = 375.0;
        const LEVEL_3: f64 = 412.5;
        const LEVEL_4: f64 = 466.55;
        // Corrupted_Sailor_1H_Club_FTUE (card 0x0161), Legion_Legionnaire_FTUE.
        assert_eq!(mob_max_health(LEVEL_1, 1.05, 1.0), 353);
        assert_eq!(mob_max_health(LEVEL_1, 1.1, 1.0), 370);
        // Boar_StarterBeach, Solo-_StarterBeach.
        assert_eq!(mob_max_health(LEVEL_2, 1.75, 0.6), 393);
        assert_eq!(mob_max_health(LEVEL_3, 1.75, 0.6), 432);
        // Corrupted_Sailor_1H_Club, _1H_Sword, _2H_Axe, Solo_StarterBeach.
        assert_eq!(mob_max_health(LEVEL_2, 0.6, 1.0), 225);
        assert_eq!(mob_max_health(LEVEL_3, 0.6, 1.0), 247);
        assert_eq!(mob_max_health(LEVEL_3, 0.7, 1.0), 288);
        assert_eq!(mob_max_health(LEVEL_4, 0.6, 1.0), 279);
        assert_eq!(mob_max_health(LEVEL_4, 0.7, 1.0), 326);
        // Named_00 sailors, Solo+_StarterBeach; the named beach captain, Named_Solo+.
        assert_eq!(mob_max_health(LEVEL_2, 0.75, 2.0), 562);
        assert_eq!(mob_max_health(LEVEL_3, 0.75, 2.0), 618);
        assert_eq!(mob_max_health(LEVEL_4, 1.3, 2.5), 1512);
    }

    #[test]
    fn a_player_hit_uses_weapon_coefficient_and_armor() {
        let hit = player_hit(&sword(false, 0.0), ARMOR_SHARE, &mut Dice::new(SEED));
        assert_eq!(
            hit,
            Hit {
                amount: 50,
                crit: false
            }
        );
    }

    #[test]
    fn a_hit_is_at_least_one() {
        let weak = PlayerAttack {
            base_damage: 0.1,
            ..sword(false, 0.0)
        };
        assert_eq!(
            player_hit(&weak, ARMOR_SHARE, &mut Dice::new(SEED)).amount,
            MIN_HIT
        );
        let mob = MobAttack {
            base_damage: 0.0,
            damage_mod: 1.0,
            factor: 1.0,
        };
        assert_eq!(mob_hit(&mob, 0.0).amount, MIN_HIT);
    }

    #[test]
    fn a_crit_multiplies_only_when_the_attack_can_crit() {
        let always = player_hit(&sword(true, 1.0), ARMOR_SHARE, &mut Dice::new(SEED));
        assert_eq!(
            always,
            Hit {
                amount: 65,
                crit: true
            }
        );
        let never = player_hit(&sword(false, 1.0), ARMOR_SHARE, &mut Dice::new(SEED));
        assert!(!never.crit);
        let mut dice = Dice::new(SEED);
        let crits = (0..ROLLS)
            .filter(|_| player_hit(&sword(true, HALF), ARMOR_SHARE, &mut dice).crit)
            .count();
        assert!((400..600).contains(&crits), "{crits} of {ROLLS}");
    }

    #[test]
    fn a_mob_hit_uses_level_damage_mod_and_factor() {
        let attack = MobAttack {
            base_damage: 686.1,
            damage_mod: HALF,
            factor: 2.0,
        };
        assert_eq!(
            mob_hit(&attack, 0.0),
            Hit {
                amount: 686,
                crit: false
            }
        );
        assert_eq!(mob_hit(&attack, HALF).amount, 343);
    }

    #[test]
    fn health_takes_hits_and_dies_once() {
        let mut health = Health::full(100);
        assert_eq!(health.take(30), Damage::Hurt);
        assert_eq!(health.current(), 70);
        assert_eq!(health.take(500), Damage::Killed);
        assert!(health.is_dead());
        assert_eq!(health.current(), 0);
        assert_eq!(health.take(1), Damage::AlreadyDead);
        health.refill();
        assert_eq!(health.current(), health.max());
    }

    #[test]
    fn a_health_can_start_below_its_max() {
        let health = Health::at(30, 100);
        assert_eq!((health.current(), health.max()), (30, 100));
        assert_eq!(Health::at(500, 100).current(), 100, "never above the max");
        assert!(Health::at(0, 100).is_dead());
    }

    #[test]
    fn threat_tops_the_largest_and_forgets() {
        let mut threat = Threat::default();
        assert_eq!(threat.top(), None);
        threat.add(1, 10);
        threat.add(2, 15);
        threat.add(1, 10);
        assert_eq!(threat.top(), Some(1));
        threat.forget(1);
        assert_eq!(threat.top(), Some(2));
        threat.add(3, 15);
        assert_eq!(threat.top(), Some(2), "a tie goes to the lower number");
        threat.clear();
        assert_eq!(threat.top(), None);
    }

    #[test]
    fn the_same_seed_gives_the_same_dice() {
        let mut one = Dice::new(SEED);
        let mut two = Dice::new(SEED);
        let first: Vec<u32> = (0..10).map(|_| one.below(1000)).collect();
        let second: Vec<u32> = (0..10).map(|_| two.below(1000)).collect();
        assert_eq!(first, second);
        assert!(first.iter().all(|&roll| roll < 1000));
        assert_ne!(
            first,
            (0..10)
                .map(|_| Dice::new(SEED + 1).below(1000))
                .collect::<Vec<_>>()
        );
    }
}
