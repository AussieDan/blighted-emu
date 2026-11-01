// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bus between `rep-gateway`, `sim` and `world-director` (M4 spec section 6, M5 spec
//! section 5, parent spec section 7.3): every
//! NATS subject and every bus message type is defined here, so both sides agree. Core NATS only,
//! with request/reply for `join`. Bus messages are `postcard` bytes.

pub mod admin;
pub mod client;
pub mod ghost_parts;
pub mod id;
pub mod instance;
pub mod message;
pub mod publisher;
pub mod snapshot;
pub mod social;
pub mod subject;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;

pub use admin::{AdminAction, AdminCommand, AdminReply, RunningEvent, ScriptLog, ScriptLogLine};
pub use client::{Bus, BusError, Incoming, Subscription};
pub use ghost_parts::{GhostAssembly, ghost_parts};
pub use id::{BusId, BusIdError, MAX_BUS_ID_LEN};
pub use instance::{
    GroupAnswer, GroupQuery, InstanceBeat, InstanceCommand, InstanceOpened, InstanceRefusal,
    InstanceReply, InstanceRequest, InstanceStay, PlayerArena, RunningInstance,
};
pub use message::{
    Admit, AdmitReply, CarriedItem, CarriedMount, CellTable, ConnectionRef, DYE_CHANNELS,
    EffectRepeat, GHOST_VISIBLE_ITEMS, GatewayOutput, Ghost, GhostItem, GhostMount, GhostSlayer,
    GhostSlotChange, GhostUpperSlots, Ghosts, HandoffMessage, HandoffOffer, HandoffReply,
    Heartbeat, Hit, HomeRecord, HousingItem, InputBatch, ItemLookState, ItemPlace, ItemState, Join,
    JoinReply, LootLimitCount, MAX_BATCH_BYTES, MAX_BUS_MESSAGE_BYTES, MAX_GHOST_BYTES,
    MISSION_BOARD_ID_LEN, MOB_ENTITY_BASE, MOUNT_DYE_LEN, MissionTake, MobGhost, OutputBatch,
    OwnedHome, PlacedCamp, PlayerFaction, PlayerGhost, PlayerInventory, PlayerMounts, PlayerMusic,
    PlayerProgression, PlayerQuests, PlayerStatusEffect, PlayerTravel, Point, QuestRun, QuestTask,
    RecipeCooldownCount, ResumeOf, SavedLoadout, SimHeartbeat, SimInput, SimSlot, StoredItem,
    StoredPoint, decode, encode, heartbeat_silence, split_batches,
};
pub use publisher::Publisher;
pub use snapshot::{
    MOB_STATES, SNAPSHOT_PART_ENTITIES, SimSnapshot, SnapMob, SnapPlayer, snapshot_parts,
};
pub use social::ICON_LEN as SOCIAL_ICON_LEN;
pub use social::{
    CoinReply, CoinTransfer, GroupEntityDelivery, GroupEntityView, LeaveReason, OwnStateDelivery,
    PlayerVitals, SocialBatch, SocialDeliveries, SocialDelivery, SocialFacetKind, SocialHeader,
    SocialInput, SocialPlayer, SocialStateKind,
};
