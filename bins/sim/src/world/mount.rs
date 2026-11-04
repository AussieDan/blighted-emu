// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mounts (mounts study, run 15: `local-data/scratch-work/run15/mounts/REPORT.md`; 20 retail
//! ledgers with mounts): the summon, the ride and the dismount, the mount stamina, the chosen
//! mount and its dye and name, and what other players see of a rider.
//!
//! The retail flow (Mount-quest capture, 13 summons and 13 dismounts):
//! - The summon key (input 26) starts a summon: the server sends 5747 with the type's
//!   `SummonTimeMs`, then the chosen mount in the own state's `m_mountId`. When the summon time
//!   is over it authorizes the summon (`m_summonAuthorization`: 1 and 8 s after the start); the
//!   client then asks to mount (6068, 13 ms later) and the server sets `m_isMounted` with the
//!   mount's stamina rates. A summon key while riding gets the player off: 5937 with the reason,
//!   `m_isMounted` 0, the authorization cleared and the summon cooldown 1 s ahead (the type's
//!   dismount cooldown). A refused or cancelled summon gets 5752 with its reason.
//! - The mount stamina drains while the rider dashes on the move (the dash key is the dodge key,
//!   held or toggled as 6268 says) and regenerates after its delay; at 0 the dash ends and the
//!   delay starts again (retail regen delay 1.0 then 0.967, 0.933 ... per tick).
//! - Deep water gets the rider off (5937 `DeepWater`); a cutscene makes the client ask (5751).
//!
//! The server's body of a rider moves at the mount's speeds ([`nw_sim_core::MountPaces`]) of the
//! `MountMovementData` row its riding rank picks.

use bevy_ecs::entity::Entity;
use nw_bus::{CarriedMount, ConnectionRef, GhostMount, MOUNT_DYE_LEN, PlayerMounts};
use nw_content::mounts::{MountRule, MountStaminaRules, MountTypeRule};
use nw_messages::items::{FIRST_VERSION, FullList};
use nw_messages::mount::{
    LOGIN_STAMINA, LOGIN_STAMINA_RATE, NEUTRAL_MULTIPLIER, name_fits, riding_active_map,
};
use nw_messages::{
    BundleMember, DismountReason, FacetHeader, InputRecord, InteractionRequest, MOUNT_FACET,
    MemberState, MountClientMsg, MountCustomization, MountServerMsg, MountState, PaperdollPublic,
    PaperdollState, RemoteFlags, SummonAuthorization, SummonCancelReason, put_is_changing_mount,
};
use nw_sim_core::character::SPRINT_SPEED_MPS;
use nw_sim_core::{MountPaces, MoveLimits};

use super::attack::{Vitals, counter_is_newer};
use super::gear::{Gear, unix_ns};
use super::replica::PAPERDOLL_COMPONENT;
use super::{MESSAGE_CHANNEL, Outgoing, Player, SimWorld, gather_answer};

/// How long a summon's authorization lasts from the summon's start, ms (Observed: 8,002, 7,988
/// and 8,005 ms after the 5747 in the Mount-quest capture, with 3,000 ms summons; 7,843 and
/// 7,875 ms with 1,500 ms summons in a capture whose clock ran 150 ms behind).
const SUMMON_AUTHORIZATION_MS: u64 = 8_000;
/// The dash starts only with this much mount stamina (`playeractions_mount.actionlist`
/// `MountDash`: `IfMountStamina GreaterThanOrEqual 15`).
const DASH_MIN_STAMINA: f32 = 15.0;
/// The regen delay of a jump on a mount (`playeractions_mount.actionlist` `MountJump`:
/// `PayMountStaminaCost Cost=0 RegenDelay=1`).
const JUMP_REGEN_DELAY_S: f32 = 1.0;
/// The 5752 byte of a refusal at the summon key and of a player's own cancel, and of a cancel of
/// a running summon by the server (summon survey of 2026-10-31, 398 retail 5752: 0 in all 213
/// refusals at the key and 103 player cancels; 1 in all 47 server cancels: `Combat` 13,
/// `RestrictedArea` 21, `Midair` 13, `Interact` 2; the meaning is Unknown).
const KEY_OR_PLAYER_CANCEL_FLAG: u8 = 0;
const SERVER_CANCEL_FLAG: u8 = 1;
/// The 5752 notification (1 in all 398 retail calls).
const CANCEL_NOTIFY: bool = true;
/// The 5752 number of a refusal on cooldown, and of every other reason (1 in all 17 retail
/// `OnCooldown` refusals, 0 in the 381 others; the meaning is Unknown).
const ON_COOLDOWN_DETAIL: u32 = 1;
const CANCEL_DETAIL: u32 = 0;
/// The 5969 result of a name taken (0 in both retail answers).
const RENAME_TAKEN: u32 = 0;
/// The 3743 status of an interaction the server takes.
const INTERACTION_ACCEPTED: u8 = 1;
/// The interaction kinds of the mount screens (the client's `InteractionUIActions`):
/// `OpenSummoningSelection`, `OpenHorseShop`, `OpenWolfShop` and `OpenLionShop`. Retail answered
/// each with a 3743 accepted that names the object (3 shops in the race captures).
const MOUNT_SCREEN_KINDS: [u8; 4] = [76, 85, 86, 87];
/// Milliseconds per second.
const MS_PER_SECOND: f32 = 1_000.0;
/// The share of the max health a fall takes that gets a rider off (cvar
/// `javelin.mount-fall-damage-dismount-threshold-pct`, client default 0.1; the retail remote
/// config layers do not set it).
const FALL_DISMOUNT_SHARE: f64 = 0.1;
/// The summon cooldown end of no cooldown (retail cards before any summon: 0).
const NO_COOLDOWN_NS: u64 = 0;
/// The 5816 byte of unknown meaning (all 4 retail calls: 1).
const SCHEDULED_DISMOUNT_FLAG: u8 = 1;

/// Where a player is with its mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// On foot.
    Idle,
    /// Summoning `mount_id`: authorized from `ready_ms` to `expires_ms` (sim ms).
    Summoning {
        mount_id: u32,
        ready_ms: u64,
        expires_ms: u64,
        authorized: bool,
    },
    /// Riding `mount_id`.
    Mounted { mount_id: u32 },
}

/// What a tick of a player's mount does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Due {
    /// Nothing more.
    Nothing,
    /// The summon ends for this reason.
    Cancel(SummonCancelReason),
    /// The rider gets off for this reason.
    Dismount(DismountReason),
    /// The body gets on this mount.
    Ride(u32),
}

/// A player's mount state.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlayerMount {
    /// The chosen mount.
    selected: Option<u32>,
    /// The mounts with a dye or a name, in the order first customized.
    customized: Vec<MountCustomization>,
    /// The version of the newest persistent list the client has.
    list_version: u64,
    phase: Phase,
    /// When the next summon may start (sim ms), and its wire time (Unix ns; 0 for none; kept
    /// over a save and a handoff).
    cooldown_end_ms: u64,
    cooldown_end_ns: u64,
    /// The summon authorization as the client has it.
    authorization: SummonAuthorization,
    stamina: f32,
    stamina_max: f32,
    /// Seconds left before the stamina regenerates.
    regen_delay_s: f32,
    regen_rate: f32,
    drain_rate: f32,
    /// The stamina rules of the mount ridden last.
    stamina_rules: Option<MountStaminaRules>,
    /// The fastest speed of the mount ridden now, m/s.
    top_speed_mps: f32,
    /// The dash key toggles the dash (6268).
    dash_toggle: bool,
    /// The dash is toggled on.
    auto_dash: bool,
    /// The dash key in the newest record.
    dash_held: bool,
    /// The jump key in the newest record.
    jump_held: bool,
    dashing: bool,
    /// The body moved in the last tick.
    moving: bool,
    /// The walk key gives the slow walk (6271).
    slow_walk: bool,
    /// The mount choice window is open (6066).
    changing_mount: bool,
    /// The counter of the newest input record read.
    last_counter: Option<u16>,
    /// The sim time of the last tick.
    last_tick_ms: Option<u64>,
    /// 2749 went out.
    enabled_sent: bool,
    /// The achievements changed since the unlock was last checked (the first card's count).
    unlock_due: bool,
    /// The own children that changed since the last bundle.
    change: MountState,
}

impl PlayerMount {
    /// The mounts a join or a handoff carries, at `now_ms` (sim ms) and server time `server_ms`:
    /// on foot, unless the handoff carries a ride; a summon cooldown that runs goes on, and one
    /// that is over is 0 ([`Self::clear_ended_cooldown`]).
    pub(super) fn of(carried: &PlayerMounts, now_ms: u64, server_ms: u64) -> Self {
        let left_ms = carried
            .summon_cooldown_end_ns
            .saturating_sub(unix_ns(server_ms))
            / nw_messages::NS_PER_MS;
        let cooldown_end_ns = if left_ms == 0 {
            NO_COOLDOWN_NS
        } else {
            carried.summon_cooldown_end_ns
        };
        Self {
            selected: carried.selected,
            customized: carried
                .customized
                .iter()
                .map(|mount| MountCustomization {
                    mount_id: mount.mount_id,
                    dye: mount.dye,
                    name: mount.name.clone(),
                })
                .collect(),
            list_version: FIRST_VERSION,
            phase: carried
                .riding
                .map_or(Phase::Idle, |mount_id| Phase::Mounted { mount_id }),
            cooldown_end_ms: now_ms.saturating_add(left_ms),
            cooldown_end_ns,
            authorization: SummonAuthorization::default(),
            stamina: LOGIN_STAMINA,
            stamina_max: LOGIN_STAMINA,
            regen_delay_s: 0.0,
            regen_rate: LOGIN_STAMINA_RATE,
            drain_rate: LOGIN_STAMINA_RATE,
            stamina_rules: None,
            top_speed_mps: 0.0,
            dash_toggle: false,
            auto_dash: false,
            dash_held: false,
            jump_held: false,
            dashing: false,
            moving: false,
            slow_walk: false,
            changing_mount: false,
            last_counter: None,
            last_tick_ms: None,
            enabled_sent: false,
            unlock_due: true,
            change: MountState::default(),
        }
    }

    /// The player's achievements changed (its own bundle sent them): the next tick checks
    /// whether a mount type is unlocked, until 2749 went out.
    pub(super) const fn achievements_changed(&mut self) {
        self.unlock_due = !self.enabled_sent;
    }

    /// The mounts a save or a handoff carries.
    pub(super) fn carried(&self) -> PlayerMounts {
        PlayerMounts {
            selected: self.selected,
            customized: self
                .customized
                .iter()
                .map(|mount| CarriedMount {
                    mount_id: mount.mount_id,
                    dye: mount.dye,
                    name: mount.name.clone(),
                })
                .collect(),
            riding: self.riding(),
            summon_cooldown_end_ns: self.cooldown_end_ns,
        }
    }

    /// The mount it rides.
    pub(super) const fn riding(&self) -> Option<u32> {
        match self.phase {
            Phase::Mounted { mount_id } => Some(mount_id),
            Phase::Idle | Phase::Summoning { .. } => None,
        }
    }

    /// What other players see: the mount it rides (0 on foot), the dash and stamina flags and
    /// the mount's dye.
    pub(super) fn remote(&self) -> GhostMount {
        let mount_id = self.riding().unwrap_or_default();
        GhostMount {
            mount_id,
            flags: RemoteFlags::of(
                self.dashing,
                self.dashing && self.moving,
                self.stamina,
                self.stamina_max,
            )
            .0,
            dye: self.dye_of(mount_id),
        }
    }

