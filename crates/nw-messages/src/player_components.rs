// SPDX-License-Identifier: AGPL-3.0-or-later
//! The replicated components of the player prefab (world-entry note, "Spawn condition traced").
//! A member's component index picks the component of the portrayed entity that takes the state
//! (`FUN_14167beb0`). When the player prefab is portrayed with no state for its other components,
//! the real client crashes while it applies the cached states. So the player's group carries one
//! state for every replicated component: the spawn card, the position, a connected type-3935
//! `PlayerComponentReplicatedState`, the look (type 1195), the vitals, stamina and mana (types
//! 15, 4297 and 1652), the gear (types 3183, 1755 and 5437) and the achievements (type 3210)
//! when known ([`player_members`]), and an empty state ("no slot set") for each other component below.
//!
//! The table is the client's own fragment log of a captured session (`NH-MSQ-10`, 2026-09-16):
//! every member of slot 1 it decoded, as (component index, class type index). All of these
//! classes use the generic slot reader `FUN_1417b4110`, whose first byte is the slot bitmap; on
//! every captured body only bits 0 to 2 are set, so one slot byte of 0 is a whole empty body.
//! Component 0 (the spawn card) and 16 (`ALCReplicatedState`) are not in the table.

use crate::abilities::{ABILITY_COMPONENT, ABILITY_TYPE_INDEX, AbilityState};
use crate::achievements::{ACHIEVEMENT_TYPE_INDEX, AchievementState};
use crate::appearance::PlayerLook;
use crate::appearance::{PLAYER_APPEARANCE_TYPE_INDEX, PlayerAppearanceState};
use crate::attributes::{ATTRIBUTE_COMPONENT, ATTRIBUTE_TYPE_INDEX, AttributeState};
use crate::categorical::{
    CATEGORICAL_PROGRESSION_COMPONENT, CATEGORICAL_PROGRESSION_TYPE_INDEX,
    CategoricalProgressionState,
};
use crate::components::{
    COMBAT_STATUS_TYPE_INDEX, CURRENCY_TYPE_INDEX, CombatStatusState, CurrencyState,
    DAMAGE_RECEIVER_TYPE_INDEX, DamageReceiverState, MANA_TYPE_INDEX, ManaState,
    STAMINA_TYPE_INDEX, StaminaState,
};
use crate::containers::{
    BAG_COMPONENT, CONTAINER_TYPE_INDEX, ContainerState, ITEM_MANAGEMENT_TYPE_INDEX,
    ItemManagementState,
};
use crate::cooldowns::{COOLDOWNS_TYPE_INDEX, CooldownsState};
use crate::faction_state::FACTION_TYPE_INDEX;
use crate::game_events::GAME_EVENT_TYPE_INDEX;
use crate::gathering::{GATHERING_TYPE_INDEX, GatheringState};
use crate::gear::PlayerGear;
use crate::interactor::{INTERACTOR_TYPE_INDEX, InteractorState};
use crate::loot::{
    GLOBAL_STORAGE_TYPE_INDEX, GlobalStorageState, LOOT_TRACKER_TYPE_INDEX, LootTrackerState,
};
use crate::mount::{MOUNT_COMPONENT, MOUNT_TYPE_INDEX, MountState};
use crate::music::PERFORMANCE_PLAYER_TYPE_INDEX as MUSICAL_PERFORMANCE_TYPE_INDEX;
use crate::objective_interactor::OBJECTIVE_INTERACTOR_TYPE_INDEX;
use crate::objectives::{OBJECTIVES_TYPE_INDEX, ObjectivesState};
use crate::paperdoll::PAPERDOLL_TYPE_INDEX;
use crate::payload::{BundleMember, MemberState};
use crate::progression::{PROGRESSION_TYPE_INDEX, ProgressionState};
use crate::progression_points::{
    PROGRESSION_POINT_COMPONENT, PROGRESSION_POINT_TYPE_INDEX, ProgressionPointState,
};
use crate::slots::{children_mask, read_children, read_slot_byte, write_known_children};
use crate::social::states::{
    CHAT_STATE_COMPONENT, CHAT_STATE_TYPE_INDEX, ChatState, GROUPS_STATE_COMPONENT,
    GROUPS_STATE_TYPE_INDEX, GUILDS_STATE_COMPONENT, GUILDS_STATE_TYPE_INDEX, GroupsState,
    GuildsState, SocialState,
};
use crate::status_effects::{STATUS_EFFECTS_TYPE_INDEX, StatusEffectsState};
use crate::temporary_affiliation::TEMPORARY_AFFILIATION_TYPE_INDEX;
use crate::title_choice::{SOCIAL_COMPONENT, SOCIAL_TYPE_INDEX};
use crate::travel_states::PLAYER_HOME_TYPE_INDEX;
use crate::vitals::{VITALS_TYPE_INDEX, VitalsData, VitalsState};
use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// Type index of `MB::PlayerComponentReplicatedState`.
pub const PLAYER_COMPONENT_TYPE_INDEX: u64 = 3935;
/// The empty slot byte of a generic-reader body.
const NO_SLOT: u8 = 0;
/// Slots of type 3935: base slot 0 plus two add-slot calls (`FUN_146713160`).
const PLAYER_COMPONENT_SLOTS: u8 = 3;
/// Slot-byte bit for slot 1.
const SLOT_1_BIT: u8 = 0b010;
/// Children registered on slot 1 (death note, 2026-09-18).
const SLOT_1_CHILDREN: u8 = 22;
/// Slot 1 child 3: `accountInProbation`, one boolean byte. While it is true the client refuses
/// to send global and trade chat and currency trades, with `ui_cannot_chat_global_blocked`
/// ("...until your account is older than {hoursNeeded} hours...") (`FUN_14683dba0` copies it,
/// `FUN_14684b480` reads it for the chat send; client defaults `javelin.account-probation-time-
/// hours` 72, `javelin.social.enable-account-age-chat-restriction` 1). Retail sent `02 08 00`
/// 1.9 to 9.4 s after the spawn in all three captured sessions (general-other-01,
/// newchar-linux, newchar-retail).
const CHILD_ACCOUNT_IN_PROBATION: u16 = 1 << 3;
/// Slot 1 child 12: `onDeathRespawnCooldown`, an `f32`. A change of it to 0 or more opens the
/// client's death screen (`FUN_14683dba0`, `Hud.LocalPlayer.OnPlayerDeath`).
const CHILD_ON_DEATH_RESPAWN_COOLDOWN: u16 = 1 << 12;
/// The `onDeathRespawnCooldown` of an open-world death (retail: 0.0 in all three captured
/// deaths; the meaning of other values is Unknown).
const OPEN_WORLD_RESPAWN_COOLDOWN: f32 = 0.0;
/// The component of the player's 3935 state.
pub const PLAYER_COMPONENT_COMPONENT: u64 = 9;
/// Slot-byte bit for slot 2 (packet `group1`: `characterId`, `playerConnected`, …).
const SLOT_2_BIT: u8 = 0b100;
/// Children registered on slot 2, in constructor order.
const SLOT_2_CHILDREN: u8 = 9;
/// Slot 2 child 0: `characterId` (presence `FUN_14087ac50`).
const CHILD_CHARACTER_ID: u16 = 1 << 0;
/// Slot 2 child 3: `playerConnected` (one boolean byte, `FUN_1417b4c20`).
const CHILD_PLAYER_CONNECTED: u16 = 1 << 3;
/// Slot 2 child 4: `lookingThroughLoadout`, one boolean byte (retail `04 10 01` and `04 10 00`).
const CHILD_LOOKING_THROUGH_LOADOUT: u16 = 1 << 4;
/// Slot 2 child 6: `isInStore`, one boolean byte (retail `unlock_store_content`: `04 40 01` 84 ms
/// after the client's 4908 with 1, `04 40 00` 48 ms after the one with 0).
const CHILD_IS_IN_STORE: u16 = 1 << 6;
/// Presence flag bit 0: take the fixed 16-byte path (`FUN_14087ac50`).
const PRESENCE_FLAG_FIXED: u8 = 0x01;
/// Presence flag bit 2: the client turns the 16 bytes into lowercase dashed UUID text. The
/// local-player check compares that text with the login token `CharacterId` (facet `+0xe8`
/// note), so `characterId` must use it; without it the text is 32 bare hex digits.
const PRESENCE_FLAG_UUID_TEXT: u8 = 0x04;
const FIELD: &str = "player component";

