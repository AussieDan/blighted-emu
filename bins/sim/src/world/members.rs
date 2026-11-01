// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bundle groups of the entities a client sees (world-entry note, "Spawn condition traced").
//! Each entity is one group in its own slot ([`super::slots`]); the own player is slot
//! [`LOCAL_PLAYER_INTEREST`]. A group holds the entity's spawn card (type 10, component
//! [`GDE_METADATA_COMPONENT`]) when the entity comes into view (and again once per
//! [`CARD_REFRESH_BUNDLES`] bundles while it stays in view), then its position in an
//! `ALCReplicatedState` (type 11) carrying the `pos10` child, as the retail bundles do. A player's
//! card names the player prefab ([`PLAYER_SLICE_ASSET`]): the client registers its local player
//! only when that prefab is portrayed in slot 1. A mob's card ids are our own values: the mob
//! prefab is Unknown.
//!
//! Bundles split by bytes: an unreliable carrier message must fit one datagram (it is never
//! chunked), so a bundle's groups and stops take at most the bytes of [`MAX_BUNDLE_MEMBERS`]
//! position members, as before M6-B.

use nw_bus::MobGhost;
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_messages::{
    AlcStamp, AlcState, AssetId, BOSS_PHASE_TYPE_INDEX, BundleGroup, BundleMember, BundlePayload,
    CardPlace, DATA_BITS_ACTIVE, DEPLETED_GATHERABLE, DYNAMIC_SLICE_SUB_ID, EffectMap,
    GATHERABLE_CONTROLLER_TYPE_INDEX, GDE_METADATA_COMPONENT, GatherableControllerState,
    GdeMetadataReplicatedState, InputPosition, LOCAL_PLAYER_INTEREST, LookDir, LookTargetingState,
    MAX_BUNDLE_INDEXES, MemberState, MessageError, PLAYER_SLICE_ASSET, POS3_LEN, PlayerStates,
    RemoteEffect, ReplicatedStateBundle, SPAWN_STATE_ID, SPAWN_STATE_MS, SPAWN_TELEPORT,
    STATUS_EFFECTS_TYPE_INDEX, ServerMessage, StatusEffectsState, StepPlace, TeleportStep,
    VitalsData, VitalsState, boss_phase_change, change_members, offset_from, player_members,
    raw_id_of_entity,
};
use nw_sim_core::Position;

use super::MAX_BUNDLE_MEMBERS;
use super::mob_timeline::MobFrame;
use super::slayer::SlayerDue;
use super::slots::Slots;
use crate::player_content::MobLook;

/// Most bytes of a position member: a 5-byte entity number, a 1-byte class, a 12-byte body.
const MAX_POSITION_MEMBER_BYTES: usize = 18;
/// The group and stop bytes of one bundle.
pub(super) const MAX_BUNDLE_PAYLOAD_BYTES: usize = MAX_BUNDLE_MEMBERS * MAX_POSITION_MEMBER_BYTES;
/// Bytes of one stopped slot in the index block (a big-endian `u16`).
const STOP_BYTES: usize = 2;
/// Most stops in one bundle: an eighth of the bundle bytes.
const MAX_STOPS_PER_BUNDLE: usize = MAX_BUNDLE_PAYLOAD_BYTES / STOP_BYTES / 8;
const _: () = assert!(MAX_STOPS_PER_BUNDLE as u64 <= MAX_BUNDLE_INDEXES);
/// An entity in view sends its card again once per this many bundles of the player (about one
/// second at one bundle per tick): a bundle is unreliable, and a lost card would hide the entity
/// for as long as it stays in view.
pub(super) const CARD_REFRESH_BUNDLES: u64 = 30;
/// The component of a player's `ALCReplicatedState`: 16 on every own-player member of the
/// captures.
pub(super) const PLAYER_ALC_COMPONENT: u64 = 16;
/// The component of a mob's `ALCReplicatedState`: the most common mob value in the captures
/// (4 and 5 both occur; which mob prefab uses which is Unknown).
pub(super) const MOB_ALC_COMPONENT: u64 = 5;
/// The component of a creature's `MB::LookTargetingComponentReplicatedState` (retail: 4 on every
/// tutorial creature).
const LOOK_TARGETING_COMPONENT: u64 = 4;

/// What a client needs to show one mob: its entity number, where it is and its vitals.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MobCard {
    /// The entity number.
    pub(super) entity: u64,
    /// Where it is.
    pub(super) position: Position,
    /// Its vitals and ids.
    pub(super) state: MobGhost,
    /// The only player who sees it, for a personal mob.
    pub(super) owner: Option<u64>,
    /// Its states of this tick, for a mob whose prefab has a first `ALCReplicatedState`.
    pub(super) alc: Option<MobFrame>,
    /// Its health changed since the last bundle step (or it is new).
    pub(super) health_changed: bool,
    /// That change is its first damage: the health it showed before was its maximum.
    pub(super) first_damage: bool,
    /// Its boss phase, for a boss mob that left phase 0.
    pub(super) boss_phase: Option<u32>,
    /// Its boss phase changed since the last bundle step.
    pub(super) boss_phase_changed: bool,
    /// The status effects others see on it (`m_remoteEffectsMap`): the map's version and
    /// entries, once it held one.
    pub(super) remote_effects: Option<(u64, Vec<(u16, RemoteEffect)>)>,
    /// Those effects changed since the last bundle step.
    pub(super) remote_effects_changed: bool,
    /// The slice asset of its look, when it has its own (a personal mob with a retail card).
    pub(super) asset: Option<[u8; ASSET_ID_LEN]>,
    /// The server time of this bundle step (ms since 2000-01-01).
    pub(super) server_ms: u64,
    /// The facet entity it looks at: the last player it picked as its target.
    pub(super) look_target: Option<u64>,
    /// It picked a new target since the last bundle step.
    pub(super) look_target_changed: bool,
    /// Its corpse was skinned.
    pub(super) skinned: bool,
    /// That happened since the last bundle step.
    pub(super) skinned_changed: bool,
}

impl MobCard {
    /// Whether player `player` sees this mob.
    pub(super) fn seen_by(&self, player: u64) -> bool {
        self.owner.is_none_or(|owner| owner == player)
    }
}

/// The card of a mob.
pub(super) const fn card_of(
    entity: u64,
    position: Position,
    state: MobGhost,
    owner: Option<u64>,
) -> MobCard {
    MobCard {
        entity,
        position,
        state,
        owner,
        alc: None,
        health_changed: true,
        first_damage: false,
        boss_phase: None,
        boss_phase_changed: false,
        remote_effects: None,
        remote_effects_changed: false,
        skinned: false,
        skinned_changed: false,
        asset: None,
        server_ms: 0,
        look_target: None,
        look_target_changed: false,
    }
}

/// The prefab and the `ALCReplicatedState` component of one kind of entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EntityKind {
    asset: AssetId,
    alc_component: u64,
    /// Whether the card comes with a state for every other replicated component of the prefab
    /// ([`player_members`]).
    empty_components: bool,
}

impl EntityKind {
    /// A player.
    pub(super) const PLAYER: Self = Self {
        asset: PLAYER_SLICE_ASSET,
        alc_component: PLAYER_ALC_COMPONENT,
        empty_components: true,
    };

    /// A mob of a prefab of the prefab table: its creature slice and ALC component.
    pub(super) const fn of_look(look: &MobLook) -> Self {
        Self {
            asset: look.asset,
            alc_component: look.alc_component,
            empty_components: false,
        }
    }

