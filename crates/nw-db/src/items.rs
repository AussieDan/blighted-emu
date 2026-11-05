// SPDX-License-Identifier: AGPL-3.0-or-later
//! The saved items of a character (admin studio spec section 6.1), with its storage chests and
//! its saved loadouts (world migration 0011), each item with its own rolls and look (world
//! migration 0017). `sim` saves them; the director loads them at a
//! join.
//!
//! Each save carries the time its item list was taken. The rows change only when that time is
//! newer than the stored one, so a late save of one `sim` never overwrites a newer list of
//! another. The storage chests and the loadouts are saved in the same transaction, under the
//! same time.

use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;
use crate::position::SaveOutcome;

/// Milliseconds per second, for the stored time.
const MILLIS_PER_SECOND: f64 = 1000.0;
/// `character_item.place` of a worn item.
const PLACE_WORN: i16 = 0;
/// `character_item.place` of a bag item.
const PLACE_BAG: i16 = 1;
/// Dye channels of an item (R, G, B, A).
pub const DYE_CHANNELS: usize = 4;
/// The item columns of migration 0017, in the order of [`RollColumns`].
macro_rules! roll_columns {
    () => {
        "gear_score, durability, perks, bound, dye, appearance, skin_dye"
    };
}
/// The worn and bag items of one character.
const SELECT_CHARACTER_ITEMS: &str = concat!(
    "SELECT place, slot, item_id, quantity, instance_id, locked, ",
    roll_columns!(),
    " FROM character_item WHERE character_id = $1 ORDER BY place, slot"
);
/// The storage items of one character.
const SELECT_STORAGE_ITEMS: &str = concat!(
    "SELECT storage_id, slot, item_id, quantity, instance_id, locked, ",
    roll_columns!(),
    " FROM character_storage_item WHERE character_id = $1 ORDER BY storage_id, slot"
);
/// One worn or bag item.
const INSERT_CHARACTER_ITEM: &str = concat!(
    "INSERT INTO character_item (character_id, place, slot, item_id, quantity, instance_id, locked, ",
    roll_columns!(),
    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"
);
/// One storage item.
const INSERT_STORAGE_ITEM: &str = concat!(
    "INSERT INTO character_storage_item (character_id, storage_id, slot, item_id, quantity, instance_id, locked, ",
    roll_columns!(),
    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"
);

/// What an item keeps of its own (world migration 0017). A `None` keeps the item's data value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedItemRolls {
    /// Its gear score; `None`: the item's data gear score.
    pub gear_score: Option<u32>,
    /// Its current durability; `None`: full.
    pub durability: Option<u32>,
    /// Its perk ids by perk slot; `None`: the item's fixed perks.
    pub perks: Option<Vec<String>>,
    /// Bound to the character; `None`: the item's `BindOnPickup`.
    pub bound: Option<bool>,
    /// The `DyeColorData` indexes of its dye channels.
    pub dye: [u8; DYE_CHANNELS],
    /// The applied skin (CRC-32 of the lowercase appearance id), 0 for none.
    pub appearance: u32,
    /// The applied skin's dye.
    pub skin_dye: [u8; DYE_CHANNELS],
}

/// The migration 0017 columns of a row: gear score, durability, perks, bound, dye, appearance,
/// skin dye.
pub(crate) type RollColumns = (
    Option<i32>,
    Option<i32>,
    Option<Vec<String>>,
    Option<bool>,
    Vec<i16>,
    i64,
    Vec<i16>,
);

/// A dye column as its channels; a channel out of range or missing is 0.
fn dye_of_column(column: &[i16]) -> [u8; DYE_CHANNELS] {
    let mut dye = [0; DYE_CHANNELS];
    for (channel, value) in dye.iter_mut().zip(column) {
        *channel = u8::try_from(*value).unwrap_or_default();
    }
    dye
}

/// A dye as its column.
pub(crate) fn dye_column(dye: [u8; DYE_CHANNELS]) -> Vec<i16> {
    dye.into_iter().map(i16::from).collect()
}

