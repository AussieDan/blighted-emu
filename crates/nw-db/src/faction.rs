// SPDX-License-Identifier: AGPL-3.0-or-later
//! The faction of a character (world migration 0025). It is part of the progression
//! ([`crate::SavedProgression`]): it loads and saves with it, in the same transaction.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// A character's faction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SavedFaction {
    /// The faction: 0 none, 1 Syndicate, 2 Marauders, 3 Covenant.
    pub faction: u8,
    /// How often it changed from one faction to another.
    pub changes: u16,
    /// Unix nanoseconds of the last change; 0 for none.
    pub changed_ns: u64,
}

/// The faction of a character that exists.
pub(crate) async fn character_faction(
    world: &PgPool,
    character_id: Uuid,
) -> Result<SavedFaction, DbError> {
    let row: Option<(i16, i32, i64)> = sqlx::query_as(
        "SELECT faction, faction_changes, faction_changed_ns FROM player_character \
         WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (faction, changes, changed_ns) = row.ok_or(DbError::NoSuchCharacter)?;
    Ok(SavedFaction {
        faction: from_column(i64::from(faction), "faction")?,
        changes: from_column(i64::from(changes), "faction_changes")?,
        changed_ns: from_column(changed_ns, "faction_changed_ns")?,
    })
}

/// Saves `faction` as the faction of a character whose progression row the transaction already
/// updated.
pub(crate) async fn save_character_faction(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    faction: &SavedFaction,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE player_character SET faction = $2, faction_changes = $3, \
         faction_changed_ns = $4 WHERE character_id = $1",
    )
    .bind(character_id)
    .bind(i16::from(faction.faction))
    .bind(i32::from(faction.changes))
    .bind(to_column(faction.changed_ns, "faction_changed_ns")?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}
