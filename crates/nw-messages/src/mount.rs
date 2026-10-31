// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mounts: the player's mount state (`MB::MountComponentReplicatedState`, type 5620, player
//! component 39) and the facet calls of the mount component (mounts study, run 15:
//! `local-data/scratch-work/run15/mounts/REPORT.md`; 20 retail ledgers with mounts, 1,071,158
//! captured 5620 bodies, all of which this reader takes).
//!
//! The state has three slots (client constructor `FUN_1438d8100`, children in registration
//! order):
//! - slot 1, the owner's: `m_isMounted`, `m_summonCooldownEndTime` (Unix ns),
//!   `m_isServerForcingWalk`, `m_isInServerExclusionVolume`, `m_summonAuthorization` (a flag and
//!   the Unix ns it lasts to), `m_persistentMountData` (a full list of the mounts with a dye or a
//!   name), then the mount stamina `staminaCur`, `staminaMax`, `staminaRegenDelay` (seconds left),
//!   `staminaRegenRate`, `staminaDrainRate`, `multMaxStamina`, `multStaminaRegenRate`;
//! - slot 2: `m_mountId`, the CRC-32 of the lowercase `MountData` `MountId`;
//! - slot 3, what other players see: `mountRemoteFlags` and `remoteDyeData`.
//!
//! The facet calls go to the player's `MountComponent` ([`MOUNT_FACET`]); 6066 and 6392 go to its
//! `PlayerComponent`, 6026 to its `CraftingComponent`, 6065 to its
//! `PlayerQuickCourseComponent` ([`PLAYER_QUICK_COURSE_FACET`]).

use nw_reflect::{Envelope, TypeId};

use crate::facet::FacetHeader;
use crate::gear::Hands;
use crate::items::FullList;
use crate::paperdoll::{PaperdollPublic, SlotBits};
use crate::payload::{BundleMember, MemberState};
use crate::slots::{
    children_mask, read_children, read_slot_byte, write_children, write_known_children,
};
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `MB::MountComponentReplicatedState`.
pub const MOUNT_TYPE_INDEX: u64 = 5620;
/// The player component of the mount state (player component table).
pub const MOUNT_COMPONENT: u64 = 39;
/// `MountComponent` of the player: the target of every 5747, 5937, 5752, 2749, 5816, 5812,
/// 6731, 5969, 6068, 5619, 5751 and 5972 of the retail captures less the player entity.
pub const MOUNT_FACET: u64 = 0x567b_67cc_f3f7_2cdf;
/// `PlayerQuickCourseComponent` of the player: the target of every retail 6065 less the player
/// entity.
pub const PLAYER_QUICK_COURSE_FACET: u64 = 0x4f2f_d2db_ca8c_b221;

/// `MountComponentClientFacet_OnSummonMountPending`.
pub const SUMMON_PENDING_TYPE_INDEX: u64 = 5747;
/// `MountComponentClientFacet_ShowDismountNotification`.
pub const DISMOUNT_NOTIFICATION_TYPE_INDEX: u64 = 5937;
/// `MountComponentClientFacet_OnSummonMountCancelled`.
pub const SUMMON_CANCELLED_TYPE_INDEX: u64 = 5752;
/// `MountComponentClientFacet_OnEnableMounts`.
pub const ENABLE_MOUNTS_TYPE_INDEX: u64 = 2749;
/// `MountComponentClientFacet_ScheduleDismount`.
pub const SCHEDULE_DISMOUNT_TYPE_INDEX: u64 = 5816;
/// `MountComponentClientFacet_CancelScheduledDismount`.
pub const CANCEL_SCHEDULED_DISMOUNT_TYPE_INDEX: u64 = 5812;
/// `MountComponentClientFacet_OnMountTrampleNotification`.
pub const TRAMPLE_NOTIFICATION_TYPE_INDEX: u64 = 6731;
/// `MountComponentClientFacet_SendMountRenameNotification`.
pub const RENAME_NOTIFICATION_TYPE_INDEX: u64 = 5969;
/// `PlayerQuickCourseComponentClientFacet_OnQuickCourseEnded`.
pub const QUICK_COURSE_ENDED_TYPE_INDEX: u64 = 6065;
/// `MountComponentServerFacet_OnRequestCompleteSummon`.
pub const COMPLETE_SUMMON_TYPE_INDEX: u64 = 6068;
/// `MountComponentServerFacet_OnMountDashToggleChanged`.
pub const DASH_TOGGLE_TYPE_INDEX: u64 = 6268;
/// `MountComponentServerFacet_OnMountSlowWalkChanged`.
pub const SLOW_WALK_TYPE_INDEX: u64 = 6271;
/// `PlayerComponentServerFacet_RequestSetIsChangingMount`.
pub const CHANGING_MOUNT_TYPE_INDEX: u64 = 6066;
/// `PlayerComponentServerFacet_RequestSetIsInMountAttachmentMode`.
pub const ATTACHMENT_MODE_TYPE_INDEX: u64 = 6392;
/// `MountComponentServerFacet_OnRequestChangeMount`.
pub const CHANGE_MOUNT_TYPE_INDEX: u64 = 5619;
/// `MountComponentServerFacet_DismissMount`.
pub const DISMISS_MOUNT_TYPE_INDEX: u64 = 5751;
/// `MountComponentServerFacet_RenameMount`.
pub const RENAME_MOUNT_TYPE_INDEX: u64 = 5972;
/// `CraftingComponentServerFacet_InitiateMountDye`.
pub const MOUNT_DYE_TYPE_INDEX: u64 = 6026;

