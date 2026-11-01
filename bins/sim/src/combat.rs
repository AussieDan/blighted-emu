// SPDX-License-Identifier: AGPL-3.0-or-later
//! Combat in a `sim` (M6-A spec sections 7 and 8).

use std::sync::Arc;

use nw_bus::{BusId, Hit};
use nw_content::combat::CombatData;

use crate::mob_ai::MobAi;
use crate::mobs::VitalsSpawn;

/// Why an attack did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttackRefusal {
    /// The connection has no player in the world.
    #[error("no player on this connection")]
    NotInWorld,
    /// The attacker is dead.
    #[error("the attacker is dead")]
    Dead,
    /// No such target here (players are no targets: there is no PvP in M6-A).
    #[error("no such target")]
    UnknownTarget,
    /// The target is dead.
    #[error("the target is dead")]
    TargetDead,
    /// The target walks back to its spawn and takes no damage.
    #[error("the target is returning")]
    Returning,
    /// The attack is not one of the weapon's.
    #[error("not an attack of the weapon")]
    NotWeaponAttack,
    /// The target is out of reach (parent spec section 9: range).
    #[error("out of reach")]
    OutOfReach,
    /// Attacks come faster than the weapon allows (parent spec section 9: attack rate).
    #[error("faster than the weapon allows")]
    TooFast,
    /// The shot has no aim: the client sent no view and holds no lock-on.
    #[error("no aim")]
    NoAim,
    /// The weapon has no shot loaded.
    #[error("no shot loaded")]
    NoAmmo,
}

impl AttackRefusal {
    /// The metric label.
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NotInWorld => "not_in_world",
            Self::Dead => "dead",
            Self::UnknownTarget => "unknown_target",
            Self::TargetDead => "target_dead",
            Self::Returning => "returning",
            Self::NotWeaponAttack => "not_weapon_attack",
            Self::OutOfReach => "out_of_reach",
            Self::TooFast => "too_fast",
            Self::NoAim => "no_aim",
            Self::NoAmmo => "no_ammo",
        }
    }
}

/// Why a respawn request did not respawn (M6-B spec section 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RespawnRefusal {
    /// The connection has no player in the world.
    #[error("no player on this connection")]
    NotInWorld,
    /// The player is alive.
    #[error("the player is not dead")]
    NotDead,
    /// The player died less than `min_respawn_ms` ago.
    #[error("too soon after the death")]
    TooSoon,
}

impl RespawnRefusal {
    /// The metric label.
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NotInWorld => "not_in_world",
            Self::NotDead => "not_dead",
            Self::TooSoon => "too_soon",
        }
    }
}

/// A hit for the sim that owns the target (M6-A spec section 7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHit {
    /// The sim that owns the target.
    pub owner: BusId,
    /// The hit.
    pub hit: Hit,
}

/// What a hit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttackOutcome {
    /// The target lost `amount` of health.
    Hit {
        /// The target.
        target: u64,
        /// Health taken.
        amount: u32,
    },
    /// The hit killed the target.
    Killed {
        /// The target.
        target: u64,
    },
    /// The target belongs to another sim; the hit goes there.
    Remote(RemoteHit),
    /// The attack happened and hit nothing: a melee swing whose damage window closed with no
    /// living, not returning mob in its shapes, or a shot whose line met none. It is still a
    /// swing: the client's action goes to its own state and to the other players' views, and
    /// attacks cost no stamina (run 13: 34 swings at nothing were counted as refused).
    Missed,
}

impl AttackOutcome {
    /// The metric label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Hit { .. } => "hit",
            Self::Killed { .. } => "killed",
            Self::Remote(_) => "remote",
            Self::Missed => "missed",
        }
    }
}

