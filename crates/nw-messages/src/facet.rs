// SPDX-License-Identifier: AGPL-3.0-or-later
//! Facet calls: messages to or from one component of the player's entity (tutorial-intro note,
//! sections 0 and 2).
//!
//! Every facet body starts with two big-endian `u64`s: the target id, then the entity. The entity
//! is the first 8 bytes of the `ClientAddEntryMsg` entry id in reverse byte order. The target id
//! is the entity plus the `Id` of the component in the player prefab; for a component on a child
//! entity of the prefab, the entity is first XORed with (main entity `Id` XOR child entity `Id`)
//! (vitals-facet note). The client looks the target id up and drops a call whose target it does
//! not know.

use crate::entity_ids::{entity_of_facet, facet_of_raw_id};
use crate::level_info::PhasingEntry;
use crate::wire::{BodyCodec, MessageError, RAW_ID_LEN, Reader, Writer, raw_id_of_entity};

/// The target-id constant of each component of the player entity (Soul Survivor session).
pub mod component {
    /// `TimeComponent`.
    pub const TIME: u64 = 0xd41e_b32c_0f15_4004;
    /// `PlayerComponent`.
    pub const PLAYER: u64 = 0xdf0c_e8c9_8d27_619d;
    /// `PlayerCutsceneComponent`.
    pub const PLAYER_CUTSCENE: u64 = 0xd499_3151_0e1d_d23f;
    /// `GridTrackingComponent`.
    pub const GRID_TRACKING: u64 = 0x3946_bc0c_0a40_83da;
    /// `DamageReceiverComponent` (mob-cards note, section 4.1).
    pub const DAMAGE_RECEIVER: u64 = 0xccc4_7f92_daf6_bfc1;
    /// `StatusEffectsComponent`, on the child entity "Stats" ([`STATS_CHILD`]): 62,354 retail
    /// calls of 606 players (facets note). On the tutorial capture's player it equals the main
    /// entity plus `0x198b_1950_2047_20ba`, which live 2026-09-16 the client could not find.
    pub const STATUS_EFFECTS: u64 = 0x5630_dc38_1f27_432b;
    /// `VitalsComponent`, on the child entity "Stats" ([`STATS_CHILD`]).
    pub const VITALS: u64 = 0xeeef_fb82_abaf_e4b1;
    /// The XOR of the player prefab's main entity `RootPlayer` and its child `Stats`.
    pub const STATS_CHILD: u64 = 0x3caa_c368_06e0_2db3;
    /// `InteractorComponent` (live-tutorial note, section 7).
    pub const INTERACTOR: u64 = 0x6c57_18db_fe6d_f612;
    /// `GatherableControllerComponent` of the tutorial's ruin chest (live-tutorial note,
    /// section 7).
    pub const CHEST_GATHERABLE_CONTROLLER: u64 = 0x45d5_7d32_25aa_2566;
    /// `GatherableControllerComponent` of Grace's corruption nodules
    /// (`IO_Custom_MSQ_CorruptedNodule_A` to `_D`): the 2724 target less the nodule's id on all
    /// four nodules of the retail newchar run of 2026-09-18.
    pub const NODULE_GATHERABLE_CONTROLLER: u64 = 0xa84c_1def_7520_cf7d;
    /// `AITargetableComponent` of the player.
    pub const AI_TARGETABLE: u64 = 0x5207_d209_ba7d_7dd8;
    /// `TutorialAIComponent` of a tutorial sailor, on its child [`TUTORIAL_AI_CHILD`].
    pub const TUTORIAL_AI: u64 = 0xd128_17b1_c795_98a5;
    /// The XOR of a tutorial sailor's main entity and the child that holds its tutorial AI.
    pub const TUTORIAL_AI_CHILD: u64 = 0x37 << 32;
    /// `CraftingComponent` (beach-quests note: the 2993 and 2997 target less the Soul entity).
    pub const CRAFTING: u64 = 0xf7e4_6d7c_2327_3ef2;
    /// `WhisperPlayerComponent` (retail 2026-09-18: the 5070 target less the player entity).
    pub const WHISPER_PLAYER: u64 = 0x5442_5e1a_b303_2637;
    /// `ActionListComponent` (retail newchar 2026-09-18: the 998 target less the player entity).
    pub const ACTION_LIST: u64 = 0x3c59_1a4e_a3c2_ad75;
    /// `HomingComponent` (retail newchar 2026-09-18: the 1790 target less the player entity).
    pub const HOMING: u64 = 0xad9a_1342_9d1d_3c9c;
    /// `GameModeParticipantComponent` (retail 2026-09-18: the 6426 target less the player
    /// entity).
    pub const GAME_MODE_PARTICIPANT: u64 = 0x8de6_3359_0b8f_bee0;
    /// `GlobalStorageComponent`, on the child entity [`GLOBAL_STORAGE_CHILD`]. Solved from the
    /// 7048 headers of 17 retail characters (newchar-retail-2026-09-18, newchar-linux-2026-09-12,
    /// 15 open-world and side-quest ledgers): (entity XOR child) + component gave every target;
    /// no constant added to the entity alone did (live 2026-09-14: the client dropped such a
    /// target).
    pub const GLOBAL_STORAGE: u64 = 0xfcf8_f502_9cc7_d818;
    /// The XOR of the player prefab's main entity and the child that holds its global storage.
    pub const GLOBAL_STORAGE_CHILD: u64 = 0x5aee_2731_f432_3d04;
}