    /// A mob without a prefab in the table: its card carries a unique asset id derived from
    /// `entity` (local-player-check note), which the client cannot find, so it shows nothing; an
    /// all-zero id is not used, so the client's offline cache does not share one key across mobs.
    pub(super) fn mob(entity: u64) -> Self {
        Self {
            asset: AssetId {
                id: raw_id_of_entity(entity),
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            empty_components: false,
        }
    }
}

/// The group of `entity` of `kind` in slot `interest`: its card (with the other component states
/// of a player) when `carded`, then its position. A player's states are `states` when known (the
/// client's own player: its login character, look and vitals), else a connected 3935 state with
/// the entity raw id as `characterId` and empty states.
/// `None` for a position that is not finite.
pub(super) fn entity_group(
    interest: u16,
    entity: u64,
    kind: EntityKind,
    position: Position,
    carded: bool,
    states: Option<PlayerStates<'_>>,
) -> Option<BundleGroup> {
    let pos = InputPosition::from_metres(position.x, position.y, position.z)?;
    let mut members = Vec::new();
    if carded {
        members.push(BundleMember {
            component: GDE_METADATA_COMPONENT,
            state: MemberState::GdeMetadata(GdeMetadataReplicatedState::for_entity(
                kind.asset, entity,
            )),
        });
        if kind.empty_components {
            members.extend(player_members(states.unwrap_or(PlayerStates {
                character_id: raw_id_of_entity(entity),
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
                mana: None,
            })));
        }
    }
    members.push(BundleMember {
        component: kind.alc_component,
        state: MemberState::Alc(AlcState::at_position(pos)),
    });
    Some(BundleGroup {
        interest: u64::from(interest),
        members,
    })
}

/// Makes the position of `group` (its last member) the full first state at `position`
/// ([`AlcState::spawn`]): a client places a new entity only from such a state; from a lone
/// `pos10` it stays at the grid origin (own-player-position note).
///
/// # Errors
///
/// Returns [`MessageError`] when the state cannot be encoded.
fn full_first_position(
    group: &mut BundleGroup,
    position: Position,
    stamp: AlcStamp,
) -> Result<(), MessageError> {
    if let Some(pos) = InputPosition::from_metres(position.x, position.y, position.z)
        && let Some(alc) = group.members.last_mut()
    {
        alc.state = MemberState::Alc(AlcState::spawn(
            CardPlace {
                anchor: None,
                at: pos,
            },
            stamp,
        )?);
    }
    Ok(())
}

/// The group of another entity of `kind` in slot `interest`: as [`entity_group`], with the full
/// first position when `carded`.
///
/// # Errors
///
/// Returns [`MessageError`] when the first position cannot be encoded.
pub(super) fn remote_group(
    interest: u16,
    entity: u64,
    kind: EntityKind,
    position: Position,
    carded: bool,
) -> Result<Option<BundleGroup>, MessageError> {
    let Some(mut group) = entity_group(interest, entity, kind, position, carded, None) else {
        return Ok(None);
    };
    if carded {
        full_first_position(&mut group, position, NO_CLOCK_STAMP)?;
    }
    Ok(Some(group))
}

/// The group of the mob of `card` in slot `interest`. With its prefab `look`, a carded group
/// carries the prefab's first states and the whole vitals, a later group the health when it
/// changed, and the group of a mob that just died the death states (mob-cards note, sections 2
/// and 5).
/// A mob with a timeline ([`MobCard::alc`]) sends its first state with the card and its later
/// states when they are due; without one, the group carries the plain position. Without a
/// look, the group is the card and the position only. `None` for a position that is not finite
/// or a group with nothing to send.
///
/// # Errors
///
/// Returns [`MessageError`] when a state cannot be encoded.
pub(super) fn mob_group(
    interest: u16,
    card: &MobCard,
    look: Option<&MobLook>,
    carded: bool,
) -> Result<Option<BundleGroup>, MessageError> {
    let Some(look) = look else {
        return remote_group(
            interest,
            card.entity,
            EntityKind::mob(card.entity),
            card.position,
            carded,
        );
    };
    let Some(mut group) = remote_group(
        interest,
        card.entity,
        EntityKind::of_look(look),
        card.position,
        carded,
    )?
    else {
        return Ok(None);
    };
    // The plain position is the last member.
    let Some(plain) = group.members.pop() else {
        return Ok(None);
    };
    let position = match &card.alc {
        None => Some(plain),
        Some(frame) => {
            let alc = if carded {
                let pos =
                    InputPosition::from_metres(card.position.x, card.position.y, card.position.z);
                match (&look.alc, pos) {
                    // A fresh spawn's card carries no state: its first state follows.
                    (Some(first), Some(at)) if frame.fresh => first.mob_spawn_card(at),
                    (Some(first), Some(at)) => first.mob_first(
                        frame.stamp,
                        CardPlace {
                            anchor: frame.anchor,
                            at,
                        },
                        frame.facing,
                        frame.animation.as_ref(),
                    )?,
                    _ => None,
                }
            } else {
                frame.step.as_ref().map(AlcState::mob_step).transpose()?
            };
            match alc {
                Some(alc) => Some(BundleMember {
                    component: plain.component,
                    state: MemberState::Alc(alc),
                }),
                None => carded.then_some(plain),
            }
        }
    };
    let mob = card.state;
    let dead = mob.health == 0;
    let vitals = if carded {
        group.members.extend(look.first.iter().cloned());
        let mut vitals = match &look.vitals {
            Some(recorded) => recorded.creature_as_recorded(
                mob.health,
                mob.max_health,
                mob.vitals_id,
                mob.category_id,
                mob.level,
            ),
            None => VitalsState::creature(
                mob.health,
                mob.max_health,
                mob.vitals_id,
                mob.category_id,
                mob.level,
            ),
        };
        if let Some(slot) = vitals.slot_0.as_mut() {
            slot.vitals_data = Some(VitalsData::creature_first(mob.health, card.server_ms));
        }
        Some(vitals)
    } else {
        // Retail sends the health once after each change (a vitals delta after each hit), with
        // its change flags at the first damage.
        card.health_changed.then(|| {
            if card.first_damage {
                VitalsState::creature_first_damage(mob.health)
            } else {
                VitalsState::creature_health(mob.health)
            }
        })
    };
    if let Some(vitals) = vitals {
        group.members.push(BundleMember {
            component: look.vitals_component,
            state: MemberState::Vitals(vitals),
        });
    }
    // A boss phase goes with the card (after the prefab's phase 0) and after each change, on
    // the component of the prefab's boss phase state.
    let boss_component = look
        .first
        .iter()
        .find(|member| member.state.type_index() == BOSS_PHASE_TYPE_INDEX)
        .map(|member| member.component);
    if let (Some(phase), Some(component)) = (card.boss_phase, boss_component)
        && (carded || card.boss_phase_changed)
    {
        group.members.push(BundleMember {
            component,
            state: MemberState::Recorded {
                class: BOSS_PHASE_TYPE_INDEX,
                body: boss_phase_change(phase),
            },
        });
    }
    // The status effects others see go with the card (after the prefab's empty map) and after
    // each change, on the component of the prefab's status effects state (status effects note:
    // retail mob component 8).
    let effects_component = look
        .first
        .iter()
        .find(|member| member.state.type_index() == STATUS_EFFECTS_TYPE_INDEX)
        .map(|member| member.component);
    if let (Some((version, entries)), Some(component)) =
        (card.remote_effects.as_ref(), effects_component)
        && (carded || card.remote_effects_changed)
    {
        group.members.push(BundleMember {
            component,
            state: MemberState::StatusEffects(StatusEffectsState {
                remote: Some(EffectMap::full(*version, entries.clone())),
                ..StatusEffectsState::default()
            }),
        });
    }
    // A skinned corpse's gatherable controller is depleted, on the prefab's controller component
    // (gather note: retail boar c6:12 `01 01 08` at the skinning's end).
    let controller = look
        .first
        .iter()
        .find(|member| member.state.type_index() == GATHERABLE_CONTROLLER_TYPE_INDEX)
        .map(|member| member.component);
    if let Some(component) = controller
        && card.skinned
        && (carded || card.skinned_changed)
    {
        group.members.push(BundleMember {
            component,
            state: MemberState::GatherableController(GatherableControllerState {
                gatherable_state: Some(DEPLETED_GATHERABLE),
            }),
        });
    }
    // The look target goes with the card and after each new target, as on retail (right after
    // each first 2415; never cleared at a 3916).
    if let Some(target) = card.look_target
        && (carded || card.look_target_changed)
    {
        group.members.push(BundleMember {
            component: LOOK_TARGETING_COMPONENT,
            state: MemberState::LookTargeting(LookTargetingState {
                target: Some(target),
            }),
        });
    }
    if dead && (carded || card.health_changed) {
        group.members.extend(look.death.iter().cloned());
    }
    // A fresh spawn draws its weapon with its first states (retail sailor 1: its paperdoll
    // change in the three groups after its card).
    if !carded && card.alc.is_some_and(|frame| frame.drawing) {
        group.members.extend(look.drawn.iter().cloned());
    }
    // The position stays the last member.
    group.members.extend(position);
    Ok((!group.members.is_empty()).then_some(group))
}

/// Logs the members of mob `mob`'s group at debug level, as hex, so a live run can be compared
/// byte for byte with the retail captures.
pub(super) fn log_mob_group(mob: u64, carded: bool, group: &BundleGroup) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let members: Vec<String> = group
        .members
        .iter()
        .map(|member| {
            member.encode().map_or_else(
                |error| format!("<{error}>"),
                |bytes| bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
            )
        })
        .collect();
    tracing::debug!(
        mob,
        interest = group.interest,
        carded,
        members = members.join(" "),
        "mob group"
    );
}

