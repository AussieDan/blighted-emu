// SPDX-License-Identifier: AGPL-3.0-or-later
//! Expeditions, raids and soul trials in the sim (run 15 instances study, REPORT sections 2 and
//! 4): the arena calls of the client (5039, 5037, 946, 926), the dungeon interactions (71
//! `OpenDungeonMenu`, 72 `EnterDungeon`, 74 `ExitDungeon`), the player's arena state (type 2406),
//! and the instance an instance-role sim runs.
//!
//! Retail order (17 soul trials, 12 expedition and raid ledgers):
//!
//! 1. 5039 (or interaction 71 at an entrance) → 5038 with the game mode's CRC and 1.
//! 2. 5037 (or interaction 72) → the director gives the group an instance sim → 6776
//!    `Success` and the arena state `isQueuedForDungeon` (a soul trial also `m_singlePlayerInstance
//!    State` 3, its time and `m_gameModeIdx`).
//! 3. 946 → the arena state `isInArena`, the cooldown and the counters → 3951 loading
//!    screen → the empty phasing → the handoff to the instance sim, which sends the instance
//!    level (1635 with the instance phase) and the whole arena state.
//! 4. 926 or interaction 74 → 3951 → the empty phasing → the handoff back to the open-world
//!    sim of the spot the player entered from, which sends the open world's 1635 with the grid
//!    cell of that spot.

use bevy_ecs::entity::Entity;
use std::collections::HashMap;

use nw_bus::{
    ConnectionRef, HandoffOffer, InstanceBeat, InstanceOpened, InstanceRefusal, InstanceReply,
    InstanceRequest, InstanceStay, PlayerArena, Point, RunningInstance, SimSlot, StoredPoint,
};
use nw_content::instances::{EntryCounter, InstanceCatalog, InstanceKind, InstanceMode, NO_INDEX};
use nw_content::loot_limits::ResetCycle;
use nw_messages::{
    ActivateResponse, ArenaClientMsg, ArenaServerMsg, FacetHeader, FullList, InteractionRequest,
    LevelInfoChangedMsg, LevelSpec, PLAYER_ARENA_FACET, PlayerArenaState, ServerMessage,
    items::FIRST_VERSION,
};
use nw_sim_core::Position;
use uuid::Uuid;

use super::mutations::{InstanceMutation, difficulty_of_request};
use super::{
    Body, HandoffStart, PHASING_BEFORE_HANDOFF_MS, Player, SimWorld, Vitals, gather_answer,
    handoff_offer, owner_in, quests_of, send_now, unix_ns,
};
use crate::login::phase_id;
use crate::player_content::ENTRANCE_PREFIX;

/// Interaction kinds of the client's `InteractionUIActions` (`sheathe/interact_enum.tsv`).
pub mod kind {
    /// `OpenDungeonMenu`: the menu of an expedition entrance.
    pub const OPEN_DUNGEON_MENU: u8 = 71;
    /// `EnterDungeon`: activate the entrance's expedition.
    pub const ENTER_DUNGEON: u8 = 72;
    /// `ExitDungeon`: leave the instance at its exit.
    pub use nw_messages::client_input::EXIT_DUNGEON_KIND as EXIT_DUNGEON;
}

/// The rank of a mutated slot a player has not raised (best guess: difficulty 1 is open).
const FIRST_MUTATION_RANK: u8 = 1;
/// The 3743 status of an accepted dungeon interaction (as for the travel interactions).
const ACCEPTED: u8 = 1;
/// The shown character level less the zero-based level of the experience table.
const SHOWN_LEVEL_OFFSET: u32 = 1;
/// Nanoseconds in a millisecond.
const NS_PER_MS: u64 = 1_000_000;
/// Nanoseconds in a second.
const NS_PER_SECOND: u64 = 1_000_000_000;
/// `m_singlePlayerInstanceState` after a trial ended (retail: 0 in the state after every exit).
const SINGLE_PLAYER_ENDED: u8 = 0;
/// The infix of a locked entrance (`DungeonEntrance_QuestAdiana_Locked_V2`): the same game mode.
const LOCKED_INFIX: &str = "_locked";
/// The version suffix of an entrance (`DungeonEntrance_QuestMammoth_V2`).
const VERSION_SUFFIX: &str = "_v2";
/// The `Dungeon` prefix of an expedition's game mode id (`DungeonAmrine` for `..._Amrine`).
const DUNGEON_PREFIX: &str = "dungeon";
/// The level folder part of an expedition map (`NW_Dungeon_Everfall_00` for `..._Everfall00`).
const MAP_PREFIXES: [&str; 2] = ["nw_dungeon_", "nw_"];

/// An instance the director gave the player: it enters it at its 946.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reservation {
    instance: Uuid,
    sim: SimSlot,
    phase: u64,
    crc: u32,
    /// Unix nanoseconds of the activation (`m_singlePlayerDungeonTime`).
    activated_ns: u64,
    /// The mutation difficulty asked for; `None` for a normal run.
    mutation_difficulty: Option<u8>,
}

/// A move of the player into or out of an instance.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Transfer {
    /// The sim that takes the player; `None` when no sim owns the return spot yet.
    target: Option<SimSlot>,
    /// Where the player appears.
    position: Position,
    /// When the empty phasing went out; the handoff waits [`PHASING_BEFORE_HANDOFF_MS`].
    phasing_left_ms: Option<u64>,
}

/// A player's expedition, raid and trial state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ArenaPlayer {
    /// What a save, a join and a handoff keep.
    record: PlayerArena,
    /// What its client has; `None` before the first state.
    shown: Option<PlayerArenaState>,
    /// The game mode its last 5039 or menu named (CRC).
    selected: Option<u32>,
    /// The mutation difficulty its last activation asked for; `None` for a normal run.
    difficulty: Option<u8>,
    /// A request waits for the director.
    waiting: bool,
    /// The instance it may enter.
    reservation: Option<Reservation>,
    /// The move into or out of an instance that waits for its handoff.
    transfer: Option<Transfer>,
    /// `isInArena` and `isQueuedForDungeon` were set in this session: the whole states send them.
    touched: bool,
    /// The player leaves its instance (926 or interaction 74): it is no longer in the arena.
    exiting: bool,
}

impl ArenaPlayer {
    /// The CRC of the expedition it waits for or may enter; `None` in no expedition queue.
    pub(super) fn queued_game_mode(&self) -> Option<u32> {
        match &self.reservation {
            Some(reservation) => Some(reservation.crc),
            None if self.waiting => self.selected,
            None => None,
        }
    }

    /// Its rank (the highest mutation difficulty open to it) of slot `slot` of the week's
    /// mutated expeditions (`dungeonRanks`); `None` when it has none.
    pub(super) fn rank(&self, slot: usize) -> Option<u8> {
        self.record.ranks.get(slot).copied()
    }

    /// Raises its rank of slot `slot` to `rank` (a smaller rank stays); the slots before it get
    /// rank [`FIRST_MUTATION_RANK`].
    pub(super) fn raise_rank(&mut self, slot: usize, rank: u8) {
        let ranks = &mut self.record.ranks;
        if ranks.len() <= slot {
            ranks.resize(slot + 1, FIRST_MUTATION_RANK);
        }
        if let Some(current) = ranks.get_mut(slot) {
            *current = (*current).max(rank);
        }
        self.touched = true;
    }

