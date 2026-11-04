// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every server message of M4 with a known layout (M4 spec section 5.2), by type index.

use crate::abilities::MasteryTreeRespecResponseMsg;
use crate::arena::ArenaServerMsg;
use crate::bundle::ReplicatedStateBundle;
use crate::client_add_entry::ClientAddEntryMsg;
use crate::combat::{DamageDealtMsg, DeathRecapMsg, MeleeHitMsg, OnDamageMsg, OnHealedMsg};
use crate::combat_effects::CombatEffectMsg;
use crate::connected::{CalendarConnectedMsg, TimingConfigurationConnectedMsg};
use crate::day_phase::SyncDayPhaseMsg;
use crate::economy::EconomyMessage;
use crate::facet::{
    AdjustFxScriptsMsg, CraftProgressMsg, CutsceneEndFinalizedMsg, CutsceneGrantedMsg,
    DisableCollisionMsg, GameModeSchedulerRefreshedMsg, GatheringInterruptedMsg, HomingOverrideMsg,
    InteractionEndedMsg, InteractionValidatedMsg, LeftGameModeQueueMsg, PlayerSeenByAiMsg,
    SelectedAsTargetMsg, StartWhisperMsg, StorageSearchItemsMsg, TeleportWithLoadingScreenMsg,
    UnselectedAsTargetMsg, UpdatePhasingMsg,
};
use crate::fishing::FishingServerMsg;
use crate::game_mode::GameModeServerMsg;
use crate::guild_lookup::UnorderedGuildsMsg;
use crate::inventories::InventoriesMsg;
use crate::item_calls::LoadoutStatusMsg;
use crate::kick::{KickClientMsg, PlayerManagerRejectedMsg};
use crate::level_info::LevelInfoChangedMsg;
use crate::lookups::{JoinableSubArmiesMsg, LobbyCountsMsg, PendingMetaAchievementsMsg};
use crate::meta_achievements::{
    MetaAchievementProgressMsg, MetaAchievementSummaryMsg, MetaProgressChangedMsg,
};
use crate::music::MusicServerMsg;
use crate::notifications::{
    SendNotificationMsg, SendNotificationWithReplacementsMsg, SyncNotificationsMsg,
};
use crate::player_data::PlayerDataResponseMsg;
use crate::self_ident::PlayerManagerSelfIdentificationMsg;
use crate::social::RequestFailedMsg;
use crate::spawn_point::ReceivePlayerSpawnPointMsg;
use crate::spreadshot::SpreadshotProcessedMsg;
use crate::storage_search::StorageSearchReadyMsg;
use crate::storage_search::StorageSearchResultsMsg;
use crate::store::{RequestInitializedMsg, RequestSuccessMsg};
use crate::time_change::SyncTimeChangeMsg;
use crate::time_synch::TimeSynchMsg;
use crate::titles::{AvailableTitlesMsg, TitlesUnlockMsg};
use crate::transmog::TransmogServerMsg;
use crate::travel::TravelServerMsg;
use crate::trigger_entity::SyncExecuteEntitiesMsg;
use crate::war::WarServerMsg;
use crate::war_signup::WarSignupServerMsg;
use nw_reflect::{Envelope, TypeId};

use crate::wire::{BodyCodec, MessageError};

/// Type indexes of [`ServerMessage`]: the login in the captured order, the answer to the
/// client's time request, then the bundle.
pub const SERVER_MESSAGE_TYPES: [u64; 10] = [
    TimingConfigurationConnectedMsg::TYPE_INDEX,
    CalendarConnectedMsg::TYPE_INDEX,
    PlayerManagerSelfIdentificationMsg::TYPE_INDEX,
    ClientAddEntryMsg::TYPE_INDEX,
    ReceivePlayerSpawnPointMsg::TYPE_INDEX,
    TimeSynchMsg::TYPE_INDEX,
    LevelInfoChangedMsg::TYPE_INDEX,
    SyncTimeChangeMsg::TYPE_INDEX,
    SyncDayPhaseMsg::TYPE_INDEX,
    ReplicatedStateBundle::TYPE_INDEX,
];

/// Type indexes of the login messages, in the captured order: the first seven types of
/// [`SERVER_MESSAGE_TYPES`]. The time change and the day phase are not among them: retail sends
/// them as the answer to the client's time request (4248), once its player exists (facets note).
pub const LOGIN_MESSAGE_TYPES: &[u64] = SERVER_MESSAGE_TYPES.split_at(LOGIN_MESSAGES).0;
/// How many login messages there are.
const LOGIN_MESSAGES: usize = 7;