/// Slots of the class: base slot 0 and three added slots (player-component slot table).
const MOUNT_SLOTS: u8 = 4;
/// Slot-byte bit of the owner's slot 1.
const SLOT_1_BIT: u8 = 0b0010;
/// Slot-byte bit of slot 2, `m_mountId`.
const SLOT_2_BIT: u8 = 0b0100;
/// Slot-byte bit of slot 3, what other players see.
const SLOT_3_BIT: u8 = 0b1000;
/// Children of slot 1.
const SLOT_1_CHILDREN: u8 = 13;
/// Children of slot 2.
const SLOT_2_CHILDREN: u8 = 1;
/// Children of slot 3.
const SLOT_3_CHILDREN: u8 = 2;
const CHILD_MOUNTED: u8 = 0;
const CHILD_COOLDOWN_END: u8 = 1;
const CHILD_FORCING_WALK: u8 = 2;
const CHILD_EXCLUSION_VOLUME: u8 = 3;
const CHILD_AUTHORIZATION: u8 = 4;
const CHILD_PERSISTENT: u8 = 5;
const CHILD_STAMINA: u8 = 6;
const CHILD_STAMINA_MAX: u8 = 7;
const CHILD_REGEN_DELAY: u8 = 8;
const CHILD_REGEN_RATE: u8 = 9;
const CHILD_DRAIN_RATE: u8 = 10;
const CHILD_MULT_MAX: u8 = 11;
const CHILD_MULT_REGEN: u8 = 12;
const CHILD_MOUNT_ID: u8 = 0;
const CHILD_REMOTE_FLAGS: u8 = 0;
const CHILD_REMOTE_DYE: u8 = 1;
/// Longest name (the client's string reader `FUN_14087a3d0` limit).
const MAX_NAME_BYTES: u64 = 0x2_ffff;
/// Bytes of a dye: one color index per mask channel.
pub const DYE_CHANNELS: usize = 4;
/// The mount stamina and its maximum before the first mount-up (Observed: 100 in every retail
/// login card).
pub const LOGIN_STAMINA: f32 = 100.0;
/// The stamina regen and drain rates before the first mount-up (Observed: 1 in every retail login
/// card, 10 and 20 from the first mount-up on, the movement rows' rates).
pub const LOGIN_STAMINA_RATE: f32 = 1.0;
/// The stamina multipliers without status effects (Observed: 1 in 1,071,147 of 1,071,158 retail
/// bodies; a mount buff gave 1.3 for 3 s).
pub const NEUTRAL_MULTIPLIER: f32 = 1.0;
/// The paperdoll slots of the mount (client enum `PaperdollSlotTypes` 46 to 49: `MOUNT_BUFF_1`,
/// `MOUNT_BUFF_2`, `MOUNT_ATTACHMENT_1`, `MOUNT_ATTACHMENT_2`): in the active map while the
/// player rides (Observed: the retail own active map went from 24 and 27 to 24, 27 and 46 to 49
/// with each mount-up, and back with each dismount).
pub const MOUNT_PAPERDOLL_SLOTS: [u32; 4] = [46, 47, 48, 49];
/// Bytes after the dye of a 6026 (always 0 in the two retail calls; meaning Unknown).
const MOUNT_DYE_TAIL: usize = 5;
const FIELD: &str = "mount";

/// Why a player gets off its mount (client enum `DismountReason`, `FUN_143922ba0`; the names of
/// 1 and 9 read from the client image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismountReason {
    /// The player's own summon key or request.
    Voluntary = 0,
    /// The timer of a mount exclusion area ran out.
    Area = 1,
    /// Combat.
    Combat = 2,
    /// Death.
    Death = 3,
    /// Over the encumbrance limit.
    Encumbered = 4,
    /// A teleport.
    Teleport = 5,
    /// Fishing.
    Fishing = 6,
    /// A musical performance.
    MusicalPerformance = 7,
    /// An interaction.
    Interact = 8,
    /// A duel.
    Duel = 9,
    /// Housing.
    Housing = 10,
    /// Deep water.
    DeepWater = 11,
    /// Mounts disabled.
    Disabled = 12,
    /// Falling damage.
    FallingDamage = 13,
    /// Another mount was chosen.
    MountChanged = 14,
    /// The game mode.
    Gamemode = 15,
    /// A cutscene started.
    EnterCutscene = 16,
    /// The mount was destroyed.
    Destroyed = 17,
}

impl DismountReason {
    /// Every reason, by its value.
    const ALL: [Self; 18] = [
        Self::Voluntary,
        Self::Area,
        Self::Combat,
        Self::Death,
        Self::Encumbered,
        Self::Teleport,
        Self::Fishing,
        Self::MusicalPerformance,
        Self::Interact,
        Self::Duel,
        Self::Housing,
        Self::DeepWater,
        Self::Disabled,
        Self::FallingDamage,
        Self::MountChanged,
        Self::Gamemode,
        Self::EnterCutscene,
        Self::Destroyed,
    ];

    /// The reason of byte `value`.
    pub fn of(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }

    /// Whether the player asked for it: a voluntary dismount waits the mount type's
    /// `VoluntaryDismountCooldownMs`, every other one its `ForcedDismountCooldownMs`.
    pub const fn is_voluntary(self) -> bool {
        matches!(self, Self::Voluntary)
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let value = reader.u8()?;
        Self::of(value).ok_or(MessageError::BadTag {
            field: "dismount reason",
            tag: value,
        })
    }
}

/// Why a summon ended without a mount (client switch `FUN_1438f42f0` over the 5752 byte, which
/// picks the text `@ui_mount_summon_failed_*`; values without a text are not here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummonCancelReason {
    /// The player cancelled (retail: a crouch, a turn away; 6 of 8 retail cancels).
    PlayerCancelled = 1,
    /// A musical performance.
    MusicalPerformance = 2,
    /// Fishing.
    Fishing = 3,
    /// Death.
    Death = 4,
    /// A teleport.
    Teleport = 5,
    /// Combat (retail: 1 cancel).
    Combat = 6,
    /// An interaction.
    Interact = 7,
    /// Housing.
    Housing = 8,
    /// Over the encumbrance limit.
    Encumbered = 9,
    /// A war.
    War = 11,
    /// An invasion.
    Invasion = 12,
    /// A trial.
    Trial = 13,
    /// The summon cooldown runs.
    OnCooldown = 14,
    /// An area where mounts are off.
    RestrictedArea = 15,
    /// In the air.
    Midair = 16,
    /// The mount's type is not unlocked.
    TypeNotUnlocked = 17,
    /// Deep water.
    DeepWater = 18,
    /// The expansion is not owned.
    ExpansionNotOwned = 20,
    /// The expansion is owned but its requirements are not met (retail: the summon key before
    /// the mount quest's unlock).
    RequirementsNotMet = 21,
    /// No mount chosen.
    NoMountEquipped = 22,
    /// Mounts are disabled.
    MountDisabled = 23,
    /// Swimming.
    Swimming = 25,
}

impl SummonCancelReason {
    /// Every reason.
    const ALL: [Self; 22] = [
        Self::PlayerCancelled,
        Self::MusicalPerformance,
        Self::Fishing,
        Self::Death,
        Self::Teleport,
        Self::Combat,
        Self::Interact,
        Self::Housing,
        Self::Encumbered,
        Self::War,
        Self::Invasion,
        Self::Trial,
        Self::OnCooldown,
        Self::RestrictedArea,
        Self::Midair,
        Self::TypeNotUnlocked,
        Self::DeepWater,
        Self::ExpansionNotOwned,
        Self::RequirementsNotMet,
        Self::NoMountEquipped,
        Self::MountDisabled,
        Self::Swimming,
    ];

    /// The reason of byte `value`.
    pub fn of(value: u8) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|reason| *reason as u8 == value)
    }
}

/// `mountRemoteFlags`: what other players see of a rider (bits in the order of the client's
/// remote listeners `OnIsDashingChanged`, `OnDashVfxChanged`, `OnRemoteStaminaFullChanged`,
/// `OnRemoteStaminaEmptyChanged`, `FUN_144dd1050`; retail values 0x04 for 8,211 bodies, 0x05 at
/// a dash start at full stamina, then 0x07 and 0x03 once the mount ran, 0x09 at empty).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteFlags(pub u8);