/// Type index of `PlayerComponentServerFacet_OnAckLevelInfoChanged`.
pub const ACK_LEVEL_INFO_TYPE_INDEX: u64 = 5181;
/// Type index of `PlayerCutsceneComponentServerFacet_NotifyCinematicStarted`.
pub const CINEMATIC_STARTED_TYPE_INDEX: u64 = 6606;
/// Type index of `PlayerCutsceneComponentServerFacet_NotifyCinematicEnded`.
pub const CINEMATIC_ENDED_TYPE_INDEX: u64 = 6542;
/// Type index of `PlayerCutsceneComponentServerFacet_OnClientRequestStartCutscene`.
pub const REQUEST_CUTSCENE_TYPE_INDEX: u64 = 6703;
/// Type index of `PlayerCutsceneComponentServerFacet_OnClientCutsceneEnded`.
pub const CUTSCENE_ENDED_TYPE_INDEX: u64 = 6705;
/// Type index of `PlayerCutsceneComponentServerFacet_OnClientCutsceneStarted`.
pub const CUTSCENE_STARTED_TYPE_INDEX: u64 = 6706;
/// Type index of `ObjectivesComponentServerFacet_UpdateTrackedObjectives`.
pub const UPDATE_TRACKED_OBJECTIVES_TYPE_INDEX: u64 = 1458;
/// Type index of `ObjectivesComponentServerFacet_SetObjectiveAutoPinning`.
pub const SET_OBJECTIVE_AUTO_PIN_TYPE_INDEX: u64 = 2709;
/// Type index of `ActionListComponentServerFacet_SetInputsThrottled`.
pub const SET_INPUTS_THROTTLED_TYPE_INDEX: u64 = 998;
/// Type index of `WhisperPlayerComponentServerFacet_RequestStartWhisper`.
pub const WHISPER_STARTED_TYPE_INDEX: u64 = 5235;
/// Type index of `HomingComponentClientFacet_SetHomingOverrideTarget`.
pub const HOMING_OVERRIDE_TYPE_INDEX: u64 = 1790;
/// Type index of `GameModeParticipantComponentClientFacet_OnLeftQueueForGameModeMsg`.
pub const LEFT_GAME_MODE_QUEUE_TYPE_INDEX: u64 = 4970;
/// The 1790 word retail sends when a mob picks the player and no homing target is set.
pub const NO_HOMING_OVERRIDE: u32 = u32::MAX;
/// The 4970 value retail sends as the player enters the tutorial (one left the queue).
pub const LEFT_QUEUE_ONCE: u64 = 1;
/// Type index of `PaperdollComponentServerFacet_ServerUnequipItem`.
pub const UNEQUIP_ITEM_TYPE_INDEX: u64 = 3896;
/// Type index of `InteractorComponentServerFacet_RequestCancelCommittedInteractionFromClient`.
pub const CANCEL_INTERACTION_TYPE_INDEX: u64 = 2473;
/// Type index of `PaperdollComponentServerFacet_ServerSwapItems`.
pub const SWAP_ITEMS_TYPE_INDEX: u64 = 3900;
/// Type index of `TimeComponentServerFacet_SyncToClient`: the client asks for the time of day.
pub const TIME_SYNC_REQUEST_TYPE_INDEX: u64 = 4248;
/// Type index of `PlayerArenaComponentServerFacet_NotifyPlayerOfFtueQTECompletion`.
pub const QTE_COMPLETED_TYPE_INDEX: u64 = 6638;
/// Type index of `PlayerComponentServerFacet_TriggerAreaEntityPassthrough`.
pub const TRIGGER_AREA_PASSTHROUGH_TYPE_INDEX: u64 = 560;
/// Type index of `ObjectivesComponentServerFacet_NotifyOpenUiScreen`.
pub const UI_SCREEN_OPENED_TYPE_INDEX: u64 = 2617;
/// Type index of `GlobalStorageComponentServerFacet_SetReplicateStorage` (Azoth catalog 387).
pub const SET_REPLICATE_STORAGE_TYPE_INDEX: u64 = 387;
/// Type index of `PlayerTradeComponentServerFacet_TakeAllItems`.
pub const TAKE_ALL_ITEMS_TYPE_INDEX: u64 = 2939;
/// Type index of `PaperdollComponentServerFacet_ServerEquipItem`.
pub const EQUIP_ITEM_TYPE_INDEX: u64 = 3895;
/// Type index of `PlayerComponentServerFacet_ReturnToMainWorld`.
pub const RETURN_TO_MAIN_WORLD_TYPE_INDEX: u64 = 502;
/// Type index of `GridTrackingComponentClientFacet_UpdatePhasing`.
pub const UPDATE_PHASING_TYPE_INDEX: u64 = 6310;
/// Type index of `PlayerCutsceneComponentClientFacet_OnServerCutsceneRequestGranted`.
pub const CUTSCENE_GRANTED_TYPE_INDEX: u64 = 6707;
/// Type index of `PlayerCutsceneComponentClientFacet_OnServerCutsceneEndRequestFinalized`.
pub const CUTSCENE_END_FINALIZED_TYPE_INDEX: u64 = 6709;
/// Type index of `InteractorComponentClientFacet_SyncInteractionValidationResult`.
pub const INTERACTION_VALIDATED_TYPE_INDEX: u64 = 3743;
/// Type index of `InteractorComponentClientFacet_CancelCurrentInteraction`.
pub const INTERACTION_ENDED_TYPE_INDEX: u64 = 3746;
/// Type index of `GatherableControllerComponentClientFacet_GatheringInterrupted`.
pub const GATHERING_INTERRUPTED_TYPE_INDEX: u64 = 2724;
/// Type index of `TutorialAIComponentClientFacet_OnPlayerSeenByAI`.
pub const PLAYER_SEEN_BY_AI_TYPE_INDEX: u64 = 955;
/// Type index of `AITargetableComponentClientFacet_OnSelectedAsTarget`.
pub const SELECTED_AS_TARGET_TYPE_INDEX: u64 = 2415;
/// Type index of `AITargetableComponentClientFacet_OnUnSelectedAsTarget`.
pub const UNSELECTED_AS_TARGET_TYPE_INDEX: u64 = 3916;
/// Type index of `StatusEffectsComponentClientFacet_AdjustFXScripts`.
pub const ADJUST_FX_SCRIPTS_TYPE_INDEX: u64 = 608;
/// Type index of `CharacterComponentClientFacet_RequestDisableCollision`.
pub const DISABLE_COLLISION_TYPE_INDEX: u64 = 1793;
/// Type index of `PlayerComponentClientFacet_OnTeleportWithLoadingScreen`.
pub const TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX: u64 = 3951;
/// Type index of `ConversationComponentServerFacet_RequestAcceptObjective`.
pub const ACCEPT_OBJECTIVE_TYPE_INDEX: u64 = 3772;
/// Type index of `ConversationComponentServerFacet_RequestCompleteObjective`.
pub const COMPLETE_OBJECTIVE_TYPE_INDEX: u64 = 794;
/// Type index of `CraftingComponentServerFacet_SetCraftingDetails`.
pub const SET_CRAFTING_DETAILS_TYPE_INDEX: u64 = 2997;
/// Type index of `CraftingComponentServerFacet_CancelCrafting`.
pub const CANCEL_CRAFTING_TYPE_INDEX: u64 = 2999;
/// Type index of `CraftingComponentClientFacet_OnCraftingProgressUpdate`.
pub const CRAFTING_PROGRESS_TYPE_INDEX: u64 = 2993;
/// Type index of `CraftingComponentServerFacet_InitiateSalvage`.
pub const INITIATE_SALVAGE_TYPE_INDEX: u64 = 1733;
/// Type index of `CraftingComponentServerFacet_SalvageItemsInInventory`.
pub const SALVAGE_ITEMS_TYPE_INDEX: u64 = 6188;
/// Type index of `CraftingComponentServerFacet_InitiateSalvageByClass`.
pub const SALVAGE_BY_CLASS_TYPE_INDEX: u64 = 6024;
/// Type index of `TerritoryDetectorComponentServerFacet_AttemptTerritoryUnlock`.
pub const TERRITORY_ENTERED_TYPE_INDEX: u64 = 2297;
/// Type index of `TerritoryDetectorComponentServerFacet_AttemptTerritoryExit`.
pub const TERRITORY_EXITED_TYPE_INDEX: u64 = 6416;
/// Type index of `ObjectivesComponentServerFacet_EnteredTerritoryFallback`.
pub const ENTERED_TERRITORY_FALLBACK_TYPE_INDEX: u64 = 6733;
/// Type index of `ObjectivesComponentServerFacet_ExitedTerritoryFallback`.
pub const EXITED_TERRITORY_FALLBACK_TYPE_INDEX: u64 = 6734;
/// Type index of `FactionComponentServerFacet_RequestTogglePvpFlag`.
pub const TOGGLE_PVP_FLAG_TYPE_INDEX: u64 = 2525;
/// Type index of `FactionComponentServerFacet_RequestSetFaction`.
pub const SET_FACTION_TYPE_INDEX: u64 = 3161;
/// Type index of `WhisperPlayerComponentClientFacet_RequestStartWhisper`.
pub const START_WHISPER_TYPE_INDEX: u64 = 5070;
/// Type index of `GameModeParticipantComponentClientFacet_OnGameModeSchedulerRefreshed`.
pub const GAME_MODE_SCHEDULER_REFRESHED_TYPE_INDEX: u64 = 6426;
/// Type index of `GlobalStorageComponentClientFacet_ReceiveStorageSearchItems`.
pub const RECEIVE_STORAGE_SEARCH_ITEMS_TYPE_INDEX: u64 = 7048;
/// Type index of `PlayerComponentServerFacet_RequestSetIsLookingThroughLoadout`.
pub const LOOKING_THROUGH_LOADOUT_TYPE_INDEX: u64 = 3970;
/// The ingredient kind of an item named by its id (beach-quests note: every captured ingredient).
pub const INGREDIENT_ITEM: u8 = 1;

/// The facet entity of entity number `entity_number`: the first 8 bytes of its raw id
/// ([`raw_id_of_entity`]) as a little-endian `u64`, the client's entity id.
pub fn facet_entity(entity_number: u64) -> u64 {
    facet_of_raw_id(&raw_id_of_entity(entity_number))
}

/// The two ids that start a facet body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetHeader {
    /// The target id: the entity plus the component constant.
    pub target: u64,
    /// The entity.
    pub entity: u64,
}

impl FacetHeader {
    /// The header of a call to `component` of the player whose entry id names entity number
    /// `entity_number` (`ClientAddEntryMsg::for_entity`).
    pub fn of(component: u64, entity_number: u64) -> Self {
        let entity = facet_entity(entity_number);
        Self {
            target: entity.wrapping_add(component),
            entity,
        }
    }

    /// The header of a call to `component` on the child entity `child` (the XOR of its `Id` with
    /// the main entity's) of the player of entity number `entity_number`.
    pub fn of_child(component: u64, child: u64, entity_number: u64) -> Self {
        let entity = facet_entity(entity_number);
        Self {
            target: (entity ^ child).wrapping_add(component),
            entity,
        }
    }

    /// The entity number of the header's entity (the inverse of [`FacetHeader::of`]); `None`
    /// for an entity whose raw id this process neither made nor adopted.
    pub fn entity_number(&self) -> Option<u64> {
        entity_of_facet(self.entity)
    }

    /// Reads the header.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when the body is shorter than the header.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            target: reader.u64()?,
            entity: reader.u64()?,
        })
    }

    /// Writes the header.
    pub fn write(&self, writer: &mut Writer) {
        writer.u64(self.target).u64(self.entity);
    }
}

/// One ingredient of a craft request (2997).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraftIngredient {
    /// Its kind ([`INGREDIENT_ITEM`] for an item).
    pub kind: u8,
    /// How many.
    pub quantity: u32,
    /// CRC-32 of the lowercase item id.
    pub item: u32,
}