    fn dye_of(&self, mount_id: u32) -> [u8; MOUNT_DYE_LEN] {
        self.customized
            .iter()
            .find(|mount| mount.mount_id == mount_id)
            .map_or([0; MOUNT_DYE_LEN], |mount| mount.dye)
    }

    /// The persistent list with its version.
    fn persistent(&self) -> FullList<MountCustomization> {
        FullList {
            version: Some(self.list_version),
            elements: self.customized.clone(),
        }
    }

    /// The whole own state, as the card carries it (retail: the persistent list only when a
    /// mount has a dye or a name).
    fn card(&self) -> MountState {
        MountState {
            mounted: Some(self.riding().is_some()),
            summon_cooldown_end_ns: Some(self.cooldown_end_ns),
            server_forcing_walk: Some(false),
            in_exclusion_volume: Some(false),
            summon_authorization: Some(self.authorization),
            persistent: (!self.customized.is_empty()).then(|| self.persistent()),
            stamina: Some(self.stamina),
            stamina_max: Some(self.stamina_max),
            regen_delay: Some(self.regen_delay_s),
            regen_rate: Some(self.regen_rate),
            drain_rate: Some(self.drain_rate),
            max_multiplier: Some(NEUTRAL_MULTIPLIER),
            regen_multiplier: Some(NEUTRAL_MULTIPLIER),
            mount_id: Some(self.selected.unwrap_or_default()),
            remote_flags: None,
            remote_dye: None,
        }
    }

    /// The own mount member of the next bundle: the whole state with the `first` card, else the
    /// children that changed, if any.
    pub(super) fn take_own_member(&mut self, first: bool) -> Option<BundleMember> {
        let change = std::mem::take(&mut self.change);
        let state = if first { self.card() } else { change };
        (!state.is_empty()).then(|| state.member())
    }

    /// The mount `mount_id` gets a new dye or name from `customize`; the client gets the whole
    /// list again.
    fn customize(&mut self, mount_id: u32, customize: impl FnOnce(&mut MountCustomization)) {
        let index = match self
            .customized
            .iter()
            .position(|mount| mount.mount_id == mount_id)
        {
            Some(index) => index,
            None => {
                self.customized.push(MountCustomization {
                    mount_id,
                    ..MountCustomization::default()
                });
                self.customized.len() - 1
            }
        };
        customize(&mut self.customized[index]);
        self.list_version += 1;
        self.change.persistent = Some(self.persistent());
    }

    /// The chosen mount is `mount_id`.
    fn select(&mut self, mount_id: u32) {
        if self.selected != Some(mount_id) {
            self.selected = Some(mount_id);
            self.change.mount_id = Some(mount_id);
        }
    }

    /// The next summon waits until `end_ms` (sim ms), `end_ns` on the wire.
    fn cool_down(&mut self, end_ms: u64, end_ns: u64) {
        self.cooldown_end_ms = end_ms;
        self.cooldown_end_ns = end_ns;
        self.change.summon_cooldown_end_ns = Some(end_ns);
    }

    /// A summon cooldown that is over at `now_ms` goes to the client as 0. The client refuses a
    /// summon while its clock is before `m_summonCooldownEndTime` (`FUN_1438e2270`: reason 14,
    /// "Summon ready in"), and live (2026-11-03) it refused every summon after the first
    /// cooldown, minutes after its end; a 0 always passes its check. Retail kept an ended time
    /// (its clients summoned after it), so the 0 is Inferred, for a client clock that is behind.
    fn clear_ended_cooldown(&mut self, now_ms: u64) {
        if self.cooldown_end_ns != NO_COOLDOWN_NS && now_ms >= self.cooldown_end_ms {
            self.cool_down(self.cooldown_end_ms, NO_COOLDOWN_NS);
        }
    }

    /// The authorization changes to `authorization`.
    fn authorize(&mut self, authorization: SummonAuthorization) {
        self.authorization = authorization;
        self.change.summon_authorization = Some(authorization);
    }

    /// It gets on the mount of the summon with the stamina `rules` of its movement row, whose
    /// fastest speed is `top_speed_mps`.
    fn mount_up(&mut self, mount_id: u32, rules: MountStaminaRules, top_speed_mps: f32) {
        self.phase = Phase::Mounted { mount_id };
        self.stamina_rules = Some(rules);
        self.top_speed_mps = top_speed_mps;
        let max = rules.max as f32;
        if max != self.stamina_max {
            self.stamina_max = max;
            self.stamina = self.stamina.min(max);
            self.change.stamina_max = Some(max);
            self.change.stamina = Some(self.stamina);
        }
        self.regen_rate = rules.regen_rate as f32;
        self.drain_rate = rules.drain_rate as f32;
        self.change.mounted = Some(true);
        self.change.regen_rate = Some(self.regen_rate);
        self.change.drain_rate = Some(self.drain_rate);
    }

    /// It gets off its mount; the next summon waits until `end_ms`, `end_ns` on the wire.
    fn get_off(&mut self, end_ms: u64, end_ns: u64) {
        self.phase = Phase::Idle;
        self.dashing = false;
        self.auto_dash = false;
        self.change.mounted = Some(false);
        self.cool_down(end_ms, end_ns);
        self.authorize(SummonAuthorization::default());
    }

    /// Reads the dash and jump keys of `record` when it is newer than the records read before: a
    /// dash press toggles the dash in toggle mode, else the key holds it; a jump press on the
    /// mount sets the jump's regen delay. Gives the phase the record finds; `None` for a record
    /// read before.
    fn read_keys(&mut self, record: &InputRecord) -> Option<Phase> {
        let counter = record.u16_after_mask;
        if self
            .last_counter
            .is_some_and(|last| !counter_is_newer(counter, last))
        {
            return None;
        }
        self.last_counter = Some(counter);
        let held = record.dodging();
        if self.dash_toggle && held && !self.dash_held {
            self.auto_dash = !self.auto_dash;
        }
        self.dash_held = held;
        let jumping = record.move_keys().jump;
        if jumping && !self.jump_held && self.riding().is_some() {
            self.regen_delay_s = self.regen_delay_s.max(JUMP_REGEN_DELAY_S);
            self.change.regen_delay = Some(self.regen_delay_s);
        }
        self.jump_held = jumping;
        Some(self.phase)
    }

    /// The dash the rider asks for: the toggle in toggle mode, else the held key.
    const fn wants_dash(&self) -> bool {
        if self.dash_toggle {
            self.auto_dash
        } else {
            self.dash_held
        }
    }

    /// One tick of `dt_s` seconds for a body that `moving`: the dash starts and ends
    /// (`MountDash`: from [`DASH_MIN_STAMINA`] up, until the stamina is 0 or the key lets go)
    /// and drains the stamina on the move (Observed: retail flags 0x05, a dash at full stamina,
    /// for 8 s before the rider ran); without a dash the stamina regenerates after its delay.
    /// Gives whether the dash changed.
    fn tick(&mut self, dt_s: f32, moving: bool) -> bool {
        self.moving = moving;
        let was_dashing = self.dashing;
        let riding = self.riding().is_some();
        self.dashing = riding
            && self.wants_dash()
            && if self.dashing {
                self.stamina > 0.0
            } else {
                self.stamina >= DASH_MIN_STAMINA
            };
        if self.dashing {
            if moving {
                self.stamina = (self.stamina - self.drain_rate * dt_s).max(0.0);
                self.change.stamina = Some(self.stamina);
            }
            if self.stamina <= 0.0 {
                self.dashing = false;
                self.auto_dash = false;
                self.regen_delay_s = self.exhaustion_delay_s();
                self.change.regen_delay = Some(self.regen_delay_s);
            }
        } else if self.regen_delay_s > 0.0 {
            self.regen_delay_s = (self.regen_delay_s - dt_s).max(0.0);
            self.change.regen_delay = Some(self.regen_delay_s);
        } else if self.stamina < self.stamina_max {
            self.stamina = (self.stamina + self.regen_rate * dt_s).min(self.stamina_max);
            self.change.stamina = Some(self.stamina);
        }
        self.dashing != was_dashing
    }

    /// The regen delay after the stamina ran out or a hit took some: the mount's
    /// `StaminaHitRegenDelay` (Inferred: retail's delay was 1.0 s after each exhaustion and each
    /// stamina loss, the row's only delay, and the jump's `PayMountStaminaCost RegenDelay` 1).
    fn exhaustion_delay_s(&self) -> f32 {
        self.stamina_rules
            .map_or(0.0, |rules| rules.hit_regen_delay_s as f32)
    }

    /// A hit of `share` of the rider's health: its stamina cost (`StaminaLightHitCost` from
    /// `StaminaLightHitThreshold`, `StaminaHeavyHitCost` from `StaminaHeavyHitThreshold`) and
    /// the regen delay. Gives whether the stamina ran out.
    fn hit(&mut self, share: f64) -> bool {
        let Some(rules) = self.stamina_rules else {
            return false;
        };
        let cost = rules.hit_cost(share) as f32;
        if cost <= 0.0 {
            return false;
        }
        self.stamina = (self.stamina - cost).max(0.0);
        self.regen_delay_s = self.exhaustion_delay_s();
        self.change.stamina = Some(self.stamina);
        self.change.regen_delay = Some(self.regen_delay_s);
        self.stamina <= 0.0
    }
}

/// Puts the change of `player`'s active map at a mount-up or a dismount into its next own
/// bundle: the map with the mount's slots while it is `riding` ([`riding_active_map`]).
fn riding_paperdoll(player: &mut Player, riding: bool) {
    let Some(hands) = player.gear.as_ref().map(Gear::hands) else {
        return;
    };
    player.extra_members.push(BundleMember {
        component: PAPERDOLL_COMPONENT,
        state: MemberState::Paperdoll(Box::new(PaperdollState {
            owner: None,
            public: Some(PaperdollPublic {
                active: Some(riding_active_map(hands, riding)),
                ..PaperdollPublic::default()
            }),
        })),
    });
}

/// The other players' state of a rider whose last seen mount was `seen` (none for a card) and
/// is now `now`: slots 2 and 3 for a card or a new mount, slot 3 alone for new flags or a new dye;
/// `None` for no change.
pub(super) fn remote_member(seen: Option<GhostMount>, now: GhostMount) -> Option<BundleMember> {
    if seen == Some(now) {
        return None;
    }
    let new_mount = seen.is_none_or(|seen| seen.mount_id != now.mount_id);
    Some(
        MountState {
            mount_id: new_mount.then_some(now.mount_id),
            remote_flags: Some(RemoteFlags(now.flags)),
            remote_dye: Some(now.dye),
            ..MountState::default()
        }
        .member(),
    )
}

