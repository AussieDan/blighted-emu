// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bus message types (M4 spec section 6.1). Game messages travel as envelope bytes; the gateway
//! does not read them.

use std::collections::BTreeMap;
use std::time::Duration;

use nw_messages::{LOOK_CHOICES, RAW_ID_LEN};
use nw_sim_core::Position;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client::BusError;
use crate::id::BusId;

/// One player connection: the gateway and the gateway's connection id. Ids of two gateways never
/// meet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionRef {
    /// The gateway.
    pub gateway: BusId,
    /// The connection id inside that gateway.
    pub connection: u64,
}

/// Gateway to director: a player with a checked ticket joins the world, or a player of a lost
/// sim resumes on another (M5 spec sections 3.1 and 3.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    /// The connection.
    pub connection: ConnectionRef,
    /// The account.
    pub account: Uuid,
    /// The character.
    pub character: Uuid,
    /// A resumed player; `None` for a new login.
    pub resume: Option<ResumeOf>,
}

/// What a resumed player keeps (M5 spec section 3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeOf {
    /// Its entity number.
    pub entity: u64,
    /// The handoff count its new sim starts from: far above the count of the lost sim, so a late
    /// route of the lost sim loses at the gateway.
    pub handoffs: u64,
    /// Bundles the gateway sent to the player so far; the new sim goes on with the sequence.
    pub bundles_sent: u64,
}

/// The answer to [`Join`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinReply {
    /// The character is in the world.
    Joined {
        /// The sim that has the player.
        sim: BusId,
        /// That sim's generation.
        generation: u64,
        /// The player's entity number.
        entity: u64,
        /// Whether the account has the game master rank: its client gets the GM flag of the
        /// registration response.
        game_master: bool,
    },
    /// The character cannot join; the gateway sends registration status 1.
    Refused {
        /// Why, for the log.
        reason: String,
    },
}

/// Gateway to sim, after a join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimInput {
    /// One client message envelope, unchanged.
    ClientMessage {
        /// The connection.
        connection: ConnectionRef,
        /// The envelope bytes.
        envelope: Vec<u8>,
    },
    /// The connection is gone.
    Left {
        /// The connection.
        connection: ConnectionRef,
    },
}

/// Sim to gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayOutput {
    /// Server message envelopes for one connection, on one carrier channel.
    ServerMessages {
        /// The connection id inside the gateway.
        connection: u64,
        /// The carrier channel.
        channel: u8,
        /// Whether the carrier sends them reliably.
        reliable: bool,
        /// The envelope bytes, in order.
        envelopes: Vec<Vec<u8>>,
    },
    /// Close the connection.
    Close {
        /// The connection id inside the gateway.
        connection: u64,
    },
    /// The connection's player moved to another sim by a handoff (M5 spec section 3.3).
    Route {
        /// The connection id inside the gateway.
        connection: u64,
        /// The sim that has the player now.
        sim: BusId,
        /// That sim's generation.
        generation: u64,
        /// The player's handoff count; only a higher one changes the route.
        sequence: u64,
    },
}

/// A world position in metres, as `sim` keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
    /// Up.
    pub z: f32,
}

impl Point {
    /// Whether every coordinate is a finite number.
    pub const fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl From<Position> for Point {
    fn from(position: Position) -> Self {
        Self {
            x: position.x,
            y: position.y,
            z: position.z,
        }
    }
}

impl From<Point> for Position {
    fn from(point: Point) -> Self {
        Self {
            x: point.x,
            y: point.y,
            z: point.z,
        }
    }
}

/// Director to sim: take a player (M5 spec section 3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Admit {
    /// The connection.
    pub connection: ConnectionRef,
    /// The account.
    pub account: Uuid,
    /// The character.
    pub character: Uuid,
    /// The character's name, for the admin snapshot.
    pub name: String,
    /// The entity number the director chose.
    pub entity: u64,
    /// Where the player appears.
    pub position: Point,
    /// Whether to send the login messages; a resumed player already has them.
    pub login: bool,
    /// The handoff count the player starts with: 0, or the count of a resume.
    pub handoffs: u64,
    /// The bundle sequence the player starts with: 0, or the count of a resume.
    pub bundles_sent: u64,
    /// The character's saved look (`nw_messages::PlayerLook` choices); `None` when the saved
    /// creation settings do not give one.
    pub look: Option<[u8; LOOK_CHOICES]>,
    /// The backstory id of the character's archetype pick (`Archetype_Mystic`, ...), which
    /// names its starter kit; `None` when the saved creation settings do not give one.
    pub backstory: Option<String>,
    /// The character's saved items; `None` when it never saved items (it gets its starter kit).
    pub items: Option<Vec<CarriedItem>>,
    /// The character's storage chests and saved loadouts; empty when it never saved any.
    #[serde(default)]
    pub inventory: PlayerInventory,
    /// The character's saved quests; empty when it never saved any.
    pub quests: PlayerQuests,
    /// The character's saved progression; empty when it never saved any.
    #[serde(default)]
    pub progression: PlayerProgression,
    /// Whether the account has the game master rank: the sim takes its client's GM messages.
    #[serde(default)]
    pub game_master: bool,
}

/// The answer to [`Admit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmitReply {
    /// The player is in the world.
    Admitted,
    /// The sim cannot take the player.
    Refused {
        /// Why, for the log.
        reason: String,
    },
}

/// A live sim in the cell table. `generation` is its `started_at_ms`; a restart is a new one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimSlot {
    /// The sim.
    pub id: BusId,
    /// Its start time, Unix milliseconds.
    pub generation: u64,
}

/// The director's cell table (M5 spec section 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellTable {
    /// Rises with every change of one director run.
    pub version: u64,
    /// Edge length of a cell, in metres.
    pub cell_size_m: f32,
    /// Columns of the grid.
    pub columns: u32,
    /// Rows of the grid.
    pub rows: u32,
    /// The live sims.
    pub sims: Vec<SimSlot>,
    /// The owner of every cell, row by row, as an index into `sims`.
    pub owners: Vec<Option<u16>>,
}

/// Where a carried item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemPlace {
    /// Worn in this paperdoll slot.
    Worn(u32),
    /// In this bag slot.
    Bag(u16),
}

/// One item stack a player carries to another sim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedItem {
    /// The item id (`MasterItemDefinitions` `ItemID`).
    pub item_id: String,
    /// Stack size.
    pub quantity: u32,
    /// Where it is.
    pub place: ItemPlace,
    /// What the stack keeps beyond its id and quantity.
    #[serde(default)]
    pub state: ItemState,
}

/// What an item stack keeps beyond its id and quantity (world migrations 0011 and 0017). A
/// `None` keeps the item's data value: a stack saved before it kept its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    /// The instance id the client names the stack by; `None`: the id the sim derives from the
    /// character and the item id (a stack saved before it kept one).
    pub instance: Option<[u8; RAW_ID_LEN]>,
    /// Whether its owner locked it (2549).
    pub locked: bool,
    /// Its rolled or upgraded gear score; `None`: the item's data gear score.
    pub gear_score: Option<u32>,
    /// Its current durability; `None`: full.
    pub durability: Option<u32>,
    /// Its perk ids (`PerkData` `PerkID`) by perk slot, no gaps; `None`: the item's fixed perks.
    pub perks: Option<Vec<String>>,
    /// Whether it is bound to the character; `None`: the item's `BindOnPickup`.
    pub bound: Option<bool>,
    /// Its look: dye, applied skin and skin dye.
    pub look: ItemLookState,
}

