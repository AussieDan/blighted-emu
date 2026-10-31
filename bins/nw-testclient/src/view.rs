// SPDX-License-Identifier: AGPL-3.0-or-later
//! What one test client learns from the world messages (M4 spec section 9): the order of the
//! login messages, its own entity number (from `ClientAddEntryMsg`), and every position of every
//! entity in the replication bundles, in order. A bundle carries one group per entity slot; a
//! slot is tied to its entity by the spawn card in it ([`EntitySlots`]), and slot 1 is the
//! client's own player. A hunt bot also keeps the mobs it is sent and counts the combat messages
//! for it (M6-B spec section 9.1).

use std::collections::BTreeMap;

use nw_messages::entity_ids::IdHolder;
use nw_messages::{
    EntitySlots, LOGIN_MESSAGE_TYPES, MemberState, MessageError, PLAYER_SLICE_ASSET, ServerMessage,
    SlotRead, WorldPosition, entity_of_facet, number_of_seen_raw_id, release_raw_id,
};
use thiserror::Error;

use crate::rep::Received;

/// A world message that does not decode.
#[derive(Debug, Error)]
pub enum ViewError {
    /// A message of a world type, or a bundle payload, does not decode.
    #[error("message type {type_index} does not decode: {source}")]
    Message {
        /// Its type index.
        type_index: u64,
        /// Why.
        #[source]
        source: MessageError,
    },
}

/// A combat message, by the entity it is for; `None` for an entity this process never saw.
enum CombatEvent {
    /// 2071: a hit the entity made.
    Dealt { attacker: Option<u64> },
    /// 3601: a hit the entity took.
    Taken { target: Option<u64> },
    /// 4299: the entity died.
    Died { player: Option<u64> },
}

/// One world message, read.
enum Read {
    /// Not a world message.
    Skipped,
    /// A bundle, before its slots are resolved.
    Bundle(SlotRead),
    /// A combat message.
    Combat { type_index: u64, event: CombatEvent },
    /// `ClientAddEntryMsg`, with the client's own entity number.
    Entry { type_index: u64, own: Option<u64> },
    /// Another world message.
    Other { type_index: u64 },
}

/// Numbers the entity of every spawn card in `read` by its raw id ([`number_of_seen_raw_id`]),
/// held by `holder`: the server's raw ids carry no entity number, so the clients of this process
/// key the entities they see by these numbers, and all of them agree.
fn number_cards(read: &mut SlotRead, holder: IdHolder) {
    for update in &mut read.updates {
        if let Some(gde_ref) = update.card.and_then(|card| card.gde_ref) {
            update.entity = Some(number_of_seen_raw_id(holder, &gde_ref));
        }
    }
}

/// How [`decode`] numbers the entities of the spawn cards: by `holder`, or not at all.
#[derive(Debug, Clone, Copy)]
enum Cards {
    /// The cards' entities are numbered and held by the holder.
    Numbered,
    /// The cards are read without numbers (a view that keeps only its own player).
    Skipped,
}

fn decode(received: &Received, holder: IdHolder, cards: Cards) -> Result<Read, ViewError> {
    let envelope = &received.envelope;
    let Some(decoded) = ServerMessage::from_envelope(envelope) else {
        return Ok(Read::Skipped);
    };
    let type_index = envelope.type_index().unwrap_or_default();
    let failed = |source| ViewError::Message { type_index, source };
    Ok(match decoded.map_err(failed)? {
        ServerMessage::Bundle(bundle) => {
            let mut read = SlotRead::of(&bundle).map_err(failed)?;
            if matches!(cards, Cards::Numbered) {
                number_cards(&mut read, holder);
            }
            Read::Bundle(read)
        }
        ServerMessage::DamageDealt(hit) => Read::Combat {
            type_index,
            event: CombatEvent::Dealt {
                attacker: entity_of_facet(hit.head_b),
            },
        },
        ServerMessage::OnDamage(hit) => Read::Combat {
            type_index,
            event: CombatEvent::Taken {
                target: entity_of_facet(hit.head_b),
            },
        },
        ServerMessage::DeathRecap(death) => Read::Combat {
            type_index,
            event: CombatEvent::Died {
                player: entity_of_facet(death.head_b),
            },
        },
        ServerMessage::ClientAddEntry(entry) => Read::Entry {
            type_index,
            own: Some(number_of_seen_raw_id(holder, &entry.entry_id)),
        },
        _ => Read::Other { type_index },
    })
}

