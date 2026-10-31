// SPDX-License-Identifier: AGPL-3.0-or-later
//! Skins, the transmog collection and the paperdoll dye of a player (run-15 items2 study).
//!
//! **Collection.** The account's skins (world table `account_skin`) are the appearances of the
//! items its characters held and the free skins ([`nw_content::skins`]); store, Twitch and
//! season skins are never granted. A player's held items are scanned every
//! [`SCAN_INTERVAL_MS`]; a new skin and the first load go to the database as a [`SkinJob`]
//! ([`SimWorld::take_skin_jobs`], answered by [`SimWorld::skins_collected`]).
//!
//! **Retail answers (Observed, `Hive_Transmog`, `HiveChangeSkins`, `Hive_Dye`, newchar
//! 2026-09-18).** At login two 5792: the character's empty global `ItemPool`, then the
//! account's `SkinCollection`. A skin enable (5692) answers with a 5771 (the worn item's
//! `ItemPool` entry and the skin's `EquippedSkins` entry with its dye) 100 to 130 ms later, then
//! 5985 (skin, slot), then the paperdoll change; a disable (5693) with a 5771 (the pool entry's
//! emptied `SkinSlot` link and the skin's entry, gone) and 5985 (0, slot). A dye (2224) takes
//! the dye items of its listed bag slots and answers, per dyed skin, a 5771 and 6147 (skin,
//! true). The screen flags (5302, 5978, 6105, 6463, 6486) had no answer in any capture.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_db::{AccountSkins, DbError};
use nw_messages::inventories::{
    ITEM_TYPE_INSTANCE, ITEM_TYPE_SKIN, REGIONALITY_GLOBAL, REGIONALITY_PERSONAL,
};
use nw_messages::items::{DYE_BYTES, SYNC_CRAFT_USED};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    DyeEntry, FacetHeader, INVENTORIES, InventoriesMsg, InventoryKey, ItemData, ItemLook,
    ServerMessage, ServiceInventory, ServiceItem, ServiceRequest, TransmogClientMsg,
    TransmogServerMsg, transmog_header,
};
use uuid::Uuid;

use super::{Player, SimWorld, send_now};

/// How often a player's held items are scanned for new skins (an engine timing, not a game
/// value: retail adds a skin when the item comes).
pub const SCAN_INTERVAL_MS: u64 = 1_000;
/// How long a failed skin job waits before the next try.
const RETRY_MS: u64 = 30_000;
/// The inventory type of the item pool.
const ITEM_POOL: &str = "ItemPool";
/// The inventory type of an account's skins.
const SKIN_COLLECTION: &str = "SkinCollection";
/// The inventory type of a character's dyed skins.
const EQUIPPED_SKINS: &str = "EquippedSkins";
/// The scope of an item-pool entry.
const GLOBAL_SCOPE: &str = "global";
/// The name of the link from an item-pool entry to its skin.
const SKIN_SLOT_LINK: &str = "SkinSlot";
/// The member of a collected skin.
const IS_COLLECTED: &str = "isCollected";
/// The owner text of an account's inventories: this and the persona id (the account id).
const ACCOUNT_OWNER_PREFIX: &str = "new-world|";
/// The value of a set integer flag member.
const MEMBER_SET: u64 = 1;
/// The version retail gives a new entry and an inventory listed at login.
const FIRST_VERSION: u64 = 1;
/// The source action of a new entry (retail 0).
const SOURCE_ADDED: u32 = 0;
/// The source action of a changed entry (retail 2, the pool entry of a disable).
const SOURCE_CHANGED: u32 = 2;
/// The transaction id of an entry that is not found.
const NO_TRANSACTION: [u8; RAW_ID_LEN] = [0; RAW_ID_LEN];
/// The dye members of an `EquippedSkins` entry, in the order retail sends them, with the dye
/// channel each holds (record channels R, G, B, A).
const DYE_MEMBERS: [(&str, usize); DYE_BYTES] = [("a", 3), ("b", 2), ("g", 1), ("r", 0)];
/// A dye channel without a dye.
const NO_DYE: u8 = 0;