/// How an item looks beyond its item: the `DyeColorData` indexes of its four dye channels, the
/// skin applied over it (CRC-32 of the lowercase appearance id, 0 for none) and that skin's dye.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemLookState {
    /// The item's dye, channels R, G, B, A; 0 is no dye.
    pub dye: [u8; DYE_CHANNELS],
    /// The applied skin; 0 is none.
    pub appearance: u32,
    /// The applied skin's dye.
    pub skin_dye: [u8; DYE_CHANNELS],
}

/// Dye channels of an item (R, G, B, A).
pub const DYE_CHANNELS: usize = 4;

/// One item stack in one of a player's storage chests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredItem {
    /// The storage: the entity id of its chest as the client names it.
    pub storage: [u8; RAW_ID_LEN],
    /// The slot of the storage's container.
    pub slot: u16,
    /// The item id (`MasterItemDefinitions` `ItemID`).
    pub item_id: String,
    /// Stack size.
    pub quantity: u32,
    /// What the stack keeps beyond its id and quantity.
    pub state: ItemState,
}

/// One saved loadout (gear set) of a player; loadout 0, the worn items, is not saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLoadout {
    /// Its place in the loadout list, 1 or more.
    pub index: u8,
    /// Its name as the client shows it (`@lo_name <index>` until the player renames it).
    pub name: String,
    /// Each paperdoll slot it fills and the instance id of the item that goes there.
    pub items: Vec<(u32, [u8; RAW_ID_LEN])>,
    /// The attribute points saved with it (attribute, points).
    pub attributes: Vec<(u32, u32)>,
}

/// A player's items besides what it wears and carries: its storage chests and its saved
/// loadouts, as a join, a handoff and a save carry them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerInventory {
    /// The items of its storage chests.
    pub stored: Vec<StoredItem>,
    /// Its saved loadouts.
    pub loadouts: Vec<SavedLoadout>,
}

/// One player moving to another sim (M5 spec section 3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffOffer {
    /// The connection.
    pub connection: ConnectionRef,
    /// The character.
    pub character: Uuid,
    /// The character's name, for the admin snapshot.
    pub name: String,
    /// The entity number; it stays the same.
    pub entity: u64,
    /// The entity's raw id (`GdeRef`), which the client knows it by; it stays the same.
    pub gde_ref: [u8; RAW_ID_LEN],
    /// The last good position.
    pub position: Point,
    /// Milliseconds since the last good move.
    pub move_age_ms: u64,
    /// The movement budget left, in metres.
    pub budget_m: f32,
    /// Bundles sent to the player so far; the sequence goes on.
    pub bundles_sent: u64,
    /// The client's entity slots: each entity in view and its slot (`interestId`). The taker
    /// keeps them, so the client's slots stay tied to the same entities.
    pub slots: Vec<(u64, u16)>,
    /// Slots that left the view and are not stopped on the client yet.
    pub pending_stops: Vec<u16>,
    /// Milliseconds since the last `TimeSynchMsg`.
    pub time_synch_age_ms: u64,
    /// Handoffs of this player so far, this one included; the gateway's route sequence.
    pub handoffs: u64,
    /// The player's health; a handoff never heals (M6-A).
    pub health: u32,
    /// The character's look, as in [`Admit::look`].
    pub look: Option<[u8; LOOK_CHOICES]>,
    /// The character's backstory, as in [`Admit::backstory`].
    pub backstory: Option<String>,
    /// The sequence of the player's last phasing update; the next one goes on from it.
    pub phasing_sequence: u8,
    /// The client-context id of the player's current level; every bundle carries it.
    pub client_context: u8,
    /// Whether the player enters the taker's level from another level (the end of the
    /// tutorial): the taker sends the level change and a new spawn state, with the next
    /// client-context id.
    pub enter_level: bool,
    /// The items the player carries; `None` when the sender kept none, so the taker gives the
    /// login kit of the backstory.
    pub items: Option<Vec<CarriedItem>>,
    /// The player's storage chests and saved loadouts.
    #[serde(default)]
    pub inventory: PlayerInventory,
    /// The main-hand option slot the player holds drawn, when it holds one (live 2026-09-18:
    /// without it the open world took the slot-24 blunderbuss as the weapon in the hand and
    /// dropped every greatsword swing).
    #[serde(default)]
    pub drawn_weapon: Option<u32>,
    /// The main-hand option slot the player has selected, kept while every weapon is sheathed:
    /// a press of attack with every weapon sheathed draws it (retail, 334 of 334 such draws).
    #[serde(default)]
    pub active_weapon: Option<u32>,
    /// The versions of the newest weapon mastery (911) and point pool (3681) lists the client
    /// has; the taker's full lists go on above them (retail's list versions only rise).
    #[serde(default)]
    pub mastery_versions: (u64, u64),
    /// The version of the newest attribute bonus lists (type 129) the client has; the taker's
    /// full lists go on above it.
    #[serde(default)]
    pub attribute_version: u64,
    /// The sequence of the next fishing step (type 5338): the client takes only steps from the
    /// one it expects, so the taker's steps go on from it.
    #[serde(default)]
    pub fishing_sequence: u16,
    /// Whether the account has the game master rank, as in [`Admit::game_master`].
    #[serde(default)]
    pub game_master: bool,
    /// The player's quests: the active one with its run, and the finished ones.
    #[serde(default)]
    pub quests: PlayerQuests,
    /// The player's experience, coins, achievements and loot limit counts. A taker that changes
    /// the level keeps the achievements (retail: the open world's first full achievement state
    /// held the tutorial's 54 bits).
    #[serde(default)]
    pub progression: PlayerProgression,
    /// The version of the newest achievement state the client has; the taker's first full state
    /// goes on above it (Inferred, as for the quest tracker: the tutorial's changes ended at
    /// version 25 and the beach sim sent its first full state at 3; retail's versions only rise,
    /// 12065 in the tutorial and 13586 on the beach, live 2026-09-18).
    #[serde(default)]
    pub achievement_version: u64,
}

/// A player's quests (admin studio spec section 6.3), as a join or a handoff carries them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerQuests {
    /// The running objectives (their ids) and their runs: in the order they started, or by id
    /// after a load.
    pub active: Vec<(String, QuestRun)>,
    /// The ids of the finished objectives.
    pub done: Vec<String>,
    /// The version of the newest quest tracker lists the client has; the next sim's lists go on
    /// above it, with or without an active quest (live 2026-09-18: the tutorial's lists ended at
    /// version 44 and the beach sim started at 2, so the client dropped every beach quest list,
    /// never tracked the accepted quest and never offered its campfire).
    #[serde(default)]
    pub tracker_version: u64,
}