    /// The state of a player who arrived with `record`.
    pub(super) fn of(record: PlayerArena) -> Self {
        Self {
            record,
            ..Self::default()
        }
    }

    /// What a save, a join and a handoff keep.
    pub(super) fn carried(&self) -> PlayerArena {
        self.record.clone()
    }

    /// The state its client should have; `mode` is the game mode of its reservation or stay.
    fn state(&self, mode: Option<&InstanceMode>) -> PlayerArenaState {
        let record = &self.record;
        let non_zero = |value: u64| (value != 0).then_some(value);
        let counter = |value: u32| (value != 0).then_some(value);
        let solo = mode.is_some_and(|mode| mode.kind == InstanceKind::SoloTrial);
        // Retail: `isInArena` from the 946 on, cleared at the 926 before the level change.
        let in_arena = record.stay.is_some() && !self.exiting;
        let queued = self.reservation.is_some() && record.stay.is_none();
        let trial_time = self
            .reservation
            .as_ref()
            .map(|reservation| reservation.activated_ns);
        let single_player_state = if !solo {
            None
        } else if record.stay.is_some() {
            Some(nw_messages::arena::SINGLE_PLAYER_ENTERED)
        } else if queued {
            Some(nw_messages::arena::SINGLE_PLAYER_ACTIVATED)
        } else {
            None
        };
        PlayerArenaState {
            in_arena: (self.touched || in_arena).then_some(in_arena),
            queued: (self.touched || queued).then_some(queued),
            dungeon_cooldown_since_2000_ns: non_zero(record.dungeon_cooldown_since_2000_ns),
            solo_trial_cooldown_since_2000_ns: non_zero(record.solo_trial_cooldown_since_2000_ns),
            dungeon_ranks: (!record.ranks.is_empty()).then(|| FullList {
                version: Some(FIRST_VERSION),
                elements: record.ranks.clone(),
            }),
            last_dungeon: record.last_dungeon,
            last_mutated_dungeon: record.last_mutated_dungeon,
            base_entered: counter(record.base_entered),
            mutated_entered: counter(record.mutated_entered),
            group_trials_entered: counter(record.group_trials_entered),
            next_base_refresh_ns: non_zero(record.next_base_refresh_ns),
            next_mutated_refresh_ns: non_zero(record.next_mutated_refresh_ns),
            next_group_trial_refresh_ns: non_zero(record.next_group_trial_refresh_ns),
            mutation_award: record.mutation_award.then_some(true),
            single_player_state,
            single_player_time_ns: single_player_state
                .and(trial_time.or(record.stay.as_ref().map(|stay| stay.activated_ns))),
            game_mode_index: single_player_state.and(mode.map(|mode| mode.game_mode_index)),
        }
    }

    /// The change its client should get now: the children that moved since the last state, or
    /// the whole state the first time (the children with a value, as in retail's first states);
    /// `None` when nothing moved.
    fn change(&mut self, mode: Option<&InstanceMode>) -> Option<PlayerArenaState> {
        let now = self.state(mode);
        let change = match &self.shown {
            // A child that went from set to unset (the trial ended) goes with its end value.
            Some(shown) => cleared_children(shown, &now, now.changes_from(shown)),
            None => now.clone(),
        };
        let first = self.shown.is_none();
        self.shown = Some(now);
        (first || !change.is_empty()).then_some(change)
    }
}

/// `change` with the children that `shown` had and `now` has not, as their end values: the
/// trial state 0, no game mode index and no cooldown (retail's state after a trial's exit).
fn cleared_children(
    shown: &PlayerArenaState,
    now: &PlayerArenaState,
    mut change: PlayerArenaState,
) -> PlayerArenaState {
    if shown.single_player_state.is_some() && now.single_player_state.is_none() {
        change.single_player_state = Some(SINGLE_PLAYER_ENDED);
        change.game_mode_index = Some(NO_INDEX);
    }
    if shown.dungeon_cooldown_since_2000_ns.is_some()
        && now.dungeon_cooldown_since_2000_ns.is_none()
    {
        change.dungeon_cooldown_since_2000_ns = Some(0);
    }
    change
}

/// The instance an instance-role sim runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InstanceRun {
    instance: Uuid,
    game_mode: String,
    phase: u64,
    members: Vec<Uuid>,
    /// Unix milliseconds when it ends, when its game mode limits it.
    ends_at_ms: Option<u64>,
    /// Sim milliseconds when it opened.
    opened_ms: u64,
    /// Sim milliseconds since its last player left; `None` while one is in it or none came.
    empty_since_ms: Option<u64>,
    /// Whether a player came.
    visited: bool,
    /// The director ended it: its players go back, and the sim is free once they left.
    closing: bool,
    /// Its mutation, from the first player that arrived with one.
    mutation: Option<InstanceMutation>,
}

impl InstanceRun {
    /// Its mutation.
    pub(super) const fn mutation(&self) -> Option<&InstanceMutation> {
        self.mutation.as_ref()
    }

    /// Its mutation, to change.
    pub(super) const fn mutation_mut(&mut self) -> Option<&mut InstanceMutation> {
        self.mutation.as_mut()
    }

    /// Sets its mutation.
    pub(super) fn set_mutation(&mut self, mutation: InstanceMutation) {
        self.mutation = Some(mutation);
    }
}

impl InstanceRun {
    /// The instance.
    pub(super) const fn id(&self) -> Uuid {
        self.instance
    }

    /// Its game mode id.
    pub(super) fn game_mode(&self) -> &str {
        &self.game_mode
    }
}

/// The game mode whose entrance slice is `slice_name`: the name after `DungeonEntrance`,
/// without `_Locked`, `_V2` and a trailing hash number, is the game mode id, the id less its
/// `Dungeon` prefix, or the map name less `NW_Dungeon_` or `NW_` (Inferred from the 40 entrance
/// slices of the open world: 35 name one game mode this way, e.g. the QuestMorgaine entrance
/// that retail answered with QuestMorgaine's 5038; the others name none or two and give none).
pub(super) fn entrance_mode<'a>(
    modes: impl Iterator<Item = &'a InstanceMode>,
    slice_name: &str,
) -> Option<&'a InstanceMode> {
    // The last path part, without an extension (`.dynamicslice`).
    let name = slice_name
        .rsplit('/')
        .next()?
        .split('.')
        .next()?
        .to_lowercase();
    let rest = name
        .strip_prefix(ENTRANCE_PREFIX)?
        .replace(LOCKED_INFIX, "")
        .replace(VERSION_SUFFIX, "");
    // A trailing number of more digits than a map number is a hash (`_1970794647`).
    let rest = match rest.rsplit_once('_') {
        Some((head, tail))
            if tail.len() > MAP_NUMBER_DIGITS && tail.chars().all(|c| c.is_ascii_digit()) =>
        {
            head.to_owned()
        }
        _ => rest,
    };
    let key = letters_and_digits(&rest);
    let mut found = modes.filter(|mode| {
        let id = mode.id.to_lowercase();
        let map = mode.map.map_name.to_lowercase();
        let map = MAP_PREFIXES
            .iter()
            .find_map(|prefix| map.strip_prefix(prefix))
            .unwrap_or(&map);
        [
            letters_and_digits(&id),
            letters_and_digits(id.strip_prefix(DUNGEON_PREFIX).unwrap_or(&id)),
            letters_and_digits(map),
        ]
        .contains(&key)
    });
    let mode = found.next()?;
    found.next().is_none().then_some(mode)
}