/// A facet call from the client that the server answers or acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFacetMsg {
    /// 5181: the client loaded the level of a `LevelInfoChangedMsg` with this client context.
    AckLevelInfo {
        /// The header.
        header: FacetHeader,
        /// The acknowledged client context.
        client_context: u8,
    },
    /// 6606: a cinematic video started; its id is the CRC-32 of its lowercase entry id.
    CinematicStarted {
        /// The header.
        header: FacetHeader,
        /// The cinematic.
        cinematic: u32,
    },
    /// 6542: a cinematic video ended; its id is the CRC-32 of its lowercase entry id.
    CinematicEnded {
        /// The header.
        header: FacetHeader,
        /// The cinematic.
        cinematic: u32,
    },
    /// 6703: the client asks to start a cutscene; its id is the CRC-32 of its lowercase name.
    RequestCutscene {
        /// The header.
        header: FacetHeader,
        /// The cutscene.
        cutscene: u32,
    },
    /// 6705: the client's cutscene ended.
    CutsceneEnded {
        /// The header.
        header: FacetHeader,
    },
    /// 6706: the client started the cutscene it was granted. The id is the CRC-32 of its
    /// lowercase name (retail newchar 2026-09-18: `a81bd1af` after the crew cutscene grant).
    CutsceneStarted {
        /// The header.
        header: FacetHeader,
        /// The cutscene.
        cutscene: u32,
    },
    /// 1458: the quests the client has pinned. A count of 0 clears the pin (retail newchar
    /// 2026-09-18: `00`, then `01` and one objective instance).
    UpdateTrackedObjectives {
        /// The header.
        header: FacetHeader,
        /// The pinned objective instances, in the client's order.
        objectives: Vec<u64>,
    },
    /// 2709: which objective kinds the client pins on its own. Retail newchar sends a count
    /// then that many flags (`24` and 36 bytes, one of them 0).
    SetObjectiveAutoPin {
        /// The header.
        header: FacetHeader,
        /// One flag per kind. A set flag pins that kind.
        flags: Vec<u8>,
    },
    /// 998: the client applied the input throttle (`01`) or left it (`00`). Retail sends it
    /// each time the server's movement-flag bit changes.
    SetInputsThrottled {
        /// The header.
        header: FacetHeader,
        /// Whether the client is throttling its input.
        throttled: bool,
    },
    /// 5235: the client started the whisper the server asked for (5070). The id is the
    /// CRC-32 of the lowercase whisper id.
    WhisperStarted {
        /// The header.
        header: FacetHeader,
        /// The whisper.
        whisper: u32,
    },
    /// 4248: the client's time component asks for the time of day (facets note: retail answers
    /// with 4246 and 4247 at entry and after the exit, and sends them at no other time).
    TimeSyncRequest {
        /// The header: the player's `TimeComponent`.
        header: FacetHeader,
    },
    /// 6638: the client finished a tutorial quick-time sequence; its id is the CRC-32 of its
    /// lowercase name.
    QteCompleted {
        /// The header.
        header: FacetHeader,
        /// The sequence.
        sequence: u32,
    },
    /// 560: the player passed through the client-side trigger area of a static level entity
    /// (ftue-static-entities note, section 3).
    TriggerAreaPassthrough {
        /// The header.
        header: FacetHeader,
        /// The entity id of the trigger area.
        entity: [u8; RAW_ID_LEN],
        /// Its facet entity.
        entity_facet: u64,
        /// An id of unknown meaning.
        extra: u64,
    },
    /// 2617: the client opened a UI screen; its id is the CRC-32 of its lowercase name (`hud`,
    /// `inventory`, ...; live-tutorial note, section 2).
    UiScreenOpened {
        /// The header.
        header: FacetHeader,
        /// The screen.
        screen: u32,
    },
    /// 2939: the Take All button of a loot window: the client asks for every item of the
    /// container (live-tutorial note, section 5.2).
    TakeAllItems {
        /// The header.
        header: FacetHeader,
        /// The container's entity id.
        container: [u8; RAW_ID_LEN],
        /// The container's facet entity.
        container_entity: u64,
        /// An id of unknown meaning.
        extra: u64,
    },
    /// 3895: the client asks to equip the item in bag slot `bag_slot` into paperdoll slot
    /// `equip_slot` (live-tutorial note, section 5.3).
    EquipItem {
        /// The header.
        header: FacetHeader,
        /// The bag slot of the item.
        bag_slot: u32,
        /// The paperdoll slot.
        equip_slot: u32,
        /// How many to equip (a stack).
        quantity: u32,
        /// The item instance id.
        instance: [u8; RAW_ID_LEN],
        /// A byte of unknown meaning (0 in the capture).
        flag: u8,
    },
    /// 3900: the client asks to swap the items of two paperdoll slots (ranged note; an all-zero
    /// instance is an empty slot).
    SwapItems {
        /// The header.
        header: FacetHeader,
        /// The first slot.
        slot_a: u32,
        /// The second slot.
        slot_b: u32,
        /// The instance id of the first slot's item.
        instance_a: [u8; RAW_ID_LEN],
        /// The instance id of the second slot's item.
        instance_b: [u8; RAW_ID_LEN],
        /// A byte of unknown meaning (0 in the captures).
        flag: u8,
    },
    /// 3896: the client asks to take off the item of a paperdoll slot (ranged note).
    UnequipItem {
        /// The header.
        header: FacetHeader,
        /// The paperdoll slot.
        slot: u32,
        /// The bag slot to put it in; `u32::MAX` for the first free one (Inferred).
        bag_slot: u32,
        /// How many (Inferred; 0 and 1 in the captures).
        quantity: u32,
        /// A byte of unknown meaning (1 in the captures).
        flag: u8,
    },
    /// 387: the client asks for the items of a storage: a loot bag of its loot tracker, about
    /// 1.2 s after the kill (loot note, retail 1792696953189).
    SetReplicateStorage {
        /// The header (the player's `GlobalStorageComponent`).
        header: FacetHeader,
        /// The storage id: the bag's entity id.
        storage: [u8; RAW_ID_LEN],
    },
    /// 2473: the client ends its interaction: its acknowledgement of the server's end (15 to 25
    /// ms after each 3746), or its cancel when it gives up or walks away (gather note).
    CancelInteraction {
        /// The header.
        header: FacetHeader,
        /// The object's entity id.
        target: [u8; RAW_ID_LEN],
        /// Its facet entity.
        target_entity: u64,
        /// The interaction id.
        interaction: u64,
    },
    /// 3970: the inventory (loadout) opened or closed. While it is open the client camera
    /// glides to the inventory view, with the look frozen (retail 2026-09-18: camera z 24.41 to
    /// 22.80 at pitch 0.766 while the server held the player still).
    LookingThroughLoadout {
        /// The header (the player's `PlayerComponent`).
        header: FacetHeader,
        /// Whether the inventory is open.
        looking: bool,
    },
    /// 502: the client asks to leave the tutorial for the open world.
    ReturnToMainWorld {
        /// The header.
        header: FacetHeader,
        /// A byte of unknown meaning (0 in the capture).
        flag: u8,
    },
    /// 3772: the player accepts the quest an NPC offers (beach-quests note, section 2).
    AcceptObjective {
        /// The header.
        header: FacetHeader,
        /// CRC-32 of the lowercase objective id.
        objective: u32,
    },
    /// 794: the player turns a finished quest in at its NPC.
    CompleteObjective {
        /// The header.
        header: FacetHeader,
        /// CRC-32 of the lowercase objective id.
        objective: u32,
    },
    /// 2997: the player crafts at the station it uses.
    SetCraftingDetails {
        /// The header (the player's `CraftingComponent`).
        header: FacetHeader,
        /// CRC-32 of the lowercase recipe id.
        recipe: u32,
        /// How many to craft.
        quantity: u32,
        /// The coins the craft costs as the client shows it: the recipes' `CraftingFee` after
        /// the station's tax (Observed: retail took exactly this many coins at 34 of 34 crafts
        /// of four ledgers, `refining_all_stations_single_batch_territorylvlup`, `Refining`,
        /// `Crafting_Forge`, `craft_one_and_batch_craft`; 0 for the beach campfire).
        fee: u32,
        /// The gear score the player picked (run-15 items2: retail made every item of a craft
        /// at exactly this gear score, 18 items of four crafts; 100 for the tools, 0 for food).
        gear_score: u16,
        /// The perk items the player added: the CRC-32 of each one's lowercase item id
        /// (retail spear craft: one `WhetstoneT1`).
        perk_items: Vec<u32>,
        /// A byte of unknown meaning after the perk items (0 in every retail request).
        unknown: u8,
        /// The ingredients the player picked.
        ingredients: Vec<CraftIngredient>,
    },
    /// 2999: the player closes the crafting window.
    CancelCrafting {
        /// The header.
        header: FacetHeader,
    },
    /// 1733: the player salvages one bag item, or opens a loot container item (salvage note:
    /// retail `Playthrough_Part_03`, `MSQ6-lv28` and two other ledgers, 173 calls).
    InitiateSalvage {
        /// The header (the player's `CraftingComponent`).
        header: FacetHeader,
        /// A number of unknown meaning (1 in all 173 retail calls).
        first: u32,
        /// The bag slot of the item.
        bag_slot: u32,
        /// The item's instance id.
        instance: [u8; RAW_ID_LEN],
        /// A number of unknown meaning (0 in all 173 retail calls).
        unknown: u32,
        /// How many of the stack to salvage (Inferred: 1 in all 173 retail calls, and a stack
        /// of two loot containers lost one).
        quantity: u32,
    },
    /// 6188: the player salvages the items of several bag slots at once (retail
    /// `20261008-145727-200`: two calls of 7 and 6 slots; each slot's item went).
    SalvageItems {
        /// The header (the player's `CraftingComponent`).
        header: FacetHeader,
        /// The bag slots.
        bag_slots: Vec<u32>,
    },
    /// 6024: the player salvages every bag item of one UI item class (craft2 report: 17 retail
    /// calls in 7 ledgers; the client's `SalvageAllByClass` writes a u32, a u64 and a u8).
    SalvageByClass {
        /// The header (the player's `CraftingComponent`).
        header: FacetHeader,
        /// The container (Inferred: 1 the bag in all 17 calls, as the first number of 1733).
        container: u32,
        /// The `eItemClass` number of the UI class (71 `UI_Weapon`, 72 `UI_Armor`;
        /// `nw_content::ui_item_class`).
        class: u64,
        /// The rarities it takes, bit `n` for rarity `n` (Inferred: 0x1f in all 17 calls took
        /// items of 0 to 5 perks).
        rarities: u8,
    },
    /// 2297: the player came into a territory area: a point of interest or a larger area
    /// (retail 2026-09-18: one call per area entered, three at the level load; retail sends no
    /// answer).
    TerritoryEntered {
        /// The header (the player's `TerritoryDetectorComponent`).
        header: FacetHeader,
        /// The entity id of the area.
        area: [u8; RAW_ID_LEN],
        /// Its facet entity.
        area_entity: u64,
        /// An id of unknown meaning.
        extra: u64,
        /// The `TerritoryID` of the area (retail: 40148, the start beach's watchtower).
        territory: u16,
        /// A number of unknown meaning (0 at the first entry, other values later).
        unknown: u32,
    },
    /// 6733: the player walked into a territory (the fallback of the territory detector;
    /// retail sends one big-endian `u16` territory after the header, no answer).
    EnteredTerritoryFallback {
        /// The header.
        header: FacetHeader,
        /// The `TerritoryID`.
        territory: u16,
    },
    /// 6734: the player left a territory; one `u16` after the header, no answer.
    ExitedTerritoryFallback {
        /// The header.
        header: FacetHeader,
        /// The `TerritoryID`.
        territory: u16,
    },
    /// 2525: the player toggles the PvP flag. Retail `PvP_Flag` and `Hive_flag_pvp_on_and_off`
    /// send the component header and nothing after it (6 of 6 calls, 16 bytes).
    TogglePvpFlag {
        /// The header (the player's `FactionComponent`).
        header: FacetHeader,
    },
    /// 3161: the player picks a faction at a faction representative (`Faction_Selection`).
    /// Retail `Hive_join_yellow_faction_quest`, `Marauders_Advancement` and `Change-Faction`:
    /// the component header and one byte, the faction (`nw_content::factions`).
    SetFaction {
        /// The header (the player's `FactionComponent`).
        header: FacetHeader,
        /// The faction.
        faction: u8,
    },
    /// 6416: the player left a territory area (retail sends no answer).
    TerritoryExited {
        /// The header (the player's `TerritoryDetectorComponent`).
        header: FacetHeader,
        /// The entity id of the area.
        area: [u8; RAW_ID_LEN],
        /// Its facet entity.
        area_entity: u64,
        /// An id of unknown meaning.
        extra: u64,
        /// The `TerritoryID` of the area.
        territory: u16,
    },
}