/// A player's progression besides its items and quests (world migration 0006), as a join or a
/// handoff carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerProgression {
    /// Its experience points (kills and quest rewards give some).
    pub experience: u32,
    /// Its coins (quest rewards give some).
    pub coins: u64,
    /// Its set achievement bits, ascending.
    pub achievements: Vec<u32>,
    /// Its loot limit counts, ended ones too, by limit id.
    pub loot_limits: Vec<LootLimitCount>,
    /// The total experience of each weapon mastery, by its `CategoricalProgressionData` id
    /// (world migration 0007).
    pub masteries: BTreeMap<String, u32>,
    /// The attribute points spent on each attribute (`CharacterAttributeType` id, points), in
    /// the order they were first spent (world migration 0008).
    #[serde(default)]
    pub attributes: Vec<(u32, u32)>,
    /// The level of each weapon progression point (`ProgressionPointId`, level), in the order
    /// they were first assigned (world migration 0008).
    #[serde(default)]
    pub weapon_points: Vec<(String, u16)>,
    /// The territory standing points spent: pool and point (both CRC-32 wire ids of the
    /// 3681 point list) and the level, in the order they were first spent (world migration
    /// 0021).
    #[serde(default)]
    pub territory_points: Vec<(u32, u32, u16)>,
    /// Its travel and housing state (world migration 0010).
    #[serde(default)]
    pub travel: PlayerTravel,
    /// The mapped ability slots: the weapon mastery of the tree, the slot (0 to 2) and the
    /// ability's `ProgressionPointId` (world migration 0013).
    #[serde(default)]
    pub ability_slots: Vec<(String, u8, String)>,
    /// The meta achievement counters (kills, gathers, deaths), by lowercase counter key (world
    /// migration 0013).
    #[serde(default)]
    pub meta_counters: BTreeMap<String, u64>,
    /// The completed meta achievements (`MetaAchievementId`), in completion order, each with
    /// whether its reward still waits for the player to see it (world migration 0013).
    #[serde(default)]
    pub meta_completed: Vec<(String, bool)>,
    /// The title it shows (CRC-32 of the lowercase `TitleID`); `None` for none (world
    /// migration 0013).
    #[serde(default)]
    pub title: Option<u32>,
    /// Its chosen mount and its dyed or named mounts (world migration 0012).
    #[serde(default)]
    pub mounts: PlayerMounts,
    /// Its expedition, raid and trial state: counters, cooldowns and its instance stay (world
    /// migration 0015).
    #[serde(default)]
    pub arena: crate::instance::PlayerArena,
    /// Its musical performance lockout (world migration 0018).
    #[serde(default)]
    pub music: PlayerMusic,
    /// How often each event notification was shown to it: (CRC-32 of the lowercase
    /// `NotificationID`, times shown), ascending (world migration 0022).
    #[serde(default)]
    pub notifications: Vec<(u32, u32)>,
}

/// A player's musical performance lockout (world migration 0018), as a join, a handoff and a save
/// carry it: the client's `MusicalPerformancePlayerPersistence` `m_nextPerformanceStartTime`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerMusic {
    /// Server time (ms since 2000-01-01) from which it may create a performance again; `None`
    /// for no lockout.
    pub next_performance_ms: Option<u64>,
}

/// Bytes of a mount dye: one color index per mask channel, R, G, B, A.
pub const MOUNT_DYE_LEN: usize = 4;

/// A player's mounts (world migration 0012), as a join, a handoff and a save carry them: the
/// client's `MountComponentPersistentData`. The mounts it owns are its achievements'.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerMounts {
    /// The chosen mount (the CRC-32 of its lowercase `MountId`); `None` for none.
    pub selected: Option<u32>,
    /// The mounts with a dye or a name, in the order first customized.
    pub customized: Vec<CarriedMount>,
    /// The mount it rides: a handoff carries the ride to the next sim; a save does not keep it
    /// (a login starts on foot, as retail's login cards).
    #[serde(default)]
    pub riding: Option<u32>,
}

/// One mount with a dye or a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedMount {
    /// The mount id.
    pub mount_id: u32,
    /// The color index of each mask channel, R, G, B, A (0 for none).
    pub dye: [u8; MOUNT_DYE_LEN],
    /// The name bytes the client sent (empty for none).
    pub name: Vec<u8>,
}

/// A world position kept as the bit patterns of its metres (`f32::to_bits`): a travel state then
/// compares whole and keeps every bit through a handoff and a save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoredPoint([u32; 3]);

impl StoredPoint {
    /// The stored form of `point`.
    pub const fn of(point: Point) -> Self {
        Self([point.x.to_bits(), point.y.to_bits(), point.z.to_bits()])
    }

    /// The position.
    pub const fn point(self) -> Point {
        Point {
            x: f32::from_bits(self.0[0]),
            y: f32::from_bits(self.0[1]),
            z: f32::from_bits(self.0[2]),
        }
    }

    /// The point of the bit patterns `bits` (x, y, z), as a save stores them.
    pub const fn from_bits(bits: [u32; 3]) -> Self {
        Self(bits)
    }

    /// The bit patterns (x, y, z), as a save stores them.
    pub const fn bits(self) -> [u32; 3] {
        self.0
    }
}

/// A player's travel and housing state (world migration 0010), as a join, a handoff and a save
/// carry it. Times are Unix milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerTravel {
    /// The map marker its client set (2728); `None` when it has none.
    pub waypoint: Option<StoredPoint>,
    /// The inn it checked in at (interaction 18, `SetSpawnPoint`): the facet entity of the inn's
    /// spawn point, the id its client names in a respawn request (1938).
    pub inn: Option<u64>,
    /// The respawn camp it bound to (interaction 73, `BindToCamp`): its facet entity, the id its
    /// client names in a respawn request (1938).
    pub camp: Option<u64>,
    /// The skin of its camp (742, `CampSkinData` id as its CRC-32).
    pub camp_skin: Option<u32>,
    /// When it may ask to be unstuck again (388); 0 for at once.
    pub unstuck_ready_ms: u64,
    /// The homes it owns.
    pub homes: Vec<OwnedHome>,
    /// Its played time before this session, ms (the player time state, type 3290).
    pub played_ms: u64,
    /// When it last recalled to its inn (3713 to its inn), Unix ms; 0 for never.
    pub inn_recalled_ms: u64,
    /// The camp it placed (2236).
    pub placed_camp: Option<PlacedCamp>,
}

/// A camp a player placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedCamp {
    /// Where it stands.
    pub position: StoredPoint,
    /// Its tier (the player's `Camping` rank when placed).
    pub tier: u8,
    /// When it was placed, Unix ms.
    pub placed_ms: u64,
}

/// The housing state of a home beyond its place and furniture (world migration 0020).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeRecord {
    /// The plot's facet entity, as its client names it.
    pub plot_facet: u64,
    /// The third id of the plot reference, as its client names it (meaning Unknown).
    pub plot_extra: u64,
    /// The house data entity's raw id (544); all zero until the client named it.
    pub house_data: [u8; RAW_ID_LEN],
    /// The house data entity's facet entity.
    pub house_data_facet: u64,
    /// The house type: the CRC-32 of the lowercase `HouseTypeID`.
    pub house_type: u32,
    /// The plot's `TerritoryID`.
    pub territory: u16,
    /// When the taxes are due, Unix ms.
    pub taxes_due_ms: u64,
    /// When the fast travel home may be used again, Unix ms; 0 for at once.
    pub fast_travel_ready_ms: u64,
}