/// The transmog collection of a player as this sim knows it.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Wardrobe {
    /// Its account, once the collection is loaded.
    account: Option<Uuid>,
    /// The account's skins.
    collected: BTreeSet<u32>,
    /// Skins to add to the account.
    pending: BTreeSet<u32>,
    /// A skin job runs.
    asking: bool,
    /// The login's 5792 are still due to its client.
    owes_inventories: bool,
    /// The next scan of its items.
    next_scan_ms: u64,
    /// No job before this time (after a failed one).
    retry_at_ms: u64,
    /// The version of its service entries.
    version: u64,
}

/// A skin job: add `collect` to the account of `character` and load its skins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinJob {
    /// The player's entity number.
    pub number: u64,
    /// The player's character.
    pub character: Uuid,
    /// The skins to add.
    pub collect: Vec<u32>,
}

/// Why a transmog call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransmogRefusal {
    /// The collection is not loaded yet.
    NotLoaded,
    /// The account has not collected the skin.
    NotCollected,
    /// No item is worn in the slot.
    NoItem,
    /// The skin does not go on the item.
    WrongKind,
    /// The slot's item shows no skin.
    NoSkin,
    /// A color is unknown or a store color.
    StoreColor,
    /// The listed bag slots hold too few dye items.
    NotEnoughDyes,
}

/// The worn item a skin call names, as the answer needs it.
struct WornForSkin {
    /// The player's character id text.
    character: String,
    /// The next version of the player's service entries.
    version: u64,
    /// The item's class parts.
    classes: Vec<String>,
    /// The item's instance id.
    instance: [u8; RAW_ID_LEN],
    /// The item's look.
    look: ItemLook,
}

/// A random transaction or request id.
fn random_id() -> [u8; RAW_ID_LEN] {
    Uuid::new_v4().into_bytes()
}

/// The request context of an answer to the player of entity number `number`.
fn service_request(number: u64, regionality: u32) -> ServiceRequest {
    ServiceRequest {
        header: FacetHeader::of(INVENTORIES, number),
        regionality,
        request: random_id(),
        group: random_id(),
        requester: transmog_header(number).target,
    }
}

