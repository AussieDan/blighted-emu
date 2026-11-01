// SPDX-License-Identifier: AGPL-3.0-or-later
//! The status effects of the used consumables a character holds (world migration 0026). They
//! are part of the progression ([`crate::SavedProgression`]): they load and save with it, in the
//! same transaction.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// One held consumable status effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedStatusEffect {
    /// The `StatusID` of the effect.
    pub status_id: String,
    /// The `ConsumableID` of the consumable that added it.
    pub consumable_id: String,
    /// Unix nanoseconds when it ends.
    pub ends_ns: u64,
    /// Its stacks.
    pub stacks: u8,
    /// Its time runs on while the player is away (`DisablePauseOnPersist`); else it pauses.
    pub runs_offline: bool,
    /// The next repeat of the consumable's vitals change and the end of the repeats (unix ns),
    /// when it has repeats.
    pub repeat: Option<(u64, u64)>,
    /// Unix nanoseconds when its state was taken: a pausing effect has paused since then.
    pub saved_ns: u64,
}

type StatusEffectRow = (
    String,
    String,
    i64,
    i16,
    bool,
    Option<i64>,
    Option<i64>,
    i64,
);

/// The held consumable status effects of a character, by effect.
pub(crate) async fn character_status_effects(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Vec<SavedStatusEffect>, DbError> {
    let rows: Vec<StatusEffectRow> = sqlx::query_as(
        "SELECT status_id, consumable_id, ends_ns, stacks, runs_offline, repeat_next_ns, \
         repeat_ends_ns, saved_ns FROM character_status_effect \
         WHERE character_id = $1 ORDER BY status_id",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    rows.into_iter()
        .map(
            |(
                status_id,
                consumable_id,
                ends_ns,
                stacks,
                runs_offline,
                next_ns,
                last_ns,
                saved_ns,
            )| {
                let repeat = match next_ns.zip(last_ns) {
                    Some((next_ns, last_ns)) => Some((
                        from_column(next_ns, "status effect repeat")?,
                        from_column(last_ns, "status effect repeat end")?,
                    )),
                    None => None,
                };
                Ok(SavedStatusEffect {
                    status_id,
                    consumable_id,
                    ends_ns: from_column(ends_ns, "status effect end")?,
                    stacks: from_column(i64::from(stacks), "status effect stacks")?,
                    runs_offline,
                    repeat,
                    saved_ns: from_column(saved_ns, "status effect save time")?,
                })
            },
        )
        .collect()
}

/// Saves `effects` as the held consumable status effects of a character whose progression row
/// the transaction already updated: the stored rows become exactly these.
pub(crate) async fn save_character_status_effects(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    effects: &[SavedStatusEffect],
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM character_status_effect WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut **transaction)
        .await?;
    for effect in effects {
        let repeat = effect
            .repeat
            .map(|(next_ns, last_ns)| {
                Ok::<_, DbError>((
                    to_column(next_ns, "status effect repeat")?,
                    to_column(last_ns, "status effect repeat end")?,
                ))
            })
            .transpose()?;
        sqlx::query(
            "INSERT INTO character_status_effect (character_id, status_id, consumable_id, \
             ends_ns, stacks, runs_offline, repeat_next_ns, repeat_ends_ns, saved_ns) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(character_id)
        .bind(&effect.status_id)
        .bind(&effect.consumable_id)
        .bind(to_column(effect.ends_ns, "status effect end")?)
        .bind(i16::from(effect.stacks))
        .bind(effect.runs_offline)
        .bind(repeat.map(|(next_ns, _)| next_ns))
        .bind(repeat.map(|(_, last_ns)| last_ns))
        .bind(to_column(effect.saved_ns, "status effect save time")?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}