/// Digits of a map number at the end of an entrance name (`brimstone00`).
const MAP_NUMBER_DIGITS: usize = 2;

fn letters_and_digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// The level of each game mode of `catalog`, by its CRC (each is built once: its strings live
/// for the rest of the process).
pub(crate) fn instance_levels(catalog: &InstanceCatalog) -> HashMap<u32, LevelSpec> {
    catalog
        .modes()
        .map(|mode| {
            let map = &mode.map;
            (
                mode.crc,
                LevelSpec::instance(&map.folder, &map.map_name, &map.coatlicue_path, map.frame),
            )
        })
        .collect()
}

/// The 6776 response of a refusal of the director (Inferred: the closest
/// `eDungeonActivateResponse`).
const fn response_of(refusal: InstanceRefusal) -> ActivateResponse {
    match refusal {
        InstanceRefusal::NotEnoughMembers => ActivateResponse::NotEnoughMembers,
        InstanceRefusal::TooManyMembers => ActivateResponse::TooManyMembers,
        InstanceRefusal::GroupInOtherInstance => ActivateResponse::GroupAlreadyInDungeon,
        InstanceRefusal::NoInstanceSim => ActivateResponse::EntranceBusy,
        InstanceRefusal::GroupTimeout => ActivateResponse::GroupTimeout,
        InstanceRefusal::OpenFailed => ActivateResponse::EntranceActivationFailure,
    }
}

impl SimWorld {
    /// The header of a call to `number`'s `PlayerArenaComponent`.
    fn arena_header(number: u64) -> FacetHeader {
        FacetHeader::of(PLAYER_ARENA_FACET, number)
    }

    /// The level a player arrives in: the level of the running instance's game mode (its own
    /// frame) on an instance sim, else the sim's level.
    pub(super) fn arrival_level(&self) -> LevelSpec {
        self.instance_run
            .as_ref()
            .and_then(|run| self.content.instances.by_id(&run.game_mode))
            .and_then(|mode| self.instance_levels.get(&mode.crc))
            .copied()
            .unwrap_or(self.settings.level)
    }

    /// The level change of the player of `offer`, which enters this sim's level with the
    /// client-context id `client_context` at `server_time`: into the running instance (its phase),
    /// back to the open world from an instance (the grid cell of the return spot, as retail), or
    /// from the tutorial.
    pub(super) fn entry_level_info(
        &self,
        offer: &HandoffOffer,
        client_context: u8,
        server_time: u64,
    ) -> Result<LevelInfoChangedMsg, getrandom::Error> {
        let level = self.arrival_level();
        if !level.private && offer.progression.arena.stay.is_some() {
            let point = offer.position;
            return Ok(LevelInfoChangedMsg::open_world_at(
                client_context,
                server_time,
                phase_id(level)?,
                [point.x, point.y],
            ));
        }
        Ok(LevelInfoChangedMsg::of_level(
            level,
            client_context,
            server_time,
            self.entry_phase(level)?,
        ))
    }

    /// The phase of a player entering this sim's level: the instance's on an instance sim, else
    /// [`phase_id`].
    pub(super) fn entry_phase(&self, level: LevelSpec) -> Result<u64, getrandom::Error> {
        match &self.instance_run {
            Some(run) => Ok(run.phase),
            None => phase_id(level),
        }
    }

    /// Whether `character` may be in this sim: every character outside an instance sim, and the
    /// members of the instance an instance sim runs.
    pub(super) fn may_enter(&self, character: Uuid) -> bool {
        if !self.instance_sim {
            return true;
        }
        self.instance_run
            .as_ref()
            .is_some_and(|run| run.members.contains(&character))
    }