/// The upper-case hex id of an item instance (retail `ItemPool` ids).
fn instance_hex(instance: &[u8; RAW_ID_LEN]) -> String {
    instance.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// The item-pool entry of the worn item `instance` of `character` at `version`.
fn pool_entry(
    instance: &[u8; RAW_ID_LEN],
    character: &str,
    version: u64,
    source: u32,
    link: Option<&str>,
) -> ServiceItem {
    ServiceItem {
        item_type: ITEM_TYPE_INSTANCE,
        id: instance_hex(instance),
        scope: GLOBAL_SCOPE.to_owned(),
        source,
        inventory: InventoryKey {
            kind: ITEM_POOL.to_owned(),
            owner: character.to_owned(),
        },
        version,
        transaction: random_id(),
        data: ItemData::Pool(link.map(str::to_owned)),
    }
}

/// The `EquippedSkins` entry of `skin` of `character`: with its dye `dye` at `version`, or gone.
fn skin_entry(skin: u32, character: &str, dyed: Option<(u64, [u8; DYE_BYTES])>) -> ServiceItem {
    let (version, transaction, data) = match dyed {
        Some((version, dye)) => (
            version,
            random_id(),
            ItemData::Skin(
                DYE_MEMBERS
                    .iter()
                    .map(|(name, channel)| ((*name).to_owned(), u64::from(dye[*channel])))
                    .collect(),
            ),
        ),
        None => (0, NO_TRANSACTION, ItemData::Missing),
    };
    ServiceItem {
        item_type: ITEM_TYPE_SKIN,
        id: skin.to_string(),
        scope: character.to_owned(),
        source: SOURCE_ADDED,
        inventory: InventoryKey {
            kind: EQUIPPED_SKINS.to_owned(),
            owner: character.to_owned(),
        },
        version,
        transaction,
        data,
    }
}

/// The two 5792 of a login: the empty item pool of `character`, then the skins of `account`.
fn login_inventories(
    number: u64,
    character: &str,
    account: Uuid,
    skins: &BTreeSet<u32>,
) -> [ServerMessage; 2] {
    let owner = format!("{ACCOUNT_OWNER_PREFIX}{account}");
    let pool = InventoriesMsg::QueryInventories {
        request: service_request(number, REGIONALITY_GLOBAL),
        inventory: ServiceInventory {
            key: InventoryKey {
                kind: ITEM_POOL.to_owned(),
                owner: character.to_owned(),
            },
            items: Vec::new(),
            version: FIRST_VERSION,
            transaction: random_id(),
        },
    };
    let collection = InventoriesMsg::QueryInventories {
        request: service_request(number, REGIONALITY_PERSONAL),
        inventory: ServiceInventory {
            key: InventoryKey {
                kind: SKIN_COLLECTION.to_owned(),
                owner: owner.clone(),
            },
            items: skins
                .iter()
                .map(|skin| ServiceItem {
                    item_type: ITEM_TYPE_SKIN,
                    id: skin.to_string(),
                    scope: owner.clone(),
                    source: SOURCE_ADDED,
                    inventory: InventoryKey {
                        kind: SKIN_COLLECTION.to_owned(),
                        owner: owner.clone(),
                    },
                    version: FIRST_VERSION,
                    transaction: random_id(),
                    data: ItemData::Listed(vec![(IS_COLLECTED.to_owned(), MEMBER_SET)]),
                })
                .collect(),
            // Best guess: one version per collected skin (retail 310 for 423 skins).
            version: skins.len() as u64,
            transaction: random_id(),
        },
    };
    [
        ServerMessage::Inventories(pool),
        ServerMessage::Inventories(collection),
    ]
}

/// The dye items `need` (color → count) takes from `listed` bag slots (slot, color, quantity):
/// (bag slot, how many) in slot order; `None` when they hold too few of a color.
fn dye_takes(need: &BTreeMap<u8, u32>, listed: &[(u32, u8, u32)]) -> Option<Vec<(u32, u32)>> {
    let mut takes = Vec::new();
    for (color, wanted) in need {
        let mut left = *wanted;
        for (bag_slot, held_color, quantity) in listed {
            if left == 0 {
                break;
            }
            if held_color == color {
                let taken = left.min(*quantity);
                takes.push((*bag_slot, taken));
                left -= taken;
            }
        }
        if left > 0 {
            return None;
        }
    }
    Some(takes)
}

impl SimWorld {
    /// Gives the player of entity number `number` its collection state; a login owes its
    /// client the 5792 of the collection.
    pub(super) fn transmog_arrived(&mut self, number: u64, login: bool) {
        if let Some(entity) = self.player_by_number(number) {
            self.world.entity_mut(entity).insert(Wardrobe {
                owes_inventories: login,
                ..Wardrobe::default()
            });
        }
    }

    /// Scans the held items of the players for new skins at `now_ms` and returns the skin jobs
    /// to run: a first load, or skins to add.
    pub fn take_skin_jobs(&mut self, now_ms: u64) -> Vec<SkinJob> {
        let content = Arc::clone(&self.content);
        let mut jobs = Vec::new();
        let mut query = self.world.query::<(&Player, &mut Wardrobe)>();
        for (player, mut wardrobe) in query.iter_mut(&mut self.world) {
            if now_ms >= wardrobe.next_scan_ms {
                wardrobe.next_scan_ms = now_ms.saturating_add(SCAN_INTERVAL_MS);
                let held = player
                    .gear
                    .iter()
                    .flat_map(|gear| gear.held_item_ids())
                    .filter_map(|item_id| content.skins.skin_of(item_id));
                let new: Vec<u32> = held
                    .chain(content.skins.free().iter().copied())
                    .filter(|skin| !wardrobe.collected.contains(skin))
                    .collect();
                wardrobe.pending.extend(new);
            }
            let wanted = wardrobe.account.is_none() || !wardrobe.pending.is_empty();
            if wanted && !wardrobe.asking && now_ms >= wardrobe.retry_at_ms {
                wardrobe.asking = true;
                jobs.push(SkinJob {
                    number: player.number,
                    character: player.character,
                    collect: wardrobe.pending.iter().copied().collect(),
                });
            }
        }
        jobs
    }

    /// Takes the answer of skin job `job`: the account and its skins, or the error (the job
    /// runs again after [`RETRY_MS`], never for a character the world does not hold). A login's
    /// 5792 go out with the first load.
    pub fn skins_collected(&mut self, job: &SkinJob, answer: Result<AccountSkins, DbError>) {
        let Some(entity) = self.player_by_number(job.number) else {
            return;
        };
        let now_ms = self.last_now_ms;
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let connection = player.connection.clone();
        let character = player.character.to_string();
        let Some(mut wardrobe) = self.world.get_mut::<Wardrobe>(entity) else {
            return;
        };
        wardrobe.asking = false;
        let skins = match answer {
            Ok(skins) => skins,
            // A character the world does not hold (a load-test bot) never gets a collection.
            Err(DbError::NoSuchCharacter) => {
                wardrobe.retry_at_ms = u64::MAX;
                return;
            }
            Err(error) => {
                tracing::warn!(player = job.number, %error, "skin collection not saved");
                wardrobe.retry_at_ms = now_ms.saturating_add(RETRY_MS);
                return;
            }
        };
        // A skin the account did not have is an unlock (5700) when the collection was
        // already loaded; the first load goes out as the login inventories below.
        let loaded = wardrobe.account.is_some();
        let fresh: Vec<u32> = skins
            .skins
            .iter()
            .copied()
            .filter(|skin| !wardrobe.collected.contains(skin))
            .collect();
        wardrobe.collected = skins.skins.into_iter().collect();
        let collected = wardrobe.collected.clone();
        if loaded && !fresh.is_empty() {
            let unlocks: Vec<ServerMessage> = fresh
                .iter()
                .map(|_| {
                    ServerMessage::Transmog(TransmogServerMsg::UnlockTransmogSkin {
                        header: transmog_header(job.number),
                        unlocked: nw_messages::transmog::SKIN_UNLOCKED,
                    })
                })
                .collect();
            send_now(&mut self.combat_messages, &connection, &unlocks);
        }
        wardrobe.pending.retain(|skin| !collected.contains(skin));
        wardrobe.account = Some(skins.account);
        if !std::mem::take(&mut wardrobe.owes_inventories) {
            return;
        }
        let messages = login_inventories(job.number, &character, skins.account, &collected);
        send_now(&mut self.combat_messages, &connection, &messages);
    }

    /// Answers transmog or dye call `call` of the player of `connection`.
    pub fn transmog_call(&mut self, connection: &ConnectionRef, call: TransmogClientMsg) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        let answer = match call {
            TransmogClientMsg::EnableItemSkin { slot, skin, .. } => {
                self.enable_skin(entity, number, slot, skin)
            }
            TransmogClientMsg::DisableItemSkin { slot, .. } => {
                self.disable_skin(entity, number, slot)
            }
            TransmogClientMsg::PaperdollDye {
                entries, bag_slots, ..
            } => self.dye(entity, number, &entries, &bag_slots),
            TransmogClientMsg::SetPreviewActive { active, .. } => {
                tracing::trace!(player = number, active, "transmog preview");
                Ok(Vec::new())
            }
            TransmogClientMsg::SetHidingSkinOnSlot { slot, hide, .. } => {
                tracing::trace!(player = number, slot, hide, "skin hidden on slot");
                Ok(Vec::new())
            }
            TransmogClientMsg::TransmogScreenOpen { open, .. }
            | TransmogClientMsg::TransmogStationScreenOpen { open, .. }
            | TransmogClientMsg::ArmorDyeingOpen { open, .. } => {
                tracing::trace!(player = number, open, "transmog screen");
                Ok(Vec::new())
            }
        };
        match answer {
            Ok(messages) if !messages.is_empty() => {
                send_now(&mut self.combat_messages, connection, &messages);
            }
            Ok(_) => {}
            Err(refusal) => tracing::debug!(player = number, ?refusal, "transmog call refused"),
        }
    }

    /// The worn item of `slot` of player `entity` with the next version of its service entries;
    /// refused before the collection is loaded.
    fn worn_for_skin(&mut self, entity: Entity, slot: u32) -> Result<WornForSkin, TransmogRefusal> {
        let content = Arc::clone(&self.content);
        let player = self
            .world
            .get::<Player>(entity)
            .ok_or(TransmogRefusal::NoItem)?;
        let character = player.character.to_string();
        let worn = player
            .gear
            .as_ref()
            .and_then(|gear| gear.worn_look(slot))
            .ok_or(TransmogRefusal::NoItem)?;
        let classes = content
            .item_rolls
            .item(worn.item_id)
            .map(|row| row.classes.clone())
            .unwrap_or_default();
        let (instance, look) = (worn.instance, worn.look);
        let mut wardrobe = self
            .world
            .get_mut::<Wardrobe>(entity)
            .ok_or(TransmogRefusal::NotLoaded)?;
        if wardrobe.account.is_none() {
            return Err(TransmogRefusal::NotLoaded);
        }
        wardrobe.version = wardrobe.version.saturating_add(1);
        Ok(WornForSkin {
            character,
            version: wardrobe.version,
            classes,
            instance,
            look,
        })
    }

    /// Whether the account of player `entity` collected `skin`.
    fn has_skin(&self, entity: Entity, skin: u32) -> bool {
        self.world
            .get::<Wardrobe>(entity)
            .is_some_and(|wardrobe| wardrobe.collected.contains(&skin))
    }

    /// Gives player `entity` the look `look` on slot `slot`.
    fn set_look(&mut self, entity: Entity, slot: u32, look: ItemLook) -> bool {
        self.world
            .get_mut::<Player>(entity)
            .and_then(|mut player| {
                player
                    .gear
                    .as_mut()
                    .map(|gear| gear.set_worn_look(slot, look))
            })
            .unwrap_or(false)
    }

    /// 5692: skin `skin` over the item of `slot` (module notes).
    fn enable_skin(
        &mut self,
        entity: Entity,
        number: u64,
        slot: u32,
        skin: u32,
    ) -> Result<Vec<ServerMessage>, TransmogRefusal> {
        if !self.has_skin(entity, skin) {
            return Err(TransmogRefusal::NotCollected);
        }
        let WornForSkin {
            character,
            version,
            classes,
            instance,
            look,
        } = self.worn_for_skin(entity, slot)?;
        if !self.content.skins.fits(skin, &classes) {
            return Err(TransmogRefusal::WrongKind);
        }
        // The skin's dye stays while the same skin is shown again.
        let skin_dye = if look.appearance == skin {
            look.skin_dye
        } else {
            [NO_DYE; DYE_BYTES]
        };
        self.set_look(
            entity,
            slot,
            ItemLook {
                appearance: skin,
                skin_dye,
                ..look
            },
        );
        Ok(vec![
            skin_items(number, &character, &instance, version, skin, skin_dye),
            ServerMessage::Transmog(TransmogServerMsg::EnableSkinSuccess {
                header: transmog_header(number),
                skin,
                slot,
            }),
        ])
    }

    /// 5693: the item of `slot` without its skin (module notes).
    fn disable_skin(
        &mut self,
        entity: Entity,
        number: u64,
        slot: u32,
    ) -> Result<Vec<ServerMessage>, TransmogRefusal> {
        let WornForSkin {
            character,
            version,
            instance,
            look,
            ..
        } = self.worn_for_skin(entity, slot)?;
        if look.appearance == 0 {
            return Err(TransmogRefusal::NoSkin);
        }
        self.set_look(
            entity,
            slot,
            ItemLook {
                appearance: 0,
                skin_dye: [NO_DYE; DYE_BYTES],
                ..look
            },
        );
        let items = InventoriesMsg::GetItems {
            request: service_request(number, REGIONALITY_GLOBAL),
            items: vec![
                pool_entry(
                    &instance,
                    &character,
                    version,
                    SOURCE_CHANGED,
                    Some(SKIN_SLOT_LINK),
                ),
                skin_entry(look.appearance, &character, None),
            ],
        };
        Ok(vec![
            ServerMessage::Inventories(items),
            ServerMessage::Transmog(TransmogServerMsg::EnableSkinSuccess {
                header: transmog_header(number),
                skin: 0,
                slot,
            }),
        ])
    }

    /// 2224: dyes the worn items of `entries` with the dye items of `bag_slots` (module notes).
    /// A channel set to a new color takes one dye item of that color; a store color or too few
    /// dye items refuse the whole request ("Not enough dyes to complete dyeing request!").
    fn dye(
        &mut self,
        entity: Entity,
        number: u64,
        entries: &[DyeEntry],
        bag_slots: &[u32],
    ) -> Result<Vec<ServerMessage>, TransmogRefusal> {
        let content = Arc::clone(&self.content);
        let player = self
            .world
            .get::<Player>(entity)
            .ok_or(TransmogRefusal::NoItem)?;
        let gear = player.gear.as_ref().ok_or(TransmogRefusal::NoItem)?;
        let mut need: BTreeMap<u8, u32> = BTreeMap::new();
        let mut changes = Vec::new();
        for entry in entries {
            let Some(worn) = gear.worn_look(entry.slot) else {
                continue;
            };
            let skinned = worn.look.appearance != 0;
            let current = if skinned {
                worn.look.skin_dye
            } else {
                worn.look.dye
            };
            for (new, old) in entry.dye.iter().zip(current) {
                if *new == NO_DYE || *new == old {
                    continue;
                }
                if !content.dyes.is_free_color(*new) {
                    return Err(TransmogRefusal::StoreColor);
                }
                *need.entry(*new).or_default() += 1;
            }
            let look = if skinned {
                ItemLook {
                    skin_dye: entry.dye,
                    ..worn.look
                }
            } else {
                ItemLook {
                    dye: entry.dye,
                    ..worn.look
                }
            };
            if look != worn.look {
                changes.push((entry.slot, look, worn.instance));
            }
        }
        let listed: Vec<(u32, u8, u32)> = bag_slots
            .iter()
            .filter_map(|bag_slot| {
                let (item_id, item) = gear.bag_item(*bag_slot)?;
                Some((*bag_slot, content.dyes.color_of(item_id)?, item.quantity))
            })
            .collect();
        let takes = dye_takes(&need, &listed).ok_or(TransmogRefusal::NotEnoughDyes)?;
        let character = player.character.to_string();
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
        {
            // Retail `Hive_Dye` 232.245 s: the dye stacks and the slots they left empty went
            // out with a craft's sync reason.
            for (bag_slot, quantity) in takes {
                gear.take_from_slot_as(bag_slot, quantity, SYNC_CRAFT_USED);
            }
        }
        let mut messages = Vec::new();
        for (slot, look, instance) in changes {
            self.set_look(entity, slot, look);
            if look.appearance == 0 {
                continue;
            }
            let version =
                self.world
                    .get_mut::<Wardrobe>(entity)
                    .map_or(FIRST_VERSION, |mut wardrobe| {
                        wardrobe.version = wardrobe.version.saturating_add(1);
                        wardrobe.version
                    });
            messages.push(skin_items(
                number,
                &character,
                &instance,
                version,
                look.appearance,
                look.skin_dye,
            ));
            messages.push(ServerMessage::Transmog(TransmogServerMsg::SkinDye {
                header: transmog_header(number),
                skin: look.appearance,
                ok: true,
            }));
        }
        Ok(messages)
    }
}