/// What a bot keeps of the world messages: its own entity number only. Many bots run in one
/// process, so the others' positions are read and dropped (M5 load spec section 7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnView {
    own: Option<u64>,
    /// Its hold on the ids of its own player and the entities its owner view keeps.
    holder: IdHolder,
}

impl OwnView {
    /// Reads one more message; returns the client's own position when it is a bundle with it.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] for a world message or a bundle payload that does not decode.
    pub fn read(&mut self, received: &Received) -> Result<Option<WorldPosition>, ViewError> {
        Ok(self.apply(&decode(received, self.holder, Cards::Skipped)?))
    }

    /// Learns the own number from `read`; returns the own position when it is in a bundle.
    fn apply(&mut self, read: &Read) -> Option<WorldPosition> {
        match read {
            Read::Bundle(bundle) => bundle.local_player_position(),
            Read::Entry { own, .. } => {
                self.own = *own;
                None
            }
            Read::Skipped | Read::Combat { .. } | Read::Other { .. } => None,
        }
    }

    /// Whether `number` is the own entity number.
    fn is_own(&self, number: u64) -> bool {
        self.own == Some(number)
    }

    /// The own entity number, once the entry message named it.
    pub const fn own_number(&self) -> Option<u64> {
        self.own
    }
}

/// What a swarm bot keeps to measure how fast others see a player's action (replication note):
/// its own position, and the block state (`isBlockActive` of 1528) of each other player as it
/// changes in its bundles.
#[derive(Debug, Clone, Default)]
pub struct SyncView {
    own: OwnView,
    slots: EntitySlots,
}

/// One message as a [`SyncView`] read it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncRead {
    /// The own position, when the message is a bundle with it.
    pub own_position: Option<WorldPosition>,
    /// The block states of other entities it carried, by entity number.
    pub blocks: Vec<(u64, bool)>,
}

impl SyncView {
    /// Reads one more message.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] for a world message or a bundle payload that does not decode.
    pub fn read(&mut self, received: &Received) -> Result<SyncRead, ViewError> {
        let mut read = decode(received, self.own.holder, Cards::Numbered)?;
        let own_position = self.own.apply(&read);
        let mut blocks = Vec::new();
        if let Read::Bundle(bundle) = &mut read {
            for gone in self.slots.resolve(bundle) {
                release_raw_id(self.own.holder, gone);
            }
            for update in &bundle.updates {
                let Some(entity) = update.entity.filter(|entity| !self.own.is_own(*entity)) else {
                    continue;
                };
                blocks.extend(
                    update
                        .members
                        .iter()
                        .filter_map(|member| match &member.state {
                            MemberState::DamageReceiver(receiver) => {
                                receiver.is_block_active.map(|blocking| (entity, blocking))
                            }
                            _ => None,
                        }),
                );
            }
        }
        Ok(SyncRead {
            own_position,
            blocks,
        })
    }

    /// The own entity number, once the entry message named it.
    pub const fn own_number(&self) -> Option<u64> {
        self.own.own_number()
    }
}

/// Bundles with the own position that a mob may miss before the view drops it: each tick sends
/// the own position once, and a mob in view in every tick.
pub const MAX_MISSED_ROUNDS: u64 = 2;

/// A mob the bot is sent.
#[derive(Debug, Clone, Copy)]
struct SeenMob {
    /// `None` until its first position.
    position: Option<WorldPosition>,
    /// The round it was last sent in.
    round: u64,
}