/// Milliseconds per own state number of a client that does not number its input records (the
/// test client, the load-test bots): one per 33 ms, retail's pace (live capture 117010 to 117112
/// ms).
const OWN_STATE_MS: u64 = 33;

/// A move of the client's own player by the server (retail moves the player into the water so):
/// the number and time of the state and the new teleport number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct OwnTeleport {
    stamp: AlcStamp,
    number: u8,
    facing: Option<f32>,
    step: TeleportStep,
}

/// The own clock re-anchors this often, before its 16-bit ms offset wraps (clock note: retail
/// sent a new anchor every 60017 to 60050 ms).
const OWN_ANCHOR_PERIOD_MS: u64 = 60_000;
/// The own states this soon after an anchor carry it again (retail: the next 2 to 5 own states,
/// about 16 ms apart).
const OWN_ANCHOR_REPEAT_MS: u64 = 100;
/// Microseconds per millisecond.
const US_PER_MS: u64 = 1_000;
/// The number and time of the full first state of an entity that is not the client's own player:
/// the first own state's, with no anchor.
const NO_CLOCK_STAMP: AlcStamp = AlcStamp {
    id: SPAWN_STATE_ID,
    anchor_us: 0,
    offset_ms: SPAWN_STATE_MS,
};

/// How far the own state numbers run ahead of the client's input counter, records (our choice:
/// the middle of the 110 to 148 records our clock-numbered states ran ahead in run 12). Retail
/// numbers each own state with the client's newest record, and its client corrects its own
/// player to an own place so numbered and runs its newer records again from there: every gap
/// between the server's movement and the client's own shows as a jump. Numbered with the
/// counter (`f7c2ad6`), ours rubber banded (live 2026-09-24 and 26); numbered ahead, before
/// `f7c2ad6` (live 2026-09-18) and with this lead (live 2026-09-26), the client moved freely. The
/// slayer starts take the same numbers.
pub(super) const OWN_STATE_LEAD: u16 = 128;

/// The own state number of the client's input record `counter` ([`OWN_STATE_LEAD`]).
pub(super) const fn own_state_counter(counter: u16) -> u16 {
    counter.wrapping_add(OWN_STATE_LEAD)
}

/// The number of a player's ALC states while its client numbers its input records: the counter
/// of the newest record the server moved the body by, and the server time of the first state
/// with it. Retail numbers every own state so (Observed: all 39,955 own state numbers of the
/// newchar capture were the counter of a record the client had sent, 36 ms before at the median;
/// a state numbered with a record repeats unchanged until a newer one came), and the slayer
/// states start in the state of the record that started them ([`super::slayer`]). Ours counted
/// the time instead and ran 110 to 148 records ahead of the client in the run-12 capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StateNumber {
    /// The number.
    pub(super) number: u16,
    /// The server time of the first state with it (ms since 2000-01-01).
    pub(super) since_ms: u64,
}

impl StateNumber {
    /// The number after the client's newest record `counter` at server time `server_ms`: the same
    /// state again until the counter moves on.
    pub(super) const fn after(this: Option<Self>, counter: u16, server_ms: u64) -> Self {
        match this {
            Some(number) if number.number == counter => number,
            _ => Self {
                number: counter,
                since_ms: server_ms,
            },
        }
    }
}

/// The clock of the client's own player: the client's 'now' is its anchor plus its offset, and
/// the cooldowns count down on it (clock note: with anchor 0 the potion's cooldown showed the
/// date). The anchor is the server time in µs since 2000-01-01, as the cooldown ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OwnClock {
    /// Milliseconds since the first own state.
    pub(super) since_spawn_ms: u64,
    /// The server time of the first own state (ms since 2000-01-01).
    pub(super) spawn_ms: u64,
    /// The state number from the client's input, when it numbers its records.
    pub(super) number: Option<u16>,
}

impl OwnClock {
    /// The clock of a player whose first state went at `spawn_ms`, at server time `server_ms`:
    /// with `number`, the state is that number's, at its time.
    pub(super) const fn at(spawn_ms: u64, server_ms: u64, number: Option<StateNumber>) -> Self {
        let (state_ms, number) = match number {
            Some(number) => (number.since_ms, Some(number.number)),
            None => (server_ms, None),
        };
        Self {
            since_spawn_ms: state_ms.saturating_sub(spawn_ms),
            spawn_ms,
            number,
        }
    }

    /// Milliseconds since the first anchor: the first own state's offset was
    /// [`SPAWN_STATE_MS`].
    const fn elapsed_ms(self) -> u64 {
        self.since_spawn_ms + SPAWN_STATE_MS as u64
    }

    /// The first anchor: the first own state's server time less its offset.
    const fn first_anchor_us(self) -> u64 {
        self.spawn_ms.saturating_sub(SPAWN_STATE_MS as u64) * US_PER_MS
    }

    /// The number and time of the own state now: the client's number, else the number goes on
    /// from the first state's; the anchor moves on each [`OWN_ANCHOR_PERIOD_MS`].
    pub(super) const fn stamp(self) -> AlcStamp {
        let elapsed = self.elapsed_ms();
        let periods = elapsed / OWN_ANCHOR_PERIOD_MS;
        AlcStamp {
            id: match self.number {
                Some(number) => number,
                None => SPAWN_STATE_ID.wrapping_add((self.since_spawn_ms / OWN_STATE_MS) as u16),
            },
            anchor_us: self.first_anchor_us() + periods * OWN_ANCHOR_PERIOD_MS * US_PER_MS,
            // Below the period, which fits 16 bits.
            offset_ms: (elapsed % OWN_ANCHOR_PERIOD_MS) as u16,
        }
    }

    /// Whether the own state now carries its anchor: soon after one starts.
    const fn anchored(self) -> bool {
        self.elapsed_ms() % OWN_ANCHOR_PERIOD_MS < OWN_ANCHOR_REPEAT_MS
    }
}

