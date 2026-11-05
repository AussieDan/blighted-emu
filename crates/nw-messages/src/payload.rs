// SPDX-License-Identifier: AGPL-3.0-or-later
//! The members inside a `ReplicatedStateBundle` payload (replication note section 1). Walk
//! `FUN_14175ccc0`, group `FUN_146af20d0`, member `FUN_146af2340`.
//!
//! A payload is groups until its end. A group is a prefix-coded integer, a `u8` member count, and
//! the members. A member is a prefix-coded integer, a type id (0 adds 16 raw bytes), and the body
//! of its class. A body has no length, so a member of a class without a reader here ends the walk.

use std::ops::Range;

use nw_reflect::{TypeId, decode_type_id, encode_type_id};

use crate::abilities::{ABILITY_TYPE_INDEX, AbilityState};
use crate::achievements::{ACHIEVEMENT_TYPE_INDEX, AchievementState};
use crate::alc::{ALC_TYPE_INDEX, AlcState};
use crate::appearance::{PLAYER_APPEARANCE_TYPE_INDEX, PlayerAppearanceState};
use crate::attributes::{ATTRIBUTE_TYPE_INDEX, AttributeState};
use crate::buildables::{
    BUILDABLE_CONTROLLER_TYPE_INDEX, BUILDABLE_GRID_TYPE_INDEX, BuildableControllerState,
    BuildableGridState, OWNERSHIP_TYPE_INDEX, OwnershipState, PLACEMENT_OBSTRUCTION_TYPE_INDEX,
    PlacementObstructionState, TERRITORY_INTERFACE_TYPE_INDEX, TerritoryInterfaceState,
};
use crate::categorical::{CATEGORICAL_PROGRESSION_TYPE_INDEX, CategoricalProgressionState};
use crate::components::{
    COMBAT_STATUS_TYPE_INDEX, CURRENCY_TYPE_INDEX, CombatStatusState, CurrencyState,
    DAMAGE_RECEIVER_TYPE_INDEX, DELAYED_EVENT_TYPE_INDEX, DamageReceiverState, DelayedEventState,
    GATHERABLE_CONTROLLER_TYPE_INDEX, GatherableControllerState, INTERACT_TYPE_INDEX,
    InteractState, LOOK_TARGETING_TYPE_INDEX, LookTargetingState, MANA_TYPE_INDEX, ManaState,
    STAMINA_TYPE_INDEX, StaminaState,
};
use crate::containers::{
    CONTAINER_TYPE_INDEX, ContainerState, ITEM_MANAGEMENT_TYPE_INDEX, ItemManagementState,
};
use crate::cooldowns::{COOLDOWNS_TYPE_INDEX, CooldownsState};
use crate::fishing::{FISHING_TYPE_INDEX, FishingComponentState};
use crate::game_events::{GAME_EVENT_TYPE_INDEX, GameEventState};
use crate::gathering::{GATHERING_TYPE_INDEX, GatheringState};
use crate::gde::{GDE_METADATA_TYPE_INDEX, GdeMetadataReplicatedState};
use crate::generic_invite::{GENERIC_INVITE_STATE_TYPE_INDEX, GenericInviteState};
use crate::interactor::{INTERACTOR_TYPE_INDEX, InteractorState};
use crate::loot::{
    GLOBAL_STORAGE_TYPE_INDEX, GlobalStorageState, LOOT_TRACKER_TYPE_INDEX, LootTrackerState,
};
use crate::member_layouts;
use crate::mount::{MOUNT_TYPE_INDEX, MountState};
use crate::music::{
    MusicalPerformanceState, PERFORMANCE_PLAYER_TYPE_INDEX, PERFORMANCE_TYPE_INDEX,
    PerformancePlayerState,
};
use crate::objective_interactor::{OBJECTIVE_INTERACTOR_TYPE_INDEX, ObjectiveInteractorState};
use crate::objectives::{OBJECTIVES_TYPE_INDEX, ObjectivesState};
use crate::p2p_trade::{P2P_TRADE_STATE_TYPE_INDEX, P2PTradeState};
use crate::paperdoll::{PAPERDOLL_TYPE_INDEX, PaperdollState};
use crate::player_components::{EmptyState, PLAYER_COMPONENT_TYPE_INDEX, PlayerComponentState};
use crate::position::{POSITION_IN_THE_WORLD_TYPE_INDEX, PositionInTheWorldReplicatedState};
use crate::progression::{PROGRESSION_TYPE_INDEX, ProgressionState};
use crate::progression_points::{PROGRESSION_POINT_TYPE_INDEX, ProgressionPointState};
use crate::social::group_data::{GROUP_DATA_TYPE_INDEX, GroupDataState};
use crate::social::states::{
    CHAT_STATE_TYPE_INDEX, ChatState, GROUPS_STATE_TYPE_INDEX, GUILDS_STATE_TYPE_INDEX,
    GroupsState, GuildsState, SocialState,
};
use crate::status_effects::{STATUS_EFFECTS_TYPE_INDEX, StatusEffectsState};
use crate::temporary_affiliation::{TEMPORARY_AFFILIATION_TYPE_INDEX, TemporaryAffiliationState};
use crate::title_choice::SOCIAL_TYPE_INDEX;
use crate::vitals::{VITALS_TYPE_INDEX, VitalsState};
use crate::wire::{MessageError, Reader, Writer};