impl SavedItemRolls {
    /// The rolls of a row's columns; a number out of range is the item's data value.
    pub(crate) fn of_columns(
        (gear_score, durability, perks, bound, dye, appearance, skin_dye): RollColumns,
    ) -> Self {
        Self {
            gear_score: gear_score.and_then(|value| u32::try_from(value).ok()),
            durability: durability.and_then(|value| u32::try_from(value).ok()),
            perks,
            bound,
            dye: dye_of_column(&dye),
            appearance: u32::try_from(appearance).unwrap_or_default(),
            skin_dye: dye_of_column(&skin_dye),
        }
    }

    /// The gear score and durability as their columns (both below 2^31 in the data).
    pub(crate) fn numbers(&self) -> (Option<i64>, Option<i64>) {
        (
            self.gear_score.map(i64::from),
            self.durability.map(i64::from),
        )
    }
}

/// Where a saved item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedPlace {
    /// Worn in this paperdoll slot.
    Worn(u32),
    /// In this bag slot.
    Bag(u16),
}

/// One saved item stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedItem {
    /// The item id (`MasterItemDefinitions` `ItemID`).
    pub item_id: String,
    /// Stack size, 1 or more.
    pub quantity: u32,
    /// Where it is.
    pub place: SavedPlace,
    /// The instance id the client names it by; `None` for a row saved before it kept one.
    pub instance: Option<Uuid>,
    /// Whether its owner locked it.
    pub locked: bool,
    /// Its own rolls and look.
    pub rolls: SavedItemRolls,
}

/// One saved item stack of a storage chest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedStoredItem {
    /// The storage: its chest's entity id.
    pub storage: Uuid,
    /// The slot of the storage's container.
    pub slot: u16,
    /// The item id.
    pub item_id: String,
    /// Stack size, 1 or more.
    pub quantity: u32,
    /// The instance id the client names it by.
    pub instance: Option<Uuid>,
    /// Whether its owner locked it.
    pub locked: bool,
    /// Its own rolls and look.
    pub rolls: SavedItemRolls,
}

/// One saved loadout (gear set); loadout 0, the worn items, is never saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLoadout {
    /// Its place in the loadout list, 1 or more.
    pub index: u8,
    /// Its name.
    pub name: String,
    /// Each paperdoll slot it fills and the instance id of the item that goes there, by slot.
    pub items: Vec<(u32, Uuid)>,
    /// The attribute points saved with it (attribute, points).
    pub attributes: Vec<(u32, u32)>,
}

/// A character's storage chests and saved loadouts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedInventory {
    /// The items of its storage chests, by storage and slot.
    pub stored: Vec<SavedStoredItem>,
    /// Its saved loadouts, by index.
    pub loadouts: Vec<SavedLoadout>,
}

impl SavedPlace {
    fn columns(self) -> (i16, i64) {
        match self {
            Self::Worn(slot) => (PLACE_WORN, i64::from(slot)),
            Self::Bag(slot) => (PLACE_BAG, i64::from(slot)),
        }
    }

    fn of_columns(place: i16, slot: i32) -> Option<Self> {
        match place {
            PLACE_WORN => Some(Self::Worn(u32::try_from(slot).ok()?)),
            PLACE_BAG => Some(Self::Bag(u16::try_from(slot).ok()?)),
            _ => None,
        }
    }
}

/// A `character_item` row's first columns: place, slot, item id, quantity, instance id, locked.
type ItemRow = (i16, i32, String, i32, Option<Uuid>, bool);
/// A `character_storage_item` row's first columns: storage, slot, item id, quantity, instance
/// id, locked.
type StoredRow = (Uuid, i32, String, i32, Option<Uuid>, bool);
/// A whole `character_item` row as read: [`ItemRow`] and [`RollColumns`].
type ItemRowWithRolls = (
    i16,
    i32,
    String,
    i32,
    Option<Uuid>,
    bool,
    Option<i32>,
    Option<i32>,
    Option<Vec<String>>,
    Option<bool>,
    Vec<i16>,
    i64,
    Vec<i16>,
);
/// A whole `character_storage_item` row as read: [`StoredRow`] and [`RollColumns`].
type StoredRowWithRolls = (
    Uuid,
    i32,
    String,
    i32,
    Option<Uuid>,
    bool,
    Option<i32>,
    Option<i32>,
    Option<Vec<String>>,
    Option<bool>,
    Vec<i16>,
    i64,
    Vec<i16>,
);

