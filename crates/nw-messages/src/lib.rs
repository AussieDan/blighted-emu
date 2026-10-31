// SPDX-License-Identifier: AGPL-3.0-or-later
//! Body codecs of the game messages of M4 (M4 spec section 5).
//!
//! A body is the part of an envelope after the type index. Layouts come only from the RE notes:
//! `docs/re/2026-09-04-bootstrap-message-bodies.md` for the messages and
//! `docs/re/2026-09-02-replication-and-movement.md` for the replicated states. A field without a
//! name in the notes keeps a neutral name. `decode` fails on missing bytes and on bytes left over.

pub mod abilities;
pub mod achievements;
pub mod afflictions;
pub mod alc;
pub mod appearance;
pub mod arena;
pub mod attributes;
pub mod buildables;
pub mod bundle;
pub mod categorical;
pub mod client_add_entry;
pub mod client_input;
pub mod client_options;
pub mod combat;
pub mod combat_effects;
pub mod components;
pub mod connected;
pub mod containers;
pub mod contracts;
pub mod cooldowns;
pub mod day_phase;
pub mod economy;
pub mod entity_ids;
pub mod entity_slots;
pub mod facet;
pub mod game_mode;
pub mod gathering;
pub mod gde;
pub mod gear;
pub mod generic_invite;
pub mod gm_client;
pub mod guild_lookup;
pub mod interactor;
pub mod item_calls;
pub mod item_descriptor;
pub mod item_repair;
pub mod items;
pub mod kick;
pub mod level_info;
pub mod list_delta;
pub mod lookups;
pub mod loot;
mod member_layouts;
pub mod meta_achievements;
pub mod mount;
pub mod objectives;
pub mod p2p_trade;
pub mod paperdoll;
pub mod payload;
pub mod player_components;
pub mod player_data;
pub mod points_client;
pub mod position;
pub mod progression;
pub mod progression_points;
pub mod seasons;
pub mod self_ident;
pub mod server;
pub mod shop;
mod slots;
pub mod social;
pub mod spawn_point;
pub mod spreadshot;
pub mod store;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod time_change;
pub mod time_synch;
pub mod title_choice;
pub mod titles;
pub mod travel;
pub mod travel_states;
pub mod vitals;
pub mod war;
pub mod war_signup;
pub mod wire;