/// (component index, class type index) of each replicated component of the player prefab, other
/// than the spawn card and the position.
pub const PLAYER_EMPTY_COMPONENTS: [(u64, u64); 57] = [
    (1, 6951),
    (2, 5437),
    (4, 2406),
    (5, 3290),
    (6, 2768),
    (PLAYER_COMPONENT_COMPONENT, PLAYER_COMPONENT_TYPE_INDEX),
    (10, 3829),
    (12, PLAYER_APPEARANCE_TYPE_INDEX),
    (13, 603),
    (14, 1528),
    (17, 3563),
    (18, 5338),
    (19, 3133),
    (20, 3652),
    (22, 3857),
    (24, 3312),
    (25, 1292),
    (26, INTERACTOR_TYPE_INDEX),
    (ATTRIBUTE_COMPONENT, ATTRIBUTE_TYPE_INDEX),
    (28, COMBAT_STATUS_TYPE_INDEX),
    (29, 6234),
    (31, 1927),
    (32, 2895),
    (crate::loot::LOOT_TRACKER_COMPONENT, LOOT_TRACKER_TYPE_INDEX),
    (34, 4913),
    (35, 3765),
    (36, GATHERING_TYPE_INDEX),
    (MOUNT_COMPONENT, MOUNT_TYPE_INDEX),
    (PLAYER_VITALS_COMPONENT, VITALS_TYPE_INDEX),
    (PLAYER_STAMINA_COMPONENT, STAMINA_TYPE_INDEX),
    (43, 1525),
    (44, 4236),
    (45, 2932),
    (ABILITY_COMPONENT, ABILITY_TYPE_INDEX),
    (47, MANA_TYPE_INDEX),
    (48, 5257),
    (49, 3152),
    (50, 4321),
    (GROUPS_STATE_COMPONENT, GROUPS_STATE_TYPE_INDEX),
    (54, PLAYER_NAME_TAG_TYPE_INDEX),
    (SOCIAL_COMPONENT, SOCIAL_TYPE_INDEX),
    (GUILDS_STATE_COMPONENT, GUILDS_STATE_TYPE_INDEX),
    (CHAT_STATE_COMPONENT, CHAT_STATE_TYPE_INDEX),
    (crate::components::CURRENCY_COMPONENT, CURRENCY_TYPE_INDEX),
    (61, 1755),
    (62, 3183),
    (
        CATEGORICAL_PROGRESSION_COMPONENT,
        CATEGORICAL_PROGRESSION_TYPE_INDEX,
    ),
    (66, 3210),
    (PROGRESSION_POINT_COMPONENT, PROGRESSION_POINT_TYPE_INDEX),
    (68, 497),
    (70, 899),
    (71, 4878),
    (
        crate::seasons::SEASONS_REWARDS_COMPONENT,
        crate::seasons::SEASONS_REWARDS_TYPE_INDEX,
    ),
    (
        crate::seasons::SEASONS_TRACKED_STATS_COMPONENT,
        crate::seasons::SEASONS_TRACKED_STATS_TYPE_INDEX,
    ),
    (74, 5691),
    (76, 1755),
    (
        crate::loot::GLOBAL_STORAGE_COMPONENT,
        GLOBAL_STORAGE_TYPE_INDEX,
    ),
];

/// Type index of `MB::PlayerNameTagComponentReplicatedState` (component 54).
pub const PLAYER_NAME_TAG_TYPE_INDEX: u64 = 100;
/// Type index of `MB::ChargeComponentReplicatedState` (component 48).
const CHARGE_TYPE_INDEX: u64 = 5257;
/// Type index of `MB::AudioProxyComponentReplicatedState` (component 13).
const AUDIO_PROXY_TYPE_INDEX: u64 = 603;

/// Type index of `Javelin::PointsAccumulatorComponentReplicatedState` (component 1).
const POINTS_ACCUMULATOR_TYPE_INDEX: u64 = 6951;
/// Type index of `MB::MagicComponentReplicatedState` (component 32).
const MAGIC_TYPE_INDEX: u64 = 2895;
/// Type index of `Javelin::InstancedSlayerScriptReplicatedState` (component 29).
const INSTANCED_SLAYER_SCRIPT_TYPE_INDEX: u64 = 6234;
/// Type index of `MB::RewardTrackComponentReplicatedState` (component 34).
const REWARD_TRACK_TYPE_INDEX: u64 = 4913;
/// Type index of `MB::ItemSkinningComponentReplicatedState` (component 35).
const ITEM_SKINNING_TYPE_INDEX: u64 = 3765;

/// The first bodies retail gives these classes on a new character's card: the bodies of both
/// new characters of newchar-retail-2026-09-18 and newchar-linux-2026-09-12 (counts: retail first
/// own groups of the 482 captured sessions with the same body). A full list's version (a counter
/// of the retail session, 12236 to 17921 in those captures) is our [`crate::items::FIRST_VERSION`].
const NEW_CHARACTER_FIRST_BODIES: [(u64, &[u8]); 8] = [
    // Slot 0 children 0 to 2: all 0 (482 of 482 retail first groups).
    (
        POINTS_ACCUMULATOR_TYPE_INDEX,
        &[
            0x01, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    ),
    // `state` and `channel` 0 (473 of 482).
    (
        MAGIC_TYPE_INDEX,
        &[0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    ),
    // No performance (476 of 479).
    (
        MUSICAL_PERFORMANCE_TYPE_INDEX,
        &[
            0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x06, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        ],
    ),
    // Slot 0 child 0: an empty list.
    (
        PLAYER_HOME_TYPE_INDEX,
        &[0x01, 0x01, 0x00, 0x01, 0x03, 0x00],
    ),
    // Slot 0 children 2 and 3: two empty lists.
    (
        INSTANCED_SLAYER_SCRIPT_TYPE_INDEX,
        &[0x01, 0x0c, 0x00, 0x01, 0x03, 0x00, 0x00, 0x01, 0x03, 0x00],
    ),
    // Slot 2 child 0 `m_rolledRewards`: an empty list.
    (
        REWARD_TRACK_TYPE_INDEX,
        &[0x04, 0x01, 0x00, 0x01, 0x03, 0x00],
    ),
    // Slot 0 child 1 `dailyBonusesUsed`: an empty list.
    (GAME_EVENT_TYPE_INDEX, &[0x01, 0x02, 0x00, 0x01, 0x03, 0x00]),
    // Slot 1: `m_enabledItemSkins` with one 0, `m_skinDyeData` empty.
    (
        ITEM_SKINNING_TYPE_INDEX,
        &[
            0x02, 0x03, 0x00, 0x01, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01, 0x03, 0x00,
        ],
    ),
];

/// The first bodies retail gives these classes on every new player's card, the same for all
/// (targeting note, 2026-09-18: beach capture q1). Our card left them empty, and the client then
/// never picked a lock-on or attack target (Inferred: its relation filter reads the faction).
/// Classes whose first body names the character (groups, abilities, attributes, status
/// effects, contribution) are not among them.
const RETAIL_FIRST_BODIES: [(u64, &[u8]); 5] = [
    // Slot 1 children 0 and 6, slot 2 children 1 and 3: all 0.
    (
        FACTION_TYPE_INDEX,
        &[0x06, 0x41, 0x00, 0x00, 0x0a, 0x00, 0x00],
    ),
    // Slot 0 child 0: 1.
    (
        PLAYER_NAME_TAG_TYPE_INDEX,
        &[0x01, 0x01, 0x00, 0x00, 0x00, 0x01],
    ),
    (CHARGE_TYPE_INDEX, &[0x01, 0x01, 0x00]),
    (AUDIO_PROXY_TYPE_INDEX, &[0x01, 0x01, 0x00]),
    (OBJECTIVE_INTERACTOR_TYPE_INDEX, &[0x01, 0x08, 0x00]),
];

/// The first body of player component `class`: retail's when it is in
/// [`RETAIL_FIRST_BODIES`] or [`NEW_CHARACTER_FIRST_BODIES`], else one slot byte of 0 (no slot
/// set).
fn first_body(class: u64) -> &'static [u8] {
    RETAIL_FIRST_BODIES
        .iter()
        .chain(&NEW_CHARACTER_FIRST_BODIES)
        .find(|(retail, _)| *retail == class)
        .map_or(&[NO_SLOT], |(_, body)| body)
}

/// The first state of a generic-reader class without a reader of its own here: retail's fixed
/// first body where it is known ([`RETAIL_FIRST_BODIES`]), else one slot byte of 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyState {
    /// The class type index.
    pub class: u64,
}

impl EmptyState {
    /// Whether `class` is a player component class that this state stands for.
    pub fn covers(class: u64) -> bool {
        !has_own_reader(class)
            && PLAYER_EMPTY_COMPONENTS
                .iter()
                .any(|(_, player_class)| *player_class == class)
    }