/// A home a player owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedHome {
    /// The raw id of the house plot entity, as its client names it (3712, 3716, 2149).
    pub plot: [u8; RAW_ID_LEN],
    /// Where the player stood when it bought the home: where a fast travel home puts it.
    pub position: StoredPoint,
    /// When it bought the home.
    pub purchased_ms: u64,
    /// Its placed furniture, by item index.
    pub items: Vec<HousingItem>,
    /// Its housing state.
    pub record: HomeRecord,
}

/// One piece of furniture placed in a home (3277).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HousingItem {
    /// Its index in the home, which the client's move and remove calls name.
    pub index: u16,
    /// The item the client placed, as its placement call names it.
    pub item: u32,
    /// Its position in the home's frame, metres.
    pub position: StoredPoint,
    /// Its rotation quaternion (x, y, z, w) as `f32` bit patterns.
    pub rotation: [u32; 4],
}

/// One loot limit's count of a player (`nw_content::loot_limits`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LootLimitCount {
    /// The lowercase `LootLimitID`.
    pub limit_id: String,
    /// The count.
    pub count: u64,
    /// Unix nanoseconds of its last count.
    pub counted_ns: u64,
    /// Unix nanoseconds when it goes back to 0.
    pub ends_ns: u64,
}

/// A player's run of a quest, as a handoff carries it to the next sim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestRun {
    /// The state of each task of the quest's tree, in tree order.
    pub tasks: Vec<QuestTask>,
    /// The tasks with progress (their tree index), in the order they first had it.
    pub listed: Vec<u32>,
    /// A mission's take; `None` for a quest.
    #[serde(default)]
    pub mission: Option<MissionTake>,
}

/// Where and when a player took a mission: what its journal record names (finding "mission
/// board": retail `m_destinationOverride` = the board's id, `m_acceptTime`,
/// `m_originTerritoryId`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionTake {
    /// The raw id of the board it was taken at.
    pub board: [u8; MISSION_BOARD_ID_LEN],
    /// When it was taken: ns since 1970-01-01.
    pub accepted_ns: u64,
    /// The territory id the record names.
    pub territory: u16,
}

/// Bytes of a board's raw id.
pub const MISSION_BOARD_ID_LEN: usize = 16;

/// The state of one task of a [`QuestRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestTask {
    /// Not started, or stopped.
    Idle,
    /// Waiting: kills or items so far, and the time left on its timer.
    Active {
        /// Kills or items so far.
        progress: u32,
        /// Milliseconds until its timer ends; `None` without a timer.
        due_in_ms: Option<u64>,
    },
    /// Done.
    Done,
}

/// Sim to sim, on the taker's handoff subject. Both go on one subject, so NATS keeps their order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HandoffMessage {
    /// Take this player (request/reply).
    Offer(Box<HandoffOffer>),
    /// The offer of this connection timed out or its player left; undo a late take.
    Cancel {
        /// The connection.
        connection: ConnectionRef,
        /// The `handoffs` of that offer; a take of another attempt stays.
        handoffs: u64,
    },
}

/// The answer to [`HandoffMessage::Offer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandoffReply {
    /// The player is in the taker's world.
    Accepted,
    /// The taker cannot take the player.
    Refused {
        /// Why, for the log.
        reason: String,
    },
}

/// Largest bus message: the NATS default payload limit (M4 spec section 6.3).
pub const MAX_BUS_MESSAGE_BYTES: usize = 1024 * 1024;

/// First entity number of mobs (M6-A spec section 6.1): a mob is this plus its spawn index, and
/// players stay below it. Every mob number stays below 2^35, the most that the 5-byte prefix code
/// of a bundle member holds.
pub const MOB_ENTITY_BASE: u64 = 1 << 34;

/// Most bytes of one [`Ghost`] in `postcard`: a 10-byte entity number, three 4-byte floats, two
/// option tags, and the larger of a mob's state (five 5-byte numbers and a 1-byte flag) and a
/// player's ([`PlayerGhost`]: about 246 bytes with all its visible items, its view, target,
/// action and mount, and 13 more bytes per visible item for its look: a 5-byte skin and two
/// 4-byte dyes), and its upper slayer slots ([`GHOST_UPPER_SLOTS_BYTES`]).
pub const MAX_GHOST_BYTES: usize =
    288 + GHOST_VISIBLE_ITEMS * GHOST_ITEM_LOOK_BYTES + GHOST_UPPER_SLOTS_BYTES;
/// Most `postcard` bytes of one ghost item's look.
const GHOST_ITEM_LOOK_BYTES: usize = 13;
/// Most `postcard` bytes of an optional `u64`: the option tag and a 10-byte varint.
const OPTION_U64_BYTES: usize = 11;
/// Most `postcard` bytes of an optional `u16`: the option tag and a 3-byte varint.
const OPTION_U16_BYTES: usize = 4;
/// Most `postcard` bytes of one [`GhostSlotChange`]: its state, start and sequence.
const GHOST_SLOT_BYTES: usize = 2 * OPTION_U64_BYTES + OPTION_U16_BYTES;
/// Most `postcard` bytes of a player ghost's [`GhostUpperSlots`]: the option tag, three slots,
/// the marker and the order flag.
const GHOST_UPPER_SLOTS_BYTES: usize = 1 + 3 * GHOST_SLOT_BYTES + OPTION_U64_BYTES + 1;

/// The paperdoll slots that show on a character (armor and weapon options; the
/// `nw_messages::is_visible_slot` slots): a [`PlayerGhost`] carries at most one item for each.
pub const GHOST_VISIBLE_ITEMS: usize = 11;

/// One worn item that shows on a player ghost: its slot, item and look, all that the public
/// paperdoll of another player names (retail sends other players the public slot only: no
/// durability, gear score or instance; replication note; the look is the item visual's dye and
/// transmog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostItem {
    /// The paperdoll slot.
    pub slot: u8,
    /// The client item index.
    pub item_index: u32,
    /// Its dye and applied skin.
    pub look: ItemLookState,
}

/// What other players see of a player near a border (replication note): its look, health,
/// stance, block, facing, clock and the items that show on it, so the players of the other sim
/// see it move, turn, fight and change weapons, not a statue.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlayerGhost {
    /// The character.
    pub character: Uuid,
    /// The saved look.
    pub look: Option<[u8; LOOK_CHOICES]>,
    /// Health now.
    pub health: u32,
    /// Max health.
    pub max_health: u32,
    /// Its facing (yaw, radians), when known.
    pub facing: Option<f32>,
    /// The pitch of its view (radians, positive looks down), when known.
    pub pitch: Option<f32>,
    /// The entity it locks on or homes to (its newest input), when it has one.
    pub target: Option<u64>,
    /// The number of its ALC states from its client's input and the server time (ms since
    /// 2000-01-01) of the first state with it, while its client numbers its input records.
    pub state_number: Option<(u16, u64)>,
    /// It holds block.
    pub blocking: bool,
    /// The server time of its first own state (ms since 2000-01-01): its ALC clock.
    pub spawned_ms: u64,
    /// The version of its gear: a new one is a gear change.
    pub gear_version: u64,
    /// The main-hand option slot drawn.
    pub drawn: Option<u32>,
    /// The main-hand option slot selected, kept while every weapon is sheathed.
    pub active: Option<u32>,
    /// The drawn weapon is in the hand.
    pub attached: bool,
    /// It has its inventory open (`lookingThroughLoadout`).
    pub looking_through_loadout: bool,
    /// The mount it rides, as other players see it.
    pub mount: GhostMount,
    /// The items that show on it.
    pub items: [Option<GhostItem>; GHOST_VISIBLE_ITEMS],
    /// The action it plays (its slayer state), when it plays one.
    pub action: Option<GhostSlayer>,
    /// Its slayer slots past the first and the slayer marker (its block, aim, aimed shot,
    /// reload and draw), when it has a change of them.
    #[serde(default)]
    pub upper: Option<GhostUpperSlots>,
}