/// One server message of M4.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// 446.
    TimingConfigurationConnected(TimingConfigurationConnectedMsg),
    /// 1034.
    CalendarConnected(CalendarConnectedMsg),
    /// 1628.
    SelfIdentification(PlayerManagerSelfIdentificationMsg),
    /// 164.
    ClientAddEntry(ClientAddEntryMsg),
    /// 1617.
    PlayerSpawnPoint(ReceivePlayerSpawnPointMsg),
    /// 335.
    TimeSynch(TimeSynchMsg),
    /// 4246.
    TimeChange(SyncTimeChangeMsg),
    /// 4247.
    DayPhase(SyncDayPhaseMsg),
    /// 1635.
    LevelInfoChanged(LevelInfoChangedMsg),
    /// 8.
    Bundle(ReplicatedStateBundle),
    /// 2071.
    DamageDealt(DamageDealtMsg),
    /// 3601.
    OnDamage(OnDamageMsg),
    /// 2218.
    MeleeHit(MeleeHitMsg),
    /// 608.
    AdjustFxScripts(AdjustFxScriptsMsg),
    /// 5936.
    RequestInitialized(RequestInitializedMsg),
    /// 1285.
    RequestSuccess(RequestSuccessMsg),
    /// 4299.
    DeathRecap(DeathRecapMsg),
    /// 4298.
    OnHealed(OnHealedMsg),
    /// 6310.
    UpdatePhasing(UpdatePhasingMsg),
    /// 6707.
    CutsceneGranted(CutsceneGrantedMsg),
    /// 6709.
    CutsceneEndFinalized(CutsceneEndFinalizedMsg),
    /// 2570.
    SpreadshotProcessed(SpreadshotProcessedMsg),
    /// 3951.
    TeleportWithLoadingScreen(TeleportWithLoadingScreenMsg),
    /// 7048.
    StorageSearchItems(StorageSearchItemsMsg),
    /// 7046.
    StorageSearchResults(StorageSearchResultsMsg),
    /// 5497.
    LoadoutStatus(LoadoutStatusMsg),
    /// 3743.
    InteractionValidated(InteractionValidatedMsg),
    /// 3746.
    InteractionEnded(InteractionEndedMsg),
    /// 2724.
    GatheringInterrupted(GatheringInterruptedMsg),
    /// 955.
    PlayerSeenByAi(PlayerSeenByAiMsg),
    /// 2415.
    SelectedAsTarget(SelectedAsTargetMsg),
    /// 1790.
    HomingOverride(HomingOverrideMsg),
    /// 4970.
    LeftGameModeQueue(LeftGameModeQueueMsg),
    /// 3916.
    UnselectedAsTarget(UnselectedAsTargetMsg),
    /// 1793.
    DisableCollision(DisableCollisionMsg),
    /// 2993.
    CraftProgress(CraftProgressMsg),
    /// 5070.
    StartWhisper(StartWhisperMsg),
    /// 6426.
    GameModeSchedulerRefreshed(GameModeSchedulerRefreshedMsg),
    /// The economy: the trading post (2278 among them), trade and seasons
    /// ([`crate::economy`]).
    Economy(EconomyMessage),
    /// 4178.
    JoinableSubArmies(JoinableSubArmiesMsg),
    /// 3802.
    PendingMetaAchievements(PendingMetaAchievementsMsg),
    /// 5458.
    LobbyCounts(LobbyCountsMsg),
    /// 3236.
    SyncNotifications(SyncNotificationsMsg),
    /// 3233.
    SendNotification(SendNotificationMsg),
    /// 3234.
    SendNotificationWithReplacements(SendNotificationWithReplacementsMsg),
    /// A travel answer ([`TravelServerMsg`]: fast travel, unstuck, camp and housing).
    Travel(TravelServerMsg),
    /// An arena answer ([`ArenaServerMsg`]: expedition and trial entry, 5038 and 6776).
    Arena(ArenaServerMsg),
    /// 4147.
    AvailableTitles(AvailableTitlesMsg),
    /// 4140.
    PlayerData(PlayerDataResponseMsg),
    /// 3610.
    UnorderedGuilds(UnorderedGuildsMsg),
    /// 1926.
    RequestFailed(RequestFailedMsg),
    /// 271.
    KickClient(KickClientMsg),
    /// 1630.
    PlayerManagerRejected(PlayerManagerRejectedMsg),
    /// 3135.
    SyncExecuteEntities(SyncExecuteEntitiesMsg),
    /// 2858.
    MasteryTreeRespecResponse(MasteryTreeRespecResponseMsg),
    /// 1679.
    MetaAchievementSummary(MetaAchievementSummaryMsg),
    /// 3800.
    MetaAchievementProgress(MetaAchievementProgressMsg),
    /// 331.
    MetaProgressChanged(MetaProgressChangedMsg),
    /// A fishing call ([`FishingServerMsg`]: 2444, 3172, 557).
    Fishing(FishingServerMsg),
    /// A musical performance call ([`MusicServerMsg`]: 4869, 4889).
    Music(MusicServerMsg),
    /// A transmog answer ([`TransmogServerMsg`]: 5985, 6147).
    Transmog(TransmogServerMsg),
    /// An inventory service answer ([`InventoriesMsg`]: 5792, 5771).
    Inventories(InventoriesMsg),
    /// A matchmaking push ([`GameModeServerMsg`]: 1234, 5449, 6295, 6414, 6970).
    GameMode(GameModeServerMsg),
    /// A war push ([`WarServerMsg`]: 839, 3530, 2968, 4062, 4063).
    War(WarServerMsg),
    /// A war sign-up answer ([`WarSignupServerMsg`]: 2726, 2729, 2169).
    WarSignup(WarSignupServerMsg),
    /// A combat effect push ([`CombatEffectMsg`]: 2832, 1924, 2900, 38, 3034, 2909, 5322,
    /// 3031, 2863, 2720, 5269).
    CombatEffect(CombatEffectMsg),
    /// 3585.
    TitlesUnlock(TitlesUnlockMsg),
    /// 7047.
    StorageSearchReady(StorageSearchReadyMsg),
}