impl SimWorld {
    /// Carries out the mount call `call` of the client of `connection` at `now_ms`.
    pub fn mount_call(&mut self, connection: &ConnectionRef, call: MountClientMsg, now_ms: u64) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        tracing::debug!(?call, "mount call");
        match call {
            MountClientMsg::CompleteSummon { .. } => self.complete_summon(entity, now_ms),
            MountClientMsg::DashToggle { toggle, .. } => {
                self.with_mount(entity, |mount| mount.dash_toggle = toggle);
            }
            MountClientMsg::SlowWalk { slow_walk, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.mount.slow_walk = slow_walk;
                    player.tracked.set_mount_slow_walk(slow_walk);
                }
            }
            // The change mount screen follows the echoed `isChangingMount`; a call that repeats
            // the value gets none (Observed: every retail 6066 that changed it, 34 to 100 ms
            // later; none for a repeat, `Hivemountdyes`).
            MountClientMsg::ChangingMount { changing, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity)
                    && player.mount.changing_mount != changing
                {
                    player.mount.changing_mount = changing;
                    put_is_changing_mount(&mut player.extra_members, changing);
                }
            }
            // The mount attachment view changes nothing on the server (Observed: no server
            // message within 1 s of the 238 retail calls).
            MountClientMsg::AttachmentMode { .. } => {}
            MountClientMsg::ChangeMount { mount_id, .. } => {
                self.change_mount(entity, mount_id, now_ms);
            }
            MountClientMsg::DismissMount { reason, .. } => {
                // Retail answered the client's own dismount with the state alone (6 of 6).
                self.dismount(entity, reason, false, now_ms);
            }
            MountClientMsg::RenameMount { mount_id, name, .. } => {
                self.rename_mount(entity, mount_id, name);
            }
            MountClientMsg::MountDye { mount_id, dye, .. } => self.dye_mount(entity, mount_id, dye),
        }
    }

    /// Reads the mount keys of the new records of `records` (newest first, as the client sends
    /// them) of the client of `connection` at `now_ms`: the summon key summons or gets off, the
    /// dash key dashes, and an attack or a posture change cancels a summon
    /// (`playeractions_mount.actionlist` `MountSummoning`: `AttackAction`, `Crouch_Desired`,
    /// `Prone_Desired`).
    pub fn mount_input(
        &mut self,
        connection: &ConnectionRef,
        records: &[InputRecord],
        now_ms: u64,
    ) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        for record in records.iter().rev() {
            let Some(phase) = self
                .world
                .get_mut::<Player>(entity)
                .and_then(|mut player| player.mount.read_keys(record))
            else {
                continue;
            };
            let cancels = record.attack_pressed()
                || record.crouch_toggle_pressed()
                || record.prone_toggle_pressed();
            match phase {
                Phase::Mounted { .. } if record.summon_mount_pressed() => {
                    self.dismount(entity, DismountReason::Voluntary, true, now_ms);
                }
                Phase::Idle if record.summon_mount_pressed() => self.summon(entity, now_ms),
                Phase::Summoning { .. } if cancels => {
                    self.cancel_summon(entity, SummonCancelReason::PlayerCancelled, now_ms);
                }
                Phase::Idle | Phase::Summoning { .. } | Phase::Mounted { .. } => {}
            }
        }
    }

    /// Runs the mounts of every player one tick at `now_ms`: a summon's authorization and its
    /// end, the stamina and the dash, the ride in deep water, a handed-off rider's body, and
    /// 2749 once a mount type is unlocked (Inferred: retail sent it 650 ms after the mount
    /// quest's unlock and after each own card of a player with mounts).
    pub(super) fn tick_mounts(&mut self, now_ms: u64) {
        let entities: Vec<Entity> = self.by_connection.values().copied().collect();
        for entity in entities {
            self.tick_mount(entity, now_ms);
        }
    }

    fn tick_mount(&mut self, entity: Entity, now_ms: u64) {
        let due = {
            let rules = &self.achievement_rules;
            let mounts = &self.content.mounts;
            let server_ms = self.server_ms;
            let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                return;
            };
            let player = &mut *player;
            if player.own_card_sent
                && std::mem::take(&mut player.mount.unlock_due)
                && mounts.types().any(|mount_type| {
                    player
                        .flow
                        .has_achievement(rules, &mount_type.required_achievement)
                })
            {
                player.mount.enabled_sent = true;
                let enable = MountServerMsg::EnableMounts {
                    header: FacetHeader::of(MOUNT_FACET, player.number),
                    enabled: true,
                };
                push_mount_message(
                    &mut self.combat_messages,
                    player.connection.clone(),
                    &enable,
                );
            }
            let dt_s = player.mount.last_tick_ms.map_or(0.0, |last| {
                now_ms.saturating_sub(last) as f32 / MS_PER_SECOND
            });
            player.mount.last_tick_ms = Some(now_ms);
            player.mount.clear_ended_cooldown(now_ms);
            let [vx, vy] = player.tracked.velocity();
            if player.mount.tick(dt_s, vx.hypot(vy) > 0.0) {
                player.tracked.set_mount_dash(player.mount.dashing);
            }
            let swimming = player.tracked.is_swimming();
            match player.mount.phase {
                Phase::Summoning {
                    expires_ms,
                    authorized: true,
                    ..
                } if now_ms > expires_ms => Due::Cancel(SummonCancelReason::PlayerCancelled),
                // Retail: 5752 `Midair` 3.0 s after the 5747 (13 of 13), at the summon's end.
                Phase::Summoning {
                    authorized: false,
                    ready_ms,
                    ..
                } if now_ms >= ready_ms && !player.tracked.is_grounded() => {
                    Due::Cancel(SummonCancelReason::Midair)
                }
                Phase::Summoning {
                    mount_id,
                    ready_ms,
                    expires_ms,
                    authorized: false,
                } if now_ms >= ready_ms => {
                    player.mount.phase = Phase::Summoning {
                        mount_id,
                        ready_ms,
                        expires_ms,
                        authorized: true,
                    };
                    player.mount.authorize(SummonAuthorization {
                        authorized: true,
                        expires_ns: unix_ns(
                            server_ms.saturating_add(expires_ms.saturating_sub(now_ms)),
                        ),
                    });
                    Due::Nothing
                }
                Phase::Summoning { .. } if swimming => Due::Cancel(SummonCancelReason::DeepWater),
                Phase::Mounted { .. } if swimming => Due::Dismount(DismountReason::DeepWater),
                // A rider handed over from another sim gets its body's mount here.
                Phase::Mounted { mount_id } if !player.tracked.is_mounted() => Due::Ride(mount_id),
                Phase::Idle | Phase::Summoning { .. } | Phase::Mounted { .. } => Due::Nothing,
            }
        };
        match due {
            Due::Cancel(reason) => self.cancel_summon(entity, reason, now_ms),
            Due::Dismount(reason) => self.dismount(entity, reason, true, now_ms),
            Due::Ride(mount_id) => {
                if !self.ride(entity, mount_id) {
                    self.dismount(entity, DismountReason::Disabled, true, now_ms);
                }
            }
            Due::Nothing => {}
        }
    }

    /// A hit of `amount` of `max_health` on player `entity`, which `killed` it: a death ends the
    /// summon or the ride; a hit while summoning cancels it (retail 5752 `Combat`, 1 of 8); a
    /// hit on a rider takes mount stamina, and at 0 the rider gets off (`Combat`).
    pub(super) fn mount_hit(
        &mut self,
        entity: Entity,
        amount: f32,
        max_health: f32,
        killed: bool,
        now_ms: u64,
    ) {
        let Some(phase) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.mount.phase)
        else {
            return;
        };
        match phase {
            Phase::Summoning { .. } => {
                let reason = if killed {
                    SummonCancelReason::Death
                } else {
                    SummonCancelReason::Combat
                };
                self.cancel_summon(entity, reason, now_ms);
            }
            Phase::Mounted { .. } if killed => {
                self.dismount(entity, DismountReason::Death, true, now_ms);
            }
            Phase::Mounted { .. } => {
                let share = if max_health > 0.0 {
                    f64::from(amount / max_health)
                } else {
                    0.0
                };
                let ran_out = self
                    .world
                    .get_mut::<Player>(entity)
                    .is_some_and(|mut player| player.mount.hit(share));
                if ran_out {
                    self.dismount(entity, DismountReason::Combat, true, now_ms);
                }
            }
            Phase::Idle => {}
        }
    }

    /// Player `entity` starts an interaction: a summon ends and a rider gets off (retail 5937
    /// `Interact`; `MountSummoning`: `Interaction`).
    pub(super) fn mount_interaction(&mut self, entity: Entity, now_ms: u64) {
        self.end_mount_for(
            entity,
            SummonCancelReason::Interact,
            DismountReason::Interact,
            true,
            now_ms,
        );
    }

    /// Player `entity` lost `share` of its max health in a fall at `now_ms`: a rider whose fall
    /// took [`FALL_DISMOUNT_SHARE`] or more gets off at once (run-15 fall study: 14 of 14 retail
    /// mounted falls from 0.111 got 5816 `ScheduleDismount` `FallingDamage` flag 1 in the ms of
    /// the 3601, then 5937 with reason 0; 37 of 37 up to 0.0988 stayed mounted). `true` when it
    /// got off: [`SimWorld::fall_dismount_notification`] then tells its client.
    pub(super) fn mount_fall(&mut self, entity: Entity, share: f64, now_ms: u64) -> bool {
        let Some((number, connection)) = self.world.get::<Player>(entity).and_then(|player| {
            matches!(player.mount.phase, Phase::Mounted { .. })
                .then(|| (player.number, player.connection.clone()))
        }) else {
            return false;
        };
        if share < FALL_DISMOUNT_SHARE {
            return false;
        }
        let message = MountServerMsg::ScheduleDismount {
            header: FacetHeader::of(MOUNT_FACET, number),
            at_ns: unix_ns(self.server_ms),
            reason: DismountReason::FallingDamage,
            flag: SCHEDULED_DISMOUNT_FLAG,
        };
        push_mount_message(&mut self.combat_messages, connection, &message);
        self.dismount(entity, DismountReason::FallingDamage, false, now_ms);
        true
    }

    /// Tells the client of player `entity` that a fall got it off its mount (retail 5937 with
    /// reason 0 after the fall's 2071).
    pub(super) fn fall_dismount_notification(&mut self, entity: Entity) {
        let Some((number, connection)) = self
            .world
            .get::<Player>(entity)
            .map(|player| (player.number, player.connection.clone()))
        else {
            return;
        };
        let message = MountServerMsg::DismountNotification {
            header: FacetHeader::of(MOUNT_FACET, number),
            reason: DismountReason::Voluntary,
        };
        push_mount_message(&mut self.combat_messages, connection, &message);
    }

    /// The server moves player `entity` far (a fast travel, an unstuck): a summon ends, and a
    /// rider gets off with the state alone (`MountActive` ends on the client by itself:
    /// `IfFastTravelTeleporting`, `IfUnstuckTeleporting`).
    pub(super) fn mount_teleported(&mut self, entity: Entity, now_ms: u64) {
        self.end_mount_for(
            entity,
            SummonCancelReason::Teleport,
            DismountReason::Teleport,
            false,
            now_ms,
        );
    }

    /// Player `entity` opened its inventory: a summon ends (`MountSummoning`:
    /// `toggleInventoryWindow` while the mount choice window is closed).
    pub(super) fn mount_inventory_opened(&mut self, entity: Entity, now_ms: u64) {
        let summoning = self.world.get::<Player>(entity).is_some_and(|player| {
            matches!(player.mount.phase, Phase::Summoning { .. }) && !player.mount.changing_mount
        });
        if summoning {
            self.cancel_summon(entity, SummonCancelReason::PlayerCancelled, now_ms);
        }
    }

    /// Opens a mount screen of interaction `request` (`name`, the object's name its client sent)
    /// for player `number` (`entity`); `false` for an interaction of another kind.
    pub(super) fn open_mount_screen(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) -> bool {
        if !MOUNT_SCREEN_KINDS.contains(&request.kind) {
            return false;
        }
        if let Some(player) = self.world.get::<Player>(entity) {
            let answer = gather_answer(number, request, INTERACTION_ACCEPTED, name);
            super::send_now(&mut self.combat_messages, &player.connection, &[answer]);
        }
        true
    }

    /// The movement limits of player `entity`: a rider's run at most the fastest speed of its
    /// mount as many times as the limits allow the sprint (`SPRINT_SPEED_MPS`).
    pub(super) fn move_limits_of(&self, entity: Entity) -> MoveLimits {
        let limits = self.settings.limits;
        let top_speed_mps = self
            .world
            .get::<Player>(entity)
            .filter(|player| player.mount.riding().is_some())
            .map_or(0.0, |player| player.mount.top_speed_mps);
        MoveLimits {
            max_speed_mps: limits
                .max_speed_mps
                .max(limits.max_speed_mps * top_speed_mps / SPRINT_SPEED_MPS),
            ..limits
        }
    }

    fn with_mount(&mut self, entity: Entity, change: impl FnOnce(&mut PlayerMount)) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            change(&mut player.mount);
        }
    }

    /// A summon of player `entity` ends for `cancel`, or the rider gets off for `dismount`,
    /// with 5937 when `notify`.
    fn end_mount_for(
        &mut self,
        entity: Entity,
        cancel: SummonCancelReason,
        dismount: DismountReason,
        notify: bool,
        now_ms: u64,
    ) {
        match self
            .world
            .get::<Player>(entity)
            .map(|player| player.mount.phase)
        {
            Some(Phase::Summoning { .. }) => self.cancel_summon(entity, cancel, now_ms),
            Some(Phase::Mounted { .. }) => self.dismount(entity, dismount, notify, now_ms),
            Some(Phase::Idle) | None => {}
        }
    }

    /// The mount `mount` with its type, when `player` owns it and its type is unlocked: its
    /// `RequiredAchievementId` is set, or, for a store mount (`IsEntitlement`), the player's
    /// account holds the store entitlement that unlocks it; and its type's achievement is set.
    fn usable<'a>(
        &'a self,
        player: &Player,
        mount: &'a MountRule,
    ) -> Option<(&'a MountRule, &'a MountTypeRule)> {
        let rules = &self.achievement_rules;
        let by_achievement = mount
            .required_achievement
            .as_deref()
            .is_some_and(|id| player.flow.has_achievement(rules, id));
        let bought = mount.entitlement
            && self
                .content
                .store
                .unlock_of(&mount.id)
                .is_some_and(|store| player.entitlements.contains(&store.unlock.entitlement));
        let owned = by_achievement || bought;
        let mount_type = self.content.mounts.mount_type(mount)?;
        (owned
            && player
                .flow
                .has_achievement(rules, &mount_type.required_achievement))
        .then_some((mount, mount_type))
    }

    /// The summon key of player `entity` at `now_ms` (retail Mount-quest capture: 5747 40 ms
    /// after the press, then the mount id): refused on cooldown, in combat (5752 6 without a
    /// 5747: 173 retail refusals, one in `general-other-01` at 687.5 s after a fight), before any
    /// mount type is unlocked (5752 21, the cooldown set to now), without a usable mount (22),
    /// while swimming (25) or while the mount choice window is open (no answer: `SummonMount`
    /// needs it closed).
    fn summon(&mut self, entity: Entity, now_ms: u64) {
        let rules = &self.achievement_rules;
        let mounts = &self.content.mounts;
        let server_ms = self.server_ms;
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        if player.mount.changing_mount
            || self
                .world
                .get::<Vitals>(entity)
                .is_some_and(|vitals| vitals.health.is_dead())
        {
            return;
        }
        let refusal = if now_ms < player.mount.cooldown_end_ms {
            Err(SummonCancelReason::OnCooldown)
        } else if player.combat.in_combat() {
            Err(SummonCancelReason::Combat)
        } else if player.tracked.is_swimming() {
            Err(SummonCancelReason::Swimming)
        } else if !mounts.types().any(|mount_type| {
            player
                .flow
                .has_achievement(rules, &mount_type.required_achievement)
        }) {
            Err(SummonCancelReason::RequirementsNotMet)
        } else {
            let chosen = player
                .mount
                .selected
                .and_then(|selected| mounts.mount(selected))
                .and_then(|mount| self.usable(player, mount));
            chosen
                .or_else(|| {
                    mounts
                        .mounts()
                        .into_iter()
                        .find_map(|mount| self.usable(player, mount))
                })
                .map(|(mount, mount_type)| (mount.crc, mount_type.summon_ms))
                .ok_or(SummonCancelReason::NoMountEquipped)
        };
        let connection = player.connection.clone();
        let header = FacetHeader::of(MOUNT_FACET, player.number);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let message = match refusal {
            Ok((mount_id, summon_ms)) => {
                player.mount.select(mount_id);
                player.mount.phase = Phase::Summoning {
                    mount_id,
                    ready_ms: now_ms.saturating_add(u64::from(summon_ms)),
                    expires_ms: now_ms.saturating_add(SUMMON_AUTHORIZATION_MS),
                    authorized: false,
                };
                MountServerMsg::SummonPending { header, summon_ms }
            }
            Err(reason) => {
                if reason == SummonCancelReason::RequirementsNotMet {
                    player.mount.cool_down(now_ms, unix_ns(server_ms));
                }
                MountServerMsg::SummonCancelled {
                    header,
                    reason,
                    flag: KEY_OR_PLAYER_CANCEL_FLAG,
                    notify: CANCEL_NOTIFY,
                    detail: if reason == SummonCancelReason::OnCooldown {
                        ON_COOLDOWN_DETAIL
                    } else {
                        CANCEL_DETAIL
                    },
                }
            }
        };
        tracing::debug!(?message, "summon");
        push_mount_message(&mut self.combat_messages, connection, &message);
    }

    /// The summon of player `entity` ends without a mount for `reason` at `now_ms`: 5752 (its
    /// flag 0 for the player's own cancel, 1 for the server's), the authorization cleared and
    /// the summon cooldown its type's voluntary dismount cooldown ahead (retail: 1,000 ms after
    /// each of 5 player cancels).
    fn cancel_summon(&mut self, entity: Entity, reason: SummonCancelReason, now_ms: u64) {
        let Some(Phase::Summoning { mount_id, .. }) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.mount.phase)
        else {
            return;
        };
        let cooldown_ms = self.dismount_cooldown_ms(mount_id, true);
        let end_ms = now_ms.saturating_add(cooldown_ms);
        let end_ns = unix_ns(self.server_ms.saturating_add(cooldown_ms));
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player.mount.phase = Phase::Idle;
        player.mount.cool_down(end_ms, end_ns);
        if player.mount.authorization != SummonAuthorization::default() {
            player.mount.authorize(SummonAuthorization::default());
        }
        let message = MountServerMsg::SummonCancelled {
            header: FacetHeader::of(MOUNT_FACET, player.number),
            reason,
            flag: if reason == SummonCancelReason::PlayerCancelled {
                KEY_OR_PLAYER_CANCEL_FLAG
            } else {
                SERVER_CANCEL_FLAG
            },
            notify: CANCEL_NOTIFY,
            detail: CANCEL_DETAIL,
        };
        let connection = player.connection.clone();
        tracing::debug!(?reason, "summon cancelled");
        push_mount_message(&mut self.combat_messages, connection, &message);
    }

    /// The client of player `entity` asks to mount (6068) at `now_ms`: within its
    /// authorization it gets on.
    fn complete_summon(&mut self, entity: Entity, now_ms: u64) {
        let Some(Phase::Summoning {
            mount_id,
            expires_ms,
            authorized: true,
            ..
        }) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.mount.phase)
        else {
            tracing::debug!("mount request without an authorized summon");
            return;
        };
        if now_ms > expires_ms {
            self.cancel_summon(entity, SummonCancelReason::PlayerCancelled, now_ms);
            return;
        }
        if !self.ride(entity, mount_id) {
            self.cancel_summon(entity, SummonCancelReason::MountDisabled, now_ms);
        }
    }

    /// Player `entity` rides `mount_id`: its stamina rates go to its client and its body moves
    /// at the mount's speeds of the movement row of its riding rank. `false` when the content
    /// has no movement for the mount.
    fn ride(&mut self, entity: Entity, mount_id: u32) -> bool {
        let mounts = &self.content.mounts;
        let progression = &self.content.progression;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return false;
        };
        let movement = mounts
            .mount(mount_id)
            .and_then(|mount| mounts.mount_type(mount))
            .and_then(|mount_type| {
                let rank = progression
                    .tradeskill(&mount_type.tradeskill)
                    .map_or(0, |mastery| {
                        mastery.rank_of(player.masteries.experience(mastery)).0
                    });
                mounts.movement(mount_type, rank)
            });
        let Some(movement) = movement else {
            tracing::warn!(mount_id, "no movement row for the mount");
            return false;
        };
        player
            .mount
            .mount_up(mount_id, movement.stamina, movement.top_speed() as f32);
        player.tracked.mount(MountPaces::of(movement));
        let slow_walk = player.mount.slow_walk;
        player.tracked.set_mount_slow_walk(slow_walk);
        riding_paperdoll(&mut player, true);
        tracing::debug!(mount_id, "mounted");
        true
    }

    /// Player `entity` gets off its mount for `reason` at `now_ms`, with 5937 when `notify`:
    /// the summon cooldown is its type's voluntary or forced dismount cooldown ahead.
    fn dismount(&mut self, entity: Entity, reason: DismountReason, notify: bool, now_ms: u64) {
        let Some(Phase::Mounted { mount_id }) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.mount.phase)
        else {
            return;
        };
        let cooldown_ms = self.dismount_cooldown_ms(mount_id, reason.is_voluntary());
        let end_ns = unix_ns(self.server_ms.saturating_add(cooldown_ms));
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player
            .mount
            .get_off(now_ms.saturating_add(cooldown_ms), end_ns);
        player.tracked.dismount();
        riding_paperdoll(&mut player, false);
        tracing::debug!(?reason, "dismounted");
        if notify {
            let message = MountServerMsg::DismountNotification {
                header: FacetHeader::of(MOUNT_FACET, player.number),
                reason,
            };
            let connection = player.connection.clone();
            push_mount_message(&mut self.combat_messages, connection, &message);
        }
    }

    /// The dismount cooldown of `mount_id`'s type, ms: voluntary or forced.
    fn dismount_cooldown_ms(&self, mount_id: u32, voluntary: bool) -> u64 {
        let mounts = &self.content.mounts;
        mounts
            .mount(mount_id)
            .and_then(|mount| mounts.mount_type(mount))
            .map_or(0, |mount_type| {
                if voluntary {
                    mount_type.voluntary_dismount_cooldown_ms
                } else {
                    mount_type.forced_dismount_cooldown_ms
                }
            })
    }

    /// Player `entity` chooses `mount_id` (5619) at `now_ms`, when it can use it: a summon ends
    /// and a rider gets off (`MountChanged`).
    fn change_mount(&mut self, entity: Entity, mount_id: u32, now_ms: u64) {
        let usable = self.world.get::<Player>(entity).is_some_and(|player| {
            self.content
                .mounts
                .mount(mount_id)
                .and_then(|mount| self.usable(player, mount))
                .is_some()
        });
        if !usable {
            tracing::debug!(mount_id, "change to a mount the player cannot use");
            return;
        }
        self.end_mount_for(
            entity,
            SummonCancelReason::PlayerCancelled,
            DismountReason::MountChanged,
            true,
            now_ms,
        );
        self.with_mount(entity, |mount| mount.select(mount_id));
    }

    /// Player `entity` names `mount_id` `name` (5972): the persistent list changes and the
    /// client gets 5969.
    fn rename_mount(&mut self, entity: Entity, mount_id: u32, name: Vec<u8>) {
        let owned = self.world.get::<Player>(entity).is_some_and(|player| {
            self.content
                .mounts
                .mount(mount_id)
                .and_then(|mount| self.usable(player, mount))
                .is_some()
        });
        if !owned || !name_fits(&name) {
            tracing::debug!(mount_id, "rename refused");
            return;
        }
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let named = name.clone();
        player.mount.customize(mount_id, |mount| mount.name = named);
        let message = MountServerMsg::RenameNotification {
            header: FacetHeader::of(MOUNT_FACET, player.number),
            result: RENAME_TAKEN,
            mount_id,
            name,
        };
        let connection = player.connection.clone();
        push_mount_message(&mut self.combat_messages, connection, &message);
    }

    /// Player `entity` dyes `mount_id` with `dye` (6026): every color must be a mount dye's and
    /// stay off the channels the mount turns off; the persistent list changes (retail: the list
    /// with the new dye 68 ms after the call).
    fn dye_mount(&mut self, entity: Entity, mount_id: u32, dye: [u8; MOUNT_DYE_LEN]) {
        let mounts = &self.content.mounts;
        let fits = self.world.get::<Player>(entity).is_some_and(|player| {
            mounts
                .mount(mount_id)
                .and_then(|mount| self.usable(player, mount))
                .is_some_and(|(mount, _)| mount.takes_dye(dye) && mounts.is_dye(dye))
        });
        if !fits {
            tracing::debug!(mount_id, ?dye, "dye refused");
            return;
        }
        self.with_mount(entity, |mount| {
            mount.customize(mount_id, |customized| customized.dye = dye);
        });
    }
}

