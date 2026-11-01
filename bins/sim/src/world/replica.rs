// SPDX-License-Identifier: AGPL-3.0-or-later
//! What other players see of a player (replication note, 2026-09-18): its look, the items that
//! show on it, its health, its block, where it faces and its slayer state (its action or its
//! movement), on its own ALC clock and numbering.
//! Retail sends a remote player in every tick of a small fight: every state with its number and
//! time, the place, the facing, the look direction and the slayer state, and the changes of the
//! vitals (15), the paperdoll (3183) and the damage receiver (1528). Before, our remote players
//! were a position alone, with an empty look, gear and health: others saw no facing, weapon,
//! block, health or death.
//!
//! A player is built once per tick into a [`Replica`] that every viewer shares; what changed is
//! found against [`ReplicaMemory`], the last state the viewers were sent. A player near a border
//! goes to the other sim as a [`PlayerGhost`], which carries the same [`ReplicaSource`].
//!
//! Each viewer gets the remote players by priority ([`RemotePlan`]): one in combat or near every
//! tick, farther ones less often, at most [`REMOTE_GROUPS_PER_TICK`] per tick, a card refresh and
//! the longest waiting first; a new player's card and a change always go.

use std::collections::HashMap;

use nw_bus::{
    GHOST_VISIBLE_ITEMS, GhostItem, GhostMount, GhostSlayer, GhostSlotChange, GhostUpperSlots,
    ItemLookState, PlayerGhost,
};
use nw_messages::mount::riding_active_map;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    AlcStamp, AlcState, BundleGroup, BundleMember, CardPlace, DamageReceiverState, Hands,
    HealthChange, InputPosition, ItemLook, LookDir, MemberState, MessageError, PaperdollPublic,
    PaperdollState, PlayerGear, PlayerLook, PlayerStates, RemoteStep, RemoteUpper,
    SecondSlayerState, ShownItem, SlayerState, SlotChange, StepPlace, UpperSlots, VitalsData,
    put_looking_through_loadout, put_member,
};
use nw_sim_core::Position;

use super::members::{EntityKind, OwnClock, PLAYER_ALC_COMPONENT, StateNumber, entity_group};
use super::mount::remote_member;

/// The own player component of the vitals.
const VITALS_COMPONENT: u64 = nw_messages::PLAYER_VITALS_COMPONENT;
/// The player component of the damage receiver (`isBlockActive`; player component table).
const DAMAGE_RECEIVER_COMPONENT: u64 = 14;
/// The player component of the paperdoll (player component table).
pub(super) const PAPERDOLL_COMPONENT: u64 = 62;
/// A remote player this near the viewer goes every tick (our choice: a melee and short-range
/// fight; retail sent the duel's remote player every 17 ms tick).
const NEAR_M: f32 = 30.0;
/// One this near goes every second tick; farther ones every [`FAR_PERIOD_TICKS`] (our choice:
/// retail's 45-vs-45 war sent remote players at a median 33 ms and a p90 135 ms).
const MID_M: f32 = 60.0;
const MID_PERIOD_TICKS: u64 = 2;
const FAR_PERIOD_TICKS: u64 = 4;
/// Most remote players one viewer gets in one tick besides cards and changes (our choice: about
/// 1 KB of 40-byte remote states, retail's p99 datagram of 1,028 bytes).
pub(super) const REMOTE_GROUPS_PER_TICK: usize = 24;

/// What a player is now, for the players who see it: from its own sim, or from a ghost.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ReplicaSource {
    pub(super) character: [u8; RAW_ID_LEN],
    pub(super) look: Option<PlayerLook>,
    pub(super) health: u32,
    pub(super) max_health: u32,
    pub(super) view: Option<LookDir>,
    /// The number of its ALC states from its client's input ([`StateNumber`]).
    pub(super) number: Option<StateNumber>,
    /// Its slayer state: the action it plays, from its client or the server's follow-up, or its
    /// movement ([`super::slayer`]).
    pub(super) action: Option<SlayerState>,
    /// Its slayer slots past the first and its marker, while its own states echo a change of
    /// them ([`super::slayer::SlayerTrack::remote_upper`]): its block, aim, aimed shot, reload
    /// and draw.
    pub(super) upper: Option<RemoteUpper>,
    pub(super) blocking: bool,
    pub(super) spawned_ms: u64,
    pub(super) gear_version: u64,
    pub(super) items: Vec<ShownItem>,
    pub(super) hands: Hands,
    pub(super) in_combat: bool,
    /// The entity it locks on or homes to.
    pub(super) target: Option<u64>,
    /// It has its inventory open (`lookingThroughLoadout`; retail sent others the change too,
    /// newchar 2026-09-18 at 1792696377099).
    pub(super) looking_through_loadout: bool,
    /// The mount it rides as others see it (mount state slots 2 and 3; Observed: other players'
    /// cards carried the ridden mount, or 0 and the flags 0x04 for a player on foot).
    pub(super) mount: GhostMount,
}