impl RemoteFlags {
    /// It dashes.
    pub const DASHING: u8 = 0x01;
    /// The dash effect shows: it dashes on the move.
    pub const DASH_VFX: u8 = 0x02;
    /// Its stamina is full.
    pub const STAMINA_FULL: u8 = 0x04;
    /// Its stamina is empty.
    pub const STAMINA_EMPTY: u8 = 0x08;

    /// The flags of a rider that `dashing`, shows the dash effect when `vfx`, with stamina
    /// `stamina` of `max`.
    pub fn of(dashing: bool, vfx: bool, stamina: f32, max: f32) -> Self {
        let bit = |on: bool, bit: u8| if on { bit } else { 0 };
        Self(
            bit(dashing, Self::DASHING)
                | bit(vfx, Self::DASH_VFX)
                | bit(stamina >= max, Self::STAMINA_FULL)
                | bit(stamina <= 0.0, Self::STAMINA_EMPTY),
        )
    }
}

/// `m_summonAuthorization`: the client may complete a summon (6068) until `expires_ns`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummonAuthorization {
    /// The summon is authorized.
    pub authorized: bool,
    /// Unix ns until which it is (0 with no authorization).
    pub expires_ns: u64,
}

/// One mount's dye and name in `m_persistentMountData`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MountCustomization {
    /// The mount id.
    pub mount_id: u32,
    /// The color index of each mask channel, R, G, B, A (0 for none; Observed: the mounts whose
    /// `MountData` turns a channel off have 0 in that byte in all 1,224 retail dyes).
    pub dye: [u8; DYE_CHANNELS],
    /// The name the player gave it (empty for none).
    pub name: Vec<u8>,
}

impl MountCustomization {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            mount_id: reader.u32()?,
            dye: reader.array()?,
            name: read_name(reader)?,
        })
    }

    fn write(&self, writer: &mut Writer) {
        writer.u32(self.mount_id).bytes(&self.dye);
        write_name(writer, &self.name);
    }
}

/// `MB::MountComponentReplicatedState`: each child is `None` when it is not sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountState {
    /// Slot 1 child 0: the player rides.
    pub mounted: Option<bool>,
    /// Slot 1 child 1: Unix ns when the next summon may start.
    pub summon_cooldown_end_ns: Option<u64>,
    /// Slot 1 child 2: an area makes the mount walk.
    pub server_forcing_walk: Option<bool>,
    /// Slot 1 child 3: the player is in a mount exclusion area.
    pub in_exclusion_volume: Option<bool>,
    /// Slot 1 child 4: the summon the client may complete.
    pub summon_authorization: Option<SummonAuthorization>,
    /// Slot 1 child 5: the mounts with a dye or a name.
    pub persistent: Option<FullList<MountCustomization>>,
    /// Slot 1 child 6: the mount stamina.
    pub stamina: Option<f32>,
    /// Slot 1 child 7: the mount stamina's maximum.
    pub stamina_max: Option<f32>,
    /// Slot 1 child 8: seconds left before the stamina regenerates.
    pub regen_delay: Option<f32>,
    /// Slot 1 child 9: stamina regenerated per second.
    pub regen_rate: Option<f32>,
    /// Slot 1 child 10: stamina a dash drains per second.
    pub drain_rate: Option<f32>,
    /// Slot 1 child 11: the multiplier of the maximum.
    pub max_multiplier: Option<f32>,
    /// Slot 1 child 12: the multiplier of the regen rate.
    pub regen_multiplier: Option<f32>,
    /// Slot 2 child 0: the mount (the CRC-32 of its lowercase `MountId`; 0 for none).
    pub mount_id: Option<u32>,
    /// Slot 3 child 0: what other players see of the rider.
    pub remote_flags: Option<RemoteFlags>,
    /// Slot 3 child 1: the dye of the mount other players see.
    pub remote_dye: Option<[u8; DYE_CHANNELS]>,
}

