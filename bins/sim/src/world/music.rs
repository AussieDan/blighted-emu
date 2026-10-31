// SPDX-License-Identifier: AGPL-3.0-or-later
//! Musical performances (run 15 music study; retail `open-world/Music`,
//! `music_abort_current_song`, `20261010-113829-464` and the play-for-the-town side quest).
//!
//! A player with an instrument in an instrument slot creates a performance (4891): the server
//! spawns the performance entity (its spawn group: the card of slice `ffce76a6...` sub 2, the
//! performance state at component 2 and the position) and answers with its id (4869), or
//! refuses with the reason, the zone radius and the zone type (4889). The performance waits to
//! be joined, then shows its pre show; the performer picks a song (4867), a sheet (4879) and a
//! reward (5022) and starts (4865): the clients load the song (`PreLoad`), the performer's client
//! says it is ready (5091), the song plays from the start time the state names, and each note the
//! performer plays comes in (4892). At the song's end plus 1.05 s the results show the score
//! (`performerScore`, `performanceScore`) and the performer gets the `Musician` experience of the
//! song's completion event; the post show follows 3.05 s later, and the performance is done at
//! the 5233 or 15 s later and goes back to its pre show. A song can be stopped (5417). Leaving
//! (4881) ends the performance (`Ending`; the group goes 5.03 s later) and starts the
//! performer's lockout (`lockoutDuration` 40, cleared 10 s later; persisted as
//! `MusicalPerformancePlayerPersistence.m_nextPerformanceStartTime`).
//!
//! Every player within the zone radius watches the performance (`observingPerformanceId`) and
//! counts in `performanceZonePlayerCount`. A viewer outside the zone gets slot 0 of the state, a
//! viewer in the zone slots 0 and 1, the performer every slot (Observed).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::{ConnectionRef, PlayerMusic};
use nw_content::music::{MUSIC_TRADESKILL, Song};
use nw_messages::gde::AssetId;
use nw_messages::items::FullList;
use nw_messages::list_delta::{ListChange, ReplicatedList};
use nw_messages::music::{
    MAX_PERFORMERS, MUSIC_CHILD, MUSIC_FACET, MusicClientMsg, MusicServerMsg,
    MusicalPerformanceState, NO_PERFORMANCE, PERFORMANCE_COMPONENT, PERFORMANCE_PLAYER_COMPONENT,
    PerformanceError, PerformancePlayerState, PerformanceStage,
};
use nw_messages::position::{PositionInTheWorldReplicatedState, WorldPosition};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BundleGroup, BundleMember, FacetHeader, GdeMetadataReplicatedState, MemberState, ServerMessage,
    facet_entity, facet_of_raw_id,
};
use nw_sim_core::Position;
use uuid::Uuid;

use super::meta_achievements::has_achievement;
use super::slots::Slot;
use super::tradeskill::progression_gain;
use super::{Body, Player, SimWorld, send_now};
use crate::objectives::Event;

/// First slot key of a performance: its serial is added. Keys are never sent; they stay above
/// every player and mob number and below the crafting stations.
pub(super) const PERFORMANCE_ENTITY_BASE: u64 = 1 << 36;
/// Names the performance ids this server makes.
const PERFORMANCE_ID_NAMESPACE: u64 = 0x6d75_7369_6370_6572;
/// The performance's slice asset (retail card at every create, six sessions).
const PERFORMANCE_ASSET: AssetId = AssetId {
    id: [
        0xff, 0xce, 0x76, 0xa6, 0xaf, 0x8d, 0x5c, 0x4a, 0xb9, 0x67, 0xf4, 0x91, 0xaf, 0x45, 0x16,
        0x42,
    ],
    sub_id: 2,
};
/// The components of a performance's group besides its state.
const CARD_COMPONENT: u64 = 0;
const POSITION_COMPONENT: u64 = 1;
/// The paperdoll slots of the instruments (`PaperdollSlotTypes` `INSTRUMENT_FLUTE_SLOT` to
/// `INSTRUMENT_DRUMS_SLOT`, `FUN_145cac380`).
const INSTRUMENT_SLOTS: std::ops::RangeInclusive<u32> = 39..=43;
/// The zone's `radius` and `zoneType` (Observed: the same in every retail state and 4889).
const ZONE_RADIUS_M: f32 = 20.0;
const ZONE_TYPE: u32 = 0x8e55_58d9;
/// `flags`, `leaderIndex`, `songBookFilterType`, `rewardFilterType` (2: `ByFilterId`) and
/// `endReason` as every retail performance has them.
const FLAGS: u8 = 1;
const LEADER_INDEX: u8 = 0;
const BY_FILTER_ID: u8 = 2;
const END_REASON: u8 = 0;
/// The performer's place in the performer lists (a group's other places: Unknown, no capture
/// has two performers).
const PERFORMER_PLACE: usize = 0;
/// A seed is a random value below this (Observed: every retail seed).
const SEED_LIMIT: u32 = 0x8000;
/// The low 24 bits of `errorSync`: its change counter; the top byte flags a performer's mistake
/// (Observed: `0x01000006` after a note without a key).
const SYNC_COUNTER: u32 = 0x00ff_ffff;
const MISTAKE_FLAG: u32 = 1 << 24;
/// The key of a note played without a key: a mistake.
const NO_KEY: u8 = 0;
/// When the pre show follows the create (retail 36 to 80 ms; 52 ms in `open-world/Music`).
const PRE_SHOW_AFTER_CREATE_MS: u64 = 52;
/// When the results follow the song's end (retail 1,052 and 1,077 ms after start time plus
/// `Duration`).
const RESULTS_AFTER_SONG_MS: u64 = 1_065;
/// When the post show follows the results (retail 3.03 to 3.07 s).
const POST_SHOW_AFTER_RESULTS_MS: u64 = 3_050;
/// When the performance is done without a 5233 (retail 15.0 s after the post show).
const DONE_AFTER_POST_SHOW_MS: u64 = 15_000;
/// When the pre show follows a done performance (retail about 34 ms).
const PRE_SHOW_AFTER_DONE_MS: u64 = 34;
/// When an ended performance's group goes (retail 5.03 s after `Ending`).
const GONE_AFTER_ENDING_MS: u64 = 5_030;
/// The `lockoutDuration` of a leave and how long it lasts (retail 40, cleared after 10.0 to
/// 10.1 s).
const LOCKOUT: u16 = 40;
const LOCKOUT_MS: u64 = 10_000;
/// 1 while the player performs, else 0 (Observed: the only values of `performanceState`).
const PERFORMING: u8 = 1;
const NOT_PERFORMING: u8 = 0;
/// Nanoseconds per millisecond and milliseconds per second.
const NS_PER_MS: u64 = 1_000_000;
const MS_PER_S: f64 = 1_000.0;
/// One firing of a game event.
const ONCE: u32 = 1;