impl ReplicaSource {
    /// The ghost of this player for a sim next door.
    pub(super) fn ghost(&self) -> PlayerGhost {
        let mut items = [None; GHOST_VISIBLE_ITEMS];
        for (shown, worn) in items.iter_mut().zip(&self.items) {
            *shown = u8::try_from(worn.slot).ok().map(|slot| GhostItem {
                slot,
                item_index: worn.item_index,
                look: ItemLookState {
                    dye: worn.look.dye,
                    appearance: worn.look.appearance,
                    skin_dye: worn.look.skin_dye,
                },
            });
        }
        PlayerGhost {
            character: uuid::Uuid::from_bytes(self.character),
            look: self.look.map(|look| look.choices()),
            health: self.health,
            max_health: self.max_health,
            facing: self.view.map(|view| view.yaw),
            pitch: self.view.map(|view| view.pitch),
            target: self.target,
            state_number: self.number.map(|number| (number.number, number.since_ms)),
            blocking: self.blocking,
            spawned_ms: self.spawned_ms,
            gear_version: self.gear_version,
            drawn: self.hands.drawn,
            active: self.hands.active,
            attached: self.hands.attached,
            looking_through_loadout: self.looking_through_loadout,
            mount: self.mount,
            items,
            action: self.action.map(|action| GhostSlayer {
                state: action.state,
                started: action.started,
                sequence: action.sequence,
                second_started: action.second.map(|second| second.started),
                second_playing: action.second.and_then(|second| second.playing),
                second_stopped: action.second.is_some_and(|second| second.stopped),
            }),
            upper: self.upper.map(|upper| GhostUpperSlots {
                second: ghost_slot(upper.slots.second),
                third: ghost_slot(upper.slots.third),
                fourth: ghost_slot(upper.slots.fourth),
                marker: upper.slots.marker,
                after_state: upper.after_state,
            }),
        }
    }

    /// The player of a ghost from a sim next door.
    pub(super) fn of_ghost(ghost: &PlayerGhost) -> Self {
        Self {
            character: *ghost.character.as_bytes(),
            look: ghost.look.map(PlayerLook::of_choices),
            health: ghost.health,
            max_health: ghost.max_health,
            view: ghost.facing.map(|yaw| LookDir {
                yaw,
                pitch: ghost.pitch.unwrap_or_default(),
            }),
            number: ghost
                .state_number
                .map(|(number, since_ms)| StateNumber { number, since_ms }),
            action: ghost.action.map(|action| SlayerState {
                state: action.state,
                started: action.started,
                sequence: action.sequence,
                second: action.second_started.map(|started| SecondSlayerState {
                    started,
                    playing: action.second_playing,
                    stopped: action.second_stopped,
                }),
            }),
            upper: ghost.upper.map(|upper| RemoteUpper {
                slots: UpperSlots {
                    second: slot_of_ghost(upper.second),
                    third: slot_of_ghost(upper.third),
                    fourth: slot_of_ghost(upper.fourth),
                    marker: upper.marker,
                },
                after_state: upper.after_state,
            }),
            blocking: ghost.blocking,
            spawned_ms: ghost.spawned_ms,
            gear_version: ghost.gear_version,
            items: ghost
                .items
                .iter()
                .flatten()
                .map(|item| ShownItem {
                    slot: u32::from(item.slot),
                    item_index: item.item_index,
                    look: ItemLook {
                        dye: item.look.dye,
                        appearance: item.look.appearance,
                        skin_dye: item.look.skin_dye,
                    },
                })
                .collect(),
            hands: Hands {
                drawn: ghost.drawn,
                active: ghost.active,
                attached: ghost.attached,
                ammo: [None; 2],
            },
            // A ghost's fight is its own sim's: near, it goes every tick anyway.
            in_combat: false,
            target: ghost.target,
            looking_through_loadout: ghost.looking_through_loadout,
            mount: ghost.mount,
        }
    }