    /// An arena call of the client of `connection` at `now_ms`.
    pub fn arena_call(&mut self, connection: &ConnectionRef, call: ArenaClientMsg, now_ms: u64) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        match call {
            ArenaClientMsg::RequestRemoteInteract { game_mode, .. } => {
                self.offer_game_mode(entity, game_mode, true);
            }
            ArenaClientMsg::RequestRemoteEnterDungeon { flag, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity)
                    && !player.arena.waiting
                    && player.arena.reservation.is_none()
                {
                    player.arena.difficulty = difficulty_of_request(flag);
                }
                self.activate(entity, true);
            }
            ArenaClientMsg::TeleportIntoDungeon { .. } => self.teleport_in(entity, now_ms),
            ArenaClientMsg::ForfeitArena { .. } => self.exit_instance(entity),
        }
    }

    /// The dungeon interactions; `false` for another kind.
    pub(super) fn instance_interaction(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) -> bool {
        match request.kind {
            // Retail: the 5038 first, the 3743 in the same millisecond.
            kind::OPEN_DUNGEON_MENU => {
                if let Some(crc) = self.entrance_at(request.target_entity) {
                    self.offer_game_mode(entity, crc, false);
                }
                let accepted = gather_answer(number, request, ACCEPTED, name);
                self.send_to(entity, &[accepted]);
            }
            kind::ENTER_DUNGEON => {
                let accepted = gather_answer(number, request, ACCEPTED, name);
                self.send_to(entity, &[accepted]);
                if let Some(crc) = self.entrance_at(request.target_entity) {
                    if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                        player.arena.selected = Some(crc);
                        player.arena.difficulty = None;
                    }
                    self.activate(entity, false);
                }
            }
            // Retail: the loading screen (3951), then the 3743.
            kind::EXIT_DUNGEON => {
                self.exit_instance(entity);
                let accepted = gather_answer(number, request, ACCEPTED, name);
                self.send_to(entity, &[accepted]);
            }
            _ => return false,
        }
        true
    }

    /// The game mode of the expedition entrance whose capital is the facet entity `entity`.
    fn entrance_at(&self, entity: u64) -> Option<u32> {
        let slice = self.content.entrances.get(&entity)?;
        entrance_mode(self.content.instances.modes(), slice).map(|mode| mode.crc)
    }

    /// Whether player `entity` may enter `mode` (from afar when `remote`): the game mode allows
    /// it, the player has the level and the achievement it needs and an item of a class it
    /// needs.
    fn may_activate(&self, entity: Entity, mode: &InstanceMode, remote: bool) -> bool {
        let Some(player) = self.world.get::<Player>(entity) else {
            return false;
        };
        if (remote && !mode.remote_entry) || mode.generated {
            return false;
        }
        // `RequiredLevel` is the character level the client shows: one above the zero-based
        // level of the experience table.
        let level = self
            .content
            .progression
            .level_of(player.experience)
            .saturating_add(SHOWN_LEVEL_OFFSET);
        if mode.required_level.is_some_and(|required| level < required) {
            return false;
        }
        if let Some(achievement) = &mode.entry_achievement
            && !player
                .flow
                .has_achievement(&self.achievement_rules, achievement)
        {
            return false;
        }
        mode.required_item_classes.is_empty()
            || player.gear.as_ref().is_some_and(|gear| {
                gear.carried().iter().any(|item| {
                    self.content
                        .instances
                        .item_has_class(&item.item_id, &mode.required_item_classes)
                })
            })
    }

    /// Answers a 5039 or a dungeon menu: may the player use `crc`'s game mode.
    fn offer_game_mode(&mut self, entity: Entity, crc: u32, remote: bool) {
        let content = std::sync::Arc::clone(&self.content);
        let allowed = content
            .instances
            .by_crc(crc)
            .is_some_and(|mode| self.may_activate(entity, mode, remote));
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if allowed {
            player.arena.selected = Some(crc);
        }
        let answer = ServerMessage::Arena(ArenaServerMsg::RemoteInteractResult {
            header: Self::arena_header(player.number),
            game_mode: crc,
            allowed,
        });
        let connection = player.connection.clone();
        send_now(&mut self.combat_messages, &connection, &[answer]);
    }

    /// Asks the director for the instance of the selected game mode (5037 or interaction 72).
    fn activate(&mut self, entity: Entity, remote: bool) {
        let content = std::sync::Arc::clone(&self.content);
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        // The 5037 that comes with the 946 finds the instance given.
        if player.arena.waiting || player.arena.reservation.is_some() || self.instance_sim {
            return;
        }
        let Some(mode) = player
            .arena
            .selected
            .and_then(|crc| content.instances.by_crc(crc))
        else {
            return;
        };
        let (number, character, connection) =
            (player.number, player.character, player.connection.clone());
        if !self.may_activate(entity, mode, remote) {
            self.answer_activation(entity, number, ActivateResponse::EntranceActivationFailure);
            return;
        }
        let shown_level = content
            .progression
            .level_of(player.experience)
            .saturating_add(SHOWN_LEVEL_OFFSET);
        if let Some(refusal) = player
            .arena
            .difficulty
            .and_then(|difficulty| self.mutation_refusal(player, mode, difficulty, shown_level))
        {
            self.answer_activation(entity, number, refusal);
            return;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.arena.waiting = true;
        }
        self.instance_requests.push((
            connection,
            InstanceRequest {
                game_mode: mode.id.clone(),
                level: mode.map.folder.clone(),
                requester: character,
                capacity: mode.capacity,
                min_group: mode.min_group,
                timeout_seconds: mode.timeout_seconds,
            },
        ));
    }

    fn answer_activation(&mut self, entity: Entity, number: u64, response: ActivateResponse) {
        let answer = ServerMessage::Arena(ArenaServerMsg::ActivateResult {
            header: Self::arena_header(number),
            response,
        });
        self.send_to(entity, &[answer]);
    }

    /// The requests for the director since the last call.
    pub fn take_instance_requests(&mut self) -> Vec<(ConnectionRef, InstanceRequest)> {
        std::mem::take(&mut self.instance_requests)
    }

    /// The director's answer to the request of `connection`'s player; `Err` when it did not
    /// answer.
    pub fn instance_reply(
        &mut self,
        connection: &ConnectionRef,
        reply: Result<InstanceReply, String>,
    ) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let activated_ns = unix_ns(self.server_ms);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player.arena.waiting = false;
        let number = player.number;
        let response = match reply {
            Ok(InstanceReply::Granted {
                instance,
                sim,
                phase,
                ..
            }) => {
                let Some(crc) = player.arena.selected else {
                    return;
                };
                let mutation_difficulty = player.arena.difficulty;
                player.arena.reservation = Some(Reservation {
                    instance,
                    sim,
                    phase,
                    crc,
                    activated_ns,
                    mutation_difficulty,
                });
                player.arena.touched = true;
                ActivateResponse::Success
            }
            Ok(InstanceReply::Refused { reason }) => response_of(reason),
            Err(error) => {
                tracing::warn!(player = number, %error, "no instance answer");
                ActivateResponse::EntranceActivationFailure
            }
        };
        self.answer_activation(entity, number, response);
        self.push_arena_change(entity);
    }

    /// 946: the player goes into its reserved instance.
    fn teleport_in(&mut self, entity: Entity, now_ms: u64) {
        let content = std::sync::Arc::clone(&self.content);
        let since_2000_ns = self.server_ms.saturating_mul(NS_PER_MS);
        let now_ns = unix_ns(self.server_ms);
        let clock = content.loot_limits.as_ref().map(|rules| rules.clock);
        let level = self.settings.level.folder.to_owned();
        let Some(position) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(reservation) = player.arena.reservation.clone() else {
            return;
        };
        if player.arena.transfer.is_some() {
            return;
        }
        let Some(mode) = content.instances.by_crc(reservation.crc) else {
            return;
        };
        let record = &mut player.arena.record;
        if let Some(clock) = clock {
            refresh(record, clock, now_ns);
        }
        let mutated = reservation.mutation_difficulty.is_some();
        record.last_dungeon_mutated = mutated;
        if mutated {
            record.mutated_entered = record.mutated_entered.saturating_add(1);
            record.last_mutated_dungeon = Some(*reservation.instance.as_bytes());
        }
        match mode.counter {
            _ if mutated => {}
            EntryCounter::BaseDungeon => {
                record.base_entered = record.base_entered.saturating_add(1);
            }
            EntryCounter::GroupTrial => {
                record.group_trials_entered = record.group_trials_entered.saturating_add(1);
            }
            EntryCounter::Uncounted => {}
        }
        let cooldown_ns = mode.cooldown_seconds().saturating_mul(NS_PER_SECOND);
        record.dungeon_cooldown_since_2000_ns = since_2000_ns.saturating_add(cooldown_ns);
        if mode.kind == InstanceKind::SoloTrial {
            record.solo_trial_cooldown_since_2000_ns = since_2000_ns;
        }
        record.last_dungeon = Some(*reservation.instance.as_bytes());
        record.stay = Some(InstanceStay {
            instance: reservation.instance,
            game_mode: mode.id.clone(),
            phase: reservation.phase,
            return_level: level,
            return_position: StoredPoint::of(Point::from(position)),
            activated_ns: reservation.activated_ns,
            mutation_difficulty: reservation.mutation_difficulty,
        });
        let [x, y, z] = mode
            .map
            .teleports
            .first()
            .map(|teleport| teleport.position)
            .unwrap_or_default();
        player.arena.transfer = Some(Transfer {
            target: Some(reservation.sim.clone()),
            position: Position { x, y, z },
            phasing_left_ms: None,
        });
        player.arena.touched = true;
        let loading = player.flow.leave();
        let connection = player.connection.clone();
        send_now(&mut self.combat_messages, &connection, &loading);
        tracing::info!(
            player = player.number,
            instance = %reservation.instance,
            game_mode = %mode.id,
            now_ms,
            "player enters an instance"
        );
        self.push_arena_change(entity);
    }

    /// 926, 3309 or interaction 74: a player in this instance sim goes back to where it entered
    /// from.
    pub(super) fn exit_instance(&mut self, entity: Entity) {
        if !self.instance_sim {
            return;
        }
        let table = self.applied_table.clone();
        let Some(player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if player.arena.transfer.is_some() {
            return;
        }
        let Some(stay) = player.arena.record.stay.clone() else {
            return;
        };
        // The game mode takes back its items (`ItemClassesToRemoveOnExit`).
        self.remove_instance_items(entity);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let position = Position::from(stay.return_position.point());
        let target = table.as_ref().and_then(|table| owner_in(table, position));
        player.arena.transfer = Some(Transfer {
            target,
            position,
            phasing_left_ms: None,
        });
        player.arena.exiting = true;
        let loading = player.flow.leave();
        let connection = player.connection.clone();
        send_now(&mut self.combat_messages, &connection, &loading);
        self.push_arena_change(entity);
    }

    /// The handoffs of the players whose transfer waited long enough after its empty phasing
    /// (as the tutorial exit: retail sends the empty phasing first, the level change after it).
    pub(super) fn instance_transfers(&mut self, now_ms: u64) -> Vec<HandoffStart> {
        let table = self.applied_table.clone();
        let quests = &self.quests;
        let mut query = self.world.query::<(&mut Player, &Body, &Vitals)>();
        let mut starts = Vec::new();
        for (mut player, body, vitals) in query.iter_mut(&mut self.world) {
            if player.frozen || now_ms < player.no_handoff_before_ms || vitals.health.is_dead() {
                continue;
            }
            let Some(transfer) = player.arena.transfer.clone() else {
                continue;
            };
            let left_ms = match transfer.phasing_left_ms {
                None => {
                    let left = player.flow.left();
                    let connection = player.connection.clone();
                    send_now(&mut self.combat_messages, &connection, &[left]);
                    if let Some(transfer) = player.arena.transfer.as_mut() {
                        transfer.phasing_left_ms = Some(now_ms);
                    }
                    continue;
                }
                Some(left_ms) => left_ms,
            };
            if now_ms < left_ms.saturating_add(PHASING_BEFORE_HANDOFF_MS) {
                continue;
            }
            // The open-world sim of the return spot, once a table names one.
            let target = transfer.target.clone().or_else(|| {
                table
                    .as_ref()
                    .and_then(|table| owner_in(table, transfer.position))
            });
            let Some(target) = target else {
                continue;
            };
            player.frozen = true;
            let carried = quests_of(quests, now_ms, &player);
            starts.push(HandoffStart {
                target,
                offer: handoff_offer(
                    &player,
                    body,
                    vitals,
                    now_ms,
                    Some(transfer.position),
                    carried,
                ),
            });
        }
        starts
    }

    /// Sets the arena state of player `number` that just arrived (a join or a handoff) at
    /// `now_ms`: a new character's refresh times, the end of a stay in an instance that is not
    /// here, the instance it is in; then its whole state goes out.
    pub(super) fn arena_arrived(&mut self, number: u64) {
        let content = std::sync::Arc::clone(&self.content);
        let now_ns = unix_ns(self.server_ms);
        let clock = content.loot_limits.as_ref().map(|rules| rules.clock);
        let here = self.instance_run.as_ref().map(|run| run.instance);
        let instance_sim = self.instance_sim;
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let arena = &mut player.arena;
        if let Some(clock) = clock {
            refresh(&mut arena.record, clock, now_ns);
        }
        let stayed = arena.record.stay.as_ref().map(|stay| stay.instance);
        match (instance_sim, stayed) {
            // Back from an instance (or it ended): no stay, and a trial's state and cooldown end
            // (retail: both cleared in the first state after every soul trial's exit).
            (false, Some(_)) => {
                let solo = arena.record.stay.as_ref().is_some_and(|stay| {
                    content
                        .instances
                        .by_id(&stay.game_mode)
                        .is_some_and(|mode| mode.kind == InstanceKind::SoloTrial)
                });
                if solo {
                    arena.record.dungeon_cooldown_since_2000_ns = 0;
                }
                arena.record.stay = None;
                arena.reservation = None;
                arena.touched = true;
            }
            (true, Some(stayed)) if Some(stayed) == here => arena.touched = true,
            _ => {}
        }
        arena.transfer = None;
        arena.exiting = false;
        let stay = arena
            .record
            .stay
            .clone()
            .filter(|stay| instance_sim && Some(stay.instance) == here);
        if let Some(run) = self.instance_run.as_mut() {
            run.visited = true;
            run.empty_since_ms = None;
        }
        if let Some(stay) = stay {
            self.adopt_mutation(&stay, number);
        }
        self.push_arena_change(entity);
    }

    /// Puts the change of `entity`'s arena state in its next bundle.
    pub(super) fn push_arena_change(&mut self, entity: Entity) {
        let content = std::sync::Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let mode = match (&player.arena.reservation, &player.arena.record.stay) {
            (_, Some(stay)) => content.instances.by_id(&stay.game_mode),
            (Some(reservation), None) => content.instances.by_crc(reservation.crc),
            (None, None) => None,
        };
        let Some(change) = player.arena.change(mode) else {
            return;
        };
        match change.member() {
            Ok(member) => player.extra_members.push(member),
            Err(error) => tracing::warn!(player = player.number, %error, "arena state not sent"),
        }
    }

    /// Runs the instance `instance` of `game_mode` (the director's opening).
    pub fn open_instance(
        &mut self,
        instance: Uuid,
        game_mode: &str,
        phase: u64,
        members: Vec<Uuid>,
        ends_at_ms: Option<u64>,
        now_ms: u64,
    ) -> InstanceOpened {
        if !self.instance_sim {
            return InstanceOpened::Refused {
                reason: "this sim has no instance role".to_owned(),
            };
        }
        if let Some(run) = &self.instance_run
            && run.instance != instance
        {
            return InstanceOpened::Refused {
                reason: format!("this sim runs instance {}", run.instance),
            };
        }
        let Some(mode) = self.content.instances.by_id(game_mode) else {
            return InstanceOpened::Refused {
                reason: format!("unknown game mode {game_mode}"),
            };
        };
        if mode.map.folder != self.settings.level.folder {
            return InstanceOpened::Refused {
                reason: format!("game mode {game_mode} is not on this level"),
            };
        }
        // A fresh instance: every mob of the level spawns again.
        self.mob_changes.add(self.mobs.set_cells(|_| false));
        self.mob_changes.add(self.mobs.set_cells(|_| true));
        self.instance_run = Some(InstanceRun {
            instance,
            game_mode: game_mode.to_owned(),
            phase,
            members,
            ends_at_ms,
            opened_ms: now_ms,
            empty_since_ms: None,
            visited: false,
            closing: false,
            mutation: None,
        });
        tracing::info!(%instance, game_mode, "instance opened");
        InstanceOpened::Opened
    }

    /// `character` joined the group after the opening and may enter.
    pub fn add_instance_member(&mut self, instance: Uuid, character: Uuid) {
        if let Some(run) = self.instance_run.as_mut()
            && run.instance == instance
            && !run.members.contains(&character)
        {
            run.members.push(character);
        }
    }

    /// Ends the instance `instance`: every player in it goes back; the sim is free once the
    /// last one left.
    pub fn close_instance(&mut self, instance: Uuid) {
        if self
            .instance_run
            .as_ref()
            .is_none_or(|run| run.instance != instance)
        {
            return;
        }
        let players: Vec<Entity> = self.by_connection.values().copied().collect();
        for entity in &players {
            self.exit_instance(*entity);
        }
        if players.is_empty() {
            tracing::info!(%instance, "instance closed");
            self.instance_run = None;
        } else if let Some(run) = self.instance_run.as_mut() {
            // No one may come in again; the last exit frees the sim.
            run.members.clear();
            run.closing = true;
        }
    }

    /// The instance part of the heartbeat at sim time `now_ms`; `None` without the instance role.
    pub fn instance_beat(&mut self, now_ms: u64) -> Option<InstanceBeat> {
        if !self.instance_sim {
            return None;
        }
        // Its game mode's time is over: it ends here too, with or without the director.
        let unix_ms = unix_ns(self.server_ms) / NS_PER_MS;
        let ended = self
            .instance_run
            .as_ref()
            .filter(|run| !run.closing && run.ends_at_ms.is_some_and(|end| unix_ms >= end))
            .map(|run| run.instance);
        if let Some(instance) = ended {
            self.close_instance(instance);
        }
        // Mobs that spawned or respawned since the last beat get the instance's mutation.
        self.mutate_live_mobs();
        let players = u32::try_from(self.by_connection.len()).unwrap_or(u32::MAX);
        // A free instance sim still reports its role.
        let Some(run) = self.instance_run.as_mut() else {
            return Some(InstanceBeat { running: None });
        };
        if players == 0 && run.visited && run.empty_since_ms.is_none() {
            run.empty_since_ms = Some(now_ms);
        }
        // Closed and empty: free again.
        if players == 0 && run.closing {
            tracing::info!(instance = %run.instance, "instance closed");
            self.instance_run = None;
            return Some(InstanceBeat { running: None });
        }
        let capacity = self
            .content
            .instances
            .by_id(&run.game_mode)
            .map_or(0, |mode| mode.capacity);
        Some(InstanceBeat {
            running: Some(RunningInstance {
                instance: run.instance,
                game_mode: run.game_mode.clone(),
                members: run.members.clone(),
                players,
                empty_for_ms: run.empty_since_ms.map(|since| now_ms.saturating_sub(since)),
                open_for_ms: now_ms.saturating_sub(run.opened_ms),
                phase: run.phase,
                capacity,
                ends_at_ms: run.ends_at_ms,
            }),
        })
    }

    /// Removes the items of player `entity` whose class the game mode of its stay takes back at
    /// the exit (`ItemClassesToRemoveOnExit`, `DungeonItem` for every expedition).
    pub(super) fn remove_instance_items(&mut self, entity: Entity) {
        let content = std::sync::Arc::clone(&self.content);
        let time_ns = unix_ns(self.server_ms);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(mode) = player
            .arena
            .record
            .stay
            .as_ref()
            .and_then(|stay| content.instances.by_id(&stay.game_mode))
        else {
            return;
        };
        let Some(gear) = player.gear.as_mut() else {
            return;
        };
        let removed: Vec<String> = gear
            .carried()
            .into_iter()
            .map(|item| item.item_id)
            .filter(|item| {
                content
                    .instances
                    .item_has_class(item, &mode.items_removed_on_exit)
            })
            .collect();
        for item in removed {
            gear.remove(&item, time_ns);
        }
    }
}

