// SPDX-License-Identifier: AGPL-3.0-or-later
//! The progression of a character besides its items and quests (world migrations 0006 to
//! 0008, 0010 for its travel state, 0012 for its mounts, 0013 for its ability slots and meta
//! achievements, 0024 for its recipe cooldowns and 0025 for its faction): its experience, coins,
//! achievement bits, loot limit counts, weapon masteries, spent points, travel state, mounts,
//! mapped ability slots, meta achievements, recipe cooldown counts and faction. `sim` saves
//! it; the director loads it at a join.
//!
//! Each save carries the time its state was taken; an older state never overwrites a newer one.

use std::collections::BTreeMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::arena::{SavedArena, character_arena, save_character_arena};
use crate::error::DbError;
use crate::faction::{SavedFaction, character_faction, save_character_faction};
use crate::meta_achievements::{SavedMeta, SavedSlot, character_meta, save_character_meta};
use crate::mounts::{SavedMounts, character_mounts, save_character_mounts};
use crate::music::{SavedMusic, character_music, save_character_music};
use crate::notifications::{
    SavedNotifications, character_notifications, save_character_notifications,
};
use crate::position::SaveOutcome;
use crate::recipe_cooldowns::{
    SavedRecipeCooldown, character_recipe_cooldowns, save_character_recipe_cooldowns,
};
use crate::travel::{SavedTravel, character_travel, save_character_travel};

/// Milliseconds per second, for the stored time.
const MILLIS_PER_SECOND: f64 = 1000.0;
/// The column of the achievement bits.
const ACHIEVEMENTS: &str = "achievements";

/// One loot limit's count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLootLimit {
    /// The lowercase `LootLimitID`.
    pub limit_id: String,
    /// The count.
    pub count: u64,
    /// Unix nanoseconds of its last count.
    pub counted_ns: u64,
    /// Unix nanoseconds when it goes back to 0.
    pub ends_ns: u64,
}

/// A character's progression.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedProgression {
    /// Experience points.
    pub experience: u32,
    /// Coins.
    pub coins: u64,
    /// The set achievement bits, ascending.
    pub achievements: Vec<u32>,
    /// The loot limit counts, by limit id.
    pub loot_limits: Vec<SavedLootLimit>,
    /// The total experience of each weapon mastery, by its `CategoricalProgressionID`.
    pub masteries: BTreeMap<String, u32>,
    /// The attribute points spent on each attribute (`CharacterAttributeType` id, points), in
    /// the order they were first spent.
    pub attributes: Vec<(u32, u32)>,
    /// The level of each weapon progression point (`ProgressionPointId`, level), in the order
    /// they were first assigned.
    pub weapon_points: Vec<(String, u16)>,
    /// The territory standing points spent: pool and point (both CRC-32 wire ids) and the
    /// level, in the order they were first spent (world migration 0021).
    pub territory_points: Vec<(u32, u32, u16)>,
    /// The travel and housing state (world migration 0010).
    pub travel: SavedTravel,
    /// The chosen mount and the dyed or named mounts (world migration 0012).
    pub mounts: SavedMounts,
    /// The mapped ability slots: the weapon mastery of the tree, the slot (0 to 2) and the
    /// ability's `ProgressionPointId` (world migration 0013).
    pub ability_slots: Vec<SavedSlot>,
    /// The meta achievement counters, by lowercase key (world migration 0013).
    pub meta_counters: BTreeMap<String, u64>,
    /// The completed meta achievements in completion order, each with whether its reward
    /// waits (world migration 0013).
    pub meta_completed: Vec<(String, bool)>,
    /// The title it shows (CRC-32 of the lowercase `TitleID`; world migration 0013).
    pub title: Option<u32>,
    /// The expedition, raid and trial state (world migration 0015).
    pub arena: SavedArena,
    /// The musical performance lockout (world migration 0018).
    pub music: SavedMusic,
    /// How often each event notification was shown (world migration 0022).
    pub notifications: SavedNotifications,
    /// The recipe cooldown counts, by recipe (world migration 0024).
    pub recipe_cooldowns: Vec<SavedRecipeCooldown>,
    /// The faction (world migration 0025).
    pub faction: SavedFaction,
}