/// The places of the client's own player that the client has: the own `pos10` and the last place
/// it got. Retail own states (movement study 2026-09-20, clean walking and running captures):
/// every state of a moving own player carries its place, an offset from the own `pos10` while it
/// is within reach and a new `pos10` when it is not; the state after a new `pos10` carries it again
/// with the offset from it; the states of a player that does not move carry no place.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct OwnPlaces {
    /// The own `pos10` the client has.
    anchor: Option<InputPosition>,
    /// The offset from `anchor` of the last place the client got.
    sent: Option<[u8; POS3_LEN]>,
    /// The last place was a new `pos10`: the next one carries it again.
    anchor_again: bool,
}

impl OwnPlaces {
    /// The place `at` for the next own state; `None` when the client has that place already.
    pub(super) fn next(&mut self, at: InputPosition) -> Option<StepPlace> {
        match StepPlace::of(self.anchor, at) {
            StepPlace::Offset(offset) if self.sent == Some(offset) => None,
            StepPlace::Offset(offset) => {
                self.sent = Some(offset);
                Some(match self.anchor.filter(|_| self.anchor_again) {
                    Some(anchor) => {
                        self.anchor_again = false;
                        StepPlace::AnchorAgain { anchor, offset }
                    }
                    None => StepPlace::Offset(offset),
                })
            }
            place => {
                if let StepPlace::Anchor(new) = place {
                    self.placed(new);
                    self.anchor_again = true;
                }
                Some(place)
            }
        }
    }

    /// The client took `pos10` as its place (a new anchor, a spawn, a server move).
    pub(super) fn placed(&mut self, pos10: InputPosition) {
        self.anchor = Some(pos10);
        self.sent = offset_from(pos10, pos10);
        self.anchor_again = false;
    }
}

/// One state of the client's own player that neither spawns nor moves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct OwnTick {
    /// The own clock.
    pub(super) clock: OwnClock,
    /// Send the place when the client does not have it yet (not while the server refuses the
    /// player's moves: that place is behind the client).
    pub(super) with_position: bool,
    /// Send the movement flags without the throttle bit (the first state after the spawn).
    pub(super) clear_throttle: bool,
    /// The slayer state and sequence time that go with this state ([`super::slayer`]).
    pub(super) slayer: SlayerDue,
    /// The facing and look direction of the client's newest input, when they changed since the
    /// last own state (retail sends them back in the own state; lock-on note).
    pub(super) view: Option<LookDir>,
    /// The `distGround` and `waterDepth` bytes of the ground and water under the player, each
    /// when it changed since its client got it ([`nw_sim_core::Footing::change_since`]).
    pub(super) footing: (Option<u8>, Option<u8>),
}

impl OwnTeleport {
    /// The `count`-th server move on the own clock `clock`, with the teleport number on from the
    /// first state's 1.
    pub(super) fn of(count: u8, clock: OwnClock, facing: Option<f32>, step: TeleportStep) -> Self {
        Self {
            stamp: clock.stamp(),
            number: SPAWN_TELEPORT.wrapping_add(count),
            facing,
            step,
        }
    }
}

/// Which card goes with the client's own player in one bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnCard {
    /// No card: only the position.
    Without,
    /// The first card, with the full first state that sets the client's start position
    /// ([`AlcState::spawn`]).
    Spawn,
}

/// The group of the client's own player with its `states`; `None` for a position that is not
/// finite. When the server `moved` the player, the position is the full first state, which the
/// client takes as its new place (retail: the move into the water, live-tutorial note section 4).
/// Any other own state is `tick`: the own clock, and the place when the tick allows it and the
/// client does not have it yet ([`OwnPlaces`]); every state that places the client keeps its
/// `pos10` in `places`.
///
/// # Errors
///
/// Returns [`MessageError`] when the spawn state cannot be encoded.
pub(super) fn own_group(
    entity: u64,
    states: PlayerStates<'_>,
    position: Position,
    card: OwnCard,
    teleport: Option<OwnTeleport>,
    tick: OwnTick,
    places: &mut OwnPlaces,
) -> Result<Option<BundleGroup>, MessageError> {
    let Some(mut group) = entity_group(
        LOCAL_PLAYER_INTEREST,
        entity,
        EntityKind::PLAYER,
        position,
        card != OwnCard::Without,
        Some(states),
    ) else {
        return Ok(None);
    };
    if card == OwnCard::Spawn {
        full_first_position(&mut group, position, tick.clock.stamp())?;
    } else if let (Some(teleport), Some(pos), Some(alc)) = (
        teleport,
        InputPosition::from_metres(position.x, position.y, position.z),
        group.members.last_mut(),
    ) {
        alc.state = MemberState::Alc(AlcState::teleport(
            teleport.stamp,
            pos,
            teleport.facing,
            teleport.number,
            teleport.step,
            tick.slayer.time,
        ));
    } else if let Some(alc) = group.members.last_mut() {
        let place = InputPosition::from_metres(position.x, position.y, position.z)
            .filter(|_| tick.with_position)
            .and_then(|at| places.next(at));
        let (ground_distance, water_depth) = tick.footing;
        alc.state = MemberState::Alc(
            AlcState::own_step(
                tick.clock.stamp(),
                place,
                tick.clear_throttle.then_some(DATA_BITS_ACTIVE),
                tick.clock.anchored(),
                tick.slayer.state,
                tick.view,
                tick.slayer.time,
            )?
            .with_upper_slots(tick.slayer.slots.unwrap_or_default())?
            .with_footing(ground_distance, water_depth),
        );
    }
    if let Some(MemberState::Alc(alc)) = group.members.last().map(|alc| &alc.state)
        && let Some(pos10) = alc.pos10
        && (card == OwnCard::Spawn || teleport.is_some())
    {
        places.placed(pos10);
    }
    // Without the card, the changes (achievements, objectives, experience, gear, health) still
    // go out, before the position.
    if card == OwnCard::Without {
        let changes = change_members(states);
        let before_position = group.members.len().saturating_sub(1);
        group
            .members
            .splice(before_position..before_position, changes);
    }
    Ok(Some(group))
}

/// The bundle envelopes of `groups`, in order, split by bytes, with the pending stops of `slots`
/// in their index blocks (in extra bundles when the groups end first); the first has sequence
/// `sequence` and each next one the next; each carries `client_context`.
pub(super) fn bundles(
    sequence: u64,
    client_context: u8,
    slots: &mut Slots,
    groups: Vec<BundleGroup>,
) -> Result<Vec<Vec<u8>>, MessageError> {
    let mut envelopes = Vec::new();
    let mut groups = groups.into_iter().peekable();
    while groups.peek().is_some() || !slots.pending_stops().is_empty() {
        let stops = slots.take_stops(MAX_STOPS_PER_BUNDLE);
        let mut used = stops.len() * STOP_BYTES;
        let mut part = Vec::new();
        while let Some(group) = groups.peek() {
            let bytes = group_bytes(group)?;
            if !part.is_empty() && used + bytes > MAX_BUNDLE_PAYLOAD_BYTES {
                break;
            }
            used += bytes;
            part.extend(groups.next());
        }
        let next = sequence + envelopes.len() as u64;
        envelopes.push(
            ServerMessage::Bundle(ReplicatedStateBundle::of_groups(
                next,
                client_context,
                stops,
                part,
            )?)
            .envelope()?,
        );
    }
    Ok(envelopes)
}

fn group_bytes(group: &BundleGroup) -> Result<usize, MessageError> {
    Ok(BundlePayload {
        groups: vec![group.clone()],
    }
    .encode()?
    .len())
}

#[cfg(test)]
mod tests {
    use nw_messages::{
        EntitySlots, GDE_ID_LEN, LOGIN_CLIENT_CONTEXT, PLAYER_COMPONENT_TYPE_INDEX, POS_REL_CHILD,
        PlayerLook, SlotRead,
    };
    use nw_reflect::Envelope;
    use uuid::Uuid;

