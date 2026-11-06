// SPDX-License-Identifier: AGPL-3.0-or-later
//! Character names (global database) and characters (world databases), M2 spec section 7.
//! `nw-login` checks names, reserves the name first, then inserts the world row, and releases
//! the name when the insert fails.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, is_unique_violation};
use crate::social_graph::{LEVEL_FIELD, narrow};
use crate::{Character, NewCharacter};

const CHARACTER_NAME_PKEY: &str = "character_name_pkey";

type CharacterRow = (
    Uuid,
    Uuid,
    String,
    serde_json::Value,
    i64,
    i64,
    Option<Vec<i32>>,
);

/// The level is the one the social save keeps from the character's last time online.
const CHARACTER_COLUMNS: &str = "character_id, account_id, name, appearance, \
     CAST(EXTRACT(EPOCH FROM created_at) AS BIGINT), \
     COALESCE((SELECT social.level FROM character_social social \
               WHERE social.character_id = player_character.character_id), 0), \
     published_look";

fn character_from_row(
    (id, account_id, name, appearance, created_at_unix, level, published_look): CharacterRow,
) -> Result<Character, DbError> {
    Ok(Character {
        id,
        account_id,
        name,
        appearance,
        created_at_unix,
        level: narrow(level, LEVEL_FIELD)?,
        published_look: published_look.map(|look| look.into_iter().map(crc_of_column).collect()),
    })
}

/// A CRC-32 stored as its signed 32-bit bit pattern.
#[allow(
    clippy::cast_sign_loss,
    reason = "the column keeps the CRC's bit pattern"
)]
const fn crc_of_column(value: i32) -> u32 {
    value as u32
}

/// A CRC-32 as its signed 32-bit bit pattern.
#[allow(
    clippy::cast_possible_wrap,
    reason = "the column keeps the CRC's bit pattern"
)]
const fn column_of_crc(crc: u32) -> i32 {
    crc as i32
}

/// Saves the armor look the character select screen shows for `character_id`
/// ([`Character::published_look`]).
pub async fn save_published_look(
    world: &PgPool,
    character_id: Uuid,
    look: &[u32],
) -> Result<(), DbError> {
    let column: Vec<i32> = look.iter().copied().map(column_of_crc).collect();
    sqlx::query("UPDATE player_character SET published_look = $2 WHERE character_id = $1")
        .bind(character_id)
        .bind(column)
        .execute(world)
        .await?;
    Ok(())
}

/// Reserves `name` (compared in lower case) for a character in `world_id`.
pub async fn reserve_character_name(
    global: &PgPool,
    name: &str,
    world_id: Uuid,
    character_id: Uuid,
) -> Result<(), DbError> {
    let inserted = sqlx::query(
        "INSERT INTO character_name (name_key, world_id, character_id) VALUES ($1, $2, $3)",
    )
    .bind(name.to_lowercase())
    .bind(world_id)
    .bind(character_id)
    .execute(global)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error, CHARACTER_NAME_PKEY) => {
            Err(DbError::DuplicateCharacterName)
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether `name` (compared in lower case) belongs to a character in any world.
pub async fn character_name_taken(global: &PgPool, name: &str) -> Result<bool, DbError> {
    let taken: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM character_name WHERE name_key = $1)")
            .bind(name.to_lowercase())
            .fetch_one(global)
            .await?;
    Ok(taken)
}

/// Frees the name of a character.
pub async fn release_character_name(global: &PgPool, character_id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM character_name WHERE character_id = $1")
        .bind(character_id)
        .execute(global)
        .await?;
    Ok(())
}

/// Inserts a character into a world database.
pub async fn insert_character(world: &PgPool, new: &NewCharacter) -> Result<Character, DbError> {
    let row: CharacterRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO player_character (character_id, account_id, name, appearance) \
         VALUES ($1, $2, $3, $4) RETURNING {CHARACTER_COLUMNS}"
    )))
    .bind(new.id)
    .bind(new.account_id)
    .bind(&new.name)
    .bind(&new.appearance)
    .fetch_one(world)
    .await?;
    character_from_row(row)
}

/// Deletes the character `character_id` of `account_id` from a world database, with its saved
/// position. Returns whether the account had that character.
pub async fn delete_character(
    world: &PgPool,
    account_id: Uuid,
    character_id: Uuid,
) -> Result<bool, DbError> {
    let deleted =
        sqlx::query("DELETE FROM player_character WHERE character_id = $1 AND account_id = $2")
            .bind(character_id)
            .bind(account_id)
            .execute(world)
            .await?;
    Ok(deleted.rows_affected() > 0)
}