/// The state of one member, by class.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberState {
    /// `MB::PositionInTheWorldReplicatedState`.
    Position(PositionInTheWorldReplicatedState),
    /// `MB::GdeMetadataReplicatedState`: the spawn card.
    GdeMetadata(GdeMetadataReplicatedState),
    /// `MB::VitalsComponentReplicatedState`.
    Vitals(VitalsState),
    /// `MB::ALCReplicatedState`: kept as bytes, with its position.
    Alc(AlcState),
    /// `MB::StaminaComponentReplicatedState`.
    Stamina(StaminaState),
    /// `MB::CombatStatusComponentReplicatedState`.
    CombatStatus(CombatStatusState),
    /// `MB::DelayedEventComponentReplicatedState`.
    DelayedEvent(DelayedEventState),
    /// `MB::LookTargetingComponentReplicatedState`.
    LookTargeting(LookTargetingState),
    /// `MB::GatheringComponentReplicatedState`.
    Gathering(GatheringState),
    /// `MB::InteractorComponentReplicatedState`.
    Interactor(InteractorState),
    /// `MB::PlayerComponentReplicatedState`.
    PlayerComponent(PlayerComponentState),
    /// `MB::PlayerAppearanceComponentReplicatedState`.
    Appearance(PlayerAppearanceState),
    /// `MB::ManaComponentReplicatedState`.
    Mana(ManaState),
    /// `MB::PaperdollComponentReplicatedState`, boxed: it is far larger than the other states.
    Paperdoll(Box<PaperdollState>),
    /// `MB::ContainerComponentReplicatedState`.
    Container(ContainerState),
    /// `MB::AchievementComponentReplicatedState`.
    Achievement(AchievementState),
    /// `MB::DamageReceiverComponentReplicatedState`.
    DamageReceiver(DamageReceiverState),
    /// `MB::GatherableControllerReplicatedState`.
    GatherableController(GatherableControllerState),
    /// `MB::InteractReplicatedState`.
    Interact(InteractState),
    /// The player's currency.
    Currency(CurrencyState),
    /// `MB::ItemManagementComponentReplicatedState`.
    ItemManagement(ItemManagementState),
    /// `ObjectivesComponentReplicatedState`.
    Objectives(ObjectivesState),
    /// `ProgressionComponentReplicatedState`.
    Progression(ProgressionState),
    /// `CooldownTimersComponentReplicatedState`.
    Cooldowns(CooldownsState),
    /// `MB::LootTrackerComponentReplicatedState`.
    LootTracker(LootTrackerState),
    /// `MB::GlobalStorageComponentReplicatedState`.
    GlobalStorage(GlobalStorageState),
    /// `MB::BuildableControllerReplicatedState`.
    BuildableController(BuildableControllerState),
    /// `MB::PlacementObstructionComponentReplicatedState`.
    PlacementObstruction(PlacementObstructionState),
    /// `MB::TerritoryInterfaceComponentReplicatedState`.
    TerritoryInterface(TerritoryInterfaceState),
    /// `MB::BuildableGridComponentReplicatedState`.
    BuildableGrid(BuildableGridState),
    /// The ownership state (type 3217).
    Ownership(OwnershipState),
    /// `CategoricalProgressionReplicatedState`.
    CategoricalProgression(CategoricalProgressionState),
    /// `ProgressionPointReplicatedState`.
    ProgressionPoints(ProgressionPointState),
    /// `MB::AttributeComponentReplicatedState`.
    Attributes(AttributeState),
    /// `MB::AbilityComponentReplicatedState`.
    Abilities(AbilityState),
    /// `MB::ObjectiveInteractorComponentReplicatedState`.
    ObjectiveInteractor(ObjectiveInteractorState),
    /// `Javelin::GameEventComponentReplicatedState`.
    GameEvents(GameEventState),
    /// `MB::TemporaryAffiliationReplicatedState`.
    TemporaryAffiliation(TemporaryAffiliationState),
    /// `MB::MountComponentReplicatedState`.
    Mount(MountState),
    /// `Javelin::P2PTradeReplicatedState`.
    P2PTrade(P2PTradeState),
    /// `Javelin::PlayerGenericInviteReplicatedState`.
    GenericInvite(GenericInviteState),
    /// `MB::FishingComponentReplicatedState`.
    Fishing(FishingComponentState),
    /// `MB::PlayerTimeComponentReplicatedState`.
    PlayerTime(crate::travel_states::PlayerTimeState),
    /// `MB::PlayerHousingReplicatedState`.
    PlayerHousing(crate::travel_states::PlayerHousingState),
    /// `MB::PlayerHomeComponentReplicatedState`.
    PlayerHome(crate::travel_states::PlayerHomeState),
    /// `MB::CampingComponentReplicatedState`.
    Camping(crate::travel_states::CampingState),
    /// `Javelin::MusicalPerformancePlayerComponentReplicatedState`.
    PerformancePlayer(PerformancePlayerState),
    /// `Javelin::MusicalPerformanceReplicatedState`.
    MusicalPerformance(MusicalPerformanceState),
    /// `Javelin::GroupsComponentReplicatedState`.
    Groups(GroupsState),
    /// `MB::SocialReplicatedState`.
    Social(SocialState),
    /// `Javelin::GuildsComponentReplicatedState`.
    Guilds(Box<GuildsState>),
    /// `MB::ChatReplicatedState`.
    Chat(ChatState),
    /// `Javelin::GroupDataComponentReplicatedState` (the group entity).
    GroupData(Box<GroupDataState>),
    /// `MB::StatusEffectsComponentReplicatedState`.
    StatusEffects(StatusEffectsState),
    /// An empty state of a player component class without a reader of its own.
    Empty(EmptyState),
    /// A state of `class` sent as recorded body bytes (a retail state replayed as it was, of a
    /// class without a reader here).
    Recorded {
        /// The class type index.
        class: u64,
        /// The body bytes.
        body: Vec<u8>,
    },
}