/// What combat needs from the configuration and the datasheets.
#[derive(Debug, Clone)]
pub struct CombatSettings {
    /// The combat rows.
    pub data: Arc<CombatData>,
    /// The vitals spawns of the level.
    pub spawns: Arc<[VitalsSpawn]>,
    /// The mob AI: the trees and slices of the game's own data.
    pub ai: Arc<MobAi>,
    /// A player this near wakes a mob.
    pub wake_radius_m: f32,
    /// An awake mob without a target and with no player near for this long falls asleep.
    pub sleep_after_ms: u64,
    /// A player's melee reach in an attack request without a swing (`SimWorld::attack`).
    pub attack_reach_m: f32,
    /// Time from a mob's death to its respawn.
    pub mob_respawn_ms: u64,
    /// Share of a player's damage that mob armor takes off.
    pub armor_share: f64,
    /// Share of a mob's damage that player armor takes off.
    pub player_armor_share: f64,
    /// Extra reach allowed for lag.
    pub reach_margin_m: f32,
    /// Shortest time between two attacks of a player.
    pub min_attack_interval_ms: u64,
    /// Shortest time from a player's death to a respawn on its request.
    pub min_respawn_ms: u64,
    /// Seed of the combat dice.
    pub seed: u64,
    /// The `u32` of a mob's spawn card `AssetId` (M6-B spec section 5.4).
    pub asset_sub_id: u32,
    /// Times a mob type's `LootDropChance` ([`crate::config::CombatConfig::loot_drop_multiplier`]).
    pub loot_drop_multiplier: f64,
}

/// Hand-made combat rows for tests (feature `test-util`).
#[cfg(any(test, feature = "test-util"))]
pub mod test_support {
    use nw_content::combat::{
        AttackRow, CombatData, CombatRows, CombatSource, DamageCategory, LevelRow, MobType,
        WeaponRow,
    };
    use nw_content::player_attacks::{AttackKind, PlayerAttacks, stance_key};

    /// The vitals id of the test mob type.
    pub const TEST_MOB: &str = "Test_Boar";
    /// The level of the test spawns.
    pub const TEST_LEVEL: u32 = 20;
    /// The test player's max health.
    pub const TEST_PLAYER_HEALTH: u32 = 856;
    /// Base health of the test level.
    pub const TEST_LEVEL_HEALTH: f64 = 1000.0;
    /// Base damage of the test level.
    pub const TEST_LEVEL_DAMAGE: f64 = 100.0;
    /// Base damage of the test sword.
    pub const TEST_SWORD_DAMAGE: f64 = 64.0;
    /// The test sword's stance (its retail `MannequinTag`).
    pub const TEST_SWORD_STANCE: &str = "1H_Melee,Sword";
    /// A mob weapon item with the test ranged profile (`mob_ai::test_support`).
    pub const TEST_RANGED_ITEM: &str = "Test_Mob_Crossbow";
    /// The weapon row of [`TEST_RANGED_ITEM`].
    const TEST_RANGED_WEAPON: &str = "Test_Mob_CrossbowT1";
    /// The player attack table of the retail paks (`content-tool player-attacks`, build
    /// 25810808).
    pub const TEST_PLAYER_ATTACKS: &str =
        include_str!("world/testdata/player-attacks-25810808.json");
    /// The slayer sequence table mined from the same build's ledgers.
    pub const TEST_PLAYER_SEQUENCES: &str =
        include_str!("world/testdata/player-sequences-25810808.json");

    /// The retail player attack table.
    pub fn test_player_attacks() -> PlayerAttacks {
        PlayerAttacks::parse(TEST_PLAYER_ATTACKS).expect("player attack table")
    }

    /// The retail slayer sequence table.
    pub fn test_player_sequences() -> nw_content::player_sequences::PlayerSequences {
        nw_content::player_sequences::PlayerSequences::parse(TEST_PLAYER_SEQUENCES)
            .expect("player sequence table")
    }

    /// Rows with the default config ids: one plain mob type, one level, the sword, the player,
    /// and a row of coefficient 1 in the player's table for every basic attack of the retail
    /// stances ([`test_player_attacks`]).
    pub fn test_data() -> CombatData {
        CombatData::build(test_source(), &CombatRows::default())
            .expect("test combat data")
            .with_player_attacks(test_player_attacks())
    }