impl ClientFacetMsg {
    /// Decodes `body` when `type_index` is a call this server reads; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            ACK_LEVEL_INFO_TYPE_INDEX => read(|header, reader| {
                Ok(Self::AckLevelInfo {
                    header,
                    client_context: reader.u8()?,
                })
            }),
            CINEMATIC_STARTED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CinematicStarted {
                    header,
                    cinematic: reader.u32()?,
                })
            }),
            TRIGGER_AREA_PASSTHROUGH_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TriggerAreaPassthrough {
                    header,
                    entity: reader.raw_id()?,
                    entity_facet: reader.u64()?,
                    extra: reader.u64()?,
                })
            }),
            UI_SCREEN_OPENED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::UiScreenOpened {
                    header,
                    screen: reader.u32()?,
                })
            }),
            TAKE_ALL_ITEMS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TakeAllItems {
                    header,
                    container: reader.raw_id()?,
                    container_entity: reader.u64()?,
                    extra: reader.u64()?,
                })
            }),
            EQUIP_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::EquipItem {
                    header,
                    bag_slot: reader.u32()?,
                    equip_slot: reader.u32()?,
                    quantity: reader.u32()?,
                    instance: reader.raw_id()?,
                    flag: reader.u8()?,
                })
            }),
            SWAP_ITEMS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SwapItems {
                    header,
                    slot_a: reader.u32()?,
                    slot_b: reader.u32()?,
                    instance_a: reader.raw_id()?,
                    instance_b: reader.raw_id()?,
                    flag: reader.u8()?,
                })
            }),
            UNEQUIP_ITEM_TYPE_INDEX => read(|header, reader| {
                Ok(Self::UnequipItem {
                    header,
                    slot: reader.u32()?,
                    bag_slot: reader.u32()?,
                    quantity: reader.u32()?,
                    flag: reader.u8()?,
                })
            }),
            SET_REPLICATE_STORAGE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetReplicateStorage {
                    header,
                    storage: reader.raw_id()?,
                })
            }),
            CANCEL_INTERACTION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CancelInteraction {
                    header,
                    target: reader.raw_id()?,
                    target_entity: reader.u64()?,
                    interaction: reader.u64()?,
                })
            }),
            CINEMATIC_ENDED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CinematicEnded {
                    header,
                    cinematic: reader.u32()?,
                })
            }),
            REQUEST_CUTSCENE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RequestCutscene {
                    header,
                    cutscene: reader.u32()?,
                })
            }),
            CUTSCENE_ENDED_TYPE_INDEX => read(|header, _| Ok(Self::CutsceneEnded { header })),
            CUTSCENE_STARTED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CutsceneStarted {
                    header,
                    cutscene: reader.u32()?,
                })
            }),
            UPDATE_TRACKED_OBJECTIVES_TYPE_INDEX => read(|header, reader| {
                let count = reader.u8()?;
                let objectives = (0..count)
                    .map(|_| reader.u64())
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::UpdateTrackedObjectives { header, objectives })
            }),
            SET_OBJECTIVE_AUTO_PIN_TYPE_INDEX => read(|header, reader| {
                let count = usize::from(reader.u8()?);
                let flags = reader.bytes(count)?.to_vec();
                Ok(Self::SetObjectiveAutoPin { header, flags })
            }),
            SET_INPUTS_THROTTLED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetInputsThrottled {
                    header,
                    throttled: reader.u8()? != 0,
                })
            }),
            WHISPER_STARTED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::WhisperStarted {
                    header,
                    whisper: reader.u32()?,
                })
            }),
            TIME_SYNC_REQUEST_TYPE_INDEX => read(|header, _| Ok(Self::TimeSyncRequest { header })),
            QTE_COMPLETED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::QteCompleted {
                    header,
                    sequence: reader.u32()?,
                })
            }),
            LOOKING_THROUGH_LOADOUT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::LookingThroughLoadout {
                    header,
                    looking: reader.u8()? != 0,
                })
            }),
            RETURN_TO_MAIN_WORLD_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ReturnToMainWorld {
                    header,
                    flag: reader.u8()?,
                })
            }),
            ACCEPT_OBJECTIVE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::AcceptObjective {
                    header,
                    objective: reader.u32()?,
                })
            }),
            COMPLETE_OBJECTIVE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CompleteObjective {
                    header,
                    objective: reader.u32()?,
                })
            }),
            SET_CRAFTING_DETAILS_TYPE_INDEX => read(|header, reader| {
                let recipe = reader.u32()?;
                let quantity = reader.u32()?;
                let fee = reader.u32()?;
                let gear_score = reader.u16()?;
                let perk_count = reader.u8()?;
                let perk_items = (0..perk_count)
                    .map(|_| reader.u32())
                    .collect::<Result<_, MessageError>>()?;
                let unknown = reader.u8()?;
                let count = reader.u8()?;
                let ingredients = (0..count)
                    .map(|_| {
                        Ok(CraftIngredient {
                            kind: reader.u8()?,
                            quantity: reader.u32()?,
                            item: reader.u32()?,
                        })
                    })
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::SetCraftingDetails {
                    header,
                    recipe,
                    quantity,
                    fee,
                    gear_score,
                    perk_items,
                    unknown,
                    ingredients,
                })
            }),
            CANCEL_CRAFTING_TYPE_INDEX => read(|header, _| Ok(Self::CancelCrafting { header })),
            INITIATE_SALVAGE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::InitiateSalvage {
                    header,
                    first: reader.u32()?,
                    bag_slot: reader.u32()?,
                    instance: reader.raw_id()?,
                    unknown: reader.u32()?,
                    quantity: reader.u32()?,
                })
            }),
            SALVAGE_ITEMS_TYPE_INDEX => read(|header, reader| {
                let count = reader.wide_prefixed()?;
                let bag_slots = (0..count)
                    .map(|_| reader.u32())
                    .collect::<Result<_, MessageError>>()?;
                Ok(Self::SalvageItems { header, bag_slots })
            }),
            SALVAGE_BY_CLASS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SalvageByClass {
                    header,
                    container: reader.u32()?,
                    class: reader.u64()?,
                    rarities: reader.u8()?,
                })
            }),
            TERRITORY_ENTERED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TerritoryEntered {
                    header,
                    area: reader.raw_id()?,
                    area_entity: reader.u64()?,
                    extra: reader.u64()?,
                    territory: reader.u16()?,
                    unknown: reader.u32()?,
                })
            }),
            ENTERED_TERRITORY_FALLBACK_TYPE_INDEX => read(|header, reader| {
                Ok(Self::EnteredTerritoryFallback {
                    header,
                    territory: reader.u16()?,
                })
            }),
            EXITED_TERRITORY_FALLBACK_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ExitedTerritoryFallback {
                    header,
                    territory: reader.u16()?,
                })
            }),
            TOGGLE_PVP_FLAG_TYPE_INDEX => {
                read(|header, _reader| Ok(Self::TogglePvpFlag { header }))
            }
            SET_FACTION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SetFaction {
                    header,
                    faction: reader.u8()?,
                })
            }),
            TERRITORY_EXITED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::TerritoryExited {
                    header,
                    area: reader.raw_id()?,
                    area_entity: reader.u64()?,
                    extra: reader.u64()?,
                    territory: reader.u16()?,
                })
            }),
            _ => return None,
        })
    }
}

/// 6310: the phases of the player's sectors (the entries of its `LevelInfoChangedMsg`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePhasingMsg {
    /// The header.
    pub header: FacetHeader,
    /// The entries.
    pub entries: Vec<PhasingEntry>,
    /// A byte of unknown meaning: 3 in the tutorial, 2 in the open world, 0 in an empty form.
    pub mode: u8,
    /// Rises with each update; the client acknowledges it (6745).
    pub sequence: u8,
}

