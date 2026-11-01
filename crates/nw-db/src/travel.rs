// SPDX-License-Identifier: AGPL-3.0-or-later
//! The travel and housing state of a character (world migrations 0010 and 0020): its map
//! marker, the inn and the respawn camp it is bound to, its camp skin, its unstuck cooldown, the
//! homes it owns and their furniture, its played time, its last inn recall and its placed camp. It is part of the progression ([`crate::SavedProgression`]): it loads and
//! saves with it, in the same transaction.
//!
//! A position or a rotation is kept as the bit patterns of its `f32`s, and a `u64` entity id as
//! its 64 bits: the column holds the same bits as a signed number, so a save gives back every bit.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::progression::{from_column, to_column};

/// Bytes of a plot's raw id.
pub const PLOT_ID_LEN: usize = 16;
/// Coordinates of a position.
const POSITION_LEN: usize = 3;
/// Parts of a rotation quaternion.
const ROTATION_LEN: usize = 4;

/// One piece of furniture placed in a home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedHousingItem {
    /// Its index in the home.
    pub index: u16,
    /// The item the client placed.
    pub item: u32,
    /// Its position, as `f32` bit patterns.
    pub position: [u32; POSITION_LEN],
    /// Its rotation quaternion (x, y, z, w), as `f32` bit patterns.
    pub rotation: [u32; ROTATION_LEN],
}

/// A home a character owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedHome {
    /// The raw id of the house plot entity.
    pub plot: [u8; PLOT_ID_LEN],
    /// Where a fast travel home puts the character, as `f32` bit patterns.
    pub position: [u32; POSITION_LEN],
    /// Unix milliseconds of the purchase.
    pub purchased_ms: u64,
    /// Its furniture, by index.
    pub items: Vec<SavedHousingItem>,
    /// Its housing state (migration 0020).
    pub record: SavedHomeRecord,
}

/// The housing state of a home (migration 0020).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SavedHomeRecord {
    /// The plot reference's facet entity.
    pub plot_facet: u64,
    /// The plot reference's third id.
    pub plot_extra: u64,
    /// The house data entity's raw id.
    pub house_data: [u8; PLOT_ID_LEN],
    /// The house data entity's facet entity.
    pub house_data_facet: u64,
    /// The house type CRC.
    pub house_type: u32,
    /// The plot's territory.
    pub territory: u16,
    /// Unix milliseconds when the taxes are due.
    pub taxes_due_ms: u64,
    /// Unix milliseconds when the fast travel home may be used again.
    pub fast_travel_ready_ms: u64,
}

/// A placed camp (migration 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedCamp {
    /// Its position, as `f32` bit patterns.
    pub position: [u32; POSITION_LEN],
    /// Its tier.
    pub tier: u8,
    /// Unix milliseconds of its placement.
    pub placed_ms: u64,
    /// Its rotation quaternion (x, y, z, w) as `f32` bit patterns; all 0 when not saved.
    pub rotation: [u32; ROTATION_LEN],
    /// The CRC-32 of its blueprint's lowercase id; 0 when not saved.
    pub blueprint: u32,
}

/// What the rotation and blueprint of a camp saved before world migration 0027 read as.
const NOT_SAVED: u32 = 0;

/// A character's travel and housing state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedTravel {
    /// The map marker, as `f32` bit patterns.
    pub waypoint: Option<[u32; POSITION_LEN]>,
    /// The facet entity of the inn's spawn point.
    pub inn: Option<u64>,
    /// The facet entity of the respawn camp.
    pub camp: Option<u64>,
    /// The camp skin.
    pub camp_skin: Option<u32>,
    /// Unix milliseconds when an unstuck may be asked again.
    pub unstuck_ready_ms: u64,
    /// The homes, in the order they were bought.
    pub homes: Vec<SavedHome>,
    /// The played time before the session, ms.
    pub played_ms: u64,
    /// Unix milliseconds of the last inn recall; 0 for never.
    pub inn_recalled_ms: u64,
    /// The placed camp.
    pub placed_camp: Option<SavedCamp>,
}

type TravelRow = (Option<Vec<i32>>, Option<i64>, Option<i64>, Option<i64>, i64);
type TimeRow = (i64, i64, Option<Vec<i32>>, Option<i16>, Option<i64>);
type HomeRow = (
    Vec<u8>,
    Vec<i32>,
    i64,
    i64,
    i64,
    Vec<u8>,
    i64,
    i64,
    i32,
    i64,
    i64,
);
type ItemRow = (Vec<u8>, i32, i64, Vec<i32>, Vec<i32>);

/// A `u32` as the `INTEGER` with the same bits.
const fn bits_column(value: u32) -> i32 {
    i32::from_be_bytes(value.to_be_bytes())
}

/// The `u32` with the bits of an `INTEGER`.
const fn bits_value(column: i32) -> u32 {
    u32::from_be_bytes(column.to_be_bytes())
}

/// A `u64` as the `BIGINT` with the same bits.
pub(crate) const fn id_column(value: u64) -> i64 {
    i64::from_be_bytes(value.to_be_bytes())
}