impl MountState {
    /// Reads a body.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, sets slot 0 or a child that does not
    /// exist, or a list or flag does not read.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slots = read_slot_byte(reader, FIELD, MOUNT_SLOTS)?;
        if slots & !(SLOT_1_BIT | SLOT_2_BIT | SLOT_3_BIT) != 0 {
            return Err(MessageError::Unsized {
                field: FIELD,
                byte: slots,
            });
        }
        let mut state = Self::default();
        if slots & SLOT_1_BIT != 0 {
            read_children(reader, FIELD, SLOT_1_CHILDREN, |reader, child| {
                state.read_owner_child(reader, child)
            })?;
        }
        if slots & SLOT_2_BIT != 0 {
            read_children(reader, FIELD, SLOT_2_CHILDREN, |reader, _| {
                state.mount_id = Some(reader.u32()?);
                Ok(())
            })?;
        }
        if slots & SLOT_3_BIT != 0 {
            read_children(reader, FIELD, SLOT_3_CHILDREN, |reader, child| {
                match child {
                    CHILD_REMOTE_FLAGS => state.remote_flags = Some(RemoteFlags(reader.u8()?)),
                    CHILD_REMOTE_DYE..=u8::MAX => state.remote_dye = Some(reader.array()?),
                }
                Ok(())
            })?;
        }
        Ok(state)
    }

    fn read_owner_child(&mut self, reader: &mut Reader<'_>, child: u8) -> Result<(), MessageError> {
        match child {
            CHILD_MOUNTED => self.mounted = Some(reader.boolean()?),
            CHILD_COOLDOWN_END => self.summon_cooldown_end_ns = Some(reader.u64()?),
            CHILD_FORCING_WALK => self.server_forcing_walk = Some(reader.boolean()?),
            CHILD_EXCLUSION_VOLUME => self.in_exclusion_volume = Some(reader.boolean()?),
            CHILD_AUTHORIZATION => {
                self.summon_authorization = Some(SummonAuthorization {
                    authorized: reader.boolean()?,
                    expires_ns: reader.u64()?,
                });
            }
            CHILD_PERSISTENT => {
                self.persistent = Some(FullList::read(reader, MountCustomization::read)?);
            }
            CHILD_STAMINA => self.stamina = Some(reader.f32()?),
            CHILD_STAMINA_MAX => self.stamina_max = Some(reader.f32()?),
            CHILD_REGEN_DELAY => self.regen_delay = Some(reader.f32()?),
            CHILD_REGEN_RATE => self.regen_rate = Some(reader.f32()?),
            CHILD_DRAIN_RATE => self.drain_rate = Some(reader.f32()?),
            CHILD_MULT_MAX => self.max_multiplier = Some(reader.f32()?),
            CHILD_MULT_REGEN..=u8::MAX => self.regen_multiplier = Some(reader.f32()?),
        }
        Ok(())
    }

    /// The child bitmap of slot 1.
    fn owner_children(&self) -> u16 {
        children_mask(&[
            self.mounted.is_some(),
            self.summon_cooldown_end_ns.is_some(),
            self.server_forcing_walk.is_some(),
            self.in_exclusion_volume.is_some(),
            self.summon_authorization.is_some(),
            self.persistent.is_some(),
            self.stamina.is_some(),
            self.stamina_max.is_some(),
            self.regen_delay.is_some(),
            self.regen_rate.is_some(),
            self.drain_rate.is_some(),
            self.max_multiplier.is_some(),
            self.regen_multiplier.is_some(),
        ])
    }

    /// The child bitmap of slot 3.
    fn remote_children(&self) -> u16 {
        children_mask(&[self.remote_flags.is_some(), self.remote_dye.is_some()])
    }

    /// Whether it sends nothing.
    pub fn is_empty(&self) -> bool {
        self.owner_children() == 0 && self.mount_id.is_none() && self.remote_children() == 0
    }

    /// Writes the body: one slot byte, then each slot with a child to send.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the persistent list does not encode.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let owner = self.owner_children();
        let remote = self.remote_children();
        let bit = |on: bool, bit: u8| if on { bit } else { 0 };
        writer.u8(bit(owner != 0, SLOT_1_BIT)
            | bit(self.mount_id.is_some(), SLOT_2_BIT)
            | bit(remote != 0, SLOT_3_BIT));
        if owner != 0 {
            write_children(writer, owner, SLOT_1_CHILDREN, |writer, child| {
                self.write_owner_child(writer, child)
            })?;
        }
        if let Some(mount_id) = self.mount_id {
            write_known_children(writer, 1 << CHILD_MOUNT_ID, SLOT_2_CHILDREN, |writer, _| {
                writer.u32(mount_id);
            });
        }
        if remote != 0 {
            write_known_children(
                writer,
                remote,
                SLOT_3_CHILDREN,
                |writer, child| match child {
                    CHILD_REMOTE_FLAGS => {
                        self.remote_flags.map(|flags| writer.u8(flags.0));
                    }
                    CHILD_REMOTE_DYE..=u8::MAX => {
                        self.remote_dye.map(|dye| writer.bytes(&dye));
                    }
                },
            );
        }
        Ok(())
    }

    fn write_owner_child(&self, writer: &mut Writer, child: u8) -> Result<(), MessageError> {
        let f32_child = |writer: &mut Writer, value: Option<f32>| {
            value.map(|value| writer.f32(value));
        };
        match child {
            CHILD_MOUNTED => {
                self.mounted.map(|mounted| writer.boolean(mounted));
            }
            CHILD_COOLDOWN_END => {
                self.summon_cooldown_end_ns.map(|end| writer.u64(end));
            }
            CHILD_FORCING_WALK => {
                self.server_forcing_walk.map(|walk| writer.boolean(walk));
            }
            CHILD_EXCLUSION_VOLUME => {
                self.in_exclusion_volume
                    .map(|inside| writer.boolean(inside));
            }
            CHILD_AUTHORIZATION => {
                self.summon_authorization.map(|authorization| {
                    writer
                        .boolean(authorization.authorized)
                        .u64(authorization.expires_ns)
                });
            }
            CHILD_PERSISTENT => {
                if let Some(list) = &self.persistent {
                    list.write(writer, |writer, customization| {
                        customization.write(writer);
                        Ok(())
                    })?;
                }
            }
            CHILD_STAMINA => f32_child(writer, self.stamina),
            CHILD_STAMINA_MAX => f32_child(writer, self.stamina_max),
            CHILD_REGEN_DELAY => f32_child(writer, self.regen_delay),
            CHILD_REGEN_RATE => f32_child(writer, self.regen_rate),
            CHILD_DRAIN_RATE => f32_child(writer, self.drain_rate),
            CHILD_MULT_MAX => f32_child(writer, self.max_multiplier),
            CHILD_MULT_REGEN..=u8::MAX => f32_child(writer, self.regen_multiplier),
        }
        Ok(())
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the persistent list does not encode.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.write(&mut writer)?;
        Ok(writer.finish())
    }

    /// The state as the bundle member of the player's mount component.
    pub fn member(self) -> BundleMember {
        BundleMember {
            component: MOUNT_COMPONENT,
            state: MemberState::Mount(self),
        }
    }

    /// The first state of a player who has not ridden yet (the first 5620 of both retail new
    /// characters, and of every login before the first mount-up): on foot, no cooldown, no
    /// authorization, full stamina at the login rates, no mount chosen.
    pub fn login_card() -> Self {
        Self {
            mounted: Some(false),
            summon_cooldown_end_ns: Some(0),
            server_forcing_walk: Some(false),
            in_exclusion_volume: Some(false),
            summon_authorization: Some(SummonAuthorization::default()),
            persistent: None,
            stamina: Some(LOGIN_STAMINA),
            stamina_max: Some(LOGIN_STAMINA),
            regen_delay: Some(0.0),
            regen_rate: Some(LOGIN_STAMINA_RATE),
            drain_rate: Some(LOGIN_STAMINA_RATE),
            max_multiplier: Some(NEUTRAL_MULTIPLIER),
            regen_multiplier: Some(NEUTRAL_MULTIPLIER),
            mount_id: Some(0),
            remote_flags: None,
            remote_dye: None,
        }
    }
}

fn read_name(reader: &mut Reader<'_>) -> Result<Vec<u8>, MessageError> {
    let length = reader.wide_prefixed()?;
    if length > MAX_NAME_BYTES {
        return Err(MessageError::CountTooLarge {
            count: length,
            limit: MAX_NAME_BYTES,
        });
    }
    // The length is at most `MAX_NAME_BYTES`.
    Ok(reader.bytes(length as usize)?.to_vec())
}

fn write_name(writer: &mut Writer, name: &[u8]) {
    // A `usize` length fits 64 bits.
    writer.wide_prefixed(name.len() as u64).bytes(name);
}

/// The active map of a player who holds its weapons as `hands`: the hands' map, with the mount's
/// slots ([`MOUNT_PAPERDOLL_SLOTS`]) while it is `riding`.
pub fn riding_active_map(hands: Hands, riding: bool) -> SlotBits {
    let SlotBits(mut bits) = PaperdollPublic::of(&[], 0, hands)
        .active
        .unwrap_or_default();
    if riding {
        let SlotBits(mount) = SlotBits::of(MOUNT_PAPERDOLL_SLOTS);
        for (byte, mount) in bits.iter_mut().zip(mount) {
            *byte |= mount;
        }
    }
    SlotBits(bits)
}

/// Whether `name` fits the client's string reader.
pub fn name_fits(name: &[u8]) -> bool {
    // A `usize` length fits 64 bits.
    name.len() as u64 <= MAX_NAME_BYTES
}