    /// Its gear as others see it: the public paperdoll of the items that show (its sheathe,
    /// attachment and active maps, slots and looks); the owner part (durability, instances, ammo,
    /// loadouts), its bag and other containers stay its own (Observed: retail sends other
    /// players paperdoll slot 2 alone).
    fn gear(&self) -> PlayerGear {
        PlayerGear {
            paperdoll: PaperdollState {
                owner: None,
                public: Some(PaperdollPublic {
                    active: Some(riding_active_map(self.hands, self.riding())),
                    ..PaperdollPublic::of(&self.items, self.gear_version, self.hands)
                }),
            },
            ..PlayerGear::default()
        }
    }

    /// Whether it rides a mount.
    const fn riding(&self) -> bool {
        self.mount.mount_id != 0
    }
}

/// One upper slayer slot of a player for its ghost.
const fn ghost_slot(slot: SlotChange) -> GhostSlotChange {
    GhostSlotChange {
        state: slot.state,
        started: slot.started,
        sequence: slot.sequence,
    }
}

/// One upper slayer slot of a ghost's player.
const fn slot_of_ghost(slot: GhostSlotChange) -> SlotChange {
    SlotChange {
        state: slot.state,
        started: slot.started,
        sequence: slot.sequence,
    }
}

/// What the viewers were last sent of one player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ReplicaMemory {
    health: u32,
    max_health: u32,
    blocking: bool,
    gear_version: u64,
    hands: Hands,
    looking_through_loadout: bool,
    mount: GhostMount,
    /// The start and sequence of its action.
    action: Option<(Option<u16>, u64)>,
    /// The server time of its last new life (ms since 2000-01-01).
    life_ms: u64,
    /// The `pos10` every viewer places it from: its states move it by offsets from it (Observed:
    /// the retail duel's remote player got a new `pos10` in 62 of its 453 placed states; the
    /// client drops a lock on a target that keeps getting a new one, Inferred, as for mobs in
    /// fadbf8b).
    anchor: Option<InputPosition>,
}

/// One player as every viewer gets it this tick.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Replica {
    source: ReplicaSource,
    gear: PlayerGear,
    stamp: AlcStamp,
    health_change: Option<HealthChange>,
    /// The change of its public paperdoll: the whole of it after a change of its items, the
    /// changed hand maps alone after a change of its hands ([`PaperdollPublic::hands_change`]).
    paperdoll_change: Option<PaperdollPublic>,
    /// The change of its `lookingThroughLoadout`.
    loadout_change: Option<bool>,
    /// The change of its mount state as others see it.
    mount_change: Option<BundleMember>,
    block_changed: bool,
    /// It started an action: the others see it now.
    action_changed: bool,
    /// Its own state echoes a change of its upper slots: the others see it now.
    upper_due: bool,
    /// Where its state of this tick puts it: an offset from its anchor, or a new anchor.
    place: Option<StepPlace>,
    /// Its anchor after this tick.
    anchor: Option<InputPosition>,
    /// It got a new anchor while the viewers had an older one: every viewer gets it now, so no
    /// viewer places its later offsets from the old one.
    anchor_moved: bool,
}