/// The `u64` with the bits of a `BIGINT`.
pub(crate) const fn id_value(column: i64) -> u64 {
    u64::from_be_bytes(column.to_be_bytes())
}

/// The bit patterns of an array column of `N` entries.
pub(crate) fn bits_array<const N: usize>(
    column: &[i32],
    field: &'static str,
) -> Result<[u32; N], DbError> {
    let values: Vec<u32> = column.iter().map(|value| bits_value(*value)).collect();
    values.try_into().map_err(|_| DbError::OutOfRange { field })
}

/// The array column of bit patterns.
pub(crate) fn bits_list(bits: &[u32]) -> Vec<i32> {
    bits.iter().map(|value| bits_column(*value)).collect()
}

/// The travel state of a character that exists.
pub(crate) async fn character_travel(
    world: &PgPool,
    character_id: Uuid,
) -> Result<SavedTravel, DbError> {
    let row: Option<TravelRow> = sqlx::query_as(
        "SELECT waypoint_bits, inn_spawn_point, respawn_camp, camp_skin, unstuck_ready_ms \
         FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (waypoint, inn, camp, camp_skin, unstuck_ready_ms) = row.ok_or(DbError::NoSuchCharacter)?;
    let times: Option<TimeRow> = sqlx::query_as(
        "SELECT played_ms, inn_recalled_ms, camp_position_bits, camp_tier, camp_placed_ms \
         FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (played_ms, inn_recalled_ms, camp_position, camp_tier, camp_placed_ms) =
        times.ok_or(DbError::NoSuchCharacter)?;
    let shape: Option<(Option<Vec<i32>>, Option<i64>)> = sqlx::query_as(
        "SELECT camp_rotation_bits, camp_blueprint FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (camp_rotation, camp_blueprint) = shape.ok_or(DbError::NoSuchCharacter)?;
    let placed_camp = match (camp_position, camp_tier, camp_placed_ms) {
        (Some(position), Some(tier), Some(placed_ms)) => Some(SavedCamp {
            position: bits_array(&position, "camp position")?,
            tier: from_column(i64::from(tier), "camp_tier")?,
            placed_ms: from_column(placed_ms, "camp_placed_ms")?,
            rotation: camp_rotation
                .as_deref()
                .map(|bits| bits_array(bits, "camp rotation"))
                .transpose()?
                .unwrap_or([NOT_SAVED; ROTATION_LEN]),
            blueprint: camp_blueprint
                .map(|crc| from_column(crc, "camp_blueprint"))
                .transpose()?
                .unwrap_or(NOT_SAVED),
        }),
        _ => None,
    };
    let homes: Vec<HomeRow> = sqlx::query_as(
        "SELECT plot, position_bits, purchased_ms, plot_facet, plot_extra, house_data, \
         house_data_facet, house_type, territory, taxes_due_ms, fast_travel_ready_ms \
         FROM character_home WHERE character_id = $1 ORDER BY position",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let items: Vec<ItemRow> = sqlx::query_as(
        "SELECT plot, item_index, item, position_bits, rotation_bits FROM character_home_item \
         WHERE character_id = $1 ORDER BY item_index",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let mut saved_homes = homes
        .into_iter()
        .map(
            |(
                plot,
                position,
                purchased_ms,
                plot_facet,
                plot_extra,
                house_data,
                house_data_facet,
                house_type,
                territory,
                taxes_due_ms,
                fast_travel_ready_ms,
            )| {
                Ok(SavedHome {
                    plot: plot
                        .try_into()
                        .map_err(|_| DbError::OutOfRange { field: "plot" })?,
                    position: bits_array(&position, "home position")?,
                    purchased_ms: from_column(purchased_ms, "purchased_ms")?,
                    items: Vec::new(),
                    record: SavedHomeRecord {
                        plot_facet: id_value(plot_facet),
                        plot_extra: id_value(plot_extra),
                        house_data: house_data.try_into().map_err(|_| DbError::OutOfRange {
                            field: "house_data",
                        })?,
                        house_data_facet: id_value(house_data_facet),
                        house_type: from_column(house_type, "house_type")?,
                        territory: from_column(i64::from(territory), "territory")?,
                        taxes_due_ms: from_column(taxes_due_ms, "taxes_due_ms")?,
                        fast_travel_ready_ms: from_column(
                            fast_travel_ready_ms,
                            "fast_travel_ready_ms",
                        )?,
                    },
                })
            },
        )
        .collect::<Result<Vec<_>, DbError>>()?;
    for (plot, index, item, position, rotation) in items {
        let home = saved_homes
            .iter_mut()
            .find(|home| home.plot.as_slice() == plot.as_slice())
            .ok_or(DbError::OutOfRange { field: "item plot" })?;
        home.items.push(SavedHousingItem {
            index: from_column(i64::from(index), "item_index")?,
            item: from_column(item, "item")?,
            position: bits_array(&position, "item position")?,
            rotation: bits_array(&rotation, "item rotation")?,
        });
    }
    Ok(SavedTravel {
        waypoint: waypoint
            .map(|bits| bits_array(&bits, "waypoint"))
            .transpose()?,
        inn: inn.map(id_value),
        camp: camp.map(id_value),
        camp_skin: camp_skin
            .map(|skin| from_column(skin, "camp_skin"))
            .transpose()?,
        unstuck_ready_ms: from_column(unstuck_ready_ms, "unstuck_ready_ms")?,
        homes: saved_homes,
        played_ms: from_column(played_ms, "played_ms")?,
        inn_recalled_ms: from_column(inn_recalled_ms, "inn_recalled_ms")?,
        placed_camp,
    })
}

/// Saves `travel` as the whole travel state of a character whose progression row the
/// transaction already updated.
pub(crate) async fn save_character_travel(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    travel: &SavedTravel,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE player_character SET waypoint_bits = $2, inn_spawn_point = $3, \
         respawn_camp = $4, camp_skin = $5, unstuck_ready_ms = $6 WHERE character_id = $1",
    )
    .bind(character_id)
    .bind(travel.waypoint.map(|bits| bits_list(&bits)))
    .bind(travel.inn.map(id_column))
    .bind(travel.camp.map(id_column))
    .bind(travel.camp_skin.map(i64::from))
    .bind(to_column(travel.unstuck_ready_ms, "unstuck_ready_ms")?)
    .execute(&mut **transaction)
    .await?;
    let camp = travel.placed_camp;
    sqlx::query(
        "UPDATE player_character SET played_ms = $2, inn_recalled_ms = $3, \
         camp_position_bits = $4, camp_tier = $5, camp_placed_ms = $6, \
         camp_rotation_bits = $7, camp_blueprint = $8 WHERE character_id = $1",
    )
    .bind(character_id)
    .bind(to_column(travel.played_ms, "played_ms")?)
    .bind(to_column(travel.inn_recalled_ms, "inn_recalled_ms")?)
    .bind(camp.map(|camp| bits_list(&camp.position)))
    .bind(camp.map(|camp| i16::from(camp.tier)))
    .bind(
        camp.map(|camp| to_column(camp.placed_ms, "camp_placed_ms"))
            .transpose()?,
    )
    .bind(camp.map(|camp| bits_list(&camp.rotation)))
    .bind(camp.map(|camp| i64::from(camp.blueprint)))
    .execute(&mut **transaction)
    .await?;
    // The items go with their homes (ON DELETE CASCADE).
    sqlx::query("DELETE FROM character_home WHERE character_id = $1")
        .bind(character_id)
        .execute(&mut **transaction)
        .await?;
    for (position, home) in travel.homes.iter().enumerate() {
        let record = &home.record;
        sqlx::query(
            "INSERT INTO character_home (character_id, plot, position_bits, purchased_ms, \
             position, plot_facet, plot_extra, house_data, house_data_facet, house_type, \
             territory, taxes_due_ms, fast_travel_ready_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(character_id)
        .bind(home.plot.as_slice())
        .bind(bits_list(&home.position))
        .bind(to_column(home.purchased_ms, "purchased_ms")?)
        .bind(i16::try_from(position).map_err(|_| DbError::OutOfRange {
            field: "home position",
        })?)
        .bind(id_column(record.plot_facet))
        .bind(id_column(record.plot_extra))
        .bind(record.house_data.as_slice())
        .bind(id_column(record.house_data_facet))
        .bind(i64::from(record.house_type))
        .bind(i32::from(record.territory))
        .bind(to_column(record.taxes_due_ms, "taxes_due_ms")?)
        .bind(to_column(
            record.fast_travel_ready_ms,
            "fast_travel_ready_ms",
        )?)
        .execute(&mut **transaction)
        .await?;
        for item in &home.items {
            sqlx::query(
                "INSERT INTO character_home_item (character_id, plot, item_index, item, \
                 position_bits, rotation_bits) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(character_id)
            .bind(home.plot.as_slice())
            .bind(i32::from(item.index))
            .bind(i64::from(item.item))
            .bind(bits_list(&item.position))
            .bind(bits_list(&item.rotation))
            .execute(&mut **transaction)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_bit_patterns_keep_every_bit_in_their_columns() {
        for id in [0, 1, u64::MAX, 17_680_737_194_352_937_251] {
            assert_eq!(id_value(id_column(id)), id);
        }
        for bits in [0, u32::MAX, (-0.0_f32).to_bits(), 6_061.9_f32.to_bits()] {
            assert_eq!(bits_value(bits_column(bits)), bits);
        }
        let position = [
            1.5_f32.to_bits(),
            (-2.0_f32).to_bits(),
            300.25_f32.to_bits(),
        ];
        assert_eq!(
            bits_array::<POSITION_LEN>(&bits_list(&position), "test").ok(),
            Some(position)
        );
        assert!(matches!(
            bits_array::<ROTATION_LEN>(&bits_list(&position), "rotation"),
            Err(DbError::OutOfRange { field: "rotation" })
        ));
    }
}