    /// Reads the first body of `class`: an empty one, or retail's fixed one.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Unsized`] for another body that sets a slot: this server does not
    /// know the children of these classes, so a bundle walk stops there.
    pub(crate) fn read(class: u64, reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let byte = reader.u8()?;
        let body = first_body(class);
        let unsized_body = MessageError::Unsized {
            field: "player component",
            byte,
        };
        if byte == NO_SLOT {
            return Ok(Self { class });
        }
        if body.first() != Some(&byte) || reader.bytes(body.len() - 1)? != &body[1..] {
            return Err(unsized_body);
        }
        Ok(Self { class })
    }

    /// Writes the first body.
    pub(crate) fn write(&self, writer: &mut Writer) {
        writer.bytes(first_body(self.class));
    }

    /// Whether `bytes` are the first body this state writes for `class`.
    pub(crate) fn is_first_body(class: u64, bytes: &[u8]) -> bool {
        bytes == first_body(class)
    }
}

/// `MB::PlayerComponentReplicatedState` (type 3935, component 9). Slot 2 holds
/// `characterId`, `playerConnected` (local-player-check note) and `lookingThroughLoadout` of
/// slot 2, and the `onDeathRespawnCooldown` of slot 1 (death note). Other children and slot 0
/// are not encoded here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerComponentState {
    /// Slot 1 child 3: whether the account is in probation (chat and trade blocked), when sent.
    pub account_in_probation: Option<bool>,
    /// Slot 1 child 12: the respawn cooldown that opens the death screen, when sent.
    pub on_death_respawn_cooldown: Option<f32>,
    /// Slot 2 child 0: the 16-byte character id, when sent.
    pub character_id: Option<[u8; RAW_ID_LEN]>,
    /// Slot 2 child 3: whether this player is connected, when sent.
    pub player_connected: Option<bool>,
    /// Slot 2 child 4: whether this player has its inventory open, when sent.
    pub looking_through_loadout: Option<bool>,
    /// Slot 2 child 6: whether this player has the store open, when sent.
    pub is_in_store: Option<bool>,
}

impl PlayerComponentState {
    /// The first body of a local player: `characterId` is `id`, `playerConnected` is true.
    pub const fn connected(id: [u8; RAW_ID_LEN]) -> Self {
        Self {
            account_in_probation: None,
            on_death_respawn_cooldown: None,
            character_id: Some(id),
            player_connected: Some(true),
            looking_through_loadout: None,
            is_in_store: None,
        }
    }

    /// The change at the player's death that opens its death screen (retail
    /// `09 9f3d 02 80 20 00000000`, 7 to 55 ms after the 4299).
    pub const fn died() -> Self {
        Self {
            account_in_probation: None,
            on_death_respawn_cooldown: Some(OPEN_WORLD_RESPAWN_COOLDOWN),
            character_id: None,
            player_connected: None,
            looking_through_loadout: None,
            is_in_store: None,
        }
    }

    /// The change of `lookingThroughLoadout` to `looking`. Retail sends it at every open and
    /// close of the inventory, 30 to 40 ms after the client's 3970 that changed it (newchar
    /// 2026-09-18: `09 9f3d 04 10 01` at 1792695864643 after the open at 1792695864606, `04 10 00`
    /// at 1792695870142 after the close at 1792695870106). Without it the client closed every
    /// inventory it opened 50 to 60 ms later, with no other server message between (live run
    /// 15; Inferred: its UI follows the replicated flag, `OnIsLookingThroughLoadoutChanged`).
    pub const fn looking_through_loadout(looking: bool) -> Self {
        Self {
            account_in_probation: None,
            on_death_respawn_cooldown: None,
            character_id: None,
            player_connected: None,
            looking_through_loadout: Some(looking),
            is_in_store: None,
        }
    }

    /// The change of `isInStore` to `in_store`, which retail sends after the client's 4908
    /// (`PlayerComponentServerFacet_RequestSetIsInStore`).
    pub const fn in_store(in_store: bool) -> Self {
        Self {
            account_in_probation: None,
            on_death_respawn_cooldown: None,
            character_id: None,
            player_connected: None,
            looking_through_loadout: None,
            is_in_store: Some(in_store),
        }
    }

    /// The change of `accountInProbation` to `in_probation` (retail `02 08 00`).
    pub const fn account_in_probation(in_probation: bool) -> Self {
        Self {
            account_in_probation: Some(in_probation),
            on_death_respawn_cooldown: None,
            character_id: None,
            player_connected: None,
            looking_through_loadout: None,
            is_in_store: None,
        }
    }

    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, sets slot 0, or sets a child this
    /// reader does not size.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slots = read_slot_byte(reader, FIELD, PLAYER_COMPONENT_SLOTS)?;
        if slots & !(SLOT_1_BIT | SLOT_2_BIT) != 0 {
            return Err(MessageError::Unsized {
                field: FIELD,
                byte: slots,
            });
        }
        let mut state = Self::default();
        let unsized_child = |child| MessageError::Unsized {
            field: FIELD,
            byte: child,
        };
        if slots & SLOT_1_BIT != 0 {
            read_children(reader, FIELD, SLOT_1_CHILDREN, |reader, child| {
                // Slot 1 has 22 children: a bit above 15 is no `u16` child.
                let bit = 1_u32 << child;
                if bit == u32::from(CHILD_ACCOUNT_IN_PROBATION) {
                    state.account_in_probation = Some(reader.boolean()?);
                } else if bit == u32::from(CHILD_ON_DEATH_RESPAWN_COOLDOWN) {
                    state.on_death_respawn_cooldown = Some(reader.f32()?);
                } else {
                    return Err(unsized_child(child));
                }
                Ok(())
            })?;
        }
        if slots & SLOT_2_BIT != 0 {
            read_children(reader, FIELD, SLOT_2_CHILDREN, |reader, child| {
                match 1 << child {
                    CHILD_CHARACTER_ID => state.character_id = Some(read_presence_id(reader)?),
                    CHILD_PLAYER_CONNECTED => state.player_connected = Some(reader.boolean()?),
                    CHILD_LOOKING_THROUGH_LOADOUT => {
                        state.looking_through_loadout = Some(reader.boolean()?);
                    }
                    CHILD_IS_IN_STORE => state.is_in_store = Some(reader.boolean()?),
                    _ => return Err(unsized_child(child)),
                }
                Ok(())
            })?;
        }
        Ok(state)
    }

    /// Writes the state for the bundle path. A slot without a known child is not sent.
    pub fn write(&self, writer: &mut Writer) {
        let probation = self.account_in_probation;
        let cooldown = self.on_death_respawn_cooldown;
        let slot_1_children = if probation.is_some() {
            CHILD_ACCOUNT_IN_PROBATION
        } else {
            0
        } | if cooldown.is_some() {
            CHILD_ON_DEATH_RESPAWN_COOLDOWN
        } else {
            0
        };
        let character_id = self.character_id;
        let player_connected = self.player_connected;
        let looking = self.looking_through_loadout;
        let in_store = self.is_in_store;
        let slot_2_children = children_mask(&[
            character_id.is_some(),
            false,
            false,
            player_connected.is_some(),
            looking.is_some(),
            false,
            in_store.is_some(),
        ]);
        let slots = if slot_1_children != 0 {
            SLOT_1_BIT
        } else {
            NO_SLOT
        } | if slot_2_children == 0 {
            NO_SLOT
        } else {
            SLOT_2_BIT
        };
        writer.u8(slots);
        if slot_1_children != 0 {
            write_known_children(
                writer,
                slot_1_children,
                SLOT_1_CHILDREN,
                |writer, child| match 1 << child {
                    CHILD_ACCOUNT_IN_PROBATION => {
                        probation.map(|in_probation| writer.boolean(in_probation));
                    }
                    _ => {
                        cooldown.map(|cooldown| writer.f32(cooldown));
                    }
                },
            );
        }
        if slot_2_children != 0 {
            write_known_children(
                writer,
                slot_2_children,
                SLOT_2_CHILDREN,
                |writer, child| match 1 << child {
                    CHILD_CHARACTER_ID => {
                        character_id.map(|id| {
                            writer
                                .u8(PRESENCE_FLAG_FIXED | PRESENCE_FLAG_UUID_TEXT)
                                .bytes(&id)
                        });
                    }
                    CHILD_PLAYER_CONNECTED => {
                        player_connected.map(|connected| writer.boolean(connected));
                    }
                    CHILD_IS_IN_STORE => {
                        in_store.map(|in_store| writer.boolean(in_store));
                    }
                    _ => {
                        looking.map(|looking| writer.boolean(looking));
                    }
                },
            );
        }
    }
}