impl BodyCodec for UpdatePhasingMsg {
    const TYPE_INDEX: u64 = UPDATE_PHASING_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        // A `usize` count fits 64 bits.
        writer.wide_prefixed(self.entries.len() as u64);
        for entry in &self.entries {
            entry.write(&mut writer);
        }
        Ok(writer.u8(self.mode).u8(self.sequence).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let count = reader.wide_prefixed()?;
        let mut entries = Vec::new();
        for _ in 0..count {
            entries.push(PhasingEntry::read(&mut reader)?);
        }
        let message = Self {
            header,
            entries,
            mode: reader.u8()?,
            sequence: reader.u8()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 6707: the server allows the cutscene the client asked for; it echoes the asked id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutsceneGrantedMsg {
    /// The header.
    pub header: FacetHeader,
    /// The cutscene id of the request.
    pub cutscene: u32,
}

impl BodyCodec for CutsceneGrantedMsg {
    const TYPE_INDEX: u64 = CUTSCENE_GRANTED_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.u32(self.cutscene).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            cutscene: reader.u32()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 6709: the server closes the cutscene that the client ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutsceneEndFinalizedMsg {
    /// The header.
    pub header: FacetHeader,
}

impl BodyCodec for CutsceneEndFinalizedMsg {
    const TYPE_INDEX: u64 = CUTSCENE_END_FINALIZED_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 3951: the client shows its loading screen for a level change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeleportWithLoadingScreenMsg {
    /// The header.
    pub header: FacetHeader,
    /// A flag (1 in the capture).
    pub flag: bool,
}

impl BodyCodec for TeleportWithLoadingScreenMsg {
    const TYPE_INDEX: u64 = TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.boolean(self.flag).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            flag: reader.boolean()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 7048: storage search items for the player's global storage. Retail sends it with no search
/// and no items before the bundle of each change of the player's items (tutorial-quest note,
/// section 10). With a search (run-15 items2 study, MSQ16) it lists the results of the last
/// 7046 whose totals changed: each one's index in the 7046 list and the item with its new
/// total; then the count of the 7046 results (`u32`) and one more byte (0 in every retail
/// body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSearchItemsMsg {
    /// The header (the player's `GlobalStorageComponent`).
    pub header: FacetHeader,
    /// The search id; all zero for no search.
    pub search: [u8; RAW_ID_LEN],
    /// The changed results.
    pub items: Vec<SearchedItem>,
    /// The count of the search's results.
    pub total: u32,
    /// The last byte (0 in every retail body; meaning Unknown).
    pub last: u8,
}

/// One changed result of a 7048: its index in the 7046 list and the item with its new total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchedItem {
    /// The index of the result in the 7046 list.
    pub index: u32,
    /// The item and its new total.
    pub item: crate::storage_search::StorageItemSimple,
}

/// Bytes of one [`SearchedItem`]: the index and the 11-byte item.
const SEARCHED_ITEM_BYTES: usize = 15;

impl StorageSearchItemsMsg {
    /// The message retail sends at a change of the items of the player of entity number
    /// `entity_number`: no search, no items.
    pub fn item_change(entity_number: u64) -> Self {
        Self {
            header: FacetHeader::of_child(
                component::GLOBAL_STORAGE,
                component::GLOBAL_STORAGE_CHILD,
                entity_number,
            ),
            search: [0; RAW_ID_LEN],
            items: Vec::new(),
            total: 0,
            last: 0,
        }
    }
}

impl BodyCodec for StorageSearchItemsMsg {
    const TYPE_INDEX: u64 = RECEIVE_STORAGE_SEARCH_ITEMS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        // A `usize` count fits 64 bits.
        writer
            .bytes(&self.search)
            .wide_prefixed(self.items.len() as u64);
        for searched in &self.items {
            writer.u32(searched.index);
            searched.item.write(&mut writer)?;
        }
        Ok(writer.u32(self.total).u8(self.last).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let search = reader.array()?;
        let count = crate::storage_search::read_count(&mut reader, SEARCHED_ITEM_BYTES)?;
        let items = (0..count)
            .map(|_| {
                Ok(SearchedItem {
                    index: reader.u32()?,
                    item: crate::storage_search::StorageItemSimple::read(&mut reader)?,
                })
            })
            .collect::<Result<_, MessageError>>()?;
        let message = Self {
            header,
            search,
            items,
            total: reader.u32()?,
            last: reader.u8()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 3743: the server's answer to an interaction the client started (ftue-static-entities note,
/// section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionValidatedMsg {
    /// The header (the player's `InteractorComponent`).
    pub header: FacetHeader,
    /// The entity id of the used object.
    pub target: [u8; RAW_ID_LEN],
    /// Its facet entity.
    pub target_entity: u64,
    /// The interaction id the client sent.
    pub interaction: u64,
    /// The interaction kind (14 to gather).
    pub kind: u8,
    /// The result (1 accepted).
    pub status: u8,
    /// The display name string id of the object.
    pub name: Vec<u8>,
    /// A number of unknown meaning (0 in the capture).
    pub extra: u32,
    /// An id of unknown meaning.
    pub id: [u8; RAW_ID_LEN],
}

impl BodyCodec for InteractionValidatedMsg {
    const TYPE_INDEX: u64 = INTERACTION_VALIDATED_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer
            .bytes(&self.target)
            .u64(self.target_entity)
            .u64(self.interaction)
            .u8(self.kind)
            .u8(self.status);
        // A `usize` length fits 64 bits.
        writer.wide_prefixed(self.name.len() as u64);
        Ok(writer
            .bytes(&self.name)
            .u32(self.extra)
            .bytes(&self.id)
            .finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let target = reader.raw_id()?;
        let target_entity = reader.u64()?;
        let interaction = reader.u64()?;
        let kind = reader.u8()?;
        let status = reader.u8()?;
        // A length past the body fails the read below.
        let length = usize::try_from(reader.wide_prefixed()?).unwrap_or(usize::MAX);
        let name = reader.bytes(length)?.to_vec();
        let message = Self {
            header,
            target,
            target_entity,
            interaction,
            kind,
            status,
            name,
            extra: reader.u32()?,
            id: reader.raw_id()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 2993: the progress of the player's craft: the recipe crafted and how many crafts, then an
/// empty update when the station is idle again (beach-quests note: retail sends both 1,057 ms
/// after the request, and one more empty update after the close). Both counts are the crafts of
/// the request, not the items they make (Observed: 34 retail updates of four crafting ledgers,
/// 1 to 85 crafts, both counts equal to the request's quantity; `Instrument_IridescentInlayT3`,
/// `OutputQty` 2, showed 1 and 1 for one craft).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftProgressMsg {
    /// The header (the player's `CraftingComponent`).
    pub header: FacetHeader,
    /// The recipe id; empty when idle.
    pub recipe: Vec<u8>,
    /// How many crafts the request asked for; 0 when idle.
    pub quantity: u32,
    /// How many crafts were made; 0 when idle.
    pub completed: u32,
}

impl CraftProgressMsg {
    /// The crafting component of header `header` crafted `crafts` crafts of recipe `recipe`.
    /// Retail answers with the header of the client's request (2997): newchar 2026-09-18, target
    /// `1ad9661ac8b0dca8` in both; the target less the entity differed between characters, and
    /// a header of the player's entity plus [`component::CRAFTING`] left the live client's craft
    /// window open (2026-09-26: it closed 31 s later, retail's 10 ms after the answer).
    pub const fn crafted(header: FacetHeader, recipe: Vec<u8>, crafts: u32) -> Self {
        Self {
            header,
            recipe,
            quantity: crafts,
            completed: crafts,
        }
    }

    /// The crafting component of header `header` is idle.
    pub const fn idle(header: FacetHeader) -> Self {
        Self {
            header,
            recipe: Vec::new(),
            quantity: 0,
            completed: 0,
        }
    }
}

impl BodyCodec for CraftProgressMsg {
    const TYPE_INDEX: u64 = CRAFTING_PROGRESS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        // A `usize` length fits 64 bits.
        writer.wide_prefixed(self.recipe.len() as u64);
        Ok(writer
            .bytes(&self.recipe)
            .u32(self.quantity)
            .u32(self.completed)
            .finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        // A length past the body fails the read below.
        let length = usize::try_from(reader.wide_prefixed()?).unwrap_or(usize::MAX);
        let message = Self {
            header,
            recipe: reader.bytes(length)?.to_vec(),
            quantity: reader.u32()?,
            completed: reader.u32()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// A facet call that is its header only.
macro_rules! header_only_message {
    ($(#[$doc:meta])* $name:ident, $type_index:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            /// The header.
            pub header: FacetHeader,
        }

        impl BodyCodec for $name {
            const TYPE_INDEX: u64 = $type_index;

            fn encode(&self) -> Result<Vec<u8>, MessageError> {
                let mut writer = Writer::new();
                self.header.write(&mut writer);
                Ok(writer.finish())
            }

            fn decode(body: &[u8]) -> Result<Self, MessageError> {
                let mut reader = Reader::new(body);
                let message = Self {
                    header: FacetHeader::read(&mut reader)?,
                };
                reader.finish()?;
                Ok(message)
            }
        }
    };
}
pub(crate) use header_only_message;

header_only_message!(
    /// 955: a tutorial sailor saw the player; its header names the sailor's tutorial AI.
    PlayerSeenByAiMsg,
    PLAYER_SEEN_BY_AI_TYPE_INDEX
);
header_only_message!(
    /// 1793: a dead creature stops colliding; its header names the creature's
    /// `CharacterComponent`.
    DisableCollisionMsg,
    DISABLE_COLLISION_TYPE_INDEX
);

/// 608: a status effect's visual script on the player turns on or off (retail: the potion's
/// `potionheal` after the drink; xp-potion-exit note, section 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustFxScriptsMsg {
    /// The header (the player's `StatusEffectsComponent`).
    pub header: FacetHeader,
    /// CRC-32 of the lowercase script name.
    pub script: u32,
    /// Whether the script turns on.
    pub on: bool,
}

impl AdjustFxScriptsMsg {
    /// The script of CRC `script` turns on for player `player`.
    pub fn on(player: u64, script: u32) -> Self {
        Self {
            header: FacetHeader::of_child(
                component::STATUS_EFFECTS,
                component::STATS_CHILD,
                player,
            ),
            script,
            on: true,
        }
    }
}

impl BodyCodec for AdjustFxScriptsMsg {
    const TYPE_INDEX: u64 = ADJUST_FX_SCRIPTS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.u32(self.script).boolean(self.on).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            script: reader.u32()?,
            on: reader.boolean()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 2415 and 3916: a creature picks the player as its target, or drops it (live-tutorial note,
/// section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetMsg {
    /// The header (the player's `AITargetableComponent`).
    pub header: FacetHeader,
    /// The creature's facet entity.
    pub creature: u64,
}

impl TargetMsg {
    fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer.u64(self.creature).finish()
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            creature: reader.u64()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 2415: a creature picked the player as its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedAsTargetMsg(pub TargetMsg);

/// 3916: a creature no longer targets the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnselectedAsTargetMsg(pub TargetMsg);

impl BodyCodec for SelectedAsTargetMsg {
    const TYPE_INDEX: u64 = SELECTED_AS_TARGET_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        Ok(self.0.encode())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        TargetMsg::decode(body).map(Self)
    }
}

impl BodyCodec for UnselectedAsTargetMsg {
    const TYPE_INDEX: u64 = UNSELECTED_AS_TARGET_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        Ok(self.0.encode())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        TargetMsg::decode(body).map(Self)
    }
}

header_only_message!(
    /// 6426: the player's game mode schedule changed (retail 2026-09-18: 27 ms after each quest
    /// turn-in, and twice at the level load).
    GameModeSchedulerRefreshedMsg,
    GAME_MODE_SCHEDULER_REFRESHED_TYPE_INDEX
);

impl GameModeSchedulerRefreshedMsg {
    /// The call to player `player`.
    pub fn to(player: u64) -> Self {
        Self {
            header: FacetHeader::of(component::GAME_MODE_PARTICIPANT, player),
        }
    }
}

/// 5070: the client plays a whisper, the voice of a game event (retail 2026-09-18: the first
/// whisper of Amidst the Wreckage; the client answers with 5235 and the same id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartWhisperMsg {
    /// The header (the player's `WhisperPlayerComponent`).
    pub header: FacetHeader,
    /// CRC-32 of the lowercase whisper id.
    pub whisper: u32,
}

impl StartWhisperMsg {
    /// Player `player` hears the whisper of CRC `whisper`.
    pub fn to(player: u64, whisper: u32) -> Self {
        Self {
            header: FacetHeader::of(component::WHISPER_PLAYER, player),
            whisper,
        }
    }
}

impl BodyCodec for StartWhisperMsg {
    const TYPE_INDEX: u64 = START_WHISPER_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.u32(self.whisper).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            whisper: reader.u32()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 1790: clear the homing override when a mob picks the player (retail newchar 2026-09-18:
/// sent with the 2415, a zero target and [`NO_HOMING_OVERRIDE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomingOverrideMsg {
    /// The header (the player's `HomingComponent`).
    pub header: FacetHeader,
    /// The override target. All zeros: none.
    pub target: [u8; RAW_ID_LEN],
    /// A word retail leaves 0 in the tutorial clear.
    pub kind: u32,
    /// [`NO_HOMING_OVERRIDE`] when no entity is forced.
    pub override_target: u32,
    /// Two words retail leaves 0 in the tutorial clear.
    pub rest: [u32; 2],
}

impl HomingOverrideMsg {
    /// The clear retail sends to player `player` when a mob picks it.
    pub fn clear(player: u64) -> Self {
        Self {
            header: FacetHeader::of(component::HOMING, player),
            target: [0; RAW_ID_LEN],
            kind: 0,
            override_target: NO_HOMING_OVERRIDE,
            rest: [0; 2],
        }
    }
}

impl BodyCodec for HomingOverrideMsg {
    const TYPE_INDEX: u64 = HOMING_OVERRIDE_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        writer.bytes(&self.target);
        writer.u32(self.kind).u32(self.override_target);
        for word in self.rest {
            writer.u32(word);
        }
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            target: reader.raw_id()?,
            kind: reader.u32()?,
            override_target: reader.u32()?,
            rest: [reader.u32()?, reader.u32()?],
        };
        reader.finish()?;
        Ok(message)
    }
}

/// 4970: the player left a game-mode queue (retail newchar 2026-09-18: one `u64` of 1,
/// 24 ms after the level info).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeftGameModeQueueMsg {
    /// The header (the player's `GameModeParticipantComponent`).
    pub header: FacetHeader,
    /// How the player left. Retail's tutorial value is [`LEFT_QUEUE_ONCE`].
    pub left: u64,
}

impl LeftGameModeQueueMsg {
    /// Player `player` left the queue the way the tutorial login does.
    pub fn tutorial(player: u64) -> Self {
        Self {
            header: FacetHeader::of(component::GAME_MODE_PARTICIPANT, player),
            left: LEFT_QUEUE_ONCE,
        }
    }
}

impl BodyCodec for LeftGameModeQueueMsg {
    const TYPE_INDEX: u64 = LEFT_GAME_MODE_QUEUE_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer.u64(self.left).finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let message = Self {
            header: FacetHeader::read(&mut reader)?,
            left: reader.u64()?,
        };
        reader.finish()?;
        Ok(message)
    }
}

header_only_message!(
    /// 3746: the interaction is over (the gather time ran out).
    InteractionEndedMsg,
    INTERACTION_ENDED_TYPE_INDEX
);
header_only_message!(
    /// 2724: the gatherable's gathering ends; its header names the gatherable.
    GatheringInterruptedMsg,
    GATHERING_INTERRUPTED_TYPE_INDEX
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The Soul Survivor session: its facet entity and its 6310 body (tutorial phase).
    const SOUL_ENTITY: u64 = 0x650f_7a09_cf50_b550;
    const SOUL_ACTION_LIST_TARGET: u64 = 0xa168_9458_7313_62c5;
    const ACTION_LIST: u64 = 0x3c59_1a4e_a3c2_ad75;
    const SOUL_PHASING: &str = concat!(
        "9e563615d991392a650f7a09cf50b550",
        "01",
        "03",
        "00000000",
        "8b2cbbb86aa99537",
        "00000002",
        "03",
        "01"
    );
    const SOUL_GRANT: &str = "39a8ab5add6e878f650f7a09cf50b550a81bd1af";
    /// The facet entity of the retail new character of 2026-09-18.
    const RETAIL_ENTITY: u64 = 0x2715_f9bc_a18f_dece;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn soul(component: u64) -> FacetHeader {
        // The entity number whose entry id reverses to the Soul entity.
        FacetHeader::of(component, crate::entity_ids::number_of_facet(SOUL_ENTITY))
    }

    #[test]
    fn the_quest_calls_read_the_retail_bodies() {
        // Retail 3772 and 794 of 12_skinningknife (`f52332f7`), beach-quests note.
        const ACCEPT: &str = "8120e72f6a93d58d650f7a09cf50b550f52332f7";
        const SKINNING_KNIFE: u32 = 0xf523_32f7;
        let header = FacetHeader {
            target: 0x8120_e72f_6a93_d58d,
            entity: SOUL_ENTITY,
        };
        assert_eq!(
            ClientFacetMsg::decode(ACCEPT_OBJECTIVE_TYPE_INDEX, &hex(ACCEPT)),
            Some(Ok(ClientFacetMsg::AcceptObjective {
                header,
                objective: SKINNING_KNIFE
            }))
        );
        assert_eq!(
            ClientFacetMsg::decode(COMPLETE_OBJECTIVE_TYPE_INDEX, &hex(ACCEPT)),
            Some(Ok(ClientFacetMsg::CompleteObjective {
                header,
                objective: SKINNING_KNIFE
            }))
        );
    }

    #[test]
    fn the_storage_call_reads_the_retail_body() {
        // Retail 387 of the loot bag of the kill at 1792696951908 (loot note).
        const ASK: &str = "7af4d38ff285bbe22715f9bca18fdeceb88319b3219d456295f52fb304838f5c";
        let body = hex(ASK);
        let storage: [u8; RAW_ID_LEN] = body[16..].try_into().expect("bag id");
        assert_eq!(
            ClientFacetMsg::decode(SET_REPLICATE_STORAGE_TYPE_INDEX, &body),
            Some(Ok(ClientFacetMsg::SetReplicateStorage {
                header: FacetHeader {
                    target: 0x7af4_d38f_f285_bbe2,
                    entity: 0x2715_f9bc_a18f_dece,
                },
                storage,
            }))
        );
    }

    #[test]
    fn the_loadout_call_reads_the_live_bodies() {
        // Live 2026-09-18: the inventory opened at the tutorial bag, then closed.
        const OPENED: &str = "8e03e9c98d27619daef701000000000001";
        const CLOSED: &str = "8e03e9c98d27619daef701000000000000";
        let header = FacetHeader {
            target: 0x8e03_e9c9_8d27_619d,
            entity: 0xaef7_0100_0000_0000,
        };
        assert_eq!(
            ClientFacetMsg::decode(LOOKING_THROUGH_LOADOUT_TYPE_INDEX, &hex(OPENED)),
            Some(Ok(ClientFacetMsg::LookingThroughLoadout {
                header,
                looking: true
            }))
        );
        assert_eq!(
            ClientFacetMsg::decode(LOOKING_THROUGH_LOADOUT_TYPE_INDEX, &hex(CLOSED)),
            Some(Ok(ClientFacetMsg::LookingThroughLoadout {
                header,
                looking: false
            }))
        );
    }

    #[test]
    fn a_faction_pick_is_the_header_and_the_faction() {
        // Retail `Hive_join_yellow_faction_quest` at 1782054439.467: the Covenant.
        const BODY: &str = "bb5f9a725dc645ade31988a05074e0b003";
        let header = FacetHeader {
            target: 0xbb5f_9a72_5dc6_45ad,
            entity: 0xe319_88a0_5074_e0b0,
        };
        assert_eq!(
            ClientFacetMsg::decode(SET_FACTION_TYPE_INDEX, &hex(BODY)),
            Some(Ok(ClientFacetMsg::SetFaction { header, faction: 3 }))
        );
    }

    #[test]
    fn a_pvp_flag_toggle_is_the_retail_header() {
        // Retail open-world_PvP_Flag and Hive_flag_pvp_on_and_off: 2525 is 16 bytes.
        const BODY: &str = "68f3a2a317cab5dd4cb591511a7370e0";
        let header = FacetHeader {
            target: 0x68f3_a2a3_17ca_b5dd,
            entity: 0x4cb5_9151_1a73_70e0,
        };
        assert_eq!(
            ClientFacetMsg::decode(TOGGLE_PVP_FLAG_TYPE_INDEX, &hex(BODY)),
            Some(Ok(ClientFacetMsg::TogglePvpFlag { header }))
        );
    }

    #[test]
    fn the_territory_calls_read_the_retail_bodies() {
        // Retail 2026-09-18: the watchtower (40148) entered at the accept of Crewmates No More,
        // and left on the way to the wreck.
        const ENTERED: &str = concat!(
            "dd2cb35d4a60bfb02715f9bca18fdece",
            "33cb5ec44ca08ecf0570d26a6694afc5cf8ea04cc45ecb331f3ec1f5325679ad",
            "9cd4",
            "00000000"
        );
        const WATCHTOWER: u16 = 40_148;
        let header = FacetHeader {
            target: 0xdd2c_b35d_4a60_bfb0,
            entity: RETAIL_ENTITY,
        };
        let area = hex("33cb5ec44ca08ecf0570d26a6694afc5")
            .try_into()
            .expect("an id");
        assert_eq!(
            ClientFacetMsg::decode(TERRITORY_ENTERED_TYPE_INDEX, &hex(ENTERED)),
            Some(Ok(ClientFacetMsg::TerritoryEntered {
                header,
                area,
                area_entity: 0xcf8e_a04c_c45e_cb33,
                extra: 0x1f3e_c1f5_3256_79ad,
                territory: WATCHTOWER,
                unknown: 0,
            }))
        );
        assert_eq!(
            ClientFacetMsg::decode(TERRITORY_EXITED_TYPE_INDEX, &hex(&ENTERED[..100])),
            Some(Ok(ClientFacetMsg::TerritoryExited {
                header,
                area,
                area_entity: 0xcf8e_a04c_c45e_cb33,
                extra: 0x1f3e_c1f5_3256_79ad,
                territory: WATCHTOWER,
            }))
        );
    }

    #[test]
    fn the_whisper_and_the_schedule_refresh_are_retail() {
        // Retail 2026-09-18: 5070 of `01_IsabellasFirstWhisper_Isabella_01` and 6426 after a
        // turn-in.
        const WHISPER: &str = "7b5857d7549305052715f9bca18fdece52f36e05";
        const REFRESHED: &str = "b4fc2d15ad1f9dae2715f9bca18fdece";
        let player = crate::entity_ids::number_of_facet(RETAIL_ENTITY);
        let whisper = StartWhisperMsg::to(player, 0x52f3_6e05);
        assert_eq!(whisper.encode().expect("encode"), hex(WHISPER));
        assert_eq!(StartWhisperMsg::decode(&hex(WHISPER)), Ok(whisper));
        assert_eq!(
            GameModeSchedulerRefreshedMsg::to(player)
                .encode()
                .expect("encode"),
            hex(REFRESHED)
        );
    }

    #[test]
    fn a_salvage_by_class_names_its_class_and_rarities() {
        // The retail 6024 shape (17 calls): 1, the class 0x47 (weapons) or 0x48 (armor), 0x1f.
        const BY_CLASS: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "00000001",
            "0000000000000047",
            "1f"
        );
        assert_eq!(
            ClientFacetMsg::decode(SALVAGE_BY_CLASS_TYPE_INDEX, &hex(BY_CLASS)),
            Some(Ok(ClientFacetMsg::SalvageByClass {
                header: soul(component::CRAFTING),
                container: 1,
                class: 0x47,
                rarities: 0x1f,
            }))
        );
        assert!(matches!(
            ClientFacetMsg::decode(SALVAGE_BY_CLASS_TYPE_INDEX, &hex(&BY_CLASS[..50])),
            Some(Err(_))
        ));
    }

    #[test]
    fn a_salvage_names_its_bag_items() {
        // The retail 1733 shape (173 calls): 1, the bag slot, the instance id, 0, 1. The
        // instance id here is made up.
        const SALVAGE: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "00000001",
            "00000013",
            "00112233445566778899aabbccddeeff",
            "00000000",
            "00000001"
        );
        // The retail 6188 shape: a count, then that many bag slots.
        const SALVAGE_ITEMS: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "03",
            "0000000a0000000e0000000f"
        );
        let header = soul(component::CRAFTING);
        assert_eq!(
            ClientFacetMsg::decode(INITIATE_SALVAGE_TYPE_INDEX, &hex(SALVAGE)),
            Some(Ok(ClientFacetMsg::InitiateSalvage {
                header,
                first: 1,
                bag_slot: 0x13,
                instance: <[u8; RAW_ID_LEN]>::try_from(
                    hex("00112233445566778899aabbccddeeff").as_slice()
                )
                .expect("id"),
                unknown: 0,
                quantity: 1,
            }))
        );
        assert_eq!(
            ClientFacetMsg::decode(SALVAGE_ITEMS_TYPE_INDEX, &hex(SALVAGE_ITEMS)),
            Some(Ok(ClientFacetMsg::SalvageItems {
                header,
                bag_slots: vec![10, 14, 15],
            }))
        );
        assert!(matches!(
            ClientFacetMsg::decode(INITIATE_SALVAGE_TYPE_INDEX, &hex(&SALVAGE[..40])),
            Some(Err(_))
        ));
    }

    #[test]
    fn a_batch_craft_answers_with_its_crafts() {
        // Retail 2993 of 40 crafts of ClothT2 (`refining_all_stations_single_batch_
        // territorylvlup`): the recipe, then 40 twice.
        const CRAFTED: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "07",
            "436c6f74685432",
            "00000028",
            "00000028"
        );
        let crafted = CraftProgressMsg::crafted(soul(component::CRAFTING), b"ClothT2".to_vec(), 40);
        assert_eq!(crafted.encode().expect("encode"), hex(CRAFTED));
        assert_eq!(
            CraftProgressMsg::decode(&hex(CRAFTED)).expect("decode"),
            crafted
        );
    }

    #[test]
    fn a_craft_request_reads_its_ingredients_and_the_answer_is_retail() {
        // Retail 2997 of the skinning knife: FlintT1 x1 and WoodT1 x1 (beach-quests note).
        const KNIFE_GEAR_SCORE: u16 = 100;
        const REQUEST: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "a546f7c5",
            "00000001",
            "00000000",
            "00640000",
            "02",
            "01000000010c9280c8",
            "01000000018a914427"
        );
        const CRAFTED: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "11316853",
            "6b696e6e696e674b6e69666554310000000100000001"
        );
        const IDLE: &str = "5cf3e785f277f442650f7a09cf50b550000000000000000000";
        let header = soul(component::CRAFTING);
        assert_eq!(
            ClientFacetMsg::decode(SET_CRAFTING_DETAILS_TYPE_INDEX, &hex(REQUEST)),
            Some(Ok(ClientFacetMsg::SetCraftingDetails {
                header,
                recipe: 0xa546_f7c5,
                quantity: 1,
                fee: 0,
                gear_score: KNIFE_GEAR_SCORE,
                perk_items: Vec::new(),
                unknown: 0,
                ingredients: vec![
                    CraftIngredient {
                        kind: INGREDIENT_ITEM,
                        quantity: 1,
                        item: 0x0c92_80c8,
                    },
                    CraftIngredient {
                        kind: INGREDIENT_ITEM,
                        quantity: 1,
                        item: 0x8a91_4427,
                    },
                ],
            }))
        );
        assert_eq!(
            ClientFacetMsg::decode(CANCEL_CRAFTING_TYPE_INDEX, &hex(&IDLE[..32])),
            Some(Ok(ClientFacetMsg::CancelCrafting { header }))
        );
        let crafted = CraftProgressMsg::crafted(header, b"1hSkinningKnifeT1".to_vec(), 1);
        assert_eq!(crafted.encode().expect("encode"), hex(CRAFTED));
        assert_eq!(
            CraftProgressMsg::idle(header).encode().expect("encode"),
            hex(IDLE)
        );
        assert_eq!(
            CraftProgressMsg::decode(&hex(CRAFTED)).expect("decode"),
            crafted
        );
    }