/// A whole item row as its first columns and its rolls.
fn split_item_row(row: ItemRowWithRolls) -> (ItemRow, RollColumns) {
    (
        (row.0, row.1, row.2, row.3, row.4, row.5),
        (row.6, row.7, row.8, row.9, row.10, row.11, row.12),
    )
}

/// A whole storage row as its first columns and its rolls.
fn split_stored_row(row: StoredRowWithRolls) -> (StoredRow, RollColumns) {
    (
        (row.0, row.1, row.2, row.3, row.4, row.5),
        (row.6, row.7, row.8, row.9, row.10, row.11, row.12),
    )
}

/// A `character_loadout` row: index, name, attribute points.
type LoadoutRow = (i16, String, Json<Vec<(u32, u32)>>);
/// A `character_loadout_item` row: loadout index, paperdoll slot, instance id.
type LoadoutItemRow = (i16, i32, Uuid);

/// The saved items of a character in place order: `Ok(None)` when the character exists and
/// never saved items. A row the program cannot read (a slot out of range) is left out.
///
/// # Errors
///
/// Returns [`DbError::NoSuchCharacter`] when the world has no such character.
pub async fn character_items(
    world: &PgPool,
    character_id: Uuid,
) -> Result<Option<Vec<SavedItem>>, DbError> {
    let saved: Option<(bool,)> = sqlx::query_as(
        "SELECT items_saved_at IS NOT NULL FROM player_character WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_optional(world)
    .await?;
    let (ever_saved,) = saved.ok_or(DbError::NoSuchCharacter)?;
    if !ever_saved {
        return Ok(None);
    }
    let rows: Vec<(ItemRow, RollColumns)> =
        sqlx::query_as::<_, ItemRowWithRolls>(SELECT_CHARACTER_ITEMS)
            .bind(character_id)
            .fetch_all(world)
            .await?
            .into_iter()
            .map(split_item_row)
            .collect();
    Ok(Some(
        rows.into_iter()
            .filter_map(
                |((place, slot, item_id, quantity, instance, locked), rolls)| {
                    Some(SavedItem {
                        item_id,
                        quantity: u32::try_from(quantity).ok()?,
                        place: SavedPlace::of_columns(place, slot)?,
                        instance,
                        locked,
                        rolls: SavedItemRolls::of_columns(rolls),
                    })
                },
            )
            .collect(),
    ))
}

/// The storage chests and saved loadouts of a character, by storage and slot and by loadout
/// index; empty when it saved none. A row the program cannot read is left out.
///
/// # Errors
///
/// Returns [`DbError::Database`] when a query fails.
pub async fn character_inventory(
    world: &PgPool,
    character_id: Uuid,
) -> Result<SavedInventory, DbError> {
    let stored: Vec<(StoredRow, RollColumns)> =
        sqlx::query_as::<_, StoredRowWithRolls>(SELECT_STORAGE_ITEMS)
            .bind(character_id)
            .fetch_all(world)
            .await?
            .into_iter()
            .map(split_stored_row)
            .collect();
    let loadouts: Vec<LoadoutRow> = sqlx::query_as(
        "SELECT loadout_index, name, attributes FROM character_loadout WHERE character_id = $1 \
         ORDER BY loadout_index",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    let loadout_items: Vec<LoadoutItemRow> = sqlx::query_as(
        "SELECT loadout_index, paperdoll_slot, instance_id FROM character_loadout_item \
         WHERE character_id = $1 ORDER BY loadout_index, paperdoll_slot",
    )
    .bind(character_id)
    .fetch_all(world)
    .await?;
    Ok(SavedInventory {
        stored: stored
            .into_iter()
            .filter_map(
                |((storage, slot, item_id, quantity, instance, locked), rolls)| {
                    Some(SavedStoredItem {
                        storage,
                        slot: u16::try_from(slot).ok()?,
                        item_id,
                        quantity: u32::try_from(quantity).ok()?,
                        instance,
                        locked,
                        rolls: SavedItemRolls::of_columns(rolls),
                    })
                },
            )
            .collect(),
        loadouts: loadouts
            .into_iter()
            .filter_map(|(index, name, Json(attributes))| {
                Some(SavedLoadout {
                    index: u8::try_from(index).ok()?,
                    name,
                    items: loadout_items
                        .iter()
                        .filter(|(of, _, _)| *of == index)
                        .filter_map(|(_, slot, instance)| {
                            Some((u32::try_from(*slot).ok()?, *instance))
                        })
                        .collect(),
                    attributes,
                })
            })
            .collect(),
    })
}

/// Saves `items` as the whole item list of a character and `inventory` as its storage chests
/// and loadouts, taken at `taken_at_ms` (Unix ms), when no list taken at the same time or later
/// is stored.
///
/// # Errors
///
/// Returns [`DbError::NoSuchCharacter`] when the world has no such character, and
/// [`DbError::Database`] for a row a table constraint refuses (a quantity of 0, two items in
/// one place).
pub async fn save_character_items(
    world: &PgPool,
    character_id: Uuid,
    items: &[SavedItem],
    inventory: &SavedInventory,
    taken_at_ms: u64,
) -> Result<SaveOutcome, DbError> {
    let taken_at_seconds = taken_at_ms as f64 / MILLIS_PER_SECOND;
    let mut transaction = world.begin().await?;
    let updated = sqlx::query(
        "UPDATE player_character SET items_saved_at = to_timestamp($2) WHERE character_id = $1 \
         AND (items_saved_at IS NULL OR items_saved_at < to_timestamp($2))",
    )
    .bind(character_id)
    .bind(taken_at_seconds)
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
    for table in [
        "DELETE FROM character_item WHERE character_id = $1",
        "DELETE FROM character_storage_item WHERE character_id = $1",
        "DELETE FROM character_loadout WHERE character_id = $1",
    ] {
        sqlx::query(table)
            .bind(character_id)
            .execute(&mut *transaction)
            .await?;
    }
    for item in items {
        let (place, slot) = item.place.columns();
        let (gear_score, durability) = item.rolls.numbers();
        sqlx::query(INSERT_CHARACTER_ITEM)
            .bind(character_id)
            .bind(place)
            .bind(slot)
            .bind(&item.item_id)
            .bind(i64::from(item.quantity))
            .bind(item.instance)
            .bind(item.locked)
            .bind(gear_score)
            .bind(durability)
            .bind(&item.rolls.perks)
            .bind(item.rolls.bound)
            .bind(dye_column(item.rolls.dye))
            .bind(i64::from(item.rolls.appearance))
            .bind(dye_column(item.rolls.skin_dye))
            .execute(&mut *transaction)
            .await?;
    }
    save_inventory(&mut transaction, character_id, inventory).await?;
    transaction.commit().await?;
    Ok(SaveOutcome::Saved)
}

/// Inserts the rows of `inventory` for `character_id` in `transaction`.
async fn save_inventory(
    transaction: &mut Transaction<'_, Postgres>,
    character_id: Uuid,
    inventory: &SavedInventory,
) -> Result<(), DbError> {
    for item in &inventory.stored {
        let (gear_score, durability) = item.rolls.numbers();
        sqlx::query(INSERT_STORAGE_ITEM)
            .bind(character_id)
            .bind(item.storage)
            .bind(i64::from(item.slot))
            .bind(&item.item_id)
            .bind(i64::from(item.quantity))
            .bind(item.instance)
            .bind(item.locked)
            .bind(gear_score)
            .bind(durability)
            .bind(&item.rolls.perks)
            .bind(item.rolls.bound)
            .bind(dye_column(item.rolls.dye))
            .bind(i64::from(item.rolls.appearance))
            .bind(dye_column(item.rolls.skin_dye))
            .execute(&mut **transaction)
            .await?;
    }
    for loadout in &inventory.loadouts {
        sqlx::query(
            "INSERT INTO character_loadout (character_id, loadout_index, name, attributes) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(character_id)
        .bind(i16::from(loadout.index))
        .bind(&loadout.name)
        .bind(Json(&loadout.attributes))
        .execute(&mut **transaction)
        .await?;
        for (slot, instance) in &loadout.items {
            sqlx::query(
                "INSERT INTO character_loadout_item \
                 (character_id, loadout_index, paperdoll_slot, instance_id) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(character_id)
            .bind(i16::from(loadout.index))
            .bind(i64::from(*slot))
            .bind(*instance)
            .execute(&mut **transaction)
            .await?;
        }
    }
    Ok(())
}

/// The skins an account collected and the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSkins {
    /// The account of the character.
    pub account: Uuid,
    /// Every appearance (skin id) the account collected, ascending.
    pub skins: Vec<u32>,
}

/// Adds `collected` to the skins of the account of `character` (a skin it holds stays as it
/// was) and returns every skin of the account.
///
/// # Errors
///
/// Returns [`DbError::NoSuchCharacter`] when the world has no such character, and
/// [`DbError::Database`] when a query fails.
pub async fn collect_account_skins(
    world: &PgPool,
    character: Uuid,
    collected: &[u32],
) -> Result<AccountSkins, DbError> {
    let mut transaction = world.begin().await?;
    let account: Uuid =
        sqlx::query_scalar("SELECT account_id FROM player_character WHERE character_id = $1")
            .bind(character)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(DbError::NoSuchCharacter)?;
    let ids: Vec<i64> = collected.iter().map(|skin| i64::from(*skin)).collect();
    sqlx::query(
        "INSERT INTO account_skin (account_id, appearance) SELECT $1, UNNEST($2::BIGINT[]) \
         ON CONFLICT (account_id, appearance) DO NOTHING",
    )
    .bind(account)
    .bind(&ids)
    .execute(&mut *transaction)
    .await?;
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT appearance FROM account_skin WHERE account_id = $1 ORDER BY appearance",
    )
    .bind(account)
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(AccountSkins {
        account,
        skins: rows
            .into_iter()
            .filter_map(|(appearance,)| u32::try_from(appearance).ok())
            .collect(),
    })
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use super::*;
    use crate::migrate::WORLD_MIGRATOR;
    use crate::test_db::with_test_schema;
    use crate::{NewCharacter, insert_character};

    /// 2027-01-01, with milliseconds.
    const TAKEN_AT_MS: u64 = 1_798_761_600_123;
    const LATER_MS: u64 = 1_000;
    const SWORD: &str = "1hSwordT2_FTUE";
    const POTION: &str = "PotionHealthT1";
    const FLINT: &str = "FlintT1";
    const MAIN_HAND: u32 = 3;
    const POTIONS: u32 = 5;
    const FLINTS: u32 = 12;
    const STORAGE_SLOT: u16 = 2;
    const LOADOUT: u8 = 1;
    const ATTRIBUTES: [(u32, u32); 2] = [(0, 150), (1, 350)];
    const SWORD_INSTANCE: Uuid = Uuid::from_u128(0x83544b8b_eb8d_4150_9668_eb37262c7422);
    const SWORD_GEAR_SCORE: u32 = 590;
    const SWORD_DURABILITY: u32 = 1_250;
    const SWORD_PERK: &str = "PerkID_StatSwap_BaseDamage";

    fn kit() -> Vec<SavedItem> {
        vec![
            SavedItem {
                item_id: SWORD.to_owned(),
                quantity: 1,
                place: SavedPlace::Worn(MAIN_HAND),
                instance: Some(SWORD_INSTANCE),
                locked: true,
                rolls: SavedItemRolls {
                    gear_score: Some(SWORD_GEAR_SCORE),
                    durability: Some(SWORD_DURABILITY),
                    perks: Some(vec![SWORD_PERK.to_owned()]),
                    bound: Some(true),
                    dye: [4, 0, 184, 0],
                    appearance: u32::MAX,
                    skin_dye: [0, 5, 0, 0],
                },
            },
            SavedItem {
                item_id: POTION.to_owned(),
                quantity: POTIONS,
                place: SavedPlace::Bag(0),
                instance: None,
                locked: false,
                rolls: SavedItemRolls::default(),
            },
        ]
    }

    fn inventory() -> SavedInventory {
        SavedInventory {
            stored: vec![SavedStoredItem {
                storage: Uuid::from_u128(0x6bbf_90ec_53d9_f35f << 64),
                slot: STORAGE_SLOT,
                item_id: FLINT.to_owned(),
                quantity: FLINTS,
                instance: Some(Uuid::new_v4()),
                locked: false,
                rolls: SavedItemRolls {
                    durability: Some(0),
                    ..SavedItemRolls::default()
                },
            }],
            loadouts: vec![SavedLoadout {
                index: LOADOUT,
                name: "Best Loadout".to_owned(),
                items: vec![(MAIN_HAND, SWORD_INSTANCE)],
                attributes: ATTRIBUTES.to_vec(),
            }],
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
    async fn items_are_none_until_saved_and_then_load_in_place_order() {
        let inventory = inventory();
        let (before, saved, after, stored, missing) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let character = new_character(pool).await;
            let before = character_items(pool, character).await.expect("load");
            let saved = save_character_items(pool, character, &kit(), &inventory, TAKEN_AT_MS)
                .await
                .expect("save");
            let after = character_items(pool, character).await.expect("load");
            let stored = character_inventory(pool, character).await.expect("load");
            let missing = character_items(pool, Uuid::new_v4()).await;
            (before, saved, after, stored, missing)
        })
        .await;
        assert_eq!(before, None);
        assert_eq!(saved, SaveOutcome::Saved);
        assert_eq!(after, Some(kit()));
        assert_eq!(stored, inventory);
        assert!(matches!(missing, Err(DbError::NoSuchCharacter)));
    }

    #[tokio::test]
    async fn collected_skins_are_kept_once_per_account() {
        const SKIN: u32 = 0xfeed_beef;
        const OTHER_SKIN: u32 = 7;
        let (first, again, missing) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let character = new_character(pool).await;
            let first = collect_account_skins(pool, character, &[SKIN, OTHER_SKIN])
                .await
                .expect("collect");
            let again = collect_account_skins(pool, character, &[SKIN])
                .await
                .expect("collect");
            let missing = collect_account_skins(pool, Uuid::new_v4(), &[SKIN]).await;
            (first, again, missing)
        })
        .await;
        assert_eq!(first.skins, [OTHER_SKIN, SKIN]);
        assert_eq!(again, first);
        assert!(matches!(missing, Err(DbError::NoSuchCharacter)));
    }

    #[tokio::test]
    async fn an_older_list_loses_and_an_empty_bag_stays_empty() {
        let (older, emptied, after, stored, missing_save) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let character = new_character(pool).await;
            save_character_items(pool, character, &kit(), &inventory(), TAKEN_AT_MS)
                .await
                .expect("save");
            let older = save_character_items(
                pool,
                character,
                &[],
                &SavedInventory::default(),
                TAKEN_AT_MS,
            )
            .await
            .expect("save");
            let emptied = save_character_items(
                pool,
                character,
                &[],
                &SavedInventory::default(),
                TAKEN_AT_MS + LATER_MS,
            )
            .await
            .expect("save");
            let after = character_items(pool, character).await.expect("load");
            let stored = character_inventory(pool, character).await.expect("load");
            let missing_save = save_character_items(
                pool,
                Uuid::new_v4(),
                &kit(),
                &SavedInventory::default(),
                TAKEN_AT_MS,
            )
            .await;
            (older, emptied, after, stored, missing_save)
        })
        .await;
        assert_eq!(older, SaveOutcome::Older);
        assert_eq!(emptied, SaveOutcome::Saved);
        assert_eq!(after, Some(Vec::new()));
        assert_eq!(stored, SavedInventory::default());
        assert!(matches!(missing_save, Err(DbError::NoSuchCharacter)));
    }
}
