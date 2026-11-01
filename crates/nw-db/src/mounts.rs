// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mounts of a character (world migration 0012): the mount it chose and each mount it dyed
//! or named. It is part of the progression ([`crate::SavedProgression`]): it loads and saves
//! with it, in the same transaction. The mounts a character owns are its achievements'.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// Bytes of a dye: one color index per mask channel.
pub const DYE_LEN: usize = 4;

/// One mount with a dye or a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMount {
    /// The mount id (the CRC-32 of its lowercase `MountId`).
    pub mount_id: u32,
    /// The color index of each mask channel, R, G, B, A.
    pub dye: [u8; DYE_LEN],
    /// The name bytes (empty for none).
    pub name: Vec<u8>,
}

/// A character's mounts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedMounts {
    /// The chosen mount; `None` for none.
    pub selected: Option<u32>,
    /// The mounts with a dye or a name, in the order first customized.
    pub customized: Vec<SavedMount>,
    /// When the next summon may start, Unix ns; 0 for none.
    pub summon_cooldown_end_ns: u64,
}

type MountRow = (i64, Vec<u8>, Vec<u8>);

/// The mounts of a character that exists.
pub(crate) async fn character_mounts(
    world: &PgPool,
    character_id: Uuid,
) -> Result<SavedMounts, DbError> {
    let row: Option<(Option<i64>, i64)> = sqlx::query_as(
        "SELECT mount_id, mount_cooldown_end_ns FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (selected, cooldown_end_ns) = row.ok_or(DbError::NoSuchCharacter)?;
    let mounts: Vec<MountRow> = sqlx::query_as(
        "SELECT mount_id, dye, name FROM character_mount WHERE character_id = $1 \
         ORDER BY position",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    Ok(SavedMounts {
        selected: selected
            .map(|mount| from_column(mount, "mount_id"))
            .transpose()?,
        customized: mounts
            .into_iter()
            .map(|(mount_id, dye, name)| {
                Ok(SavedMount {
                    mount_id: from_column(mount_id, "mount_id")?,
                    dye: dye
                        .try_into()
                        .map_err(|_| DbError::OutOfRange { field: "dye" })?,
                    name,
                })
            })
            .collect::<Result<_, DbError>>()?,
        summon_cooldown_end_ns: from_column(cooldown_end_ns, "mount_cooldown_end_ns")?,
    })
}

/// Saves `mounts` as the whole mount state of a character whose progression row the transaction
/// already updated.
pub(crate) async fn save_character_mounts(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    mounts: &SavedMounts,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE player_character SET mount_id = $2, mount_cooldown_end_ns = $3 \
         WHERE character_id = $1",
    )
    .bind(character_id)
    .bind(mounts.selected.map(i64::from))
    .bind(to_column(
        mounts.summon_cooldown_end_ns,
        "mount_cooldown_end_ns",
    )?)
    .execute(&mut **transaction)
    .await?;
    sqlx::query("DELETE FROM character_mount WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut **transaction)
        .await?;
    for (position, mount) in mounts.customized.iter().enumerate() {
        sqlx::query(
            "INSERT INTO character_mount (character_id, mount_id, dye, name, position) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(character_id)
        .bind(i64::from(mount.mount_id))
        .bind(mount.dye.as_slice())
        .bind(mount.name.as_slice())
        .bind(i16::try_from(position).map_err(|_| DbError::OutOfRange {
            field: "mount position",
        })?)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}