/// Starts the counters of `record` again at their refresh times, at Unix nanosecond `now_ns`:
/// the base dungeon and group trial counts each day, the mutated count each week, at the
/// server's resets (retail refresh times: daily ones on every weekday at the reset hour, the
/// mutated one on the weekly reset day; a new character gets the three times at once).
fn refresh(record: &mut PlayerArena, clock: nw_content::loot_limits::ResetClock, now_ns: u64) {
    if now_ns >= record.next_base_refresh_ns {
        if record.next_base_refresh_ns != 0 {
            record.base_entered = 0;
        }
        record.next_base_refresh_ns = clock.end_ns(ResetCycle::Daily, now_ns);
    }
    if now_ns >= record.next_mutated_refresh_ns {
        if record.next_mutated_refresh_ns != 0 {
            record.mutated_entered = 0;
        }
        record.next_mutated_refresh_ns = clock.end_ns(ResetCycle::Weekly, now_ns);
    }
    if now_ns >= record.next_group_trial_refresh_ns {
        if record.next_group_trial_refresh_ns != 0 {
            record.group_trials_entered = 0;
        }
        record.next_group_trial_refresh_ns = clock.end_ns(ResetCycle::Daily, now_ns);
    }
}

#[cfg(test)]
mod tests {
    use nw_content::instances::{InstanceMap, Teleport};
    use nw_content::loot_limits::{ResetClock, Weekday};