/// What a viewer of a performance gets of its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Slot 0: a player in view outside the zone.
    Outside,
    /// Slots 0 and 1: a player in the zone.
    Zone,
    /// Every slot: the performer.
    Performer,
}

impl Role {
    /// The part of `state` this role gets.
    fn filter(self, mut state: MusicalPerformanceState) -> MusicalPerformanceState {
        if self != Self::Performer {
            state.pages = None;
            state.song_book_filter = None;
            state.song_book_filter_id = None;
            state.reward_filter = None;
            state.reward_filter_id = None;
            state.rank = None;
            state.end_reason = None;
            state.seeds = [None; MAX_PERFORMERS];
            state.zone_players = None;
        }
        if self == Self::Outside {
            state.leader_index = None;
            state.error_sync = None;
            state.selected_reward = None;
            state.performer_scores = None;
            state.performance_score = None;
        }
        state
    }
}

/// What a performance does next, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Nothing until a call.
    Wait,
    /// The stage changes at `at_ms`.
    Stage { stage: PerformanceStage, at_ms: u64 },
    /// The group goes at `at_ms`.
    Gone { at_ms: u64 },
}

/// One performance.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Performance {
    /// Its entity id (the card's `GdeRef`).
    id: [u8; RAW_ID_LEN],
    /// Its wire id: the first 8 bytes of the entity id, reversed.
    wire_id: u64,
    /// Where it is.
    position: Position,
    /// The performer's number and facet entity.
    performer: u64,
    performer_entity: u64,
    stage: PerformanceStage,
    next: Next,
    song: Option<u32>,
    sheet_column: Option<u16>,
    reward: Option<u32>,
    start_ns: Option<u64>,
    /// The `errorSync` counter and flag.
    error_sync: u32,
    /// The notes played: by the bits of their time, whether every key of the note had one.
    notes: BTreeMap<u32, bool>,
    /// The performer's score: its maximum and earned points.
    score: (u32, u32),
    seed: u64,
    zone_players: u16,
    /// The achievement bits of the performer's unlocked pages.
    pages: Vec<u16>,
    /// The performer's `Musician` rank.
    rank: u16,
    /// The version of the lists it sends.
    list_version: u64,
    /// The changes not yet published, and the last published change and its version.
    pending: MusicalPerformanceState,
    published: MusicalPerformanceState,
    version: u32,
}

impl Performance {
    fn sync(&mut self) -> u32 {
        self.error_sync = (self.error_sync & SYNC_COUNTER).wrapping_add(1) & SYNC_COUNTER;
        self.error_sync
    }

    fn next_list_version(&mut self) -> u64 {
        self.list_version += 1;
        self.list_version
    }

    /// The whole state.
    fn whole(&self) -> MusicalPerformanceState {
        let mut performers = vec![NO_PERFORMANCE; MAX_PERFORMERS];
        performers[PERFORMER_PLACE] = self.performer_entity;
        let mut scores = vec![0; MAX_PERFORMERS];
        scores[PERFORMER_PLACE] = self.packed_score();
        let mut seeds = [None; MAX_PERFORMERS];
        seeds[PERFORMER_PLACE] = Some(self.seed);
        MusicalPerformanceState {
            performers: full_list(self.list_version, performers),
            radius: Some(ZONE_RADIUS_M),
            zone_type: Some(ZONE_TYPE),
            selected_song: self.song,
            sheet_indices: self.sheet_column,
            start_time_ns: self.start_ns,
            stage: Some(self.stage as u8),
            flags: Some(FLAGS),
            leader_index: Some(LEADER_INDEX),
            error_sync: Some(self.error_sync),
            selected_reward: self.reward,
            performer_scores: full_list(self.list_version, scores),
            performance_score: (self.score.0 > 0).then(|| self.performance_score()),
            pages: full_list(self.list_version, self.pages.clone()),
            song_book_filter: Some(BY_FILTER_ID),
            song_book_filter_id: None,
            reward_filter: Some(BY_FILTER_ID),
            reward_filter_id: None,
            rank: Some(self.rank),
            end_reason: Some(END_REASON),
            seeds,
            zone_players: Some(self.zone_players),
        }
    }

    /// The performer's score as `performerScore` holds it: maximum high, earned low.
    fn packed_score(&self) -> u32 {
        let (max, earned) = self.score;
        (max.min(u32::from(u16::MAX)) << u16::BITS) | earned.min(u32::from(u16::MAX))
    }

    /// The earned points over the maximum; 0 without notes.
    fn performance_score(&self) -> f32 {
        if self.score.0 == 0 {
            0.0
        } else {
            self.score.1 as f32 / self.score.0 as f32
        }
    }

    /// The stage changes to `stage`, with the next `errorSync`.
    fn enter(&mut self, stage: PerformanceStage) {
        self.stage = stage;
        self.pending.stage = Some(stage as u8);
        self.pending.error_sync = Some(self.sync());
    }