/// A change of one slayer slot past the first of a player ghost (ALC children 16 to 24).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostSlotChange {
    /// Its state (the second slot's as on the wire), when sent.
    pub state: Option<u64>,
    /// The ALC state number in which it starts, when sent.
    pub started: Option<u16>,
    /// Its sequence, when sent.
    pub sequence: Option<u64>,
}

/// The slayer slots past the first and the slayer marker of a player ghost (ALC children 16 to
/// 24 and 48): what other players see of its block, aim, aimed shot, reload and draw (Observed:
/// the retail duel capture's remote players carried these children as the own states do).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostUpperSlots {
    /// The second slot (children 16 to 18).
    pub second: GhostSlotChange,
    /// The third slot (children 19 to 21).
    pub third: GhostSlotChange,
    /// The fourth slot (children 22 to 24).
    pub fourth: GhostSlotChange,
    /// The slayer marker (child 48).
    pub marker: Option<u64>,
    /// They changed after its slayer state started: they take the place of its second slot.
    pub after_state: bool,
}

/// The mount of a player ghost, as other players see it (the mount state's `m_mountId` and slot
/// 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostMount {
    /// The mount it rides; 0 when it rides none.
    pub mount_id: u32,
    /// `mountRemoteFlags`.
    pub flags: u8,
    /// `remoteDyeData`.
    pub dye: [u8; MOUNT_DYE_LEN],
}

/// The slayer state of a player ghost: the action it plays (ALC children 13 to 18; slayer note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostSlayer {
    /// `slayerStateId`, when it changed with the action.
    pub state: Option<u64>,
    /// The ALC state number in which the action started; `None` for a sequence change in a
    /// state that goes on.
    pub started: Option<u16>,
    /// `slayerSequenceId`.
    pub sequence: u64,
    /// The second slot's start, when it goes with the action.
    pub second_started: Option<u16>,
    /// The second slot's state (as on the wire) and sequence, when it plays one.
    pub second_playing: Option<(u64, u64)>,
    /// The second slot's action stops ([`nw_messages`]' `SecondSlayerState::stopped`).
    pub second_stopped: bool,
}

/// The state of a mob ghost (M6-A spec section 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobGhost {
    /// Health now.
    pub health: u32,
    /// Max health.
    pub max_health: u32,
    /// The mob walks back to its spawn and takes no damage.
    pub returning: bool,
    /// crc32 of the lower-case vitals id (the world file's value).
    pub vitals_id: u32,
    /// crc32 of the lower-case vitals category id.
    pub category_id: u32,
    /// The spawn level.
    pub level: u32,
}

/// A read-only copy of an entity near a border.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ghost {
    /// The entity number.
    pub entity: u64,
    /// Its position.
    pub position: Point,
    /// A mob's state; `None` for a player.
    pub mob: Option<MobGhost>,
    /// A player's state, for the players who see it.
    #[serde(default)]
    pub player: Option<PlayerGhost>,
}

/// A hit on an entity of another sim (M6-A spec section 7.3), sent to the sim that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    /// The target's entity number.
    pub target: u64,
    /// The attacker's entity number.
    pub attacker: u64,
    /// The attacker's sim.
    pub attacker_sim: BusId,
    /// Health it takes.
    pub amount: u32,
    /// Threat it adds.
    pub threat: u64,
    /// A mob's hit on a player: the stamina its block takes, in thousandths (the player's sim
    /// runs the block and the dodge); `None` for a player's hit.
    #[serde(default)]
    pub block_stamina_milli: Option<u32>,
    /// A mob's hit by a projectile (only a weapon with `CanBlockRanged` blocks it).
    #[serde(default)]
    pub ranged: bool,
}

/// Thousandths per unit of [`Hit::block_stamina_milli`].
const MILLI_PER_UNIT: f32 = 1_000.0;

impl Hit {
    /// `stamina` in the thousandths of [`Hit::block_stamina_milli`].
    #[must_use]
    // A block takes at most the full stamina, never below 0.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn milli_of(stamina: f32) -> u32 {
        (stamina * MILLI_PER_UNIT).round() as u32
    }

    /// The stamina a block of this mob hit takes; `None` for a player's hit.
    #[must_use]
    // Thousandths of a stamina of at most 100.
    #[allow(clippy::cast_precision_loss)]
    pub fn block_stamina(&self) -> Option<f32> {
        self.block_stamina_milli
            .map(|milli| milli as f32 / MILLI_PER_UNIT)
    }
}

/// The ghosts of one sim for another, every tick (M5 spec section 3.4), in one or more parts
/// ([`crate::ghost_parts`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ghosts {
    /// The sender.
    pub from: BusId,
    /// Its players near the receiver's cells; an empty list clears the last one.
    pub entities: Vec<Ghost>,
    /// The raw id (`GdeRef`) of each entity in [`Ghosts::entities`], by entity number: the
    /// receiver's clients see the entity by the same id as the owner's.
    pub gde_refs: Vec<(u64, [u8; RAW_ID_LEN])>,
    /// The sender's ghost round: every part of one round has it.
    pub round: u64,
    /// This part of the round, from 0.
    pub part: u16,
    /// The parts of the round; at least 1.
    pub parts: u16,
}

/// A sim is alive, with its load for the director.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimHeartbeat {
    /// The sim.
    pub id: BusId,
    /// Unix milliseconds when the sim started.
    pub started_at_ms: u64,
    /// Players in its world.
    pub players: u32,
    /// Players it takes at most.
    pub max_players: u32,
    /// The level it runs (its world-file folder name, for example `newworld_vitaeeterna`). The
    /// director splits the open map over the sims of its own level and sends new characters to
    /// the sims of the tutorial level.
    pub level: String,
    /// `Some` for a sim with the instance role (it gets no cells; the director opens instances
    /// on it), with the instance it runs.
    #[serde(default)]
    pub instance: Option<crate::instance::InstanceBeat>,
}

/// Largest batch, in envelope bytes: well below [`MAX_BUS_MESSAGE_BYTES`] (M5 load spec
/// section 4).
pub const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Bytes of one batch item besides its envelopes: ids, flags and lengths.
const ITEM_OVERHEAD_BYTES: usize = 96;

/// Sim to gateway: the outputs of one tick for one gateway, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputBatch {
    /// The sim that sends them; it hears `Left` for a connection the gateway no longer has.
    pub sim: BusId,
    /// The outputs.
    pub outputs: Vec<GatewayOutput>,
}