/// A mount call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountClientMsg {
    /// 6068: the summon timer ended; the client asks to mount (retail: 13 ms after the summon
    /// authorization, 3,049 to 3,070 ms after the 5747).
    CompleteSummon {
        /// The header (the player's `MountComponent`).
        header: FacetHeader,
    },
    /// 6268: the dash key's mode: a toggle, or held (retail sends it at each entry).
    DashToggle {
        /// The header.
        header: FacetHeader,
        /// The dash key toggles the dash.
        toggle: bool,
    },
    /// 6271: the walk key's mount speed: the slow walk, or the walk (sent at each entry).
    SlowWalk {
        /// The header.
        header: FacetHeader,
        /// The walk key gives the slow walk.
        slow_walk: bool,
    },
    /// 6066: the mount choice window opened (1) or closed (0).
    ChangingMount {
        /// The header (the player's `PlayerComponent`).
        header: FacetHeader,
        /// The window is open.
        changing: bool,
    },
    /// 6392: the mount attachment view opened (1) or closed (0).
    AttachmentMode {
        /// The header (the player's `PlayerComponent`).
        header: FacetHeader,
        /// The view is open.
        active: bool,
    },
    /// 5619: the player chooses another mount.
    ChangeMount {
        /// The header.
        header: FacetHeader,
        /// The mount id.
        mount_id: u32,
    },
    /// 5751: the client gets off the mount (retail: `EnterCutscene` with the 6703 of a cutscene,
    /// 6 calls).
    DismissMount {
        /// The header.
        header: FacetHeader,
        /// Why.
        reason: DismountReason,
        /// A byte of unknown meaning (0 in all 6 retail calls).
        flag: u8,
    },
    /// 5972: the player names a mount.
    RenameMount {
        /// The header.
        header: FacetHeader,
        /// The mount id.
        mount_id: u32,
        /// The new name.
        name: Vec<u8>,
    },
    /// 6026: the player dyes a mount.
    MountDye {
        /// The header (the player's `CraftingComponent`).
        header: FacetHeader,
        /// The mount id.
        mount_id: u32,
        /// The color index of each channel, R, G, B, A.
        dye: [u8; DYE_CHANNELS],
        /// Bytes of unknown meaning (all 0 in the 2 retail calls).
        tail: [u8; MOUNT_DYE_TAIL],
    },
}

impl MountClientMsg {
    /// Decodes `body` when `type_index` is a mount call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            COMPLETE_SUMMON_TYPE_INDEX => read(|header, _| Ok(Self::CompleteSummon { header })),
            DASH_TOGGLE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DashToggle {
                    header,
                    toggle: reader.boolean()?,
                })
            }),
            SLOW_WALK_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SlowWalk {
                    header,
                    slow_walk: reader.boolean()?,
                })
            }),
            CHANGING_MOUNT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ChangingMount {
                    header,
                    changing: reader.boolean()?,
                })
            }),
            ATTACHMENT_MODE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::AttachmentMode {
                    header,
                    active: reader.boolean()?,
                })
            }),
            CHANGE_MOUNT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ChangeMount {
                    header,
                    mount_id: reader.u32()?,
                })
            }),
            DISMISS_MOUNT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DismissMount {
                    header,
                    reason: DismountReason::read(reader)?,
                    flag: reader.u8()?,
                })
            }),
            RENAME_MOUNT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RenameMount {
                    header,
                    mount_id: reader.u32()?,
                    name: read_name(reader)?,
                })
            }),
            MOUNT_DYE_TYPE_INDEX => read(|header, reader| {
                Ok(Self::MountDye {
                    header,
                    mount_id: reader.u32()?,
                    dye: reader.array()?,
                    tail: reader.array()?,
                })
            }),
            _ => return None,
        })
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::CompleteSummon { .. } => COMPLETE_SUMMON_TYPE_INDEX,
            Self::DashToggle { .. } => DASH_TOGGLE_TYPE_INDEX,
            Self::SlowWalk { .. } => SLOW_WALK_TYPE_INDEX,
            Self::ChangingMount { .. } => CHANGING_MOUNT_TYPE_INDEX,
            Self::AttachmentMode { .. } => ATTACHMENT_MODE_TYPE_INDEX,
            Self::ChangeMount { .. } => CHANGE_MOUNT_TYPE_INDEX,
            Self::DismissMount { .. } => DISMISS_MOUNT_TYPE_INDEX,
            Self::RenameMount { .. } => RENAME_MOUNT_TYPE_INDEX,
            Self::MountDye { .. } => MOUNT_DYE_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::CompleteSummon { header } => header.write(&mut writer),
            Self::DashToggle {
                header,
                toggle: flag,
            }
            | Self::SlowWalk {
                header,
                slow_walk: flag,
            }
            | Self::ChangingMount {
                header,
                changing: flag,
            }
            | Self::AttachmentMode {
                header,
                active: flag,
            } => {
                header.write(&mut writer);
                writer.boolean(*flag);
            }
            Self::ChangeMount { header, mount_id } => {
                header.write(&mut writer);
                writer.u32(*mount_id);
            }
            Self::DismissMount {
                header,
                reason,
                flag,
            } => {
                header.write(&mut writer);
                writer.u8(*reason as u8).u8(*flag);
            }
            Self::RenameMount {
                header,
                mount_id,
                name,
            } => {
                header.write(&mut writer);
                writer.u32(*mount_id);
                write_name(&mut writer, name);
            }
            Self::MountDye {
                header,
                mount_id,
                dye,
                tail,
            } => {
                header.write(&mut writer);
                writer.u32(*mount_id).bytes(dye).bytes(tail);
            }
        }
        writer.finish()
    }
}

/// A mount call of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountServerMsg {
    /// 5747: the summon started; it takes `summon_ms` (retail 3,000, the `MountTypeData`
    /// `SummonTimeMs`; 1,500 with a summon time bonus).
    SummonPending {
        /// The header (the player's `MountComponent`).
        header: FacetHeader,
        /// The summon time, ms.
        summon_ms: u32,
    },
    /// 5937: the player got off its mount; the client shows the reason's text when it has one.
    DismountNotification {
        /// The header.
        header: FacetHeader,
        /// Why.
        reason: DismountReason,
    },
    /// 5752: the summon ended without a mount.
    SummonCancelled {
        /// The header.
        header: FacetHeader,
        /// Why.
        reason: SummonCancelReason,
        /// A byte of unknown meaning (398 retail calls: 0 for a refusal at the summon key and
        /// a player's cancel, 1 for the server's cancel of a running summon).
        flag: u8,
        /// The client shows the reason's text (1 in all 398 retail calls; `FUN_1438f42f0`
        /// returns early on 0).
        notify: bool,
        /// A number of unknown meaning (398 retail calls: 1 with `OnCooldown`, 0 otherwise).
        detail: u32,
    },
    /// 2749: the player may use mounts (1 in all 23 retail calls).
    EnableMounts {
        /// The header.
        header: FacetHeader,
        /// Mounts are on.
        enabled: bool,
    },
    /// 5816: the server gets the player off its mount at `at_ns` (retail: `Area` 10 s ahead in
    /// an exclusion area, `FallingDamage` at once).
    ScheduleDismount {
        /// The header.
        header: FacetHeader,
        /// Unix ns of the dismount.
        at_ns: u64,
        /// Why.
        reason: DismountReason,
        /// A byte of unknown meaning (1 in all 4 retail calls).
        flag: u8,
    },
    /// 5812: the scheduled dismount is off (retail: the player left the exclusion area).
    CancelScheduledDismount {
        /// The header.
        header: FacetHeader,
    },
    /// 6731: the mount trampled something (no arguments).
    TrampleNotification {
        /// The header.
        header: FacetHeader,
    },
    /// 5969: the answer to a 5972.
    RenameNotification {
        /// The header.
        header: FacetHeader,
        /// The result (0 in both retail calls: the name was taken).
        result: u32,
        /// The mount id.
        mount_id: u32,
        /// The name.
        name: Vec<u8>,
    },
    /// 6065: a timed course (a mount race) ended.
    QuickCourseEnded {
        /// The header (the player's `PlayerQuickCourseComponent`).
        header: FacetHeader,
        /// The CRC-32 of the lowercase `QuickCourseId`.
        course: u32,
        /// The result (6 in all 42 retail calls).
        result: u8,
    },
}