/// What a hunt bot keeps: its own position, the mobs it is sent, and counts of the combat
/// messages for it. An entity is a mob from a spawn card of another prefab than the player's
/// on; a mob whose slot is stopped, or that is not sent for more than [`MAX_MISSED_ROUNDS`]
/// rounds (one round per bundle with the own position), is dead or out of view, and it comes back
/// with a new card.
#[derive(Debug, Clone, Default)]
pub struct CombatView {
    own: OwnView,
    round: u64,
    slots: EntitySlots,
    mobs: BTreeMap<u64, SeenMob>,
    hits_dealt: u64,
    hits_taken: u64,
    deaths: u64,
}

impl CombatView {
    /// Reads one more message; returns the client's own position when it is a bundle with it.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] for a world message or a bundle payload that does not decode.
    pub fn read(&mut self, received: &Received) -> Result<Option<WorldPosition>, ViewError> {
        let read = decode(received, self.own.holder, Cards::Numbered)?;
        let own_position = self.own.apply(&read);
        match read {
            Read::Bundle(mut bundle) => {
                for gone in self.slots.resolve(&mut bundle) {
                    self.mobs.remove(&gone);
                    release_raw_id(self.own.holder, gone);
                }
                let round = self.round;
                for update in bundle
                    .updates
                    .iter()
                    .filter(|update| !update.is_local_player())
                {
                    let Some(entity) = update.entity else {
                        continue;
                    };
                    if update
                        .card
                        .is_some_and(|card| card.asset != Some(PLAYER_SLICE_ASSET))
                    {
                        self.mobs.insert(
                            entity,
                            SeenMob {
                                position: None,
                                round,
                            },
                        );
                    }
                    if let Some(mob) = self.mobs.get_mut(&entity) {
                        mob.round = round;
                        if update.position.is_some() {
                            mob.position = update.position;
                        }
                    }
                }
                if own_position.is_some() {
                    self.round += 1;
                    let now = self.round;
                    self.mobs
                        .retain(|_, mob| now - mob.round <= MAX_MISSED_ROUNDS);
                }
            }
            Read::Combat { event, .. } => {
                let (counter, number) = match event {
                    CombatEvent::Dealt { attacker } => (&mut self.hits_dealt, attacker),
                    CombatEvent::Taken { target } => (&mut self.hits_taken, target),
                    CombatEvent::Died { player } => (&mut self.deaths, player),
                };
                if number.is_some_and(|number| self.own.is_own(number)) {
                    *counter += 1;
                }
            }
            Read::Skipped | Read::Entry { .. } | Read::Other { .. } => {}
        }
        Ok(own_position)
    }

    /// The mobs with a position, by entity number.
    pub fn mobs(&self) -> impl Iterator<Item = (u64, WorldPosition)> + '_ {
        self.mobs
            .iter()
            .filter_map(|(number, mob)| Some((*number, mob.position?)))
    }

    /// 2071 messages for hits this bot made.
    pub const fn hits_dealt(&self) -> u64 {
        self.hits_dealt
    }

    /// 3601 messages for hits this bot took.
    pub const fn hits_taken(&self) -> u64 {
        self.hits_taken
    }

    /// 4299 messages for this bot's deaths.
    pub const fn deaths(&self) -> u64 {
        self.deaths
    }
}

/// The world as one client saw it.
#[derive(Debug, Clone, Default)]
pub struct WorldView {
    /// Type indexes of the world messages that are not bundles, in arrival order.
    login: Vec<u64>,
    /// How many of them came before the first bundle.
    login_before_first_bundle: Option<usize>,
    own: Option<u64>,
    slots: EntitySlots,
    tracks: BTreeMap<u64, Vec<WorldPosition>>,
    /// Its hold on the ids it numbered; a world view keeps every track of its run, so it holds
    /// them for its life (it reads one short run).
    holder: IdHolder,
}

/// Two views are equal when they saw the same; their id holders differ.
impl PartialEq for WorldView {
    fn eq(&self, other: &Self) -> bool {
        self.login == other.login
            && self.login_before_first_bundle == other.login_before_first_bundle
            && self.own == other.own
            && self.slots == other.slots
            && self.tracks == other.tracks
    }
}