    use super::*;

    /// The CRC of every test mode: the state and the entrances do not read it.
    const TEST_CRC: u32 = 7;

    fn mode(id: &str, map_name: &str, kind: InstanceKind) -> InstanceMode {
        InstanceMode {
            id: id.to_owned(),
            crc: TEST_CRC,
            kind,
            map: InstanceMap {
                folder: map_name.to_lowercase(),
                map_name: map_name.to_owned(),
                coatlicue_path: format!("coatlicue/{map_name}"),
                frame: [0.0, 0.0, 1.0, 1.0],
                teleports: vec![Teleport {
                    position: [1.0, 2.0, 3.0],
                    yaw_deg: 0.0,
                }],
            },
            capacity: 5,
            min_group: 3,
            required_level: None,
            remote_entry: true,
            respawns: true,
            mutable: false,
            timeout_seconds: None,
            victory_delay_seconds: None,
            items_removed_on_exit: Vec::new(),
            required_item_classes: Vec::new(),
            entry_achievement: None,
            loot_tags: Vec::new(),
            container_level: None,
            counter: EntryCounter::BaseDungeon,
            game_mode_index: NO_INDEX,
            generated: false,
        }
    }

    fn modes() -> Vec<InstanceMode> {
        vec![
            mode(
                "DungeonAmrine",
                "NW_Dungeon_Windsward_00",
                InstanceKind::Expedition,
            ),
            mode(
                "DungeonShatteredObelisk",
                "NW_Dungeon_Everfall_00",
                InstanceKind::Expedition,
            ),
            mode(
                "QuestMorgaine",
                "NewWorld_VitaeEterna",
                InstanceKind::SoloTrial,
            ),
            mode(
                "TrialSeason04Q02",
                "NW_Trial_Season_04_DaichiDojo",
                InstanceKind::SoloTrial,
            ),
            mode(
                "TrialSeason01",
                "NW_Trial_Season_02",
                InstanceKind::RaidTrial,
            ),
            mode(
                "TrialSeason04",
                "NW_Trial_Season_04",
                InstanceKind::RaidTrial,
            ),
            mode(
                "TrialSeason04Q05",
                "NW_Trial_Season_04",
                InstanceKind::SoloTrial,
            ),
        ]
    }

    fn found(slice: &str) -> Option<String> {
        let modes = modes();
        entrance_mode(modes.iter(), slice).map(|mode| mode.id.clone())
    }

    #[test]
    fn entrance_slices_name_their_game_modes() {
        assert_eq!(
            found("Dungeon/Entrance/dungeonentrance_questmorgaine").as_deref(),
            Some("QuestMorgaine")
        );
        assert_eq!(
            found("dungeon/entrance/dungeonentrance_amrine").as_deref(),
            Some("DungeonAmrine")
        );
        assert_eq!(
            found("Dungeon/Entrance/DungeonEntrance_Everfall00").as_deref(),
            Some("DungeonShatteredObelisk"),
            "by the map name"
        );
        assert_eq!(
            found("Dungeon/Entrance/DungeonEntrance_TrialSeason04Q02_Locked_V2_1970794647")
                .as_deref(),
            Some("TrialSeason04Q02")
        );
        assert_eq!(
            found("Dungeon/Entrance/DungeonEntranceTrial_Season_01").as_deref(),
            Some("TrialSeason01")
        );
        // Two modes share the map NW_Trial_Season_04: no game mode.
        assert_eq!(
            found("Dungeon/Entrance/DungeonEntranceTrial_Season_04"),
            None
        );
        assert_eq!(found("Dungeon/Entrance/DungeonEntrance_FirstLight02"), None);
        assert_eq!(found("POIs/Settlement/Townboard"), None);
    }