    #[test]
    fn a_craft_request_reads_its_gear_score_and_perk_items() {
        // The shape of the retail T4 spear craft (run-15 items2 R3): gear score 420, one perk
        // item (`WhetstoneT1`), the unknown byte, then the ingredients.
        const SPEAR_GEAR_SCORE: u16 = 420;
        const WHETSTONE: u32 = 0x9979_b957;
        const REQUEST: &str = concat!(
            "5cf3e785f277f442650f7a09cf50b550",
            "a546f7c5",
            "00000001",
            "0000005c",
            "01a4",
            "01",
            "9979b957",
            "00",
            "01",
            "01000000010c9280c8"
        );
        let Some(Ok(ClientFacetMsg::SetCraftingDetails {
            fee,
            gear_score,
            perk_items,
            unknown,
            ingredients,
            ..
        })) = ClientFacetMsg::decode(SET_CRAFTING_DETAILS_TYPE_INDEX, &hex(REQUEST))
        else {
            panic!("a craft request");
        };
        assert_eq!(fee, 0x5c);
        assert_eq!(gear_score, SPEAR_GEAR_SCORE);
        assert_eq!(perk_items, [WHETSTONE]);
        assert_eq!(unknown, 0);
        assert_eq!(ingredients.len(), 1);
    }