impl Replica {
    /// The player of `source` at `position` at server time `server_ms`, with what changed since
    /// `seen` (none for a player first seen), and what the viewers saw then.
    pub(super) fn new(
        source: ReplicaSource,
        position: Position,
        seen: Option<ReplicaMemory>,
        server_ms: u64,
    ) -> (Self, ReplicaMemory) {
        let seen_anchor = seen.and_then(|seen| seen.anchor);
        let place = InputPosition::from_metres(position.x, position.y, position.z)
            .map(|at| StepPlace::of(seen_anchor, at));
        let anchor = match place {
            Some(StepPlace::Anchor(at)) => Some(at),
            _ => seen_anchor,
        };
        let anchor_moved = seen.is_some() && matches!(place, Some(StepPlace::Anchor(_)));
        let clock = OwnClock::at(source.spawned_ms, server_ms, source.number);
        let health = source.health;
        let action = source
            .action
            .map(|action| (action.started, action.sequence));
        let action_changed = seen.is_some_and(|seen| seen.action != action);
        let upper_due = seen.is_some() && source.upper.is_some();
        let gear = source.gear();
        let paperdoll_change = seen.and_then(|seen| {
            // A mount-up or a dismount changes the active map ([`riding_active_map`]).
            let riding_changed = (seen.mount.mount_id != 0) != source.riding();
            if seen.gear_version == source.gear_version {
                let mut change = PaperdollPublic::hands_change(seen.hands, source.hands);
                if riding_changed || change.active.is_some() {
                    change.active = Some(riding_active_map(source.hands, source.riding()));
                }
                Some(change).filter(|change| *change != PaperdollPublic::default())
            } else {
                gear.paperdoll.public.clone()
            }
        });
        let loadout_change = seen
            .filter(|seen| seen.looking_through_loadout != source.looking_through_loadout)
            .map(|_| source.looking_through_loadout);
        let mount_change = seen.and_then(|seen| remote_member(Some(seen.mount), source.mount));
        let (health_change, block_changed, life_ms) = match seen {
            None => (None, false, source.spawned_ms),
            Some(seen) => {
                let life = match (seen.health, health) {
                    (shown, 0) if shown > 0 => Some(VitalsData::player_dead(seen.life_ms)),
                    (0, now) if now > 0 => Some(VitalsData::player_alive(server_ms)),
                    _ => None,
                };
                let life_ms = if health > 0 && seen.health == 0 {
                    server_ms
                } else {
                    seen.life_ms
                };
                let max = (seen.max_health != source.max_health).then_some(source.max_health);
                let change = (seen.health != health || max.is_some()).then_some(HealthChange {
                    health,
                    first_damage: seen.health == source.max_health,
                    life,
                    max,
                });
                (change, seen.blocking != source.blocking, life_ms)
            }
        };
        let memory = ReplicaMemory {
            health,
            max_health: source.max_health,
            blocking: source.blocking,
            gear_version: source.gear_version,
            hands: source.hands,
            looking_through_loadout: source.looking_through_loadout,
            mount: source.mount,
            action,
            life_ms,
            anchor,
        };
        let replica = Self {
            gear,
            stamp: clock.stamp(),
            source,
            health_change,
            paperdoll_change,
            loadout_change,
            mount_change,
            block_changed,
            action_changed,
            upper_due,
            place,
            anchor,
            anchor_moved,
        };
        (replica, memory)
    }

    /// Whether it changed this tick: it goes to every viewer now, whatever its priority.
    pub(super) const fn changed(&self) -> bool {
        self.health_change.is_some()
            || self.paperdoll_change.is_some()
            || self.loadout_change.is_some()
            || self.mount_change.is_some()
            || self.block_changed
            || self.action_changed
            || self.anchor_moved
            || self.upper_due
    }

    /// Whether it is in combat: it goes first.
    pub(super) const fn in_combat(&self) -> bool {
        self.source.in_combat
    }

    /// Whether it fights the player `viewer` (of replica `viewer_replica`): one targets the
    /// other, or both are in combat. The viewer then never loses sight of it in a crowd.
    pub(super) fn fights(&self, number: u64, viewer: u64, viewer_replica: &Self) -> bool {
        self.source.target == Some(viewer)
            || viewer_replica.source.target == Some(number)
            || (self.source.in_combat && viewer_replica.source.in_combat)
    }