/// Reads one presence id (`FUN_14087ac50`): a flag byte whose bit 0 must be set, then 16 bytes.
fn read_presence_id(reader: &mut Reader<'_>) -> Result<[u8; RAW_ID_LEN], MessageError> {
    let flag = reader.u8()?;
    if flag & PRESENCE_FLAG_FIXED == 0 {
        return Err(MessageError::BadTag {
            field: "PlayerComponentReplicatedState characterId flag",
            tag: flag,
        });
    }
    reader.raw_id()
}

/// The empty state of player component `class`: the default of a class with its own reader here
/// (it writes the same single slot byte of 0), else an [`EmptyState`].
pub fn empty_state(class: u64) -> MemberState {
    match class {
        VITALS_TYPE_INDEX => MemberState::Vitals(VitalsState::default()),
        STAMINA_TYPE_INDEX => MemberState::Stamina(StaminaState::default()),
        COMBAT_STATUS_TYPE_INDEX => MemberState::CombatStatus(CombatStatusState::default()),
        PLAYER_COMPONENT_TYPE_INDEX => {
            MemberState::PlayerComponent(PlayerComponentState::default())
        }
        PLAYER_APPEARANCE_TYPE_INDEX => MemberState::Appearance(PlayerAppearanceState::default()),
        MANA_TYPE_INDEX => MemberState::Mana(ManaState::default()),
        PAPERDOLL_TYPE_INDEX => MemberState::Paperdoll(Box::default()),
        CONTAINER_TYPE_INDEX => MemberState::Container(ContainerState::default()),
        ITEM_MANAGEMENT_TYPE_INDEX => MemberState::ItemManagement(ItemManagementState::default()),
        ACHIEVEMENT_TYPE_INDEX => MemberState::Achievement(AchievementState::default()),
        DAMAGE_RECEIVER_TYPE_INDEX => MemberState::DamageReceiver(DamageReceiverState::default()),
        OBJECTIVES_TYPE_INDEX => MemberState::Objectives(ObjectivesState::default()),
        PROGRESSION_TYPE_INDEX => MemberState::Progression(ProgressionState::default()),
        CATEGORICAL_PROGRESSION_TYPE_INDEX => {
            MemberState::CategoricalProgression(CategoricalProgressionState::default())
        }
        PROGRESSION_POINT_TYPE_INDEX => {
            MemberState::ProgressionPoints(ProgressionPointState::default())
        }
        CURRENCY_TYPE_INDEX => MemberState::Currency(CurrencyState::default()),
        COOLDOWNS_TYPE_INDEX => MemberState::Cooldowns(CooldownsState::default()),
        LOOT_TRACKER_TYPE_INDEX => MemberState::LootTracker(LootTrackerState::default()),
        GLOBAL_STORAGE_TYPE_INDEX => MemberState::GlobalStorage(GlobalStorageState::default()),
        GATHERING_TYPE_INDEX => MemberState::Gathering(GatheringState::default()),
        INTERACTOR_TYPE_INDEX => MemberState::Interactor(InteractorState::default()),
        ATTRIBUTE_TYPE_INDEX => MemberState::Attributes(AttributeState::default()),
        ABILITY_TYPE_INDEX => MemberState::Abilities(AbilityState::default()),
        // No mount: retail's login card (mounts note; both new characters, 28 of 482).
        MOUNT_TYPE_INDEX => MemberState::Mount(MountState::login_card()),
        // No group and no title yet: the social service sends them as changes after the admit.
        GROUPS_STATE_TYPE_INDEX => MemberState::Groups(GroupsState::default()),
        SOCIAL_TYPE_INDEX => MemberState::Social(SocialState::default()),
        // No company: `lastGuildLeaveTime` 0 (both new characters; 90 of 482).
        GUILDS_STATE_TYPE_INDEX => MemberState::Guilds(Box::new(GuildsState::new_character())),
        // No mutes: an empty list.
        CHAT_STATE_TYPE_INDEX => MemberState::Chat(ChatState::first()),
        // No board open: the retail first body `01 08 00` (both new characters).
        OBJECTIVE_INTERACTOR_TYPE_INDEX => MemberState::ObjectiveInteractor(
            crate::objective_interactor::ObjectiveInteractorState::new_character(),
        ),
        // No played time yet: one slot byte of 0.
        crate::travel_states::PLAYER_TIME_TYPE_INDEX => {
            MemberState::PlayerTime(crate::travel_states::PlayerTimeState::default())
        }
        // No house yet: one slot byte of 0.
        crate::travel_states::PLAYER_HOUSING_TYPE_INDEX => {
            MemberState::PlayerHousing(crate::travel_states::PlayerHousingState::default())
        }
        // No camp yet: one slot byte of 0.
        crate::travel_states::CAMPING_TYPE_INDEX => {
            MemberState::Camping(crate::travel_states::CampingState::default())
        }
        // No fishing step yet: the empty slot byte, as the generic first body.
        crate::fishing::FISHING_TYPE_INDEX => {
            MemberState::Fishing(crate::fishing::FishingComponentState::default())
        }
        // No performance (476 of 479 retail first groups, [`NEW_CHARACTER_FIRST_BODIES`]).
        MUSICAL_PERFORMANCE_TYPE_INDEX => {
            MemberState::PerformancePlayer(crate::music::PerformancePlayerState::NONE)
        }
        // No status effect yet: the empty slot byte; the sim puts its own state in its place.
        STATUS_EFFECTS_TYPE_INDEX => MemberState::StatusEffects(StatusEffectsState::default()),
        _ => MemberState::Empty(EmptyState { class }),
    }
}

/// What a player's card says of the player besides its prefab and position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerStates<'a> {
    /// The 3935 `characterId`: the login character of the client's own player.
    pub character_id: [u8; RAW_ID_LEN],
    /// The saved look, when known.
    pub look: Option<PlayerLook>,
    /// Current and max health, when known; with them go full stamina and mana.
    pub health: Option<(u32, u32)>,
    /// The gear states, when known.
    pub gear: Option<&'a PlayerGear>,
    /// The achievements, when known.
    pub achievements: Option<&'a AchievementState>,
    /// The objectives (quest tracker), when known.
    pub objectives: Option<&'a ObjectivesState>,
    /// The level and experience, when known.
    pub progression: Option<ProgressionState>,
    /// The weapon masteries and other categorical progressions, with the card and after a
    /// change.
    pub categorical_progression: Option<&'a CategoricalProgressionState>,
    /// The progression point pools, with the card and after a change.
    pub progression_points: Option<&'a ProgressionPointState>,
    /// The coins, with the card and after a change.
    pub currency: Option<CurrencyState>,
    /// The new health after it changed (a hit or a heal), when it did.
    pub health_change: Option<HealthChange>,
    /// The consumable cooldowns after a change, when there is one.
    pub cooldowns: Option<&'a CooldownsState>,
    /// The stamina children that changed, when some did.
    pub stamina: Option<StaminaState>,
    /// The combat status children that changed, when some did.
    pub combat_status: Option<CombatStatusState>,
}

/// A change of a player's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthChange {
    /// The new health.
    pub health: u32,
    /// It is the first damage from full health (retail `healthChangeFlags` 3).
    pub first_damage: bool,
    /// The player died or lives again: its new `vitalsData`.
    pub life: Option<VitalsData>,
    /// The new max health, when it changed (retail newchar 2026-09-18: `healthMax` and
    /// `maxHealth` after each level up and Constitution spend).
    pub max: Option<u32>,
}

impl HealthChange {
    /// The vitals member of the change.
    pub fn vitals(&self) -> VitalsState {
        let vitals = match self.life {
            Some(life) => VitalsState::player_life(self.health, life),
            None if self.first_damage => VitalsState::creature_first_damage(self.health),
            None => VitalsState::creature_health(self.health),
        };
        match self.max {
            Some(max) => vitals.with_health_max(max),
            None => vitals,
        }
    }
}

/// The members of every player component in [`PLAYER_EMPTY_COMPONENTS`] for `states`: a connected
/// 3935 state, the look, vitals and gear that `states` knows, and an empty state for the rest.
pub fn player_members(states: PlayerStates<'_>) -> impl Iterator<Item = BundleMember> {
    PLAYER_EMPTY_COMPONENTS
        .iter()
        .map(move |(component, class)| BundleMember {
            component: *component,
            state: player_state(states, *component, *class),
        })
}

/// The own player component of the vitals.
pub const PLAYER_VITALS_COMPONENT: u64 = 41;
/// The own player component of the stamina.
pub const PLAYER_STAMINA_COMPONENT: u64 = 42;

