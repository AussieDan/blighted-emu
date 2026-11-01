// SPDX-License-Identifier: AGPL-3.0-or-later
//! The running consumable cooldowns of a character (world migration 0029). They are part of the
//! progression ([`crate::SavedProgression`]): they load and save with it, in the same
//! transaction.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// One running consumable cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedConsumableCooldown {
    /// The CRC-32 of the lowercase `CooldownId`: the consumables that share it.
    pub group: u32,
    /// The CRC-32 of the lowercase `ConsumableID` that started it.
    pub consumable: u32,
    /// When it ends, microseconds since 2000-01-01.
    pub end_us: u64,
}

/// The running consumable cooldowns of a character, by group.
pub(crate) async fn character_consumable_cooldowns(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Vec<SavedConsumableCooldown>, DbError> {
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT cooldown_group, consumable, end_us FROM character_consumable_cooldown \
         WHERE character_id = $1 ORDER BY cooldown_group",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    rows.into_iter()
        .map(|(group, consumable, end_us)| {
            Ok(SavedConsumableCooldown {
                group: from_column(group, "consumable cooldown group")?,
                consumable: from_column(consumable, "consumable cooldown item")?,
                end_us: from_column(end_us, "consumable cooldown end")?,
            })
        })
        .collect()
}

/// Saves `cooldowns` as the running consumable cooldowns of a character whose progression row
/// the transaction already updated: the stored rows become exactly these.
pub(crate) async fn save_character_consumable_cooldowns(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    cooldowns: &[SavedConsumableCooldown],
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM character_consumable_cooldown WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut **transaction)
        .await?;
    for cooldown in cooldowns {
        sqlx::query(
            "INSERT INTO character_consumable_cooldown \
             (character_id, cooldown_group, consumable, end_us) VALUES ($1, $2, $3, $4)",
        )
        .bind(character_id)
        .bind(i64::from(cooldown.group))
        .bind(i64::from(cooldown.consumable))
        .bind(to_column(cooldown.end_us, "consumable cooldown end")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}