    /// Its group in slot `interest` for a viewer, at `position`: with its card when `carded`
    /// (its look, the items that show and its health, then its full first state on its clock),
    /// else its state of this tick and its changes. `None` for a position that is not finite.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a state cannot be encoded.
    pub(super) fn group(
        &self,
        interest: u16,
        entity: u64,
        position: Position,
        carded: bool,
    ) -> Result<Option<BundleGroup>, MessageError> {
        let Some(pos) = InputPosition::from_metres(position.x, position.y, position.z) else {
            return Ok(None);
        };
        let source = &self.source;
        if carded {
            let states = PlayerStates {
                character_id: source.character,
                look: source.look,
                health: Some((source.health, source.max_health)),
                gear: Some(&self.gear),
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
                mana: None,
            };
            let Some(mut group) = entity_group(
                interest,
                entity,
                EntityKind::PLAYER,
                position,
                true,
                Some(states),
            ) else {
                return Ok(None);
            };
            if source.looking_through_loadout {
                put_looking_through_loadout(&mut group.members, true);
            }
            if let Some(mount) = remote_member(None, source.mount) {
                put_member(&mut group.members, mount);
            }
            if let Some(alc) = group.members.last_mut() {
                let place = CardPlace {
                    anchor: self.anchor,
                    at: pos,
                };
                alc.state = MemberState::Alc(AlcState::spawn(place, self.stamp)?);
            }
            return Ok(Some(group));
        }
        let mut members = vec![BundleMember {
            component: PLAYER_ALC_COMPONENT,
            state: MemberState::Alc(AlcState::remote_step(&RemoteStep {
                stamp: self.stamp,
                position: self.place,
                facing: source.view.map(|view| view.yaw),
                look: source.view,
                slayer: source.action,
                sequence_time: None,
                upper: source.upper,
            })?),
        }];
        if let Some(change) = self.health_change {
            members.push(BundleMember {
                component: VITALS_COMPONENT,
                state: MemberState::Vitals(change.vitals()),
            });
        }
        if let Some(public) = &self.paperdoll_change {
            members.push(BundleMember {
                component: PAPERDOLL_COMPONENT,
                state: MemberState::Paperdoll(Box::new(PaperdollState {
                    owner: None,
                    public: Some(public.clone()),
                })),
            });
        }
        if let Some(looking) = self.loadout_change {
            put_looking_through_loadout(&mut members, looking);
        }
        members.extend(self.mount_change.clone());
        if self.block_changed {
            members.push(BundleMember {
                component: DAMAGE_RECEIVER_COMPONENT,
                state: MemberState::DamageReceiver(DamageReceiverState {
                    is_block_active: Some(source.blocking),
                    ..DamageReceiverState::default()
                }),
            });
        }
        Ok(Some(BundleGroup {
            interest: u64::from(interest),
            members,
        }))
    }
}

/// How long a viewer may wait for a remote player `distance_m` away (in ticks): every tick near
/// or in combat, then less often with distance.
const fn period_ticks(distance_m: f32, in_combat: bool) -> u64 {
    if in_combat || distance_m <= NEAR_M {
        1
    } else if distance_m <= MID_M {
        MID_PERIOD_TICKS
    } else {
        FAR_PERIOD_TICKS
    }
}

/// One remote player a viewer may get this tick.
#[derive(Debug, Clone, Copy)]
pub(super) struct RemoteCandidate<'a> {
    pub(super) number: u64,
    pub(super) distance_m: f32,
    pub(super) replica: &'a Replica,
    /// The viewer has no slot for it yet: its card goes now.
    pub(super) new: bool,
    /// Its card refresh is due.
    pub(super) refresh: bool,
}

/// Which remote players one viewer gets this tick.
#[derive(Debug, Default)]
pub(super) struct RemotePlan {
    /// The bundle step each remote player was last sent in, by entity number.
    last_sent: HashMap<u64, u64>,
}