impl MountServerMsg {
    /// Decodes `body` when `type_index` is a mount call of the server; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let message = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(message)
        };
        Some(match type_index {
            SUMMON_PENDING_TYPE_INDEX => read(|header, reader| {
                Ok(Self::SummonPending {
                    header,
                    summon_ms: reader.u32()?,
                })
            }),
            DISMOUNT_NOTIFICATION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::DismountNotification {
                    header,
                    reason: DismountReason::read(reader)?,
                })
            }),
            SUMMON_CANCELLED_TYPE_INDEX => read(|header, reader| {
                let value = reader.u8()?;
                Ok(Self::SummonCancelled {
                    header,
                    reason: SummonCancelReason::of(value).ok_or(MessageError::BadTag {
                        field: "summon cancel reason",
                        tag: value,
                    })?,
                    flag: reader.u8()?,
                    notify: reader.boolean()?,
                    detail: reader.u32()?,
                })
            }),
            ENABLE_MOUNTS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::EnableMounts {
                    header,
                    enabled: reader.boolean()?,
                })
            }),
            SCHEDULE_DISMOUNT_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ScheduleDismount {
                    header,
                    at_ns: reader.u64()?,
                    reason: DismountReason::read(reader)?,
                    flag: reader.u8()?,
                })
            }),
            CANCEL_SCHEDULED_DISMOUNT_TYPE_INDEX => {
                read(|header, _| Ok(Self::CancelScheduledDismount { header }))
            }
            TRAMPLE_NOTIFICATION_TYPE_INDEX => {
                read(|header, _| Ok(Self::TrampleNotification { header }))
            }
            RENAME_NOTIFICATION_TYPE_INDEX => read(|header, reader| {
                Ok(Self::RenameNotification {
                    header,
                    result: reader.u32()?,
                    mount_id: reader.u32()?,
                    name: read_name(reader)?,
                })
            }),
            QUICK_COURSE_ENDED_TYPE_INDEX => read(|header, reader| {
                Ok(Self::QuickCourseEnded {
                    header,
                    course: reader.u32()?,
                    result: reader.u8()?,
                })
            }),
            _ => return None,
        })
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::SummonPending { .. } => SUMMON_PENDING_TYPE_INDEX,
            Self::DismountNotification { .. } => DISMOUNT_NOTIFICATION_TYPE_INDEX,
            Self::SummonCancelled { .. } => SUMMON_CANCELLED_TYPE_INDEX,
            Self::EnableMounts { .. } => ENABLE_MOUNTS_TYPE_INDEX,
            Self::ScheduleDismount { .. } => SCHEDULE_DISMOUNT_TYPE_INDEX,
            Self::CancelScheduledDismount { .. } => CANCEL_SCHEDULED_DISMOUNT_TYPE_INDEX,
            Self::TrampleNotification { .. } => TRAMPLE_NOTIFICATION_TYPE_INDEX,
            Self::RenameNotification { .. } => RENAME_NOTIFICATION_TYPE_INDEX,
            Self::QuickCourseEnded { .. } => QUICK_COURSE_ENDED_TYPE_INDEX,
        }
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            Self::SummonPending { header, summon_ms } => {
                header.write(&mut writer);
                writer.u32(*summon_ms);
            }
            Self::DismountNotification { header, reason } => {
                header.write(&mut writer);
                writer.u8(*reason as u8);
            }
            Self::SummonCancelled {
                header,
                reason,
                flag,
                notify,
                detail,
            } => {
                header.write(&mut writer);
                writer
                    .u8(*reason as u8)
                    .u8(*flag)
                    .boolean(*notify)
                    .u32(*detail);
            }
            Self::EnableMounts { header, enabled } => {
                header.write(&mut writer);
                writer.boolean(*enabled);
            }
            Self::ScheduleDismount {
                header,
                at_ns,
                reason,
                flag,
            } => {
                header.write(&mut writer);
                writer.u64(*at_ns).u8(*reason as u8).u8(*flag);
            }
            Self::CancelScheduledDismount { header } | Self::TrampleNotification { header } => {
                header.write(&mut writer);
            }
            Self::RenameNotification {
                header,
                result,
                mount_id,
                name,
            } => {
                header.write(&mut writer);
                writer.u32(*result).u32(*mount_id);
                write_name(&mut writer, name);
            }
            Self::QuickCourseEnded {
                header,
                course,
                result,
            } => {
                header.write(&mut writer);
                writer.u32(*course).u8(*result);
            }
        }
        writer.finish()
    }

    /// The whole envelope: header, type index and body.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the envelope does not encode.
    pub fn envelope(&self) -> Result<Vec<u8>, MessageError> {
        Ok(Envelope::message(TypeId::FileSlot(self.type_index()), self.encode()).encode()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::FIRST_VERSION;

    /// A header (synthetic ids).
    const HEADER: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    /// `Mount_MTX_5_lion_basic`.
    const LION_BASIC: u32 = 0x0c13_84ae;
    /// `Mount_MTX_1_bear_Preorder`.
    const BEAR_PREORDER: u32 = 0xde1f_a300;
    /// A Unix ns time of the captures' day.
    const TIME_NS: u64 = 0x18d6_c39e_bf2c_1dc8;

    fn header() -> FacetHeader {
        FacetHeader::read(&mut Reader::new(&HEADER)).expect("header")
    }

    fn body(rest: &[u8]) -> Vec<u8> {
        let mut body = HEADER.to_vec();
        body.extend_from_slice(rest);
        body
    }

    fn state(bytes: &[u8]) -> MountState {
        let mut reader = Reader::new(bytes);
        let state = MountState::read(&mut reader).expect("read");
        reader.finish().expect("whole body");
        assert_eq!(state.encode().expect("encode"), bytes, "round trip");
        state
    }

    #[test]
    fn the_new_character_card_reads_and_encodes_back() {
        // The first 5620 of both retail new characters (player-components table).
        let card = [
            0x06, 0xdf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xc8, 0x00, 0x00, 0x3f, 0x42,
            0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00,
            0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        let read = state(&card);
        assert_eq!(read.mounted, Some(false));
        assert_eq!(
            read.summon_authorization,
            Some(SummonAuthorization::default())
        );
        assert_eq!(read.persistent, None);
        assert_eq!(read.stamina, Some(100.0));
        assert_eq!(read.regen_delay, Some(0.0));
        assert_eq!(read.regen_rate, Some(1.0));
        assert_eq!(read.mount_id, Some(0));
        assert_eq!(read.remote_flags, None);
    }

    #[test]
    fn the_retail_mounted_card_reads() {
        // A retail own card while riding: cooldown, authorization, regen 10 and drain 20.
        let mut card = vec![0x06, 0xdf, 0x01];
        card.extend(TIME_NS.to_be_bytes());
        card.extend([0x00, 0x00, 0x01]);
        card.extend((TIME_NS + 1).to_be_bytes());
        card.extend([0x42, 0xc8, 0x00, 0x00, 0x3f, 0x42, 0xc8, 0x00, 0x00]);
        card.extend([
            0x00, 0x00, 0x00, 0x00, 0x41, 0x20, 0x00, 0x00, 0x41, 0xa0, 0x00, 0x00,
        ]);
        card.extend([0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x01]);
        card.extend(LION_BASIC.to_be_bytes());
        let read = state(&card);
        assert_eq!(read.mounted, Some(true));
        assert_eq!(read.summon_cooldown_end_ns, Some(TIME_NS));
        assert_eq!(
            read.summon_authorization,
            Some(SummonAuthorization {
                authorized: true,
                expires_ns: TIME_NS + 1
            })
        );
        assert_eq!(read.regen_rate, Some(10.0));
        assert_eq!(read.drain_rate, Some(20.0));
        assert_eq!(read.mount_id, Some(LION_BASIC));
    }

    #[test]
    fn the_retail_deltas_read() {
        // Stamina alone (a dash), the regen delay alone (a continued child byte), the mount up,
        // the dismount, and another player's card and flags.
        assert_eq!(
            state(&[0x02, 0x40, 0x42, 0xc6, 0x00, 0x00]).stamina,
            Some(99.0)
        );
        assert_eq!(
            state(&[0x02, 0x80, 0x02, 0x3f, 0x80, 0x00, 0x00]).regen_delay,
            Some(1.0)
        );
        let up = state(&[
            0x02, 0x81, 0x01, 0x0c, 0x41, 0x20, 0x00, 0x00, 0x41, 0xa0, 0x00, 0x00,
        ]);
        assert_eq!(
            (up.mounted, up.regen_rate, up.drain_rate),
            (Some(true), Some(10.0), Some(20.0))
        );
        let mut down = vec![0x02, 0x13, 0x00];
        down.extend(TIME_NS.to_be_bytes());
        down.extend([0x00; 9]);
        let down = state(&down);
        assert_eq!(down.mounted, Some(false));
        assert_eq!(
            down.summon_authorization,
            Some(SummonAuthorization::default())
        );
        let remote = state(&[
            0x0c, 0x01, 0xde, 0x1f, 0xa3, 0x00, 0x03, 0x04, 0x19, 0x25, 0x19, 0x25,
        ]);
        assert_eq!(remote.mount_id, Some(BEAR_PREORDER));
        assert_eq!(
            remote.remote_flags,
            Some(RemoteFlags(RemoteFlags::STAMINA_FULL))
        );
        assert_eq!(remote.remote_dye, Some([0x19, 0x25, 0x19, 0x25]));
        assert_eq!(
            state(&[0x08, 0x01, 0x09]).remote_flags,
            Some(RemoteFlags(0x09))
        );
    }

    #[test]
    fn the_persistent_mount_data_is_a_full_list_of_dyes_and_names() {
        // Retail shape: list version, then each mount with its dye and name.
        let mut bytes = vec![0x02, 0x20, 0x00, 0x01, 0x07, 0x02];
        bytes.extend(LION_BASIC.to_be_bytes());
        bytes.extend([0x00, 0x00, 0x33, 0x00, 0x00]);
        bytes.extend(BEAR_PREORDER.to_be_bytes());
        bytes.extend([0x19, 0x25, 0x19, 0x25, 0x03, b'R', b'e', b'x']);
        let read = state(&bytes);
        let list = read.persistent.expect("list");
        assert_eq!(list.version, Some(7));
        assert_eq!(
            list.elements,
            [
                MountCustomization {
                    mount_id: LION_BASIC,
                    dye: [0, 0, 0x33, 0],
                    name: Vec::new(),
                },
                MountCustomization {
                    mount_id: BEAR_PREORDER,
                    dye: [0x19, 0x25, 0x19, 0x25],
                    name: b"Rex".to_vec(),
                },
            ]
        );
    }

    #[test]
    fn slot_0_and_unknown_children_are_refused() {
        assert!(MountState::read(&mut Reader::new(&[0x01, 0x01, 0x00])).is_err());
        assert!(MountState::read(&mut Reader::new(&[0x10])).is_err());
        assert!(MountState::read(&mut Reader::new(&[0x08, 0x04])).is_err());
    }

    #[test]
    fn a_state_becomes_the_mount_member_and_reads_back_from_a_bundle() {
        let state = MountState {
            mount_id: Some(LION_BASIC),
            persistent: Some(FullList {
                version: Some(FIRST_VERSION),
                elements: Vec::new(),
            }),
            ..MountState::default()
        };
        assert!(!state.is_empty());
        assert!(MountState::default().is_empty());
        let member = state.clone().member();
        assert_eq!(member.component, MOUNT_COMPONENT);
        assert_eq!(member.state.type_index(), MOUNT_TYPE_INDEX);
        let payload = crate::payload::BundlePayload {
            groups: vec![crate::payload::BundleGroup {
                interest: 1,
                members: vec![member],
            }],
        };
        let read = crate::payload::BundlePayload::decode(&payload.encode().expect("encode"))
            .expect("decode");
        assert_eq!(read.groups[0].members[0].state, MemberState::Mount(state));
    }

    #[test]
    fn the_login_card_is_the_retail_new_character_body() {
        let card = [
            0x06, 0xdf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xc8, 0x00, 0x00, 0x3f, 0x42,
            0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00,
            0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(MountState::login_card().encode().expect("encode"), card);
    }

    #[test]
    fn remote_flags_follow_the_dash_and_the_stamina() {
        assert_eq!(RemoteFlags::of(false, false, 100.0, 100.0).0, 0x04);
        assert_eq!(RemoteFlags::of(true, false, 100.0, 100.0).0, 0x05);
        assert_eq!(RemoteFlags::of(true, true, 50.0, 100.0).0, 0x03);
        assert_eq!(RemoteFlags::of(true, false, 0.0, 100.0).0, 0x09);
        assert_eq!(RemoteFlags::of(false, false, 0.0, 100.0).0, 0x08);
    }

    #[test]
    fn the_client_calls_read_their_retail_bodies() {
        let header = header();
        let cases = [
            (
                COMPLETE_SUMMON_TYPE_INDEX,
                Vec::new(),
                MountClientMsg::CompleteSummon { header },
            ),
            (
                DASH_TOGGLE_TYPE_INDEX,
                vec![0],
                MountClientMsg::DashToggle {
                    header,
                    toggle: false,
                },
            ),
            (
                SLOW_WALK_TYPE_INDEX,
                vec![0],
                MountClientMsg::SlowWalk {
                    header,
                    slow_walk: false,
                },
            ),
            (
                CHANGING_MOUNT_TYPE_INDEX,
                vec![1],
                MountClientMsg::ChangingMount {
                    header,
                    changing: true,
                },
            ),
            (
                ATTACHMENT_MODE_TYPE_INDEX,
                vec![0],
                MountClientMsg::AttachmentMode {
                    header,
                    active: false,
                },
            ),
            (
                CHANGE_MOUNT_TYPE_INDEX,
                LION_BASIC.to_be_bytes().to_vec(),
                MountClientMsg::ChangeMount {
                    header,
                    mount_id: LION_BASIC,
                },
            ),
            (
                DISMISS_MOUNT_TYPE_INDEX,
                vec![0x10, 0x00],
                MountClientMsg::DismissMount {
                    header,
                    reason: DismountReason::EnterCutscene,
                    flag: 0,
                },
            ),
            (
                RENAME_MOUNT_TYPE_INDEX,
                [&BEAR_PREORDER.to_be_bytes()[..], &[3, b'R', b'e', b'x'][..]].concat(),
                MountClientMsg::RenameMount {
                    header,
                    mount_id: BEAR_PREORDER,
                    name: b"Rex".to_vec(),
                },
            ),
            (
                MOUNT_DYE_TYPE_INDEX,
                [
                    &BEAR_PREORDER.to_be_bytes()[..],
                    &[0x19, 0x25, 0x19, 0x25, 0, 0, 0, 0, 0][..],
                ]
                .concat(),
                MountClientMsg::MountDye {
                    header,
                    mount_id: BEAR_PREORDER,
                    dye: [0x19, 0x25, 0x19, 0x25],
                    tail: [0; MOUNT_DYE_TAIL],
                },
            ),
        ];
        for (type_index, rest, expected) in cases {
            let bytes = body(&rest);
            assert_eq!(
                MountClientMsg::decode(type_index, &bytes),
                Some(Ok(expected.clone()))
            );
            assert_eq!(expected.encode(), bytes);
            assert_eq!(expected.type_index(), type_index);
        }
        assert_eq!(
            MountClientMsg::decode(SUMMON_PENDING_TYPE_INDEX, &HEADER),
            None
        );
        assert!(matches!(
            MountClientMsg::decode(DISMISS_MOUNT_TYPE_INDEX, &body(&[0x40, 0x00])),
            Some(Err(_))
        ));
    }

    #[test]
    fn the_server_calls_have_the_retail_bodies() {
        let header = header();
        let cases = [
            (
                MountServerMsg::SummonPending {
                    header,
                    summon_ms: 3000,
                },
                vec![0x00, 0x00, 0x0b, 0xb8],
            ),
            (
                MountServerMsg::DismountNotification {
                    header,
                    reason: DismountReason::DeepWater,
                },
                vec![0x0b],
            ),
            (
                MountServerMsg::SummonCancelled {
                    header,
                    reason: SummonCancelReason::RequirementsNotMet,
                    flag: 0,
                    notify: true,
                    detail: 0,
                },
                vec![0x15, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00],
            ),
            (
                MountServerMsg::EnableMounts {
                    header,
                    enabled: true,
                },
                vec![0x01],
            ),
            (
                MountServerMsg::ScheduleDismount {
                    header,
                    at_ns: TIME_NS,
                    reason: DismountReason::Area,
                    flag: 1,
                },
                [&TIME_NS.to_be_bytes()[..], &[0x01, 0x01][..]].concat(),
            ),
            (
                MountServerMsg::CancelScheduledDismount { header },
                Vec::new(),
            ),
            (MountServerMsg::TrampleNotification { header }, Vec::new()),
            (
                MountServerMsg::RenameNotification {
                    header,
                    result: 0,
                    mount_id: BEAR_PREORDER,
                    name: b"Rex".to_vec(),
                },
                [
                    &[0, 0, 0, 0][..],
                    &BEAR_PREORDER.to_be_bytes()[..],
                    &[3, b'R', b'e', b'x'][..],
                ]
                .concat(),
            ),
            (
                MountServerMsg::QuickCourseEnded {
                    header,
                    course: 0x02e0_a7ba,
                    result: 6,
                },
                vec![0x02, 0xe0, 0xa7, 0xba, 0x06],
            ),
        ];
        for (message, rest) in cases {
            let bytes = body(&rest);
            assert_eq!(message.encode(), bytes);
            assert_eq!(
                MountServerMsg::decode(message.type_index(), &bytes),
                Some(Ok(message.clone()))
            );
            let envelope = Envelope::decode(&message.envelope().expect("envelope")).expect("read");
            assert_eq!(envelope.type_index(), Some(message.type_index()));
            assert_eq!(envelope.body, bytes);
        }
    }

    #[test]
    fn reasons_have_the_client_values() {
        assert_eq!(DismountReason::of(11), Some(DismountReason::DeepWater));
        assert_eq!(DismountReason::of(17), Some(DismountReason::Destroyed));
        assert_eq!(DismountReason::of(18), None);
        assert!(DismountReason::Voluntary.is_voluntary());
        assert!(!DismountReason::DeepWater.is_voluntary());
        assert_eq!(
            SummonCancelReason::of(1),
            Some(SummonCancelReason::PlayerCancelled)
        );
        assert_eq!(
            SummonCancelReason::of(25),
            Some(SummonCancelReason::Swimming)
        );
        assert_eq!(SummonCancelReason::of(10), None);
    }

    #[test]
    fn a_rider_s_active_map_holds_the_mount_slots() {
        // Retail own active maps: 24 and 27 on foot, with 46 to 49 riding.
        let hands = Hands::default();
        assert_eq!(
            riding_active_map(hands, false),
            SlotBits([0, 0, 0, 0x09, 0, 0, 0])
        );
        assert_eq!(
            riding_active_map(hands, true),
            SlotBits([0, 0, 0, 0x09, 0, 0xc0, 0x03])
        );
    }

    #[test]
    fn a_name_longer_than_the_client_reads_does_not_fit() {
        assert!(name_fits(b"Rex"));
        assert!(!name_fits(&vec![b'a'; 0x3_0000]));
    }
}