    /// The performer's score list entry changes.
    fn score_change(&mut self) {
        let version = self.next_list_version();
        self.pending.performer_scores = Some(ReplicatedList::Changes(vec![ListChange {
            index: PERFORMER_PLACE as u32,
            version,
            value: self.packed_score(),
        }]));
    }

    /// The members of the spawn group for a viewer in role `role`.
    fn spawn_members(&self, role: Role) -> Vec<BundleMember> {
        vec![
            BundleMember {
                component: CARD_COMPONENT,
                state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
                    asset: Some(PERFORMANCE_ASSET),
                    gde_ref: Some(self.id),
                }),
            },
            self.state_member(role.filter(self.whole())),
            BundleMember {
                component: POSITION_COMPONENT,
                state: MemberState::Position(PositionInTheWorldReplicatedState {
                    position: WorldPosition::from_metres(
                        self.position.x,
                        self.position.y,
                        self.position.z,
                    ),
                    ..PositionInTheWorldReplicatedState::default()
                }),
            },
        ]
    }

    fn state_member(&self, state: MusicalPerformanceState) -> BundleMember {
        BundleMember {
            component: PERFORMANCE_COMPONENT,
            state: MemberState::MusicalPerformance(state),
        }
    }
}

/// A viewer's knowledge of a performance: the version and the role it was last sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Seen {
    version: u32,
    role: Role,
}

/// The performances of one sim.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Performances {
    by_number: BTreeMap<u64, Performance>,
    next: u64,
}

impl Performances {
    /// The slot keys of the performances within `radius_m` of `position`.
    pub(super) fn numbers_near(
        &self,
        position: Position,
        radius_m: f32,
    ) -> impl Iterator<Item = u64> + '_ {
        self.by_number
            .iter()
            .filter(move |(_, performance)| {
                performance.position.horizontal_distance(position) <= radius_m
            })
            .map(|(number, _)| *number)
    }

    /// The group of the performance of slot key `number` in `slot` for viewer `viewer` at
    /// `position`, who got the versions `seen`: the spawn members when the slot is new, the
    /// whole state when its role changed or it missed a change, the last change otherwise.
    /// Updates `seen`.
    pub(super) fn group(
        &self,
        number: u64,
        slot: Slot,
        viewer: u64,
        position: Position,
        seen: &mut HashMap<u64, Seen>,
    ) -> Option<BundleGroup> {
        let performance = self.by_number.get(&number)?;
        let role = if viewer == performance.performer {
            Role::Performer
        } else if performance.position.horizontal_distance(position) <= ZONE_RADIUS_M {
            Role::Zone
        } else {
            Role::Outside
        };
        let known = seen.get(&number).copied();
        let members = if slot.new {
            performance.spawn_members(role)
        } else if known
            .is_some_and(|known| known.version == performance.version && known.role == role)
        {
            return None;
        } else if known.is_some_and(|known| {
            known.role == role && known.version.wrapping_add(1) == performance.version
        }) {
            let change = role.filter(performance.published.clone());
            if change.is_empty() {
                seen.insert(
                    number,
                    Seen {
                        version: performance.version,
                        role,
                    },
                );
                return None;
            }
            vec![performance.state_member(change)]
        } else {
            vec![performance.state_member(role.filter(performance.whole()))]
        };
        seen.insert(
            number,
            Seen {
                version: performance.version,
                role,
            },
        );
        Some(BundleGroup {
            interest: u64::from(slot.interest),
            members,
        })
    }

    /// The performance of performer `performer`.
    fn of_performer(&mut self, performer: u64) -> Option<(u64, &mut Performance)> {
        self.by_number
            .iter_mut()
            .find(|(_, performance)| {
                performance.performer == performer && performance.stage != PerformanceStage::Ending
            })
            .map(|(number, performance)| (*number, performance))
    }
}

/// One player's side of performances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PlayerMusicState {
    /// The performance it plays in (its wire id).
    performing: Option<u64>,
    /// The performance whose zone it is in (its wire id).
    observing: Option<u64>,
    /// Server time (ms since 2000-01-01) when its lockout ends.
    lockout_until_ms: Option<u64>,
    /// The changes of its own state not yet sent.
    change: PerformancePlayerState,
    /// The versions of the performances its client got, by slot key.
    pub(super) seen: HashMap<u64, Seen>,
}

impl PlayerMusicState {
    /// The state of a player that arrives with `saved`: a lockout that has not ended goes on.
    pub(super) fn of(saved: &PlayerMusic) -> Self {
        Self {
            lockout_until_ms: saved.next_performance_ms,
            ..Self::default()
        }
    }

    /// What a save or a handoff keeps.
    pub(super) fn carried(&self) -> PlayerMusic {
        PlayerMusic {
            next_performance_ms: self.lockout_until_ms,
        }
    }

    /// Whether it plays in a performance: the attack key plays notes.
    pub(super) const fn performing(&self) -> bool {
        self.performing.is_some()
    }

    /// Its own state for the next bundle: the whole state with the first card (with the
    /// lockout when one runs), the changes after one, else none.
    pub(super) fn take_own_member(&mut self, first: bool, server_ms: u64) -> Option<BundleMember> {
        let change = std::mem::take(&mut self.change);
        let state = if first {
            PerformancePlayerState {
                performance: Some(self.performing.unwrap_or(NO_PERFORMANCE)),
                lockout: self
                    .lockout_until_ms
                    .filter(|until| *until > server_ms)
                    .map(|_| LOCKOUT),
                performing: Some(if self.performing.is_some() {
                    PERFORMING
                } else {
                    NOT_PERFORMING
                }),
                observing: Some(self.observing.unwrap_or(NO_PERFORMANCE)),
            }
        } else {
            change
        };
        (!state.is_empty()).then_some(BundleMember {
            component: PERFORMANCE_PLAYER_COMPONENT,
            state: MemberState::PerformancePlayer(state),
        })
    }