impl ServerMessage {
    /// Decodes `body` when `type_index` is one of [`SERVER_MESSAGE_TYPES`], a combat message of
    /// M6-B (2071, 3601, 4299, 2218) or a facet call (6310, 6707, 6709, 3951, 3743, 3746, 2724,
    /// 955, 2415, 3916, 1793, 2993, 5070, 6426) or a lookup answer (2278, 4178, 3802, 5458,
    /// 4147, 4140, 3610) or a session end (271, 1630); `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        if let Some(message) = EconomyMessage::decode(type_index, body) {
            return Some(message.map(Self::Economy));
        }
        let message = match type_index {
            TimingConfigurationConnectedMsg::TYPE_INDEX => {
                TimingConfigurationConnectedMsg::decode(body)
                    .map(Self::TimingConfigurationConnected)
            }
            CalendarConnectedMsg::TYPE_INDEX => {
                CalendarConnectedMsg::decode(body).map(Self::CalendarConnected)
            }
            PlayerManagerSelfIdentificationMsg::TYPE_INDEX => {
                PlayerManagerSelfIdentificationMsg::decode(body).map(Self::SelfIdentification)
            }
            ClientAddEntryMsg::TYPE_INDEX => {
                ClientAddEntryMsg::decode(body).map(Self::ClientAddEntry)
            }
            ReceivePlayerSpawnPointMsg::TYPE_INDEX => {
                ReceivePlayerSpawnPointMsg::decode(body).map(Self::PlayerSpawnPoint)
            }
            TimeSynchMsg::TYPE_INDEX => TimeSynchMsg::decode(body).map(Self::TimeSynch),
            SyncTimeChangeMsg::TYPE_INDEX => SyncTimeChangeMsg::decode(body).map(Self::TimeChange),
            SyncDayPhaseMsg::TYPE_INDEX => SyncDayPhaseMsg::decode(body).map(Self::DayPhase),
            LevelInfoChangedMsg::TYPE_INDEX => {
                LevelInfoChangedMsg::decode(body).map(Self::LevelInfoChanged)
            }
            ReplicatedStateBundle::TYPE_INDEX => {
                ReplicatedStateBundle::decode(body).map(Self::Bundle)
            }
            DamageDealtMsg::TYPE_INDEX => DamageDealtMsg::decode(body).map(Self::DamageDealt),
            OnDamageMsg::TYPE_INDEX => OnDamageMsg::decode(body).map(Self::OnDamage),
            MeleeHitMsg::TYPE_INDEX => MeleeHitMsg::decode(body).map(Self::MeleeHit),
            AdjustFxScriptsMsg::TYPE_INDEX => {
                AdjustFxScriptsMsg::decode(body).map(Self::AdjustFxScripts)
            }
            RequestInitializedMsg::TYPE_INDEX => {
                RequestInitializedMsg::decode(body).map(Self::RequestInitialized)
            }
            RequestSuccessMsg::TYPE_INDEX => {
                RequestSuccessMsg::decode(body).map(Self::RequestSuccess)
            }
            DeathRecapMsg::TYPE_INDEX => DeathRecapMsg::decode(body).map(Self::DeathRecap),
            OnHealedMsg::TYPE_INDEX => OnHealedMsg::decode(body).map(Self::OnHealed),
            UpdatePhasingMsg::TYPE_INDEX => UpdatePhasingMsg::decode(body).map(Self::UpdatePhasing),
            CutsceneGrantedMsg::TYPE_INDEX => {
                CutsceneGrantedMsg::decode(body).map(Self::CutsceneGranted)
            }
            SpreadshotProcessedMsg::TYPE_INDEX => {
                SpreadshotProcessedMsg::decode(body).map(Self::SpreadshotProcessed)
            }
            CutsceneEndFinalizedMsg::TYPE_INDEX => {
                CutsceneEndFinalizedMsg::decode(body).map(Self::CutsceneEndFinalized)
            }
            TeleportWithLoadingScreenMsg::TYPE_INDEX => {
                TeleportWithLoadingScreenMsg::decode(body).map(Self::TeleportWithLoadingScreen)
            }
            StorageSearchItemsMsg::TYPE_INDEX => {
                StorageSearchItemsMsg::decode(body).map(Self::StorageSearchItems)
            }
            StorageSearchResultsMsg::TYPE_INDEX => {
                StorageSearchResultsMsg::decode(body).map(Self::StorageSearchResults)
            }
            LoadoutStatusMsg::TYPE_INDEX => LoadoutStatusMsg::decode(body).map(Self::LoadoutStatus),
            InteractionValidatedMsg::TYPE_INDEX => {
                InteractionValidatedMsg::decode(body).map(Self::InteractionValidated)
            }
            InteractionEndedMsg::TYPE_INDEX => {
                InteractionEndedMsg::decode(body).map(Self::InteractionEnded)
            }
            GatheringInterruptedMsg::TYPE_INDEX => {
                GatheringInterruptedMsg::decode(body).map(Self::GatheringInterrupted)
            }
            PlayerSeenByAiMsg::TYPE_INDEX => {
                PlayerSeenByAiMsg::decode(body).map(Self::PlayerSeenByAi)
            }
            SelectedAsTargetMsg::TYPE_INDEX => {
                SelectedAsTargetMsg::decode(body).map(Self::SelectedAsTarget)
            }
            HomingOverrideMsg::TYPE_INDEX => {
                HomingOverrideMsg::decode(body).map(Self::HomingOverride)
            }
            LeftGameModeQueueMsg::TYPE_INDEX => {
                LeftGameModeQueueMsg::decode(body).map(Self::LeftGameModeQueue)
            }
            UnselectedAsTargetMsg::TYPE_INDEX => {
                UnselectedAsTargetMsg::decode(body).map(Self::UnselectedAsTarget)
            }
            DisableCollisionMsg::TYPE_INDEX => {
                DisableCollisionMsg::decode(body).map(Self::DisableCollision)
            }
            CraftProgressMsg::TYPE_INDEX => CraftProgressMsg::decode(body).map(Self::CraftProgress),
            StartWhisperMsg::TYPE_INDEX => StartWhisperMsg::decode(body).map(Self::StartWhisper),
            GameModeSchedulerRefreshedMsg::TYPE_INDEX => {
                GameModeSchedulerRefreshedMsg::decode(body).map(Self::GameModeSchedulerRefreshed)
            }
            JoinableSubArmiesMsg::TYPE_INDEX => {
                JoinableSubArmiesMsg::decode(body).map(Self::JoinableSubArmies)
            }
            PendingMetaAchievementsMsg::TYPE_INDEX => {
                PendingMetaAchievementsMsg::decode(body).map(Self::PendingMetaAchievements)
            }
            LobbyCountsMsg::TYPE_INDEX => LobbyCountsMsg::decode(body).map(Self::LobbyCounts),
            SyncNotificationsMsg::TYPE_INDEX => {
                SyncNotificationsMsg::decode(body).map(Self::SyncNotifications)
            }
            SendNotificationMsg::TYPE_INDEX => {
                SendNotificationMsg::decode(body).map(Self::SendNotification)
            }
            SendNotificationWithReplacementsMsg::TYPE_INDEX => {
                SendNotificationWithReplacementsMsg::decode(body)
                    .map(Self::SendNotificationWithReplacements)
            }
            AvailableTitlesMsg::TYPE_INDEX => {
                AvailableTitlesMsg::decode(body).map(Self::AvailableTitles)
            }
            TitlesUnlockMsg::TYPE_INDEX => TitlesUnlockMsg::decode(body).map(Self::TitlesUnlock),
            StorageSearchReadyMsg::TYPE_INDEX => {
                StorageSearchReadyMsg::decode(body).map(Self::StorageSearchReady)
            }
            index if GameModeServerMsg::is_type(index) => {
                GameModeServerMsg::decode(index, body).map(Self::GameMode)
            }
            index if WarServerMsg::is_type(index) => {
                WarServerMsg::decode(index, body).map(Self::War)
            }
            index if WarSignupServerMsg::is_type(index) => {
                WarSignupServerMsg::decode(index, body).map(Self::WarSignup)
            }
            index if CombatEffectMsg::is_type(index) => {
                CombatEffectMsg::decode(index, body).map(Self::CombatEffect)
            }
            PlayerDataResponseMsg::TYPE_INDEX => {
                PlayerDataResponseMsg::decode(body).map(Self::PlayerData)
            }
            UnorderedGuildsMsg::TYPE_INDEX => {
                UnorderedGuildsMsg::decode(body).map(Self::UnorderedGuilds)
            }
            RequestFailedMsg::TYPE_INDEX => RequestFailedMsg::decode(body).map(Self::RequestFailed),
            KickClientMsg::TYPE_INDEX => KickClientMsg::decode(body).map(Self::KickClient),
            PlayerManagerRejectedMsg::TYPE_INDEX => {
                PlayerManagerRejectedMsg::decode(body).map(Self::PlayerManagerRejected)
            }
            SyncExecuteEntitiesMsg::TYPE_INDEX => {
                SyncExecuteEntitiesMsg::decode(body).map(Self::SyncExecuteEntities)
            }
            MasteryTreeRespecResponseMsg::TYPE_INDEX => {
                MasteryTreeRespecResponseMsg::decode(body).map(Self::MasteryTreeRespecResponse)
            }
            MetaAchievementSummaryMsg::TYPE_INDEX => {
                MetaAchievementSummaryMsg::decode(body).map(Self::MetaAchievementSummary)
            }
            MetaAchievementProgressMsg::TYPE_INDEX => {
                MetaAchievementProgressMsg::decode(body).map(Self::MetaAchievementProgress)
            }
            MetaProgressChangedMsg::TYPE_INDEX => {
                MetaProgressChangedMsg::decode(body).map(Self::MetaProgressChanged)
            }
            index if TravelServerMsg::is_type(index) => {
                TravelServerMsg::decode(index, body).map(Self::Travel)
            }
            index if ArenaServerMsg::is_type(index) => {
                ArenaServerMsg::decode(index, body).map(Self::Arena)
            }
            index if FishingServerMsg::is_type(index) => {
                FishingServerMsg::decode(index, body).map(Self::Fishing)
            }
            index if MusicServerMsg::is_type(index) => {
                MusicServerMsg::decode(index, body).map(Self::Music)
            }
            index if TransmogServerMsg::is_type(index) => {
                TransmogServerMsg::decode(index, body).map(Self::Transmog)
            }
            index if InventoriesMsg::is_type(index) => {
                InventoriesMsg::decode(index, body).map(Self::Inventories)
            }
            _ => return None,
        };
        Some(message)
    }

    /// The type index of this message.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::TimingConfigurationConnected(_) => TimingConfigurationConnectedMsg::TYPE_INDEX,
            Self::CalendarConnected(_) => CalendarConnectedMsg::TYPE_INDEX,
            Self::SelfIdentification(_) => PlayerManagerSelfIdentificationMsg::TYPE_INDEX,
            Self::ClientAddEntry(_) => ClientAddEntryMsg::TYPE_INDEX,
            Self::PlayerSpawnPoint(_) => ReceivePlayerSpawnPointMsg::TYPE_INDEX,
            Self::TimeSynch(_) => TimeSynchMsg::TYPE_INDEX,
            Self::TimeChange(_) => SyncTimeChangeMsg::TYPE_INDEX,
            Self::DayPhase(_) => SyncDayPhaseMsg::TYPE_INDEX,
            Self::LevelInfoChanged(_) => LevelInfoChangedMsg::TYPE_INDEX,
            Self::Bundle(_) => ReplicatedStateBundle::TYPE_INDEX,
            Self::DamageDealt(_) => DamageDealtMsg::TYPE_INDEX,
            Self::OnDamage(_) => OnDamageMsg::TYPE_INDEX,
            Self::MeleeHit(_) => MeleeHitMsg::TYPE_INDEX,
            Self::AdjustFxScripts(_) => AdjustFxScriptsMsg::TYPE_INDEX,
            Self::RequestInitialized(_) => RequestInitializedMsg::TYPE_INDEX,
            Self::RequestSuccess(_) => RequestSuccessMsg::TYPE_INDEX,
            Self::DeathRecap(_) => DeathRecapMsg::TYPE_INDEX,
            Self::OnHealed(_) => OnHealedMsg::TYPE_INDEX,
            Self::UpdatePhasing(_) => UpdatePhasingMsg::TYPE_INDEX,
            Self::CutsceneGranted(_) => CutsceneGrantedMsg::TYPE_INDEX,
            Self::CutsceneEndFinalized(_) => CutsceneEndFinalizedMsg::TYPE_INDEX,
            Self::SpreadshotProcessed(_) => SpreadshotProcessedMsg::TYPE_INDEX,
            Self::TeleportWithLoadingScreen(_) => TeleportWithLoadingScreenMsg::TYPE_INDEX,
            Self::StorageSearchItems(_) => StorageSearchItemsMsg::TYPE_INDEX,
            Self::StorageSearchResults(_) => StorageSearchResultsMsg::TYPE_INDEX,
            Self::LoadoutStatus(_) => LoadoutStatusMsg::TYPE_INDEX,
            Self::InteractionValidated(_) => InteractionValidatedMsg::TYPE_INDEX,
            Self::InteractionEnded(_) => InteractionEndedMsg::TYPE_INDEX,
            Self::GatheringInterrupted(_) => GatheringInterruptedMsg::TYPE_INDEX,
            Self::PlayerSeenByAi(_) => PlayerSeenByAiMsg::TYPE_INDEX,
            Self::SelectedAsTarget(_) => SelectedAsTargetMsg::TYPE_INDEX,
            Self::HomingOverride(_) => HomingOverrideMsg::TYPE_INDEX,
            Self::LeftGameModeQueue(_) => LeftGameModeQueueMsg::TYPE_INDEX,
            Self::UnselectedAsTarget(_) => UnselectedAsTargetMsg::TYPE_INDEX,
            Self::DisableCollision(_) => DisableCollisionMsg::TYPE_INDEX,
            Self::CraftProgress(_) => CraftProgressMsg::TYPE_INDEX,
            Self::StartWhisper(_) => StartWhisperMsg::TYPE_INDEX,
            Self::GameModeSchedulerRefreshed(_) => GameModeSchedulerRefreshedMsg::TYPE_INDEX,
            Self::Economy(message) => message.type_index(),
            Self::JoinableSubArmies(_) => JoinableSubArmiesMsg::TYPE_INDEX,
            Self::PendingMetaAchievements(_) => PendingMetaAchievementsMsg::TYPE_INDEX,
            Self::LobbyCounts(_) => LobbyCountsMsg::TYPE_INDEX,
            Self::SyncNotifications(_) => SyncNotificationsMsg::TYPE_INDEX,
            Self::SendNotification(_) => SendNotificationMsg::TYPE_INDEX,
            Self::SendNotificationWithReplacements(_) => {
                SendNotificationWithReplacementsMsg::TYPE_INDEX
            }
            Self::Travel(message) => message.type_index(),
            Self::Arena(message) => message.type_index(),
            Self::AvailableTitles(_) => AvailableTitlesMsg::TYPE_INDEX,
            Self::PlayerData(_) => PlayerDataResponseMsg::TYPE_INDEX,
            Self::UnorderedGuilds(_) => UnorderedGuildsMsg::TYPE_INDEX,
            Self::RequestFailed(_) => RequestFailedMsg::TYPE_INDEX,
            Self::KickClient(_) => KickClientMsg::TYPE_INDEX,
            Self::PlayerManagerRejected(_) => PlayerManagerRejectedMsg::TYPE_INDEX,
            Self::SyncExecuteEntities(_) => SyncExecuteEntitiesMsg::TYPE_INDEX,
            Self::MasteryTreeRespecResponse(_) => MasteryTreeRespecResponseMsg::TYPE_INDEX,
            Self::MetaAchievementSummary(_) => MetaAchievementSummaryMsg::TYPE_INDEX,
            Self::MetaAchievementProgress(_) => MetaAchievementProgressMsg::TYPE_INDEX,
            Self::MetaProgressChanged(_) => MetaProgressChangedMsg::TYPE_INDEX,
            Self::Fishing(message) => message.type_index(),
            Self::Music(message) => message.type_index(),
            Self::Transmog(message) => message.type_index(),
            Self::Inventories(message) => message.type_index(),
            Self::GameMode(message) => message.type_index(),
            Self::War(message) => message.type_index(),
            Self::WarSignup(message) => message.type_index(),
            Self::CombatEffect(message) => message.type_index(),
            Self::TitlesUnlock(_) => TitlesUnlockMsg::TYPE_INDEX,
            Self::StorageSearchReady(_) => StorageSearchReadyMsg::TYPE_INDEX,
        }
    }

    /// The whole envelope: header, type index and body.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a value does not fit its field.
    pub fn envelope(&self) -> Result<Vec<u8>, MessageError> {
        Ok(Envelope::message(TypeId::FileSlot(self.type_index()), self.encode()?).encode()?)
    }

    /// The message in an envelope; `None` for an envelope of another type.
    pub fn from_envelope(envelope: &Envelope) -> Option<Result<Self, MessageError>> {
        Self::decode(envelope.type_index()?, &envelope.body)
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a value does not fit its field.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        match self {
            Self::TimingConfigurationConnected(message) => message.encode(),
            Self::CalendarConnected(message) => message.encode(),
            Self::SelfIdentification(message) => message.encode(),
            Self::ClientAddEntry(message) => message.encode(),
            Self::PlayerSpawnPoint(message) => message.encode(),
            Self::TimeSynch(message) => message.encode(),
            Self::TimeChange(message) => message.encode(),
            Self::DayPhase(message) => message.encode(),
            Self::LevelInfoChanged(message) => message.encode(),
            Self::Bundle(message) => message.encode(),
            Self::DamageDealt(message) => message.encode(),
            Self::OnDamage(message) => message.encode(),
            Self::MeleeHit(message) => message.encode(),
            Self::AdjustFxScripts(message) => message.encode(),
            Self::RequestInitialized(message) => message.encode(),
            Self::RequestSuccess(message) => message.encode(),
            Self::DeathRecap(message) => message.encode(),
            Self::OnHealed(message) => message.encode(),
            Self::UpdatePhasing(message) => message.encode(),
            Self::CutsceneGranted(message) => message.encode(),
            Self::CutsceneEndFinalized(message) => message.encode(),
            Self::SpreadshotProcessed(message) => message.encode(),
            Self::TeleportWithLoadingScreen(message) => message.encode(),
            Self::StorageSearchItems(message) => message.encode(),
            Self::StorageSearchResults(message) => message.encode(),
            Self::LoadoutStatus(message) => message.encode(),
            Self::InteractionValidated(message) => message.encode(),
            Self::InteractionEnded(message) => message.encode(),
            Self::GatheringInterrupted(message) => message.encode(),
            Self::PlayerSeenByAi(message) => message.encode(),
            Self::SelectedAsTarget(message) => message.encode(),
            Self::HomingOverride(message) => message.encode(),
            Self::LeftGameModeQueue(message) => message.encode(),
            Self::UnselectedAsTarget(message) => message.encode(),
            Self::DisableCollision(message) => message.encode(),
            Self::CraftProgress(message) => message.encode(),
            Self::StartWhisper(message) => message.encode(),
            Self::GameModeSchedulerRefreshed(message) => message.encode(),
            Self::Economy(message) => message.encode(),
            Self::JoinableSubArmies(message) => message.encode(),
            Self::PendingMetaAchievements(message) => message.encode(),
            Self::LobbyCounts(message) => message.encode(),
            Self::SyncNotifications(message) => message.encode(),
            Self::SendNotification(message) => message.encode(),
            Self::SendNotificationWithReplacements(message) => message.encode(),
            Self::Travel(message) => Ok(message.encode()),
            Self::Arena(message) => Ok(message.encode()),
            Self::AvailableTitles(message) => message.encode(),
            Self::PlayerData(message) => message.encode(),
            Self::UnorderedGuilds(message) => message.encode(),
            Self::RequestFailed(message) => message.encode(),
            Self::KickClient(message) => message.encode(),
            Self::PlayerManagerRejected(message) => message.encode(),
            Self::SyncExecuteEntities(message) => message.encode(),
            Self::MasteryTreeRespecResponse(message) => message.encode(),
            Self::MetaAchievementSummary(message) => message.encode(),
            Self::MetaAchievementProgress(message) => message.encode(),
            Self::MetaProgressChanged(message) => message.encode(),
            Self::Fishing(message) => Ok(message.encode()),
            Self::Music(message) => Ok(message.encode()),
            Self::Transmog(message) => Ok(message.encode()),
            Self::Inventories(message) => message.encode(),
            Self::GameMode(message) => message.encode(),
            Self::War(message) => Ok(message.encode()),
            Self::WarSignup(message) => message.encode(),
            Self::CombatEffect(message) => Ok(message.encode()),
            Self::TitlesUnlock(message) => message.encode(),
            Self::StorageSearchReady(message) => message.encode(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{login_samples, time_samples};

    const ENTITY: u64 = 4;

    const OTHER_TYPE: u64 = 349;

    fn samples() -> Vec<ServerMessage> {
        let mut samples = login_samples(ENTITY);
        samples.extend(time_samples());
        samples.push(ServerMessage::Bundle(ReplicatedStateBundle {
            leading: None,
            byte_3: 0,
            byte_4: 1,
            flag_5: false,
            index_block: None,
            payload: vec![12; 3],
        }));
        samples
    }

    #[test]
    fn every_type_decodes_by_its_index_in_login_order() {
        let samples = samples();
        let order: Vec<u64> = samples.iter().map(ServerMessage::type_index).collect();
        assert_eq!(order, SERVER_MESSAGE_TYPES);
        assert_eq!(
            LOGIN_MESSAGE_TYPES.last(),
            Some(&LevelInfoChangedMsg::TYPE_INDEX)
        );
        for message in samples {
            let body = message.encode().expect("encode");
            assert_eq!(
                ServerMessage::decode(message.type_index(), &body),
                Some(Ok(message))
            );
        }
    }

    /// crc32 of `potionheal`.
    const POTION_HEAL_SCRIPT: u32 = 0xf7e1_e0d6;

    #[test]
    fn the_retail_potion_script_message_has_the_retail_bytes() {
        // Retail 608 (xp-potion-exit note, section 2.2): `d934888ec32a8b14 bfa96f3ea2e36a5a
        // f7e1e0d6 01`.
        let player = crate::entity_ids::number_of_facet(0xbfa9_6f3e_a2e3_6a5a);
        let body = AdjustFxScriptsMsg::on(player, POTION_HEAL_SCRIPT)
            .encode()
            .expect("encode");
        assert_eq!(
            body,
            [
                0xd9, 0x34, 0x88, 0x8e, 0xc3, 0x2a, 0x8b, 0x14, 0xbf, 0xa9, 0x6f, 0x3e, 0xa2, 0xe3,
                0x6a, 0x5a, 0xf7, 0xe1, 0xe0, 0xd6, 0x01
            ]
        );
        // Another retail player (facets note): header `33e9e69df0f79674 e113c90dd7307efa`.
        let other = AdjustFxScriptsMsg::on(
            crate::entity_ids::number_of_facet(0xe113_c90d_d730_7efa),
            POTION_HEAL_SCRIPT,
        );
        assert_eq!(other.header.target, 0x33e9_e69d_f0f7_9674);
        assert_eq!(other.header.entity, 0xe113_c90d_d730_7efa);
    }

    #[test]
    fn combat_messages_decode_by_their_index() {
        let samples = [
            ServerMessage::DamageDealt(DamageDealtMsg::hit(ENTITY, ENTITY + 1, 5.0, false)),
            ServerMessage::OnDamage(crate::test_support::melee_sample(ENTITY, ENTITY + 1, 5.0)),
            ServerMessage::DeathRecap(DeathRecapMsg::of(ENTITY)),
            ServerMessage::MeleeHit(MeleeHitMsg::on(ENTITY, true)),
            ServerMessage::AdjustFxScripts(AdjustFxScriptsMsg::on(ENTITY, POTION_HEAL_SCRIPT)),
        ];
        for message in samples {
            let body = message.encode().expect("encode");
            assert_eq!(
                ServerMessage::decode(message.type_index(), &body),
                Some(Ok(message))
            );
        }
    }

    #[test]
    fn a_message_round_trips_through_its_envelope() {
        for message in samples() {
            let bytes = message.envelope().expect("envelope");
            let envelope = Envelope::decode(&bytes).expect("decode");
            assert_eq!(ServerMessage::from_envelope(&envelope), Some(Ok(message)));
        }
    }

    #[test]
    fn travel_answers_decode_by_their_index() {
        let header = crate::FacetHeader::of(crate::travel::component::PLAYER_HOUSING, ENTITY);
        let samples = [
            TravelServerMsg::FastTravelCompleted { header },
            TravelServerMsg::FastTravelFailed { header },
            TravelServerMsg::UnstuckAccepted {
                header,
                delay_ns: 10_000_000_000,
            },
        ];
        for message in samples.map(ServerMessage::Travel) {
            let body = message.encode().expect("encode");
            assert_eq!(
                ServerMessage::decode(message.type_index(), &body),
                Some(Ok(message))
            );
        }
    }

    #[test]
    fn other_types_are_not_decoded() {
        assert_eq!(ServerMessage::decode(OTHER_TYPE, &[]), None);
    }
}