pub use abilities::{
    ABILITY_COMPONENT, ABILITY_TYPE_INDEX, AbilityState, MAX_MAPPED_SLOTS, MappedSlot, MappedTable,
    MasteryTreeRespecResponseMsg, PersistentAbilities,
};
pub use achievements::{
    ACHIEVEMENT_COMPONENT, ACHIEVEMENT_TYPE_INDEX, AchievementBits, AchievementState, ByteChange,
};
pub use afflictions::{Affliction, AfflictionId, AfflictionList, ColdLong, HotLong, LongForm};
pub use alc::{
    ALC_TYPE_INDEX, AlcChild, AlcStamp, AlcState, CardPlace, DATA_BITS_ACTIVE, FRAG_TAGS_LEN,
    LayerEntry, LookDir, MobAnimation, MobStep, POS_REL_CHILD, POS_REL_STEP_M, POS3_LEN,
    POS10_CHILD, RemoteStep, RemoteUpper, SLOT0_CHILDREN, SLOT1_CHILDREN, SPAWN_STATE_ID,
    SPAWN_STATE_MS, SPAWN_TELEPORT, ScopeEntry, SecondSlayerState, SequenceClock, SequenceTime,
    SlayerState, SlotChange, StepPlace, StepScope, TeleportStep, UpperSlots, offset_from,
    offset_metres,
};
pub use appearance::{
    LOOK_CHOICES, PLAYER_APPEARANCE_TYPE_INDEX, PlayerAppearanceState, PlayerIconData, PlayerLook,
};
pub use arena::{
    ActivateResponse, ArenaClientMsg, ArenaServerMsg, NO_GAME_MODE_INDEX, PLAYER_ARENA_COMPONENT,
    PLAYER_ARENA_FACET, PLAYER_ARENA_TYPE_INDEX, PlayerArenaState,
};
pub use attributes::{
    ATTRIBUTE_COMPONENT, ATTRIBUTE_TYPE_INDEX, AttributeState, AttributeValue, PersistentAttributes,
};
pub use buildables::{
    BUILDABLE_CONTROLLER_TYPE_INDEX, BUILDABLE_GRID_TYPE_INDEX, BuildableControllerState,
    BuildableGridState, CommittedResource, GridSide, GuildCrest, OWNERSHIP_TYPE_INDEX,
    OwnerIdentification, OwnershipState, PLACEMENT_OBSTRUCTION_TYPE_INDEX,
    PlacementObstructionState, TERRITORY_INTERFACE_TYPE_INDEX, TerritoryInterfaceState,
};
pub use bundle::{
    IndexBlock, LOCAL_PLAYER_INTEREST, MAX_BUNDLE_INDEXES, OBSERVED_BUNDLE_BYTE_4,
    REPLICATED_STATE_BUNDLE_TYPE_INDEX, ReplicatedStateBundle,
};
pub use categorical::{
    CATEGORICAL_PROGRESSION_COMPONENT, CATEGORICAL_PROGRESSION_TYPE_INDEX,
    CategoricalProgressionState, MasteryChange, MasteryEntry, MasteryFields,
};
pub use client_add_entry::{CLIENT_ADD_ENTRY_MSG_TYPE_INDEX, ClientAddEntryMsg};
pub use client_input::{
    Bit8, ClientInputMsg, HeadStep9, INPUT_Z_MAX, INPUT_Z_MIN, InputHead, InputPosition,
    InputRecord, InputSlots, InteractionRequest, MoveKeys, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX,
    SlayerInput,
};
pub use client_options::{ClientOptionMsg, FULL_CULL_LIMIT_PERCENT};
pub use combat::{
    DAMAGE_DEALT_TYPE_INDEX, DEATH_RECAP_TYPE_INDEX, DamageDealtMsg, DamageItem, DeathRecapMsg,
    HIT_TAG, MAX_RESPAWN_PAYLOAD, MELEE_HIT_TYPE_INDEX, MeleeDamage, MeleeHitMsg, NEUTRAL_SCALE,
    ON_DAMAGE_TYPE_INDEX, OnDamageMsg, OnHealedMsg, REQUEST_RESPAWN_TYPE_INDEX, RequestRespawnMsg,
};
pub use combat_effects::{CombatEffectMsg, HitScanEffects};
pub use components::{
    BOSS_PHASE_TYPE_INDEX, COMBAT_STATUS_TYPE_INDEX, CURRENCY_COMPONENT, CURRENCY_TYPE_INDEX,
    CombatStatusState, CurrencyState, DAMAGE_RECEIVER_TYPE_INDEX, DELAYED_EVENT_TYPE_INDEX,
    DEPLETED_GATHERABLE, DamageReceiverState, DelayedEventState, GATHERABLE_CONTROLLER_TYPE_INDEX,
    GatherableControllerState, INTERACT_TYPE_INDEX, InteractState, LOOK_TARGETING_TYPE_INDEX,
    LookTargetingState, MANA_TYPE_INDEX, ManaState, STAMINA_TYPE_INDEX, StaminaState,
    boss_phase_change,
};
pub use connected::{
    Block1415acc30, CALENDAR_CONNECTED_MSG_TYPE_INDEX, CalendarConnectedMsg, ConnectedBody,
    TIMING_CONFIGURATION_CONNECTED_MSG_TYPE_INDEX, TimingConfigurationConnectedMsg,
};
pub use containers::{
    BAG_COMPONENT, CONTAINER_TYPE_INDEX, ContainerState, ITEM_MANAGEMENT_TYPE_INDEX,
    ItemManagementState, OwnedChange, OwnedItem, OwnedItems,
};
pub use cooldowns::{
    COOLDOWNS_COMPONENT, COOLDOWNS_TYPE_INDEX, ConsumableCooldown, CooldownsState,
};
pub use day_phase::{OBSERVED_DAY_PHASE, SYNC_DAY_PHASE_MSG_TYPE_INDEX, SyncDayPhaseMsg};
pub use economy::{EconomyCall, EconomyMessage};
#[cfg(any(test, feature = "test-util"))]
pub use entity_ids::number_of_facet;
pub use entity_ids::{
    Adopted, IdHolder, RawIdError, adopt_raw_id, as_dynamic_raw_id, entity_of_facet,
    facet_of_raw_id, hold_raw_id, is_dynamic_raw_id, number_of_seen_raw_id, raw_id_count,
    release_raw_id, renew_raw_id,
};
pub use entity_slots::{EntitySlots, SlotRead, SlotUpdate};
pub use facet::{
    AdjustFxScriptsMsg, ClientFacetMsg, CraftIngredient, CraftProgressMsg, CutsceneEndFinalizedMsg,
    CutsceneGrantedMsg, DisableCollisionMsg, FacetHeader, GameModeSchedulerRefreshedMsg,
    GatheringInterruptedMsg, HomingOverrideMsg, InteractionEndedMsg, InteractionValidatedMsg,
    LeftGameModeQueueMsg, PlayerSeenByAiMsg, SelectedAsTargetMsg, StartWhisperMsg,
    StorageSearchItemsMsg, TargetMsg, TeleportWithLoadingScreenMsg, UnselectedAsTargetMsg,
    UpdatePhasingMsg, facet_entity,
};
pub use game_mode::{GameModeClientMsg, GameModeServerMsg, QueueActivity};
pub use gathering::{GATHERING_TYPE_INDEX, GatheringState, NO_GATHERABLE};
pub use gde::{
    AssetId, DYNAMIC_SLICE_SUB_ID, GDE_ID_LEN, GDE_METADATA_COMPONENT, GDE_METADATA_TYPE_INDEX,
    GdeMetadataReplicatedState, PLAYER_SLICE_ASSET,
};
pub use gear::{
    EquippedItem, GearExtras, GearItem, Hands, ItemLook, OpenContainer, PlayerGear,
    STARTER_BAG_SLOTS, ShownItem, is_visible_slot,
};
pub use gm_client::GmClientMsg;
pub use guild_lookup::{GuildsRequest, OtherGuildData, SiegeWindow, UnorderedGuildsMsg};
pub use interactor::{INTERACTOR_TYPE_INDEX, InteractorState};
pub use item_calls::{ContainerRef, DropEntry, ItemCallMsg, ItemRef, LoadoutStatusMsg};
pub use item_repair::ItemRepairMsg;
pub use items::{FullList, ItemRecord};
pub use kick::{KickClientMsg, PlayerManagerRejectedMsg, RETAIL_KICK_REASON};
pub use level_info::{
    LEVEL_INFO_CHANGED_MSG_TYPE_INDEX, LEVELS, LOGIN_CLIENT_CONTEXT, LevelInfoChangedMsg,
    LevelSpec, OPEN_WORLD_LEVEL, PhasingEntry, TUTORIAL_LEVEL,
};
pub use list_delta::{ListChange, ReplicatedList, list_change};
pub use lookups::{
    JoinableSubArmiesMsg, LobbyCountsMsg, LookupRequest, PendingMetaAchievementsMsg,
};
pub use loot::{
    GLOBAL_STORAGE_COMPONENT, GLOBAL_STORAGE_TYPE_INDEX, GlobalStorageState, LOOT_CREATURE,
    LOOT_DROPPED, LOOT_QUEST_OBJECT, LOOT_ROLLED, LOOT_TAKEN, LOOT_TRACKER_COMPONENT,
    LOOT_TRACKER_TYPE_INDEX, LootTrackerState, StoredItems, TrackedLoot,
};
pub use meta_achievements::{
    MetaAchievementProgressMsg, MetaAchievementRequest, MetaAchievementSummaryMsg, MetaProgress,
    MetaProgressChangedMsg,
};
pub use mount::{
    DismountReason, MOUNT_COMPONENT, MOUNT_FACET, MOUNT_TYPE_INDEX, MountClientMsg,
    MountCustomization, MountServerMsg, MountState, PLAYER_QUICK_COURSE_FACET, RemoteFlags,
    SummonAuthorization, SummonCancelReason,
};
pub use objectives::{
    OBJECTIVES_COMPONENT, OBJECTIVES_TYPE_INDEX, ObjectiveRecord, ObjectivesState, TaskEntry,
    TaskWireState,
};
pub use paperdoll::{
    ItemVisual, LoadedAmmo, Loadout, PAPERDOLL_TYPE_INDEX, PaperdollOwner, PaperdollPublic,
    PaperdollState, SlotBits,
};
pub use payload::{BundleGroup, BundleMember, BundlePayload, KnownMembers, MemberState};
pub use player_components::{
    EmptyState, HealthChange, PLAYER_COMPONENT_COMPONENT, PLAYER_COMPONENT_TYPE_INDEX,
    PLAYER_EMPTY_COMPONENTS, PLAYER_VITALS_COMPONENT, PlayerComponentState, PlayerStates,
    change_members, empty_state, gear_members, player_members, put_account_in_probation,
    put_is_in_store, put_looking_through_loadout, put_member,
};
pub use player_data::{
    NO_GUILD, NO_PLATFORM, NO_PLATFORM_PRESENCE, PlayerDataRequest, PlayerDataResponseMsg,
    PlayerDataView,
};
pub use points_client::PointsClientMsg;
pub use position::{
    POSITION_IN_THE_WORLD_TYPE_INDEX, POSITION_Z_MAX, POSITION_Z_MIN,
    PositionInTheWorldReplicatedState, Rotation, RotationComponent, Scale, WorldPosition,
};
pub use progression::{PROGRESSION_COMPONENT, PROGRESSION_TYPE_INDEX, ProgressionState};
pub use progression_points::{
    PROGRESSION_POINT_COMPONENT, PROGRESSION_POINT_TYPE_INDEX, PointEntry, PointPool, PoolCategory,
    ProgressionPointState,
};
pub use self_ident::{
    PLAYER_MANAGER_SELF_IDENTIFICATION_MSG_TYPE_INDEX, PlayerManagerSelfIdentificationMsg,
};
pub use server::{LOGIN_MESSAGE_TYPES, SERVER_MESSAGE_TYPES, ServerMessage};
pub use spawn_point::{RECEIVE_PLAYER_SPAWN_POINT_MSG_TYPE_INDEX, ReceivePlayerSpawnPointMsg};
pub use spreadshot::{
    SPREADSHOT_PROCESSED_TYPE_INDEX, SPREADSHOT_REQUEST_TYPE_INDEX, SpreadHit,
    SpreadshotProcessedMsg, SpreadshotRequestMsg,
};
pub use time_change::{
    OBSERVED_TIME_CHANGE_F32S_AT_16, OBSERVED_TIME_CHANGE_F32S_AT_60,
    OBSERVED_TIME_CHANGE_U32S_AT_28, SYNC_TIME_CHANGE_MSG_TYPE_INDEX, SyncTimeChangeMsg,
};
pub use time_synch::{NS_PER_MS, TimeSynchMsg};
pub use title_choice::{SOCIAL_COMPONENT, SetTitleRequest, title_state};
pub use titles::{AvailableTitlesMsg, TitleRequest};
pub use travel::{
    ItemAnchor, ItemPlacement, LandmarkRef, PlotRef, StructurePlacement, TerritoryResidentData,
    TravelClientMsg, TravelServerMsg, UnstuckDenial, WAYPOINTS_COMPONENT, WAYPOINTS_TYPE_INDEX,
    WaypointsState,
};
pub use travel_states::{
    CAMPING_COMPONENT, CAMPING_TYPE_INDEX, CampRef, CampingState, HomePoint, HouseDataRef,
    NO_PHASED_ENTITY, OwnedHouse, PLAYER_HOME_COMPONENT, PLAYER_HOME_TYPE_INDEX,
    PLAYER_HOUSING_COMPONENT, PLAYER_HOUSING_TYPE_INDEX, PLAYER_TIME_COMPONENT,
    PLAYER_TIME_TYPE_INDEX, PlayerHomeState, PlayerHousingState, PlayerTimeState, home_point_id,
    home_point_kind,
};
pub use vitals::{
    FIRST_DAMAGE_FLAGS, PLAYER_VITALS_ID, VITALS_TYPE_INDEX, VitalsData, VitalsSlot0, VitalsSlot1,
    VitalsState,
};
pub use war::{WarClientMsg, WarServerMsg, WarboardManifest};
pub use war_signup::{SIGNUP_LIST_TRAILER, SignupEntry, WarSignupClientMsg, WarSignupServerMsg};
pub use wire::{
    BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer, entity_of_raw_id, raw_id_of_entity,
};
pub mod trigger_entity;
pub use trigger_entity::{
    SYNC_EXECUTE_ENTITIES_TYPE_INDEX, SyncExecuteEntitiesMsg, TriggerAddress,
};
pub mod conversation_client;
pub use conversation_client::{ConversationClientMsg, ObjectiveInteractorClientMsg};
pub mod notifications;
pub use notifications::{
    EVENT_NOTIFICATION_COMPONENT, GAME_EVENTS_CHILD, NotificationCount, NotificationSentMsg,
    Replacement, SendNotificationMsg, SendNotificationWithReplacementsMsg, SyncNotificationsMsg,
};
pub mod objective_interactor;
pub use objective_interactor::{
    BOARD_EXPIRATION_NS, CommunityGoal, MissionParams, OBJECTIVE_INTERACTOR_COMPONENT,
    OBJECTIVE_INTERACTOR_TYPE_INDEX, ObjectiveInteractorState, PROVIDER_ID_LEN,
};
pub mod game_events;
pub use game_events::{
    DailyBonus, GAME_EVENT_COMPONENT, GAME_EVENT_TYPE_INDEX, GameEventEntry, GameEventState,
    MAX_DAILY_BONUSES, MAX_GAME_EVENTS, RewardItem,
};
pub mod temporary_affiliation;
pub use temporary_affiliation::{
    Affiliation, TEMPORARY_AFFILIATION_COMPONENT, TEMPORARY_AFFILIATION_TYPE_INDEX,
    TemporaryAffiliationState,
};
pub mod fishing;
pub use fishing::{
    FISHING_COMPONENT, FISHING_FACET, FISHING_TYPE_INDEX, FishCatch, FishingClientMsg,
    FishingComponentState, FishingServerMsg, FishingState, RollModifier, StateTransition,
};
pub mod music;
pub use music::{
    MUSIC_CHILD, MUSIC_FACET, MusicClientMsg, MusicServerMsg, MusicalPerformanceState,
    NO_PERFORMANCE, PERFORMANCE_COMPONENT, PERFORMANCE_PLAYER_COMPONENT,
    PERFORMANCE_PLAYER_TYPE_INDEX, PERFORMANCE_TYPE_INDEX, PerformanceError,
    PerformancePlayerState, PerformanceStage,
};
pub mod mutation_scheduler;
pub use mutation_scheduler::{
    MUTATION_SCHEDULER_COMPONENT, MUTATION_SCHEDULER_INTEREST, MUTATION_SCHEDULER_TYPE_INDEX,
    MutationSchedulerState, MutationSetEntry,
};
pub mod storage_search;
pub use facet::SearchedItem;
pub use storage_search::{
    RECEIVE_STORAGE_SEARCH_RESULTS_TYPE_INDEX, STORAGE_ITEM_SIMPLE_TYPE_INDEX, StorageItemSimple,
    StorageSearchReadyMsg, StorageSearchResultsMsg, global_storage_header,
};
pub use titles::TitlesUnlockMsg;
pub mod transmog;
pub use transmog::{
    DyeEntry, TRANSMOG, TRANSMOG_CHILD, TransmogClientMsg, TransmogServerMsg, transmog_header,
};
pub mod inventories;
pub use inventories::{
    INVENTORIES, InventoriesMsg, InventoryKey, ItemData, ServiceInventory, ServiceItem,
    ServiceRequest,
};
pub mod status_effects;
pub use status_effects::{
    EffectMap, LightweightValue, LocalEffect, MapChange, RemoteEffect, STATUS_EFFECTS_COMPONENT,
    STATUS_EFFECTS_TYPE_INDEX, StatusEffectsState,
};