/// The 5771 of a shown skin: the worn item's pool entry and the skin's entry with `dye`.
fn skin_items(
    number: u64,
    character: &str,
    instance: &[u8; RAW_ID_LEN],
    version: u64,
    skin: u32,
    dye: [u8; DYE_BYTES],
) -> ServerMessage {
    ServerMessage::Inventories(InventoriesMsg::GetItems {
        request: service_request(number, REGIONALITY_GLOBAL),
        items: vec![
            pool_entry(instance, character, version, SOURCE_ADDED, None),
            skin_entry(skin, character, Some((version, dye))),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dye_items_come_from_the_listed_slots_in_order() {
        let need = BTreeMap::from([(4, 2), (9, 1)]);
        let listed = [(7, 4, 1), (3, 9, 5), (8, 4, 3)];
        assert_eq!(
            dye_takes(&need, &listed),
            Some(vec![(7, 1), (8, 1), (3, 1)])
        );
        let short = BTreeMap::from([(4, 5)]);
        assert_eq!(dye_takes(&short, &listed), None);
        assert_eq!(dye_takes(&BTreeMap::new(), &listed), Some(Vec::new()));
    }

    #[test]
    fn a_skin_entry_holds_its_dye_as_retail_members() {
        let entry = skin_entry(0xb051_8525, "character", Some((2, [6, 0x94, 0x3e, 0x20])));
        assert_eq!(entry.id, "2958132517");
        assert_eq!(
            entry.data,
            ItemData::Skin(vec![
                ("a".to_owned(), 0x20),
                ("b".to_owned(), 0x3e),
                ("g".to_owned(), 0x94),
                ("r".to_owned(), 6),
            ])
        );
        let gone = skin_entry(1, "character", None);
        assert_eq!((gone.version, gone.data), (0, ItemData::Missing));
        assert_eq!(instance_hex(&[0xab; RAW_ID_LEN]).len(), 2 * RAW_ID_LEN);
    }

    mod in_world {
        use std::sync::Arc;

        use nw_content::item_rolls::{ItemRollRow, ItemRolls};
        use nw_content::perks::{PerkTable, parts};
        use nw_content::skins::{DyeCatalog, SkinCatalog, SkinItemRow, SkinSource};
        use nw_messages::paperdoll::slot;
        use nw_messages::{FacetHeader, ServerMessage};

        use super::super::*;
        use crate::player_content::PlayerContent;
        use crate::world::gear::Gear;
        use crate::world::gear::tests::{BACKSTORY, CHARACTER, FLINT, SWORD, kits};
        use crate::world::tests::{NOW, connection, join, messages_to, tick, world_with_spawns};

        const ACCOUNT: Uuid = Uuid::from_u128(0x5eed);
        const SWORD_CLASS: &str = "Weapon+EquippableMainHand+Sword+Melee";
        /// The test flint is the dye of color 4 here.
        const DYE_COLOR: u8 = 4;
        const FLINTS: u32 = 3;

        fn header() -> FacetHeader {
            FacetHeader {
                target: 0,
                entity: 0,
            }
        }

        fn world() -> SimWorld {
            let mut world = world_with_spawns(Vec::new());
            world.content = Arc::new(PlayerContent {
                kits: kits(),
                item_rolls: ItemRolls::build(
                    [ItemRollRow {
                        item_id: SWORD.to_owned(),
                        classes: parts(Some(SWORD_CLASS)),
                        ..ItemRollRow::default()
                    }],
                    PerkTable::default(),
                ),
                skins: SkinCatalog::build(SkinSource {
                    items: vec![SkinItemRow {
                        item_id: SWORD.to_owned(),
                        classes: parts(Some(SWORD_CLASS)),
                        armor_appearance: None,
                        weapon_appearance: None,
                    }],
                    ..SkinSource::default()
                }),
                dyes: DyeCatalog::build([(DYE_COLOR, false)], [(FLINT.to_owned(), DYE_COLOR)]),
                ..PlayerContent::default()
            });
            join(&mut world, 1, 0.0);
            let kits = kits();
            let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
            assert!(gear.add(kits.item(FLINT).expect("flint"), FLINTS));
            let entity = world.by_connection[&connection(1)];
            world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
            world
        }

        fn sword_look(world: &SimWorld) -> ItemLook {
            let entity = world.by_connection[&connection(1)];
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref()?.worn_look(slot::MAIN_HAND_OPTION_1))
                .map(|worn| worn.look)
                .expect("sword")
        }

        #[test]
        fn a_held_item_gives_its_skin_and_the_login_gets_the_collection() {
            let mut world = world();
            let skin = world.content.skins.skin_of(SWORD).expect("skin");
            world.transmog_arrived(1, true);
            let jobs = world.take_skin_jobs(NOW);
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].collect, [skin]);
            assert!(world.take_skin_jobs(NOW).is_empty(), "one job at a time");
            let answer = AccountSkins {
                account: ACCOUNT,
                skins: vec![skin],
            };
            world.skins_collected(&jobs[0], Ok(answer));
            let sent = messages_to(&tick(&mut world, NOW).messages, 1);
            let inventories: Vec<&InventoriesMsg> = sent
                .iter()
                .filter_map(|message| match message {
                    ServerMessage::Inventories(answer) => Some(answer),
                    _ => None,
                })
                .collect();
            assert_eq!(inventories.len(), 2, "the item pool and the collection");
            assert!(world.take_skin_jobs(NOW + SCAN_INTERVAL_MS).is_empty());
        }

        #[test]
        fn a_character_the_world_does_not_hold_is_not_asked_again() {
            let mut world = world();
            world.transmog_arrived(1, true);
            let jobs = world.take_skin_jobs(NOW);
            world.skins_collected(&jobs[0], Err(DbError::NoSuchCharacter));
            assert!(world.take_skin_jobs(NOW + RETRY_MS).is_empty());
        }

        #[test]
        fn a_collected_skin_shows_on_the_sword_and_goes_again() {
            let mut world = world();
            let skin = world.content.skins.skin_of(SWORD).expect("skin");
            world.transmog_arrived(1, false);
            let enable = TransmogClientMsg::EnableItemSkin {
                header: header(),
                slot: slot::MAIN_HAND_OPTION_1,
                skin,
            };
            world.transmog_call(&connection(1), enable.clone());
            assert_eq!(
                sword_look(&world).appearance,
                0,
                "the collection is not loaded"
            );
            let jobs = world.take_skin_jobs(NOW);
            world.skins_collected(
                &jobs[0],
                Ok(AccountSkins {
                    account: ACCOUNT,
                    skins: vec![skin],
                }),
            );
            world.transmog_call(&connection(1), enable);
            assert_eq!(sword_look(&world).appearance, skin);
            let sent = messages_to(&tick(&mut world, NOW).messages, 1);
            assert!(sent.iter().any(|message| matches!(
                message,
                ServerMessage::Transmog(TransmogServerMsg::EnableSkinSuccess { skin: shown, .. })
                    if *shown == skin
            )));
            world.transmog_call(
                &connection(1),
                TransmogClientMsg::DisableItemSkin {
                    header: header(),
                    slot: slot::MAIN_HAND_OPTION_1,
                },
            );
            assert_eq!(sword_look(&world).appearance, 0);
        }

        #[test]
        fn a_dye_takes_one_dye_item_per_new_color() {
            let mut world = world();
            let dye = |dye| TransmogClientMsg::PaperdollDye {
                header: header(),
                entries: vec![DyeEntry {
                    slot: slot::MAIN_HAND_OPTION_1,
                    dye,
                    skin: None,
                }],
                extra: 0,
                bag_slots: vec![0],
            };
            world.transmog_call(&connection(1), dye([DYE_COLOR, DYE_COLOR, 0, 0]));
            assert_eq!(sword_look(&world).dye, [DYE_COLOR, DYE_COLOR, 0, 0]);
            let entity = world.by_connection[&connection(1)];
            let flints = |world: &SimWorld| {
                world
                    .world
                    .get::<Player>(entity)
                    .and_then(|player| player.gear.as_ref())
                    .map(|gear| gear.bag_count(FLINT))
            };
            assert_eq!(flints(&world), Some(FLINTS - 2));
            let states = world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map(crate::world::gear::Gear::states);
            let Some(nw_messages::list_delta::ReplicatedList::Full(bag)) =
                states.and_then(|states| states.bag.items)
            else {
                panic!("a full bag list");
            };
            assert_eq!(
                bag.elements[0].sync_reason, SYNC_CRAFT_USED,
                "retail Hive_Dye 232.245 s"
            );
            // A store color and too few dyes change nothing.
            world.transmog_call(&connection(1), dye([9, 0, 0, 0]));
            world.transmog_call(&connection(1), dye([DYE_COLOR; DYE_BYTES]));
            assert_eq!(sword_look(&world).dye, [DYE_COLOR, DYE_COLOR, 0, 0]);
            assert_eq!(flints(&world), Some(FLINTS - 2));
        }
    }
}