/// Gateway (or an old sim, after a handoff) to sim: inputs in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBatch {
    /// Inputs that an old sim sends on after a handoff; a client message among them is dropped
    /// once the new sim has a direct input of that connection.
    pub forwarded: bool,
    /// The inputs.
    pub inputs: Vec<SimInput>,
}

impl GatewayOutput {
    /// About the bytes of this output in a batch.
    pub fn size_bytes(&self) -> usize {
        let envelopes = match self {
            Self::ServerMessages { envelopes, .. } => envelopes.iter().map(Vec::len).sum(),
            Self::Close { .. } | Self::Route { .. } => 0,
        };
        envelopes + ITEM_OVERHEAD_BYTES
    }
}

impl SimInput {
    /// About the bytes of this input in a batch.
    pub fn size_bytes(&self) -> usize {
        let envelope = match self {
            Self::ClientMessage { envelope, .. } => envelope.len(),
            Self::Left { .. } => 0,
        };
        envelope + ITEM_OVERHEAD_BYTES
    }
}

/// `items` in order, cut into batches of at most [`MAX_BATCH_BYTES`] by `size`. An item larger
/// than that is a batch of its own.
pub fn split_batches<T>(items: Vec<T>, size: impl Fn(&T) -> usize) -> Vec<Vec<T>> {
    let mut batches: Vec<Vec<T>> = Vec::new();
    let mut bytes = 0;
    for item in items {
        let item_bytes = size(&item);
        match batches.last_mut() {
            Some(batch) if bytes + item_bytes <= MAX_BATCH_BYTES => {
                bytes += item_bytes;
                batch.push(item);
            }
            _ => {
                bytes = item_bytes;
                batches.push(vec![item]);
            }
        }
    }
    batches
}

/// A process is alive. `started_at_ms` changes when the process restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// The process: a gateway id or a sim id.
    pub id: BusId,
    /// Unix milliseconds when the process started.
    pub started_at_ms: u64,
}

/// How long a process may stay silent before it counts as gone: `misses` heartbeat intervals. A
/// huge setting saturates instead of overflowing.
pub fn heartbeat_silence(interval: Duration, misses: u32) -> Duration {
    interval.saturating_mul(misses)
}

/// The `postcard` bytes of a bus message.
///
/// # Errors
///
/// Returns [`BusError::Encode`] when `postcard` fails.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, BusError> {
    postcard::to_allocvec(message).map_err(BusError::Encode)
}