impl WorldView {
    /// Reads `messages` in order. Messages of other types are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] for a world message or a bundle payload that does not decode.
    pub fn from_messages(messages: &[Received]) -> Result<Self, ViewError> {
        let mut view = Self::default();
        for received in messages {
            view.read(received)?;
        }
        Ok(view)
    }

    /// Reads one more message. Messages of other types are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] for a world message or a bundle payload that does not decode.
    pub fn read(&mut self, received: &Received) -> Result<(), ViewError> {
        match decode(received, self.holder, Cards::Numbered)? {
            Read::Skipped => {}
            Read::Bundle(mut bundle) => {
                self.login_before_first_bundle
                    .get_or_insert(self.login.len());
                self.slots.resolve(&mut bundle);
                for update in bundle.updates {
                    let entity = update
                        .entity
                        .or_else(|| update.is_local_player().then_some(self.own).flatten());
                    if let (Some(entity), Some(position)) = (entity, update.position) {
                        self.tracks.entry(entity).or_default().push(position);
                    }
                }
            }
            Read::Entry { type_index, own } => {
                self.own = own;
                self.login.push(type_index);
            }
            Read::Combat { type_index, .. } | Read::Other { type_index } => {
                self.login.push(type_index);
            }
        }
        Ok(())
    }

    /// Whether the login messages came first, in the captured order, and bundles after them.
    pub fn in_captured_order(&self) -> bool {
        self.login.starts_with(LOGIN_MESSAGE_TYPES)
            && self
                .login_before_first_bundle
                .is_some_and(|count| count >= LOGIN_MESSAGE_TYPES.len())
    }

    /// The client's own entity number.
    pub const fn own_number(&self) -> Option<u64> {
        self.own
    }

    /// The last position of entity `number`.
    pub fn position(&self, number: u64) -> Option<WorldPosition> {
        self.track(number).last().copied()
    }

    /// Every position of entity `number`, in order.
    pub fn track(&self, number: u64) -> &[WorldPosition] {
        self.tracks.get(&number).map_or(&[], Vec::as_slice)
    }

    /// The client's own last position.
    pub fn own_position(&self) -> Option<WorldPosition> {
        self.position(self.own?)
    }