    fn clock() -> ResetClock {
        ResetClock {
            hour_utc: 3,
            weekly_day: Weekday::Tuesday,
        }
    }

    /// Thursday 2026-10-22 12:00:00.25 UTC, Unix nanoseconds.
    const NOW_NS: u64 = 1_792_670_400_250_000_000;
    /// Friday 2026-10-23 03:00 UTC, Unix seconds.
    const NEXT_DAILY_S: u64 = 1_792_724_400;
    /// Tuesday 2026-10-27 03:00 UTC, Unix seconds.
    const NEXT_WEEKLY_S: u64 = 1_793_070_000;

    #[test]
    fn a_new_record_gets_its_refresh_times_and_an_old_one_starts_again() {
        let mut record = PlayerArena::default();
        refresh(&mut record, clock(), NOW_NS);
        let sub_second = NOW_NS % NS_PER_SECOND;
        assert_eq!(
            record.next_base_refresh_ns,
            NEXT_DAILY_S * NS_PER_SECOND + sub_second
        );
        assert_eq!(
            record.next_group_trial_refresh_ns,
            record.next_base_refresh_ns
        );
        assert_eq!(
            record.next_mutated_refresh_ns,
            NEXT_WEEKLY_S * NS_PER_SECOND + sub_second
        );
        record.base_entered = 2;
        record.group_trials_entered = 1;
        refresh(&mut record, clock(), NOW_NS + NS_PER_SECOND);
        assert_eq!(record.base_entered, 2, "before the refresh");
        refresh(
            &mut record,
            clock(),
            NEXT_DAILY_S * NS_PER_SECOND + sub_second,
        );
        assert_eq!((record.base_entered, record.group_trials_entered), (0, 0));
    }

    #[test]
    fn the_state_follows_the_activation_the_teleport_and_the_exit() {
        let trial = mode(
            "TrialSeason04Q06",
            "NW_Trial_Season_04_DeviceRoom",
            InstanceKind::SoloTrial,
        );
        let trial = InstanceMode {
            game_mode_index: 48,
            ..trial
        };
        let mut arena = ArenaPlayer::default();
        refresh(&mut arena.record, clock(), NOW_NS);
        // The first state: the three refresh times only (retail newchar).
        let first = arena.change(None).expect("first");
        assert!(first.in_arena.is_none() && first.queued.is_none());
        assert!(first.next_base_refresh_ns.is_some());
        assert!(arena.change(None).is_none(), "nothing moved");
        // Activated: queued with the trial's state 3, time and index.
        arena.reservation = Some(Reservation {
            instance: Uuid::from_u128(7),
            sim: SimSlot {
                id: nw_bus::BusId::try_from("inst-1".to_owned()).expect("id"),
                generation: 1,
            },
            phase: 9,
            crc: trial.crc,
            activated_ns: NOW_NS,
            mutation_difficulty: None,
        });
        arena.touched = true;
        let queued = arena.change(Some(&trial)).expect("queued");
        assert_eq!(queued.queued, Some(true));
        assert_eq!(queued.in_arena, Some(false));
        assert_eq!(
            queued.single_player_state,
            Some(nw_messages::arena::SINGLE_PLAYER_ACTIVATED)
        );
        assert_eq!(queued.game_mode_index, Some(48));
        assert_eq!(queued.single_player_time_ns, Some(NOW_NS));
        // Inside: in arena, not queued, state 4.
        arena.record.stay = Some(InstanceStay {
            instance: Uuid::from_u128(7),
            game_mode: trial.id.clone(),
            phase: 9,
            return_level: "newworld_vitaeeterna".to_owned(),
            return_position: StoredPoint::default(),
            activated_ns: NOW_NS,
            mutation_difficulty: None,
        });
        arena.record.dungeon_cooldown_since_2000_ns = 5;
        let inside = arena.change(Some(&trial)).expect("inside");
        assert_eq!((inside.in_arena, inside.queued), (Some(true), Some(false)));
        assert_eq!(
            inside.single_player_state,
            Some(nw_messages::arena::SINGLE_PLAYER_ENTERED)
        );
        assert_eq!(inside.dungeon_cooldown_since_2000_ns, Some(5));
        // Back in the open world: not in arena, the trial ends with state 0 and no index, the
        // cooldown is cleared.
        arena.record.stay = None;
        arena.reservation = None;
        arena.record.dungeon_cooldown_since_2000_ns = 0;
        let out = arena.change(None).expect("out");
        assert_eq!(out.in_arena, Some(false));
        assert_eq!(out.single_player_state, Some(SINGLE_PLAYER_ENDED));
        assert_eq!(out.game_mode_index, Some(NO_INDEX));
        assert_eq!(out.dungeon_cooldown_since_2000_ns, Some(0));
    }

    #[test]
    fn refusals_answer_with_the_closest_response() {
        assert_eq!(
            response_of(InstanceRefusal::NotEnoughMembers),
            ActivateResponse::NotEnoughMembers
        );
        assert_eq!(
            response_of(InstanceRefusal::GroupInOtherInstance),
            ActivateResponse::GroupAlreadyInDungeon
        );
        assert_eq!(
            response_of(InstanceRefusal::NoInstanceSim),
            ActivateResponse::EntranceBusy
        );
    }

    /// The solo trial of the end-to-end test: one player, its level and teleport spot.
    fn trial() -> InstanceMode {
        InstanceMode {
            capacity: 1,
            min_group: 0,
            counter: EntryCounter::Uncounted,
            game_mode_index: TRIAL_INDEX,
            ..mode(
                "TrialSeason04Q06",
                "NW_Trial_Season_04_DeviceRoom",
                InstanceKind::SoloTrial,
            )
        }
    }

    /// The captured index of the trial.
    const TRIAL_INDEX: u8 = 48;
    /// The test instance's phase.
    const PHASE: u64 = 0x8b2c_bbb8_6aa9_8fd9;
    /// Where the player stands in the open world.
    const ENTRY_X: f32 = 100.0;

    fn content() -> std::sync::Arc<crate::player_content::PlayerContent> {
        std::sync::Arc::new(crate::player_content::PlayerContent {
            instances: InstanceCatalog::build([trial()]),
            ..crate::player_content::PlayerContent::default()
        })
    }

    fn arena_messages(world: &mut SimWorld, now: u64) -> Vec<ServerMessage> {
        let out = crate::world::tests::tick(world, now);
        crate::world::tests::messages_to(&out.messages, PLAYER)
    }

    const PLAYER: u64 = 41;

    #[test]
    fn a_player_goes_into_a_solo_trial_and_back_by_the_handoff_path() {
        trial_round_trip(|inside, who, header| {
            inside.arena_call(
                who,
                ArenaClientMsg::ForfeitArena { header, flag: 0 },
                crate::world::tests::NOW,
            );
        });
    }