/// Queues the reliable mount message `message` to `connection` in `outgoing`.
pub(super) fn push_mount_message(
    outgoing: &mut Vec<Outgoing>,
    connection: ConnectionRef,
    message: &MountServerMsg,
) {
    match message.envelope() {
        Ok(envelope) => outgoing.push(Outgoing {
            connection,
            channel: MESSAGE_CHANNEL,
            reliable: true,
            envelopes: vec![envelope],
        }),
        Err(error) => tracing::warn!(%error, "mount message not encoded"),
    }
}

#[cfg(test)]
mod tests {
    use nw_content::mounts::{MountMovement, SurfaceSpeeds};

    use super::*;

    /// `Mount_MTX_5_lion_basic`.
    const LION_BASIC: u32 = 0x0c13_84ae;
    /// The `ConfigMountRiding_0` stamina.
    fn riding_stamina() -> MountStaminaRules {
        MountStaminaRules {
            max: 100.0,
            regen_rate: 10.0,
            drain_rate: 20.0,
            light_hit_threshold: 0.01,
            light_hit_cost: 25.0,
            heavy_hit_threshold: 0.15,
            heavy_hit_cost: 50.0,
            hit_regen_delay_s: 1.0,
        }
    }
    /// One tick at 30 Hz.
    const TICK_S: f32 = 1.0 / 30.0;
    const CLOSE: f32 = 1e-3;