    /// It watches performance `observing` (its wire id; `None` for none).
    fn observe(&mut self, observing: Option<u64>) {
        if self.observing != observing {
            self.observing = observing;
            self.change.observing = Some(observing.unwrap_or(NO_PERFORMANCE));
        }
    }

    /// Its lockout ended by `server_ms`: the client gets 0.
    fn lockout_due(&mut self, server_ms: u64) {
        if self
            .lockout_until_ms
            .is_some_and(|until| server_ms >= until)
        {
            self.lockout_until_ms = None;
            self.change.lockout = Some(0);
        }
    }
}

/// The header of a music call of player `number`.
fn music_header(number: u64) -> FacetHeader {
    FacetHeader::of_child(MUSIC_FACET, MUSIC_CHILD, number)
}

impl SimWorld {
    /// A music call of the client of `connection` at `now_ms`.
    pub fn music_call(&mut self, connection: &ConnectionRef, call: MusicClientMsg, now_ms: u64) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        if call.header() != music_header(number) {
            tracing::debug!(player = number, ?call, "music call of another target");
            return;
        }
        match call {
            MusicClientMsg::CreatePerformance { .. } => {
                self.create_performance(entity, number, now_ms);
            }
            MusicClientMsg::LeavePerformance { .. } => {
                self.leave_performance(entity, number, now_ms);
            }
            MusicClientMsg::SetSong { song, .. } => {
                self.with_performance(number, |performance, content| {
                    if song != 0 && content.music.song(song).is_none() {
                        return;
                    }
                    performance.song = (song != 0).then_some(song);
                    performance.pending.selected_song = Some(song);
                    if performance.sheet_column.take().is_some() {
                        performance.pending.sheet_indices = Some(0);
                    }
                });
            }
            MusicClientMsg::SetSheet { sheet, .. } => {
                let worn = self.worn_instrument_slots(entity);
                self.with_performance(number, |performance, content| {
                    let column = performance
                        .song
                        .and_then(|song| content.music.song(song))
                        .zip(content.music.sheet(sheet))
                        .filter(|(_, sheet)| {
                            content
                                .music
                                .instrument_slot(&sheet.instrument)
                                .is_some_and(|slot| worn.contains(&slot))
                        })
                        .and_then(|(song, sheet)| song.sheet_column(&sheet.id));
                    if let Some(column) = column {
                        performance.sheet_column = Some(column);
                        performance.pending.sheet_indices = Some(column);
                    }
                });
            }
            MusicClientMsg::SetReward { reward, .. } => {
                let (rank, _) = self.tradeskill_perks(entity, MUSIC_TRADESKILL);
                self.with_performance(number, |performance, content| {
                    if content
                        .music
                        .reward(reward)
                        .is_some_and(|row| rank >= row.rank)
                    {
                        performance.reward = Some(reward);
                        performance.pending.selected_reward = Some(reward);
                    }
                });
            }
            MusicClientMsg::StartPerformance { .. } => {
                self.with_performance(number, |performance, _| {
                    if performance.stage == PerformanceStage::PreShow
                        && performance.song.is_some()
                        && performance.sheet_column.is_some()
                    {
                        performance.notes.clear();
                        performance.enter(PerformanceStage::PreLoad);
                    }
                });
            }
            MusicClientMsg::ClientReady { ready, .. } => {
                let server_ms = self.server_ms;
                self.with_performance(number, |performance, content| {
                    let Some(song) = performance.song.and_then(|song| content.music.song(song))
                    else {
                        return;
                    };
                    if !ready || performance.stage != PerformanceStage::PreLoad {
                        return;
                    }
                    let start_ns = server_ms.saturating_mul(NS_PER_MS);
                    performance.start_ns = Some(start_ns);
                    performance.pending.start_time_ns = Some(start_ns);
                    performance.enter(PerformanceStage::Playing);
                    performance.next = Next::Stage {
                        stage: PerformanceStage::Results,
                        at_ms: now_ms
                            .saturating_add((song.duration_s * MS_PER_S) as u64)
                            .saturating_add(RESULTS_AFTER_SONG_MS),
                    };
                });
            }
            MusicClientMsg::ValidateInput { key, time_s, .. } => {
                self.with_performance(number, |performance, _| {
                    if performance.stage != PerformanceStage::Playing {
                        return;
                    }
                    let hit = key != NO_KEY;
                    let note = performance.notes.entry(time_s.to_bits()).or_insert(true);
                    *note &= hit;
                    if !hit {
                        performance.error_sync = performance.sync() | MISTAKE_FLAG;
                        performance.pending.error_sync = Some(performance.error_sync);
                    }
                });
            }
            MusicClientMsg::EndPostShow { .. } => {
                self.with_performance(number, |performance, _| {
                    if performance.stage == PerformanceStage::PostShow {
                        performance.enter(PerformanceStage::Done);
                        performance.next = Next::Stage {
                            stage: PerformanceStage::PreShow,
                            at_ms: now_ms.saturating_add(PRE_SHOW_AFTER_DONE_MS),
                        };
                    }
                });
            }
            MusicClientMsg::AbortSong { .. } => {
                let seed = self.dice.below(SEED_LIMIT);
                self.with_performance(number, |performance, _| {
                    if matches!(
                        performance.stage,
                        PerformanceStage::PreLoad | PerformanceStage::Playing
                    ) {
                        pre_show(performance, u64::from(seed));
                    }
                });
            }
        }
    }

    /// Runs `change` on the performance of performer `number` with the content.
    fn with_performance(
        &mut self,
        number: u64,
        change: impl FnOnce(&mut Performance, &crate::player_content::PlayerContent),
    ) {
        let content = Arc::clone(&self.content);
        if let Some((_, performance)) = self.performances.of_performer(number) {
            change(performance, &content);
        }
    }

    /// The instrument slots player `entity` wears an item in.
    fn worn_instrument_slots(&self, entity: Entity) -> Vec<u32> {
        self.world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map_or_else(Vec::new, |gear| {
                INSTRUMENT_SLOTS
                    .filter(|slot| gear.worn_item(*slot).is_some())
                    .collect()
            })
    }

    /// Player `number` (`entity`) asks for a performance at `now_ms`: refused with an instrument
    /// slot empty, during its lockout, while it performs, or within another performance's zone;
    /// else created where it stands.
    fn create_performance(&mut self, entity: Entity, number: u64, now_ms: u64) {
        let server_ms = self.server_ms;
        let Some(place) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return;
        };
        let instrument = !self.worn_instrument_slots(entity).is_empty();
        let performs = self.performances.by_number.values().any(|performance| {
            performance.performer == number && performance.stage != PerformanceStage::Ending
        });
        let refusal = {
            let Some(player) = self.world.get::<Player>(entity) else {
                return;
            };
            if performs {
                Some(PerformanceError::AlreadyAMember)
            } else if !instrument {
                Some(PerformanceError::CreateFailedNoInstrument)
            } else if player
                .music
                .lockout_until_ms
                .is_some_and(|until| until > server_ms)
            {
                Some(PerformanceError::CreateFailedInLockout)
            } else if self
                .performances
                .numbers_near(place, ZONE_RADIUS_M)
                .next()
                .is_some()
            {
                Some(PerformanceError::CreateFailedTooCloseToPerformance)
            } else {
                None
            }
        };
        let header = music_header(number);
        let answer = match refusal {
            Some(error) => {
                tracing::debug!(player = number, ?error, "performance refused");
                MusicServerMsg::PerformanceFailed {
                    header,
                    error: error as u8,
                    radius: ZONE_RADIUS_M,
                    zone_type: ZONE_TYPE,
                }
            }
            None => {
                let wire_id = self.spawn_performance(entity, number, place, now_ms);
                MusicServerMsg::PerformanceCreated {
                    header,
                    performance: wire_id,
                    created: true,
                }
            }
        };
        if let Some(player) = self.world.get::<Player>(entity) {
            send_now(
                &mut self.combat_messages,
                &player.connection,
                &[ServerMessage::Music(answer)],
            );
        }
    }

    /// Spawns the performance of performer `number` (`entity`) at `place`; gives its wire id.
    fn spawn_performance(
        &mut self,
        entity: Entity,
        number: u64,
        place: Position,
        now_ms: u64,
    ) -> u64 {
        let content = Arc::clone(&self.content);
        let (rank, _) = self.tradeskill_perks(entity, MUSIC_TRADESKILL);
        let pages = self.unlocked_pages(entity, &content.music.page_achievements());
        let serial = self.performances.next;
        self.performances.next += 1;
        let namespace = Uuid::from_u64_pair(PERFORMANCE_ID_NAMESPACE, self.me.generation);
        // A dynamic spawn: retail gives dynamic entities an even first id byte (GDE id note).
        let id = nw_messages::as_dynamic_raw_id(
            *Uuid::new_v5(&namespace, &serial.to_be_bytes()).as_bytes(),
        );
        let wire_id = facet_of_raw_id(&id);
        let seed = u64::from(self.dice.below(SEED_LIMIT));
        let mut performance = Performance {
            id,
            wire_id,
            position: place,
            performer: number,
            performer_entity: facet_entity(number),
            stage: PerformanceStage::WaitingToJoin,
            next: Next::Stage {
                stage: PerformanceStage::PreShow,
                at_ms: now_ms.saturating_add(PRE_SHOW_AFTER_CREATE_MS),
            },
            song: None,
            sheet_column: None,
            reward: None,
            start_ns: None,
            error_sync: 0,
            notes: BTreeMap::new(),
            score: (0, 0),
            seed,
            zone_players: 0,
            pages,
            rank,
            list_version: nw_messages::items::FIRST_VERSION,
            pending: MusicalPerformanceState::default(),
            published: MusicalPerformanceState::default(),
            version: 0,
        };
        performance.sync();
        self.performances
            .by_number
            .insert(PERFORMANCE_ENTITY_BASE + serial, performance);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.music.change.performing = Some(PERFORMING);
        }
        tracing::debug!(player = number, wire_id, "performance created");
        wire_id
    }

    /// The achievement bits of the song page achievements `pages` that player `entity` has,
    /// ascending.
    fn unlocked_pages(&self, entity: Entity, pages: &[&str]) -> Vec<u16> {
        let Some(player) = self.world.get::<Player>(entity) else {
            return Vec::new();
        };
        let mut bits: Vec<u16> = pages
            .iter()
            .filter(|page| has_achievement(&self.achievement_rules, player, page))
            .filter_map(|page| u16::try_from(self.achievement_rules.bit(page)?).ok())
            .collect();
        bits.sort_unstable();
        bits
    }

    /// Player `number` (`entity`) leaves its performance at `now_ms`: the performance ends and
    /// its lockout starts. A leave without a performance changes nothing (retail sends two).
    fn leave_performance(&mut self, entity: Entity, number: u64, now_ms: u64) {
        let server_ms = self.server_ms;
        let Some((_, performance)) = self.performances.of_performer(number) else {
            return;
        };
        let version = performance.next_list_version();
        performance.pending.performers = Some(ReplicatedList::Changes(vec![ListChange {
            index: PERFORMER_PLACE as u32,
            version,
            value: NO_PERFORMANCE,
        }]));
        performance.sheet_column = None;
        performance.pending.sheet_indices = Some(0);
        performance.stage = PerformanceStage::Ending;
        performance.pending.stage = Some(PerformanceStage::Ending as u8);
        performance.next = Next::Gone {
            at_ms: now_ms.saturating_add(GONE_AFTER_ENDING_MS),
        };
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            let music = &mut player.music;
            music.performing = None;
            music.lockout_until_ms = Some(server_ms.saturating_add(LOCKOUT_MS));
            music.change.performance = Some(NO_PERFORMANCE);
            music.change.lockout = Some(LOCKOUT);
            music.change.performing = Some(NOT_PERFORMING);
        }
        tracing::debug!(player = number, "performance left");
    }

    /// Runs the performances at `now_ms`: their timed stages, their zones, the ends of the
    /// lockouts, and publishes their changes for this tick's bundles.
    pub(super) fn tick_music(&mut self, now_ms: u64) {
        let numbers: Vec<u64> = self.performances.by_number.keys().copied().collect();
        for number in numbers {
            self.performance_due(number, now_ms);
        }
        self.music_zones();
        let server_ms = self.server_ms;
        let entities: Vec<Entity> = self.by_connection.values().copied().collect();
        for entity in entities {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.music.lockout_due(server_ms);
            }
        }
        for performance in self.performances.by_number.values_mut() {
            if !performance.pending.is_empty() {
                performance.published = std::mem::take(&mut performance.pending);
                performance.version = performance.version.wrapping_add(1);
            }
        }
    }

    /// The timed stage of performance `number` that is due at `now_ms`; a performance whose
    /// performer left the sim ends.
    fn performance_due(&mut self, number: u64, now_ms: u64) {
        let Some((performer, next, stage)) = self
            .performances
            .by_number
            .get(&number)
            .map(|performance| (performance.performer, performance.next, performance.stage))
        else {
            return;
        };
        let performer_entity = self.player_by_number(performer);
        if performer_entity.is_none() && stage != PerformanceStage::Ending {
            let Some(performance) = self.performances.by_number.get_mut(&number) else {
                return;
            };
            performance.stage = PerformanceStage::Ending;
            performance.pending.stage = Some(PerformanceStage::Ending as u8);
            performance.next = Next::Gone {
                at_ms: now_ms.saturating_add(GONE_AFTER_ENDING_MS),
            };
            return;
        }
        match next {
            Next::Wait => {}
            Next::Gone { at_ms } if now_ms >= at_ms => {
                self.performances.by_number.remove(&number);
            }
            Next::Stage { stage, at_ms } if now_ms >= at_ms => {
                let seed = u64::from(self.dice.below(SEED_LIMIT));
                let Some(performance) = self.performances.by_number.get_mut(&number) else {
                    return;
                };
                performance.next = Next::Wait;
                match stage {
                    PerformanceStage::PreShow => {
                        let first = performance.stage == PerformanceStage::WaitingToJoin;
                        let wire_id = performance.wire_id;
                        pre_show(performance, seed);
                        if first
                            && let Some(mut player) = performer_entity
                                .and_then(|entity| self.world.get_mut::<Player>(entity))
                        {
                            player.music.performing = Some(wire_id);
                            player.music.change.performance = Some(wire_id);
                        }
                    }
                    PerformanceStage::Results => self.results(number, now_ms),
                    PerformanceStage::PostShow => {
                        performance.enter(PerformanceStage::PostShow);
                        performance.next = Next::Stage {
                            stage: PerformanceStage::Done,
                            at_ms: now_ms.saturating_add(DONE_AFTER_POST_SHOW_MS),
                        };
                        if let Some(entity) = performer_entity {
                            let (rank, _) = self.tradeskill_perks(entity, MUSIC_TRADESKILL);
                            if let Some(performance) = self.performances.by_number.get_mut(&number)
                            {
                                performance.rank = rank;
                                performance.pending.rank = Some(rank);
                            }
                        }
                    }
                    PerformanceStage::Done => {
                        performance.enter(PerformanceStage::Done);
                        performance.next = Next::Stage {
                            stage: PerformanceStage::PreShow,
                            at_ms: now_ms.saturating_add(PRE_SHOW_AFTER_DONE_MS),
                        };
                    }
                    other => performance.enter(other),
                }
            }
            Next::Gone { .. } | Next::Stage { .. } => {}
        }
    }

    /// The song of performance `number` ends at `now_ms`: the notes are scored (each note
    /// [`nw_content::music::NotePoints`] `perfect` when every key of it had a key, else
    /// `missed`; the chart's timing that tells `Great` from `Perfect` is Unknown), the results
    /// show, and the performer gets its rewards.
    fn results(&mut self, number: u64, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let Some(performance) = self.performances.by_number.get_mut(&number) else {
            return;
        };
        let points = content.music.points;
        let notes = u32::try_from(performance.notes.len()).unwrap_or(u32::MAX);
        let hits = u32::try_from(performance.notes.values().filter(|hit| **hit).count())
            .unwrap_or(u32::MAX);
        performance.score = (
            notes.saturating_mul(points.perfect),
            hits.saturating_mul(points.perfect)
                .saturating_add((notes - hits).saturating_mul(points.missed)),
        );
        performance.enter(PerformanceStage::Results);
        performance.score_change();
        let score = performance.performance_score();
        performance.pending.performance_score = Some(score);
        performance.next = Next::Stage {
            stage: PerformanceStage::PostShow,
            at_ms: now_ms.saturating_add(POST_SHOW_AFTER_RESULTS_MS),
        };
        let performer = performance.performer;
        let song = performance
            .song
            .and_then(|song| content.music.song(song))
            .cloned();
        let reward = performance
            .reward
            .and_then(|reward| content.music.reward(reward))
            .cloned();
        let Some(song) = song else {
            return;
        };
        self.song_rewards(performer, &song, reward.as_ref(), f64::from(score), now_ms);
    }

    /// Performer `number` finished `song` with performance score `score` and picked `reward`:
    /// the `Musician` experience of the song's completion event times the grade's
    /// `MusicalScoreMult` and the remote config's `Musician` multiplier (Inferred from the
    /// columns' names: retail's gains, 408 to 1,518 for easy songs, do not fit it; the formula is
    /// Unknown), and the completion, grade and reward events fire for its quests.
    fn song_rewards(
        &mut self,
        number: u64,
        song: &Song,
        reward: Option<&nw_content::music::PerformanceReward>,
        score: f64,
        now_ms: u64,
    ) {
        let content = Arc::clone(&self.content);
        let grade = content.music.grade(score);
        let multiplier = grade.map_or(1.0, |(_, grade)| grade.score_multiplier);
        if let Some((id, base)) = content
            .event_grants
            .get(&song.completion_event)
            .and_then(|grant| grant.progression.clone())
            && let Some(mastery) = content.progression.tradeskill(&id)
            && let Some(mut player) = self
                .player_by_number(number)
                .and_then(|entity| self.world.get_mut::<Player>(entity))
        {
            let gain = progression_gain(
                base,
                ONCE,
                content.gameplay.xp.progression(&id) * multiplier,
            );
            let experience = player.masteries.experience(mastery).saturating_add(gain);
            player.masteries.set(mastery, experience);
            tracing::debug!(
                player = number,
                song = %song.id,
                grade = grade.map_or("", |(_, grade)| grade.name.as_str()),
                gain,
                "song finished"
            );
        }
        let mut events = vec![song.completion_event.clone()];
        if let Some((index, grade)) = grade {
            events.extend(grade.event.clone());
            events.extend(
                reward
                    .and_then(|reward| reward.event(index))
                    .map(str::to_owned),
            );
        }
        for event in events {
            self.quest_event(number, Event::GameEvent(&event), now_ms);
        }
        if let Some(at) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Body>(entity))
            .map(|body| body.motion.position())
        {
            self.quest_event(number, Event::SongPerformed { song: &song.id, at }, now_ms);
        }
    }

    /// Every player within a performance's zone watches it; the performances count them (a
    /// new player in a zone moves its `errorSync`, Observed).
    fn music_zones(&mut self) {
        let zones: Vec<(u64, u64, Position)> = self
            .performances
            .by_number
            .iter()
            .filter(|(_, performance)| performance.stage != PerformanceStage::Ending)
            .map(|(number, performance)| (*number, performance.wire_id, performance.position))
            .collect();
        let mut counts: HashMap<u64, u16> = HashMap::new();
        let mut query = self.world.query::<(&mut Player, &Body)>();
        for (mut player, body) in query.iter_mut(&mut self.world) {
            let place = body.motion.position();
            let watched = zones
                .iter()
                .filter(|(_, _, position)| position.horizontal_distance(place) <= ZONE_RADIUS_M)
                .min_by(|(_, _, near), (_, _, far)| {
                    near.horizontal_distance(place)
                        .total_cmp(&far.horizontal_distance(place))
                });
            if let Some((number, _, _)) = watched {
                *counts.entry(*number).or_default() += 1;
            }
            player
                .music
                .observe(watched.map(|(_, wire_id, _)| *wire_id));
        }
        for (number, _, _) in zones {
            let count = counts.get(&number).copied().unwrap_or_default();
            if let Some(performance) = self.performances.by_number.get_mut(&number)
                && performance.zone_players != count
            {
                if count > performance.zone_players {
                    performance.pending.error_sync = Some(performance.sync());
                }
                performance.zone_players = count;
                performance.pending.zone_players = Some(count);
            }
        }
    }

    /// Whether the attack key of the client of `connection` plays music: it performs.
    pub fn music_holds_attack(&self, connection: &ConnectionRef) -> bool {
        self.by_connection
            .get(connection)
            .and_then(|entity| self.world.get::<Player>(*entity))
            .is_some_and(|player| player.music.performing())
    }
}