/// The retail first body of player component `class` is the empty state. Any other body is
/// `read`.
fn empty_player_body(
    class: u64,
    reader: &mut Reader<'_>,
    read: impl FnOnce(&mut Reader<'_>) -> Result<MemberState, MessageError>,
) -> Result<MemberState, MessageError> {
    let rest = reader.rest();
    let mut probe = Reader::new(rest);
    if EmptyState::read(class, &mut probe).is_ok() {
        let used = rest.len() - probe.remaining();
        if EmptyState::is_first_body(class, &rest[..used]) {
            reader.bytes(used)?;
            return Ok(MemberState::Empty(EmptyState { class }));
        }
    }
    read(reader)
}

impl MemberState {
    /// The type index of this state's class.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Position(_) => POSITION_IN_THE_WORLD_TYPE_INDEX,
            Self::GdeMetadata(_) => GDE_METADATA_TYPE_INDEX,
            Self::Vitals(_) => VITALS_TYPE_INDEX,
            Self::Alc(_) => ALC_TYPE_INDEX,
            Self::Stamina(_) => STAMINA_TYPE_INDEX,
            Self::CombatStatus(_) => COMBAT_STATUS_TYPE_INDEX,
            Self::DelayedEvent(_) => DELAYED_EVENT_TYPE_INDEX,
            Self::LookTargeting(_) => LOOK_TARGETING_TYPE_INDEX,
            Self::Gathering(_) => GATHERING_TYPE_INDEX,
            Self::Interactor(_) => INTERACTOR_TYPE_INDEX,
            Self::PlayerComponent(_) => PLAYER_COMPONENT_TYPE_INDEX,
            Self::Appearance(_) => PLAYER_APPEARANCE_TYPE_INDEX,
            Self::Mana(_) => MANA_TYPE_INDEX,
            Self::Paperdoll(_) => PAPERDOLL_TYPE_INDEX,
            Self::Container(_) => CONTAINER_TYPE_INDEX,
            Self::Achievement(_) => ACHIEVEMENT_TYPE_INDEX,
            Self::DamageReceiver(_) => DAMAGE_RECEIVER_TYPE_INDEX,
            Self::GatherableController(_) => GATHERABLE_CONTROLLER_TYPE_INDEX,
            Self::Interact(_) => INTERACT_TYPE_INDEX,
            Self::Currency(_) => CURRENCY_TYPE_INDEX,
            Self::ItemManagement(_) => ITEM_MANAGEMENT_TYPE_INDEX,
            Self::Objectives(_) => OBJECTIVES_TYPE_INDEX,
            Self::Progression(_) => PROGRESSION_TYPE_INDEX,
            Self::Cooldowns(_) => COOLDOWNS_TYPE_INDEX,
            Self::LootTracker(_) => LOOT_TRACKER_TYPE_INDEX,
            Self::GlobalStorage(_) => GLOBAL_STORAGE_TYPE_INDEX,
            Self::BuildableController(_) => BUILDABLE_CONTROLLER_TYPE_INDEX,
            Self::PlacementObstruction(_) => PLACEMENT_OBSTRUCTION_TYPE_INDEX,
            Self::TerritoryInterface(_) => TERRITORY_INTERFACE_TYPE_INDEX,
            Self::BuildableGrid(_) => BUILDABLE_GRID_TYPE_INDEX,
            Self::Ownership(_) => OWNERSHIP_TYPE_INDEX,
            Self::CategoricalProgression(_) => CATEGORICAL_PROGRESSION_TYPE_INDEX,
            Self::ProgressionPoints(_) => PROGRESSION_POINT_TYPE_INDEX,
            Self::Attributes(_) => ATTRIBUTE_TYPE_INDEX,
            Self::Abilities(_) => ABILITY_TYPE_INDEX,
            Self::ObjectiveInteractor(_) => OBJECTIVE_INTERACTOR_TYPE_INDEX,
            Self::GameEvents(_) => GAME_EVENT_TYPE_INDEX,
            Self::TemporaryAffiliation(_) => TEMPORARY_AFFILIATION_TYPE_INDEX,
            Self::Mount(_) => MOUNT_TYPE_INDEX,
            Self::P2PTrade(_) => P2P_TRADE_STATE_TYPE_INDEX,
            Self::GenericInvite(_) => GENERIC_INVITE_STATE_TYPE_INDEX,
            Self::Fishing(_) => FISHING_TYPE_INDEX,
            Self::PlayerTime(_) => crate::travel_states::PLAYER_TIME_TYPE_INDEX,
            Self::PlayerHousing(_) => crate::travel_states::PLAYER_HOUSING_TYPE_INDEX,
            Self::PlayerHome(_) => crate::travel_states::PLAYER_HOME_TYPE_INDEX,
            Self::Camping(_) => crate::travel_states::CAMPING_TYPE_INDEX,
            Self::PerformancePlayer(_) => PERFORMANCE_PLAYER_TYPE_INDEX,
            Self::MusicalPerformance(_) => PERFORMANCE_TYPE_INDEX,
            Self::Groups(_) => GROUPS_STATE_TYPE_INDEX,
            Self::Social(_) => SOCIAL_TYPE_INDEX,
            Self::Guilds(_) => GUILDS_STATE_TYPE_INDEX,
            Self::Chat(_) => CHAT_STATE_TYPE_INDEX,
            Self::GroupData(_) => GROUP_DATA_TYPE_INDEX,
            Self::StatusEffects(_) => STATUS_EFFECTS_TYPE_INDEX,
            Self::Empty(state) => state.class,
            Self::Recorded { class, .. } => *class,
        }
    }

    /// The class type id of this state.
    pub const fn class(&self) -> TypeId {
        TypeId::FileSlot(self.type_index())
    }

    /// The state of class `class` in the body bytes `body`, which it must use up. A class without
    /// a reader here, or a body its reader leaves unsized, is passed by its wire layout as its
    /// recorded bytes, as in a bundle walk ([`BundlePayload::decode_known`]).
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the class has neither a reader nor a layout, or the body does
    /// not match it.
    pub fn decode(class: u64, body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let state = match Self::read(TypeId::FileSlot(class), &mut reader) {
            Ok(state) => state,
            Err(MessageError::Unsized { .. } | MessageError::UnknownMemberClass { .. }) => {
                reader = Reader::new(body);
                recorded(TypeId::FileSlot(class), &mut reader)?
            }
            Err(other) => return Err(other),
        };
        reader.finish()?;
        Ok(state)
    }

    fn read(class: TypeId, reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        match class {
            TypeId::FileSlot(POSITION_IN_THE_WORLD_TYPE_INDEX) => {
                PositionInTheWorldReplicatedState::read(reader).map(Self::Position)
            }
            TypeId::FileSlot(GDE_METADATA_TYPE_INDEX) => {
                GdeMetadataReplicatedState::read(reader).map(Self::GdeMetadata)
            }
            TypeId::FileSlot(VITALS_TYPE_INDEX) => VitalsState::read(reader).map(Self::Vitals),
            TypeId::FileSlot(ALC_TYPE_INDEX) => AlcState::read(reader).map(Self::Alc),
            TypeId::FileSlot(STAMINA_TYPE_INDEX) => StaminaState::read(reader).map(Self::Stamina),
            TypeId::FileSlot(COMBAT_STATUS_TYPE_INDEX) => {
                CombatStatusState::read(reader).map(Self::CombatStatus)
            }
            TypeId::FileSlot(DELAYED_EVENT_TYPE_INDEX) => {
                DelayedEventState::read(reader).map(Self::DelayedEvent)
            }
            TypeId::FileSlot(LOOK_TARGETING_TYPE_INDEX) => {
                LookTargetingState::read(reader).map(Self::LookTargeting)
            }
            TypeId::FileSlot(GATHERING_TYPE_INDEX) => {
                GatheringState::read(reader).map(Self::Gathering)
            }
            TypeId::FileSlot(INTERACTOR_TYPE_INDEX) => {
                InteractorState::read(reader).map(Self::Interactor)
            }
            TypeId::FileSlot(PLAYER_COMPONENT_TYPE_INDEX) => {
                PlayerComponentState::read(reader).map(Self::PlayerComponent)
            }
            TypeId::FileSlot(PLAYER_APPEARANCE_TYPE_INDEX) => {
                PlayerAppearanceState::read(reader).map(Self::Appearance)
            }
            TypeId::FileSlot(MANA_TYPE_INDEX) => ManaState::read(reader).map(Self::Mana),
            TypeId::FileSlot(PAPERDOLL_TYPE_INDEX) => {
                PaperdollState::read(reader).map(|state| Self::Paperdoll(Box::new(state)))
            }
            TypeId::FileSlot(CONTAINER_TYPE_INDEX) => {
                ContainerState::read(reader).map(Self::Container)
            }
            TypeId::FileSlot(DAMAGE_RECEIVER_TYPE_INDEX) => {
                DamageReceiverState::read(reader).map(Self::DamageReceiver)
            }
            TypeId::FileSlot(GATHERABLE_CONTROLLER_TYPE_INDEX) => {
                GatherableControllerState::read(reader).map(Self::GatherableController)
            }
            TypeId::FileSlot(INTERACT_TYPE_INDEX) => {
                InteractState::read(reader).map(Self::Interact)
            }
            TypeId::FileSlot(CURRENCY_TYPE_INDEX) => {
                CurrencyState::read(reader).map(Self::Currency)
            }
            TypeId::FileSlot(ACHIEVEMENT_TYPE_INDEX) => {
                AchievementState::read(reader).map(Self::Achievement)
            }
            TypeId::FileSlot(ITEM_MANAGEMENT_TYPE_INDEX) => {
                ItemManagementState::read(reader).map(Self::ItemManagement)
            }
            TypeId::FileSlot(OBJECTIVES_TYPE_INDEX) => {
                ObjectivesState::read(reader).map(Self::Objectives)
            }
            TypeId::FileSlot(PROGRESSION_TYPE_INDEX) => {
                ProgressionState::read(reader).map(Self::Progression)
            }
            TypeId::FileSlot(COOLDOWNS_TYPE_INDEX) => {
                CooldownsState::read(reader).map(Self::Cooldowns)
            }
            TypeId::FileSlot(LOOT_TRACKER_TYPE_INDEX) => {
                LootTrackerState::read(reader).map(Self::LootTracker)
            }
            TypeId::FileSlot(GLOBAL_STORAGE_TYPE_INDEX) => {
                GlobalStorageState::read(reader).map(Self::GlobalStorage)
            }
            TypeId::FileSlot(BUILDABLE_CONTROLLER_TYPE_INDEX) => {
                BuildableControllerState::read(reader).map(Self::BuildableController)
            }
            TypeId::FileSlot(PLACEMENT_OBSTRUCTION_TYPE_INDEX) => {
                PlacementObstructionState::read(reader).map(Self::PlacementObstruction)
            }
            TypeId::FileSlot(TERRITORY_INTERFACE_TYPE_INDEX) => {
                TerritoryInterfaceState::read(reader).map(Self::TerritoryInterface)
            }
            TypeId::FileSlot(BUILDABLE_GRID_TYPE_INDEX) => {
                BuildableGridState::read(reader).map(Self::BuildableGrid)
            }
            TypeId::FileSlot(OWNERSHIP_TYPE_INDEX) => {
                OwnershipState::read(reader).map(Self::Ownership)
            }
            TypeId::FileSlot(CATEGORICAL_PROGRESSION_TYPE_INDEX) => {
                CategoricalProgressionState::read(reader).map(Self::CategoricalProgression)
            }
            TypeId::FileSlot(PROGRESSION_POINT_TYPE_INDEX) => {
                ProgressionPointState::read(reader).map(Self::ProgressionPoints)
            }
            TypeId::FileSlot(ATTRIBUTE_TYPE_INDEX) => {
                AttributeState::read(reader).map(Self::Attributes)
            }
            TypeId::FileSlot(ABILITY_TYPE_INDEX) => AbilityState::read(reader).map(Self::Abilities),
            TypeId::FileSlot(OBJECTIVE_INTERACTOR_TYPE_INDEX) => {
                ObjectiveInteractorState::read(reader).map(Self::ObjectiveInteractor)
            }
            TypeId::FileSlot(GAME_EVENT_TYPE_INDEX) => {
                // The new-character body stays the empty state (`01 02 00 01 03 00`).
                empty_player_body(GAME_EVENT_TYPE_INDEX, reader, |reader| {
                    GameEventState::read(reader).map(Self::GameEvents)
                })
            }
            TypeId::FileSlot(TEMPORARY_AFFILIATION_TYPE_INDEX) => {
                // No affiliation yet: one slot byte of 0, the empty state.
                empty_player_body(TEMPORARY_AFFILIATION_TYPE_INDEX, reader, |reader| {
                    TemporaryAffiliationState::read(reader).map(Self::TemporaryAffiliation)
                })
            }
            TypeId::FileSlot(MOUNT_TYPE_INDEX) => MountState::read(reader).map(Self::Mount),
            TypeId::FileSlot(P2P_TRADE_STATE_TYPE_INDEX) => {
                P2PTradeState::read(reader).map(Self::P2PTrade)
            }
            TypeId::FileSlot(GENERIC_INVITE_STATE_TYPE_INDEX) => {
                GenericInviteState::read(reader).map(Self::GenericInvite)
            }
            TypeId::FileSlot(FISHING_TYPE_INDEX) => {
                FishingComponentState::read(reader).map(Self::Fishing)
            }
            TypeId::FileSlot(crate::travel_states::PLAYER_TIME_TYPE_INDEX) => {
                crate::travel_states::PlayerTimeState::read(reader).map(Self::PlayerTime)
            }
            TypeId::FileSlot(crate::travel_states::PLAYER_HOUSING_TYPE_INDEX) => {
                crate::travel_states::PlayerHousingState::read(reader).map(Self::PlayerHousing)
            }
            TypeId::FileSlot(crate::travel_states::PLAYER_HOME_TYPE_INDEX) => empty_player_body(
                crate::travel_states::PLAYER_HOME_TYPE_INDEX,
                reader,
                |reader| crate::travel_states::PlayerHomeState::read(reader).map(Self::PlayerHome),
            ),
            TypeId::FileSlot(crate::travel_states::CAMPING_TYPE_INDEX) => {
                crate::travel_states::CampingState::read(reader).map(Self::Camping)
            }
            TypeId::FileSlot(PERFORMANCE_PLAYER_TYPE_INDEX) => {
                PerformancePlayerState::read(reader).map(Self::PerformancePlayer)
            }
            TypeId::FileSlot(PERFORMANCE_TYPE_INDEX) => {
                MusicalPerformanceState::read(reader).map(Self::MusicalPerformance)
            }
            TypeId::FileSlot(GROUPS_STATE_TYPE_INDEX) => {
                GroupsState::read(reader).map(Self::Groups)
            }
            TypeId::FileSlot(SOCIAL_TYPE_INDEX) => SocialState::read(reader).map(Self::Social),
            TypeId::FileSlot(GUILDS_STATE_TYPE_INDEX) => {
                GuildsState::read(reader).map(Box::new).map(Self::Guilds)
            }
            TypeId::FileSlot(CHAT_STATE_TYPE_INDEX) => ChatState::read(reader).map(Self::Chat),
            TypeId::FileSlot(GROUP_DATA_TYPE_INDEX) => GroupDataState::read(reader)
                .map(Box::new)
                .map(Self::GroupData),
            TypeId::FileSlot(STATUS_EFFECTS_TYPE_INDEX) => {
                StatusEffectsState::read(reader).map(Self::StatusEffects)
            }
            TypeId::FileSlot(index) if EmptyState::covers(index) => {
                EmptyState::read(index, reader).map(Self::Empty)
            }
            _ => Err(MessageError::UnknownMemberClass { class }),
        }
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::Position(state) => state.write(writer),
            Self::GdeMetadata(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Vitals(state) => state.write(writer),
            Self::Alc(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Stamina(state) => {
                state.write(writer);
                Ok(())
            }
            Self::CombatStatus(state) => {
                state.write(writer);
                Ok(())
            }
            Self::DelayedEvent(state) => {
                state.write(writer);
                Ok(())
            }
            Self::LookTargeting(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Gathering(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Interactor(state) => {
                state.write(writer);
                Ok(())
            }
            Self::PlayerComponent(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Appearance(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Mana(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Paperdoll(state) => state.write(writer),
            Self::Container(state) => state.write(writer),
            Self::Achievement(state) => state.write(writer),
            Self::DamageReceiver(state) => {
                state.write(writer);
                Ok(())
            }
            Self::GatherableController(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Interact(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Currency(state) => {
                state.write(writer);
                Ok(())
            }
            Self::ItemManagement(state) => state.write(writer),
            Self::Objectives(state) => state.write(writer),
            Self::Progression(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Cooldowns(state) => state.write(writer),
            Self::LootTracker(state) => state.write(writer),
            Self::GlobalStorage(state) => state.write(writer),
            Self::BuildableController(state) => state.write(writer),
            Self::PlacementObstruction(state) => {
                state.write(writer);
                Ok(())
            }
            Self::TerritoryInterface(state) => {
                state.write(writer);
                Ok(())
            }
            Self::BuildableGrid(state) => state.write(writer),
            Self::Ownership(state) => state.write(writer),
            Self::CategoricalProgression(state) => state.write(writer),
            Self::ProgressionPoints(state) => state.write(writer),
            Self::Attributes(state) => state.write(writer),
            Self::Abilities(state) => {
                state.write(writer);
                Ok(())
            }
            Self::ObjectiveInteractor(state) => state.write(writer),
            Self::GameEvents(state) => state.write(writer),
            Self::TemporaryAffiliation(state) => state.write(writer),
            Self::Mount(state) => state.write(writer),
            Self::P2PTrade(state) => state.write(writer),
            Self::GenericInvite(state) => state.write(writer),
            Self::Fishing(state) => state.write(writer),
            Self::PlayerTime(state) => {
                state.write(writer);
                Ok(())
            }
            Self::PlayerHousing(state) => state.write(writer),
            Self::PlayerHome(state) => state.write(writer),
            Self::Camping(state) => {
                state.write(writer);
                Ok(())
            }
            Self::PerformancePlayer(state) => {
                state.write(writer);
                Ok(())
            }
            Self::MusicalPerformance(state) => state.write(writer),
            Self::Groups(state) => state.write(writer),
            Self::Social(state) => state.write(writer),
            Self::Guilds(state) => state.write(writer),
            Self::Chat(state) => state.write(writer),
            Self::GroupData(state) => state.write(writer),
            Self::StatusEffects(state) => state.write(writer),
            Self::Empty(state) => {
                state.write(writer);
                Ok(())
            }
            Self::Recorded { body, .. } => {
                writer.bytes(body);
                Ok(())
            }
        }
    }
}

/// One member: one replicated state of one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct BundleMember {
    /// The first prefix-coded integer: which replicated component of the entity this state is.
    /// In the captures the spawn card is always 0, the own player's `ALCReplicatedState` is 16,
    /// and a mob's is 4 or 5.
    pub component: u64,
    /// The state.
    pub state: MemberState,
}

impl BundleMember {
    /// The member bytes: the integer, the class type id, and the state.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Prefix`] when the integer does not fit the prefix code.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        writer
            .prefixed(self.component)?
            .bytes(&encode_type_id(&self.state.class())?);
        self.state.write(&mut writer)?;
        Ok(writer.finish())
    }

    /// Reads the member at the start of `bytes` (the integer, the class type id and the state)
    /// and how many bytes it takes; the bytes after it are not read.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::UnknownMemberClass`] for a class without a reader, and another
    /// [`MessageError`] when the bytes do not match the layout.
    pub fn read_from(bytes: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut reader = Reader::new(bytes);
        let member = read_member(&mut reader)?;
        Ok((member, bytes.len() - reader.remaining()))
    }
}

/// One group of members.
#[derive(Debug, Clone, PartialEq)]
pub struct BundleGroup {
    /// The group's prefix-coded integer: the entity slot (`interestId`) of the members. The client
    /// keeps its low 16 bits.
    pub interest: u64,
    /// The members; at most `u8::MAX`.
    pub members: Vec<BundleMember>,
}

/// A whole payload of members whose classes all have a reader here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BundlePayload {
    /// The groups, in order.
    pub groups: Vec<BundleGroup>,
}

/// The members at the front of a payload that have a reader here, with the byte range of each.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KnownMembers {
    /// The members and their bytes in the payload.
    pub members: Vec<(BundleMember, Range<usize>)>,
    /// The class of the first member without a reader; `None` when the whole payload was read.
    pub stopped_at: Option<TypeId>,
    /// The class and the error of a member whose reader failed; the walk ends there.
    pub failed: Option<(TypeId, String)>,
}

impl BundlePayload {
    /// The payload bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] for a group of more than `u8::MAX` members, and
    /// [`MessageError::Prefix`] for an integer that does not fit the prefix code.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        for group in &self.groups {
            let count =
                u8::try_from(group.members.len()).map_err(|_| MessageError::CountTooLarge {
                    count: u64::try_from(group.members.len()).unwrap_or(u64::MAX),
                    limit: u64::from(u8::MAX),
                })?;
            writer.prefixed(group.interest)?.u8(count);
            for member in &group.members {
                writer.bytes(&member.encode()?);
            }
        }
        Ok(writer.finish())
    }

    /// Reads a payload whose members all have a reader here.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::UnknownMemberClass`] for a member without a reader, and another
    /// [`MessageError`] when the bytes do not match the layout.
    pub fn decode(payload: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(payload);
        let mut groups = Vec::new();
        while !reader.is_empty() {
            let interest = reader.prefixed()?;
            let count = reader.u8()?;
            let mut members = Vec::new();
            for _ in 0..count {
                members.push(read_member(&mut reader)?);
            }
            groups.push(BundleGroup { interest, members });
        }
        Ok(Self { groups })
    }

    /// Reads the members at the front of a payload until the end, a member of a class without a
    /// reader ([`KnownMembers::stopped_at`]), or a member whose reader fails
    /// ([`KnownMembers::failed`]); the members before it are kept.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the bytes before that point do not match the layout.
    pub fn decode_known(payload: &[u8]) -> Result<KnownMembers, MessageError> {
        let mut reader = Reader::new(payload);
        let mut known = KnownMembers::default();
        while !reader.is_empty() {
            reader.prefixed()?;
            let count = reader.u8()?;
            for _ in 0..count {
                let start = payload.len() - reader.remaining();
                let member = match read_member(&mut reader) {
                    Err(MessageError::UnknownMemberClass { class }) => {
                        known.stopped_at = Some(class);
                        return Ok(known);
                    }
                    Err(MessageError::InMember { class, source }) => {
                        let error = MessageError::InMember { class, source };
                        known.failed = Some((class, error.to_string()));
                        return Ok(known);
                    }
                    other => other?,
                };
                let end = payload.len() - reader.remaining();
                known.members.push((member, start..end));
            }
        }
        Ok(known)
    }
}

/// One member: the integer, the class type id, and the state of that class. A class without a
/// reader here, or a body its reader cannot size (a slot or child it does not know), is passed
/// by the class's wire layout ([`member_layouts`]) and kept as its recorded bytes; a class
/// without a layout stops the walk. Any other failure names the member's class.
fn read_member(reader: &mut Reader<'_>) -> Result<BundleMember, MessageError> {
    let component = reader.prefixed()?;
    let class = read_class(reader)?;
    let body = reader.rest();
    let mut own = Reader::new(body);
    let state = match MemberState::read(class, &mut own) {
        Ok(state) => state,
        Err(MessageError::Unsized { .. } | MessageError::UnknownMemberClass { .. }) => {
            own = Reader::new(body);
            recorded(class, &mut own)?
        }
        Err(other) => {
            return Err(MessageError::InMember {
                class,
                source: Box::new(other),
            });
        }
    };
    reader.bytes(body.len() - own.remaining())?;
    Ok(BundleMember { component, state })
}

/// The state of class `class` passed by its wire layout from `reader`, as its recorded bytes.
fn recorded(class: TypeId, reader: &mut Reader<'_>) -> Result<MemberState, MessageError> {
    let TypeId::FileSlot(index) = class else {
        return Err(MessageError::UnknownMemberClass { class });
    };
    let layout = member_layouts::layout(index).ok_or(MessageError::UnknownMemberClass { class })?;
    let body = reader.rest();
    member_layouts::skip_body(layout, reader).map_err(|error| MessageError::InMember {
        class,
        source: Box::new(error),
    })?;
    Ok(MemberState::Recorded {
        class: index,
        body: body[..body.len() - reader.remaining()].to_vec(),
    })
}

fn read_class(reader: &mut Reader<'_>) -> Result<TypeId, MessageError> {
    let (class, len) = decode_type_id(reader.rest())?;
    reader.bytes(len)?;
    Ok(class)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::gde::{AssetId, GDE_ID_LEN, GdeMetadataReplicatedState};
    use crate::position::WorldPosition;
    use crate::vitals::{VITALS_TYPE_INDEX, VitalsSlot0, VitalsState};
    use crate::wire::RAW_ID_LEN;

    const ENTITY: u64 = 300;
    const LEADING: u64 = 7;
    /// A class without a reader or a layout here (a message class, never a member).
    const OTHER_CLASS: u64 = 1;
    /// An entity integer that fits one prefix byte.
    const SMALL_ENTITY: u8 = 5;

    fn position_member(x: f32) -> BundleMember {
        BundleMember {
            component: ENTITY,
            state: MemberState::Position(PositionInTheWorldReplicatedState {
                position: Some(WorldPosition::from_metres(x, 2.0, 3.0).expect("finite")),
                ..PositionInTheWorldReplicatedState::default()
            }),
        }
    }

    fn payload() -> BundlePayload {
        BundlePayload {
            groups: vec![
                BundleGroup {
                    interest: LEADING,
                    members: vec![position_member(1.0), position_member(4.0)],
                },
                BundleGroup {
                    interest: LEADING + 1,
                    members: vec![position_member(5.0)],
                },
            ],
        }
    }

    #[test]
    fn members_of_the_new_classes_walk_in_a_payload() {
        let card = BundleMember {
            component: ENTITY,
            state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
                asset: Some(AssetId {
                    id: [1; GDE_ID_LEN],
                    sub_id: 2,
                }),
                gde_ref: Some([9; GDE_ID_LEN]),
            }),
        };
        let vitals = BundleMember {
            component: ENTITY,
            state: MemberState::Vitals(VitalsState {
                slot_0: Some(VitalsSlot0 {
                    vitals_level: Some(5),
                    max_health: Some(1000),
                    ..VitalsSlot0::default()
                }),
                slot_1: None,
            }),
        };
        let mixed = BundlePayload {
            groups: vec![BundleGroup {
                interest: LEADING,
                members: vec![position_member(1.0), card, vitals],
            }],
        };
        let bytes = mixed.encode().expect("encode");
        assert_eq!(BundlePayload::decode(&bytes), Ok(mixed));
    }

    /// A body of a class without a reader decodes alone by its layout, as in a bundle walk (the
    /// retail mob copies: `AlignToTerrain` `01 01 01` on 782 creatures), and a body longer than
    /// its layout does not.
    #[test]
    fn a_body_of_a_class_without_a_reader_decodes_by_its_layout() {
        const ALIGN_TO_TERRAIN_TYPE_INDEX: u64 = 2850;
        const RETAIL_BODY: [u8; 3] = [0x01, 0x01, 0x01];
        assert_eq!(
            MemberState::decode(ALIGN_TO_TERRAIN_TYPE_INDEX, &RETAIL_BODY),
            Ok(MemberState::Recorded {
                class: ALIGN_TO_TERRAIN_TYPE_INDEX,
                body: RETAIL_BODY.to_vec(),
            })
        );
        assert!(
            MemberState::decode(ALIGN_TO_TERRAIN_TYPE_INDEX, &[0x01, 0x01, 0x01, 0x00]).is_err()
        );
    }

    #[test]
    fn a_member_reads_alone_with_its_length_and_leaves_the_rest() {
        let member = position_member(1.0);
        let mut bytes = member.encode().expect("encode");
        let length = bytes.len();
        bytes.extend([0xff, 0xff]);
        assert_eq!(BundleMember::read_from(&bytes), Ok((member, length)));
    }

    #[test]
    fn a_recorded_member_writes_its_bytes_as_they_are() {
        const FTUE_ISLAND_TYPE_INDEX: u64 = 3194;
        let body = vec![0x01, 0x0f, 0xaa];
        let member = BundleMember {
            component: 2,
            state: MemberState::Recorded {
                class: FTUE_ISLAND_TYPE_INDEX,
                body: body.clone(),
            },
        };
        let bytes = member.encode().expect("encode");
        assert!(bytes.ends_with(&body));
        assert_eq!(member.state.type_index(), FTUE_ISLAND_TYPE_INDEX);
    }

    #[test]
    fn a_member_of_a_class_without_a_reader_is_passed_by_its_layout() {
        // `MB::TransformLinkComponentReplicatedState` (5980) has no reader here; its layout
        // passes it: slot 0, child 0 `m_childGDEVector`, the full form of an empty list.
        const TRANSFORM_LINK: u64 = 5980;
        let mut bytes = Writer::new();
        bytes.prefixed(LEADING).expect("leading").u8(3);
        bytes.bytes(&position_member(1.0).encode().expect("member"));
        bytes.prefixed(ENTITY).expect("entity");
        bytes.bytes(&nw_reflect::encode_type_id(&TypeId::FileSlot(TRANSFORM_LINK)).expect("class"));
        let body = [0x01, 0x01, 0x00, 0x00, 0x00];
        bytes.bytes(&body);
        bytes.bytes(&position_member(2.0).encode().expect("member"));
        let payload = bytes.finish();
        let known = BundlePayload::decode_known(&payload).expect("known");
        assert_eq!((known.stopped_at, known.failed), (None, None));
        assert_eq!(known.members.len(), 3);
        assert_eq!(
            known.members[1].0.state,
            MemberState::Recorded {
                class: TRANSFORM_LINK,
                body: body.to_vec()
            }
        );
        let decoded = BundlePayload::decode(&payload).expect("decode");
        assert_eq!(decoded.encode().expect("encode"), payload);
    }

    #[test]
    fn a_vitals_member_on_a_slot_its_class_lacks_fails_the_walk() {
        let mut bytes = Writer::new();
        bytes.prefixed(LEADING).expect("leading").u8(2);
        let first = position_member(1.0).encode().expect("member");
        bytes.bytes(&first);
        bytes.prefixed(ENTITY).expect("entity");
        bytes.bytes(
            &nw_reflect::encode_type_id(&TypeId::FileSlot(VITALS_TYPE_INDEX)).expect("class"),
        );
        bytes.u8(0b100).u8(0);
        let payload = bytes.finish();
        let known = BundlePayload::decode_known(&payload).expect("known");
        assert_eq!(known.members.len(), 1);
        assert_eq!(known.stopped_at, None);
        let (class, _) = known.failed.expect("no such slot");
        assert_eq!(class, TypeId::FileSlot(VITALS_TYPE_INDEX));
    }

    #[test]
    fn a_vitals_member_with_an_empty_last_child_byte_fails_the_walk() {
        // The client reads an empty last child byte as "no more children"; the writer never
        // makes that form, so neither the reader nor the layout passes it.
        let mut bytes = Writer::new();
        bytes.prefixed(LEADING).expect("leading").u8(2);
        bytes.bytes(&position_member(1.0).encode().expect("member"));
        bytes.prefixed(ENTITY).expect("entity");
        bytes.bytes(
            &nw_reflect::encode_type_id(&TypeId::FileSlot(VITALS_TYPE_INDEX)).expect("class"),
        );
        bytes.u8(0x01).u8(0x80).u8(0x00);
        let payload = bytes.finish();
        let known = BundlePayload::decode_known(&payload).expect("known");
        assert_eq!(known.members.len(), 1);
        assert_eq!(known.stopped_at, None);
        let (class, _) = known.failed.expect("an empty last child byte");
        assert_eq!(class, TypeId::FileSlot(VITALS_TYPE_INDEX));
    }

    #[test]
    fn a_member_that_ends_early_names_its_class() {
        let mut bytes = Writer::new();
        bytes
            .prefixed(LEADING)
            .expect("leading")
            .u8(1)
            .prefixed(ENTITY)
            .expect("entity");
        bytes.bytes(
            &nw_reflect::encode_type_id(&TypeId::FileSlot(VITALS_TYPE_INDEX)).expect("class"),
        );
        bytes.u8(0x01).u8(0x04).u8(0);
        let known = BundlePayload::decode_known(&bytes.finish()).expect("known");
        let (class, error) = known.failed.expect("ends early");
        assert_eq!(class, TypeId::FileSlot(VITALS_TYPE_INDEX));
        assert!(error.starts_with("member of class"), "{error}");
    }

    #[test]
    fn groups_and_members_round_trip() {
        let bytes = payload().encode().expect("encode");
        assert_eq!(&bytes[..2], &[LEADING as u8, 2]);
        assert_eq!(BundlePayload::decode(&bytes), Ok(payload()));
        let known = BundlePayload::decode_known(&bytes).expect("known");
        assert_eq!(known.members.len(), 3);
        assert_eq!(known.stopped_at, None);
        for (member, range) in known.members {
            assert_eq!(member.encode().expect("member"), bytes[range]);
        }
    }

    #[test]
    fn a_failing_member_keeps_the_members_before_it() {
        let mut bytes = vec![LEADING as u8, 2];
        bytes.extend(position_member(1.0).encode().expect("member"));
        // A type 11 member whose `blob33` count is over its limit.
        let mut broken = Writer::new();
        broken
            .prefixed(ENTITY)
            .expect("entity")
            .bytes(
                &nw_reflect::encode_type_id(&TypeId::FileSlot(crate::alc::ALC_TYPE_INDEX))
                    .expect("class"),
            )
            .u8(1)
            .wide_prefixed(1 << 29)
            .prefixed(40)
            .expect("count");
        bytes.extend(broken.finish());
        let known = BundlePayload::decode_known(&bytes).expect("known");
        assert_eq!(known.members.len(), 1);
        assert_eq!(known.stopped_at, None);
        let failed = known.failed.expect("the failing member");
        assert_eq!(failed.0, TypeId::FileSlot(crate::alc::ALC_TYPE_INDEX));
    }

    #[test]
    fn a_member_without_a_reader_ends_the_known_members() {
        let mut bytes = vec![LEADING as u8, 2];
        bytes.extend(position_member(1.0).encode().expect("member"));
        bytes.extend([SMALL_ENTITY, OTHER_CLASS as u8, 9, 9, 9]);
        let known = BundlePayload::decode_known(&bytes).expect("known");
        assert_eq!(known.members.len(), 1);
        assert_eq!(known.stopped_at, Some(TypeId::FileSlot(OTHER_CLASS)));
        assert_eq!(
            BundlePayload::decode(&bytes),
            Err(MessageError::UnknownMemberClass {
                class: TypeId::FileSlot(OTHER_CLASS)
            })
        );
    }

    #[test]
    fn an_inline_uuid_class_takes_sixteen_bytes() {
        let mut bytes = vec![LEADING as u8, 1, 1, 0];
        bytes.extend([4; RAW_ID_LEN]);
        let known = BundlePayload::decode_known(&bytes).expect("known");
        assert_eq!(known.stopped_at, Some(TypeId::InlineUuid([4; RAW_ID_LEN])));
    }

    proptest! {
        #[test]
        fn random_payloads_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = BundlePayload::decode(&bytes);
            let _ = BundlePayload::decode_known(&bytes);
        }

        #[test]
        fn random_bytes_after_a_position_member_header_never_panic(
            tail in proptest::collection::vec(any::<u8>(), 0..48),
        ) {
            let mut bytes = vec![LEADING as u8, 1, SMALL_ENTITY, POSITION_IN_THE_WORLD_TYPE_INDEX as u8];
            bytes.extend(tail);
            let _ = BundlePayload::decode(&bytes);
            let _ = BundlePayload::decode_known(&bytes);
        }
    }

    #[test]
    fn too_many_members_or_a_short_payload_are_errors() {
        let group = BundleGroup {
            interest: LEADING,
            members: vec![position_member(1.0); usize::from(u8::MAX) + 1],
        };
        assert!(matches!(
            BundlePayload {
                groups: vec![group]
            }
            .encode(),
            Err(MessageError::CountTooLarge { .. })
        ));
        let bytes = payload().encode().expect("encode");
        assert!(BundlePayload::decode(&bytes[..bytes.len() - 1]).is_err());
        let known = BundlePayload::decode_known(&bytes[..bytes.len() - 1]).expect("known");
        assert_eq!(known.members.len(), 2, "the members before the cut one");
        assert!(known.failed.is_some());
        assert_eq!(BundlePayload::decode(&[]), Ok(BundlePayload::default()));
    }
}