    fn rider() -> PlayerMount {
        let mut mount = PlayerMount::of(&PlayerMounts::default(), 0, 0);
        mount.mount_up(LION_BASIC, riding_stamina(), 10.0);
        mount.change = MountState::default();
        mount
    }

    fn state_of(member: &BundleMember) -> MountState {
        assert_eq!(member.component, nw_messages::MOUNT_COMPONENT);
        let MemberState::Mount(state) = &member.state else {
            panic!("a mount state: {member:?}");
        };
        state.clone()
    }

    #[test]
    fn a_new_player_s_card_is_retail_s_login_card() {
        // The first 5620 of both retail new characters.
        let retail = [
            0x06, 0xdf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xc8, 0x00, 0x00, 0x3f, 0x42,
            0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00,
            0x00, 0x3f, 0x80, 0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut mount = PlayerMount::of(&PlayerMounts::default(), 0, 0);
        assert_eq!(mount.card(), MountState::login_card());
        assert_eq!(mount.card().encode().expect("encode"), retail);
        assert!(mount.take_own_member(true).is_some());
        assert!(mount.take_own_member(false).is_none(), "no change");
    }

    #[test]
    fn the_mount_up_sends_the_rates_and_the_ride_drains_and_regenerates() {
        let mut mount = PlayerMount::of(&PlayerMounts::default(), 0, 0);
        mount.mount_up(LION_BASIC, riding_stamina(), 10.0);
        let up = state_of(&mount.take_own_member(false).expect("change"));
        assert_eq!(
            (up.mounted, up.regen_rate, up.drain_rate),
            (Some(true), Some(10.0), Some(20.0))
        );
        // Held dash on the move: 20 per second.
        mount.dash_held = true;
        assert!(mount.tick(TICK_S, true), "the dash starts");
        assert!((mount.stamina - (100.0 - 20.0 * TICK_S)).abs() < CLOSE);
        assert_eq!(
            RemoteFlags(mount.remote().flags),
            RemoteFlags(RemoteFlags::DASHING | RemoteFlags::DASH_VFX)
        );
        // Dashing in place drains nothing.
        let before = mount.stamina;
        assert!(!mount.tick(TICK_S, false));
        assert_eq!(mount.stamina, before);
        // Until empty: the dash ends with the regen delay.
        while mount.stamina > 0.0 {
            mount.tick(TICK_S, true);
        }
        assert!(!mount.dashing);
        assert!((mount.regen_delay_s - 1.0).abs() < CLOSE);
        assert_eq!(mount.remote().flags, RemoteFlags::STAMINA_EMPTY);
        let empty = state_of(&mount.take_own_member(false).expect("change"));
        assert_eq!((empty.stamina, empty.regen_delay), (Some(0.0), Some(1.0)));
        // The held key does not start the dash again below 15.
        assert!(!mount.tick(TICK_S, true));
        assert!(!mount.dashing);
        // One second of delay, then 10 per second.
        mount.dash_held = false;
        for _ in 0..31 {
            mount.tick(TICK_S, true);
        }
        assert_eq!(mount.regen_delay_s, 0.0);
        let before = mount.stamina;
        mount.tick(TICK_S, true);
        assert!((mount.stamina - before - 10.0 * TICK_S).abs() < CLOSE);
    }

    #[test]
    fn the_toggle_mode_keeps_the_dash_on_without_the_key() {
        let mut mount = rider();
        mount.dash_toggle = true;
        mount.auto_dash = true;
        assert!(mount.tick(TICK_S, true));
        assert!(mount.dashing);
        mount.auto_dash = false;
        assert!(mount.tick(TICK_S, true));
        assert!(!mount.dashing);
    }

    #[test]
    fn hits_take_stamina_by_their_share_and_empty_it() {
        let mut mount = rider();
        assert!(!mount.hit(0.005), "below the light threshold");
        assert_eq!(mount.stamina, 100.0);
        assert!(!mount.hit(0.05));
        assert_eq!(mount.stamina, 75.0);
        assert!((mount.regen_delay_s - 1.0).abs() < CLOSE);
        assert!(!mount.hit(0.2));
        assert_eq!(mount.stamina, 25.0);
        assert!(mount.hit(0.2), "a heavy hit empties it");
    }

    #[test]
    fn a_dismount_clears_the_authorization_and_sets_the_cooldown() {
        const END_MS: u64 = 5_000;
        const END_NS: u64 = 1_789_833_431_303_000_000;
        let mut mount = rider();
        mount.authorize(SummonAuthorization {
            authorized: true,
            expires_ns: END_NS,
        });
        mount.change = MountState::default();
        mount.get_off(END_MS, END_NS);
        let down = state_of(&mount.take_own_member(false).expect("change"));
        assert_eq!(down.mounted, Some(false));
        assert_eq!(down.summon_cooldown_end_ns, Some(END_NS));
        assert_eq!(
            down.summon_authorization,
            Some(SummonAuthorization::default())
        );
        assert_eq!(mount.remote().mount_id, 0);
    }

    #[test]
    fn a_summon_cooldown_is_carried_and_goes_on_at_the_next_spawn() {
        const SERVER_MS: u64 = 1_000_000;
        const LEFT_MS: u64 = 7_500;
        const NOW_MS: u64 = 40_000;
        let mut mount = rider();
        let end_ns = unix_ns(SERVER_MS + LEFT_MS);
        mount.get_off(LEFT_MS, end_ns);
        let carried = mount.carried();
        assert_eq!(carried.summon_cooldown_end_ns, end_ns, "a save keeps it");
        let again = PlayerMount::of(&carried, NOW_MS, SERVER_MS);
        assert_eq!(
            again.cooldown_end_ms,
            NOW_MS + LEFT_MS,
            "the time left goes on"
        );
        assert_eq!(
            again.cooldown_end_ns, end_ns,
            "the client gets the same end"
        );
        let later = PlayerMount::of(&carried, NOW_MS, SERVER_MS + 2 * LEFT_MS);
        assert_eq!(later.cooldown_end_ms, NOW_MS, "an ended cooldown is over");
        assert_eq!(
            later.card().summon_cooldown_end_ns,
            Some(NO_COOLDOWN_NS),
            "the client gets no ended time"
        );
    }

    #[test]
    fn an_ended_summon_cooldown_goes_to_the_client_as_0_once() {
        const END_MS: u64 = 1_000;
        let mut mount = rider();
        mount.get_off(END_MS, unix_ns(END_MS));
        let _ = mount.take_own_member(false);
        mount.clear_ended_cooldown(END_MS - 1);
        assert!(mount.take_own_member(false).is_none(), "not yet over");
        mount.clear_ended_cooldown(END_MS);
        let member = mount.take_own_member(false).expect("the change");
        assert_eq!(
            state_of(&member).summon_cooldown_end_ns,
            Some(NO_COOLDOWN_NS)
        );
        mount.clear_ended_cooldown(END_MS + 1);
        assert!(mount.take_own_member(false).is_none(), "sent once");
    }

    #[test]
    fn dyes_and_names_go_in_the_persistent_list_and_are_carried() {
        let mut mount = rider();
        mount.customize(LION_BASIC, |customized| customized.dye = [0, 0, 0x33, 0]);
        let changed = state_of(&mount.take_own_member(false).expect("change"));
        let list = changed.persistent.expect("list");
        assert_eq!(list.version, Some(FIRST_VERSION + 1));
        assert_eq!(list.elements[0].dye, [0, 0, 0x33, 0]);
        mount.customize(LION_BASIC, |customized| customized.name = b"Rex".to_vec());
        assert_eq!(mount.customized.len(), 1, "one entry per mount");
        assert_eq!(mount.remote().dye, [0, 0, 0x33, 0]);
        mount.select(LION_BASIC);
        let carried = mount.carried();
        assert_eq!(carried.selected, Some(LION_BASIC));
        assert_eq!(carried.riding, Some(LION_BASIC));
        let again = PlayerMount::of(&carried, 0, 0);
        assert_eq!(again.customized, mount.customized);
        assert_eq!(again.riding(), Some(LION_BASIC), "a handoff keeps the ride");
        assert!(again.card().persistent.is_some());
    }

    #[test]
    fn other_players_get_slots_2_and_3_for_a_new_mount_and_slot_3_for_new_flags() {
        let riding = GhostMount {
            mount_id: LION_BASIC,
            flags: RemoteFlags::STAMINA_FULL,
            dye: [0; MOUNT_DYE_LEN],
        };
        let card = state_of(&remote_member(None, riding).expect("card"));
        assert_eq!(card.mount_id, Some(LION_BASIC));
        assert_eq!(
            card.remote_flags,
            Some(RemoteFlags(RemoteFlags::STAMINA_FULL))
        );
        assert_eq!(card.mounted, None, "the owner's slot stays the owner's");
        let dashing = GhostMount {
            flags: RemoteFlags::DASHING | RemoteFlags::STAMINA_FULL,
            ..riding
        };
        let flags = state_of(&remote_member(Some(riding), dashing).expect("flags"));
        assert_eq!(flags.mount_id, None);
        assert_eq!(flags.remote_flags, Some(RemoteFlags(0x05)));
        assert!(remote_member(Some(dashing), dashing).is_none());
    }

    /// Slots of a client record (19 words of the action set, as 10 values).
    const ACTION_SLOTS: u8 = 19;
    /// The neutral scaled bytes of a record.
    const NEUTRAL_BYTES: [u8; 4] = [128; 4];
    /// Input 26, the summon key (value 0).
    const SUMMON_KEY: u64 = 1 << 26;
    /// Input 67, the dodge key (value 1): the mount's dash.
    const DASH_KEY: u64 = 1 << 3;
    /// Input 41, the jump key (value 1).
    const JUMP_KEY: u64 = 1 << 9;

    fn record(counter: u16, values: Vec<u64>) -> InputRecord {
        InputRecord {
            u16_after_mask: counter,
            slots: nw_messages::client_input::InputSlots {
                count: ACTION_SLOTS,
                values,
                bytes: NEUTRAL_BYTES,
            },
            ..InputRecord::default()
        }
    }

    #[test]
    fn a_jump_on_the_mount_sets_the_regen_delay_once_per_press() {
        let mut mount = rider();
        mount.regen_delay_s = 0.0;
        mount.read_keys(&record(1, vec![0, JUMP_KEY]));
        assert_eq!(mount.regen_delay_s, JUMP_REGEN_DELAY_S);
        assert_eq!(mount.change.regen_delay, Some(JUMP_REGEN_DELAY_S));
        mount.tick(0.5, true);
        let waited = mount.regen_delay_s;
        mount.read_keys(&record(2, vec![0, JUMP_KEY]));
        assert_eq!(mount.regen_delay_s, waited, "a held key is one jump");
        mount.read_keys(&record(3, vec![0, 0]));
        mount.read_keys(&record(4, vec![0, JUMP_KEY]));
        assert_eq!(mount.regen_delay_s, JUMP_REGEN_DELAY_S, "a new press");
        let mut walker = PlayerMount::of(&PlayerMounts::default(), 0, 0);
        walker.read_keys(&record(1, vec![0, JUMP_KEY]));
        assert_eq!(walker.regen_delay_s, 0.0, "only a rider");
    }

    #[test]
    fn a_record_is_read_once_and_the_dash_key_toggles_in_toggle_mode() {
        let mut mount = rider();
        let riding = Some(Phase::Mounted {
            mount_id: LION_BASIC,
        });
        assert_eq!(mount.read_keys(&record(5, vec![0, DASH_KEY])), riding);
        assert!(mount.wants_dash(), "the held key dashes");
        assert_eq!(
            mount.read_keys(&record(5, vec![0, DASH_KEY])),
            None,
            "a record read before"
        );
        mount.dash_toggle = true;
        mount.read_keys(&record(6, vec![0, 0]));
        mount.read_keys(&record(7, vec![0, DASH_KEY]));
        assert!(mount.wants_dash(), "a press toggles the dash on");
        mount.read_keys(&record(8, vec![0, DASH_KEY]));
        assert!(mount.wants_dash(), "holding it changes nothing");
        mount.read_keys(&record(9, vec![0, 0]));
        mount.read_keys(&record(10, vec![0, DASH_KEY]));
        assert!(!mount.wants_dash(), "the next press toggles it off");
    }

    /// A sim with one horse that the achievements unlock, and its player.
    mod flow {
        use std::collections::HashMap;
        use std::sync::Arc;

        use nw_content::achievements::AchievementIndexes;
        use nw_content::mounts::{MountRules, MountTypeRule};
        use nw_formats::objectstream::lowercase_crc32;
        use nw_reflect::Envelope;

        use super::super::super::Outgoing;
        use super::super::super::tests::{
            NOW, SERVER_TIME, admit_at, connection, world_with_spawns,
        };
        use super::*;
        use crate::player_content::PlayerContent;
        use nw_db::{AccountEntitlement, Purchase, Wallet};
        use nw_login::config::ShopConfig;
        use nw_messages::store::StorePurchase;
        use nw_messages::store::WALLET_ENTITLEMENT_KIND;
        use nw_messages::{RAW_ID_LEN, ServerMessage};
        use nw_protocol::store_api::{STORE_CURRENCY, StoreEntitlement, StoreMount, StoreTable};
        use uuid::Uuid;

        const PLAYER: u64 = 1;
        const HORSE: &str = "Mount_Horse_1";
        /// A store mount of the store table, and the entitlement that unlocks it.
        const STORE_BEAR: &str = "Mount_MTX_7_bear_Ronin";
        const BEAR_ENTITLEMENT: &str = "Ent_Mount_MTX_7_bear_Ronin";
        const HORSE_UNLOCK: &str = "MountType_Unlock_Horse";
        const HORSE_GIFT: &str = "AwardAchievement_Mount_Horse_1";
        const UNLOCK_BIT: u32 = 1_000;
        const GIFT_BIT: u32 = 1_001;
        /// The retail summon time and dismount cooldowns.
        const SUMMON_MS: u32 = 3_000;
        const COOLDOWN_MS: u64 = 1_000;
        /// The `ConfigMountRiding_0` road run and dash.
        const RUN_MPS: f64 = 9.25;
        const DASH_SCALE: f64 = 1.12;
        const TICK_MS: u64 = 33;

        fn movement() -> MountMovement {
            let speeds = SurfaceSpeeds {
                max_speed: RUN_MPS,
                walk_speed: 6.3,
                slow_walk_speed: 2.0,
                acceleration: 15.0,
                deceleration: 9.0,
                walk_acceleration: 15.0,
                walk_deceleration: 12.0,
            };
            MountMovement {
                surfaces: [speeds; 5],
                dash_speed_scale: DASH_SCALE,
                dash_acceleration_scale: 1.15,
                stamina: riding_stamina(),
            }
        }

        /// Whether the player of `world` may use the mount `id`.
        fn may_use(world: &mut SimWorld, id: &str) -> bool {
            let entity = world.player_by_number(PLAYER).expect("player");
            let player = world.world.get::<Player>(entity).expect("player");
            let rule = world
                .content
                .mounts
                .mount(lowercase_crc32(id))
                .expect("mount rule");
            world.usable(player, rule).is_some()
        }

        /// A free store purchase of offer `offer`, as retail's client sends it.
        fn purchase(offer: &str) -> StorePurchase {
            StorePurchase {
                language: b"en-US".to_vec(),
                currency: b"USD".to_vec(),
                country: b"US".to_vec(),
                word_a: 1,
                transaction: [7; RAW_ID_LEN],
                offer: offer.as_bytes().to_vec(),
                offer_again: offer.as_bytes().to_vec(),
                byte_b: 0,
                product: b"Marks of Fortune".to_vec(),
                price: 0,
                byte_c: 1,
            }
        }

        fn store_answers(world: &mut SimWorld) -> Vec<ServerMessage> {
            std::mem::take(&mut world.combat_messages)
                .into_iter()
                .filter(|out| out.connection == connection(PLAYER))
                .flat_map(|out| out.envelopes)
                .filter_map(|bytes| {
                    let envelope = Envelope::decode(&bytes).ok()?;
                    ServerMessage::decode(envelope.type_index()?, &envelope.body)?.ok()
                })
                .filter(|message| {
                    matches!(
                        message,
                        ServerMessage::RequestInitialized(_) | ServerMessage::RequestSuccess(_)
                    )
                })
                .collect()
        }

        #[test]
        fn a_store_mount_is_free_and_usable_once_its_account_holds_it() {
            /// When the account got it: 2026-11-04 12:32:26.050 UTC.
            const GRANTED_MS: i64 = 1_793_795_546_050;
            /// The account's Marks of Fortune.
            const BALANCE: i64 = 13_000;
            let mut world = world_with(vec![UNLOCK_BIT]);
            let entity = world.player_by_number(PLAYER).expect("player");
            // Any facet header: the answers carry the call's own.
            let header = FacetHeader::of(MOUNT_FACET, PLAYER);
            assert!(!may_use(&mut world, STORE_BEAR), "not bought yet");
            world.store_purchase(entity, header, &purchase("Ent_Unknown"));
            assert!(store_answers(&mut world).is_empty(), "no such offer");
            assert!(world.take_store_grants().is_empty());
            world.store_purchase(entity, header, &purchase(BEAR_ENTITLEMENT));
            let [ServerMessage::RequestInitialized(started)] = &store_answers(&mut world)[..]
            else {
                panic!("5936 at once");
            };
            assert_eq!(started.0.header, header);
            assert_eq!(started.0.request, [7; RAW_ID_LEN]);
            assert!(started.0.entitlements.is_empty());
            let grants = world.take_store_grants();
            let [grant] = &grants[..] else {
                panic!("one grant");
            };
            assert_eq!(
                (grant.entitlement.as_str(), grant.price),
                (BEAR_ENTITLEMENT, 0)
            );
            assert!(!may_use(&mut world, STORE_BEAR), "not saved yet");
            world.store_granted(
                grant,
                Ok(Purchase::Bought {
                    held: AccountEntitlement {
                        entitlement: BEAR_ENTITLEMENT.to_owned(),
                        instance: Uuid::from_u128(7),
                        granted_at_ms: GRANTED_MS,
                    },
                    wallet: Wallet {
                        marks: BALANCE,
                        updated_at_ms: GRANTED_MS,
                    },
                }),
            );
            let [ServerMessage::RequestSuccess(done)] = &store_answers(&mut world)[..] else {
                panic!("1285 after the save");
            };
            assert_eq!(done.0.transaction, started.0.transaction);
            let [bought, wallet] = &done.0.entitlements[..] else {
                panic!("the bought entitlement and the wallet");
            };
            assert_eq!(wallet.alias, STORE_CURRENCY.as_bytes());
            assert_eq!(
                (wallet.kind, wallet.total),
                (WALLET_ENTITLEMENT_KIND, 13_000)
            );
            assert_eq!(wallet.amounts, [13_000, 0, 0]);
            assert_eq!(bought.alias, BEAR_ENTITLEMENT.as_bytes());
            assert_eq!(bought.name, b"Ronin Bear Mount");
            assert_eq!(
                bought.instances,
                [Uuid::from_u128(7).to_string().into_bytes()]
            );
            assert_eq!(bought.updated, b"2026-11-04T12:32:26.050Z");
            assert!(may_use(&mut world, STORE_BEAR));
            world.change_mount(entity, lowercase_crc32(STORE_BEAR), NOW);
            assert_eq!(
                mount_of(&mut world).selected,
                Some(lowercase_crc32(STORE_BEAR))
            );
        }

        /// The sim of [`world_with`] whose store has the settings `shop`.
        fn world_with_shop(shop: ShopConfig) -> SimWorld {
            let mut world = world_with(vec![UNLOCK_BIT]);
            Arc::make_mut(&mut world.content).shop = shop;
            world
        }

        #[test]
        fn a_closed_store_sells_nothing() {
            let mut world = world_with_shop(ShopConfig {
                enabled: false,
                ..ShopConfig::default()
            });
            let entity = world.player_by_number(PLAYER).expect("player");
            let header = FacetHeader::of(MOUNT_FACET, PLAYER);
            world.store_purchase(entity, header, &purchase(BEAR_ENTITLEMENT));
            assert!(store_answers(&mut world).is_empty());
            assert!(world.take_store_grants().is_empty());
        }

        #[test]
        fn a_store_mount_costs_its_price_and_a_short_wallet_gets_no_success() {
            const PRICE: u32 = 15_000;
            let mut world = world_with_shop(ShopConfig {
                free: false,
                mount_price: PRICE,
                ..ShopConfig::default()
            });
            let entity = world.player_by_number(PLAYER).expect("player");
            let header = FacetHeader::of(MOUNT_FACET, PLAYER);
            world.store_purchase(entity, header, &purchase(BEAR_ENTITLEMENT));
            assert_eq!(store_answers(&mut world).len(), 1, "5936");
            let grants = world.take_store_grants();
            let [grant] = &grants[..] else {
                panic!("one grant");
            };
            assert_eq!(grant.price, PRICE);
            world.store_granted(
                grant,
                Ok(Purchase::TooPoor {
                    wallet: Wallet::default(),
                }),
            );
            assert!(store_answers(&mut world).is_empty(), "no 1285");
            assert!(!may_use(&mut world, STORE_BEAR));
        }

        #[test]
        fn the_change_mount_screen_s_flag_is_echoed_once_per_change() {
            use nw_messages::player_components::PLAYER_COMPONENT_COMPONENT;
            let mut world = world_with(Vec::new());
            let entity = world.player_by_number(PLAYER).expect("player");
            let echoes = |world: &SimWorld| -> Vec<Option<bool>> {
                world
                    .world
                    .get::<Player>(entity)
                    .expect("player")
                    .extra_members
                    .iter()
                    .filter(|member| member.component == PLAYER_COMPONENT_COMPONENT)
                    .filter_map(|member| match &member.state {
                        MemberState::PlayerComponent(state) => Some(state.is_changing_mount),
                        _ => None,
                    })
                    .collect()
            };
            // Open, open again (no echo: the value did not change), close.
            for (changing, expected) in [
                (true, vec![Some(true)]),
                (true, Vec::new()),
                (false, vec![Some(false)]),
            ] {
                world
                    .world
                    .get_mut::<Player>(entity)
                    .expect("player")
                    .extra_members
                    .clear();
                world.mount_call(
                    &connection(PLAYER),
                    MountClientMsg::ChangingMount {
                        header: FacetHeader::of(PLAYER_COMPONENT_COMPONENT, PLAYER),
                        changing,
                    },
                    NOW,
                );
                assert_eq!(echoes(&world), expected, "changing {changing}");
            }
        }

        #[test]
        fn a_mount_food_of_quick_slot_3_feeds_the_horse_up_to_the_daily_limit() {
            use nw_content::item_rules::{ConsumableRow, ItemRuleRow, ItemRuleSource, ItemRules};
            use nw_content::loot_limits::{
                LimitRow, LootLimitRules, LootLimits, ResetClock, Weekday,
            };
            use nw_content::progression::{Progression, ProgressionSource};
            use nw_content::rewards::{EventGrant, EventGrants};
            use nw_content::status_effects::{
                EffectSourceRow, StatusEffectRules, StatusEffectSource,
            };
            use nw_messages::ServerMessage;
            use nw_messages::paperdoll::slot;

            use super::super::super::attack::MOUNT_FOOD_USE_MS;
            use super::super::super::tests::messages_to;

            use crate::objectives::{TaskBook, TaskRow};
            use crate::player_content::{ObjectiveRow, QuestBook};
            use crate::world::gear::{Gear, tests as gear_tests};
            // Retail rows (content bundle 25810808): `MountConsum_GruelT1` adds the effect, whose
            // game event gives `Riding` 2,500 and counts on the daily limit of 3; the quest step
            // `9401_TutorialFood` counts the event.
            const GRUEL: &str = "MountConsum_GruelT1";
            const FOOD_EFFECT: &str = "Status_Mount_GrantRidingXP_Small";
            const FOOD_EVENT: &str = "Mount_GrantRidingXP_Small";
            const FOOD_LIMIT: &str = "Mount_FoodDailyXP_Limit";
            const DAILY_USES: i64 = 3;
            const DAILY_EXPIRE_S: i64 = 86_400;
            const RIDING_REWARD: f64 = 2_500.0;
            const RATIONS: u32 = 5;
            /// Input 140, `Use_Quickslot_3` (value 2 bit 12).
            const QUICK_SLOT_3_KEY: u64 = 1 << 12;
            const FOOD_QUEST: &str = "q_feed";
            let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
                task_id: id.to_owned(),
                kind: kind.to_owned(),
                sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
                ..TaskRow::default()
            };
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            let content = Arc::make_mut(&mut world.content);
            content.item_rules = ItemRules::build(ItemRuleSource {
                items: vec![ItemRuleRow {
                    item_id: GRUEL.to_owned(),
                    stats_ref: None,
                    consume_on_use: true,
                    not_droppable: false,
                    nonremovable: false,
                }],
                consumables: vec![ConsumableRow {
                    id: GRUEL.to_owned(),
                    add_status_effects: Some(FOOD_EFFECT.to_owned()),
                    potency: Some(1.0),
                    duration_override_s: None,
                    cooldown_id: None,
                    cooldown_s: Some(15),
                    disabled_in_combat: false,
                    use_tag: Some("MountItem".to_owned()),
                    mount_only: true,
                    required_inactive_limit: Some(FOOD_LIMIT.to_owned()),
                    remove_categories: Some("MountFoodUtility".to_owned()),
                }],
                effects: Vec::new(),
            });
            content.status_effects = StatusEffectRules::build(StatusEffectSource {
                effects: vec![EffectSourceRow {
                    status_id: FOOD_EFFECT.to_owned(),
                    categories: Some("MountFoodUtility+Food".to_owned()),
                    game_event: Some(FOOD_EVENT.to_owned()),
                    shows_damage_text: true,
                    ..EffectSourceRow::default()
                }],
                ..StatusEffectSource::default()
            });
            content.event_grants = EventGrants::build([(
                FOOD_EVENT.to_owned(),
                EventGrant {
                    progression: Some(("Riding".to_owned(), RIDING_REWARD)),
                    loot_limit: Some(FOOD_LIMIT.to_owned()),
                    limit_reached: Some("Mount_FoodDailyXP_LimitReached".to_owned()),
                    ..EventGrant::default()
                },
            )]);
            content.loot_limits = Some(LootLimitRules {
                limits: LootLimits::build([LimitRow {
                    id: FOOD_LIMIT,
                    count_limit: DAILY_USES,
                    expire_seconds: DAILY_EXPIRE_S,
                    max_limit_mult: 64_800,
                    replicated: true,
                }]),
                clock: ResetClock {
                    hour_utc: 3,
                    weekly_day: Weekday::Tuesday,
                },
            });
            content.progression = Progression::default().with_progressions([ProgressionSource {
                id: "Riding".to_owned(),
                max_level: 200,
                auto_rank_up: true,
                ranks: vec![(0, 1_000_000)],
            }]);
            content.quests = QuestBook::build(
                [ObjectiveRow {
                    id: FOOD_QUEST.to_owned(),
                    root: "t_root".to_owned(),
                    ..ObjectiveRow::default()
                }],
                TaskBook::new(vec![
                    task("t_root", "ConsecutiveTaskContainer", &["t_feed", "t_wait"]),
                    TaskRow {
                        kill_enemy_type: Some(FOOD_EVENT.to_owned()),
                        target_qty: Some(1),
                        ..task("t_feed", "TaskGameEvent", &[])
                    },
                    TaskRow {
                        kill_enemy_type: Some("Never".to_owned()),
                        target_qty: Some(1),
                        ..task("t_wait", "TaskGameEvent", &[])
                    },
                ]),
            );
            let entity = world.player_by_number(PLAYER).expect("player");
            let kits = gear_tests::kits_with(&[(GRUEL, "Consumable+MountConsumable")]);
            let mut gear = Gear::starter(&kits, gear_tests::CHARACTER, Some(gear_tests::BACKSTORY))
                .expect("gear");
            gear.renew(
                kits.item(GRUEL).expect("gruel"),
                RATIONS,
                slot::QUICK_SLOTS[2],
                0,
            );
            world.world.get_mut::<Player>(entity).expect("player").gear = Some(gear);
            world.start_quest(PLAYER, FOOD_QUEST, NOW).expect("start");
            let left = |world: &SimWorld| -> u32 {
                world
                    .world
                    .get::<Player>(entity)
                    .and_then(|player| player.gear.as_ref())
                    .map(Gear::carried)
                    .unwrap_or_default()
                    .iter()
                    .filter(|stack| stack.item_id == GRUEL)
                    .map(|stack| stack.quantity)
                    .sum()
            };
            let riding = |world: &SimWorld| -> u32 {
                let mastery = world
                    .content
                    .progression
                    .categorical("Riding")
                    .expect("riding");
                world
                    .world
                    .get::<Player>(entity)
                    .expect("player")
                    .masteries
                    .experience(mastery)
            };
            let played = |world: &SimWorld| -> bool {
                messages_to(&world.combat_messages, PLAYER)
                    .iter()
                    .any(|message| {
                        matches!(message, ServerMessage::AdjustFxScripts(script)
                            if script.script == lowercase_crc32(FOOD_EFFECT))
                    })
            };
            // A press of quick slot key 3 and its release; the food is used at its use time.
            let feed = |world: &mut SimWorld, counter: u16, at_ms: u64| {
                world.combat_messages.clear();
                world.attack_input(
                    &connection(PLAYER),
                    &[record(counter, vec![0, 0, QUICK_SLOT_3_KEY])],
                    at_ms,
                );
                world.attack_input(&connection(PLAYER), &[record(counter + 1, vec![0])], at_ms);
                let before = left(world);
                world.drink_due(at_ms + MOUNT_FOOD_USE_MS - 1, SERVER_TIME);
                assert_eq!(left(world), before, "nothing before the use time");
                world.drink_due(at_ms + MOUNT_FOOD_USE_MS, SERVER_TIME);
            };
            feed(&mut world, 1, NOW);
            assert_eq!(
                left(&world),
                RATIONS,
                "a mount food off the mount is not used"
            );
            assert!(world.ride(entity, lowercase_crc32(HORSE)));
            for day_use in 1..=DAILY_USES as u32 {
                feed(
                    &mut world,
                    (day_use * 2 + 1) as u16,
                    NOW + u64::from(day_use) * 1_000,
                );
                assert_eq!(left(&world), RATIONS - day_use, "use {day_use}");
                assert_eq!(
                    riding(&world),
                    RIDING_REWARD as u32 * day_use,
                    "use {day_use}"
                );
                assert!(played(&world), "the effect's visual script, use {day_use}");
            }
            assert!(
                world.quests_of_player(PLAYER)[0].run.is_done("t_feed"),
                "the quest step counts the food's event"
            );
            feed(&mut world, 9, NOW + 9_000);
            assert_eq!(
                left(&world),
                RATIONS - DAILY_USES as u32,
                "the daily limit of 3 is reached"
            );
            assert_eq!(riding(&world), RIDING_REWARD as u32 * DAILY_USES as u32);
        }

        fn world_with(achievements: Vec<u32>) -> SimWorld {
            world_admitted(achievements, false)
        }

        /// The sim of [`world_with`], with a player admitted at a login (`login`): a login's
        /// level flow holds the achievement bits, so achievements set later reach it.
        fn world_admitted(achievements: Vec<u32>, login: bool) -> SimWorld {
            let mounts = MountRules::build(
                [
                    MountRule {
                        id: HORSE.to_owned(),
                        crc: lowercase_crc32(HORSE),
                        mount_type: "Horse".to_owned(),
                        required_achievement: Some(HORSE_GIFT.to_owned()),
                        entitlement: false,
                        sorted_order: 1,
                        disabled_dye_channels: [false; MOUNT_DYE_LEN],
                    },
                    MountRule {
                        id: STORE_BEAR.to_owned(),
                        crc: lowercase_crc32(STORE_BEAR),
                        mount_type: "Horse".to_owned(),
                        required_achievement: None,
                        entitlement: true,
                        sorted_order: 2,
                        disabled_dye_channels: [false; MOUNT_DYE_LEN],
                    },
                ],
                [MountTypeRule {
                    id: "Horse".to_owned(),
                    summon_ms: SUMMON_MS,
                    voluntary_dismount_cooldown_ms: COOLDOWN_MS,
                    forced_dismount_cooldown_ms: COOLDOWN_MS,
                    required_achievement: HORSE_UNLOCK.to_owned(),
                    tradeskill: "Riding".to_owned(),
                }],
                [("ConfigMountRiding_0".to_owned(), movement())],
                HashMap::from([(
                    "riding".to_owned(),
                    vec![(0, "ConfigMountRiding_0".to_owned())],
                )]),
                [25],
            );
            let indexes = AchievementIndexes::build([
                (HORSE_UNLOCK.to_owned(), UNLOCK_BIT),
                (HORSE_GIFT.to_owned(), GIFT_BIT),
            ]);
            let mut world = world_with_spawns(Vec::new());
            world.achievement_rules = crate::world::level_flow::AchievementRules::of(&indexes, &[]);
            world.content = Arc::new(PlayerContent {
                mounts,
                achievements: indexes,
                store: StoreTable {
                    mounts: vec![StoreMount {
                        unlock: StoreEntitlement {
                            entitlement: BEAR_ENTITLEMENT.to_owned(),
                            name: "Ronin Bear Mount".to_owned(),
                        },
                        mount: STORE_BEAR.to_owned(),
                    }],
                    held_by_all: Vec::new(),
                },
                ..PlayerContent::default()
            });
            let mut admit = admit_at(PLAYER, 0.0, login);
            admit.progression.achievements = achievements;
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            world
        }

        fn mount_messages(outgoing: &[Outgoing]) -> Vec<MountServerMsg> {
            outgoing
                .iter()
                .filter(|out| out.connection == connection(PLAYER) && out.reliable)
                .flat_map(|out| &out.envelopes)
                .filter_map(|bytes| {
                    let envelope = Envelope::decode(bytes).ok()?;
                    MountServerMsg::decode(envelope.type_index()?, &envelope.body)?.ok()
                })
                .collect()
        }

        fn ticked(world: &mut SimWorld, now_ms: u64) -> Vec<MountServerMsg> {
            mount_messages(&world.tick(now_ms, SERVER_TIME).expect("tick").messages)
        }

        fn mount_of(world: &mut SimWorld) -> PlayerMount {
            let entity = world.player_by_number(PLAYER).expect("player");
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .mount
                .clone()
        }

        fn press(world: &mut SimWorld, counter: u16, now_ms: u64) -> Vec<MountServerMsg> {
            world.mount_input(
                &connection(PLAYER),
                &[record(counter, vec![SUMMON_KEY])],
                now_ms,
            );
            ticked(world, now_ms)
        }

        #[test]
        fn a_player_summons_rides_and_gets_off_as_retail() {
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            assert!(ticked(&mut world, NOW).is_empty(), "the card goes first");
            let enable = ticked(&mut world, NOW + TICK_MS);
            assert!(
                matches!(
                    enable[..],
                    [MountServerMsg::EnableMounts { enabled: true, .. }]
                ),
                "{enable:?}"
            );
            let start = NOW + 2 * TICK_MS;
            let pending = press(&mut world, 1, start);
            assert!(
                matches!(
                    pending[..],
                    [MountServerMsg::SummonPending {
                        summon_ms: SUMMON_MS,
                        ..
                    }]
                ),
                "{pending:?}"
            );
            let mount = mount_of(&mut world);
            assert_eq!(mount.selected, Some(lowercase_crc32(HORSE)));
            ticked(&mut world, start + u64::from(SUMMON_MS));
            assert!(mount_of(&mut world).authorization.authorized);
            world.mount_call(
                &connection(PLAYER),
                MountClientMsg::CompleteSummon {
                    header: FacetHeader::of(MOUNT_FACET, PLAYER),
                },
                start + u64::from(SUMMON_MS) + TICK_MS,
            );
            let entity = world.player_by_number(PLAYER).expect("player");
            assert!(
                world
                    .world
                    .get::<Player>(entity)
                    .expect("player")
                    .tracked
                    .is_mounted()
            );
            assert_eq!(mount_of(&mut world).regen_rate, 10.0);
            let limit = world.move_limits_of(entity).max_speed_mps;
            assert!(
                limit > world.settings.limits.max_speed_mps,
                "a rider may go faster than a runner"
            );
            let off_ms = start + u64::from(SUMMON_MS) + 2 * TICK_MS;
            let off = press(&mut world, 2, off_ms);
            assert!(
                matches!(
                    off[..],
                    [MountServerMsg::DismountNotification {
                        reason: DismountReason::Voluntary,
                        ..
                    }]
                ),
                "{off:?}"
            );
            assert!(
                !world
                    .world
                    .get::<Player>(entity)
                    .expect("player")
                    .tracked
                    .is_mounted()
            );
            let early = press(&mut world, 3, off_ms + TICK_MS);
            assert!(
                matches!(
                    early[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::OnCooldown,
                        ..
                    }]
                ),
                "the dismount cooldown: {early:?}"
            );
            let again = press(&mut world, 4, off_ms + COOLDOWN_MS + TICK_MS);
            assert!(matches!(again[..], [MountServerMsg::SummonPending { .. }]));
        }