/// The whole list of `elements` at `version`.
fn full_list<T>(version: u64, elements: Vec<T>) -> Option<ReplicatedList<T>> {
    Some(ReplicatedList::Full(FullList {
        version: Some(version),
        elements,
    }))
}

/// `performance` goes back to its pre show: a new seed `seed`, its notes and score cleared, its
/// pages listed again.
fn pre_show(performance: &mut Performance, seed: u64) {
    performance.notes.clear();
    performance.score = (0, 0);
    performance.start_ns = None;
    performance.seed = seed;
    performance.enter(PerformanceStage::PreShow);
    performance.score_change();
    performance.pending.performance_score = Some(0.0);
    let version = performance.next_list_version();
    performance.pending.pages = full_list(version, performance.pages.clone());
    performance.pending.seeds[PERFORMER_PLACE] = Some(seed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERFORMER: u64 = 7;
    const OTHER: u64 = 8;
    const PLACE: Position = Position {
        x: 100.0,
        y: 200.0,
        z: 40.0,
    };

    fn performance() -> Performance {
        Performance {
            id: [2; RAW_ID_LEN],
            wire_id: 0x0202_0202_0202_0202,
            position: PLACE,
            performer: PERFORMER,
            performer_entity: 0x0102_0304_0506_0708,
            stage: PerformanceStage::WaitingToJoin,
            next: Next::Wait,
            song: None,
            sheet_column: None,
            reward: None,
            start_ns: None,
            error_sync: 1,
            notes: BTreeMap::new(),
            score: (0, 0),
            seed: 0x6898,
            zone_players: 0,
            pages: vec![4514, 4661],
            rank: 219,
            list_version: nw_messages::items::FIRST_VERSION,
            pending: MusicalPerformanceState::default(),
            published: MusicalPerformanceState::default(),
            version: 0,
        }
    }

    fn performances() -> Performances {
        Performances {
            by_number: BTreeMap::from([(PERFORMANCE_ENTITY_BASE, performance())]),
            next: 1,
        }
    }

    fn state_of(group: &BundleGroup) -> &MusicalPerformanceState {
        group
            .members
            .iter()
            .find_map(|member| match &member.state {
                MemberState::MusicalPerformance(state) => Some(state),
                _ => None,
            })
            .expect("a performance state")
    }

    #[test]
    fn the_spawn_group_holds_the_retail_card_and_the_slots_of_the_viewers_role() {
        let performances = performances();
        let new = Slot {
            interest: 148,
            new: true,
        };
        let mut seen = HashMap::new();
        let group = performances
            .group(PERFORMANCE_ENTITY_BASE, new, PERFORMER, PLACE, &mut seen)
            .expect("group");
        assert_eq!(group.interest, 148);
        assert_eq!(
            group
                .members
                .iter()
                .map(|member| member.component)
                .collect::<Vec<_>>(),
            [CARD_COMPONENT, PERFORMANCE_COMPONENT, POSITION_COMPONENT]
        );
        let MemberState::GdeMetadata(card) = &group.members[0].state else {
            panic!("card");
        };
        assert_eq!(card.asset, Some(PERFORMANCE_ASSET));
        let state = state_of(&group);
        assert_eq!(state.rank, Some(219), "the performer gets slot 2");
        assert_eq!(state.seeds[0], Some(0x6898));
        assert_eq!(state.radius, Some(ZONE_RADIUS_M));
        assert_eq!(state.stage, Some(PerformanceStage::WaitingToJoin as u8));
        let far = Position {
            x: PLACE.x + ZONE_RADIUS_M + 1.0,
            ..PLACE
        };
        let outside = performances
            .group(
                PERFORMANCE_ENTITY_BASE,
                new,
                OTHER,
                far,
                &mut HashMap::new(),
            )
            .expect("group");
        let state = state_of(&outside);
        assert_eq!(state.error_sync, None, "slot 0 only");
        assert_eq!(state.rank, None);
        assert!(state.performers.is_some());
        let zone = performances
            .group(
                PERFORMANCE_ENTITY_BASE,
                new,
                OTHER,
                PLACE,
                &mut HashMap::new(),
            )
            .expect("group");
        assert_eq!(state_of(&zone).error_sync, Some(1), "slot 1 in the zone");
        assert_eq!(state_of(&zone).rank, None);
    }

    #[test]
    fn a_viewer_gets_each_change_once_and_the_whole_state_after_a_gap() {
        let mut performances = performances();
        let kept = Slot {
            interest: 148,
            new: false,
        };
        let mut seen = HashMap::from([(
            PERFORMANCE_ENTITY_BASE,
            Seen {
                version: 0,
                role: Role::Performer,
            },
        )]);
        assert!(
            performances
                .group(PERFORMANCE_ENTITY_BASE, kept, PERFORMER, PLACE, &mut seen)
                .is_none()
        );
        let performance = performances
            .by_number
            .get_mut(&PERFORMANCE_ENTITY_BASE)
            .expect("performance");
        performance.enter(PerformanceStage::PreLoad);
        performance.published = std::mem::take(&mut performance.pending);
        performance.version = 1;
        let change = performances
            .group(PERFORMANCE_ENTITY_BASE, kept, PERFORMER, PLACE, &mut seen)
            .expect("the change");
        assert_eq!(
            *state_of(&change),
            MusicalPerformanceState {
                stage: Some(PerformanceStage::PreLoad as u8),
                error_sync: Some(2),
                ..MusicalPerformanceState::default()
            },
            "the slots and children of the retail PreLoad change (33715: 03 40 04 02 ...)"
        );
        assert!(
            performances
                .group(PERFORMANCE_ENTITY_BASE, kept, PERFORMER, PLACE, &mut seen)
                .is_none(),
            "once"
        );
        performances
            .by_number
            .get_mut(&PERFORMANCE_ENTITY_BASE)
            .expect("performance")
            .version = 3;
        let whole = performances
            .group(PERFORMANCE_ENTITY_BASE, kept, PERFORMER, PLACE, &mut seen)
            .expect("whole");
        assert!(
            state_of(&whole).performers.is_some(),
            "a missed change sends the whole"
        );
    }

    #[test]
    fn a_pre_show_resets_the_score_and_lists_the_pages_again() {
        let mut performance = performance();
        performance.score = (630, 624);
        performance.notes.insert(1, true);
        pre_show(&mut performance, 0x0dec);
        assert_eq!(performance.stage, PerformanceStage::PreShow);
        assert!(performance.notes.is_empty());
        assert_eq!(performance.pending.seeds[0], Some(0x0dec));
        assert_eq!(performance.pending.performance_score, Some(0.0));
        assert!(matches!(
            &performance.pending.pages,
            Some(ReplicatedList::Full(list)) if list.elements == [4514, 4661]
        ));
        assert_eq!(performance.pending.error_sync, Some(2));
    }

    #[test]
    fn the_score_packs_the_maximum_high_and_the_earned_points_low() {
        let mut performance = performance();
        performance.score = (630, 624);
        assert_eq!(performance.packed_score(), 0x0276_0270, "retail 91415");
        assert!((performance.performance_score() - 0.990_476).abs() < 1e-6);
        performance.score = (0, 0);
        assert!(performance.performance_score().abs() < f32::EPSILON);
    }

    #[test]
    fn the_own_state_carries_the_lockout_and_the_performance() {
        let mut music = PlayerMusicState::of(&PlayerMusic {
            next_performance_ms: Some(5_000),
        });
        let first = music.take_own_member(true, 1_000).expect("first");
        assert_eq!(
            first.state,
            MemberState::PerformancePlayer(PerformancePlayerState {
                lockout: Some(LOCKOUT),
                ..PerformancePlayerState::NONE
            })
        );
        assert_eq!(first.component, PERFORMANCE_PLAYER_COMPONENT);
        assert!(music.take_own_member(false, 1_000).is_none());
        music.lockout_due(5_000);
        music.observe(Some(9));
        assert_eq!(
            music
                .take_own_member(false, 5_000)
                .map(|member| member.state),
            Some(MemberState::PerformancePlayer(PerformancePlayerState {
                lockout: Some(0),
                observing: Some(9),
                ..PerformancePlayerState::default()
            }))
        );
        assert_eq!(music.carried(), PlayerMusic::default());
        let fresh = PlayerMusicState::default()
            .take_own_member(true, 0)
            .expect("first");
        assert_eq!(
            fresh.state,
            MemberState::PerformancePlayer(PerformancePlayerState::NONE),
            "the retail new character's first body"
        );
    }
}