type ProgressionRow = (bool, i64, i64, Vec<i32>);
type LootLimitRow = (String, i64, i64, i64);
type MasteryRow = (String, i64);
type AttributeRow = (i16, i64);
type WeaponPointRow = (String, i32);
type TerritoryPointRow = (i64, i64, i32);

/// A stored value as the program's type.
pub(crate) fn from_column<T: TryFrom<i64>>(value: i64, field: &'static str) -> Result<T, DbError> {
    T::try_from(value).map_err(|_| DbError::OutOfRange { field })
}

/// A program value as its column.
pub(crate) fn to_column<T: TryInto<i64>>(value: T, field: &'static str) -> Result<i64, DbError> {
    value.try_into().map_err(|_| DbError::OutOfRange { field })
}

/// A program value as its `SMALLINT` column.
fn small_column<T: TryInto<i16>>(value: T, field: &'static str) -> Result<i16, DbError> {
    value.try_into().map_err(|_| DbError::OutOfRange { field })
}

/// The saved progression of a character: `Ok(None)` when it exists and never saved one.
///
/// # Errors
///
/// Returns [`DbError::NoSuchCharacter`] when the world has no such character, and
/// [`DbError::OutOfRange`] for a stored value the program's types do not hold.
pub async fn character_progression(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Option<SavedProgression>, DbError> {
    let row: Option<ProgressionRow> = sqlx::query_as(
        "SELECT progression_saved_at IS NOT NULL, experience, coins, achievements \
         FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (ever_saved, experience, coins, achievements) = row.ok_or(DbError::NoSuchCharacter)?;
    if !ever_saved {
        return Ok(None);
    }
    let limits: Vec<LootLimitRow> = sqlx::query_as(
        "SELECT limit_id, count, counted_ns, ends_ns FROM character_loot_limit \
         WHERE character_id = $1 ORDER BY limit_id",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let masteries: Vec<MasteryRow> = sqlx::query_as(
        "SELECT category, experience FROM character_mastery WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let attributes: Vec<AttributeRow> = sqlx::query_as(
        "SELECT attribute, points FROM character_attribute WHERE character_id = $1 \
         ORDER BY position",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let weapon_points: Vec<WeaponPointRow> = sqlx::query_as(
        "SELECT point_id, level FROM character_weapon_point WHERE character_id = $1 \
         ORDER BY position",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let territory_points: Vec<TerritoryPointRow> = sqlx::query_as(
        "SELECT pool_id, point_id, level FROM character_territory_point \
         WHERE character_id = $1 ORDER BY position",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let travel = character_travel(world, character_id).await?;
    let mounts = character_mounts(world, character_id).await?;
    let meta = character_meta(world, character_id).await?;
    let arena = character_arena(world, character_id).await?;
    let music = character_music(world, character_id).await?;
    let notifications = character_notifications(world, character_id).await?;
    let recipe_cooldowns = character_recipe_cooldowns(world, character_id).await?;
    let faction = character_faction(world, character_id).await?;
    Ok(Some(SavedProgression {
        experience: from_column(experience, "experience")?,
        coins: from_column(coins, "coins")?,
        achievements: achievements
            .into_iter()
            .map(|bit| from_column(i64::from(bit), ACHIEVEMENTS))
            .collect::<Result<_, _>>()?,
        loot_limits: limits
            .into_iter()
            .map(|(limit_id, count, counted_ns, ends_ns)| {
                Ok(SavedLootLimit {
                    limit_id,
                    count: from_column(count, "count")?,
                    counted_ns: from_column(counted_ns, "counted_ns")?,
                    ends_ns: from_column(ends_ns, "ends_ns")?,
                })
            })
            .collect::<Result<_, DbError>>()?,
        masteries: masteries
            .into_iter()
            .map(|(category, experience)| Ok((category, from_column(experience, "mastery")?)))
            .collect::<Result<_, DbError>>()?,
        attributes: attributes
            .into_iter()
            .map(|(attribute, points)| {
                Ok((
                    from_column(i64::from(attribute), "attribute")?,
                    from_column(points, "attribute points")?,
                ))
            })
            .collect::<Result<_, DbError>>()?,
        weapon_points: weapon_points
            .into_iter()
            .map(|(point, level)| Ok((point, from_column(i64::from(level), "weapon point")?)))
            .collect::<Result<_, DbError>>()?,
        territory_points: territory_points
            .into_iter()
            .map(|(pool, point, level)| {
                Ok((
                    from_column(pool, "territory pool")?,
                    from_column(point, "territory point")?,
                    from_column(i64::from(level), "territory point level")?,
                ))
            })
            .collect::<Result<_, DbError>>()?,
        travel,
        mounts,
        ability_slots: meta.ability_slots,
        meta_counters: meta.counters,
        meta_completed: meta.completed,
        title: meta.title,
        arena,
        music,
        notifications,
        recipe_cooldowns,
        faction,
    }))
}

/// Saves `progression` as the whole progression of a character, taken at `taken_at_ms` (Unix
/// ms), when no state taken at the same time or later is stored.
///
/// # Errors
///
/// Returns [`DbError::NoSuchCharacter`] when the world has no such character,
/// [`DbError::OutOfRange`] for a value its column cannot hold, and [`DbError::Database`] for a
/// row a table constraint refuses (two counts of one limit, an id that is not lowercase).
pub async fn save_character_progression(
    world: &PgPool,
    character_id: Uuid,
    progression: &SavedProgression,
    taken_at_ms: u64,
) -> Result<SaveOutcome, DbError> {
    let coins = to_column(progression.coins, "coins")?;
    let achievements = progression
        .achievements
        .iter()
        .map(|bit| {
            i32::try_from(*bit).map_err(|_| DbError::OutOfRange {
                field: ACHIEVEMENTS,
            })
        })
        .collect::<Result<Vec<i32>, _>>()?;
    let limits = progression
        .loot_limits
        .iter()
        .map(|limit| {
            Ok((
                limit.limit_id.as_str(),
                to_column(limit.count, "count")?,
                to_column(limit.counted_ns, "counted_ns")?,
                to_column(limit.ends_ns, "ends_ns")?,
            ))
        })
        .collect::<Result<Vec<_>, DbError>>()?;
    let taken_at_seconds = taken_at_ms as f64 / MILLIS_PER_SECOND;
    let mut transaction = world.begin().await?;
    let updated = sqlx::query(
        "UPDATE player_character SET progression_saved_at = to_timestamp($2), \
         experience = $3, coins = $4, achievements = $5 \
         WHERE character_id = $1 \
         AND (progression_saved_at IS NULL OR progression_saved_at < to_timestamp($2))",
    )
    .bind(character_id)
    .bind(taken_at_seconds)
    .bind(i64::from(progression.experience))
    .bind(coins)
    .bind(&achievements)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 0 {
        let exists: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM player_character WHERE character_id = $1")
                .bind(character_id)
                .fetch_optional(&mut *transaction)
                .await?;
        return exists
            .map(|_| SaveOutcome::Older)
            .ok_or(DbError::NoSuchCharacter);
    }
    sqlx::query("DELETE FROM character_loot_limit WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut *transaction)
        .await?;
    for (limit_id, count, counted_ns, ends_ns) in limits {
        sqlx::query(
            "INSERT INTO character_loot_limit (character_id, limit_id, count, counted_ns, ends_ns) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(character_id)
        .bind(limit_id)
        .bind(count)
        .bind(counted_ns)
        .bind(ends_ns)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM character_mastery WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut *transaction)
        .await?;
    for (category, experience) in &progression.masteries {
        sqlx::query(
            "INSERT INTO character_mastery (character_id, category, experience) \
             VALUES ($1, $2, $3)",
        )
        .bind(character_id)
        .bind(category)
        .bind(i64::from(*experience))
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM character_attribute WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut *transaction)
        .await?;
    for (position, (attribute, points)) in progression.attributes.iter().enumerate() {
        sqlx::query(
            "INSERT INTO character_attribute (character_id, attribute, points, position) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(character_id)
        .bind(small_column(*attribute, "attribute")?)
        .bind(i64::from(*points))
        .bind(small_column(position, "attribute position")?)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM character_weapon_point WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut *transaction)
        .await?;
    for (position, (point, level)) in progression.weapon_points.iter().enumerate() {
        sqlx::query(
            "INSERT INTO character_weapon_point (character_id, point_id, level, position) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(character_id)
        .bind(point)
        .bind(i32::from(*level))
        .bind(i32::try_from(position).map_err(|_| DbError::OutOfRange {
            field: "weapon point position",
        })?)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM character_territory_point WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut *transaction)
        .await?;
    for (position, (pool, point, level)) in progression.territory_points.iter().enumerate() {
        sqlx::query(
            "INSERT INTO character_territory_point \
             (character_id, pool_id, point_id, level, position) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(character_id)
        .bind(i64::from(*pool))
        .bind(i64::from(*point))
        .bind(i32::from(*level))
        .bind(i32::try_from(position).map_err(|_| DbError::OutOfRange {
            field: "territory point position",
        })?)
        .execute(&mut *transaction)
        .await?;
    }
    save_character_travel(&mut transaction, character_id, &progression.travel).await?;
    save_character_mounts(&mut transaction, character_id, &progression.mounts).await?;
    let meta = SavedMeta {
        ability_slots: progression.ability_slots.clone(),
        counters: progression.meta_counters.clone(),
        completed: progression.meta_completed.clone(),
        title: progression.title,
    };
    save_character_meta(&mut transaction, character_id, &meta).await?;
    save_character_arena(&mut transaction, character_id, &progression.arena).await?;
    save_character_music(&mut transaction, character_id, &progression.music).await?;
    save_character_notifications(&mut transaction, character_id, &progression.notifications)
        .await?;
    save_character_recipe_cooldowns(
        &mut transaction,
        character_id,
        &progression.recipe_cooldowns,
    )
    .await?;
    save_character_faction(&mut transaction, character_id, &progression.faction).await?;
    transaction.commit().await?;
    Ok(SaveOutcome::Saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_out_of_a_column_range_are_refused() {
        assert_eq!(to_column(u64::MAX, "coins").ok(), None);
        assert_eq!(to_column(375_u64, "coins").ok(), Some(375));
        assert!(matches!(
            from_column::<u32>(i64::from(u32::MAX) + 1, "experience"),
            Err(DbError::OutOfRange {
                field: "experience"
            })
        ));
        assert_eq!(from_column::<u64>(1, "count").ok(), Some(1));
        assert_eq!(small_column(4_u32, "attribute").ok(), Some(4));
        assert!(matches!(
            small_column(usize::MAX, "attribute position"),
            Err(DbError::OutOfRange {
                field: "attribute position"
            })
        ));
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use super::*;
    use crate::migrate::WORLD_MIGRATOR;
    use crate::test_db::with_test_schema;
    use crate::{NewCharacter, insert_character};

    /// 2027-01-01, with milliseconds.
    const TAKEN_AT_MS: u64 = 1_798_761_600_123;
    const LATER_MS: u64 = 1_000;

    /// The tutorial's experience and achievements, coins of a quest reward, and the retail
    /// newchar loot limit count.
    fn progression() -> SavedProgression {
        SavedProgression {
            experience: 1_250,
            coins: 375,
            achievements: vec![14_809, 15_317],
            loot_limits: vec![
                SavedLootLimit {
                    limit_id: "2hwarhammer_ftue_corruptedsailer_v2".to_owned(),
                    count: 1,
                    counted_ns: 1_793_091_600_894_621_000,
                    ends_ns: 1_793_091_600_894_621_000,
                },
                SavedLootLimit {
                    limit_id: "gypsumgreen".to_owned(),
                    count: 32,
                    counted_ns: 1_792_696_951_894_621_000,
                    ends_ns: 1_792_746_000_894_621_000,
                },
            ],
            masteries: BTreeMap::from([
                ("GreatSwordAbilityTable".to_owned(), 20),
                ("BlunderbussAbilityTable".to_owned(), 4),
            ]),
            // Retail newchar 2026-09-18: Strength first, then Constitution.
            attributes: vec![(2, 25), (0, 5)],
            weapon_points: vec![
                ("Ability_Greatsword_Combo".to_owned(), 1),
                ("Upgrade_Greatsword_Combo_1".to_owned(), 1),
            ],
            territory_points: vec![(0xd65a_1577, 0x1234_5678, 3)],
            travel: travel(),
            mounts: mounts(),
            ability_slots: vec![(
                "GreatSwordAbilityTable".to_owned(),
                0,
                "Ability_Greatsword_Combo".to_owned(),
            )],
            meta_counters: BTreeMap::from([("pve_familykills_corrupted".to_owned(), 3)]),
            meta_completed: vec![
                ("SkillLevel_Fishing2".to_owned(), false),
                ("AchievementsEarnedCount3".to_owned(), true),
            ],
            title: Some(0x431d_f4ea),
            arena: arena(),
            music: SavedMusic {
                next_performance_ms: Some(846_118_000_000),
            },
            notifications: SavedNotifications {
                shown: vec![(0x1e06_e356, 12)],
            },
            // The retail refining ledger's `LeatherT51` entry.
            recipe_cooldowns: vec![crate::SavedRecipeCooldown {
                recipe_id: "leathert51".to_owned(),
                count: 10,
                ends_ns: 0x18b8_d351_b144_f000,
            }],
            // The retail `Change-Faction` ledger's change to the Syndicate.
            faction: SavedFaction {
                faction: 1,
                changes: 1,
                changed_ns: 0x18e1_bb69_e04f_ba64,
            },
        }
    }

    /// The arena state of a character inside the Amrine expedition (retail counters of one
    /// entry, the daily and weekly refresh times of a 03:00 UTC server).
    fn arena() -> SavedArena {
        SavedArena {
            dungeon_cooldown_since_2000_ns: 834_685_420_779_850_554,
            solo_trial_cooldown_since_2000_ns: 0,
            ranks: vec![3, 3, 3],
            last_dungeon: Some([0x2d; crate::DUNGEON_ID_LEN]),
            last_mutated_dungeon: Some([0x7d; crate::DUNGEON_ID_LEN]),
            base_entered: 1,
            mutated_entered: 0,
            group_trials_entered: 0,
            next_base_refresh_ns: 1_781_406_000_122_895_400,
            next_mutated_refresh_ns: 1_781_578_800_998_609_500,
            next_group_trial_refresh_ns: 1_781_406_000_122_932_600,
            mutation_award: true,
            last_dungeon_mutated: true,
            stay: Some(crate::SavedStay {
                instance: Uuid::from_u128(0x2d30),
                game_mode: "DungeonAmrine".to_owned(),
                phase: u64::MAX,
                return_level: "newworld_vitaeeterna".to_owned(),
                return_position: [7_700.5_f32.to_bits(), 7_300.25_f32.to_bits(), 1],
                activated_ns: 1_781_404_200_000_000_000,
            }),
        }
    }

    /// A chosen mount and two customized mounts: a dye on the open channel of a basic lion
    /// (`Mount_MTX_5_lion_basic`) and a named, dyed bear (`Mount_MTX_1_bear_Preorder`).
    fn mounts() -> SavedMounts {
        SavedMounts {
            selected: Some(0x0c13_84ae),
            customized: vec![
                crate::SavedMount {
                    mount_id: 0x0c13_84ae,
                    dye: [0, 0, 0x33, 0],
                    name: Vec::new(),
                },
                crate::SavedMount {
                    mount_id: 0xde1f_a300,
                    dye: [0x19, 0x25, 0x19, 0x25],
                    name: b"Rex".to_vec(),
                },
            ],
        }
    }

    /// A travel state with every field set: a marker, an inn and a respawn camp of the retail
    /// captures, and one home with two pieces of furniture.
    fn travel() -> SavedTravel {
        let position = [
            6_061.9_f32.to_bits(),
            9_856.0_f32.to_bits(),
            324.9_f32.to_bits(),
        ];
        let rotation = [0, 0, 0.967_457_f32.to_bits(), 0.255_766_f32.to_bits()];
        SavedTravel {
            waypoint: Some(position),
            inn: Some(17_680_737_194_352_937_251),
            camp: Some(16_695_474_225_659_806_323),
            camp_skin: Some(0x3e89_166b),
            unstuck_ready_ms: 1_790_860_734_747,
            homes: vec![crate::SavedHome {
                plot: [
                    0xc7, 0x37, 0x66, 0xcd, 0xa0, 0x43, 0x41, 0x6a, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
                position,
                purchased_ms: 1_782_815_151_259,
                items: vec![
                    crate::SavedHousingItem {
                        index: 24,
                        item: 0xb39e,
                        position,
                        rotation,
                    },
                    crate::SavedHousingItem {
                        index: 25,
                        item: 0xb39e,
                        position,
                        rotation,
                    },
                ],
                record: crate::SavedHomeRecord {
                    plot_facet: 0x6a41_43a0_cd66_37c7,
                    plot_extra: 0x99ed_e7e1_d5a5_b24d,
                    house_data: [9; crate::PLOT_ID_LEN],
                    house_data_facet: 0xc13e_b7f0_5396_3b4c,
                    house_type: 0xd0e4_0188,
                    territory: 16,
                    taxes_due_ms: 1_783_419_951_259,
                    fast_travel_ready_ms: 1_782_829_551_259,
                },
            }],
            played_ms: 448_201,
            inn_recalled_ms: 1_790_860_000_000,
            placed_camp: Some(crate::SavedCamp {
                position,
                tier: 2,
                placed_ms: 1_790_861_000_000,
            }),
        }
    }

    async fn new_character(pool: &PgPool) -> Uuid {
        insert_character(
            pool,
            &NewCharacter {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                name: "Ada".to_owned(),
                appearance: serde_json::json!({}),
            },
        )
        .await
        .expect("insert")
        .id
    }

    #[tokio::test]
    async fn progression_round_trips_and_an_older_state_loses() {
        let (before, after, older, cleared, missing, too_rich) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let character = new_character(pool).await;
            let before = character_progression(pool, character).await.expect("load");
            save_character_progression(pool, character, &progression(), TAKEN_AT_MS)
                .await
                .expect("save");
            let after = character_progression(pool, character).await.expect("load");
            let older = save_character_progression(
                pool,
                character,
                &SavedProgression::default(),
                TAKEN_AT_MS,
            )
            .await
            .expect("save");
            save_character_progression(
                pool,
                character,
                &SavedProgression::default(),
                TAKEN_AT_MS + LATER_MS,
            )
            .await
            .expect("save");
            let cleared = character_progression(pool, character).await.expect("load");
            let missing = character_progression(pool, Uuid::new_v4()).await;
            let too_rich = save_character_progression(
                pool,
                character,
                &SavedProgression {
                    coins: u64::MAX,
                    ..progression()
                },
                TAKEN_AT_MS + LATER_MS + LATER_MS,
            )
            .await;
            (before, after, older, cleared, missing, too_rich)
        })
        .await;
        assert_eq!(before, None, "a new character has saved no progression");
        assert_eq!(after, Some(progression()));
        assert_eq!(older, SaveOutcome::Older);
        assert_eq!(cleared, Some(SavedProgression::default()));
        assert!(matches!(missing, Err(DbError::NoSuchCharacter)));
        assert!(matches!(
            too_rich,
            Err(DbError::OutOfRange { field: "coins" })
        ));
    }

    #[tokio::test]
    async fn the_migration_adds_the_columns_with_their_checks() {
        let (defaults, negative, upper_id) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let character = new_character(pool).await;
            let defaults: (Option<f64>, i64, i64, Vec<i32>) = sqlx::query_as(
                "SELECT extract(epoch FROM progression_saved_at)::float8, experience, coins, \
                 achievements FROM player_character WHERE character_id = $1",
            )
            .bind(character)
            .fetch_one(pool)
            .await
            .expect("row");
            let negative = sqlx::query("UPDATE player_character SET coins = -1")
                .execute(pool)
                .await;
            let upper_id =
                sqlx::query("INSERT INTO character_loot_limit VALUES ($1, 'GypsumGreen', 1, 0, 0)")
                    .bind(character)
                    .execute(pool)
                    .await;
            (defaults, negative, upper_id)
        })
        .await;
        assert_eq!(defaults, (None, 0, 0, Vec::new()));
        assert!(negative.is_err(), "coins are never negative");
        assert!(upper_id.is_err(), "limit ids are stored lowercase");
    }
}