    /// Retail `M1-TEMPEST-HEART` and `CATACOMBS2`: a 3309 leaves the instance as a 926 does (the
    /// loading screen, then the open world level).
    #[test]
    fn a_game_mode_leave_takes_the_player_out_of_its_instance() {
        trial_round_trip(|inside, who, header| {
            inside.game_mode_call(
                who,
                nw_messages::GameModeClientMsg::LeaveGameMode {
                    header,
                    first: [1; nw_messages::RAW_ID_LEN],
                    second: [2; nw_messages::RAW_ID_LEN],
                },
            );
        });
    }

    /// A player goes into a solo trial and back by the handoff path; `exit` makes it leave the
    /// instance.
    fn trial_round_trip(exit: impl Fn(&mut SimWorld, &ConnectionRef, FacetHeader)) {
        use crate::world::tests::{NOW, SERVER_TIME, connection, join, table, world_with_spawns};

        let header = SimWorld::arena_header(PLAYER);
        let mode = trial();
        // The open world.
        let mut open = world_with_spawns(Vec::new());
        open.content = content();
        join(&mut open, PLAYER, ENTRY_X);
        let who = connection(PLAYER);
        open.arena_call(
            &who,
            ArenaClientMsg::RequestRemoteInteract {
                header,
                game_mode: mode.crc,
            },
            NOW,
        );
        assert!(
            arena_messages(&mut open, NOW).contains(&ServerMessage::Arena(
                ArenaServerMsg::RemoteInteractResult {
                    header,
                    game_mode: mode.crc,
                    allowed: true,
                }
            ))
        );
        open.arena_call(
            &who,
            ArenaClientMsg::RequestRemoteEnterDungeon { header, flag: 0 },
            NOW,
        );
        let requests = open.take_instance_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.game_mode, mode.id);
        assert_eq!(requests[0].1.capacity, 1);
        let instance = Uuid::from_u128(7);
        let entity = open.player_by_number(PLAYER).expect("player");
        let character = open.world.get::<Player>(entity).expect("player").character;
        open.instance_reply(
            &who,
            Ok(InstanceReply::Granted {
                instance,
                sim: crate::world::tests::slot("inst-1"),
                phase: PHASE,
                members: vec![character],
                opened: true,
            }),
        );
        assert!(
            arena_messages(&mut open, NOW).contains(&ServerMessage::Arena(
                ArenaServerMsg::ActivateResult {
                    header,
                    response: ActivateResponse::Success,
                }
            ))
        );
        // The 946: the loading screen, then the empty phasing, then the handoff.
        open.arena_call(&who, ArenaClientMsg::TeleportIntoDungeon { header }, NOW);
        let first = crate::world::tests::tick(&mut open, NOW + 1);
        assert!(first.handoffs.is_empty(), "the empty phasing goes first");
        let later = crate::world::tests::tick(&mut open, NOW + 1 + PHASING_BEFORE_HANDOFF_MS);
        assert_eq!(later.handoffs.len(), 1);
        let start = later.handoffs[0].clone();
        assert_eq!(start.target, crate::world::tests::slot("inst-1"));
        assert!(start.offer.enter_level);
        assert_eq!(
            start.offer.position,
            Point {
                x: 1.0,
                y: 2.0,
                z: 3.0
            }
        );
        let stay = start.offer.progression.arena.stay.clone().expect("stay");
        assert_eq!((stay.instance, stay.phase), (instance, PHASE));
        assert_eq!(stay.return_position.point().x, ENTRY_X);
        assert_ne!(
            start
                .offer
                .progression
                .arena
                .solo_trial_cooldown_since_2000_ns,
            0
        );
        assert_eq!(
            open.handoff_done(&who, true, NOW + 2 + PHASING_BEFORE_HANDOFF_MS),
            super::super::HandoffEnd::Moved
        );
        // The instance sim.
        let map = &mode.map;
        let mut settings = crate::world::tests::settings(MANY_PLAYERS, MANY_PLAYERS as u32);
        settings.instance_role = true;
        settings.level =
            LevelSpec::instance(&map.folder, &map.map_name, &map.coatlicue_path, map.frame);
        let mut inside = SimWorld::new(
            settings,
            crate::login::WorldIds::random(0).expect("ids"),
            std::sync::Arc::new(nw_sim_core::test_support::NoGround),
            crate::world::tests::slot("inst-1"),
            crate::combat::tests::test_combat(),
            content(),
        );
        assert_eq!(
            inside.open_instance(instance, &mode.id, PHASE, vec![character], None, NOW),
            InstanceOpened::Opened
        );
        let level_change = inside
            .take(&start.offer, NOW, SERVER_TIME)
            .expect("take")
            .expect("level change");
        let level_info = level_change
            .envelopes
            .iter()
            .map(|bytes| nw_reflect::Envelope::decode(bytes).expect("envelope"))
            .find(|envelope| {
                envelope.type_index() == Some(nw_messages::LEVEL_INFO_CHANGED_MSG_TYPE_INDEX)
            })
            .expect("level info");
        let body = &level_info.body;
        assert_eq!(&body[1..1 + map.map_name.len()], map.map_name.as_bytes());
        assert!(
            body.windows(8).any(|window| window == PHASE.to_be_bytes()),
            "the instance phase"
        );
        let beat = inside.instance_beat(NOW).expect("instance role");
        let running = beat.running.expect("running");
        assert_eq!((running.instance, running.players), (instance, 1));
        // The exit: back to the open-world sim of the return spot.
        inside.set_table(&table(&["sim-a"])).expect("table");
        exit(&mut inside, &who, header);
        // After the taker's retry wait of the handoff that brought the player in.
        let exit_ms = NOW + AFTER_RETRY_MS;
        assert!(
            crate::world::tests::tick(&mut inside, exit_ms)
                .handoffs
                .is_empty()
        );
        let back = crate::world::tests::tick(&mut inside, exit_ms + PHASING_BEFORE_HANDOFF_MS);
        assert_eq!(back.handoffs.len(), 1);
        let home = back.handoffs[0].clone();
        assert_eq!(home.target, crate::world::tests::slot("sim-a"));
        assert_eq!(home.offer.position.x, ENTRY_X);
        // The open world takes it back with its grid cell and no stay.
        let returned = open
            .take(&home.offer, exit_ms + AFTER_RETRY_MS, SERVER_TIME)
            .expect("take back")
            .expect("level change");
        let level_info = returned
            .envelopes
            .iter()
            .map(|bytes| nw_reflect::Envelope::decode(bytes).expect("envelope"))
            .find_map(|envelope| match ServerMessage::from_envelope(&envelope) {
                Some(Ok(ServerMessage::LevelInfoChanged(level_info))) => Some(level_info),
                _ => None,
            })
            .expect("open world level info");
        assert_eq!(level_info.phasing.len(), 1);
        let entity = open.player_by_number(PLAYER).expect("player");
        let player = open.world.get::<Player>(entity).expect("player");
        assert!(player.arena.carried().stay.is_none());
    }

    /// Players a test sim takes.
    const MANY_PLAYERS: usize = 10;
    /// Past the retry wait of a taken handoff in the test settings.
    const AFTER_RETRY_MS: u64 = 60_000;
}