/// A bus message from its `postcard` bytes.
///
/// # Errors
///
/// Returns [`BusError::Decode`] for bytes that are not that message.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, BusError> {
    postcard::from_bytes(bytes).map_err(BusError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTION: u64 = 7;
    /// A raw id of the dynamic form (bit 0 of the first byte clear).
    const GDE_REF: [u8; RAW_ID_LEN] = [
        0x42, 0x9c, 0x17, 0xe3, 0x05, 0xb8, 0x6a, 0xd1, 0x3f, 0x80, 0x2e, 0xc4, 0x91, 0x0b, 0x7d,
        0x56,
    ];
    const STARTED_AT_MS: u64 = 1_800_000_000_000;
    const MISSES: u32 = 3;

    fn connection() -> ConnectionRef {
        ConnectionRef {
            gateway: BusId::try_from("gw-1".to_owned()).expect("id"),
            connection: CONNECTION,
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(message: T) {
        let bytes = encode(&message).expect("encode");
        assert_eq!(decode::<T>(&bytes).expect("decode"), message);
    }

    #[test]
    fn every_bus_message_round_trips() {
        round_trip(Join {
            connection: connection(),
            account: Uuid::new_v4(),
            character: Uuid::new_v4(),
            resume: Some(ResumeOf {
                entity: CONNECTION,
                handoffs: 1 << 32,
                bundles_sent: 7,
            }),
        });
        round_trip(JoinReply::Joined {
            sim: connection().gateway,
            generation: STARTED_AT_MS,
            entity: CONNECTION,
            game_master: true,
        });
        round_trip(JoinReply::Refused {
            reason: "no position".to_owned(),
        });
        round_trip(SimInput::ClientMessage {
            connection: connection(),
            envelope: vec![1, 2, 3],
        });
        round_trip(SimInput::Left {
            connection: connection(),
        });
        round_trip(GatewayOutput::ServerMessages {
            connection: CONNECTION,
            channel: 1,
            reliable: false,
            envelopes: vec![vec![4], vec![5, 6]],
        });
        round_trip(GatewayOutput::Close {
            connection: CONNECTION,
        });
        round_trip(GatewayOutput::Route {
            connection: CONNECTION,
            sim: connection().gateway,
            generation: STARTED_AT_MS,
            sequence: 2,
        });
        round_trip(Heartbeat {
            id: connection().gateway,
            started_at_ms: STARTED_AT_MS,
        });
    }

    fn sim() -> BusId {
        BusId::try_from("sim-1".to_owned()).expect("id")
    }

    const LOOK: [u8; LOOK_CHOICES] = [0, 4, 0, 64, 43, 4, 4, 11, 0, 5, 25, 14];

    const POINT: Point = Point {
        x: 1.5,
        y: -2.0,
        z: 300.25,
    };

    /// A progression with every field set: the tutorial's experience and achievements, and the
    /// retail newchar loot limit count.
    fn progression() -> PlayerProgression {
        PlayerProgression {
            experience: 1_250,
            coins: 375,
            achievements: vec![14_809, 15_317],
            loot_limits: vec![LootLimitCount {
                limit_id: "2hwarhammer_ftue_corruptedsailer_v2".to_owned(),
                count: 1,
                counted_ns: 1_792_696_951_894_621_000,
                ends_ns: 1_793_091_600_894_621_000,
            }],
            masteries: BTreeMap::from([("GreatSwordAbilityTable".to_owned(), 20)]),
            attributes: vec![(2, 15)],
            weapon_points: vec![("Ability_Greatsword_Combo".to_owned(), 1)],
            territory_points: vec![(0xd65a_1577, 0x1234_5678, 3)],
            travel: travel(),
            mounts: PlayerMounts {
                selected: Some(u32::MAX),
                customized: vec![CarriedMount {
                    mount_id: u32::MAX,
                    dye: [u8::MAX; MOUNT_DYE_LEN],
                    name: b"Rex".to_vec(),
                }],
                riding: Some(u32::MAX),
            },
            ability_slots: vec![(
                "GreatSwordAbilityTable".to_owned(),
                0,
                "Ability_Greatsword_Combo".to_owned(),
            )],
            meta_counters: BTreeMap::from([("pve_totalkills".to_owned(), u64::MAX)]),
            meta_completed: vec![("CharacterProgression1".to_owned(), true)],
            title: Some(u32::MAX),
            arena: arena(),
            music: PlayerMusic {
                next_performance_ms: Some(u64::MAX),
            },
            notifications: vec![(u32::MAX, u32::MAX)],
        }
    }

    /// An arena state with every field set: the counters of a retail expedition entry.
    fn arena() -> crate::instance::PlayerArena {
        crate::instance::PlayerArena {
            dungeon_cooldown_since_2000_ns: u64::MAX,
            solo_trial_cooldown_since_2000_ns: 1,
            ranks: vec![3, 3, 3],
            last_dungeon: Some([0x2d; 16]),
            last_mutated_dungeon: Some([0x7d; 16]),
            base_entered: 1,
            mutated_entered: u32::MAX,
            group_trials_entered: 2,
            next_base_refresh_ns: 1_792_746_000_226_611_400,
            next_mutated_refresh_ns: 1_793_091_600_226_670_700,
            next_group_trial_refresh_ns: 1_792_746_000_226_725_900,
            mutation_award: true,
            last_dungeon_mutated: true,
            stay: Some(crate::instance::InstanceStay {
                instance: Uuid::from_u128(u128::MAX),
                game_mode: "DungeonAmrine".to_owned(),
                phase: u64::MAX,
                return_level: "newworld_vitaeeterna".to_owned(),
                return_position: StoredPoint::from_bits([u32::MAX, 0, 1]),
                activated_ns: u64::MAX,
                mutation_difficulty: Some(u8::MAX),
            }),
        }
    }

    /// A travel state with every field set.
    fn travel() -> PlayerTravel {
        PlayerTravel {
            waypoint: Some(StoredPoint::of(POINT)),
            inn: Some(17_680_737_194_352_937_251),
            camp: Some(16_695_474_225_659_806_323),
            camp_skin: Some(0x3e89_166b),
            unstuck_ready_ms: 1_790_860_734_747,
            homes: vec![OwnedHome {
                plot: [7; RAW_ID_LEN],
                position: StoredPoint::of(POINT),
                purchased_ms: 1_783_814_228_305,
                items: vec![HousingItem {
                    index: 24,
                    item: 0xb39e,
                    position: StoredPoint::of(POINT),
                    rotation: [0, 0, 0.967_457_f32.to_bits(), 0.255_766_f32.to_bits()],
                }],
                record: HomeRecord {
                    plot_facet: 0x6a41_43a0_cd66_37c7,
                    plot_extra: 0x99ed_e7e1_d5a5_b24d,
                    house_data: [9; RAW_ID_LEN],
                    house_data_facet: 0xc13e_b7f0_5396_3b4c,
                    house_type: 0xd0e4_0188,
                    territory: 16,
                    taxes_due_ms: 1_784_418_228_305,
                    fast_travel_ready_ms: 1_783_828_628_305,
                },
            }],
            played_ms: 448_201,
            inn_recalled_ms: 1_790_860_000_000,
            placed_camp: Some(PlacedCamp {
                position: StoredPoint::of(POINT),
                tier: 2,
                placed_ms: 1_790_861_000_000,
            }),
        }
    }

    #[test]
    fn a_stored_point_keeps_every_bit() {
        let odd = Point {
            x: -0.0,
            y: f32::MIN_POSITIVE,
            z: 14_335.875,
        };
        let back = StoredPoint::of(odd).point();
        assert_eq!(
            [back.x.to_bits(), back.y.to_bits(), back.z.to_bits()],
            [odd.x.to_bits(), odd.y.to_bits(), odd.z.to_bits()]
        );
        assert_eq!(StoredPoint::of(POINT).point(), POINT);
    }

    #[test]
    fn every_m5_message_round_trips() {
        round_trip(Admit {
            connection: connection(),
            account: Uuid::new_v4(),
            character: Uuid::new_v4(),
            name: "Ada".to_owned(),
            entity: CONNECTION,
            position: POINT,
            login: true,
            handoffs: 0,
            bundles_sent: 0,
            look: Some(LOOK),
            backstory: Some("Archetype_Mystic".to_owned()),
            items: Some(vec![CarriedItem {
                item_id: "PotionHealthT1".to_owned(),
                quantity: 5,
                place: ItemPlace::Bag(0),
                state: ItemState {
                    instance: Some(GDE_REF),
                    locked: true,
                    gear_score: Some(590),
                    durability: Some(1200),
                    perks: Some(vec!["PerkID_StatSwap_BaseDamage".to_owned()]),
                    bound: Some(true),
                    look: ItemLookState {
                        dye: [4, 0, 184, 0],
                        appearance: u32::MAX,
                        skin_dye: [0, 5, 0, 0],
                    },
                },
            }]),
            inventory: PlayerInventory {
                stored: vec![StoredItem {
                    storage: GDE_REF,
                    slot: 3,
                    item_id: "FlintT1".to_owned(),
                    quantity: 12,
                    state: ItemState::default(),
                }],
                loadouts: vec![SavedLoadout {
                    index: 1,
                    name: "@lo_name 1".to_owned(),
                    items: vec![(24, GDE_REF)],
                    attributes: vec![(0, 150), (1, 350)],
                }],
            },
            quests: PlayerQuests::default(),
            progression: progression(),
            game_master: false,
        });
        round_trip(AdmitReply::Admitted);
        round_trip(AdmitReply::Refused {
            reason: "full".to_owned(),
        });
        round_trip(CellTable {
            version: 3,
            cell_size_m: 2048.0,
            columns: 2,
            rows: 1,
            sims: vec![SimSlot {
                id: sim(),
                generation: STARTED_AT_MS,
            }],
            owners: vec![Some(0), None],
        });
        let offer = HandoffOffer {
            connection: connection(),
            character: Uuid::new_v4(),
            name: "Ada".to_owned(),
            entity: CONNECTION,
            gde_ref: GDE_REF,
            position: POINT,
            move_age_ms: 40,
            budget_m: 3.5,
            bundles_sent: 900,
            slots: vec![(CONNECTION + 1, 3)],
            pending_stops: vec![4],
            time_synch_age_ms: 1200,
            handoffs: 2,
            health: 500,
            look: None,
            backstory: None,
            phasing_sequence: 2,
            client_context: 1,
            enter_level: false,
            progression: progression(),
            mastery_versions: (12, 7),
            attribute_version: 9,
            fishing_sequence: 4,
            game_master: true,
            achievement_version: 25,
            items: Some(vec![CarriedItem {
                item_id: "PotionHealthT1".to_owned(),
                quantity: 2,
                place: ItemPlace::Worn(14),
                state: ItemState::default(),
            }]),
            inventory: PlayerInventory::default(),
            drawn_weapon: Some(25),
            active_weapon: Some(25),
            quests: PlayerQuests {
                active: vec![(
                    "12_GetOffBeach".to_owned(),
                    QuestRun {
                        tasks: vec![
                            QuestTask::Done,
                            QuestTask::Active {
                                progress: 1,
                                due_in_ms: Some(500),
                            },
                            QuestTask::Idle,
                        ],
                        listed: vec![0, 1],
                        mission: Some(MissionTake {
                            board: [0x81; MISSION_BOARD_ID_LEN],
                            accepted_ns: 1_798_761_600_000_000_000,
                            territory: 300,
                        }),
                    },
                )],
                done: vec!["01_SoleSurvivor2".to_owned()],
                tracker_version: 7,
            },
        };
        round_trip(HandoffMessage::Offer(Box::new(offer)));
        round_trip(HandoffMessage::Cancel {
            connection: connection(),
            handoffs: 2,
        });
        round_trip(HandoffReply::Accepted);
        round_trip(HandoffReply::Refused {
            reason: "already here".to_owned(),
        });
        round_trip(Ghosts {
            from: sim(),
            entities: vec![Ghost {
                entity: CONNECTION,
                position: POINT,
                mob: None,
                player: None,
            }],
            gde_refs: vec![(CONNECTION, GDE_REF)],
            round: 1,
            part: 0,
            parts: 1,
        });
        round_trip(SimHeartbeat {
            id: sim(),
            started_at_ms: STARTED_AT_MS,
            players: 10,
            max_players: 2000,
            level: "newworld_vitaeeterna".to_owned(),
            instance: None,
        });
        round_trip(SimHeartbeat {
            id: sim(),
            started_at_ms: STARTED_AT_MS,
            players: 1,
            max_players: 5,
            level: "nw_dungeon_windsward_00".to_owned(),
            instance: Some(crate::instance::InstanceBeat {
                running: Some(crate::instance::RunningInstance {
                    instance: Uuid::from_u128(1),
                    game_mode: "DungeonAmrine".to_owned(),
                    members: vec![Uuid::from_u128(2)],
                    players: 1,
                    empty_for_ms: Some(u64::MAX),
                    open_for_ms: 1,
                }),
            }),
        });
    }

    #[test]
    fn batches_round_trip() {
        round_trip(OutputBatch {
            sim: sim(),
            outputs: vec![GatewayOutput::Close {
                connection: CONNECTION,
            }],
        });
        round_trip(InputBatch {
            forwarded: true,
            inputs: vec![SimInput::Left {
                connection: connection(),
            }],
        });
    }

    fn output_of(bytes: usize, connection: u64) -> GatewayOutput {
        GatewayOutput::ServerMessages {
            connection,
            channel: 1,
            reliable: false,
            envelopes: vec![vec![0; bytes]],
        }
    }

    #[test]
    fn a_batch_splits_in_order() {
        let third = MAX_BATCH_BYTES / 3;
        let outputs: Vec<GatewayOutput> = (0..7).map(|n| output_of(third, n)).collect();
        let batches = split_batches(outputs.clone(), GatewayOutput::size_bytes);
        assert_eq!(batches.len(), 4, "two outputs per batch");
        assert!(batches.iter().all(|batch| {
            batch.iter().map(GatewayOutput::size_bytes).sum::<usize>() <= MAX_BATCH_BYTES
        }));
        assert_eq!(batches.concat(), outputs, "the order stays");
        let huge = vec![output_of(MAX_BATCH_BYTES * 2, 0), output_of(1, 1)];
        assert_eq!(
            split_batches(huge.clone(), GatewayOutput::size_bytes).len(),
            2
        );
        assert!(split_batches(Vec::<GatewayOutput>::new(), GatewayOutput::size_bytes).is_empty());
        let input = SimInput::ClientMessage {
            connection: connection(),
            envelope: vec![0; third],
        };
        assert!(input.size_bytes() >= third);
    }

    #[test]
    fn the_largest_ghost_fits_its_byte_limit() {
        let ghost = Ghost {
            entity: u64::MAX,
            position: Point {
                x: f32::MAX,
                y: f32::MIN,
                z: f32::NAN,
            },
            mob: Some(MobGhost {
                health: u32::MAX,
                max_health: u32::MAX,
                returning: true,
                vitals_id: u32::MAX,
                category_id: u32::MAX,
                level: u32::MAX,
            }),
            player: None,
        };
        assert!(encode(&ghost).expect("encode").len() <= MAX_GHOST_BYTES);
        round_trip(Ghost {
            position: POINT,
            ..ghost
        });
        let item = GhostItem {
            slot: u8::MAX,
            item_index: u32::MAX,
            look: ItemLookState {
                dye: [u8::MAX; DYE_CHANNELS],
                appearance: u32::MAX,
                skin_dye: [u8::MAX; DYE_CHANNELS],
            },
        };
        let largest_slot = GhostSlotChange {
            state: Some(u64::MAX),
            started: Some(u16::MAX),
            sequence: Some(u64::MAX),
        };
        let player = Ghost {
            mob: None,
            player: Some(PlayerGhost {
                character: Uuid::from_u128(u128::MAX),
                look: Some([u8::MAX; LOOK_CHOICES]),
                health: u32::MAX,
                max_health: u32::MAX,
                facing: Some(f32::MAX),
                pitch: Some(f32::MAX),
                target: Some(u64::MAX),
                state_number: Some((u16::MAX, u64::MAX)),
                blocking: true,
                spawned_ms: u64::MAX,
                gear_version: u64::MAX,
                drawn: Some(u32::MAX),
                active: Some(u32::MAX),
                attached: true,
                looking_through_loadout: true,
                mount: GhostMount {
                    mount_id: u32::MAX,
                    flags: u8::MAX,
                    dye: [u8::MAX; MOUNT_DYE_LEN],
                },
                items: [Some(item); GHOST_VISIBLE_ITEMS],
                action: Some(GhostSlayer {
                    state: Some(u64::MAX),
                    started: Some(u16::MAX),
                    sequence: u64::MAX,
                    second_started: Some(u16::MAX),
                    second_playing: Some((u64::MAX, u64::MAX)),
                    second_stopped: true,
                }),
                upper: Some(GhostUpperSlots {
                    second: largest_slot,
                    third: largest_slot,
                    fourth: largest_slot,
                    marker: Some(u64::MAX),
                    after_state: true,
                }),
            }),
            ..ghost
        };
        assert!(encode(&player).expect("encode").len() <= MAX_GHOST_BYTES);
        round_trip(Ghost {
            position: POINT,
            ..player
        });
    }

    #[test]
    fn hits_round_trip() {
        round_trip(Hit {
            target: MOB_ENTITY_BASE + 7,
            attacker: CONNECTION,
            attacker_sim: sim(),
            amount: 42,
            threat: 43,
            block_stamina_milli: None,
            ranged: false,
        });
        round_trip(Hit {
            target: CONNECTION,
            attacker: MOB_ENTITY_BASE + 7,
            attacker_sim: sim(),
            amount: 42,
            threat: 0,
            block_stamina_milli: Some(16_500),
            ranged: true,
        });
    }

    #[test]
    fn a_point_and_a_position_convert_both_ways() {
        let position = Position::from(POINT);
        assert_eq!(
            (position.x, position.y, position.z),
            (POINT.x, POINT.y, POINT.z)
        );
        assert_eq!(Point::from(position), POINT);
    }

    #[test]
    fn a_point_is_finite_only_with_three_numbers() {
        assert!(POINT.is_finite());
        assert!(
            !Point {
                z: f32::INFINITY,
                ..POINT
            }
            .is_finite()
        );
    }

    #[test]
    fn the_silence_limit_is_the_missed_intervals_and_saturates() {
        let second = Duration::from_secs(1);
        assert_eq!(heartbeat_silence(second, MISSES), second * MISSES);
        assert_eq!(heartbeat_silence(Duration::MAX, MISSES), Duration::MAX);
    }

    #[test]
    fn a_bad_gateway_id_or_other_bytes_do_not_decode() {
        let bytes = encode(&"a.b".to_owned()).expect("encode");
        assert!(decode::<BusId>(&bytes).is_err());
        assert!(decode::<Join>(&[0xFF; 3]).is_err());
    }
}