    /// Every other entity number the client saw, in ascending order.
    pub fn others(&self) -> Vec<u64> {
        self.tracks
            .keys()
            .copied()
            .filter(|number| Some(*number) != self.own)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::test_support::{
        FIRST_OTHER_SLOT, entity_group, login_samples, melee_sample, players_bundle,
    };
    use nw_messages::{
        AssetId, BodyCodec, BundleGroup, ClientAddEntryMsg, DYNAMIC_SLICE_SUB_ID, DamageDealtMsg,
        DeathRecapMsg, GDE_ID_LEN, LOCAL_PLAYER_INTEREST, LOGIN_CLIENT_CONTEXT,
        ReplicatedStateBundle,
    };
    use nw_reflect::{Envelope, TypeId};

    use super::*;

    const OWN: u64 = 1;
    const OTHER: u64 = 2;
    const LOGIN_CHANNEL: u8 = 0;
    const BUNDLE_CHANNEL: u8 = 1;
    /// `PingMsg`: not a world message.
    const OTHER_TYPE_INDEX: u64 = 349;
    const SHORT_BODY: [u8; 3] = [1, 2, 3];

    fn received(channel: u8, message: &ServerMessage) -> Received {
        let body = message.encode().expect("encode");
        Received {
            channel,
            envelope: Envelope::message(TypeId::FileSlot(message.type_index()), body),
        }
    }

    fn login() -> Vec<Received> {
        login_samples(OWN)
            .iter()
            .map(|message| received(LOGIN_CHANNEL, message))
            .collect()
    }

    fn at(x: f32) -> WorldPosition {
        WorldPosition::from_metres(x, x, x).expect("finite")
    }

    fn bundle(sequence: u64, positions: &[(u64, WorldPosition)]) -> Received {
        let bundle = players_bundle(sequence, OWN, positions);
        received(BUNDLE_CHANNEL, &ServerMessage::Bundle(bundle))
    }

    #[test]
    fn the_login_order_the_own_number_and_the_last_positions_are_learned() {
        let mut messages = login();
        messages.push(bundle(0, &[(OWN, at(1.0)), (OTHER, at(2.0))]));
        messages.push(Received {
            channel: LOGIN_CHANNEL,
            envelope: Envelope::message(TypeId::FileSlot(OTHER_TYPE_INDEX), SHORT_BODY.to_vec()),
        });
        messages.push(bundle(1, &[(OWN, at(1.0)), (OTHER, at(3.0))]));
        let view = WorldView::from_messages(&messages).expect("view");
        assert!(view.in_captured_order());
        assert_eq!(view.own_number(), Some(OWN));
        assert_eq!(view.own_position(), Some(at(1.0)));
        assert_eq!(view.position(OTHER), Some(at(3.0)));
        assert_eq!(view.track(OTHER), [at(2.0), at(3.0)]);
        assert!(view.track(OTHER + 1).is_empty());
        assert_eq!(view.others(), vec![OTHER]);
    }

    #[test]
    fn the_light_view_keeps_only_the_own_position() {
        let mut view = OwnView::default();
        let before = bundle(0, &[(OWN, at(1.0))]);
        assert_eq!(
            view.read(&before).expect("read"),
            Some(at(1.0)),
            "slot 1 is the own player before the own number is known"
        );
        for message in login() {
            assert_eq!(view.read(&message).expect("read"), None);
        }
        let seen = bundle(1, &[(OTHER, at(2.0)), (OWN, at(3.0))]);
        assert_eq!(view.read(&seen).expect("read"), Some(at(3.0)));
        let others_only = bundle(2, &[(OTHER, at(4.0))]);
        assert_eq!(view.read(&others_only).expect("read"), None);
    }

    #[test]
    fn messages_can_be_read_one_by_one() {
        let mut messages = login();
        messages.push(bundle(0, &[(OWN, at(1.0))]));
        let mut view = WorldView::default();
        for received in &messages {
            view.read(received).expect("read");
        }
        assert_eq!(view, WorldView::from_messages(&messages).expect("view"));
    }

    #[test]
    fn a_bundle_before_the_login_or_a_changed_order_is_out_of_order() {
        let mut early = vec![bundle(0, &[(OWN, at(1.0))])];
        early.extend(login());
        assert!(
            !WorldView::from_messages(&early)
                .expect("view")
                .in_captured_order()
        );
        let mut swapped = login();
        swapped.swap(0, 1);
        swapped.push(bundle(0, &[(OWN, at(1.0))]));
        assert!(
            !WorldView::from_messages(&swapped)
                .expect("view")
                .in_captured_order()
        );
        assert!(
            !WorldView::from_messages(&login())
                .expect("view")
                .in_captured_order()
        );
    }

    const MOB: u64 = (1 << 34) + 5;
    /// The mob of the slot stop test: the stop releases its raw id from the process registry,
    /// which the other tests, running at the same time, must not lose for [`MOB`].
    const STOPPED_MOB: u64 = (1 << 34) + 6;
    const HIT: f32 = 42.0;

    const MOB_SLOT: u16 = FIRST_OTHER_SLOT;
    const OTHER_SLOT: u16 = FIRST_OTHER_SLOT + 1;
    const MOB_ASSET: AssetId = AssetId {
        id: [0; GDE_ID_LEN],
        sub_id: DYNAMIC_SLICE_SUB_ID,
    };

    /// A bundle that stops `stops`, with the own position and then `groups`.
    fn mob_bundle(sequence: u64, stops: Vec<u16>, groups: Vec<BundleGroup>) -> Received {
        let own = entity_group(LOCAL_PLAYER_INTEREST, OWN, None, Some(at(1.0)));
        let groups = std::iter::once(own).chain(groups).collect();
        let bundle =
            ReplicatedStateBundle::of_groups(sequence, LOGIN_CLIENT_CONTEXT, stops, groups)
                .expect("bundle");
        received(BUNDLE_CHANNEL, &ServerMessage::Bundle(bundle))
    }

    fn mob_at(mob: u64, x: f32, carded: bool) -> BundleGroup {
        entity_group(MOB_SLOT, mob, carded.then_some(MOB_ASSET), Some(at(x)))
    }

    fn combat_view() -> CombatView {
        let mut view = CombatView::default();
        for message in login() {
            view.read(&message).expect("read");
        }
        view
    }

    #[test]
    fn the_view_keeps_mobs_while_they_are_sent() {
        let mut view = combat_view();
        let other = entity_group(
            OTHER_SLOT,
            OTHER,
            Some(nw_messages::PLAYER_SLICE_ASSET),
            Some(at(3.0)),
        );
        let first = mob_bundle(0, Vec::new(), vec![mob_at(MOB, 2.0, true), other]);
        assert_eq!(view.read(&first).expect("read"), Some(at(1.0)));
        assert_eq!(
            view.mobs().collect::<Vec<_>>(),
            vec![(MOB, at(2.0))],
            "a player is no mob"
        );
        view.read(&mob_bundle(1, Vec::new(), vec![mob_at(MOB, 4.0, false)]))
            .expect("read");
        assert_eq!(view.mobs().collect::<Vec<_>>(), vec![(MOB, at(4.0))]);
        for sequence in 2..=2 + MAX_MISSED_ROUNDS {
            view.read(&mob_bundle(sequence, Vec::new(), Vec::new()))
                .expect("read");
        }
        assert_eq!(
            view.mobs().count(),
            0,
            "no longer sent: dead or out of view"
        );
        view.read(&mob_bundle(9, Vec::new(), vec![mob_at(MOB, 4.0, false)]))
            .expect("read");
        assert_eq!(view.mobs().count(), 0, "a mob comes back with a new card");
    }

    #[test]
    fn a_stopped_slot_drops_its_mob_at_once() {
        let mut view = combat_view();
        view.read(&mob_bundle(
            0,
            Vec::new(),
            vec![mob_at(STOPPED_MOB, 2.0, true)],
        ))
        .expect("read");
        assert_eq!(view.mobs().count(), 1);
        view.read(&mob_bundle(1, vec![MOB_SLOT], Vec::new()))
            .expect("read");
        assert_eq!(view.mobs().count(), 0);
    }

    #[test]
    fn the_view_counts_hits_damage_and_deaths() {
        let mut view = combat_view();
        let messages = [
            ServerMessage::DamageDealt(DamageDealtMsg::hit(OWN, MOB, HIT, false)),
            ServerMessage::DamageDealt(DamageDealtMsg::hit(OTHER, MOB, HIT, false)),
            ServerMessage::OnDamage(melee_sample(OWN, MOB, HIT)),
            ServerMessage::OnDamage(melee_sample(OTHER, MOB, HIT)),
            ServerMessage::DeathRecap(DeathRecapMsg::of(OWN)),
            ServerMessage::DeathRecap(DeathRecapMsg::of(OTHER)),
        ];
        for message in &messages {
            assert_eq!(
                view.read(&received(LOGIN_CHANNEL, message)).expect("read"),
                None
            );
        }
        assert_eq!(
            (view.hits_dealt(), view.hits_taken(), view.deaths()),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_world_message_that_does_not_decode_is_an_error() {
        let broken = Received {
            channel: LOGIN_CHANNEL,
            envelope: Envelope::message(
                TypeId::FileSlot(ClientAddEntryMsg::TYPE_INDEX),
                SHORT_BODY.to_vec(),
            ),
        };
        assert!(matches!(
            WorldView::from_messages(&[broken]),
            Err(ViewError::Message {
                type_index: ClientAddEntryMsg::TYPE_INDEX,
                ..
            })
        ));
    }
}