/// The members of the states that `states` holds besides the card's: the achievements, the
/// objectives, the progression, the categorical progression, the progression points, the gear,
/// the combat status, a health change and a stamina change. A bundle without the card sends
/// these changes.
pub fn change_members(states: PlayerStates<'_>) -> Vec<BundleMember> {
    let mut members: Vec<BundleMember> = player_members(states)
        .filter(|member| match member.state.type_index() {
            ACHIEVEMENT_TYPE_INDEX => states.achievements.is_some(),
            OBJECTIVES_TYPE_INDEX => states.objectives.is_some(),
            PROGRESSION_TYPE_INDEX => states.progression.is_some(),
            CATEGORICAL_PROGRESSION_TYPE_INDEX => states.categorical_progression.is_some(),
            PROGRESSION_POINT_TYPE_INDEX => states.progression_points.is_some(),
            CURRENCY_TYPE_INDEX => states.currency.is_some(),
            COOLDOWNS_TYPE_INDEX => states.cooldowns.is_some(),
            COMBAT_STATUS_TYPE_INDEX => states.combat_status.is_some(),
            PAPERDOLL_TYPE_INDEX | CONTAINER_TYPE_INDEX | ITEM_MANAGEMENT_TYPE_INDEX => {
                states.gear.is_some() && carries_gear_change(&member.state)
            }
            _ => false,
        })
        .collect();
    if let Some(change) = states.health_change {
        // A death opens the client's death screen through the 3935 respawn cooldown, which goes
        // with the dead vitals (death note).
        if change.life.is_some_and(|life| life.is_dead()) {
            members.push(BundleMember {
                component: PLAYER_COMPONENT_COMPONENT,
                state: MemberState::PlayerComponent(PlayerComponentState::died()),
            });
        }
        members.push(BundleMember {
            component: PLAYER_VITALS_COMPONENT,
            state: MemberState::Vitals(change.vitals()),
        });
    }
    if let Some(stamina) = states.stamina {
        members.push(BundleMember {
            component: PLAYER_STAMINA_COMPONENT,
            state: MemberState::Stamina(stamina),
        });
    }
    members
}

/// Whether the gear member `state` of a change carries a child: a change of the hands alone
/// ([`PlayerGear::hands_change`]) sends the paperdoll alone, as retail does.
fn carries_gear_change(state: &MemberState) -> bool {
    match state {
        MemberState::Paperdoll(paperdoll) => !paperdoll.is_empty(),
        MemberState::Container(container) => *container != ContainerState::default(),
        MemberState::ItemManagement(owned) => owned.owned.is_some(),
        _ => true,
    }
}

/// The members of the gear components of a player who has `gear`: a gear change sends them
/// again whole (see [`PlayerGear`]).
pub fn gear_members(gear: &PlayerGear) -> impl Iterator<Item = BundleMember> + '_ {
    let states = PlayerStates {
        character_id: [0; RAW_ID_LEN],
        look: None,
        health: None,
        gear: Some(gear),
        achievements: None,
        objectives: None,
        progression: None,
        categorical_progression: None,
        progression_points: None,
        currency: None,
        health_change: None,
        cooldowns: None,
        stamina: None,
        combat_status: None,
    };
    player_members(states).filter(|member| {
        matches!(
            member.state.type_index(),
            PAPERDOLL_TYPE_INDEX | CONTAINER_TYPE_INDEX | ITEM_MANAGEMENT_TYPE_INDEX
        )
    })
}

/// The state of player component `component` of class `class` for `states`.
fn player_state(states: PlayerStates<'_>, component: u64, class: u64) -> MemberState {
    if class == ACHIEVEMENT_TYPE_INDEX
        && let Some(achievements) = states.achievements
    {
        return MemberState::Achievement(achievements.clone());
    }
    if class == OBJECTIVES_TYPE_INDEX
        && let Some(objectives) = states.objectives
    {
        return MemberState::Objectives(objectives.clone());
    }
    if class == PROGRESSION_TYPE_INDEX
        && let Some(progression) = states.progression
    {
        return MemberState::Progression(progression);
    }
    if class == CATEGORICAL_PROGRESSION_TYPE_INDEX
        && let Some(categorical) = states.categorical_progression
    {
        return MemberState::CategoricalProgression(categorical.clone());
    }
    if class == PROGRESSION_POINT_TYPE_INDEX
        && let Some(points) = states.progression_points
    {
        return MemberState::ProgressionPoints(points.clone());
    }
    if class == CURRENCY_TYPE_INDEX
        && let Some(currency) = states.currency
    {
        return MemberState::Currency(currency);
    }
    if class == COOLDOWNS_TYPE_INDEX
        && let Some(cooldowns) = states.cooldowns
    {
        return MemberState::Cooldowns(cooldowns.clone());
    }
    if class == COMBAT_STATUS_TYPE_INDEX
        && let Some(status) = states.combat_status
    {
        return MemberState::CombatStatus(status);
    }
    if let Some(gear) = states.gear {
        match class {
            PAPERDOLL_TYPE_INDEX => {
                return MemberState::Paperdoll(Box::new(gear.paperdoll.clone()));
            }
            CONTAINER_TYPE_INDEX if component == BAG_COMPONENT => {
                return MemberState::Container(gear.bag.clone());
            }
            CONTAINER_TYPE_INDEX => {
                return MemberState::Container(gear.second_container.clone());
            }
            ITEM_MANAGEMENT_TYPE_INDEX => {
                return MemberState::ItemManagement(gear.owned.clone());
            }
            _ => {}
        }
    }
    match (class, states.look, states.health) {
        (PLAYER_COMPONENT_TYPE_INDEX, _, _) => {
            MemberState::PlayerComponent(PlayerComponentState::connected(states.character_id))
        }
        (PLAYER_APPEARANCE_TYPE_INDEX, Some(look), _) => {
            MemberState::Appearance(PlayerAppearanceState::of(look))
        }
        (VITALS_TYPE_INDEX, _, Some((health, health_max))) => {
            MemberState::Vitals(VitalsState::player(health, health_max))
        }
        (STAMINA_TYPE_INDEX, _, Some(_)) => MemberState::Stamina(StaminaState::player()),
        (DAMAGE_RECEIVER_TYPE_INDEX, _, _) => {
            MemberState::DamageReceiver(DamageReceiverState::player())
        }
        (MANA_TYPE_INDEX, _, Some(_)) => MemberState::Mana(ManaState::player()),
        _ => empty_state(class),
    }
}

fn has_own_reader(class: u64) -> bool {
    matches!(
        class,
        VITALS_TYPE_INDEX
            | STAMINA_TYPE_INDEX
            | COMBAT_STATUS_TYPE_INDEX
            | PLAYER_COMPONENT_TYPE_INDEX
            | PLAYER_APPEARANCE_TYPE_INDEX
            | MANA_TYPE_INDEX
            | PAPERDOLL_TYPE_INDEX
            | CONTAINER_TYPE_INDEX
            | ITEM_MANAGEMENT_TYPE_INDEX
            | ACHIEVEMENT_TYPE_INDEX
            | DAMAGE_RECEIVER_TYPE_INDEX
            | OBJECTIVES_TYPE_INDEX
            | PROGRESSION_TYPE_INDEX
            | CATEGORICAL_PROGRESSION_TYPE_INDEX
            | PROGRESSION_POINT_TYPE_INDEX
            | COOLDOWNS_TYPE_INDEX
            | GATHERING_TYPE_INDEX
            | INTERACTOR_TYPE_INDEX
            | CURRENCY_TYPE_INDEX
            | LOOT_TRACKER_TYPE_INDEX
            | GLOBAL_STORAGE_TYPE_INDEX
            | ATTRIBUTE_TYPE_INDEX
            | ABILITY_TYPE_INDEX
            | MOUNT_TYPE_INDEX
            | GROUPS_STATE_TYPE_INDEX
            | SOCIAL_TYPE_INDEX
            | GUILDS_STATE_TYPE_INDEX
            | CHAT_STATE_TYPE_INDEX
            | crate::fishing::FISHING_TYPE_INDEX
            | MUSICAL_PERFORMANCE_TYPE_INDEX
            | STATUS_EFFECTS_TYPE_INDEX
            | crate::travel_states::PLAYER_TIME_TYPE_INDEX
            | crate::travel_states::PLAYER_HOUSING_TYPE_INDEX
            | crate::travel_states::CAMPING_TYPE_INDEX
            | OBJECTIVE_INTERACTOR_TYPE_INDEX
            | TEMPORARY_AFFILIATION_TYPE_INDEX
            | GAME_EVENT_TYPE_INDEX
    )
}

/// Puts `member` into the own group `members`: in place of the member of its component (an
/// empty state of the card), else before the last member (the position, which ends an own
/// group, as [`change_members`] go).
pub fn put_member(members: &mut Vec<BundleMember>, member: BundleMember) {
    match members
        .iter_mut()
        .find(|known| known.component == member.component)
    {
        Some(known) => *known = member,
        None => members.insert(members.len().saturating_sub(1), member),
    }
}