impl RemotePlan {
    /// The remote players of `candidates` that the viewer gets in its bundle step `step`: every
    /// new one and every change, then the ones whose wait came, card refreshes first, then those
    /// in combat, then the longest waiting, then the nearest, at most [`REMOTE_GROUPS_PER_TICK`].
    /// Players no longer seen are forgotten.
    pub(super) fn pick(&mut self, step: u64, candidates: &[RemoteCandidate<'_>]) -> Vec<u64> {
        self.last_sent.retain(|number, _| {
            candidates
                .iter()
                .any(|candidate| candidate.number == *number)
        });
        let mut picked: Vec<u64> = Vec::new();
        let mut waiting: Vec<(bool, bool, u64, f32, u64)> = Vec::new();
        for candidate in candidates {
            let replica = candidate.replica;
            if candidate.new || replica.changed() {
                picked.push(candidate.number);
                continue;
            }
            let age = self
                .last_sent
                .get(&candidate.number)
                .map_or(u64::MAX, |sent| step.saturating_sub(*sent));
            if candidate.refresh || age >= period_ticks(candidate.distance_m, replica.in_combat()) {
                waiting.push((
                    !candidate.refresh,
                    !replica.in_combat(),
                    u64::MAX - age,
                    candidate.distance_m,
                    candidate.number,
                ));
            }
        }
        waiting.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.total_cmp(&b.3))
                .then(a.4.cmp(&b.4))
        });
        picked.extend(
            waiting
                .into_iter()
                .take(REMOTE_GROUPS_PER_TICK)
                .map(|(.., number)| number),
        );
        for number in &picked {
            self.last_sent.insert(*number, step);
        }
        picked
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    const SPAWNED_MS: u64 = 1_000_000;
    const MAX_HEALTH: u32 = 856;
    /// A remote aim of the retail arena capture (2026-09-13): `19=2e 20=0dea 21=9606 48=28`.
    const AIM_STATE: u64 = 0x2e;
    const AIM_STARTED: u16 = 0x0dea;
    const AIM_SEQUENCE: u64 = 406;
    const AIM_MARKER: u64 = 0x28;
    const AT: Position = Position {
        x: 7300.0,
        y: 3090.0,
        z: 45.0,
    };

    pub(in crate::world) fn source() -> ReplicaSource {
        ReplicaSource {
            character: [7; RAW_ID_LEN],
            look: None,
            health: MAX_HEALTH,
            max_health: MAX_HEALTH,
            view: Some(LookDir {
                yaw: 1.0,
                pitch: 0.25,
            }),
            number: Some(StateNumber {
                number: 0x28c5,
                since_ms: SPAWNED_MS + 10,
            }),
            action: None,
            upper: None,
            blocking: false,
            spawned_ms: SPAWNED_MS,
            gear_version: 3,
            items: vec![ShownItem {
                slot: nw_messages::paperdoll::slot::MAIN_HAND_OPTION_1,
                item_index: 42,
                look: ItemLook {
                    dye: [4, 0, 0, 0],
                    appearance: 7,
                    skin_dye: [0; 4],
                },
            }],
            hands: Hands::default(),
            in_combat: false,
            target: None,
            looking_through_loadout: false,
            mount: GhostMount::default(),
        }
    }

    #[test]
    fn others_get_the_public_paperdoll_only() {
        let gear = source().gear();
        assert_eq!(
            gear.paperdoll.owner, None,
            "no durability, instance or ammo"
        );
        let public = gear.paperdoll.public.expect("public");
        assert_eq!(
            public
                .visible_slots
                .and_then(|slots| slots.full().map(|list| list.elements.clone())),
            Some(vec![nw_messages::paperdoll::slot::MAIN_HAND_OPTION_1])
        );
    }

    /// The source of a player whose own state echoes the retail aim.
    fn aiming() -> ReplicaSource {
        ReplicaSource {
            upper: Some(RemoteUpper {
                slots: UpperSlots {
                    third: SlotChange {
                        state: Some(AIM_STATE),
                        started: Some(AIM_STARTED),
                        sequence: Some(AIM_SEQUENCE),
                    },
                    marker: Some(AIM_MARKER),
                    ..UpperSlots::default()
                },
                after_state: true,
            }),
            ..source()
        }
    }

    #[test]
    fn a_ghost_carries_the_whole_replica() {
        for source in [source(), aiming()] {
            let back = ReplicaSource::of_ghost(&source.ghost());
            assert_eq!(back, source);
        }
    }

    #[test]
    fn others_see_the_block_and_aim_slots_while_they_echo() {
        let (_, seen) = Replica::new(source(), AT, None, SPAWNED_MS + 10);
        let (aim, seen) = Replica::new(aiming(), AT, Some(seen), SPAWNED_MS + 20);
        assert!(aim.changed(), "every viewer gets the aim now");
        let group = aim
            .group(5, 9, AT, false)
            .expect("encodes")
            .expect("a group");
        let MemberState::Alc(state) = &group.members[0].state else {
            panic!("the state first: {group:?}");
        };
        let children = state.slot0_children().expect("walks");
        let child = |index: usize| {
            children
                .iter()
                .find(|(child, _)| *child == index)
                .map(|(_, bytes)| bytes.clone())
        };
        assert_eq!(child(19), Some(vec![0x2e]), "the third slot's state");
        assert_eq!(child(20), Some(AIM_STARTED.to_be_bytes().to_vec()));
        assert_eq!(
            child(21),
            Some(vec![0x96, 0x06]),
            "the retail sequence bytes"
        );
        assert_eq!(child(48), Some(vec![0x28]), "the marker");
        let (quiet, _) = Replica::new(source(), AT, Some(seen), SPAWNED_MS + 30);
        assert!(!quiet.changed(), "the echo is over");
        let group = quiet
            .group(5, 9, AT, false)
            .expect("encodes")
            .expect("a group");
        let MemberState::Alc(state) = &group.members[0].state else {
            panic!("the state first: {group:?}");
        };
        assert!(
            state
                .slot0_children()
                .expect("walks")
                .iter()
                .all(|(child, _)| !(16..=24).contains(child) && *child != 48),
            "no upper slot after the echo"
        );
    }

    #[test]
    fn changes_are_found_against_what_viewers_saw() {
        let (first, seen) = Replica::new(source(), AT, None, SPAWNED_MS + 10);
        assert!(!first.changed(), "a first sight is its card");
        let hit = ReplicaSource {
            health: 500,
            blocking: true,
            gear_version: 4,
            ..source()
        };
        let (changed, seen) = Replica::new(hit.clone(), AT, Some(seen), SPAWNED_MS + 20);
        assert_eq!(
            changed
                .health_change
                .map(|change| (change.health, change.first_damage)),
            Some((500, true)),
            "the first damage from full health"
        );
        assert!(changed.paperdoll_change.is_some() && changed.block_changed);
        let (dead, seen) = Replica::new(
            ReplicaSource { health: 0, ..hit },
            AT,
            Some(seen),
            SPAWNED_MS + 30,
        );
        assert!(
            dead.health_change
                .and_then(|change| change.life)
                .is_some_and(|life| life.is_dead())
        );
        let (again, _) = Replica::new(source(), AT, Some(seen), SPAWNED_MS + 40);
        assert!(
            again
                .health_change
                .and_then(|change| change.life)
                .is_some_and(|life| !life.is_dead()),
            "a new life"
        );
    }

    #[test]
    fn others_get_a_change_of_the_hands_as_the_changed_maps_alone() {
        let (_, seen) = Replica::new(source(), AT, None, SPAWNED_MS);
        let drawn = ReplicaSource {
            hands: Hands {
                drawn: Some(nw_messages::paperdoll::slot::MAIN_HAND_OPTION_1),
                attached: true,
                ..Hands::default()
            },
            ..source()
        };
        let (draw, seen) = Replica::new(drawn.clone(), AT, Some(seen), SPAWNED_MS + 33);
        assert!(draw.changed(), "every viewer sees the draw now");
        let public = draw.paperdoll_change.clone().expect("a paperdoll change");
        assert!(public.sheathed.is_some() && public.attached.is_some());
        assert_eq!(
            (public.visible_slots, public.visuals),
            (None, None),
            "no slot or look list (retail newchar 2026-09-18)"
        );
        let group = draw.group(9, 1, AT, false).expect("group").expect("state");
        assert!(group.members.iter().any(|member| matches!(
            &member.state,
            MemberState::Paperdoll(paperdoll) if paperdoll.owner.is_none()
        )));
        let (same, seen) = Replica::new(drawn.clone(), AT, Some(seen), SPAWNED_MS + 66);
        assert_eq!(same.paperdoll_change, None, "nothing new");
        // The inventory open goes to the viewers as `lookingThroughLoadout`.
        let looking = ReplicaSource {
            looking_through_loadout: true,
            ..drawn
        };
        let (open, _) = Replica::new(looking, AT, Some(seen), SPAWNED_MS + 99);
        assert_eq!(open.loadout_change, Some(true));
        let group = open.group(9, 1, AT, false).expect("group").expect("state");
        assert!(group.members.iter().any(|member| {
            member.component == nw_messages::PLAYER_COMPONENT_COMPONENT
                && member.state
                    == MemberState::PlayerComponent(
                        nw_messages::PlayerComponentState::looking_through_loadout(true),
                    )
        }));
    }

    #[test]
    fn others_see_the_mount_with_the_card_and_after_each_change() {
        /// `Mount_MTX_5_lion_basic`.
        const LION_BASIC: u32 = 0x0c13_84ae;
        let mount_member = |group: &BundleGroup| {
            group
                .members
                .iter()
                .find(|member| member.component == nw_messages::MOUNT_COMPONENT)
                .cloned()
        };
        let (first, seen) = Replica::new(source(), AT, None, SPAWNED_MS);
        let card = first.group(9, 1, AT, true).expect("group").expect("card");
        assert_eq!(
            mount_member(&card),
            remote_member(None, GhostMount::default()),
            "a card names no mount"
        );
        let riding = ReplicaSource {
            mount: GhostMount {
                mount_id: LION_BASIC,
                flags: nw_messages::RemoteFlags::STAMINA_FULL,
                dye: [0; nw_bus::MOUNT_DYE_LEN],
            },
            ..source()
        };
        let (up, seen) = Replica::new(riding.clone(), AT, Some(seen), SPAWNED_MS + 33);
        assert!(up.changed(), "a mount-up goes to every viewer now");
        let group = up.group(9, 1, AT, false).expect("group").expect("state");
        assert_eq!(
            mount_member(&group),
            remote_member(Some(GhostMount::default()), riding.mount)
        );
        let (same, _) = Replica::new(riding, AT, Some(seen), SPAWNED_MS + 66);
        assert_eq!(same.mount_change, None, "nothing new");
    }

    #[test]
    fn a_player_moves_by_offsets_from_an_anchor_every_viewer_shares() {
        let east = |metres: f32| Position {
            x: AT.x + metres,
            ..AT
        };
        let (first, seen) = Replica::new(source(), AT, None, SPAWNED_MS);
        assert!(
            matches!(first.place, Some(StepPlace::Anchor(_))),
            "a first anchor"
        );
        assert!(!first.changed());
        // A step within reach is an offset, and nothing forces it to every viewer.
        let (near, seen) = Replica::new(source(), east(1.0), Some(seen), SPAWNED_MS + 33);
        assert!(matches!(near.place, Some(StepPlace::Offset(_))));
        assert!(!near.changed());
        assert_eq!(near.anchor, first.anchor, "the anchor stays");
        // Out of reach: a new anchor, which every viewer gets now.
        let (far, seen) = Replica::new(source(), east(3.0), Some(seen), SPAWNED_MS + 66);
        assert!(matches!(far.place, Some(StepPlace::Anchor(_))));
        assert!(far.changed(), "a new anchor goes to every viewer");
        // A viewer that sees it from now on gets the shared anchor with the offset.
        let (late, _) = Replica::new(source(), east(3.5), Some(seen), SPAWNED_MS + 99);
        assert_eq!(late.anchor, far.anchor);
        let group = late
            .group(9, 1, east(3.5), true)
            .expect("group")
            .expect("card");
        let MemberState::Alc(card) = &group.members.last().expect("alc").state else {
            panic!("the card ends with its ALC");
        };
        assert_eq!(card.pos10, far.anchor, "the card is on the shared anchor");
    }

    #[test]
    fn near_and_fighting_players_go_first_and_far_ones_wait() {
        let (calm, _) = Replica::new(source(), AT, None, SPAWNED_MS);
        let (fighting, _) = Replica::new(
            ReplicaSource {
                in_combat: true,
                ..source()
            },
            AT,
            None,
            SPAWNED_MS,
        );
        let candidate = |number, distance_m, replica| RemoteCandidate {
            number,
            distance_m,
            replica,
            new: false,
            refresh: false,
        };
        let mut plan = RemotePlan::default();
        let far = MID_M + 1.0;
        let candidates = [candidate(1, far, &calm), candidate(2, far, &fighting)];
        assert_eq!(plan.pick(0, &candidates), [2, 1], "a fight first");
        assert_eq!(plan.pick(1, &candidates), [2], "the far one waits");
        assert_eq!(plan.pick(FAR_PERIOD_TICKS, &candidates), [2, 1]);
        let many: Vec<RemoteCandidate<'_>> = (0..100_u64)
            .map(|number| candidate(number, 1.0, &calm))
            .collect();
        let mut plan = RemotePlan::default();
        let first = plan.pick(0, &many);
        assert_eq!(first.len(), REMOTE_GROUPS_PER_TICK, "the budget holds");
        let second = plan.pick(1, &many);
        assert!(
            second.iter().all(|number| !first.contains(number)),
            "the ones that waited go next"
        );
    }
}
