// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe cooldown counts of a character (world migration 0024). They are part of the
//! progression ([`crate::SavedProgression`]): they load and save with it, in the same
//! transaction.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// One recipe's cooldown count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedRecipeCooldown {
    /// The lowercase `RecipeID`.
    pub recipe_id: String,
    /// The crafts made.
    pub count: u32,
    /// Unix nanoseconds when it goes back to 0.
    pub ends_ns: u64,
}

/// The recipe cooldown counts of a character, by recipe.
pub(crate) async fn character_recipe_cooldowns(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Vec<SavedRecipeCooldown>, DbError> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT recipe_id, count, ends_ns FROM character_recipe_cooldown \
         WHERE character_id = $1 ORDER BY recipe_id",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    rows.into_iter()
        .map(|(recipe_id, count, ends_ns)| {
            Ok(SavedRecipeCooldown {
                recipe_id,
                count: from_column(count, "recipe cooldown count")?,
                ends_ns: from_column(ends_ns, "recipe cooldown end")?,
            })
        })
        .collect()
}

/// Saves `cooldowns` as the recipe cooldown counts of a character whose progression row the
/// transaction already updated: the stored rows become exactly these.
pub(crate) async fn save_character_recipe_cooldowns(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    cooldowns: &[SavedRecipeCooldown],
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM character_recipe_cooldown WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut **transaction)
        .await?;
    for cooldown in cooldowns {
        sqlx::query(
            "INSERT INTO character_recipe_cooldown (character_id, recipe_id, count, ends_ns) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(character_id)
        .bind(&cooldown.recipe_id)
        .bind(i64::from(cooldown.count))
        .bind(to_column(cooldown.ends_ns, "recipe cooldown end")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}