/// Puts the change of `lookingThroughLoadout` to `looking` into the own group `members`: into
/// the 3935 state the group carries already (the card's, a death's), else as its own member
/// before the position ([`put_member`]; retail sends it before the own ALC).
pub fn put_looking_through_loadout(members: &mut Vec<BundleMember>, looking: bool) {
    put_player_change(
        members,
        PlayerComponentState::looking_through_loadout(looking),
        |state| state.looking_through_loadout = Some(looking),
    );
}

/// Puts the change of `accountInProbation` to `in_probation` into the own group `members`, as
/// [`put_looking_through_loadout`] puts its change.
pub fn put_account_in_probation(members: &mut Vec<BundleMember>, in_probation: bool) {
    put_player_change(
        members,
        PlayerComponentState::account_in_probation(in_probation),
        |state| state.account_in_probation = Some(in_probation),
    );
}

/// Puts the change of `isInStore` to `in_store` into the own group `members`, as
/// [`put_looking_through_loadout`] puts its change.
pub fn put_is_in_store(members: &mut Vec<BundleMember>, in_store: bool) {
    put_player_change(members, PlayerComponentState::in_store(in_store), |state| {
        state.is_in_store = Some(in_store);
    });
}

/// Applies `apply` to the 3935 state that `members` carries already, else puts `change` as its
/// own member ([`put_member`]).
fn put_player_change(
    members: &mut Vec<BundleMember>,
    change: PlayerComponentState,
    apply: impl FnOnce(&mut PlayerComponentState),
) {
    let known = members
        .iter_mut()
        .filter(|known| known.component == PLAYER_COMPONENT_COMPONENT)
        .find_map(|known| match &mut known.state {
            MemberState::PlayerComponent(state) => Some(state),
            _ => None,
        });
    match known {
        Some(state) => apply(state),
        None => put_member(
            members,
            BundleMember {
                component: PLAYER_COMPONENT_COMPONENT,
                state: MemberState::PlayerComponent(change),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{BundleGroup, BundlePayload};

    const LOCAL_SLOT: u64 = 1;
    /// A member: a 1-byte component index, a class id of at most 2 bytes, a 1-byte body.
    const MAX_EMPTY_MEMBER_BYTES: usize = 4;

    const CHARACTER_ID: [u8; RAW_ID_LEN] = [5; RAW_ID_LEN];
    const HEALTH: (u32, u32) = (800, 856);
    /// The classes that carry a body when the look and the health are known.
    const KNOWN_CLASSES: [u64; 5] = [
        PLAYER_COMPONENT_TYPE_INDEX,
        PLAYER_APPEARANCE_TYPE_INDEX,
        VITALS_TYPE_INDEX,
        STAMINA_TYPE_INDEX,
        MANA_TYPE_INDEX,
    ];

    fn reads_back(members: &[BundleMember]) {
        let payload = BundlePayload {
            groups: vec![BundleGroup {
                interest: LOCAL_SLOT,
                members: members.to_vec(),
            }],
        };
        let read = BundlePayload::decode(&payload.encode().expect("payload")).expect("read");
        assert_eq!(read.groups[0].members, members);
    }

    /// The first bodies of the new character of newchar-retail-2026-09-18 (open-world card,
    /// 451700 ms), whose list versions are `8c bf` (12236).
    const NEW_CHARACTER_RETAIL: [(u64, &str); 8] = [
        (
            POINTS_ACCUMULATOR_TYPE_INDEX,
            "010700000000000000000000000000000000",
        ),
        (MAGIC_TYPE_INDEX, "01030000000000000000"),
        (
            MUSICAL_PERFORMANCE_TYPE_INDEX,
            "030100000000ffffffff060000000000ffffffff",
        ),
        (PLAYER_HOME_TYPE_INDEX, "010100018cbf00"),
        (
            INSTANCED_SLAYER_SCRIPT_TYPE_INDEX,
            "010c00018cbf0000018cbf00",
        ),
        (REWARD_TRACK_TYPE_INDEX, "040100018cbf00"),
        (GAME_EVENT_TYPE_INDEX, "010200018cbf00"),
        (
            ITEM_SKINNING_TYPE_INDEX,
            "020300018cbf01000000000000000000018cbf00",
        ),
    ];

    #[test]
    fn a_new_characters_company_and_chat_states_are_retails_with_our_list_version() {
        const RETAIL_VERSION: &str = "8cbf";
        const OUR_VERSION: &str = "03";
        for (class, retail) in [
            (GUILDS_STATE_TYPE_INDEX, "0280040000000000000000"),
            (CHAT_STATE_TYPE_INDEX, "010100018cbf00"),
        ] {
            let member = BundleMember {
                component: 0,
                state: empty_state(class),
            };
            let bytes = member.encode().expect("member");
            let ours: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            assert!(
                ours.ends_with(&retail.replace(RETAIL_VERSION, OUR_VERSION)),
                "class {class}: {ours}"
            );
        }
    }

    #[test]
    fn a_new_characters_first_bodies_are_retails_with_our_list_version() {
        /// Retail's list version and ours, as wide prefix integers.
        const RETAIL_VERSION: &str = "8cbf";
        const OUR_VERSION: &str = "03";
        for (class, retail) in NEW_CHARACTER_RETAIL {
            let ours: String = first_body(class)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(
                ours,
                retail.replace(RETAIL_VERSION, OUR_VERSION),
                "class {class}"
            );
            let mut reader = Reader::new(first_body(class));
            assert_eq!(
                EmptyState::read(class, &mut reader),
                Ok(EmptyState { class }),
                "class {class} reads back"
            );
        }
    }

    #[test]
    fn without_look_and_health_every_other_component_is_empty_or_its_retail_first_body() {
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: None,
            gear: None,
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let members: Vec<BundleMember> = player_members(states).collect();
        assert_eq!(members.len(), PLAYER_EMPTY_COMPONENTS.len());
        for member in &members {
            if member.state.type_index() == PLAYER_COMPONENT_TYPE_INDEX {
                assert_eq!(
                    member.state,
                    MemberState::PlayerComponent(PlayerComponentState::connected(CHARACTER_ID))
                );
                continue;
            }
            let bytes = member.encode().expect("member");
            let class = member.state.type_index();
            if class == DAMAGE_RECEIVER_TYPE_INDEX {
                assert_eq!(
                    member.state,
                    MemberState::DamageReceiver(DamageReceiverState::player())
                );
                continue;
            }
            if class == MOUNT_TYPE_INDEX {
                assert_eq!(member.state, MemberState::Mount(MountState::login_card()));
                continue;
            }
            if matches!(class, GUILDS_STATE_TYPE_INDEX | CHAT_STATE_TYPE_INDEX) {
                assert_eq!(member.state, empty_state(class));
                continue;
            }
            assert!(bytes.ends_with(first_body(class)), "{member:?}");
            if first_body(class) == [NO_SLOT] {
                assert!(bytes.len() <= MAX_EMPTY_MEMBER_BYTES, "{member:?}");
            }
        }
        // Retail: the faction is `064100000a0000`, the name tag `010100000001`.
        let body_of = |class: u64| {
            members
                .iter()
                .find(|member| member.state.type_index() == class)
                .map(|member| member.encode().expect("member"))
        };
        assert!(
            body_of(FACTION_TYPE_INDEX)
                .is_some_and(|bytes| bytes.ends_with(&[0x06, 0x41, 0x00, 0x00, 0x0a, 0x00, 0x00]))
        );
        assert!(
            body_of(PLAYER_NAME_TAG_TYPE_INDEX)
                .is_some_and(|bytes| bytes.ends_with(&[0x01, 0x01, 0x00, 0x00, 0x00, 0x01]))
        );
        reads_back(&members);
    }

    #[test]
    fn a_known_look_and_health_fill_the_appearance_and_vitals() {
        let look = PlayerLook::default();
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: Some(look),
            health: Some(HEALTH),
            gear: None,
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let members: Vec<BundleMember> = player_members(states).collect();
        for member in &members {
            let class = member.state.type_index();
            let expected = match class {
                PLAYER_COMPONENT_TYPE_INDEX => {
                    MemberState::PlayerComponent(PlayerComponentState::connected(CHARACTER_ID))
                }
                PLAYER_APPEARANCE_TYPE_INDEX => {
                    MemberState::Appearance(PlayerAppearanceState::of(look))
                }
                VITALS_TYPE_INDEX => MemberState::Vitals(VitalsState::player(HEALTH.0, HEALTH.1)),
                STAMINA_TYPE_INDEX => MemberState::Stamina(StaminaState::player()),
                MANA_TYPE_INDEX => MemberState::Mana(ManaState::player()),
                DAMAGE_RECEIVER_TYPE_INDEX => {
                    MemberState::DamageReceiver(DamageReceiverState::player())
                }
                _ => empty_state(class),
            };
            assert_eq!(member.state, expected);
        }
        for class in KNOWN_CLASSES {
            assert_eq!(
                members
                    .iter()
                    .filter(|member| member.state.type_index() == class)
                    .count(),
                1,
                "class {class} is one player component"
            );
        }
        reads_back(&members);
    }

    #[test]
    fn the_death_cooldown_is_the_retail_slot_1_child_12() {
        // Retail: `09 9f3d 02 80 20 00000000` (death note: all three captured deaths).
        const RETAIL_BODY: [u8; 7] = [0x02, 0x80, 0x20, 0, 0, 0, 0];
        let state = PlayerComponentState::read(&mut Reader::new(&RETAIL_BODY)).expect("read");
        assert_eq!(state, PlayerComponentState::died());
        let mut writer = Writer::new();
        state.write(&mut writer);
        assert_eq!(writer.finish(), RETAIL_BODY);
    }

    #[test]
    fn a_death_sends_the_respawn_cooldown_with_the_dead_vitals() {
        const SPAWNED_MS: u64 = 5_000;
        let death = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: Some(HEALTH),
            gear: None,
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: Some(HealthChange {
                health: 0,
                first_damage: false,
                life: Some(VitalsData::player_dead(SPAWNED_MS)),
                max: None,
            }),
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let members = change_members(death);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].component, PLAYER_COMPONENT_COMPONENT);
        assert_eq!(
            members[0].state,
            MemberState::PlayerComponent(PlayerComponentState::died())
        );
        assert_eq!(members[1].component, PLAYER_VITALS_COMPONENT);
        let alive = PlayerStates {
            health_change: Some(HealthChange {
                health: HEALTH.1,
                first_damage: false,
                life: Some(VitalsData::player_alive(SPAWNED_MS)),
                max: None,
            }),
            ..death
        };
        assert_eq!(
            change_members(alive).len(),
            1,
            "a respawn sends the vitals only"
        );
    }

    #[test]
    fn the_changes_are_the_states_given_and_a_health_change() {
        const HEALED: u32 = 865;
        let objectives = ObjectivesState::default();
        // Retail after a land dodge: `01 09 425c0000 3f800000` (stamina-wire note, section 1).
        const RETAIL_SPEND: [u8; 10] = [0x01, 0x09, 0x42, 0x5c, 0, 0, 0x3f, 0x80, 0, 0];
        let spend = StaminaState {
            cur: Some(55.0),
            regen: Some(1.0),
            ..StaminaState::default()
        };
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: Some(HEALTH),
            gear: None,
            achievements: None,
            objectives: Some(&objectives),
            progression: Some(ProgressionState::experience(20)),
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: Some(HealthChange {
                health: HEALED,
                first_damage: false,
                life: None,
                max: None,
            }),
            cooldowns: None,
            stamina: Some(spend),
            combat_status: None,
        };
        let members = change_members(states);
        let classes: Vec<u64> = members
            .iter()
            .map(|member| member.state.type_index())
            .collect();
        assert_eq!(
            classes,
            [
                OBJECTIVES_TYPE_INDEX,
                PROGRESSION_TYPE_INDEX,
                VITALS_TYPE_INDEX,
                STAMINA_TYPE_INDEX
            ]
        );
        assert_eq!(members[3].component, PLAYER_STAMINA_COMPONENT);
        let mut writer = crate::wire::Writer::new();
        spend.write(&mut writer);
        assert_eq!(writer.finish(), RETAIL_SPEND, "retail bytes");
        assert_eq!(members[3].state, MemberState::Stamina(spend));
        assert_eq!(members[2].component, PLAYER_VITALS_COMPONENT);
        assert_eq!(
            members[2].state,
            MemberState::Vitals(VitalsState::creature_health(HEALED))
        );
    }

    #[test]
    fn the_masteries_and_point_pools_go_out_on_components_65_and_67() {
        let masteries = CategoricalProgressionState::full(
            crate::items::FIRST_VERSION,
            &[crate::categorical::MasteryEntry {
                progression_id: nw_formats::objectstream::lowercase_crc32("GreatSwordAbilityTable"),
                rank: 1,
                points: 10,
            }],
        );
        let pools = ProgressionPointState::full(crate::items::FIRST_VERSION, &[], &[]);
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: None,
            gear: None,
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: Some(&masteries),
            progression_points: Some(&pools),
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let members = change_members(states);
        assert_eq!(
            members,
            [
                BundleMember {
                    component: CATEGORICAL_PROGRESSION_COMPONENT,
                    state: MemberState::CategoricalProgression(masteries),
                },
                BundleMember {
                    component: PROGRESSION_POINT_COMPONENT,
                    state: MemberState::ProgressionPoints(pools),
                },
            ]
        );
        reads_back(&members);
    }

    #[test]
    fn a_combat_status_change_goes_out_on_component_28() {
        // Retail 78148 of the tutorial capture.
        const RETAIL: [u8; 19] = [
            0x01, 0x0d, 0x01, 0x0b, 0xbc, 0xa3, 0x48, 0x1f, 0xb3, 0xeb, 0x61, 0x0b, 0xbc, 0xa3,
            0x45, 0xcb, 0xa8, 0x07, 0x61,
        ];
        let status = CombatStatusState {
            in_combat: Some(true),
            in_pvp_combat: None,
            logged_out_time: Some(0x0bbc_a348_1fb3_eb61),
            concluded_time: Some(0x0bbc_a345_cba8_0761),
        };
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: Some(HEALTH),
            gear: None,
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: Some(status),
        };
        let members = change_members(states);
        assert_eq!(
            members,
            [BundleMember {
                component: 28,
                state: MemberState::CombatStatus(status),
            }]
        );
        let mut writer = crate::wire::Writer::new();
        status.write(&mut writer);
        assert_eq!(writer.finish(), RETAIL, "retail bytes");
    }

    #[test]
    fn gear_members_are_the_four_gear_components() {
        let gear = PlayerGear::first(&[]);
        let classes: Vec<u64> = gear_members(&gear)
            .map(|member| member.state.type_index())
            .collect();
        assert_eq!(classes.len(), 4);
        assert!(classes.contains(&PAPERDOLL_TYPE_INDEX));
        assert!(classes.contains(&ITEM_MANAGEMENT_TYPE_INDEX));
    }

    #[test]
    fn known_gear_fills_the_paperdoll_the_two_containers_and_the_owned_items() {
        const SWORD_INDEX: u32 = 1228;
        const SWORD_DURABILITY: u32 = 1000;
        const SWORD_GEAR_SCORE: u32 = 60;
        let gear = PlayerGear::first(&[crate::gear::EquippedItem {
            slot: crate::paperdoll::slot::MAIN_HAND_OPTION_1,
            key: 0,
            item: Some(crate::gear::GearItem {
                item_index: SWORD_INDEX,
                quantity: 1,
                gear_score: SWORD_GEAR_SCORE,
                durability: SWORD_DURABILITY,
                max_durability: SWORD_DURABILITY,
                perks: [0; crate::items::PERK_SLOTS],
                look: crate::gear::ItemLook::default(),
                counter: 0,
                bound: true,
                locked: false,
                instance: CHARACTER_ID,
            }),
        }]);
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: None,
            gear: Some(&gear),
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let members: Vec<BundleMember> = player_members(states).collect();
        let state_of = |component: u64| {
            members
                .iter()
                .find(|member| member.component == component)
                .map(|member| member.state.clone())
        };
        const PAPERDOLL_COMPONENT: u64 = 62;
        const SECOND_CONTAINER_COMPONENT: u64 = 76;
        const ITEM_MANAGEMENT_COMPONENT: u64 = 2;
        assert_eq!(
            state_of(PAPERDOLL_COMPONENT),
            Some(MemberState::Paperdoll(Box::new(gear.paperdoll.clone())))
        );
        assert_eq!(
            state_of(BAG_COMPONENT),
            Some(MemberState::Container(gear.bag.clone()))
        );
        assert_eq!(
            state_of(SECOND_CONTAINER_COMPONENT),
            Some(MemberState::Container(gear.second_container.clone()))
        );
        assert_eq!(
            state_of(ITEM_MANAGEMENT_COMPONENT),
            Some(MemberState::ItemManagement(gear.owned.clone()))
        );
        reads_back(&members);
    }

    #[test]
    fn a_set_slot_of_an_empty_class_stops_the_walk() {
        // A class we still have no fixed first body for: a set slot stops the bundle walk.
        // Classes with a known first body ([`RETAIL_FIRST_BODIES`]/[`NEW_CHARACTER_FIRST_BODIES`])
        // read that body instead, so they are not what this test checks.
        let class = PLAYER_EMPTY_COMPONENTS
            .iter()
            .map(|(_, player_class)| *player_class)
            .find(|player_class| {
                EmptyState::covers(*player_class) && first_body(*player_class) == [NO_SLOT]
            })
            .expect("a covered player-empty class with no fixed first body");
        assert!(EmptyState::covers(class));
        assert!(
            !EmptyState::covers(VITALS_TYPE_INDEX),
            "vitals has its own reader"
        );
        assert!(
            !EmptyState::covers(PLAYER_COMPONENT_TYPE_INDEX),
            "player component has its own reader"
        );
        let mut reader = Reader::new(&[1]);
        assert!(matches!(
            EmptyState::read(class, &mut reader),
            Err(MessageError::Unsized { .. })
        ));
    }

    #[test]
    fn a_connected_player_component_round_trips() {
        const ID: [u8; RAW_ID_LEN] = [9; RAW_ID_LEN];
        let state = PlayerComponentState::connected(ID);
        let mut writer = Writer::new();
        state.write(&mut writer);
        let bytes = writer.finish();
        assert_eq!(bytes.first(), Some(&SLOT_2_BIT));
        let mut reader = Reader::new(&bytes);
        let read = PlayerComponentState::read(&mut reader).expect("read");
        reader.finish().expect("to the end");
        assert_eq!(read, state);
        assert_eq!(read.player_connected, Some(true));
        assert_eq!(read.character_id, Some(ID));
    }

    #[test]
    fn the_probation_change_is_retail_s_bytes_and_merges_with_a_death() {
        // Retail, all three captured sessions: `02 08 00` (slot 1, child 3, false).
        const RETAIL_NOT_IN_PROBATION: [u8; 3] = [0x02, 0x08, 0x00];
        const IN_PROBATION: [u8; 3] = [0x02, 0x08, 0x01];
        for (in_probation, retail) in [(false, RETAIL_NOT_IN_PROBATION), (true, IN_PROBATION)] {
            let state = PlayerComponentState::account_in_probation(in_probation);
            let mut writer = Writer::new();
            state.write(&mut writer);
            let bytes = writer.finish();
            assert_eq!(bytes, retail);
            let mut reader = Reader::new(&bytes);
            assert_eq!(
                PlayerComponentState::read(&mut reader).expect("read"),
                state
            );
            reader.finish().expect("to the end");
        }
        let mut members = vec![BundleMember {
            component: PLAYER_COMPONENT_COMPONENT,
            state: MemberState::PlayerComponent(PlayerComponentState::died()),
        }];
        put_account_in_probation(&mut members, false);
        let MemberState::PlayerComponent(state) = &members[0].state else {
            panic!("player component");
        };
        let mut writer = Writer::new();
        state.write(&mut writer);
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        let read = PlayerComponentState::read(&mut reader).expect("read");
        reader.finish().expect("to the end");
        assert_eq!(read.account_in_probation, Some(false));
        assert_eq!(
            read.on_death_respawn_cooldown,
            Some(OPEN_WORLD_RESPAWN_COOLDOWN)
        );
    }

    #[test]
    fn a_slot_1_child_above_bit_15_is_refused_not_a_panic() {
        // Slot 1, child bytes for children 0 to 20 empty, then child 21 (the last of 22).
        const CHILD_21: [u8; 5] = [0x02, 0x80, 0x80, 0x80, 0x01];
        let mut reader = Reader::new(&CHILD_21);
        assert!(PlayerComponentState::read(&mut reader).is_err());
    }

    #[test]
    fn a_connected_body_carries_the_character_id_as_uuid_text() {
        const ID: [u8; RAW_ID_LEN] = [7; RAW_ID_LEN];
        let mut writer = Writer::new();
        PlayerComponentState::connected(ID).write(&mut writer);
        let mut expected = vec![
            SLOT_2_BIT,
            0x09,
            PRESENCE_FLAG_FIXED | PRESENCE_FLAG_UUID_TEXT,
        ];
        expected.extend_from_slice(&ID);
        expected.push(1);
        assert_eq!(writer.finish(), expected, "facet +0xe8 note, section 6");
    }

    #[test]
    fn slot_0_or_another_slot_1_child_stops_the_walk() {
        // Slot 0; slot 1 child 0, whose width is Unknown.
        for body in [&[0b001][..], &[0b010, 0b001, 0]] {
            assert!(matches!(
                PlayerComponentState::read(&mut Reader::new(body)),
                Err(MessageError::Unsized { .. })
            ));
        }
    }

    #[test]
    fn the_inventory_open_and_close_are_the_retail_slot_2_child_4() {
        // Retail newchar 2026-09-18 at 1792695864643 and 1792695870142.
        for (looking, retail) in [(true, [0x04, 0x10, 0x01]), (false, [0x04, 0x10, 0x00])] {
            let state = PlayerComponentState::looking_through_loadout(looking);
            let mut writer = Writer::new();
            state.write(&mut writer);
            assert_eq!(writer.finish(), retail);
            let mut reader = Reader::new(&retail);
            assert_eq!(
                PlayerComponentState::read(&mut reader).expect("read"),
                state
            );
            reader.finish().expect("to the end");
        }
    }

    #[test]
    fn the_inventory_change_joins_the_groups_3935_state_or_goes_before_the_position() {
        const POSITION_COMPONENT: u64 = 16;
        let position = BundleMember {
            component: POSITION_COMPONENT,
            state: MemberState::PlayerComponent(PlayerComponentState::default()),
        };
        let mut members = vec![position.clone()];
        put_looking_through_loadout(&mut members, true);
        assert_eq!(
            members,
            [
                BundleMember {
                    component: PLAYER_COMPONENT_COMPONENT,
                    state: MemberState::PlayerComponent(
                        PlayerComponentState::looking_through_loadout(true)
                    ),
                },
                position.clone(),
            ],
            "retail: component 9 before the own ALC"
        );
        let mut card = vec![
            BundleMember {
                component: PLAYER_COMPONENT_COMPONENT,
                state: MemberState::PlayerComponent(PlayerComponentState::connected(CHARACTER_ID)),
            },
            position,
        ];
        put_looking_through_loadout(&mut card, false);
        assert_eq!(
            card[0].state,
            MemberState::PlayerComponent(PlayerComponentState {
                looking_through_loadout: Some(false),
                ..PlayerComponentState::connected(CHARACTER_ID)
            }),
            "the card keeps its character id"
        );
        assert_eq!(card.len(), 2);
    }

    /// Retail `unlock_store_content`: the 3935 bodies after the client's 4908 with 1 and with 0.
    #[test]
    fn the_store_flag_has_the_retail_bytes() {
        for (in_store, body) in [(true, [0x04, 0x40, 0x01]), (false, [0x04, 0x40, 0x00])] {
            let mut writer = Writer::new();
            PlayerComponentState::in_store(in_store).write(&mut writer);
            assert_eq!(writer.finish(), body);
            let read = PlayerComponentState::read(&mut Reader::new(&body)).expect("read");
            assert_eq!(read.is_in_store, Some(in_store));
        }
        let mut members = Vec::new();
        put_is_in_store(&mut members, true);
        put_looking_through_loadout(&mut members, true);
        assert_eq!(
            members[0].state,
            MemberState::PlayerComponent(PlayerComponentState {
                looking_through_loadout: Some(true),
                ..PlayerComponentState::in_store(true)
            }),
            "one 3935 member carries both changes"
        );
    }

    #[test]
    fn a_change_of_the_hands_alone_sends_the_paperdoll_alone() {
        let before = crate::gear::Hands::default();
        let now = crate::gear::Hands {
            drawn: Some(crate::paperdoll::slot::MAIN_HAND_OPTION_1),
            attached: true,
            ..before
        };
        let gear = PlayerGear::hands_change(before, now);
        let states = PlayerStates {
            character_id: CHARACTER_ID,
            look: None,
            health: None,
            gear: Some(&gear),
            achievements: None,
            objectives: None,
            progression: None,
            categorical_progression: None,
            progression_points: None,
            currency: None,
            health_change: None,
            cooldowns: None,
            stamina: None,
            combat_status: None,
        };
        let classes: Vec<u64> = change_members(states)
            .iter()
            .map(|member| member.state.type_index())
            .collect();
        assert_eq!(classes, [PAPERDOLL_TYPE_INDEX]);
        let unchanged = PlayerGear::hands_change(now, now);
        let states = PlayerStates {
            gear: Some(&unchanged),
            ..states
        };
        assert!(change_members(states).is_empty(), "nothing to send");
    }
}