    #[test]
    fn the_target_is_the_entity_plus_the_component() {
        let header = soul(ACTION_LIST);
        assert_eq!(header.entity, SOUL_ENTITY);
        assert_eq!(header.target, SOUL_ACTION_LIST_TARGET);
    }

    #[test]
    fn a_vitals_call_goes_to_the_stats_child() {
        // Retail sessions (vitals-facet note): player facet entity and 3601 target.
        const SESSIONS: [(u64, u64); 3] = [
            (0xe34e_14f3_44a9_74cc, 0xced4_d31d_edf9_3e30),
            (0x2896_c8c1_4210_296e, 0x032c_072b_f09f_e98e),
            (0x67e7_d282_99ff_40aa, 0x4a3d_0d6d_4acf_51ca),
        ];
        for (player, target) in SESSIONS {
            let header = FacetHeader::of_child(
                component::VITALS,
                component::STATS_CHILD,
                crate::entity_ids::number_of_facet(player),
            );
            assert_eq!((header.target, header.entity), (target, player));
        }
    }

    #[test]
    fn phasing_and_grant_are_the_retail_bytes() {
        let phasing = UpdatePhasingMsg {
            header: soul(component::GRID_TRACKING),
            entries: vec![PhasingEntry {
                key_type: 3,
                key_id: 0,
                phase_id: 0x8b2c_bbb8_6aa9_9537,
                extra: 2,
            }],
            mode: 3,
            sequence: 1,
        };
        let body = phasing.encode().expect("encode");
        assert_eq!(body, hex(SOUL_PHASING));
        assert_eq!(UpdatePhasingMsg::decode(&body), Ok(phasing));
        let grant = CutsceneGrantedMsg {
            header: soul(component::PLAYER_CUTSCENE),
            cutscene: 0xa81b_d1af,
        };
        assert_eq!(grant.encode().expect("encode"), hex(SOUL_GRANT));
    }

    #[test]
    fn a_started_cutscene_and_a_pin_list_are_the_retail_bytes() {
        // newchar-retail-2026-09-18: the crew cutscene started, then an empty pin and one pin.
        const STARTED: &str = "fbaf2b0dafadb10d2715f9bca18fdecea81bd1af";
        const EMPTY_PIN: &str = "7fa055bf3c7be17a2715f9bca18fdece00";
        const ONE_PIN: &str = "7fa055bf3c7be17a2715f9bca18fdece014445f704683ebc78";
        let started = ClientFacetMsg::decode(CUTSCENE_STARTED_TYPE_INDEX, &hex(STARTED))
            .expect("type")
            .expect("body");
        assert!(matches!(
            started,
            ClientFacetMsg::CutsceneStarted {
                cutscene: 0xa81b_d1af,
                ..
            }
        ));
        let empty = ClientFacetMsg::decode(UPDATE_TRACKED_OBJECTIVES_TYPE_INDEX, &hex(EMPTY_PIN))
            .expect("type")
            .expect("body");
        assert!(matches!(
            empty,
            ClientFacetMsg::UpdateTrackedObjectives { objectives, .. } if objectives.is_empty()
        ));
        let one = ClientFacetMsg::decode(UPDATE_TRACKED_OBJECTIVES_TYPE_INDEX, &hex(ONE_PIN))
            .expect("type")
            .expect("body");
        assert!(matches!(
            one,
            ClientFacetMsg::UpdateTrackedObjectives { objectives, .. }
                if objectives == [0x4445_f704_683e_bc78]
        ));
    }