    use super::super::slots::FIRST_REMOTE_INTEREST;
    use super::*;

    const NOW: u64 = 66_000;

    const ENTITY: u64 = (1 << 34) + 7;
    const OWN: u64 = 9;
    const CHARACTER: Uuid = Uuid::from_u128(0x2b87_684d_c82c_46a4_90e4_c764_0247_7507);
    const HEALTH: (u32, u32) = (700, 856);

    fn own_states() -> PlayerStates<'static> {
        PlayerStates {
            character_id: *CHARACTER.as_bytes(),
            look: Some(PlayerLook::default()),
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
            mana: None,
        }
    }
    const SEQUENCE: u64 = 4;
    const MANY: u64 = 60;
    const ALL_STOPS: usize = 1000;

    fn at() -> Position {
        Position {
            x: 10.0,
            y: 20.0,
            z: 30.0,
        }
    }

    fn reads(envelopes: &[Vec<u8>]) -> Vec<SlotRead> {
        envelopes
            .iter()
            .map(|bytes| {
                let envelope = Envelope::decode(bytes).expect("envelope");
                let Some(Ok(ServerMessage::Bundle(bundle))) =
                    ServerMessage::from_envelope(&envelope)
                else {
                    panic!("a bundle");
                };
                SlotRead::of(&bundle).expect("read")
            })
            .collect()
    }

    #[test]
    fn the_own_player_is_the_player_prefab_in_slot_1() {
        let mut places = OwnPlaces::default();
        let group = own_group(
            OWN,
            own_states(),
            at(),
            OwnCard::Spawn,
            None,
            STILL,
            &mut places,
        )
        .expect("encode")
        .expect("finite");
        assert_eq!(group.interest, u64::from(LOCAL_PLAYER_INTEREST));
        let MemberState::GdeMetadata(card) = &group.members[0].state else {
            panic!("the card comes first: {group:?}");
        };
        assert_eq!(group.members[0].component, GDE_METADATA_COMPONENT);
        assert_eq!(card.asset, Some(PLAYER_SLICE_ASSET));
        assert_eq!(card.entity_number(), Some(OWN));
        let states: Vec<BundleMember> = player_members(own_states()).collect();
        assert_eq!(
            group.members[1..=states.len()],
            states[..],
            "every other player component gets a state with the card: the own states"
        );
        let player = states
            .iter()
            .find(|member| member.state.type_index() == PLAYER_COMPONENT_TYPE_INDEX)
            .expect("player component");
        let MemberState::PlayerComponent(state) = &player.state else {
            panic!("3935 is the player component: {player:?}");
        };
        assert_eq!(
            state.character_id,
            Some(*CHARACTER.as_bytes()),
            "the own characterId is the login character, so the client sees the local player"
        );
        assert_eq!(state.player_connected, Some(true));
        let last = group.members.last().expect("position");
        assert_eq!(last.component, PLAYER_ALC_COMPONENT);
        let MemberState::Alc(alc) = &last.state else {
            panic!("then the ALC position: {group:?}");
        };
        assert_eq!(alc.pos10, InputPosition::from_metres(10.0, 20.0, 30.0));
        assert_eq!(
            places.anchor, alc.pos10,
            "the spawn's pos10 is the own anchor"
        );
        let later = own_group(
            OWN,
            own_states(),
            at(),
            OwnCard::Without,
            None,
            STILL,
            &mut places,
        )
        .expect("encode")
        .expect("finite");
        assert_eq!(later.members.len(), 1, "later only the ALC state");
        let MemberState::Alc(still) = &later.members[0].state else {
            panic!("the ALC state: {later:?}");
        };
        assert_eq!(still.pos10, None);
        assert!(
            !still
                .slot0_children()
                .expect("walk")
                .iter()
                .any(|(child, _)| *child == POS_REL_CHILD),
            "the client has the place already: none goes"
        );
        assert_eq!(places.anchor, alc.pos10);
        let payload = BundlePayload {
            groups: vec![group.clone()],
        };
        let read = BundlePayload::decode(&payload.encode().expect("payload")).expect("read");
        assert_eq!(read.groups[0].members, group.members);
    }

    #[test]
    fn a_moving_own_player_gets_its_place_in_every_state_as_retail() {
        // Retail clean walking capture: a new place in every state of the walk, offsets from the
        // own pos10 while within its 1.99 m reach; a new pos10 at zero offset when out of it
        // (state 61), the same pos10 again with the next offset (state 62), then offsets.
        let start = InputPosition::from_metres(4232.0, 9452.5, 342.6).expect("finite");
        let east = |metres: f32| {
            InputPosition::from_metres(start.x + metres, start.y, start.z()).expect("finite")
        };
        let mut places = OwnPlaces::default();
        places.placed(start);
        assert_eq!(
            places.next(start),
            None,
            "standing: the client has its place"
        );
        /// A run's step per state: 5.5 m/s over 33 ms.
        const STEP_M: f32 = 0.183;
        let mut kinds = Vec::new();
        for state in 1..=14_u8 {
            let place = places.next(east(f32::from(state) * STEP_M)).expect("moved");
            kinds.push(match place {
                StepPlace::Offset(_) => 'o',
                StepPlace::Anchor(_) => 'A',
                StepPlace::AnchorAgain { anchor, .. } => {
                    assert_eq!(Some(anchor), places.anchor, "the same pos10 again");
                    'a'
                }
            });
        }
        // 1.99 m reach / 0.183 m: the 11th state is out of reach of the spawn's pos10.
        assert_eq!(kinds.iter().collect::<String>(), "ooooooooooAaoo");
        let last = east(14.0 * STEP_M);
        assert_eq!(places.next(last), None, "stopped: no place");
    }

    /// The server time of the first own state (retail 845730262408 ms since 2000-01-01).
    const SPAWN_MS: u64 = 845_730_262_408;
    /// The own clock at the first own state.
    const SPAWN_CLOCK: OwnClock = OwnClock {
        since_spawn_ms: 0,
        spawn_ms: SPAWN_MS,
        number: None,
    };

    /// A tick whose place may go, and one of a player whose moves are refused.
    const STILL: OwnTick = OwnTick {
        clock: SPAWN_CLOCK,
        with_position: true,
        clear_throttle: false,
        slayer: SlayerDue {
            state: None,
            time: None,
            slots: None,
        },
        view: None,
        footing: (None, None),
    };
    const REFUSED: OwnTick = OwnTick {
        with_position: false,
        ..STILL
    };

    #[test]
    fn the_own_clock_runs_on_the_server_time_and_anchors_each_minute() {
        // The client's 'now' is the anchor plus the offset: the server time.
        let now_us = |clock: OwnClock| {
            let stamp = clock.stamp();
            stamp.anchor_us + u64::from(stamp.offset_ms) * US_PER_MS
        };
        assert_eq!(now_us(SPAWN_CLOCK), SPAWN_MS * US_PER_MS);
        assert!(SPAWN_CLOCK.anchored(), "the first states carry the anchor");
        let later = OwnClock {
            since_spawn_ms: 30_000,
            ..SPAWN_CLOCK
        };
        assert_eq!(now_us(later), (SPAWN_MS + 30_000) * US_PER_MS);
        assert!(!later.anchored());
        let next_minute = OwnClock {
            since_spawn_ms: OWN_ANCHOR_PERIOD_MS - u64::from(SPAWN_STATE_MS) + 16,
            ..SPAWN_CLOCK
        };
        assert_eq!(next_minute.stamp().offset_ms, 16);
        assert!(next_minute.anchored(), "a new anchor goes again");
        assert_eq!(
            now_us(next_minute),
            (SPAWN_MS + next_minute.since_spawn_ms) * US_PER_MS
        );
    }

    #[test]
    fn a_server_move_sends_the_next_teleport_number_on_the_own_clock() {
        const SINCE_SPAWN_MS: u64 = 117_000;
        let pos = InputPosition::from_metres(10.0, 20.0, 30.0).expect("finite");
        const FACING: f32 = 1.434;
        let clock = OwnClock {
            since_spawn_ms: SINCE_SPAWN_MS,
            spawn_ms: SPAWN_MS,
            number: None,
        };
        let teleport = OwnTeleport::of(1, clock, Some(FACING), TeleportStep::Confirm);
        assert_eq!(teleport.number, SPAWN_TELEPORT + 1, "retail: 1, then 2");
        assert_eq!(
            teleport.stamp.id,
            SPAWN_STATE_ID + (SINCE_SPAWN_MS / OWN_STATE_MS) as u16
        );
        assert_eq!(
            teleport.stamp.offset_ms, 57_009,
            "the time counts from the anchor of the second minute"
        );
        let group = own_group(
            OWN,
            own_states(),
            at(),
            OwnCard::Without,
            Some(teleport),
            REFUSED,
            &mut OwnPlaces::default(),
        )
        .expect("encode")
        .expect("finite");
        assert_eq!(
            group.members.last().map(|member| &member.state),
            Some(&MemberState::Alc(AlcState::teleport(
                teleport.stamp,
                pos,
                Some(FACING),
                teleport.number,
                TeleportStep::Confirm,
                None,
            )))
        );
    }

    #[test]
    fn a_refused_player_gets_the_clock_without_a_position_but_its_spawn_does() {
        let alc_of = |card, tick| {
            let group = own_group(
                OWN,
                own_states(),
                at(),
                card,
                None,
                tick,
                &mut OwnPlaces::default(),
            )
            .expect("encode")
            .expect("finite");
            let last = group.members.last().expect("alc");
            assert_eq!(last.component, PLAYER_ALC_COMPONENT);
            last.state.clone()
        };
        let MemberState::Alc(refused) = alc_of(OwnCard::Without, REFUSED) else {
            panic!("an ALC");
        };
        assert_eq!(
            refused,
            AlcState::own_step(SPAWN_CLOCK.stamp(), None, None, true, None, None, None)
                .expect("step")
        );
        let MemberState::Alc(spawn) = alc_of(OwnCard::Spawn, REFUSED) else {
            panic!("an ALC");
        };
        assert!(
            spawn.pos10.is_some(),
            "the spawn card sets the start position"
        );
        let first = OwnTick {
            clear_throttle: true,
            ..REFUSED
        };
        assert_eq!(
            alc_of(OwnCard::Without, first),
            MemberState::Alc(
                AlcState::own_step(
                    SPAWN_CLOCK.stamp(),
                    None,
                    Some(DATA_BITS_ACTIVE),
                    true,
                    None,
                    None,
                    None
                )
                .expect("step")
            ),
            "the first tick clears the throttle"
        );
    }

    #[test]
    fn only_the_spawn_card_carries_the_full_first_state() {
        let pos = InputPosition::from_metres(10.0, 20.0, 30.0).expect("finite");
        let alc_of = |card: OwnCard| {
            let group = own_group(
                OWN,
                own_states(),
                at(),
                card,
                None,
                STILL,
                &mut OwnPlaces::default(),
            )
            .expect("encode")
            .expect("finite");
            group.members.last().expect("alc").state.clone()
        };
        assert_eq!(
            alc_of(OwnCard::Spawn),
            MemberState::Alc(
                AlcState::spawn(
                    CardPlace {
                        anchor: None,
                        at: pos,
                    },
                    SPAWN_CLOCK.stamp(),
                )
                .expect("spawn"),
            ),
            "the first card sets the start position with the full state"
        );
        let still = MemberState::Alc(
            AlcState::own_step(
                SPAWN_CLOCK.stamp(),
                Some(StepPlace::Anchor(pos)),
                None,
                true,
                None,
                None,
                None,
            )
            .expect("step"),
        );
        assert_eq!(alc_of(OwnCard::Without), still);
    }

    #[test]
    fn a_plain_own_state_carries_the_changed_footing_and_a_teleport_does_not() {
        // The fall into the tutorial sea: `distGround` 193 and `waterDepth` 2 (retail newchar
        // 2026-09-18, run-15 movement study).
        const SWIM_FOOTING: (Option<u8>, Option<u8>) = (Some(193), Some(2));
        /// The slot byte's bit of slot 1.
        const SLOT_1_BIT: u8 = 0b10;
        let swimming = OwnTick {
            footing: SWIM_FOOTING,
            ..REFUSED
        };
        let alc = |teleport: Option<OwnTeleport>| {
            let group = own_group(
                OWN,
                own_states(),
                at(),
                OwnCard::Without,
                teleport,
                swimming,
                &mut OwnPlaces::default(),
            )
            .expect("encode")
            .expect("finite");
            let MemberState::Alc(alc) = group.members.last().expect("alc").state.clone() else {
                panic!("an ALC");
            };
            alc
        };
        let plain = AlcState::own_step(SPAWN_CLOCK.stamp(), None, None, true, None, None, None)
            .expect("step");
        assert_eq!(
            alc(None),
            plain.with_footing(SWIM_FOOTING.0, SWIM_FOOTING.1)
        );
        let teleport = OwnTeleport::of(1, SPAWN_CLOCK, None, TeleportStep::Place);
        let moved = alc(Some(teleport));
        assert_eq!(
            moved.bytes.first().map(|mask| mask & SLOT_1_BIT),
            Some(0),
            "the server move's states carry no slot 1, as retail's"
        );
    }

    #[test]
    fn a_mob_group_carries_its_own_card() {
        let group = entity_group(
            FIRST_REMOTE_INTEREST,
            ENTITY,
            EntityKind::mob(ENTITY),
            at(),
            true,
            None,
        )
        .expect("finite");
        let MemberState::GdeMetadata(card) = &group.members[0].state else {
            panic!("the card comes first: {group:?}");
        };
        let asset = card.asset.expect("asset");
        assert_eq!(asset.id, raw_id_of_entity(ENTITY));
        assert_eq!(asset.sub_id, DYNAMIC_SLICE_SUB_ID);
        assert_ne!(asset.id, [0; GDE_ID_LEN]);
        assert_eq!(card.entity_number(), Some(ENTITY));
        assert_eq!(group.members[1].component, MOB_ALC_COMPONENT);
    }

    #[test]
    fn a_mob_card_has_the_retail_bytes_with_a_dynamic_id() {
        // Retail 2026-09-18, the first legionnaire's card (component 0, class 10): `01 03`, its
        // slice asset `c05fc957…fb64` with sub id 2, then a random 16-byte `GdeRef`.
        const LEGIONNAIRE: [u8; GDE_ID_LEN] = [
            0xc0, 0x5f, 0xc9, 0x57, 0x84, 0x87, 0x55, 0x59, 0xaf, 0xe8, 0x74, 0xa7, 0x8f, 0x95,
            0xfb, 0x64,
        ];
        const RETAIL_HEAD: [u8; 22] = [
            0x01, 0x03, 0xc0, 0x5f, 0xc9, 0x57, 0x84, 0x87, 0x55, 0x59, 0xaf, 0xe8, 0x74, 0xa7,
            0x8f, 0x95, 0xfb, 0x64, 0x00, 0x00, 0x00, 0x02,
        ];
        /// A mob number of this test only.
        const MOB: u64 = (1 << 34) + 0x7c01;
        let look = MobLook {
            asset: AssetId {
                id: LEGIONNAIRE,
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: 10,
            alc: None,
            vitals: None,
            first: Vec::new(),
            death: Vec::new(),
            drawn: Vec::new(),
        };
        let state = MobGhost {
            health: 370,
            max_health: 370,
            returning: false,
            vitals_id: 1,
            category_id: 2,
            level: 1,
        };
        let group = mob_group(
            FIRST_REMOTE_INTEREST,
            &card_of(MOB, at(), state, None),
            Some(&look),
            true,
        )
        .expect("encode")
        .expect("group");
        let card = group
            .members
            .iter()
            .find(|member| member.component == GDE_METADATA_COMPONENT)
            .and_then(|member| match &member.state {
                MemberState::GdeMetadata(card) => Some(*card),
                _ => None,
            })
            .expect("card");
        let mut writer = nw_messages::Writer::new();
        card.write(&mut writer);
        let body = writer.finish();
        assert_eq!(body[..RETAIL_HEAD.len()], RETAIL_HEAD);
        let mut gde_ref = [0; GDE_ID_LEN];
        gde_ref.copy_from_slice(&body[RETAIL_HEAD.len()..]);
        assert_eq!(gde_ref, raw_id_of_entity(MOB));
        assert!(
            nw_messages::is_dynamic_raw_id(&gde_ref),
            "bit 0 clear, no zero half"
        );
        assert_ne!(
            gde_ref[..GDE_ID_LEN / 2],
            gde_ref[GDE_ID_LEN / 2..],
            "independent halves"
        );
        assert_eq!(card.entity_number(), Some(MOB));
    }

    #[test]
    fn a_first_damage_sends_the_change_flags_and_later_ones_do_not() {
        const VITALS_COMPONENT: u64 = 9;
        let look = MobLook {
            asset: AssetId {
                id: [0x3c; GDE_ID_LEN],
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: VITALS_COMPONENT,
            alc: None,
            vitals: None,
            first: Vec::new(),
            death: Vec::new(),
            drawn: Vec::new(),
        };
        let state = MobGhost {
            health: 245,
            max_health: 353,
            returning: false,
            vitals_id: 1,
            category_id: 2,
            level: 1,
        };
        let flags_of = |first_damage| {
            let card = MobCard {
                first_damage,
                ..card_of(ENTITY, at(), state, None)
            };
            let group = mob_group(FIRST_REMOTE_INTEREST, &card, Some(&look), false)
                .expect("encode")
                .expect("group");
            group.members.iter().find_map(|member| match &member.state {
                MemberState::Vitals(vitals) => {
                    vitals.slot_0.as_ref().map(|slot| slot.health_change_flags)
                }
                _ => None,
            })
        };
        assert_eq!(flags_of(true), Some(Some(nw_messages::FIRST_DAMAGE_FLAGS)));
        assert_eq!(flags_of(false), Some(None));
    }

    #[test]
    fn a_mob_sends_the_effects_others_see_after_each_change_on_its_prefab_component() {
        // Retail mob component 8 (status effects note 2026-10-31).
        const EFFECTS_COMPONENT: u64 = 8;
        const HEX_KEY: u16 = 1_234;
        let look = MobLook {
            asset: AssetId {
                id: [0x3c; GDE_ID_LEN],
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: 9,
            alc: None,
            vitals: None,
            first: vec![BundleMember {
                component: EFFECTS_COMPONENT,
                state: MemberState::StatusEffects(StatusEffectsState::default()),
            }],
            death: Vec::new(),
            drawn: Vec::new(),
        };
        let state = MobGhost {
            health: 100,
            max_health: 100,
            returning: false,
            vitals_id: 1,
            category_id: 2,
            level: 1,
        };
        let entries = vec![(HEX_KEY, RemoteEffect::of(3, 1.0))];
        let card = |changed: bool| MobCard {
            health_changed: false,
            remote_effects: Some((2, entries.clone())),
            remote_effects_changed: changed,
            ..card_of(ENTITY, at(), state, None)
        };
        let sent = MemberState::StatusEffects(StatusEffectsState {
            remote: Some(EffectMap::full(2, entries.clone())),
            ..StatusEffectsState::default()
        });
        let carries = |card: &MobCard, carded: bool| {
            mob_group(FIRST_REMOTE_INTEREST, card, Some(&look), carded)
                .expect("encode")
                .expect("group")
                .members
                .iter()
                .any(|member| member.component == EFFECTS_COMPONENT && member.state == sent)
        };
        assert!(carries(&card(true), false), "the change goes out");
        assert!(!carries(&card(false), false), "no change, nothing");
        assert!(carries(&card(false), true), "a card carries them");
    }

    #[test]
    fn a_boss_sends_its_phase_after_each_change_on_its_prefab_component() {
        const BOSS_COMPONENT: u64 = 14;
        const PHASE: u32 = 1;
        let card_phase = BundleMember {
            component: BOSS_COMPONENT,
            state: MemberState::Recorded {
                class: BOSS_PHASE_TYPE_INDEX,
                body: vec![0x01, 0x03, 0, 0, 0, 0, 0x01],
            },
        };
        let look = MobLook {
            asset: AssetId {
                id: [0x3c; GDE_ID_LEN],
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: 9,
            alc: None,
            vitals: None,
            first: vec![card_phase.clone()],
            death: Vec::new(),
            drawn: Vec::new(),
        };
        let state = MobGhost {
            health: 100,
            max_health: 100,
            returning: false,
            vitals_id: 1,
            category_id: 2,
            level: 1,
        };
        let changed = MobCard {
            health_changed: false,
            boss_phase: Some(PHASE),
            boss_phase_changed: true,
            ..card_of(ENTITY, at(), state, None)
        };
        let phase = BundleMember {
            component: BOSS_COMPONENT,
            state: MemberState::Recorded {
                class: BOSS_PHASE_TYPE_INDEX,
                body: boss_phase_change(PHASE),
            },
        };
        let group = mob_group(FIRST_REMOTE_INTEREST, &changed, Some(&look), false)
            .expect("encode")
            .expect("group");
        assert!(group.members.contains(&phase), "the change goes out");
        let carded = mob_group(FIRST_REMOTE_INTEREST, &changed, Some(&look), true)
            .expect("encode")
            .expect("group");
        let at_card = carded
            .members
            .iter()
            .position(|member| *member == card_phase);
        let at_phase = carded.members.iter().position(|member| *member == phase);
        assert!(
            at_card < at_phase,
            "a card carries the phase after the prefab's"
        );
        let unchanged = MobCard {
            boss_phase_changed: false,
            ..changed
        };
        let later = mob_group(FIRST_REMOTE_INTEREST, &unchanged, Some(&look), false)
            .expect("encode")
            .expect("the position");
        assert!(!later.members.contains(&phase), "sent once");
    }

    #[test]
    fn a_new_target_goes_out_as_the_look_target() {
        const PLAYER_FACET: u64 = 0xbfa9_6f3e_a2e3_6a5a;
        const MOB_HEALTH: u32 = 353;
        let look = MobLook {
            asset: AssetId {
                id: [0xb7; GDE_ID_LEN],
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: 9,
            alc: None,
            vitals: None,
            first: Vec::new(),
            death: Vec::new(),
            drawn: Vec::new(),
        };
        let looking = BundleMember {
            component: LOOK_TARGETING_COMPONENT,
            state: MemberState::LookTargeting(LookTargetingState {
                target: Some(PLAYER_FACET),
            }),
        };
        let picked = MobCard {
            health_changed: false,
            look_target: Some(PLAYER_FACET),
            look_target_changed: true,
            ..card_of(
                ENTITY,
                at(),
                MobGhost {
                    health: MOB_HEALTH,
                    max_health: MOB_HEALTH,
                    returning: false,
                    vitals_id: 1,
                    category_id: 2,
                    level: 1,
                },
                None,
            )
        };
        let members = |card: &MobCard, carded| {
            mob_group(FIRST_REMOTE_INTEREST, card, Some(&look), carded)
                .expect("encode")
                .map(|group| group.members)
                .unwrap_or_default()
        };
        assert!(
            members(&picked, false).contains(&looking),
            "the change goes out"
        );
        let same = MobCard {
            look_target_changed: false,
            ..picked
        };
        assert!(!members(&same, false).contains(&looking), "sent once");
        assert!(
            members(&same, true).contains(&looking),
            "and with each card"
        );
    }

    #[test]
    fn a_mob_with_a_prefab_shows_its_slice_states_health_and_death() {
        const SAILOR_ASSET: [u8; GDE_ID_LEN] = [0xb7; GDE_ID_LEN];
        const VITALS_COMPONENT: u64 = 9;
        const HEALTH: u32 = 353;
        let gatherable = BundleMember {
            component: 6,
            state: MemberState::GatherableController(nw_messages::GatherableControllerState {
                gatherable_state: Some(4),
            }),
        };
        let death = BundleMember {
            component: 20,
            state: MemberState::DelayedEvent(nw_messages::DelayedEventState {
                total_delay: None,
                completed: Some(true),
            }),
        };
        let look = MobLook {
            asset: AssetId {
                id: SAILOR_ASSET,
                sub_id: DYNAMIC_SLICE_SUB_ID,
            },
            alc_component: MOB_ALC_COMPONENT,
            vitals_component: VITALS_COMPONENT,
            // A recorded first position somewhere else: the group moves it to the mob.
            alc: InputPosition::from_metres(1.0, 2.0, 3.0).map(AlcState::at_position),
            vitals: None,
            first: vec![gatherable.clone()],
            death: vec![death.clone()],
            drawn: Vec::new(),
        };
        let state = |health| MobGhost {
            health,
            max_health: HEALTH,
            returning: false,
            vitals_id: 1,
            category_id: 2,
            level: 1,
        };
        let stamp = nw_messages::AlcStamp {
            id: 7,
            anchor_us: NOW * 1000,
            offset_ms: 33,
        };
        let frame = |step| MobFrame {
            stamp,
            animation: None,
            facing: None,
            anchor: None,
            step,
            alert_started: false,
            fresh: false,
            drawing: false,
        };
        const CARD_SERVER_MS: u64 = 845_730_278_206;
        let card = MobCard {
            alc: Some(frame(None)),
            health_changed: false,
            server_ms: CARD_SERVER_MS,
            ..card_of(ENTITY, at(), state(HEALTH), None)
        };
        let first = mob_group(FIRST_REMOTE_INTEREST, &card, Some(&look), true)
            .expect("encode")
            .expect("group");
        let MemberState::GdeMetadata(gde) = &first.members[0].state else {
            panic!("the card first: {first:?}");
        };
        assert_eq!(gde.asset.map(|asset| asset.id), Some(SAILOR_ASSET));
        assert_eq!(first.members[1], gatherable);
        assert_eq!(first.members[2].component, VITALS_COMPONENT);
        let MemberState::Vitals(vitals) = &first.members[2].state else {
            panic!("the vitals third: {first:?}");
        };
        assert_eq!(
            vitals.slot_0.and_then(|slot| slot.vitals_data),
            Some(VitalsData::creature_first(HEALTH, CARD_SERVER_MS)),
            "a live mob's first vitals carry the card time, as on retail"
        );
        assert_eq!(
            first.members.last().map(|member| member.component),
            Some(MOB_ALC_COMPONENT)
        );
        let Some(MemberState::Alc(alc)) = first.members.last().map(|member| &member.state) else {
            panic!("the position last");
        };
        assert_eq!(
            alc.pos10,
            InputPosition::from_metres(at().x, at().y, at().z),
            "the recorded state at the mob's place"
        );
        assert_eq!(
            mob_group(FIRST_REMOTE_INTEREST, &card, Some(&look), false).expect("encode"),
            None,
            "no health change and no state due: nothing to send"
        );
        let step = nw_messages::MobStep {
            stamp,
            anchored: false,
            position: InputPosition::from_metres(at().x, at().y, at().z).map(StepPlace::Anchor),
            facing: None,
            scope: None,
            awake: false,
        };
        let moving = MobCard {
            alc: Some(frame(Some(step))),
            ..card
        };
        let full = mob_group(FIRST_REMOTE_INTEREST, &moving, Some(&look), false)
            .expect("encode")
            .expect("group");
        assert_eq!(full.members.len(), 1, "full health: the state only");
        assert_eq!(
            full.members[0].state,
            MemberState::Alc(AlcState::mob_step(&step).expect("step")),
            "a later state carries the state number and clock"
        );
        let unchanged = MobCard {
            health_changed: false,
            ..card_of(ENTITY, at(), state(HEALTH / 2), None)
        };
        assert_eq!(
            mob_group(FIRST_REMOTE_INTEREST, &unchanged, Some(&look), false)
                .expect("encode")
                .map(|group| group.members.len()),
            Some(1),
            "a hurt mob sends its health once per change: the plain position only"
        );
        let hurt = card_of(ENTITY, at(), state(HEALTH / 2), None);
        let hurt = mob_group(FIRST_REMOTE_INTEREST, &hurt, Some(&look), false)
            .expect("encode")
            .expect("group");
        assert_eq!(
            hurt.members[0].state,
            MemberState::Vitals(VitalsState::creature_health(HEALTH / 2))
        );
        let dead = card_of(ENTITY, at(), state(0), Some(OWN));
        assert!(dead.seen_by(OWN) && !dead.seen_by(OWN + 1));
        let dead = mob_group(FIRST_REMOTE_INTEREST, &dead, Some(&look), false)
            .expect("encode")
            .expect("group");
        assert_eq!(dead.members[1], death);
        let MemberState::Vitals(vitals) = &dead.members[0].state else {
            panic!("vitals first: {dead:?}");
        };
        assert_eq!(
            vitals
                .slot_0
                .and_then(|slot| slot.vitals_data)
                .map(|data| data.head),
            Some(1)
        );
    }

    #[test]
    fn bundles_split_by_bytes_and_carry_the_stops() {
        let mut slots = Slots::new();
        let groups: Vec<BundleGroup> = (0..MANY)
            .map(|index| {
                let interest = slots.slot(ENTITY + index).expect("slot").interest;
                entity_group(
                    interest,
                    ENTITY + index,
                    EntityKind::mob(ENTITY + index),
                    at(),
                    true,
                    None,
                )
                .expect("finite")
            })
            .collect();
        slots.keep_only(&[]);
        let envelopes =
            bundles(SEQUENCE, LOGIN_CLIENT_CONTEXT, &mut slots, groups).expect("bundles");
        assert!(envelopes.len() > 1);
        let reads = reads(&envelopes);
        let mut entities = EntitySlots::default();
        let mut seen = Vec::new();
        let mut stopped = 0;
        for mut read in reads {
            stopped += read.stops.len();
            entities.resolve(&mut read);
            seen.extend(read.updates.iter().filter_map(|update| update.entity));
        }
        let expected: Vec<u64> = (0..MANY).map(|index| ENTITY + index).collect();
        assert_eq!(seen, expected, "every group once, in order");
        assert_eq!(stopped, MANY as usize, "every stop goes out");
        assert!(slots.take_stops(ALL_STOPS).is_empty());
    }

    #[test]
    fn stops_without_groups_still_go_out() {
        let mut slots = Slots::new();
        slots.slot(ENTITY);
        slots.keep_only(&[]);
        let envelopes =
            bundles(SEQUENCE, LOGIN_CLIENT_CONTEXT, &mut slots, Vec::new()).expect("bundles");
        let reads = reads(&envelopes);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].stops, vec![FIRST_REMOTE_INTEREST]);
        assert!(
            bundles(SEQUENCE, LOGIN_CLIENT_CONTEXT, &mut slots, Vec::new())
                .expect("bundles")
                .is_empty()
        );
    }
}