        #[test]
        fn the_mount_quest_s_reward_box_gives_the_horse_it_summons() {
            // Content bundle 25810808: salvaging `MountItem_MountReward_Box_1` fires
            // `Reward_MountReward_Box_1`, whose `AchievementId` owns the horse. Retail (mount
            // quest capture, 283.210 s): the box opened (1733), then the first summon (5747).
            const BOX_EVENT: &str = "Reward_MountReward_Box_1";
            let mut world = world_admitted(vec![UNLOCK_BIT], true);
            let mut content = (*world.content).clone();
            content.event_grants = nw_content::rewards::EventGrants::build([(
                BOX_EVENT.to_owned(),
                nw_content::rewards::EventGrant {
                    achievement: Some(HORSE_GIFT.to_owned()),
                    ..nw_content::rewards::EventGrant::default()
                },
            )]);
            world.content = Arc::new(content);
            ticked(&mut world, NOW);
            ticked(&mut world, NOW + TICK_MS);
            let before = press(&mut world, 1, NOW + 2 * TICK_MS);
            assert!(
                before.iter().any(|message| matches!(
                    message,
                    MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::NoMountEquipped,
                        ..
                    }
                )),
                "no horse before the box: {before:?}"
            );
            let opened = NOW + 3 * TICK_MS;
            world.grant_event(PLAYER, BOX_EVENT, 1, opened);
            ticked(&mut world, opened);
            let summoned = press(&mut world, 2, opened + TICK_MS);
            assert!(
                summoned
                    .iter()
                    .any(|message| matches!(message, MountServerMsg::SummonPending { .. })),
                "{summoned:?}"
            );
            assert_eq!(mount_of(&mut world).selected, Some(lowercase_crc32(HORSE)));
        }

        #[test]
        fn a_summon_before_the_unlock_is_refused_as_retail() {
            let mut world = world_with(Vec::new());
            ticked(&mut world, NOW);
            assert!(ticked(&mut world, NOW + TICK_MS).is_empty(), "no 2749");
            let refused = press(&mut world, 1, NOW + 2 * TICK_MS);
            assert!(
                matches!(
                    refused[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::RequirementsNotMet,
                        notify: true,
                        ..
                    }]
                ),
                "{refused:?}"
            );
        }

        #[test]
        fn a_summon_in_combat_is_refused_and_a_hit_ends_a_running_one_as_retail() {
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            ticked(&mut world, NOW);
            ticked(&mut world, NOW + TICK_MS);
            let entity = world.player_by_number(PLAYER).expect("player");
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .combat
                .hit(NOW, false);
            let refused = press(&mut world, 1, NOW + 2 * TICK_MS);
            assert!(
                matches!(
                    refused[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::Combat,
                        flag: KEY_OR_PLAYER_CANCEL_FLAG,
                        detail: CANCEL_DETAIL,
                        ..
                    }]
                ),
                "no 5747 in combat: {refused:?}"
            );
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            ticked(&mut world, NOW);
            ticked(&mut world, NOW + TICK_MS);
            let entity = world.player_by_number(PLAYER).expect("player");
            press(&mut world, 1, NOW + 2 * TICK_MS);
            world.mount_hit(entity, 10.0, 100.0, false, NOW + 3 * TICK_MS);
            let ended = ticked(&mut world, NOW + 3 * TICK_MS);
            assert!(
                matches!(
                    ended[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::Combat,
                        flag: SERVER_CANCEL_FLAG,
                        ..
                    }]
                ),
                "{ended:?}"
            );
            let early = press(&mut world, 2, NOW + 4 * TICK_MS);
            assert!(
                matches!(
                    early[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::OnCooldown,
                        flag: KEY_OR_PLAYER_CANCEL_FLAG,
                        detail: ON_COOLDOWN_DETAIL,
                        ..
                    }]
                ),
                "{early:?}"
            );
        }

        #[test]
        fn a_summon_that_ends_in_the_air_is_cancelled_midair() {
            const HEIGHT_M: f32 = 20.0;
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            ticked(&mut world, NOW);
            let start = NOW + TICK_MS;
            press(&mut world, 1, start);
            let entity = world.player_by_number(PLAYER).expect("player");
            {
                let mut player = world.world.get_mut::<Player>(entity).expect("player");
                let at = player.tracked.position();
                player.tracked.drop_at(nw_sim_core::Position {
                    z: at.z + HEIGHT_M,
                    ..at
                });
            }
            let ended = ticked(&mut world, start + u64::from(SUMMON_MS));
            assert!(
                matches!(
                    ended[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::Midair,
                        flag: SERVER_CANCEL_FLAG,
                        ..
                    }]
                ),
                "{ended:?}"
            );
            assert!(!mount_of(&mut world).authorization.authorized);
        }

        #[test]
        fn a_crouch_cancels_the_summon_and_a_hit_on_a_rider_takes_stamina() {
            const CROUCH: u64 = 1 << 21;
            let mut world = world_with(vec![UNLOCK_BIT, GIFT_BIT]);
            ticked(&mut world, NOW);
            press(&mut world, 1, NOW + TICK_MS);
            world.mount_input(
                &connection(PLAYER),
                &[record(2, vec![0, 0, 0, 0, CROUCH])],
                NOW + 2 * TICK_MS,
            );
            let cancelled = ticked(&mut world, NOW + 2 * TICK_MS);
            assert!(
                matches!(
                    cancelled[..],
                    [MountServerMsg::SummonCancelled {
                        reason: SummonCancelReason::PlayerCancelled,
                        ..
                    }]
                ),
                "{cancelled:?}"
            );
            let start = NOW + 2 * TICK_MS + COOLDOWN_MS;
            press(&mut world, 3, start);
            ticked(&mut world, start + u64::from(SUMMON_MS));
            world.mount_call(
                &connection(PLAYER),
                MountClientMsg::CompleteSummon {
                    header: FacetHeader::of(MOUNT_FACET, PLAYER),
                },
                start + u64::from(SUMMON_MS),
            );
            let entity = world.player_by_number(PLAYER).expect("player");
            // A heavy hit, then a second one: the stamina is gone and the rider gets off.
            world.mount_hit(entity, 30.0, 100.0, false, start + u64::from(SUMMON_MS));
            assert_eq!(mount_of(&mut world).stamina, 50.0);
            world.mount_hit(entity, 30.0, 100.0, false, start + u64::from(SUMMON_MS));
            let off = ticked(&mut world, start + u64::from(SUMMON_MS) + TICK_MS);
            assert!(
                matches!(
                    off[..],
                    [MountServerMsg::DismountNotification {
                        reason: DismountReason::Combat,
                        ..
                    }]
                ),
                "{off:?}"
            );
        }
    }
}