    #[test]
    fn the_homing_clear_and_the_queue_leave_are_the_retail_bytes() {
        // newchar-retail-2026-09-18: 1790 with the 2415, and 4970 after the level info.
        const HOMING: &str = "d4b00cff3ead1b6a2715f9bca18fdece0000000000000000000000000000000000000000ffffffff0000000000000000";
        const LEFT: &str = "b4fc2d15ad1f9dae2715f9bca18fdece0000000000000001";
        const THROTTLE: &str = "636f140b45528c432715f9bca18fdece01";
        const WHISPER: &str = "7b5857d7549305052715f9bca18fdece52f36e05";
        let homing = HomingOverrideMsg::decode(&hex(HOMING)).expect("homing");
        assert_eq!(homing.target, [0; RAW_ID_LEN]);
        assert_eq!(homing.override_target, NO_HOMING_OVERRIDE);
        assert_eq!(
            HomingOverrideMsg::decode(&homing.encode().expect("encode")),
            Ok(homing)
        );
        let left = LeftGameModeQueueMsg::decode(&hex(LEFT)).expect("left");
        assert_eq!(left.left, LEFT_QUEUE_ONCE);
        assert_eq!(
            LeftGameModeQueueMsg::decode(&left.encode().expect("encode")),
            Ok(left)
        );
        let throttled = ClientFacetMsg::decode(SET_INPUTS_THROTTLED_TYPE_INDEX, &hex(THROTTLE))
            .expect("type")
            .expect("body");
        assert!(matches!(
            throttled,
            ClientFacetMsg::SetInputsThrottled {
                throttled: true,
                ..
            }
        ));
        let whisper = ClientFacetMsg::decode(WHISPER_STARTED_TYPE_INDEX, &hex(WHISPER))
            .expect("type")
            .expect("body");
        assert!(matches!(
            whisper,
            ClientFacetMsg::WhisperStarted {
                whisper: 0x52f3_6e05,
                ..
            }
        ));
    }

    #[test]
    fn the_ai_messages_are_the_retail_bytes() {
        // Live tutorial (live-tutorial note, section 7): player facet, sailor 1, and the 2415
        // and 955 bodies.
        const PLAYER: u64 = 0xbfa9_6f3e_a2e3_6a5a;
        const SAILOR: u64 = 0xe5be_db8e_4d15_10aa;
        const SELECTED: &str = "11b141485d60e832bfa96f3ea2e36a5ae5bedb8e4d1510aa";
        const SEEN: &str = "b6e6f36b14aaa94fe5bedb8e4d1510aa";
        let selected = SelectedAsTargetMsg(TargetMsg {
            header: FacetHeader::of(
                component::AI_TARGETABLE,
                crate::entity_ids::number_of_facet(PLAYER),
            ),
            creature: SAILOR,
        });
        assert_eq!(selected.encode().expect("encode"), hex(SELECTED));
        assert_eq!(SelectedAsTargetMsg::decode(&hex(SELECTED)), Ok(selected));
        let seen = PlayerSeenByAiMsg {
            header: FacetHeader::of_child(
                component::TUTORIAL_AI,
                component::TUTORIAL_AI_CHILD,
                crate::entity_ids::number_of_facet(SAILOR),
            ),
        };
        assert_eq!(seen.encode().expect("encode"), hex(SEEN));
    }

    #[test]
    fn the_interaction_answer_round_trips() {
        const GATHER: u8 = 14;
        let answer = InteractionValidatedMsg {
            header: soul(component::INTERACTOR),
            target: [0xef; RAW_ID_LEN],
            target_entity: 0x5870_3581_9fa0_a5ef,
            interaction: 0xd296_06d2_31fe_d9f2,
            kind: GATHER,
            status: 1,
            name: b"@LootContainer_AncientSmall_DisplayName".to_vec(),
            extra: 0,
            id: [0x1b; RAW_ID_LEN],
        };
        let body = answer.encode().expect("encode");
        // Header 16, target 16, entity 8, interaction 8, kind and status: the retail name
        // length 0x27 follows at byte 50.
        const NAME_LENGTH_AT: usize = 50;
        assert_eq!(
            body[NAME_LENGTH_AT],
            u8::try_from(answer.name.len()).expect("len")
        );
        assert_eq!(InteractionValidatedMsg::decode(&body), Ok(answer));
        let ended = InteractionEndedMsg {
            header: soul(component::INTERACTOR),
        };
        assert_eq!(
            InteractionEndedMsg::decode(&ended.encode().expect("encode")),
            Ok(ended)
        );
    }

    #[test]
    fn bag_and_equip_calls_decode() {
        const BAG_SLOT: u32 = 2;
        const MAIN_HAND_OPTION_2: u32 = 25;
        let instance = [0xaf; RAW_ID_LEN];
        let mut writer = Writer::new();
        soul(component::PLAYER).write(&mut writer);
        let equip = writer
            .u32(BAG_SLOT)
            .u32(MAIN_HAND_OPTION_2)
            .u32(1)
            .bytes(&instance)
            .u8(0)
            .finish();
        assert_eq!(
            ClientFacetMsg::decode(EQUIP_ITEM_TYPE_INDEX, &equip),
            Some(Ok(ClientFacetMsg::EquipItem {
                header: soul(component::PLAYER),
                bag_slot: BAG_SLOT,
                equip_slot: MAIN_HAND_OPTION_2,
                quantity: 1,
                instance,
                flag: 0,
            }))
        );
        let mut writer = Writer::new();
        soul(component::PLAYER).write(&mut writer);
        let area = [0xff; RAW_ID_LEN];
        let passthrough = writer.bytes(&area).u64(3).u64(4).finish();
        assert_eq!(
            ClientFacetMsg::decode(TRIGGER_AREA_PASSTHROUGH_TYPE_INDEX, &passthrough),
            Some(Ok(ClientFacetMsg::TriggerAreaPassthrough {
                header: soul(component::PLAYER),
                entity: area,
                entity_facet: 3,
                extra: 4,
            }))
        );
        let container = [0x2f; RAW_ID_LEN];
        let mut writer = Writer::new();
        soul(component::PLAYER).write(&mut writer);
        let take = writer.bytes(&container).u64(1).u64(2).finish();
        assert_eq!(
            ClientFacetMsg::decode(TAKE_ALL_ITEMS_TYPE_INDEX, &take),
            Some(Ok(ClientFacetMsg::TakeAllItems {
                header: soul(component::PLAYER),
                container,
                container_entity: 1,
                extra: 2,
            }))
        );
    }

    #[test]
    fn client_calls_decode_and_others_are_left() {
        let mut body = Vec::new();
        let mut writer = Writer::new();
        soul(component::PLAYER).write(&mut writer);
        body.extend(writer.u8(1).finish());
        assert_eq!(
            ClientFacetMsg::decode(ACK_LEVEL_INFO_TYPE_INDEX, &body),
            Some(Ok(ClientFacetMsg::AckLevelInfo {
                header: soul(component::PLAYER),
                client_context: 1,
            }))
        );
        assert!(matches!(
            ClientFacetMsg::decode(CUTSCENE_ENDED_TYPE_INDEX, &body),
            Some(Err(MessageError::TrailingBytes { .. }))
        ));
        assert_eq!(
            ClientFacetMsg::decode(UPDATE_PHASING_TYPE_INDEX, &body),
            None
        );
        // 4248 is the header alone (facets note: retail 1432 ms).
        let mut writer = Writer::new();
        soul(component::TIME).write(&mut writer);
        assert_eq!(
            ClientFacetMsg::decode(TIME_SYNC_REQUEST_TYPE_INDEX, &writer.finish()),
            Some(Ok(ClientFacetMsg::TimeSyncRequest {
                header: soul(component::TIME),
            }))
        );
    }

    #[test]
    fn the_storage_search_items_of_an_item_change_match_the_retail_body() {
        // Retail 7048 of the new character of 2026-09-18: the header and 22 zero bytes.
        const RETAIL_ITEM_CHANGE: &str = concat!(
            "7af4d38ff285bbe2",
            "2715f9bca18fdece",
            "00000000000000000000000000000000",
            "00",
            "00000000",
            "00"
        );
        let number = crate::entity_ids::number_of_facet(RETAIL_ENTITY);
        let message = StorageSearchItemsMsg::item_change(number);
        assert_eq!(message.encode(), Ok(hex(RETAIL_ITEM_CHANGE)));
        assert_eq!(
            StorageSearchItemsMsg::decode(&hex(RETAIL_ITEM_CHANGE)),
            Ok(message)
        );
    }

    #[test]
    fn a_storage_search_reads_its_total_and_its_changed_items() {
        // The settlement capture's form: a search id, no items, the total 158 (0x9e), 0.
        const SEARCH_TOTAL: u32 = 158;
        // Retail MSQ16: result 1 of 12 changed to a total of 2.
        const RETAIL_CHANGED: &str = concat!(
            "fd34bbfc0f73dcfe5ad5e1c8869e39e2",
            "44031344c3064ddfb1025e7d99a01c12",
            "01",
            "00000001",
            "01896e2997d99400000002",
            "0000000c",
            "00"
        );
        let header = FacetHeader::of_child(
            component::GLOBAL_STORAGE,
            component::GLOBAL_STORAGE_CHILD,
            1,
        );
        let searched = StorageSearchItemsMsg {
            header,
            search: [7; RAW_ID_LEN],
            items: Vec::new(),
            total: SEARCH_TOTAL,
            last: 0,
        };
        let body = searched.encode().expect("encode");
        assert_eq!(&body[body.len() - 6..], &hex("000000009e00")[..]);
        assert_eq!(StorageSearchItemsMsg::decode(&body), Ok(searched));
        let retail = hex(RETAIL_CHANGED);
        let changed = StorageSearchItemsMsg::decode(&retail).expect("changed");
        assert_eq!(changed.total, 12);
        assert_eq!(
            changed.items,
            [SearchedItem {
                index: 1,
                item: crate::storage_search::StorageItemSimple {
                    item: 0x2997_d994,
                    quantity: 2,
                },
            }]
        );
        assert_eq!(changed.encode(), Ok(retail));
        let mut cut = hex(RETAIL_CHANGED);
        // Two items announced, one there.
        cut[32] = 2;
        assert!(StorageSearchItemsMsg::decode(&cut).is_err());
    }
}