    /// The source of [`test_data`], for tests that add rows.
    pub fn test_source() -> CombatSource {
        let rows = CombatRows::default();
        CombatSource {
            base_types: vec![(TEST_MOB.to_owned(), MobType::PLAIN)],
            levels: vec![(
                TEST_LEVEL,
                LevelRow {
                    base_max_health: TEST_LEVEL_HEALTH,
                    base_damage: TEST_LEVEL_DAMAGE,
                    physical_armor_rating: 0.0,
                },
            )],
            weapons: vec![(
                rows.weapon_id.clone(),
                WeaponRow {
                    base_damage: TEST_SWORD_DAMAGE,
                    crit_chance: 0.0,
                    crit_multiplier: 1.0,
                    spread_shot: false,
                    max_loaded_ammo: 0,
                    reload_ms: 0,
                    stance: stance_key(TEST_SWORD_STANCE),
                    block_stamina_damage: 0.0,
                    block_stability: 0.0,
                    ranged_block: None,
                },
            )],
            attacks: test_attack_rows(&rows.damage_table)
                .into_iter()
                .chain([test_mob_row()])
                .collect(),

            player_health: vec![(rows.player_vitals_id.clone(), TEST_PLAYER_HEALTH)],
            item_stats: vec![(TEST_RANGED_ITEM.to_owned(), TEST_RANGED_WEAPON.to_owned())],
            ranged_profiles: vec![(
                TEST_RANGED_WEAPON.to_owned(),
                crate::mob_ai::test_support::TEST_RANGED_PROFILE.to_owned(),
            )],
            ..CombatSource::default()
        }
    }

    /// The row of the test mob's damage window, of coefficient 1 (`mob_ai::test_support`).
    fn test_mob_row() -> AttackRow {
        AttackRow {
            damage_id: crate::mob_ai::test_support::TEST_MOB_ROW.to_owned(),
            coefficient: 1.0,
            can_crit: false,
            threat: 0,
            damage_type: None,
            table: crate::mob_ai::test_support::TEST_MOB_TABLE.to_owned(),
            block_stamina_mod: 0.0,
            blockable: true,
            category: DamageCategory::Physical,
            affix: None,
        }
    }