/// The characters of one account in one world, oldest first.
pub async fn characters_for_account(
    world: &PgPool,
    account_id: Uuid,
) -> Result<Vec<Character>, DbError> {
    let rows: Vec<CharacterRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {CHARACTER_COLUMNS} FROM player_character \
         WHERE account_id = $1 ORDER BY created_at, character_id"
    )))
    .bind(account_id)
    .fetch_all(world)
    .await?;
    rows.into_iter().map(character_from_row).collect()
}

/// One character of a world database.
pub async fn character_by_id(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Option<Character>, DbError> {
    let row: Option<CharacterRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {CHARACTER_COLUMNS} FROM player_character WHERE character_id = $1"
    )))
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    row.map(character_from_row).transpose()
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use crate::migrate::{GLOBAL_MIGRATOR, WORLD_MIGRATOR};
    use crate::test_db::with_test_schema;

    const NAME: &str = "Ada";

    fn new_character(account_id: Uuid) -> NewCharacter {
        NewCharacter {
            id: Uuid::new_v4(),
            account_id,
            name: NAME.to_owned(),
            appearance: serde_json::json!({"face": 3}),
        }
    }

    #[tokio::test]
    async fn names_are_unique_over_worlds_and_case() {
        let (first, other_world, other_case, after_release) = with_test_schema(async |pool| {
            GLOBAL_MIGRATOR.run(pool).await.expect("migrate");
            assert!(!character_name_taken(pool, NAME).await.expect("free"));
            let id = Uuid::new_v4();
            let first = reserve_character_name(pool, NAME, Uuid::new_v4(), id).await;
            assert!(character_name_taken(pool, "ADA").await.expect("taken"));
            let other_world =
                reserve_character_name(pool, NAME, Uuid::new_v4(), Uuid::new_v4()).await;
            let other_case =
                reserve_character_name(pool, "ADA", Uuid::new_v4(), Uuid::new_v4()).await;
            release_character_name(pool, id).await.expect("release");
            let after_release =
                reserve_character_name(pool, NAME, Uuid::new_v4(), Uuid::new_v4()).await;
            (first, other_world, other_case, after_release)
        })
        .await;
        assert!(first.is_ok());
        assert!(matches!(other_world, Err(DbError::DuplicateCharacterName)));
        assert!(matches!(other_case, Err(DbError::DuplicateCharacterName)));
        assert!(after_release.is_ok());
    }

    #[tokio::test]
    async fn characters_are_listed_per_account() {
        let (inserted, listed, other, loaded) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let account = Uuid::new_v4();
            let inserted = insert_character(pool, &new_character(account))
                .await
                .expect("insert");
            let listed = characters_for_account(pool, account).await.expect("list");
            let other = characters_for_account(pool, Uuid::new_v4())
                .await
                .expect("other");
            let loaded = character_by_id(pool, inserted.id).await.expect("load");
            (inserted, listed, other, loaded)
        })
        .await;
        assert_eq!(inserted.name, NAME);
        assert_eq!(inserted.level, 0, "a new character has no saved level");
        assert_eq!(inserted.appearance, serde_json::json!({"face": 3}));
        assert_eq!(listed, vec![inserted.clone()]);
        assert!(other.is_empty());
        assert_eq!(loaded, Some(inserted));
    }

    #[tokio::test]
    async fn the_published_look_is_saved_with_the_crc_bit_patterns() {
        /// `m_Archetype_Sellsword_gloves` and `m_DynastyWarriorHeavy_Chest`: one above i32::MAX.
        const LOOK: [u32; 5] = [0, 0x49ee_1aec, 0xc3d5_0a8f, 0, 0];
        let (before, after) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let inserted = insert_character(pool, &new_character(Uuid::new_v4()))
                .await
                .expect("insert");
            save_published_look(pool, inserted.id, &LOOK)
                .await
                .expect("save");
            let after = character_by_id(pool, inserted.id).await.expect("load");
            (
                inserted.published_look,
                after.and_then(|c| c.published_look),
            )
        })
        .await;
        assert_eq!(before, None, "a new character has no published look");
        assert_eq!(after, Some(LOOK.to_vec()));
    }

    #[tokio::test]
    async fn the_level_is_the_one_saved_when_last_online() {
        const SAVED_LEVEL: u32 = 7;
        let (listed, loaded) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let account = Uuid::new_v4();
            let inserted = insert_character(pool, &new_character(account))
                .await
                .expect("insert");
            sqlx::query("INSERT INTO character_social (character_id, level) VALUES ($1, $2)")
                .bind(inserted.id)
                .bind(i64::from(SAVED_LEVEL))
                .execute(pool)
                .await
                .expect("social");
            let listed = characters_for_account(pool, account).await.expect("list");
            let loaded = character_by_id(pool, inserted.id).await.expect("load");
            (listed, loaded)
        })
        .await;
        assert_eq!(listed[0].level, SAVED_LEVEL);
        assert_eq!(loaded.map(|character| character.level), Some(SAVED_LEVEL));
    }
}