    /// A row of coefficient 1 in `table` for the damage row of every basic attack of the retail
    /// stances.
    fn test_attack_rows(table: &str) -> Vec<AttackRow> {
        let stances: Vec<nw_content::player_attacks::Stance> =
            serde_json::from_str(TEST_PLAYER_ATTACKS).expect("player attack table");
        let mut ids: Vec<String> = stances
            .iter()
            .flat_map(|stance| {
                [AttackKind::Light, AttackKind::Heavy, AttackKind::Charged]
                    .into_iter()
                    .filter_map(|kind| Some(stance.attack(kind)?.damage_row.clone()))
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .map(|damage_id| AttackRow {
                damage_id,
                coefficient: 1.0,
                can_crit: false,
                threat: 0,
                damage_type: None,
                table: table.to_owned(),
                block_stamina_mod: 0.0,
                blockable: true,
                category: DamageCategory::Physical,
                affix: None,
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::test_support::test_data;
    use super::*;
    use crate::config::tests::committed;

    /// The committed config's combat settings on the test rows.
    pub(crate) fn test_combat() -> CombatSettings {
        committed().combat_settings(Arc::new(test_data()), Arc::from([]), test_mob_ai())
    }

    /// The test AI: the test mob type runs the test tree (`mob_ai::test_support`).
    pub(crate) fn test_mob_ai() -> Arc<crate::mob_ai::MobAi> {
        Arc::new(crate::mob_ai::test_support::test_ai(&[
            super::test_support::TEST_MOB,
        ]))
    }

    use nw_content::player_attacks::{AttackHit, AttackKind, BasicAttack, Stance, stance_key};

    fn stance(tags: &str) -> Stance {
        super::test_support::test_player_attacks()
            .stance(stance_key(tags))
            .cloned()
            .unwrap_or_else(|| panic!("stance {tags}"))
    }

    fn attack(tags: &str, kind: AttackKind) -> BasicAttack {
        stance(tags)
            .attack(kind)
            .cloned()
            .unwrap_or_else(|| panic!("{tags} {kind:?}"))
    }

    fn window(tags: &str, kind: AttackKind) -> [u64; 2] {
        match attack(tags, kind).hit {
            AttackHit::Melee { window_ms, .. } => window_ms,
            hit => panic!("{tags} {kind:?}: {hit:?}"),
        }
    }

    #[test]
    fn the_retail_table_has_the_damage_windows_of_the_attach_databases() {
        // `playermaleanims_{1h,2h}_meleeattach.adb`, `CAGE-Damage` clips (combat audit).
        assert_eq!(window("1H_Melee,Sword", AttackKind::Light), [333, 433]);
        assert_eq!(window("1H_Melee,Flail", AttackKind::Light), [167, 233]);
        assert_eq!(window("2H_Melee,GreatAxe", AttackKind::Heavy), [433, 567]);
        assert_eq!(window("2H_Melee,GreatSword", AttackKind::Heavy), [233, 333]);
        assert_eq!(window("1H_Melee,Rapier", AttackKind::Charged), [200, 500]);
        assert_eq!(window("2H_Melee,Warhammer", AttackKind::Light), [400, 533]);
        assert_eq!(
            attack("2H_Melee,GreatSword", AttackKind::Charged).damage_row,
            "GreatSword_HeavyAttack1"
        );
        assert_eq!(
            attack("1H_Melee,Sword", AttackKind::Charged).damage_row,
            "1H_Sword_Attack2_Charged"
        );
        // The homing of the greatsword's light attack (`player_greatsword.adb`).
        let AttackHit::Melee {
            homing: Some(homing),
            ..
        } = attack("2H_Melee,GreatSword", AttackKind::Light).hit
        else {
            panic!("a homing melee attack");
        };
        assert_eq!(
            (homing.radius_m, homing.max_angle_deg, homing.height_m),
            (4.5, 60.0, 5.0)
        );
    }

    #[test]
    fn the_retail_table_has_the_holds_dodges_and_uses_of_the_player_databases() {
        // `Attack_Start_Charge` of `player_<weapon>.adb`: heavy from 333 ms, charged at 667 ms;
        // the flail charges at 500 ms.
        let sword = stance("1H_Melee,Sword");
        assert_eq!(
            (sword.heavy_from_ms, sword.charged_at_ms),
            (Some(333), Some(667))
        );
        assert_eq!(stance("1H_Melee,Flail").charged_at_ms, Some(500));
        // `Dodge_Roll` `Invulnerability` 0-500 ms; `Use` `UsePaperdollItem`: a potion at 333 ms,
        // a drink, food and a bandage at 1000 ms.
        assert_eq!(sword.dodge_invulnerable_ms, Some([0, 500]));
        assert_eq!(sword.use_effect_ms("Potion"), Some(333));
        for tag in ["Drink", "Eat", "Bandage"] {
            assert_eq!(sword.use_effect_ms(tag), Some(1_000), "{tag}");
        }
    }

    #[test]
    fn the_retail_table_has_the_ranged_attacks_of_the_database_and_slices() {
        // `rangedattackdatabase.radb` and the projectile slices.
        let AttackHit::Projectile {
            fire_ms,
            range_m,
            radius_m,
            flight,
        } = attack("Bow,LongBow", AttackKind::Light).hit
        else {
            panic!("an arrow");
        };
        assert_eq!((fire_ms, range_m, radius_m), (400, 100.0, 0.15));
        assert_eq!(
            (
                flight.base_speed_mps,
                flight.gravity_mps2,
                flight.lifetime_s
            ),
            (100.0, 9.8, 10.0)
        );
        let AttackHit::Projectile {
            range_m, flight, ..
        } = attack("2H_Ranged,Rifle", AttackKind::Light).hit
        else {
            panic!("a musket shot");
        };
        assert_eq!((range_m, flight.base_speed_mps), (125.0, 250.0));
        assert_eq!(
            attack("2H_Ranged,Blunderbuss", AttackKind::Light).hit,
            AttackHit::HitScan {
                fire_ms: 200,
                range_m: 14.0
            }
        );
        let AttackHit::Projectile {
            range_m, radius_m, ..
        } = attack("MagicGauntlet,Ice", AttackKind::Light).hit
        else {
            panic!("an ice bolt");
        };
        assert_eq!((range_m, radius_m), (50.0, 0.15));
        assert_eq!(
            attack("MagicStaff,Fire", AttackKind::Light).damage_row,
            "Firestaff_PrimaryAttackC1"
        );
    }
}
