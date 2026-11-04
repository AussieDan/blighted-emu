// SPDX-License-Identifier: AGPL-3.0-or-later
//! The world state of `sim` (M4 spec sections 3.1, 7.3; M5 spec sections 3.3, 3.4, 7), in a
//! `bevy_ecs` world, with no I/O: the caller passes times in and sends what a tick returns.
//!
//! Every tick, each player gets replication bundles on channel 1, unreliable, as in the captures:
//! the player's own state first, then the nearest players, mobs and ghosts inside the interest
//! radius, at most [`MAX_BUNDLE_MEMBERS`] members' bytes per bundle. Other players go by priority
//! ([`replica`]): near or fighting ones every tick, far ones less often, each state with its full
//! position, so a lost datagram costs one state. How the retail service removes an entity from a
//! client is Unknown, so a player who leaves the radius is simply not in the next bundles.
//!
//! With a cell table, a tick also starts the handoff of each player that is past the margin into
//! another sim's cells (the player is frozen until the answer), and lists the players near each
//! other sim's cells as ghosts for that sim.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use bevy_ecs::change_detection::Mut;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use nw_bus::{
    Admit, BusId, CarriedItem, CellTable, ConnectionRef, Ghost, HandoffOffer, ItemPlace,
    MOB_STATES, MissionTake, MobGhost, PlayerGhost, PlayerInventory, PlayerProgression,
    PlayerQuests, Point, QuestRun, ScriptLogLine, SimSlot, SnapMob, SnapPlayer,
};
use nw_content::mutations::MutationCatalog;
use nw_content::recipe_cooldowns::{CooldownCount, RecipeCooldownCounts};
use nw_messages::client_input::{
    CRAFTING_STATION_KIND, GATHER_KIND, LOOT_KIND, LORE_KIND, REVIVE_PLAYER_KIND, TALK_KIND,
    TOWN_BOARD_KIND, TRIGGER_KIND, WATER_GATHER_KIND,
};
use nw_messages::entity_ids::IdHolder;
use nw_messages::items::{FIRST_VERSION, FullList};
use nw_messages::paperdoll::slot::{
    CHEST, FEET, HANDS, HEAD, LEGS, MAIN_HAND_OPTION_1, MAIN_HAND_OPTION_2, MAIN_HAND_OPTION_3,
    OFF_HAND_OPTION_1, OFF_HAND_OPTION_3, QUICK_SLOT_1,
};
use nw_messages::{
    ABILITY_COMPONENT, ATTRIBUTE_COMPONENT, Adopted, BundleGroup, BundleMember, ClientFacetMsg,
    ConsumableCooldown, CooldownChange, CooldownMap, CooldownsState, CurrencyState,
    DisableCollisionMsg, GatheringInterruptedMsg, Hands, HealthChange, InputRecord,
    InteractionEndedMsg, InteractionRequest, InteractionValidatedMsg, LOGIN_CLIENT_CONTEXT,
    LevelSpec, LookDir, LookupRequest, MeleeHitMsg, MemberState, MessageError, PlayerLook,
    PlayerSeenByAiMsg, PlayerStates, ProgressionState, SelectedAsTargetMsg, ServerMessage,
    ShownItem, TUTORIAL_LEVEL, TargetMsg, TeleportStep, TimeSynchMsg, UnselectedAsTargetMsg,
    VitalsData, WorldPosition, facet_entity, put_looking_through_loadout, put_member,
};
use nw_sim_core::{
    CellBounds, CellGrid, CellOwners, Dice, Footing, Ground, GroundCheck, Health, Interest,
    LevelGround, MobState, Motion, MotionState, MoveLimits, MoveRefusal, OPEN_WORLD_START,
    Position, Seen, SpatialGrid, TrackedBody, at_tutorial_exit,
};
use nw_world::collision_index::CollisionIndex;
use thiserror::Error;
use uuid::Uuid;

mod abilities;
mod admin;
mod asset_script;
mod attack;
mod camps;
mod charms;
mod combat_status;
mod conversation;
mod crafting;
mod dungeon_script;
pub mod economy;
mod encounter;
mod events;
mod faction;
mod fall_damage;
mod fishing;
mod game_mode;
mod gathering;
mod gear;
mod gm_chat;
mod gm_client;
mod hit_shape;
pub(crate) mod instances;
mod invites;
mod item_calls;
mod kill_rewards;
mod level_assets;
mod level_flow;
mod loot_bag;
mod members;
mod meta_achievements;
mod missions;
mod mob_shot;
mod mob_timeline;
mod mob_trail;
mod mount;
mod music;
mod mutations;
mod notifications;
mod points;
mod progression;
mod projectile;
mod quest_objects;
mod quest_tracker;
mod quick_course;
mod repair;
mod replica;
mod revive;
mod rolled_loot;
mod salvage;
mod scripts;
mod slayer;
mod slots;
pub mod social;
mod social_bus;
mod social_states;
mod spell_areas;
mod stamina;
mod statics;
mod status_effects;
mod storage_search;
pub(crate) mod store_purchase;
mod title_choice;
mod tradeskill;
pub mod transmog;
mod travel;
mod walked_floor;

use asset_script::Step;
use combat_status::CombatTimer;
use encounter::{BAG_PREFIX, Order, Shove, Tutorial};
use gathering::{GATHERABLE_ENTITY_BASE, Nodes, OpenGather};
use gear::{Gear, Stow, unix_ns};
use level_assets::{ASSET_ENTITY_BASE, AssetViews, asset_gde_ref, spell_gde_ref};
use level_flow::{AchievementRules, LevelFlow};
use members::{
    CARD_REFRESH_BUNDLES, EntityKind, MobCard, OwnCard, OwnClock, OwnPlaces, OwnTeleport, OwnTick,
    StateNumber, bundles, card_of, log_mob_group, mob_group, own_group, own_state_counter,
    remote_group,
};
use mob_timeline::{Animations, MobPose, MobTimelines, TreeEntry, TreePose, animations_of};
use quest_tracker::{JournalRecord, QuestTracker};
use replica::{RemoteCandidate, Replica, ReplicaSource};
use slots::Slots;
use stamina::Stamina;
use statics::{
    STATIC_ENTITY_BASE, StaticViews, bag_emptied, bag_loot, chest_gather_ended,
    chest_gather_started, facet_of, interactor_target,
};

pub use attack::{AttackKeys, Blow, INPUT_RECORD_MS, KeyAttack};
pub use gm_chat::{GmChat, answer_lines};

use attack::{PendingMobSwing, PlayerSwing, Vitals};

use crate::combat::{AttackOutcome, AttackRefusal, CombatSettings};
use crate::login::{
    SessionIds, WorldIds, level_change_messages, login_level_info, login_messages, time_answers,
};
use crate::mob_ai::SpawnAi;
use crate::mobs::{MobAttackIntent, MobChanges, Mobs};
use crate::objectives::{Effect, Event, Facts, ObjectiveRun, TaskProgress};
use crate::player_content::{
    MobLooks, OPEN_WORLD_OBJECTIVE, PlayerContent, QuestDef, QuestObjects, TUTORIAL_OBJECTIVE,
    TUTORIAL_SWORD,
};
use crate::stats::StatEvent;
use nw_content::creature_animations::CreatureAnimations;
use nw_content::ftue_entities::{self, FACET_LEN, FtueLevel};
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_content::progression::Progression;
use nw_messages::FRAG_TAGS_LEN;
use nw_messages::raw_id_of_entity;

/// The paperdoll slots whose items count toward the equip load: the armor and the weapons
/// (dodge study 2026-09-20: retail's new character's dodges fit the slow class of its armor and
/// weapons).
const EQUIP_LOAD_SLOTS: [u32; 10] = [
    HEAD,
    CHEST,
    HANDS,
    LEGS,
    FEET,
    MAIN_HAND_OPTION_1,
    MAIN_HAND_OPTION_2,
    MAIN_HAND_OPTION_3,
    OFF_HAND_OPTION_1,
    OFF_HAND_OPTION_3,
];

/// Carrier channel of the login messages and the time messages.
pub const MESSAGE_CHANNEL: u8 = 0;
/// Carrier channel of the replication bundles.
pub const REPLICATION_CHANNEL: u8 = 1;

/// First replication-bundle sequence of a fresh login. The real client drops a bundle whose
/// sequence is not greater than its `m_lastUnreliableReplicationSeq`, which is already 1 right
/// after `LevelInfoChanged`, so a login that started its sequence at 0 had its first bundles
/// rejected and never spawned. A fresh login starts above that mark; a handed-off player keeps
/// its own running sequence (see [`SimWorld::take`]).
const FIRST_LOGIN_BUNDLE_SEQUENCE: u64 = 2;

/// Wait after a fresh login before the one-time own spawn card goes out. The card is sent once
/// before the client spawns (see [`Neighbourhood`]); a card sent on the login tick reaches the
/// client before it has applied `LevelInfoChanged` (channel 0), so the client drops it as a
/// bundle "for a previous ClientContext", never loads the level, and never moves to spawn.
pub(crate) const OWN_CARD_DELAY_MS: u64 = 1_000;
/// Time between two sends of the own spawn card bundle while the client loads its level.
/// Retail resends its first bundle about every 350 ms until the client answers with its
/// first time sync request (Soul Survivor capture, 2026-09-27: bundle 1 at 0, 348 and
/// 697 ms, the stream from 797 ms). One send is not enough: the client drops every bundle
/// that reaches it while its level context is down (live 2026-09-22, the beach switch).
const OWN_CARD_RESEND_MS: u64 = 350;
/// The longest pause between two inputs that repeat one interaction request: after the client's
/// cancel, a longer one starts a new press of it (Observed: retail `Hive_Dye` repeats every 50 ms, then the second water
/// gather's requests came 0.5 s after the first's with the same interaction id; live clients
/// repeat every 30 to 40 ms; the value between them is Inferred).
const INTERACTION_REPEAT_GAP_MS: u64 = 250;

/// The last open-world interaction request of a player: the client repeats it in each input until
/// it ends the interaction, and retail answers each one once (retail 2026-09-18: one 3743 for a
/// talk the client held 20 s; ours answered all 667 inputs). One interaction id can carry two
/// kinds: an inn's open (70) and its check in (18) (retail `12a_msq_02_inntutorial`, 110.8 s and
/// 112.5 s). The same request is a new press only after the client's cancel of it and a pause
/// of more than [`INTERACTION_REPEAT_GAP_MS`] (retail `Hive_Dye`: a second water gather on the
/// same id came 0.5 s after the first's cancel; the client repeated for 100 ms after that cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LastInteraction {
    /// Its (kind, interaction id).
    asked: (u8, u64),
    /// When the client last asked, ms.
    at_ms: u64,
    /// Whether the client cancelled it since.
    cancelled: bool,
}

impl LastInteraction {
    /// Whether request `asked` at `now_ms` repeats this one.
    fn repeats(&self, asked: (u8, u64), now_ms: u64) -> bool {
        self.asked == asked
            && !(self.cancelled && now_ms.saturating_sub(self.at_ms) > INTERACTION_REPEAT_GAP_MS)
    }
}
/// Time after the client's time-sync (InGame) before other entities go out.
/// Retail starts the stream about 797 ms after the first own bundle; dumping
/// every nearby first-card in the same tick as InGame failed the live client's
/// StateReplication (Game.log 2026-09-24, `stateSequence` 4585).
const OTHER_ENTITY_SETTLE_MS: u64 = 800;
/// Sentinel: the time-sync just ended the spawn hold; the next bundle starts
/// the settle clock.
const OTHER_ENTITY_SETTLE_PENDING: u64 = u64::MAX;
/// How long a mob stands from the start of its alert before it moves (retail sailors: 2.0 s from
/// sailor 1's `Alert_Turn` at 73978 to its run at 76014; 2.8-3.1 s for sailors 2 and 3).
const ALERT_STAND_MS: u64 = 2_000;
/// How long a player who asked to leave the tutorial (502) stays: its last states (the quest's
/// end, the lore, the potions) go out before the handoff (retail: 3951 at 653944, the bundles to
/// 654011, the empty 6310 at 654271, 361 ms after the 502).
const LEAVE_DELAY_MS: u64 = 361;
/// How long after the empty phasing of a leave the handoff starts (retail 6310 at 654271 ms, the
/// open world's level change at 654344 ms).
const PHASING_BEFORE_HANDOFF_MS: u64 = 73;

/// How long a row of refused moves may run before the server moves the player back to the last
/// good position. A refused move leaves the client where it was — but it never sees that, because
/// it only applies the teleport-numbered own state (own-player-position note). Without this
/// move-back the client keeps walking on its own while the server keeps the stale position:
/// later moves miss the distance check too, and area tasks never see the player (live run of
/// 2026-09-14). The honest case — one unlucky refused move — resynchronises on the next input.
const REFUSED_RESYNC_MS: u64 = 1_000;
/// How long the client camera still glides back after the inventory closes, while its inputs
/// do not yet carry the body (retail 2026-09-18: 663 ms, 870114 to 870777).
const LOADOUT_CAMERA_SETTLE_MS: u64 = 700;

/// How a body the server puts somewhere lands there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// It stands at the place (the last good place of a player whose moves were refused).
    Stand,
    /// It falls from the place to the floor it finds (a teleport, a respawn, a script's move:
    /// retail's move into the tutorial sea put the player 7.6 m above the water, and it fell).
    Drop,
}

/// How far under the terrain a body estimate may be and still count as standing on it (our
/// choice: the terrain samples are about 0.06 m apart in height; the camera estimate is good to
/// about 0.1 m, retail median).
const GROUNDED_SLACK_M: f32 = 0.5;

/// Debug bisection knob (env `NW_RELIABLE_BUNDLES`): send the replication bundles reliably on
/// channel 1 instead of unreliably. Off by default; used to test a client that ignores our
/// unreliable channel-1 frames. The name is outside the `NW_SIM_` prefix on purpose, because the
/// sim config parser rejects unknown `NW_SIM_*` env vars. Read once.
fn reliable_bundles() -> bool {
    use std::sync::OnceLock;
    static RELIABLE: OnceLock<bool> = OnceLock::new();
    *RELIABLE.get_or_init(|| std::env::var_os("NW_RELIABLE_BUNDLES").is_some())
}

/// Work-in-progress knob (env `NW_LOCAL_PLAYER_CARD`): send the client its own player as a spawn
/// card (player prefab, slot 1) with a connected type-3935 state and empty states of the other
/// player components (local-player-check note). Off by default until a live InGame check passes.
/// Overnight 2026-09-10: do not make this return true. Without the card the client loads the
/// level and waits at `WaitingForPlayerSpawn`. Read once.
pub(crate) fn local_player_card() -> bool {
    use std::sync::OnceLock;
    static CARD: OnceLock<bool> = OnceLock::new();
    *CARD.get_or_init(|| std::env::var_os("NW_LOCAL_PLAYER_CARD").is_some())
}

/// Debug bisection knob (env `NW_POSITIONS_ONLY`): put only the own player's group in the
/// bundles and omit every other entity, to test the local-player spawn with nothing else in the
/// bundles. Off by default. Read once.
fn positions_only() -> bool {
    use std::sync::OnceLock;
    static POSITIONS_ONLY: OnceLock<bool> = OnceLock::new();
    *POSITIONS_ONLY.get_or_init(|| std::env::var_os("NW_POSITIONS_ONLY").is_some())
}
/// Time between two `TimeSynchMsg` of one player (about 3 s in the captures).
pub const TIME_SYNCH_INTERVAL_MS: u64 = 3000;
/// Most members in one bundle. An unreliable carrier message must fit one datagram (it is never
/// chunked), and a member takes at most 18 bytes (a 5-byte entity number, a 1-byte class, a
/// 12-byte position), so 48 members leave room for the headers.
pub const MAX_BUNDLE_MEMBERS: usize = 48;

/// Settings of the world state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldSettings {
    /// Players see each other inside this radius, in metres.
    pub interest_radius_m: f32,
    /// Most other players and ghosts in a player's bundles: the nearest.
    pub max_visible_players: usize,
    /// Most players in this world.
    pub max_players: u32,
    /// The movement limits.
    pub limits: MoveLimits,
    /// The edges of the level.
    pub bounds: CellBounds,
    /// How far into another sim's cells a player goes before its handoff starts.
    pub handoff_margin_m: f32,
    /// Wait after a refused or late handoff, and after a take, before the next handoff.
    pub handoff_retry_ms: u64,
    /// Ghosts not renewed for this long are dropped.
    pub ghost_ttl_ms: u64,
    /// Threads of the bundle step.
    pub tick_threads: usize,
    /// Whether a client gets its own player as a spawn card ([`local_player_card`]).
    pub local_player_card: bool,
    /// The level this sim runs. The tutorial is a set of private instances: it hands players
    /// on only at its exit, into the open world (new-player-start note).
    pub level: LevelSpec,
    /// The height of the level's sea (`terrain.json` `oceanLevel`), in metres: a player near it
    /// swims.
    pub ocean_level_m: f32,
    /// Whether this sim has the instance role: it runs one expedition, raid or trial at a time
    /// ([`instances`]).
    pub instance_role: bool,
    /// Longest wait for the director's answer to an expedition request.
    pub instance_request_timeout_ms: u64,
}

/// Fewest players for which the bundle step uses more than one thread; below that, starting
/// threads costs more than it saves.
pub const MIN_PLAYERS_PER_THREAD: usize = 64;

/// Why a move did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MoveError {
    /// The connection has no player in the world.
    #[error("no player on this connection")]
    NotInWorld,
    /// The player waits for the answer to its handoff; its moves are ignored.
    #[error("the player is frozen for a handoff")]
    Frozen,
    /// The movement checks refused the move; the player keeps its last good position.
    #[error("move refused: {}", .0.reason())]
    Refused(MoveRefusal),
    /// The player is dead.
    #[error("the player is dead")]
    Dead,
    /// The server moves the player now (a pull or push); its client's moves wait.
    #[error("the server moves the player")]
    Pushed,
}

/// A move of the player that the server makes over time (a boss's pull or push).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Push {
    from: Position,
    to: Position,
    start_ms: u64,
    end_ms: u64,
    /// The player got to `to`: the next tick ends the push, after its last own state.
    arrived: bool,
}

/// Messages for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// The connection.
    pub connection: ConnectionRef,
    /// The carrier channel.
    pub channel: u8,
    /// Whether the carrier sends them reliably.
    pub reliable: bool,
    /// The envelopes.
    pub envelopes: Vec<Vec<u8>>,
}

/// A player who left, for the last save.
#[derive(Debug, Clone, PartialEq)]
pub struct Departure {
    /// The connection the player had.
    pub connection: ConnectionRef,
    /// The character.
    pub character: Uuid,
    /// The last good position.
    pub position: Position,
    /// Server time of the last good move: a save of an older move never overwrites a newer one.
    pub moved_at_ms: u64,
    /// The quests to save; `None` when there is nothing to save (no change since the last save,
    /// or the tutorial).
    pub quests: Option<PlayerQuests>,
    /// The items to save; `None` when there is nothing to save (no change since the last save,
    /// or the tutorial, which always starts from the starter kit).
    pub items: Option<Vec<CarriedItem>>,
    /// The storage chests and loadouts, saved with the items: `Some` with them.
    pub inventory: Option<PlayerInventory>,
    /// The progression to save; `None` when there is nothing to save (no change since the last
    /// save, or the tutorial).
    pub progression: Option<PlayerProgression>,
}

/// A handoff that a tick started: the player is frozen until [`SimWorld::handoff_done`].
#[derive(Debug, Clone, PartialEq)]
pub struct HandoffStart {
    /// The sim that owns the player's cell.
    pub target: SimSlot,
    /// What the target needs.
    pub offer: HandoffOffer,
}

/// The end of a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffEnd {
    /// The target took the player; it is gone from this world.
    Moved,
    /// The target refused or did not answer; the player stays and may move again.
    Kept,
    /// The player left during the handoff.
    Gone,
}

/// Time of each part of one tick, in seconds, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TickParts {
    /// Old ghosts, handoffs to start and the ghost lists for other sims.
    pub ghosts: f64,
    /// Mob minds, their swings and shots.
    pub mobs: f64,
    /// Everything players do: quests, effects, gathers, crafts, abilities, travel.
    pub gameplay: f64,
    /// The replicas of the players and the cards of the shown mobs, on one thread.
    pub cards: f64,
    /// The bundles of every player and the messages of the tick.
    pub bundles: f64,
}

impl TickParts {
    /// Label value of the ghost part.
    pub const GHOSTS: &'static str = "ghosts";
    /// Label value of the mob part.
    pub const MOBS: &'static str = "mobs";
    /// Label value of the gameplay part.
    pub const GAMEPLAY: &'static str = "gameplay";
    /// Label value of the card part.
    pub const CARDS: &'static str = "cards";
    /// Label value of the bundle part.
    pub const BUNDLES: &'static str = "bundles";

    /// Each part with its label value.
    #[must_use]
    pub fn labelled(&self) -> [(&'static str, f64); 5] {
        [
            (Self::GHOSTS, self.ghosts),
            (Self::MOBS, self.mobs),
            (Self::GAMEPLAY, self.gameplay),
            (Self::CARDS, self.cards),
            (Self::BUNDLES, self.bundles),
        ]
    }
}

/// What one tick sends.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TickOutput {
    /// Hit and death messages, then bundles and time messages.
    pub messages: Vec<Outgoing>,
    /// Handoffs to ask for.
    pub handoffs: Vec<HandoffStart>,
    /// Ghost lists for other sims; an empty list clears the receiver's last one.
    pub ghosts: Vec<(BusId, Vec<Ghost>)>,
    /// What the mobs' hits on players did this tick.
    pub mob_hits: Vec<AttackOutcome>,
    /// What the players' swings that landed this tick did.
    pub player_attacks: Vec<Result<AttackOutcome, AttackRefusal>>,
    /// Players who died since the last tick, to save at once (M6-A spec section 8).
    pub deaths: Vec<Departure>,
    /// Time of each part of the tick.
    pub parts: TickParts,
    /// Members put in this tick's bundles: each player's own member and every other entity.
    pub bundle_members: u64,
    /// The script run log lines of this tick.
    pub script_log: Vec<ScriptLogLine>,
}

/// The bundle messages of some players and the members they hold.
#[derive(Debug, Default)]
struct Bundled {
    outgoing: Vec<Outgoing>,
    members: u64,
    /// Time of the replicas and mob cards before the bundles, in seconds.
    cards_seconds: f64,
}

/// A refused world change.
#[derive(Debug, Error)]
pub enum WorldError {
    /// The connection is already in the world.
    #[error("connection {0:?} is already in the world")]
    AlreadyJoined(ConnectionRef),
    /// The position is not a finite point.
    #[error("position is not finite")]
    NotFinite,
    /// The character is already in the world on another connection.
    #[error("character {0} is already in the world")]
    CharacterInWorld(Uuid),
    /// The world has `max_players` players.
    #[error("the sim is full")]
    Full,
    /// No random bytes for the session ids.
    #[error("no random bytes: {0}")]
    Random(#[from] getrandom::Error),
    /// A message cannot be encoded.
    #[error(transparent)]
    Message(#[from] MessageError),
    /// An instance sim takes only the members of the instance it runs.
    #[error("character {0} is not a member of this sim's instance")]
    NotInInstance(Uuid),
}

/// A cell table that does not fit the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TableError {
    /// Its grid is not the grid of this level, or its owner list has another length.
    #[error("the cell table does not fit the level")]
    WrongSize,
}

#[derive(Component)]
struct Player {
    connection: ConnectionRef,
    character: Uuid,
    name: String,
    /// The saved look, sent with the own spawn card.
    look: Option<PlayerLook>,
    /// The backstory of the archetype pick; it goes on with a handoff.
    backstory: Option<String>,
    /// The worn and carried items, sent with the own spawn card and after each change.
    gear: Option<Gear>,
    /// The items at the last gear change, to count the 7048 of the next one.
    item_changes: storage_search::ItemChanges,
    number: u64,
    bundles_sent: u64,
    last_time_synch_ms: u64,
    interest: Interest<u64>,
    /// The client's entity slots.
    slots: Slots,
    /// Whether the own spawn card went out from this sim.
    own_card_sent: bool,
    /// Earliest time the own spawn card may go out.
    own_card_due_ms: u64,
    /// The envelopes of the own spawn card bundle, kept so the same bundle can go out again
    /// until the client spawns ([`OWN_CARD_RESEND_MS`]); `None` before the card and after
    /// the spawn.
    own_card_bundle: Option<Vec<Vec<u8>>>,
    /// It came by a login ([`Arrival::login`]).
    login: bool,
    /// When the own spawn card bundle last went out.
    own_card_sent_ms: u64,
    /// When other entities may go out after the time-sync spawn ([`OTHER_ENTITY_SETTLE_MS`]).
    /// `None` until the client has spawned; `Some(0)` when this player was already in the
    /// world (a same-level handoff).
    others_due_ms: Option<u64>,
    /// The client-context id of the player's current level; every bundle carries it.
    client_context: u8,
    /// What the client and this server still say to each other in this level.
    flow: LevelFlow,
    /// When the player leaves the tutorial, once its client asked to (502).
    leave_at_ms: Option<u64>,
    /// When the empty phasing of the leave went out; the handoff waits for it.
    phasing_left_ms: Option<u64>,
    /// Whether the server moved the player: its next own state moves it ([`OwnTeleport`]).
    moved: bool,
    /// Its body as the server moves it from its client's keys, held to the client's camera
    /// ([`Self::move_from_input`]).
    tracked: TrackedBody,
    /// The counter of the newest input record its body moved by.
    input_counter: Option<u16>,
    /// The number of its ALC states from that counter ([`StateNumber`]).
    state_number: Option<StateNumber>,
    /// The view angles of its newest input: where other players see it face and look.
    view: Option<LookDir>,
    /// The view angles the newest own state carried back to its client.
    view_sent: Option<LookDir>,
    /// Its slayer state: the action its client started, then the server's follow-up, or its
    /// movement ([`slayer`]).
    slayer: slayer::SlayerTrack,
    /// Which other players it got in which bundle step ([`replica::RemotePlan`]).
    remote_plan: replica::RemotePlan,
    /// An own state after the spawn cleared the input throttle that the spawn state sets.
    throttle_cleared: bool,
    /// The client reported throttle on (998 `01`). The next plain state does not clear the
    /// bit; the state after that does. Retail clears only after the client has applied it.
    defer_throttle_clear: bool,
    /// The client still auto-pins a new quest (2709). A mask of all zeros turns it off.
    auto_pin: bool,
    /// The player toggled the PvP flag (2525). Retail sends the faction header and no argument.
    pvp_flagged: bool,
    /// The own places its client has ([`own_group`]).
    own_places: OwnPlaces,
    /// The ground and water under its body after its newest moves ([`SimWorld::footing_of`]).
    footing: Footing,
    /// The footing its client has: the spawn state's, then what the own states carried.
    footing_sent: Footing,
    /// Where the player faces after the server's move, when the move says so.
    move_facing: Option<f32>,
    /// The server moves the player now: its own states carry the place.
    push: Option<Push>,
    /// The next state of a server move still to send.
    teleport_step: Option<TeleportStep>,
    /// Server time of the first refused move in the current row; `None` after a good move.
    /// A row of [`REFUSED_RESYNC_MS`] sets `moved`, so the client returns to the last good
    /// position it could not otherwise see.
    refused_since_ms: Option<u64>,
    /// The last accepted position on or above the terrain: where a move-back goes. An accepted
    /// body estimate can be under the terrain within the ground tolerance, and a move-back there
    /// put the player under the world (live 2026-09-18: 12 move-backs to 1.8 m under the beach).
    last_grounded: Option<Position>,
    /// Until this server time the client camera does not carry the body: the inventory camera
    /// is open, or still gliding back. Its inputs move nothing (retail held the player still).
    camera_held_until_ms: Option<u64>,
    /// Its inventory is open (`lookingThroughLoadout` of its 3935 state), as its client said in
    /// its newest 3970.
    looking_through_loadout: bool,
    /// The change of [`Player::looking_through_loadout`] is not sent yet
    /// ([`put_looking_through_loadout`]).
    loadout_change_due: bool,
    /// Server time of the first own state, the start of the own clock.
    own_spawned_ms: Option<u64>,
    /// Moves of the player by the server so far.
    teleports: u8,
    /// The quest tracker of its tutorial or active quest.
    quest: Option<QuestTracker>,
    /// The objectives it finished.
    done_quests: Vec<String>,
    /// Whether its quests changed since they were last taken for a save.
    quests_unsaved: bool,
    /// The version of the newest quest tracker lists its client got from the sim before: this
    /// sim's lists go on above it.
    tracker_floor: u64,
    /// Its experience points, and whether they changed since the last bundle.
    experience: u32,
    experience_changed: bool,
    /// Its coins, and whether they changed since the last bundle.
    coins: u64,
    coins_changed: bool,
    /// Its progression as last taken for a save, or as loaded at its join; `None` after a
    /// handoff (the sender may not have saved it), so the next save round saves it.
    progression_saved: Option<PlayerProgression>,
    /// Its weapon masteries and weapon points.
    masteries: progression::Masteries,
    /// Its attribute points.
    attributes: points::Attributes,
    /// Its meta achievements.
    meta: meta_achievements::MetaState,
    /// The title it shows (2474), and whether its client was not told of it yet.
    title: Option<u32>,
    title_changed: bool,
    /// The level its client was last sent; `None` before the first card.
    level_sent: Option<u32>,
    /// Whether its account has the game master rank: the sim takes its client's GM messages.
    game_master: bool,
    /// Whether its account is in probation: its client blocks global and trade chat.
    account_in_probation: bool,
    /// Its account.
    account: Uuid,
    /// The store entitlements its account holds (the store mounts it got).
    entitlements: BTreeSet<String>,
    /// The crafting station it uses and the interaction it opened it with, until that
    /// interaction ends.
    station: Option<String>,
    /// The interaction that opened [`Player::station`].
    station_interaction: Option<u64>,
    /// Its recipe cooldown counts ([`nw_content::recipe_cooldowns`]).
    recipe_cooldowns: RecipeCooldownCounts,
    /// Its faction ([`faction`]).
    faction: nw_bus::PlayerFaction,
    /// The quick course it rides ([`quick_course`]).
    course: quick_course::PlayerCourse,
    /// The last open-world interaction its client asked for ([`LastInteraction`]).
    last_interaction: Option<LastInteraction>,
    /// The open-world interaction it runs and the interact tags of its object, from its start
    /// to the client's end of it (`TaskInteract`).
    open_interaction: Option<(u64, Vec<String>)>,
    /// The raw id of the mission board it opened last.
    open_board: Option<[u8; nw_bus::MISSION_BOARD_ID_LEN]>,
    /// The version of its objective interactor's mission list.
    board_version: u64,
    /// Its client asks for an interaction in its inputs: the interaction ends at the first input
    /// without one (retail gives a gather's put-away weapon back then, [`Stow::Gather`]).
    interacting: bool,
    /// The territory areas its client says it is in (2297 and 6416), for go-to and exit tasks.
    territories: Vec<u16>,
    /// The zone (`IsTerritory`) of the newest area its client named on entering or leaving, for
    /// a camp's place: the client names a zone only on crossing into it, and the points of
    /// interest it passes lie in the zone it is in (Inferred).
    last_zone: Option<u32>,
    /// Its hits on the level's quest objects.
    strikes: quest_objects::Strikes,
    /// The level's quest objects whose detection sphere it is in and whose game event it set off
    /// on the way in (an index of [`crate::player_content::QuestObjects`]).
    detected: HashSet<usize>,
    /// Its running consumable cooldowns, and whether they changed since the last bundle.
    cooldowns: Vec<ConsumableCooldown>,
    cooldowns_changed: bool,
    /// The repeating vitals changes of the consumables it used.
    item_effects: item_calls::ItemEffects,
    /// Its status effects ([`status_effects`]).
    status_effects: status_effects::StatusEffects,
    /// The fall its body is in ([`fall_damage`]).
    fall: fall_damage::FallTrack,
    /// The health its client last got (a card or a vitals change).
    health_sent: Option<u32>,
    /// Its client got its death's door block and not yet the end of it.
    deaths_door_sent: bool,
    /// The max health its client was last sent; `None` before the first card.
    health_max_sent: Option<u32>,
    /// The server time of its last spawn or respawn (ms since 2000-01-01).
    life_ms: u64,
    /// Its stamina.
    stamina: Stamina,
    /// Its weapon abilities: the key presses, the running ability actions, the cooldowns and the
    /// mana ([`abilities`]).
    abilities: abilities::AbilityRun,
    /// The basic attack its client plays, for the dodge's cancel windows
    /// ([`attack::PlayingAttack`]).
    attack_playing: Option<attack::PlayingAttack>,
    /// Its mounts: the chosen one, their dyes and names, the summon, the ride and its stamina
    /// ([`mount`]).
    mount: mount::PlayerMount,
    /// Its fishing: the pole, the cast and the replicated steps ([`fishing`]).
    fishing: fishing::PlayerFishing,
    /// Its side of the musical performances: the one it plays in, the one it watches, its
    /// lockout and what its client got of each ([`music`]).
    music: music::PlayerMusicState,
    /// How often each event notification was shown to it ([`notifications`]).
    notifications: notifications::ShownNotifications,
    /// Its combat status timer.
    combat: CombatTimer,
    /// The version of its replicated lists that this server sends besides the gear.
    list_version: u64,
    /// Own-group members to append to the next bundle (the interactor target of a take, the
    /// gather states of the chest).
    extra_members: Vec<BundleMember>,
    /// Its replicated social states and group entity from the social service
    /// ([`social_states`]).
    social: social_states::PlayerSocial,
    /// Gathers this player made, for the counters of the gather state.
    gathers: u32,
    /// Its turns of each cyclic gatherable whose gatherable shows in many states (a puzzle
    /// stone), by lowercase variant id: the state it sees is the turns modulo its states, from 0
    /// (retail `99c_msq_03_ancientbridge`: the first turn of a stone was its state 0, the third
    /// its state 2).
    cyclic_turns: HashMap<String, u32>,
    /// The version of each gather node its client has, by slot key.
    node_versions: HashMap<u64, u32>,
    /// The version of each loot bag its client has, by slot key.
    bag_versions: HashMap<u64, u32>,
    /// Its loot tracker's bags.
    loot: loot_bag::PlayerLoot,
    /// Its last open-world gather that ended: its client's cancel of it comes after the end.
    ended_gather: Option<InteractionRequest>,
    /// The tutorial's static level entities it sees.
    statics: StaticViews,
    /// The tutorial's asset entities it sees.
    assets: AssetViews,
    /// The solids of the asset entities it sees ([`crate::asset_solids`]): its ground stands on
    /// them over the level's.
    solids: Option<Arc<CollisionIndex>>,
    /// Its travel state: what a save keeps and the travel that runs.
    travel: travel::TravelState,
    /// Its expeditions, raids and trials ([`instances`]).
    arena: instances::ArenaPlayer,
    /// Its matchmaking queue, war board and war sign-up state ([`game_mode`]).
    game_mode: game_mode::GameModePlayer,
    /// The weekly mutations its client has ([`mutations`]).
    mutation_view: mutations::ScheduleView,
    frozen: bool,
    handoffs: u64,
    no_handoff_before_ms: u64,
}

impl Player {
    /// Its experience, coins, achievement bits and loot limit counts.
    fn progression(&self) -> PlayerProgression {
        PlayerProgression {
            experience: self.experience,
            coins: self.coins,
            achievements: self.flow.achievement_bits(),
            loot_limits: self.loot.limit_counts(),
            masteries: self.masteries.carried(),
            attributes: self.attributes.carried(),
            weapon_points: self.masteries.carried_points(),
            territory_points: self.masteries.carried_territory_points(),
            travel: self.travel.carried(),
            mounts: self.mount.carried(),
            ability_slots: self.masteries.carried_slots(),
            meta_counters: self.meta.carried_counters(),
            meta_completed: self.meta.carried_completed(),
            title: self.title,
            arena: self.arena.carried(),
            music: self.music.carried(),
            notifications: self.notifications.carried(),
            recipe_cooldowns: self
                .recipe_cooldowns
                .counts()
                .into_iter()
                .map(|count| nw_bus::RecipeCooldownCount {
                    recipe_id: count.recipe,
                    count: count.count,
                    ends_ns: count.ends_ns,
                })
                .collect(),
            faction: self.faction,
            status_effects: self
                .status_effects
                .kept(|status_id| self.item_effects.repeat_of(status_id)),
            consumable_cooldowns: self
                .cooldowns
                .iter()
                .map(|cooldown| nw_bus::PlayerConsumableCooldown {
                    group: cooldown.group,
                    consumable: cooldown.consumable,
                    end_us: cooldown.end_us,
                })
                .collect(),
        }
    }

    /// Its progression when it changed since it was last taken for a save or loaded; `None`
    /// when it did not.
    fn take_unsaved_progression(&mut self) -> Option<PlayerProgression> {
        let progression = self.progression();
        if self.progression_saved.as_ref() == Some(&progression) {
            return None;
        }
        self.progression_saved = Some(progression.clone());
        Some(progression)
    }

    /// The version of the newest quest tracker lists its client has: its tracker's, or what the
    /// sim before sent.
    fn tracker_shown(&self) -> u64 {
        self.quest
            .as_ref()
            .map_or(0, QuestTracker::version)
            .max(self.tracker_floor)
    }
}

/// What a client already has of the replication stream: it goes on across a handoff.
struct Replication {
    bundles_sent: u64,
    slots: Slots,
}

#[derive(Component, Clone, Copy)]
struct Body {
    motion: Motion,
}

/// The cells as this sim sees them.
struct Cells {
    owners: CellOwners<u16>,
    sims: Vec<SimSlot>,
    me: u16,
}

struct ReceivedGhosts {
    received_ms: u64,
    entities: Vec<ReceivedGhost>,
}

/// One entity of another sim.
#[derive(Clone, Copy)]
struct ReceivedGhost {
    entity: u64,
    position: Position,
    /// A mob's state; `None` for a player.
    mob: Option<MobGhost>,
    /// A player's state, for the players who see it.
    player: Option<PlayerGhost>,
}

/// The players of one sim.
pub struct SimWorld {
    world: World,
    by_connection: HashMap<ConnectionRef, Entity>,
    settings: WorldSettings,
    ids: WorldIds,
    ground: Arc<dyn Ground + Send + Sync>,
    /// The floor the players walked on: where a server push may go.
    walked_floor: walked_floor::WalkedFloor,
    /// What the viewers were last sent of each player ([`replica`]).
    replica_memory: HashMap<u64, replica::ReplicaMemory>,
    /// Where the mobs stood in the last moments: where a swing may find them.
    mob_trails: mob_trail::MobTrails,
    me: SimSlot,
    cells: Option<Cells>,
    /// The last cell table this sim applied. The director sends its table again every second (on
    /// its heartbeat timer), and applying one walks every spawn of the level: 30 to 38 ms for
    /// the open world's 115,965 spawns, a late tick each second (run 13: 505 in 9 minutes).
    applied_table: Option<CellTable>,
    /// Tutorial sims only: the open-world sim that owns [`OPEN_WORLD_START`], where the exit
    /// hands players.
    exit_target: Option<SimSlot>,
    ghosts: HashMap<BusId, ReceivedGhosts>,
    ghost_targets: BTreeSet<BusId>,
    /// Whether this sim has the instance role ([`WorldSettings::instance_role`]).
    instance_sim: bool,
    /// The instance this instance sim runs.
    instance_run: Option<instances::InstanceRun>,
    /// The war sign-ups of this sim's players, by war ([`game_mode`]).
    war_signups: game_mode::WarSignups,
    /// The encounters of the running instance's level script.
    dungeon_run: Option<dungeon_script::DungeonRun>,
    /// The expedition requests for the director since the last tick.
    instance_requests: Vec<(ConnectionRef, nw_bus::InstanceRequest)>,
    /// The level of each instance game mode, by its CRC: an instance names its own frame.
    instance_levels: HashMap<u32, LevelSpec>,
    /// This world's hold on the raw ids of its players, its shown mobs and the ghosts it adopted
    /// (GDE id note, "Dynamic entities").
    raw_ids: IdHolder,
    /// The ghosts whose owner's ids this world holds.
    ghost_ids: HashSet<u64>,
    mobs: Mobs,
    mob_changes: MobChanges,
    combat: CombatSettings,
    /// The starter kits and achievement bits.
    content: Arc<PlayerContent>,
    /// The achievement bits players start with and the tutorial flow sets.
    achievement_rules: AchievementRules,
    /// Tutorial sims only: the tutorial of each player, by entity number.
    tutorials: HashMap<u64, Tutorial>,
    /// Open-world sims: the running quests of each player, by entity number.
    quests: HashMap<u64, Vec<ActiveQuest>>,
    /// The time of the last tick, for the quest runs that a save takes between ticks.
    last_now_ms: u64,
    /// The scripts of the level and their runs.
    scripts: scripts::Scripts,
    /// The events of the level and the running ones.
    events: events::Events,
    /// The state numbers, clock and animation of each shown mob.
    mob_timelines: MobTimelines,
    /// The animations of each creature slice.
    creature_animations: HashMap<[u8; ASSET_ID_LEN], Animations>,
    /// The health each shown mob had in the last bundle step.
    mob_health_shown: HashMap<u64, u32>,
    /// The boss phase of each boss mob that left phase 0.
    boss_phases: HashMap<u64, u32>,
    /// The boss phase each shown boss mob last sent.
    boss_phases_shown: HashMap<u64, u32>,
    /// The remote effects each shown mob's clients have, with the map's version.
    mob_effects_shown: HashMap<u64, (u64, Vec<(u16, nw_messages::RemoteEffect)>)>,
    /// The last player each shown mob picked as its target, by entity number.
    look_targets: HashMap<u64, u64>,
    /// The open world's gather nodes that left their default state.
    nodes: Nodes,
    /// The level's crafting stations.
    stations: crafting::Stations,
    /// The loot bags of killed mobs.
    bags: loot_bag::LootBags,
    /// The musical performances of this sim's players.
    performances: music::Performances,
    /// The gathers that run.
    open_gathers: Vec<OpenGather>,
    /// The crafts that run.
    crafts: Vec<crafting::PendingCraft>,
    /// The dead mobs that were skinned, and those whose corpse state the bundles sent.
    skinned: HashMap<u64, u64>,
    skinned_shown: HashSet<u64>,
    dice: Dice,
    deaths: Vec<Departure>,
    /// Hit, death and facet messages since the last [`SimWorld::take_messages`].
    combat_messages: Vec<Outgoing>,
    /// Messages that go out after the bundles of this tick: an answer that must reach the client
    /// after the states it names (the craft progress after its items, crafting module).
    after_bundles: Vec<Outgoing>,
    /// Quick slot items pressed: the player number, the quick slot and when the use works.
    drinks: Vec<(u64, u32, u64)>,
    /// Mob swings that have not hit yet: the time they hit, the server time they started, and
    /// the swing.
    swings: Vec<PendingMobSwing>,
    /// Player swings that have not hit yet.
    player_swings: Vec<PlayerSwing>,
    /// The projectiles of ranged and magic attacks on their way to their mob.
    projectile_flights: Vec<projectile::ProjectileFlight>,
    /// The status effects of friendly ability projectiles on their way to their player.
    effect_flights: Vec<abilities::EffectFlight>,
    /// The spell areas of player abilities, open or still to open.
    spell_areas: Vec<spell_areas::SpellArea>,
    /// The projectiles mobs fired, on their way to where they aimed.
    mob_shots: Vec<mob_shot::MobShot>,
    /// Swings that a tutorial script started, for the next tick.
    scripted_swings: Vec<MobAttackIntent>,
    /// What the players did that leaderboard stats count, since the last
    /// [`SimWorld::take_stat_events`].
    stat_events: Vec<StatEvent>,
    /// The trading post calls, deliveries and player trades of this sim's players.
    economy: economy::Economy,
    /// The status effects of the mobs that have any, by entity number.
    mob_effects: HashMap<u64, status_effects::StatusEffects>,
    /// The server time of the last tick (ms since 2000-01-01).
    server_ms: u64,
}

/// A quest a player runs.
#[derive(Debug)]
struct ActiveQuest {
    def: QuestDef,
    run: ObjectiveRun,
    /// A mission's take.
    take: Option<MissionTake>,
}

/// The journal record of quest `def`, taken as `take` when it is a mission.
fn record_of(def: &QuestDef, take: Option<MissionTake>) -> JournalRecord {
    JournalRecord {
        mission: def.mission.unwrap_or_default(),
        take,
        ..JournalRecord::of(&def.wire_objective)
    }
}

/// Why a quest did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QuestRefusal {
    /// The player is not on this sim.
    #[error("the player is not here")]
    NoPlayer,
    /// The content has no such objective, or its root task is missing.
    #[error("no such quest")]
    NoQuest,
    /// The player runs this quest already, or the tutorial.
    #[error("the player already runs this quest or the tutorial")]
    Active,
    /// The journal holds as many quests of its kind as the kind's limit.
    #[error("the journal holds the most quests of this kind")]
    Full,
    /// The player finished it and it is not repeatable.
    #[error("the player finished this quest")]
    Done,
    /// The player is not of its `RequiredFaction`, or is of the faction it excludes.
    #[error("the player's faction may not take this quest")]
    Faction,
}

/// A new player before it is in the world.
struct Arrival {
    connection: ConnectionRef,
    character: Uuid,
    name: String,
    look: Option<PlayerLook>,
    backstory: Option<String>,
    number: u64,
    position: Position,
    health: Health,
    /// Whether the player is already in the world (spawn protection off). A fresh login is
    /// `false`; a handed-off player is `true`. See [`attack::Vitals`].
    spawned: bool,
    /// Earliest time the own spawn card may go out ([`OWN_CARD_DELAY_MS`]).
    own_card_due_ms: u64,
    /// A login, not a handoff: only a login resends its own card ([`OWN_CARD_RESEND_MS`]) and
    /// waits [`OTHER_ENTITY_SETTLE_MS`] after the spawn (retail newchar 2026-09-18: at the beach
    /// level change the card went once and the other entities 2 ms after it).
    login: bool,
    /// The client-context id of the player's current level.
    client_context: u8,
    /// The flow of the level it arrives in.
    flow: LevelFlow,
    /// The items another sim sent; `None` gives the login kit.
    items: Option<Vec<CarriedItem>>,
    /// Its storage chests and saved loadouts.
    inventory: PlayerInventory,
    /// The main-hand option slot it held drawn on the other sim.
    drawn_weapon: Option<u32>,
    /// The main-hand option slot it had selected on the other sim.
    active_weapon: Option<u32>,
    /// Its saved or carried progression.
    progression: PlayerProgression,
    /// Whether its items, quests and progression are what the database holds or a save of this
    /// sim waits with (a join). A handoff's may not be saved yet: the sender saves nothing at a
    /// handoff, so this sim saves them at its next save round.
    saved: bool,
    /// The versions of the newest 911 and 3681 lists its client has (0 for none).
    mastery_versions: (u64, u64),
    /// The version of the newest 129 lists its client has (0 for none).
    attribute_version: u64,
    /// The sequence of the next fishing step its client expects (0 for a join).
    fishing_sequence: u16,
    /// Whether its account has the game master rank.
    game_master: bool,
    /// Whether its account is in probation.
    account_in_probation: bool,
    /// Its account.
    account: Uuid,
    /// The store entitlements its account holds.
    entitlements: BTreeSet<String>,
    /// Its saved or carried quests.
    quests: PlayerQuests,
    /// Its PvP flag: off at a login, carried by a handoff.
    pvp_flagged: bool,
    /// Whether its client auto-pins a new quest: on at a login, carried by a handoff.
    auto_pin: bool,
}

impl SimWorld {
    /// An empty world of the sim `me` on the ground of `ground`, with the mobs of `combat`.
    pub fn new(
        settings: WorldSettings,
        ids: WorldIds,
        ground: Arc<dyn Ground + Send + Sync>,
        me: SimSlot,
        combat: CombatSettings,
        content: Arc<PlayerContent>,
    ) -> Self {
        // Only an instance sim names the levels of the game modes.
        let instance_levels = if settings.instance_role {
            instances::instance_levels(&content.instances)
        } else {
            HashMap::new()
        };
        Self {
            achievement_rules: AchievementRules::of(
                &content.achievements,
                &content.archetype_unlocks,
            ),
            tutorials: HashMap::new(),
            quests: HashMap::new(),
            last_now_ms: 0,
            scripts: scripts::Scripts::default(),
            events: events::Events::default(),
            mob_timelines: MobTimelines::default(),
            creature_animations: animations_of(&content.creature_animations),
            mob_health_shown: HashMap::new(),
            boss_phases: HashMap::new(),
            boss_phases_shown: HashMap::new(),
            mob_effects_shown: HashMap::new(),
            look_targets: HashMap::new(),
            nodes: Nodes::default(),
            stations: crafting::Stations::build(&content.stations, &content.structure_costs),
            bags: loot_bag::LootBags::default(),
            performances: music::Performances::default(),
            open_gathers: Vec::new(),
            crafts: Vec::new(),
            skinned: HashMap::new(),
            skinned_shown: HashSet::new(),
            content,
            mobs: Mobs::new(Arc::clone(&combat.spawns), &combat),
            raw_ids: IdHolder::new(),
            ghost_ids: HashSet::new(),
            mob_changes: MobChanges::default(),
            dice: Dice::new(combat.seed),
            deaths: Vec::new(),
            walked_floor: walked_floor::WalkedFloor::default(),
            replica_memory: HashMap::new(),
            mob_trails: mob_trail::MobTrails::default(),
            combat_messages: Vec::new(),
            after_bundles: Vec::new(),
            drinks: Vec::new(),
            swings: Vec::new(),
            player_swings: Vec::new(),
            projectile_flights: Vec::new(),
            effect_flights: Vec::new(),
            spell_areas: Vec::new(),
            mob_shots: Vec::new(),
            scripted_swings: Vec::new(),
            stat_events: Vec::new(),
            economy: economy::Economy::default(),
            mob_effects: HashMap::new(),
            server_ms: 0,
            instance_sim: settings.instance_role,
            instance_run: None,
            war_signups: game_mode::WarSignups::default(),
            dungeon_run: None,
            instance_requests: Vec::new(),
            instance_levels,
            combat,
            world: World::new(),
            by_connection: HashMap::new(),
            settings,
            ids,
            ground,
            me,
            cells: None,
            applied_table: None,
            exit_target: None,
            ghosts: HashMap::new(),
            ghost_targets: BTreeSet::new(),
        }
    }

    /// Whether the player of `connection` is here.
    pub fn has(&self, connection: &ConnectionRef) -> bool {
        self.by_connection.contains_key(connection)
    }

    /// The design overrides of the level's spawns (`mob_ai::resolve_ai`); set before the first
    /// cells.
    pub fn set_mob_ai(&mut self, ai: Arc<[Option<SpawnAi>]>) {
        self.mobs.set_ai(ai);
    }

    /// Players in the world.
    pub fn players(&self) -> usize {
        self.by_connection.len()
    }

    /// Adds the player of `admit` and returns its login messages, or `None` for a resumed
    /// player, which has them already. A resume of a player that this sim already has on that
    /// connection with that entity number (it took the player in a handoff that its lost sender
    /// never ended) keeps the player as it is.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError`] when the world is full, the connection or the character is already
    /// in the world, or the position is not finite.
    pub fn admit(
        &mut self,
        admit: &Admit,
        now_ms: u64,
        server_time: u64,
    ) -> Result<Option<Outgoing>, WorldError> {
        let here = self
            .by_connection
            .get(&admit.connection)
            .and_then(|entity| self.world.get::<Player>(*entity))
            .is_some_and(|player| player.number == admit.entity);
        if here && !admit.login {
            return Ok(None);
        }
        let level = self.arrival_level();
        let level_info = if admit.login {
            Some(login_level_info(
                level,
                server_time,
                self.entry_phase(level)?,
            ))
        } else {
            None
        };
        let flow = match &level_info {
            Some(level_info) => LevelFlow::after_level_info(
                admit.entity,
                level_info.clone(),
                now_ms,
                0,
                &self.achievement_rules,
                self.settings.level.private,
                admit.backstory.as_deref(),
                &admit.progression.achievements,
                0,
            ),
            None => LevelFlow::resumed(admit.entity, 0, &admit.progression.achievements, 0),
        };
        let arrival = Arrival {
            connection: admit.connection.clone(),
            character: admit.character,
            name: admit.name.clone(),
            look: admit.look.map(PlayerLook::of_choices),
            backstory: admit.backstory.clone(),
            number: admit.entity,
            // A saved spot can be in the air or under the ground (a save during a jump, or a
            // height from another world file); the player arrives on the ground.
            position: self
                .settings
                .limits
                .onto_ground(&*self.ground, Position::from(admit.position)),
            health: Health::full(
                self.player_max_health(admit.progression.experience, &admit.progression.attributes),
            ),
            // A fresh login gets spawn protection until its client spawns; a resume was already
            // in the world.
            spawned: !admit.login,
            own_card_due_ms: if admit.login {
                now_ms.saturating_add(OWN_CARD_DELAY_MS)
            } else {
                now_ms
            },
            login: admit.login,
            client_context: LOGIN_CLIENT_CONTEXT,
            flow,
            items: admit.items.clone(),
            inventory: admit.inventory.clone(),
            drawn_weapon: None,
            active_weapon: None,
            progression: admit.progression.clone(),
            saved: true,
            mastery_versions: (0, 0),
            attribute_version: 0,
            fishing_sequence: 0,
            game_master: admit.game_master,
            account_in_probation: admit.account_in_probation,
            account: admit.account,
            entitlements: admit.entitlements.iter().cloned().collect(),
            quests: admit.quests.clone(),
            pvp_flagged: false,
            auto_pin: true,
        };
        self.check(&arrival)?;
        let login = if let Some(level_info) = &level_info {
            let mut envelopes = login_messages(
                &self.ids,
                SessionIds::random()?,
                admit.entity,
                server_time,
                level_info,
            )?;
            // Retail syncs the notification counts in the login burst, before the level info.
            if let Some(sync) =
                notifications::ShownNotifications::of(&admit.progression.notifications)
                    .login_sync(admit.entity)
            {
                let level_info_at = envelopes.len().saturating_sub(1);
                envelopes.insert(level_info_at, sync.envelope()?);
            }
            if admit.login {
                // Retail leaves the game-mode queue 24 ms after the level info (4970, value 1).
                envelopes.push(
                    ServerMessage::LeftGameModeQueue(nw_messages::LeftGameModeQueueMsg::tutorial(
                        admit.entity,
                    ))
                    .envelope()?,
                );
            }
            Some(Outgoing {
                connection: admit.connection.clone(),
                channel: MESSAGE_CHANNEL,
                reliable: true,
                envelopes,
            })
        } else {
            None
        };
        let motion = Motion::new(arrival.position, now_ms, &self.settings.limits);
        // A fresh login must start its bundle sequence above the client's mark; a resume keeps the
        // sequence it already carried.
        let first_sequence = if admit.login {
            admit.bundles_sent.max(FIRST_LOGIN_BUNDLE_SEQUENCE)
        } else {
            admit.bundles_sent
        };
        let replication = Replication {
            bundles_sent: first_sequence,
            slots: Slots::new(),
        };
        self.spawn(arrival, motion, replication, now_ms, admit.handoffs, now_ms);
        self.arena_arrived(admit.entity);
        self.transmog_arrived(admit.entity, admit.login);
        if admit.login {
            // Retail's login state lists the loot limit counts and the recipe cooldowns.
            self.send_loot_limits(admit.entity, loot_bag::LimitsSent::Spawn);
            self.send_recipe_cooldowns(admit.entity, loot_bag::LimitsSent::Spawn);
            self.send_faction(admit.entity);
        }
        if admit.login && self.is_tutorial() {
            self.start_tutorial(admit.entity, now_ms);
        } else {
            for (objective, run) in &admit.quests.active {
                self.resume_quest(admit.entity, objective, run, now_ms);
            }
        }
        Ok(login)
    }

    /// Takes the player of `offer` from another sim, with the same entity number, bundle
    /// sequence and movement budget. A player that enters this level from another one (the end
    /// of the tutorial) gets the level change messages, which this returns, the next
    /// client-context id, and a new spawn as at a login; it lands on the ground.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError`] as [`SimWorld::admit`] does.
    pub fn take(
        &mut self,
        offer: &HandoffOffer,
        now_ms: u64,
        server_time: u64,
    ) -> Result<Option<Outgoing>, WorldError> {
        // The player keeps the id the giver's client knows it by.
        if let Err(refusal) = nw_messages::adopt_raw_id(self.raw_ids, offer.entity, offer.gde_ref) {
            tracing::warn!(player = offer.entity, %refusal, "handoff id not adopted");
        }
        let enters = offer.enter_level;
        let position = Position::from(offer.position);
        let client_context = if enters {
            offer.client_context.saturating_add(1)
        } else {
            offer.client_context
        };
        let level_info = if enters {
            Some(self.entry_level_info(offer, client_context, server_time)?)
        } else {
            None
        };
        let flow = match &level_info {
            Some(level_info) => LevelFlow::after_level_info(
                offer.entity,
                level_info.clone(),
                now_ms,
                offer.phasing_sequence,
                &self.achievement_rules,
                self.settings.level.private,
                offer.backstory.as_deref(),
                &offer.progression.achievements,
                offer.achievement_version,
            ),
            None => LevelFlow::resumed(
                offer.entity,
                offer.phasing_sequence,
                &offer.progression.achievements,
                offer.achievement_version,
            ),
        };
        let arrival = Arrival {
            connection: offer.connection.clone(),
            character: offer.character,
            name: offer.name.clone(),
            look: offer.look.map(PlayerLook::of_choices),
            backstory: offer.backstory.clone(),
            number: offer.entity,
            position: if enters {
                self.settings.limits.onto_ground(&*self.ground, position)
            } else {
                position
            },
            health: Health::at(
                offer.health,
                self.player_max_health(offer.progression.experience, &offer.progression.attributes),
            ),
            // A handed-off player was already in the world; one that enters a new level spawns
            // again once its client has loaded it.
            spawned: !enters,
            own_card_due_ms: if enters {
                now_ms.saturating_add(OWN_CARD_DELAY_MS)
            } else {
                now_ms
            },
            login: false,
            client_context,
            flow,
            items: offer.items.clone(),
            inventory: offer.inventory.clone(),
            drawn_weapon: offer.drawn_weapon,
            active_weapon: offer.active_weapon,
            progression: offer.progression.clone(),
            saved: false,
            mastery_versions: offer.mastery_versions,
            attribute_version: offer.attribute_version,
            fishing_sequence: offer.fishing_sequence,
            game_master: offer.game_master,
            account_in_probation: offer.account_in_probation,
            account: offer.account,
            entitlements: offer.entitlements.iter().cloned().collect(),
            quests: offer.quests.clone(),
            pvp_flagged: offer.pvp_flagged,
            auto_pin: offer.auto_pin,
        };
        self.check(&arrival)?;
        let level_change = if let Some(level_info) = &level_info {
            let envelopes = level_change_messages(offer.entity, level_info)?;
            Some(Outgoing {
                connection: offer.connection.clone(),
                channel: MESSAGE_CHANNEL,
                reliable: true,
                envelopes,
            })
        } else {
            None
        };
        let state = MotionState {
            position: arrival.position,
            move_age_ms: offer.move_age_ms,
            budget_m: offer.budget_m,
        };
        let motion = Motion::import(state, now_ms, &self.settings.limits);
        let retry_at = now_ms.saturating_add(self.settings.handoff_retry_ms);
        // A new level starts with no entity slots on the client.
        let slots = if enters {
            Slots::new()
        } else {
            Slots::restored(
                offer.slots.iter().copied().collect(),
                offer.pending_stops.clone(),
            )
        };
        let replication = Replication {
            // A new client-context starts its bundle sequence as a login does.
            // Carrying the tutorial's sequence (live 6077) onto context 2 failed
            // StateReplication at InGame (Game.log 2026-09-24).
            bundles_sent: if enters {
                FIRST_LOGIN_BUNDLE_SEQUENCE
            } else {
                offer.bundles_sent
            },
            slots,
        };
        self.spawn(
            arrival,
            motion,
            replication,
            now_ms.saturating_sub(offer.time_synch_age_ms),
            offer.handoffs,
            retry_at,
        );
        self.arena_arrived(offer.entity);
        self.transmog_arrived(offer.entity, false);
        if enters {
            // A new level's spawn state lists them as a login's does (Inferred).
            self.send_loot_limits(offer.entity, loot_bag::LimitsSent::Spawn);
            self.send_recipe_cooldowns(offer.entity, loot_bag::LimitsSent::Spawn);
            self.send_faction(offer.entity);
        }
        // From the tutorial only: an instance move keeps the quests (its offer has a stay).
        if enters
            && !self.is_tutorial()
            && !self.instance_sim
            && offer.progression.arena.stay.is_none()
        {
            // Retail: the first open-world state lists the tutorial as completed.
            if let Some(entity) = self.player_by_number(offer.entity)
                && let Some(mut player) = self.world.get_mut::<Player>(entity)
            {
                player.done_quests.push(TUTORIAL_OBJECTIVE.to_owned());
            }
            if let Err(refusal) = self.start_quest(offer.entity, OPEN_WORLD_OBJECTIVE, now_ms) {
                tracing::debug!(player = offer.entity, ?refusal, "no open-world quest");
            }
        } else {
            for (objective, run) in &offer.quests.active {
                self.resume_quest(offer.entity, objective, run, now_ms);
            }
        }
        // A player that comes into a running instance enters its game mode (`TaskGameMode`).
        if let Some(mode) = self
            .instance_run
            .as_ref()
            .map(|run| run.game_mode().to_owned())
        {
            self.quest_event(offer.entity, Event::GameModeEntered(&mode), now_ms);
            self.update_quest_tracker(offer.entity);
        }
        Ok(level_change)
    }

    /// Notes that the client of `connection` is in the game: it sent its first time sync
    /// request after the own spawn card. That request comes in the same moment the client
    /// logs `player spawn succeeds, in game` (live 2026-09-10 and 2026-09-18; retail beach
    /// capture 2026-09-27: the request, then the bundle stream starts). So that message ends
    /// the hold on the replication stream and the spawn protection. Other messages do not:
    /// after a level change the client sends its teardown calls (4143, 3970 and more) while
    /// it unloads the old level, and mob cards sent then show a name plate without a model
    /// (live 2026-09-18). Movement input does not count either: on a fresh connection the
    /// client sends it before the new level has loaded (live 2026-09-22, the beach switch:
    /// the card flood it let loose reached a client with no level context, which dropped the
    /// bundles and failed the first bundle it read after the load). Before the card the
    /// request means nothing, and a player who already spawned is not changed.
    pub fn heard_from(&mut self, connection: &ConnectionRef) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        if !self
            .world
            .get::<Player>(entity)
            .is_some_and(|player| player.own_card_sent)
        {
            return;
        }
        let was_spawned = self
            .world
            .get::<Vitals>(entity)
            .is_some_and(|vitals| vitals.spawned);
        if let Some(mut vitals) = self.world.get_mut::<Vitals>(entity) {
            vitals.spawned = true;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            if !was_spawned && player.login {
                player.others_due_ms = Some(OTHER_ENTITY_SETTLE_PENDING);
            }
            if !was_spawned && let Some(quest) = player.quest.as_mut() {
                quest.in_game();
            }
            // The season states (retail sends both in one bundle) start the client's pin of the
            // tracked quest.
            if !was_spawned {
                player.extra_members.extend([
                    nw_messages::seasons::seasons_rewards_initialized(),
                    nw_messages::seasons::seasons_tracked_stats_initialized(),
                ]);
                // Whether the account is in probation, which blocks global and trade chat:
                // retail sent `accountInProbation` 1.9 to 9.4 s after every captured spawn,
                // apart from the own card.
                let in_probation = player.account_in_probation;
                nw_messages::put_account_in_probation(&mut player.extra_members, in_probation);
            }
            player.own_card_bundle = None;
        }
    }

    /// Moves the player of `connection` to `next`, received at `now_ms`, when the movement checks
    /// (M4 spec section 7.2 with sections 15 and 16) allow it.
    ///
    /// # Errors
    ///
    /// Returns [`MoveError`] when there is no such player, it is frozen, or a check refuses the
    /// move; then the player keeps its last good position. When moves stay refused for
    /// [`REFUSED_RESYNC_MS`] the player is moved back to its last good position on or above the
    /// terrain.
    pub fn move_to(
        &mut self,
        connection: &ConnectionRef,
        next: Position,
        now_ms: u64,
    ) -> Result<GroundCheck, MoveError> {
        let entity = *self
            .by_connection
            .get(connection)
            .ok_or(MoveError::NotInWorld)?;
        if self
            .world
            .get::<Player>(entity)
            .is_some_and(|player| player.frozen)
        {
            return Err(MoveError::Frozen);
        }
        if self
            .world
            .get::<Player>(entity)
            .is_some_and(|player| player.push.is_some())
        {
            return Err(MoveError::Pushed);
        }
        // A move does not end the spawn protection: on a fresh connection the client sends
        // movement before it has loaded the level (live 2026-09-22, the beach switch), so
        // only the time sync request after the own card marks the spawn
        // ([`Self::heard_from`], see [`attack::Vitals`]).
        if self
            .world
            .get::<Vitals>(entity)
            .is_some_and(|vitals| vitals.health.is_dead())
        {
            return Err(MoveError::Dead);
        }
        let limits = self.move_limits_of(entity);
        let (result, last_good) = {
            let mut body = self
                .world
                .get_mut::<Body>(entity)
                .ok_or(MoveError::NotInWorld)?;
            (
                body.motion
                    .try_move(&limits, &*self.ground, &self.settings.bounds, next, now_ms),
                body.motion.position(),
            )
        };
        let grounded = self
            .ground
            .height_at(next.x, next.y)
            .is_none_or(|height| next.z >= height - GROUNDED_SLACK_M);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return result.map_err(MoveError::Refused);
        };
        match result {
            Err(refusal) => {
                let since = player.refused_since_ms.get_or_insert(now_ms);
                if now_ms.saturating_sub(*since) < REFUSED_RESYNC_MS {
                    return Err(MoveError::Refused(refusal));
                }
                player.refused_since_ms = None;
                let back = player.last_grounded.unwrap_or(last_good);
                tracing::debug!(
                    player = player.number,
                    x = back.x,
                    y = back.y,
                    z = back.z,
                    "moves refused too long: player moved back"
                );
                self.place_player(entity, back, None, Placement::Stand, now_ms);
                Err(MoveError::Refused(refusal))
            }
            Ok(check) => {
                player.refused_since_ms = None;
                if grounded {
                    player.last_grounded = Some(next);
                }
                Ok(check)
            }
        }
    }

    /// Moves the player of `connection` by the new records of its input `records` (newest first,
    /// as the client sends them), received at `now_ms`: the server moves the body by each
    /// record's keys and holds it to the record's camera ([`nw_sim_core::TrackedBody`],
    /// [`crate::camera`]), then checks the place ([`Self::move_to`]). A record not newer than the
    /// last one read is an old copy. While the inventory camera holds the client's camera, the
    /// records move nothing (retail held the player still). A client that sends no view angles
    /// (the test client, the load-test bots, which number no records) moves the body to each of
    /// its camera points. The places the bodies stand on are walked floors (where a push may go).
    /// Gives the result and the place checked; `None` when no record was new or there is no such
    /// player.
    pub fn move_from_input(
        &mut self,
        connection: &ConnectionRef,
        records: &[InputRecord],
        now_ms: u64,
    ) -> Option<(Result<GroundCheck, MoveError>, Position)> {
        let entity = *self.by_connection.get(connection)?;
        let server_at = self.world.get::<Body>(entity)?.motion.position();
        let views = crate::camera::record_views(records);
        let combat = Arc::clone(&self.combat.data);
        let mut player = self.world.get_mut::<Player>(entity)?;
        let with_solids = player
            .solids
            .clone()
            .map(|solids| LevelGround::new(&*self.ground, solids));
        let ground: &dyn Ground = match &with_solids {
            Some(ground) => ground,
            None => &*self.ground,
        };
        let held = player
            .camera_held_until_ms
            .is_some_and(|until| now_ms < until);
        // The weapon in the hand picks the slayer sequences the server starts ([`slayer`]).
        let weapon = player.gear.as_ref().and_then(|gear| {
            let (item, _) = gear.main_weapon()?;
            Some(slayer::HeldWeapon {
                stance: combat.weapon_of_item(item)?.stance,
                drawn: gear.drawn().is_some(),
                empty: gear.held_empty(),
            })
        });
        if let Some(load) = player.gear.as_ref().and_then(|gear| {
            self.content.equip_load(
                EQUIP_LOAD_SLOTS
                    .iter()
                    .filter_map(|slot| gear.worn_item(*slot)),
            )
        }) {
            player.tracked.set_equip_load(load);
        }
        if player.tracked.position() != server_at {
            // The server moved the player, or kept it at its last good place.
            player.tracked.stand_at(server_at);
        }
        let mut next = None;
        let mut walked = Vec::new();
        let mut read_any = false;
        for (record, seen) in records.iter().zip(views).rev() {
            let Some(mut input) = crate::camera::track_input(record, seen) else {
                // No view angles: the test client's point, with no counter (0 in every record).
                if let Some(camera) = seen.camera {
                    read_any = true;
                    if !held {
                        player.tracked.stand_at(camera);
                        walked.push(camera);
                        next = Some(camera);
                    }
                }
                continue;
            };
            let counter = record.u16_after_mask;
            if player
                .input_counter
                .is_some_and(|last| !attack::counter_is_newer(counter, last))
            {
                continue;
            }
            player.input_counter = Some(counter);
            read_any = true;
            // A cutscene's or a video's camera shows the scene, not the player: it carries nothing.
            if player.flow.camera_in_scene() {
                input.view = None;
            }
            let swimming = player.tracked.is_swimming();
            let falling = player.tracked.is_falling();
            player.slayer.record(
                slayer::SlayerRecord {
                    counter: own_state_counter(counter),
                    frame_s: input.character.dt_s,
                    action: record.slayer_input(),
                    intent: (!held).then_some(input.character.intent),
                    attack: slayer::AttackKey {
                        pressed: !held && record.attack_pressed(),
                        released: !held && record.attack_released(),
                    },
                    weapon,
                    swimming,
                    falling,
                },
                combat.player_sequences(),
            );
            if held {
                // The inventory camera swings round to the front of the body: its view is not
                // the player's look. Its look in the next own state turned the player at once,
                // and the client closed the inventory or the loot 40 to 60 ms after it opened
                // (run 17). Retail's first own states after an open kept the rotation; it turned
                // only from 200 ms on, in small steps (newchar 2026-09-18, the open at
                // 1792695864606: `rotation` 46e4, 46de, 46d5, ... 4603 over 450 ms).
                continue;
            }
            player.view = seen.view;
            let tracked = player
                .tracked
                .record(input, ground, self.settings.ocean_level_m);
            if tracked.consistent && tracked.on_floor {
                walked.push(tracked.position);
            }
            next = Some(tracked.position);
        }
        if held && read_any {
            return Some((Ok(GroundCheck::Checked), server_at));
        }
        let next = next?;
        for at in walked {
            self.walked_floor.walked(at);
        }
        self.sheathe_in_deep_water(entity, next, now_ms);
        // Retail sends the lock id on row 3 of the newest input while the lock is held, including
        // movement records that are not an attack.
        if let Some(target) = records.first().and_then(InputRecord::lock_target)
            && let Some(mut vitals) = self.world.get_mut::<Vitals>(entity)
        {
            vitals.set_lock_target(Some(target));
        }
        let result = self.move_to(connection, next, now_ms);
        if let Some(footing) = self.footing_of(entity)
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
        {
            player.footing = footing;
        }
        Some((result, next))
    }

    /// The ground and water under player `entity`'s body where the server has it, over its
    /// ground with the solids shown to it ([`Footing`]): what its next own state tells its
    /// client when it changed.
    fn footing_of(&self, entity: Entity) -> Option<Footing> {
        let at = self.world.get::<Body>(entity)?.motion.position();
        let solids = self.world.get::<Player>(entity)?.solids.clone();
        let sea_level_m = self.settings.ocean_level_m;
        Some(match solids {
            Some(solids) => Footing::of(&LevelGround::new(&*self.ground, solids), at, sea_level_m),
            None => Footing::of(&*self.ground, at, sea_level_m),
        })
    }

    /// The server puts player `entity` at `to` at `now_ms`, facing `facing` when given: its
    /// next own state moves its client there ([`OwnTeleport`]), a push on it ends, and its body
    /// lands as `placement` says.
    fn place_player(
        &mut self,
        entity: Entity,
        to: Position,
        facing: Option<f32>,
        placement: Placement,
        now_ms: u64,
    ) {
        if let Some(mut body) = self.world.get_mut::<Body>(entity) {
            body.motion = Motion::new(to, now_ms, &self.settings.limits);
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.moved = true;
            player.move_facing = facing;
            player.push = None;
            match placement {
                Placement::Stand => player.tracked.stand_at(to),
                Placement::Drop => player.tracked.drop_at(to),
            }
        }
    }

    /// Removes a player.
    pub fn leave(&mut self, connection: &ConnectionRef) -> Option<Departure> {
        let entity = *self.by_connection.get(connection)?;
        let departure = self.departure(entity);
        self.remove_player(connection, entity);
        departure
    }

    /// Takes the player `entity` of `connection` out of this world, with its tutorial.
    fn remove_player(&mut self, connection: &ConnectionRef, entity: Entity) {
        self.by_connection.remove(connection);
        if let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) {
            nw_messages::release_raw_id(self.raw_ids, number);
            self.quests.remove(&number);
            self.economy_left(number);
            if let Some(tutorial) = self.tutorials.remove(&number) {
                tutorial.end(&mut self.mobs);
            }
        }
        self.despawn_camp(entity);
        self.world.despawn(entity);
    }

    /// Removes every player of a gateway.
    pub fn leave_gateway(&mut self, gateway: &BusId) -> Vec<Departure> {
        let connections: Vec<ConnectionRef> = self
            .by_connection
            .keys()
            .filter(|connection| connection.gateway == *gateway)
            .cloned()
            .collect();
        self.leave_all(&connections)
    }

    /// Removes a player that this sim took by the handoff `handoffs` that the sender then
    /// cancelled. The sender kept the player, so nothing is saved here. A player of another
    /// attempt stays. Returns whether it was removed.
    pub fn cancel(&mut self, connection: &ConnectionRef, handoffs: u64) -> bool {
        let Some(&entity) = self.by_connection.get(connection) else {
            return false;
        };
        let same_attempt = self
            .world
            .get::<Player>(entity)
            .is_some_and(|player| player.handoffs == handoffs);
        if same_attempt {
            self.remove_player(connection, entity);
        }
        same_attempt
    }

    /// The answer to the handoff of the player of `connection` at `now_ms`.
    pub fn handoff_done(
        &mut self,
        connection: &ConnectionRef,
        accepted: bool,
        now_ms: u64,
    ) -> HandoffEnd {
        let Some(&entity) = self.by_connection.get(connection) else {
            return HandoffEnd::Gone;
        };
        if accepted {
            self.remove_player(connection, entity);
            return HandoffEnd::Moved;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.frozen = false;
            player.no_handoff_before_ms = now_ms.saturating_add(self.settings.handoff_retry_ms);
        }
        HandoffEnd::Kept
    }

    /// Takes a new cell table. When it does not name this sim with its generation, the sim owns
    /// no cell and every player leaves (M5 spec section 3.6); their departures are returned.
    ///
    /// # Errors
    ///
    /// Returns [`TableError::WrongSize`] for a table that does not fit the level; the last table
    /// stays.
    pub fn set_table(&mut self, table: &CellTable) -> Result<Vec<Departure>, TableError> {
        if self.applied_table.as_ref() == Some(table) {
            return Ok(Vec::new());
        }
        let departures = self.apply_table(table)?;
        self.applied_table = Some(table.clone());
        Ok(departures)
    }

    /// Applies the new cell table `table` ([`SimWorld::set_table`]).
    fn apply_table(&mut self, table: &CellTable) -> Result<Vec<Departure>, TableError> {
        if self.instance_sim {
            // An instance sim owns its whole level for its instance; from the table it needs
            // only the open-world sims its exits hand players to (looked up at each exit).
            self.mob_changes.add(self.mobs.set_cells(|_| true));
            return Ok(Vec::new());
        }
        if self.is_tutorial() {
            // A tutorial sim owns its whole private level and is in no open-world table; from the
            // table it needs only the sim its exit hands players to.
            self.mob_changes.add(self.mobs.set_cells(|_| true));
            self.exit_target = owner_in(table, OPEN_WORLD_START);
            return Ok(Vec::new());
        }
        let bounds = self.settings.bounds;
        let grid = CellGrid::new(bounds.max_x, bounds.max_y, table.cell_size_m)
            .ok()
            .filter(|grid| grid.columns() == table.columns && grid.rows() == table.rows)
            .ok_or(TableError::WrongSize)?;
        let owners =
            CellOwners::from_cells(grid, table.owners.clone()).ok_or(TableError::WrongSize)?;
        let me = table
            .sims
            .iter()
            .position(|slot| *slot == self.me)
            .and_then(|index| u16::try_from(index).ok());
        let Some(me) = me else {
            self.cells = None;
            self.mob_changes.add(self.mobs.set_cells(|_| false));
            self.drop_foreign_gm();
            let everyone: Vec<ConnectionRef> = self.by_connection.keys().cloned().collect();
            return Ok(self.leave_all(&everyone));
        };
        self.mob_changes.add(
            self.mobs
                .set_cells(|position| owners.owner_at(position) == Some(me)),
        );
        self.cells = Some(Cells {
            owners,
            sims: table.sims.clone(),
            me,
        });
        self.drop_foreign_gm();
        Ok(Vec::new())
    }

    /// The ghosts of another sim, received at `now_ms`; positions that are not finite are left
    /// out.
    pub fn set_ghosts(&mut self, from: &BusId, entities: Vec<Ghost>, now_ms: u64) {
        if *from == self.me.id {
            return;
        }
        let entities = entities
            .into_iter()
            .filter(|ghost| ghost.position.is_finite())
            .map(|ghost| ReceivedGhost {
                entity: ghost.entity,
                position: Position::from(ghost.position),
                mob: ghost.mob,
                player: ghost.player,
            })
            .collect();
        self.ghosts.insert(
            from.clone(),
            ReceivedGhosts {
                received_ms: now_ms,
                entities,
            },
        );
    }

    /// Holds the owner's raw id of each ghost in `gde_refs` (another sim's
    /// [`nw_bus::Ghosts::gde_refs`]),
    /// so this sim's clients see the entity by the same id. A ghost whose owner gave it a new id
    /// (a respawn) gets a new card in every view.
    pub fn adopt_ghost_ids(&mut self, gde_refs: &[(u64, [u8; nw_messages::RAW_ID_LEN])]) {
        for &(entity, gde_ref) in gde_refs {
            match nw_messages::adopt_raw_id(self.raw_ids, entity, gde_ref) {
                Ok(adopted) => {
                    self.ghost_ids.insert(entity);
                    if adopted == Adopted::Renewed {
                        self.recard(entity);
                    }
                }
                Err(refusal) => tracing::warn!(entity, %refusal, "ghost id not adopted"),
            }
        }
    }

    /// Releases the ids of the ghosts that no other sim sends any more, unless the entity is this
    /// sim's own now (a player it took).
    fn release_ghost_ids(&mut self) {
        let sent: HashSet<u64> = self
            .ghosts
            .values()
            .flat_map(|ghosts| &ghosts.entities)
            .map(|ghost| ghost.entity)
            .collect();
        let gone: Vec<u64> = self
            .ghost_ids
            .iter()
            .copied()
            .filter(|entity| !sent.contains(entity))
            .collect();
        for entity in gone {
            self.ghost_ids.remove(&entity);
            let own = self.mobs.health(entity).is_some()
                || self
                    .world
                    .query::<&Player>()
                    .iter(&self.world)
                    .any(|player| player.number == entity);
            if !own {
                nw_messages::release_raw_id(self.raw_ids, entity);
            }
        }
    }

    /// The ids of the mobs that left end with them; a respawned mob is a new spawn with a new
    /// id, and every client that had its old card stops that slot and gets a new card (GDE id
    /// note, "Dynamic entities": retail gave 654 respawns a new `GdeRef` and card, and never
    /// showed a dead mob's id alive again). A mob that fell asleep starts a new timeline: when a
    /// player comes near again it plays `Spawn` ([`crate::mobs::Mobs::take_slept`]).
    fn end_mob_ids(&mut self) {
        for entity in self.mobs.take_left() {
            nw_messages::release_raw_id(self.raw_ids, entity);
        }
        for entity in self.mobs.take_respawned() {
            nw_messages::renew_raw_id(entity);
            self.recard(entity);
        }
        for entity in self.mobs.take_slept() {
            self.mob_timelines.forget(entity);
        }
    }

    /// Every client that has a card of `entity` stops its slot and gets a new card in a new slot,
    /// with the entity's views shown afresh.
    fn recard(&mut self, entity: u64) {
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            player.slots.restart(entity);
        }
        self.mob_timelines.forget(entity);
        self.mob_health_shown.remove(&entity);
        self.look_targets.remove(&entity);
        self.boss_phases_shown.remove(&entity);
    }

    /// The ghost `entity` and the sim it belongs to.
    fn ghost(&self, entity: u64) -> Option<(&BusId, ReceivedGhost)> {
        self.ghosts.iter().find_map(|(owner, ghosts)| {
            ghosts
                .entities
                .iter()
                .find(|ghost| ghost.entity == entity)
                .map(|ghost| (owner, *ghost))
        })
    }

    /// The admin snapshot: every player of this sim, every awake mob, and mobs by state (admin
    /// tools spec section 5).
    pub fn snapshot(&mut self) -> (Vec<SnapPlayer>, Vec<SnapMob>, [u32; MOB_STATES]) {
        let mut query = self.world.query::<(&Player, &Body, &Vitals)>();
        let players = query
            .iter(&self.world)
            .map(|(player, body, vitals)| {
                let at = body.motion.position();
                SnapPlayer {
                    entity: player.number,
                    name: player.name.clone(),
                    x: at.x,
                    y: at.y,
                    z: at.z,
                    dead: vitals.health.is_dead(),
                    frozen: player.frozen,
                    health: vitals.health.current(),
                    max_health: vitals.health.max(),
                    target_entity: None,
                }
            })
            .collect();
        let (mobs, counts) = self.mobs.snapshot();
        (players, mobs, counts)
    }

    /// Every awake mob of this sim and every mob ghost: entity number, position and state. An
    /// asleep mob has no player within its wake radius, so it is in no player's view or reach.
    pub(super) fn mob_states(&self) -> impl Iterator<Item = (u64, Position, MobGhost)> + '_ {
        self.ghosts
            .values()
            .flat_map(|ghosts| &ghosts.entities)
            .filter_map(|ghost| ghost.mob.map(|state| (ghost.entity, ghost.position, state)))
            .chain(self.mobs.awake_states())
    }

    /// What the cell tables did to the mobs since the last call.
    pub fn take_mob_changes(&mut self) -> MobChanges {
        std::mem::take(&mut self.mob_changes)
    }

    /// What the players did that leaderboard stats count, since the last call.
    pub fn take_stat_events(&mut self) -> Vec<StatEvent> {
        std::mem::take(&mut self.stat_events)
    }

    /// Brain steps and player wake scans of the mobs since the start.
    pub const fn mob_work(&self) -> (u64, u64) {
        (self.mobs.brain_runs(), self.mobs.wake_scans())
    }

    /// Mobs by state.
    pub fn mob_counts(&self) -> [(MobState, usize); MobState::ALL.len()] {
        self.mobs.counts()
    }

    /// Ghosts of other sims.
    pub fn ghosts(&self) -> usize {
        self.ghosts
            .values()
            .map(|ghosts| ghosts.entities.len())
            .sum()
    }

    /// Every player's character and position, for the periodic save, with the items that
    /// changed since the last save.
    pub fn positions(&mut self) -> Vec<Departure> {
        self.positions_where(|_| true)
    }

    /// The departures of the players of `characters` with what changed since their last save
    /// ([`SimWorld::positions`]).
    pub fn positions_of(&mut self, characters: &[Uuid]) -> Vec<Departure> {
        self.positions_where(|character| characters.contains(&character))
    }

    /// The departures of the players whose character `picked` picks, with what changed since
    /// their last save.
    fn positions_where(&mut self, picked: impl Fn(Uuid) -> bool) -> Vec<Departure> {
        let saves = !self.is_tutorial();
        let quests = &self.quests;
        let now_ms = self.last_now_ms;
        let mut query = self.world.query::<(&mut Player, &Body)>();
        query
            .iter_mut(&mut self.world)
            .filter(|(player, _)| picked(player.character))
            .map(|(mut player, body)| {
                let items = player
                    .gear
                    .as_mut()
                    .filter(|_| saves)
                    .and_then(Gear::take_unsaved);
                let changed = saves && std::mem::take(&mut player.quests_unsaved);
                let saved_quests = changed.then(|| quests_of(quests, now_ms, &player));
                let progression = saves.then(|| player.take_unsaved_progression()).flatten();
                departure_of(&player, body, items, saved_quests, progression)
            })
            .collect()
    }

    /// Every player's departure with all its items, for the saves at shutdown.
    pub fn departures(&self) -> Vec<Departure> {
        self.by_connection
            .values()
            .filter_map(|entity| self.departure(*entity))
            .collect()
    }

    /// One tick: handoffs to start, ghost lists for other sims, a bundle for every player that
    /// is not frozen, and a `TimeSynchMsg` when one is due.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a message cannot be encoded.
    pub fn tick(&mut self, now_ms: u64, server_time: u64) -> Result<TickOutput, MessageError> {
        self.last_now_ms = now_ms;
        let ghosts_started = Instant::now();
        let ttl = self.settings.ghost_ttl_ms;
        self.ghosts
            .retain(|_, ghosts| now_ms.saturating_sub(ghosts.received_ms) <= ttl);
        self.release_ghost_ids();
        let handoffs = self.start_handoffs(now_ms);
        let ghosts = self.ghosts_out();
        let ghost_seconds = ghosts_started.elapsed().as_secs_f64();
        let mobs_started = Instant::now();
        let mut swings = self.tick_mobs(now_ms);
        self.end_mob_ids();
        swings.append(&mut self.scripted_swings);
        let intents = self.swings_landing(swings, now_ms, server_time);
        let mut mob_hits = self.mob_hits(intents, now_ms);
        mob_hits.extend(self.mob_shots_due(now_ms));
        let mob_seconds = mobs_started.elapsed().as_secs_f64();
        let gameplay_started = Instant::now();
        let places: Vec<(u64, Position)> =
            self.mob_states().map(|(mob, at, _)| (mob, at)).collect();
        self.mob_trails.record(now_ms, places.into_iter());
        let mut player_attacks = self.player_swings_due(now_ms);
        self.tick_tutorials(now_ms);
        self.tick_quests(now_ms);
        self.detect_quest_objects(now_ms);
        self.tick_events(now_ms);
        self.tick_dungeon_script(now_ms);
        self.tick_scripts(now_ms);
        self.drink_due(now_ms, server_time);
        self.tick_item_effects(server_time);
        self.tick_stamina(now_ms);
        self.server_ms = server_time;
        self.tick_max_health();
        self.tick_status_effects(now_ms);
        self.tick_deaths_door(now_ms);
        self.tick_falls(now_ms);
        self.tick_mounts(now_ms);
        self.tick_quick_courses(now_ms);
        self.tick_hands(now_ms, server_time);
        self.tick_pushes(now_ms);
        self.gathers_due(now_ms);
        self.bury_corpses(now_ms);
        self.bags.tick(now_ms);
        self.loot_rights_due(server_time);
        self.crafts_due(now_ms);
        self.tick_fishing(now_ms);
        self.tick_music(now_ms);
        player_attacks.extend(self.tick_abilities(now_ms));
        self.tick_meta_achievements();
        self.tick_travel(now_ms);
        self.tick_phasing();
        let gameplay_seconds = gameplay_started.elapsed().as_secs_f64();
        let bundles_started = Instant::now();
        let mut messages = self.take_messages();
        let bundled = self.bundles(now_ms, server_time)?;
        messages.extend(bundled.outgoing);
        messages.append(&mut self.after_bundles);
        let parts = TickParts {
            ghosts: ghost_seconds,
            mobs: mob_seconds,
            gameplay: gameplay_seconds,
            cards: bundled.cards_seconds,
            bundles: bundles_started.elapsed().as_secs_f64() - bundled.cards_seconds,
        };
        Ok(TickOutput {
            messages,
            handoffs,
            ghosts,
            mob_hits,
            player_attacks,
            deaths: std::mem::take(&mut self.deaths),
            script_log: self.take_script_log(),
            parts,
            bundle_members: bundled.members,
        })
    }

    /// Runs the mobs with the living players of this sim and the player ghosts of others.
    fn tick_mobs(&mut self, now_ms: u64) -> Vec<MobAttackIntent> {
        let mut seen: Vec<Seen> = Vec::new();
        let mut query = self.world.query::<(&Player, &Body, &Vitals)>();
        for (player, body, vitals) in query.iter(&self.world) {
            // A downed player stays a target: a mob's hit finishes it (retail `NH-MSQ-5.1`).
            if vitals.health.is_dead() && !vitals.downed() {
                continue;
            }
            seen.push(Seen {
                entity: player.number,
                position: body.motion.position(),
            });
        }
        let ghosts = self.ghosts.values().flat_map(|ghosts| &ghosts.entities);
        // A ghost player that died is no one's target either.
        for ghost in ghosts.filter(|ghost| {
            ghost.mob.is_none() && ghost.player.as_ref().is_none_or(|player| player.health > 0)
        }) {
            seen.push(Seen {
                entity: ghost.entity,
                position: ghost.position,
            });
        }
        self.mobs
            .tick(&seen, now_ms, &*self.ground, self.settings.tick_threads)
    }

    /// Starts the tutorial of player `number` at `now_ms`; without the quest in the content
    /// there is none, and the tutorial ends at its exit.
    fn start_tutorial(&mut self, number: u64, now_ms: u64) {
        let Some(quest) = self.content.tutorial_quest.clone() else {
            return;
        };
        let (mut tutorial, orders) = Tutorial::start(
            number,
            quest,
            &self.content.tutorial_level,
            &mut self.mobs,
            now_ms,
        );
        // The expansion is in the player's first achievement state.
        if let Some(bit) = self.achievement_rules.expansion() {
            tutorial.achievement(bit, now_ms);
        }
        if let Some(old) = self.tutorials.insert(number, tutorial) {
            old.end(&mut self.mobs);
        }
        self.carry_out(number, orders, now_ms);
    }

    /// Runs the tutorials with their players' positions.
    fn tick_tutorials(&mut self, now_ms: u64) {
        let numbers: Vec<u64> = self.tutorials.keys().copied().collect();
        for number in numbers {
            let position = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<Body>(entity))
                .map(|body| body.motion.position());
            let alive = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<Vitals>(entity))
                .is_some_and(|vitals| !vitals.health.is_dead());
            let (Some(position), Some(tutorial)) = (position, self.tutorials.get_mut(&number))
            else {
                continue;
            };
            tutorial.player_alive(alive);
            let mut orders =
                tutorial.tick(&mut self.mobs, &self.content.kill_types, position, now_ms);
            // Items the player already holds meet the item tasks that start later.
            let carried = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<Player>(entity))
                .and_then(|player| player.gear.as_ref().map(Gear::carried))
                .unwrap_or_default();
            if let Some(tutorial) = self.tutorials.get_mut(&number) {
                orders.extend(tutorial.sync_held(&held_items(&carried), &mut self.mobs, now_ms));
            }
            self.carry_out(number, orders, now_ms);
            self.update_quest_tracker(number);
        }
    }

    /// The running quests of player `number`.
    fn quests_of_player(&self, number: u64) -> &[ActiveQuest] {
        self.quests.get(&number).map_or(&[][..], Vec::as_slice)
    }

    /// The journal of player `entity`'s client, made when it has none yet: its lists go on above
    /// the ones the client has, after the objectives the player finished.
    fn journal_of(player: &mut Player) -> &mut QuestTracker {
        let number = player.number;
        let shown = player.tracker_shown();
        let pin = player.auto_pin;
        let done: Vec<&str> = player.done_quests.iter().map(String::as_str).collect();
        let fresh = QuestTracker::new(number)
            .after(&done)
            .above(shown)
            .with_auto_pin(pin);
        player.quest.get_or_insert(fresh)
    }

    /// Gives the journal of player `number` its tutorial's or its quests' task lists; a quest
    /// that finished is completed, unless it waits for its turn-in at its NPC (retail: the
    /// client asks to complete it, 794, when the player talks to the NPC).
    fn update_quest_tracker(&mut self, number: u64) {
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let lists: Vec<(String, Vec<TaskProgress>, bool)> =
            if let Some(tutorial) = self.tutorials.get(&number) {
                vec![(
                    TUTORIAL_OBJECTIVE.to_owned(),
                    tutorial.progress(),
                    tutorial.finished(),
                )]
            } else {
                self.quests_of_player(number)
                    .iter()
                    .map(|quest| {
                        let waits = quest.def.turn_in.is_some();
                        (
                            quest.def.id.clone(),
                            quest.run.progress(),
                            quest.run.finished() && !waits,
                        )
                    })
                    .collect()
            };
        if lists.is_empty() {
            return;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            let journal = Self::journal_of(&mut player);
            for (objective, tasks, finished) in &lists {
                journal.update(objective, tasks.clone(), *finished);
            }
        }
        if self.tutorials.contains_key(&number) {
            return;
        }
        for (objective, _, finished) in &lists {
            if *finished {
                self.complete_quest(number, objective);
            }
        }
    }

    /// Player `number` accepts quest `objective` at an NPC (3772): it starts when the journal
    /// holds fewer quests of its kind than the kind's limit (`javelin.objectives-max-*`, the
    /// client checks the same limit before it asks). A quest the game starts by itself, as the
    /// next step of a chain, goes through [`SimWorld::start_quest`] and is never refused by a
    /// limit, so a full journal cannot break a chain.
    pub(crate) fn accept_quest(
        &mut self,
        number: u64,
        objective: &str,
        take: Option<MissionTake>,
        now_ms: u64,
    ) -> Result<(), QuestRefusal> {
        let def = self
            .content
            .quests
            .quest(objective)
            .ok_or(QuestRefusal::NoQuest)?;
        let faction = self
            .player_by_number(number)
            .and_then(|entity| self.world.get::<Player>(entity))
            .map_or(nw_content::factions::NO_FACTION, |player| {
                player.faction.faction
            });
        if !nw_content::factions::faction_requirement_met(def.required_faction.as_deref(), faction)
        {
            return Err(QuestRefusal::Faction);
        }
        if let Some(group) = def.limit_group {
            let limit = self.content.gameplay.objective_limits.limit(group);
            let running = self
                .quests_of_player(number)
                .iter()
                .filter(|quest| quest.def.limit_group == Some(group))
                .count();
            if u64::try_from(running).unwrap_or(u64::MAX) >= limit {
                return Err(QuestRefusal::Full);
            }
        }
        self.begin_quest(number, objective, take, now_ms)
    }

    /// Starts quest `objective` for player `number`, as [`SimWorld::begin_quest`] without a
    /// mission take.
    pub(crate) fn start_quest(
        &mut self,
        number: u64,
        objective: &str,
        now_ms: u64,
    ) -> Result<(), QuestRefusal> {
        self.begin_quest(number, objective, None, now_ms)
    }

    /// Starts quest or mission `objective` (a mission taken as `take`) for player `number` (admin studio spec section 6.3) next to the
    /// quests the player runs: not while the tutorial runs, not twice at once, and a finished one
    /// only again when it is repeatable.
    fn begin_quest(
        &mut self,
        number: u64,
        objective: &str,
        take: Option<MissionTake>,
        now_ms: u64,
    ) -> Result<(), QuestRefusal> {
        let entity = self
            .player_by_number(number)
            .ok_or(QuestRefusal::NoPlayer)?;
        let def = self
            .content
            .quests
            .quest(objective)
            .ok_or(QuestRefusal::NoQuest)?;
        let running = self
            .quests_of_player(number)
            .iter()
            .any(|quest| quest.def.id.eq_ignore_ascii_case(&def.id));
        if running || self.tutorials.contains_key(&number) {
            return Err(QuestRefusal::Active);
        }
        let done = self
            .world
            .get::<Player>(entity)
            .map(|player| player.done_quests.clone())
            .unwrap_or_default();
        if !def.repeatable && done.iter().any(|id| id.eq_ignore_ascii_case(&def.id)) {
            return Err(QuestRefusal::Done);
        }
        let (run, effects) = ObjectiveRun::start(Arc::clone(&def.tree), now_ms);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            Self::journal_of(&mut player);
            // A repeated quest leaves the done list while it runs: the database keeps one row
            // per objective, active or done.
            player
                .done_quests
                .retain(|id| !id.eq_ignore_ascii_case(&def.id));
            player.quests_unsaved = true;
        }
        let id = def.id.clone();
        let record = record_of(&def, take);
        self.quests
            .entry(number)
            .or_default()
            .push(ActiveQuest { def, run, take });
        self.quest_effects(number, effects, now_ms);
        let tasks = self
            .quests_of_player(number)
            .iter()
            .find(|quest| quest.def.id == id)
            .map(|quest| quest.run.progress());
        if let Some(tasks) = tasks
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(journal) = player.quest.as_mut()
        {
            journal.accept(&id, record, tasks);
        }
        self.update_quest_tracker(number);
        Ok(())
    }

    /// Goes on with quest `objective`'s run `run` of player `number`, who came from another sim or
    /// logged in again: the first own card on this sim sends it. A run that no longer fits the
    /// quest's tree (a design change) is dropped.
    fn resume_quest(&mut self, number: u64, objective: &str, run: &QuestRun, now_ms: u64) {
        let Some(def) = self.content.quests.quest(objective) else {
            tracing::warn!(
                player = number,
                objective,
                "saved quest is not in the content"
            );
            return;
        };
        let Some(quest) = ObjectiveRun::resume(Arc::clone(&def.tree), run, now_ms) else {
            tracing::warn!(
                player = number,
                objective,
                "saved quest run does not fit the quest tree"
            );
            return;
        };
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let tasks = quest.progress();
        // A finished quest's end states went out from the other sim; it does not join the
        // journal. One that waits for its turn-in goes on waiting.
        let finished = quest.finished() && def.turn_in.is_none();
        let id = def.id.clone();
        let record = record_of(&def, run.mission);
        self.quests.entry(number).or_default().push(ActiveQuest {
            def,
            run: quest,
            take: run.mission,
        });
        if finished {
            self.complete_quest(number, &id);
            return;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            Self::journal_of(&mut player).resume(&id, record, tasks);
        }
    }

    /// Starts objective `objective` for player `number` for a quest's `TaskObjective`, as a chain
    /// step; one done before (and not repeatable) is done for the task at once.
    fn start_tracked_objective(&mut self, number: u64, objective: &str, now_ms: u64) {
        match self.start_quest(number, objective, now_ms) {
            Ok(()) => {}
            Err(QuestRefusal::Done) => {
                self.quest_event(number, Event::ObjectiveDone(objective), now_ms);
            }
            Err(refusal) => {
                tracing::debug!(player = number, objective, %refusal, "tracked objective not started");
            }
        }
    }

    /// Ends the finished quest `objective` of player `number`: it joins the done list, its
    /// achievement is set, the achievements of its temporary unlocks are cleared, its rewards go
    /// to the player (the coins and experience of its success game event, its items into the bag,
    /// and the event's standing, categorical progression and achievement), and the quests that
    /// track it hear of its end.
    fn complete_quest(&mut self, number: u64, objective: &str) {
        let Some(quests) = self.quests.get_mut(&number) else {
            return;
        };
        let Some(position) = quests
            .iter()
            .position(|quest| quest.def.id.eq_ignore_ascii_case(objective))
        else {
            return;
        };
        let quest = quests.remove(position);
        if quests.is_empty() {
            self.quests.remove(&number);
        }
        let reward = quest
            .def
            .success_event
            .as_deref()
            .and_then(|event| self.content.rewards.get(event))
            .cloned()
            .unwrap_or_default();
        tracing::info!(player = number, objective = %quest.def.id, "quest done");
        self.scripts.push(scripts::ScriptEvent::QuestDone {
            player: number,
            objective: quest.def.id.clone(),
        });
        if let Some(entity) = self.player_by_number(number)
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
        {
            if !player
                .done_quests
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&quest.def.id))
            {
                player.done_quests.push(quest.def.id.clone());
            }
            player.quests_unsaved = true;
            if let Some(achievement) = &quest.def.achievement {
                player
                    .flow
                    .set_achievement(&self.achievement_rules, achievement);
            }
            for achievement in quest.run.temporary_unlocks() {
                player
                    .flow
                    .clear_achievement(&self.achievement_rules, &achievement);
            }
            quest_objects::grant(&mut player, &reward, &self.content.gameplay);
            // The turn-in takes back the items the quest had the player bring.
            if let Some(gear) = player.gear.as_mut() {
                for (item, _) in quest.def.tree.returned_items() {
                    gear.remove(&item, unix_ns(self.server_ms));
                }
            }
        }
        let success_event = quest.def.success_event.clone();
        if let Some(event) = success_event.as_deref() {
            self.grant_event_standing(number, event);
            self.grant_event_progression(number, event, self.last_now_ms);
        }
        let items: Vec<(String, u32)> = quest.def.reward.into_iter().chain(reward.item).collect();
        self.give_loot(number, items, self.last_now_ms);
        self.quest_event(
            number,
            Event::ObjectiveDone(&quest.def.id),
            self.last_now_ms,
        );
        // Another quest's step can wait for this success event (`09A_S5_SavePrivateer` on
        // `09A_S5_Success`, `Mission_Success_Procurer_T1`).
        if let Some(event) = success_event.as_deref() {
            self.quest_event(number, Event::GameEvent(event), self.last_now_ms);
        }
    }

    /// Feeds game event `event` to the quests of player `number`.
    fn quest_event(&mut self, number: u64, event: Event<'_>, now_ms: u64) {
        self.quest_event_where(number, |_| true, event, now_ms);
    }

    /// Sets achievement `achievement` of player `number` and feeds it to the player's quests.
    fn earn_achievement(&mut self, number: u64, achievement: &str, now_ms: u64) {
        if let Some(entity) = self.player_by_number(number)
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
        {
            player
                .flow
                .set_achievement(&self.achievement_rules, achievement);
        }
        self.quest_event(number, Event::AchievementSet(achievement), now_ms);
    }

    /// Feeds game event `event` to quest `objective` of player `number` only.
    fn quest_event_for(&mut self, number: u64, objective: &str, event: Event<'_>, now_ms: u64) {
        self.quest_event_where(
            number,
            |quest| quest.def.id.eq_ignore_ascii_case(objective),
            event,
            now_ms,
        );
    }

    fn quest_event_where(
        &mut self,
        number: u64,
        wanted: impl Fn(&ActiveQuest) -> bool,
        event: Event<'_>,
        now_ms: u64,
    ) {
        let Some(quests) = self.quests.get_mut(&number) else {
            return;
        };
        let effects: Vec<Effect> = quests
            .iter_mut()
            .filter(|quest| wanted(quest))
            .flat_map(|quest| quest.run.event(event, now_ms))
            .collect();
        self.mark_quests_unsaved(number);
        self.quest_effects(number, effects, now_ms);
    }

    fn mark_quests_unsaved(&mut self, number: u64) {
        if let Some(entity) = self.player_by_number(number)
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
        {
            player.quests_unsaved = true;
        }
    }

    /// Carries out the `effects` of player `number`'s quests: achievements are set or cleared (a
    /// set one is an event of every quest of the player again), items given or removed, game
    /// events fired.
    fn quest_effects(&mut self, number: u64, effects: Vec<Effect>, now_ms: u64) {
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let mut pending = effects;
        while !pending.is_empty() {
            let mut set = Vec::new();
            let mut fired = Vec::new();
            let mut given = Vec::new();
            let mut started = Vec::new();
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                for effect in pending.drain(..) {
                    match effect {
                        Effect::Achievement(id) => {
                            player.flow.set_achievement(&self.achievement_rules, &id);
                            set.push(id);
                        }
                        Effect::LockAchievement(id) => {
                            player.flow.clear_achievement(&self.achievement_rules, &id);
                        }
                        Effect::RemoveItem(id, count) => {
                            if let Some(gear) = player.gear.as_mut() {
                                remove_quest_item(gear, &id, count, unix_ns(self.server_ms));
                            }
                        }
                        Effect::GameEvent(id) => fired.push(id),
                        Effect::GiveItem(id, count) => given.push((id, count)),
                        Effect::StartObjective(id) => started.push(id),
                        Effect::Done(_) => {}
                    }
                }
            }
            for event in fired {
                self.fire_game_event(number, &event, now_ms);
            }
            if !given.is_empty() {
                self.give_loot(number, given, now_ms);
            }
            for objective in started {
                self.start_tracked_objective(number, &objective, now_ms);
            }
            let Some(quests) = self.quests.get_mut(&number) else {
                return;
            };
            for id in set {
                for quest in quests.iter_mut() {
                    pending.extend(quest.run.event(Event::AchievementSet(&id), now_ms));
                }
            }
        }
    }

    /// Runs the quests: their timers and the players' places.
    fn tick_quests(&mut self, now_ms: u64) {
        let numbers: Vec<u64> = self.quests.keys().copied().collect();
        for number in numbers {
            let position = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<Body>(entity))
                .map(|body| body.motion.position());
            let Some(position) = position else {
                continue;
            };
            let player = self
                .player_by_number(number)
                .and_then(|entity| self.world.get::<Player>(entity));
            let Some(quests) = self.quests.get_mut(&number) else {
                continue;
            };
            let mut effects = Vec::new();
            for quest in quests.iter_mut() {
                effects.extend(quest.run.tick(now_ms));
                effects.extend(quest.run.event(Event::Moved(position), now_ms));
            }
            // What the player holds, where it is and its achievements meet the tasks, also those
            // that start later.
            if let Some(player) = player {
                let carried = player.gear.as_ref().map(Gear::carried).unwrap_or_default();
                let held = held_items(&carried);
                let mut places = quest_objects::places(&self.content.poi_tags, &player.territories);
                let at = [position.x, position.y, position.z];
                places.extend(
                    self.content
                        .quest_volumes
                        .iter()
                        .filter(|volume| volume.contains(at))
                        .map(|volume| volume.tag.as_str()),
                );
                let rules = &self.achievement_rules;
                let achieved = |id: &str| player.flow.has_achievement(rules, id);
                let content = &self.content;
                let level = |progression: Option<&str>, points: bool| match progression {
                    None => Some(
                        content
                            .progression
                            .level_of(player.experience)
                            .saturating_add(1),
                    ),
                    Some(id) => {
                        // Any categorical progression: a faction's (`9804_M_0001_RankUp` names
                        // `Marauder`), not only a mastery or a tradeskill.
                        let mastery = content.progression.categorical(id)?;
                        let experience = player.masteries.experience(mastery);
                        Some(if points {
                            experience
                        } else {
                            u32::from(mastery.rank_of(experience).0)
                        })
                    }
                };
                let territories = player_territories(content, &player.territories);
                let of_family = |item: &str, family: &str| item_of_family(content, item, family);
                let facts = Facts {
                    held: &held,
                    places: Some(&places),
                    achieved: Some(&achieved),
                    level: Some(&level),
                    territories: Some(&territories),
                    of_family: Some(&of_family),
                    houses: Some(player.travel.houses()),
                };
                for quest in quests.iter_mut() {
                    effects.extend(quest.run.sync(&facts, now_ms));
                }
            }
            if !effects.is_empty() {
                self.mark_quests_unsaved(number);
            }
            self.quest_effects(number, effects, now_ms);
            self.update_quest_tracker(number);
        }
    }

    /// Carries out the tutorial `orders` for player `number` at `now_ms`; an item it gets goes
    /// back to its tutorial as an event, whose orders are carried out too.
    /// Carries out `order` of the tutorial of player `number` (`entity`) at `now_ms` when it is a
    /// mob order: a move fragment, a scripted swing at the player, or a server move of the player
    /// from the mob. Whether it was one.
    fn mob_order(&mut self, entity: Entity, number: u64, order: &Order, now_ms: u64) -> bool {
        // A dead player is no one's target: no scripted swing, shove or pull reaches its body
        // (live 2026-09-18: Thorpe's grab pulled a body 1.25 s after its death screen).
        let alive = self
            .world
            .get::<Vitals>(entity)
            .is_some_and(|vitals| !vitals.health.is_dead());
        let body_at = self
            .world
            .get::<Body>(entity)
            .filter(|_| alive)
            .map(|body| body.motion.position());
        match *order {
            Order::PlayMove {
                mob,
                fragment,
                length_ms,
            } => self.mob_timelines.play(mob, fragment, length_ms),
            Order::Swing { mob, mob_move } => {
                if let Some(target_at) = body_at {
                    self.scripted_swings.push(MobAttackIntent {
                        mob,
                        target: number,
                        target_at,
                        mob_move: Some(mob_move),
                        strike: None,
                    });
                }
            }
            Order::Shove {
                mob,
                shove,
                length_ms,
            } => {
                let mob_at = self.mobs.get(mob).map(|mob| mob.position);
                if let (Some(from), Some(mob_at)) = (body_at, mob_at)
                    && let Some(to) = shoved_to(from, mob_at, shove)
                    && let to = self.push_reach(entity, from, to)
                    && to != from
                    && let Some(mut player) = self.world.get_mut::<Player>(entity)
                {
                    if matches!(shove, Shove::To(_)) {
                        player.slayer.pulled(now_ms);
                    }
                    player.push = Some(Push {
                        from,
                        to,
                        start_ms: now_ms,
                        end_ms: now_ms.saturating_add(length_ms),
                        arrived: false,
                    });
                }
            }
            _ => return false,
        }
        true
    }

    /// Sends each open-world player the phasing of the sectors around it when they changed.
    fn tick_phasing(&mut self) {
        let content = Arc::clone(&self.content);
        let mut query = self.world.query::<(&mut Player, &Body)>();
        for (mut player, body) in query.iter_mut(&mut self.world) {
            let at = body.motion.position();
            if let Some(update) = player.flow.phasing_at(at.x, at.y, &content.phasing_areas) {
                let connection = player.connection.clone();
                send_now(&mut self.combat_messages, &connection, &[update]);
            }
        }
    }

    /// Moves the players that the server moves at `now_ms` along their pushes; a push that got
    /// to its end stops at the next tick, after the own state that sent its end. The end goes as
    /// a server move too, which a moving client takes (live 2026-09-18: a running player
    /// followed 1 of 5 grab pulls; retail also set slayer-state children whose meaning is
    /// Unknown).
    fn tick_pushes(&mut self, now_ms: u64) {
        let limits = &self.settings.limits;
        let mut query = self.world.query::<(&mut Player, &mut Body)>();
        for (mut player, mut body) in query.iter_mut(&mut self.world) {
            let Some(push) = player.push else {
                continue;
            };
            if push.arrived {
                player.push = None;
                continue;
            }
            let length_ms = push.end_ms.saturating_sub(push.start_ms).max(1);
            #[allow(clippy::cast_precision_loss, reason = "a push lasts under a second")]
            let share = (now_ms.saturating_sub(push.start_ms) as f32 / length_ms as f32).min(1.0);
            let along = |from: f32, to: f32| from + (to - from) * share;
            let at = Position {
                x: along(push.from.x, push.to.x),
                y: along(push.from.y, push.to.y),
                z: along(push.from.z, push.to.z),
            };
            body.motion = Motion::new(at, now_ms, limits);
            let arrived = share >= 1.0;
            if arrived {
                player.moved = true;
                player.move_facing = None;
            }
            player.push = Some(Push { arrived, ..push });
        }
    }

    fn carry_out(&mut self, number: u64, orders: Vec<Order>, now_ms: u64) {
        let Some(entity) = self.player_by_number(number) else {
            return;
        };
        let mut pending = orders;
        while !pending.is_empty() {
            let mut gained = Vec::new();
            let mut set_bits = Vec::new();
            for order in pending.drain(..) {
                if self.mob_order(entity, number, &order, now_ms) {
                    continue;
                }
                if let Order::MoveTo(position, facing) = order {
                    self.place_player(entity, position, Some(facing), Placement::Drop, now_ms);
                    continue;
                }
                let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                    return;
                };
                match order {
                    Order::Achievement(id) => {
                        player.flow.set_achievement(&self.achievement_rules, &id);
                        set_bits.extend(self.achievement_rules.bit(&id));
                    }
                    Order::Asset(asset, step) => {
                        tracing::debug!(player = number, asset, ?step, "level asset");
                        match step {
                            Step::Show => {
                                player.assets.show(asset, asset_gde_ref(number, asset));
                            }
                            Step::Cast(cast) => {
                                let gde_ref = spell_gde_ref(number, asset, cast.number);
                                player.assets.cast(asset, gde_ref, cast);
                            }
                            Step::Change(change) => player.assets.change(asset, change),
                            Step::Hide => player.assets.hide(asset),
                        }
                        player.solids = self.content.asset_solids.shown(player.assets.indices());
                    }
                    Order::RemoveItem(id, count) => {
                        if let Some(gear) = player.gear.as_mut() {
                            remove_quest_item(gear, &id, count, unix_ns(self.server_ms));
                        }
                    }
                    Order::RefillPotions(id, quantity) => {
                        let player = &mut *player;
                        if let (Some(item), Some(gear)) =
                            (self.content.kits.item(&id), player.gear.as_mut())
                        {
                            gear.renew(item, quantity, QUICK_SLOT_1, unix_ns(self.server_ms));
                        }
                        player.cooldowns.clear();
                        player.cooldowns_changed = true;
                    }
                    Order::GiveItem(id, quantity) => {
                        let item = self.content.kits.item(&id);
                        let given = item
                            .zip(player.gear.as_mut())
                            .is_some_and(|(item, gear)| gear.add(item, quantity));
                        if given {
                            gained.push((id, quantity));
                        } else {
                            tracing::warn!(player = number, item = %id, "tutorial item not given");
                        }
                    }
                    Order::ShowIsland => {
                        let statics = &self.content.tutorial_level.statics;
                        player.statics.show_island(statics);
                        // Retail sends the bag's container contents ~300 ms after its card;
                        // the client refuses a take-all on an empty container.
                        let items = player
                            .backstory
                            .as_deref()
                            .map(|backstory| self.content.kits.bag(backstory))
                            .unwrap_or_default();
                        let loot = bag_loot(player.character, items);
                        player.statics.queue(statics, &BAG_PREFIX, loot);
                    }
                    Order::StaticChange(prefix, change) => {
                        player.statics.change(
                            &self.content.tutorial_level.statics,
                            &prefix,
                            change,
                        );
                    }
                    Order::HideStatic(prefix) => {
                        player
                            .statics
                            .hide(&self.content.tutorial_level.statics, &prefix);
                    }
                    Order::AcceptGather(request) => {
                        let accept = gather_answer(
                            number,
                            request,
                            INTERACTION_ACCEPTED,
                            CHEST_DISPLAY_NAME,
                        );
                        send_now(&mut self.combat_messages, &player.connection, &[accept]);
                        let started = chest_gather_started(request.target_entity, player.gathers);
                        player.extra_members.extend(started);
                    }
                    Order::EndGather(request) => {
                        let end = gather_end(number, request);
                        send_now(&mut self.combat_messages, &player.connection, &end);
                        let ended = chest_gather_ended(request.target_entity, player.gathers);
                        player.extra_members.push(ended);
                        player.gathers = player.gathers.saturating_add(1);
                        // The looted chest goes in the loot tracker, so the client offers it no
                        // more; without it the client opened it again with no answer and its
                        // player stayed in the open animation (live 2026-09-29).
                        player.list_version += 1;
                        let version = player.list_version;
                        let tracked = player.loot.track_opened(
                            request.target,
                            encounter::CHEST_AT,
                            self.server_ms,
                            self.server_ms.saturating_add(encounter::CHEST_LOCKOUT_MS),
                            false,
                            version,
                        );
                        player.extra_members.push(tracked);
                    }
                    Order::Targeted { mob, tutorial_ai } => {
                        let messages = targeted(number, mob, tutorial_ai);
                        send_now(&mut self.combat_messages, &player.connection, &messages);
                    }
                    Order::BossPhase { mob, phase } => {
                        self.boss_phases.insert(mob, phase);
                    }
                    Order::Died { mob, collision } => {
                        let messages = died(number, mob, collision);
                        send_now(&mut self.combat_messages, &player.connection, &messages);
                    }
                    Order::LockAchievement(id) => {
                        player.flow.clear_achievement(&self.achievement_rules, &id);
                    }
                    Order::KillExperience(level) => {
                        let base = self.content.kill_experience.of(level).unwrap_or_default();
                        quest_objects::gain_experience(
                            &mut player,
                            base,
                            &self.content.gameplay.xp,
                        );
                    }
                    Order::Blow => {
                        let blow = ServerMessage::MeleeHit(MeleeHitMsg::on(number, false));
                        send_now(&mut self.combat_messages, &player.connection, &[blow]);
                    }
                    Order::MoveTo(..)
                    | Order::PlayMove { .. }
                    | Order::Swing { .. }
                    | Order::Shove { .. } => {}
                }
            }
            let Some(tutorial) = self.tutorials.get_mut(&number) else {
                break;
            };
            for bit in set_bits {
                tutorial.achievement(bit, now_ms);
            }
            for (id, quantity) in &gained {
                pending.extend(tutorial.event(
                    Event::ItemGained {
                        item: id,
                        quantity: *quantity,
                    },
                    &mut self.mobs,
                    now_ms,
                ));
            }
        }
    }

    /// The player of `connection` starts interaction `request`: the kinds that keep the hands
    /// free put its drawn weapon away ([`Gear::put_away`]): a talk, a lore note, a crafting
    /// station, a revive and a trigger for good, a gather by hand until the interaction ends.
    /// Loot bags, gathers with a tool and the exit of a dungeon keep it. Retail, 437 ledgers, the
    /// own put-away within 100 ms of the start with a weapon drawn: revive 90 of 92 (32 to 85
    /// ms; no draw came back at the end sooner than 350 ms, the player's own), trigger 20 of 24,
    /// dungeon exit 0 of 29, loot bag 1 of 1,151, tool gathers 0 of 827.
    fn stow_for_interaction(&mut self, connection: &ConnectionRef, request: &InteractionRequest) {
        self.set_interacting(connection);
        let back = match request.kind {
            TALK_KIND | LORE_KIND | CRAFTING_STATION_KIND | REVIVE_PLAYER_KIND | TRIGGER_KIND => {
                None
            }
            GATHER_KIND
                if request
                    .gatherable
                    .and_then(|crc| self.content.gatherables.get(crc))
                    .is_some_and(|gatherable| gatherable.by_hand) =>
            {
                Some(Stow::Gather)
            }
            _ => return,
        };
        self.put_weapon_away(connection, back);
    }

    /// Notes that the client of `connection` asks for an interaction.
    fn set_interacting(&mut self, connection: &ConnectionRef) {
        if let Some(entity) = self.by_connection.get(connection).copied()
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
        {
            player.interacting = true;
        }
    }

    /// The input of `connection` asks for no interaction at `now_ms`: one that ran has ended,
    /// and a gather's put-away weapon comes back ([`Gear::give_back`]).
    pub fn interaction_over(&mut self, connection: &ConnectionRef, now_ms: u64) {
        let Some(entity) = self.by_connection.get(connection).copied() else {
            return;
        };
        let ended = self
            .world
            .get_mut::<Player>(entity)
            .is_some_and(|mut player| std::mem::replace(&mut player.interacting, false));
        if ended {
            self.give_weapon_back(entity, Stow::Gather, now_ms);
        }
    }

    /// The server puts the drawn weapon of the player of `connection` away now, to give it back
    /// for `back` ([`Gear::put_away`]).
    fn put_weapon_away(&mut self, connection: &ConnectionRef, back: Option<Stow>) {
        let time_ns = unix_ns(self.server_ms);
        if let Some(entity) = self.by_connection.get(connection).copied()
            && let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
            && gear.put_away(time_ns, back)
        {
            tracing::debug!(player = player.number, ?back, "weapon put away");
        }
    }

    /// The reason `reason` of a put-away of player `entity` ends at `now_ms` ([`Gear::give_back`]).
    fn give_weapon_back(&mut self, entity: Entity, reason: Stow, now_ms: u64) {
        let time_ns = unix_ns(self.server_ms);
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(gear) = player.gear.as_mut()
            && gear.give_back(reason, now_ms, time_ns)
        {
            tracing::debug!(player = player.number, ?reason, "weapon given back");
        }
    }

    /// The client of player `entity` ended interaction `interaction` at `now_ms`: each interact
    /// tag of its object counts for the player's `TaskInteract` tasks (retail: the vault step
    /// was done 49 ms after the storage interaction ended).
    fn interaction_ended(&mut self, entity: Entity, interaction: u64, now_ms: u64) {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if let Some(last) = player.last_interaction.as_mut()
            && last.asked.1 == interaction
        {
            last.cancelled = true;
        }
        let number = player.number;
        let Some((open, tags)) = player.open_interaction.take() else {
            return;
        };
        if open != interaction {
            player.open_interaction = Some((open, tags));
            return;
        }
        for tag in &tags {
            tracing::debug!(player = number, tag, "interact tag");
            self.quest_event(number, Event::Interacted(tag), now_ms);
        }
        self.update_quest_tracker(number);
    }

    /// Whether this sim runs the tutorial.
    fn is_tutorial(&self) -> bool {
        self.settings.level == TUTORIAL_LEVEL
    }

    /// The client of `connection` starts interaction `request` with the object it names `name`
    /// at `now_ms` (the use of a static entity, from its input): its tutorial answers a use of
    /// the chest; in the open world it is a talk to an NPC, the use of a crafting station, the
    /// read of a lore note or a gather, by its kind, answered once per interaction.
    pub fn interact(
        &mut self,
        connection: &ConnectionRef,
        request: InteractionRequest,
        name: &[u8],
        now_ms: u64,
    ) {
        let Some(number) = self
            .by_connection
            .get(connection)
            .and_then(|entity| self.world.get::<Player>(*entity))
            .map(|player| player.number)
        else {
            return;
        };
        tracing::debug!(
            player = number,
            kind = request.kind,
            interaction = request.interaction,
            target = ?request.target,
            "interaction request"
        );
        self.stow_for_interaction(connection, &request);
        let Some(tutorial) = self.tutorials.get_mut(&number) else {
            let Some(&entity) = self.by_connection.get(connection) else {
                return;
            };
            let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                return;
            };
            let asked = (request.kind, request.interaction);
            let repeat = player
                .last_interaction
                .as_ref()
                .is_some_and(|last| last.repeats(asked, now_ms));
            let cancelled = repeat
                && player
                    .last_interaction
                    .as_ref()
                    .is_some_and(|last| last.cancelled);
            player.last_interaction = Some(LastInteraction {
                asked,
                at_ms: now_ms,
                cancelled,
            });
            if repeat {
                return;
            }
            let tags = self
                .content
                .interact_tags
                .of_interaction(request.target_entity, request.gatherable);
            player.open_interaction =
                (!tags.is_empty()).then(|| (request.interaction, tags.to_vec()));
            self.mount_interaction(entity, now_ms);
            match request.kind {
                TALK_KIND => self.talk(entity, number, request, name, now_ms),
                TOWN_BOARD_KIND => self.open_board(entity, number, request, name),
                CRAFTING_STATION_KIND => self.open_station(entity, number, request),
                LORE_KIND => self.read_lore(entity, number, request, name, now_ms),
                GATHER_KIND => self.gather_open_world(entity, number, request, name, now_ms),
                WATER_GATHER_KIND => self.gather_water(number, request, now_ms),
                LOOT_KIND => self.take_loot_bag(entity, number, request, now_ms),
                REVIVE_PLAYER_KIND => self.revive_player(entity, number, request, name),
                item_calls::OPEN_STORAGE_KIND => self.open_storage(entity, number, request, name),
                economy::TRADING_POST_KIND => {
                    self.open_trading_post(entity, number, request, name);
                }
                kind => {
                    if !self.travel_interaction(entity, number, request, name, now_ms)
                        && !self.instance_interaction(entity, number, request, name)
                        && !self.open_mount_screen(entity, number, request, name)
                    {
                        tracing::debug!(player = number, kind, "interaction of an unknown kind");
                    }
                }
            }
            return;
        };
        let orders = tutorial.interact(request, now_ms);
        self.carry_out(number, orders, now_ms);
    }

    /// Answers a lookup call of `connection`'s player (streamer groups, waiting meta achievement
    /// rewards, lobby counts) with the messages that go out at once, retail answered 25 to 60 ms
    /// after the call; the contracts lookup goes to the trading post.
    pub fn lookup(&mut self, connection: &ConnectionRef, request: &LookupRequest) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        // The waiting meta achievement rewards are the player's (meta achievements module).
        let answer = match *request {
            LookupRequest::PendingMetaAchievements { header } => {
                self.pending_meta_achievements(entity, header)
            }
            // The trading post's orders are in the world database (economy module).
            LookupRequest::Contracts {
                header,
                ref query,
                lookup,
            } => {
                self.lookup_contracts(entity, header, query, lookup);
                None
            }
            _ => request.answer(),
        };
        if let Some(answer) = answer {
            send_now(&mut self.combat_messages, connection, &[answer]);
        }
    }

    /// Answers a facet call of the client of `connection` at `now_ms` (tutorial-intro note,
    /// section 4), and gives it to the player's tutorial: videos, cutscenes, the quick-time
    /// lesson, UI screens, the beach bag and equips (live-tutorial note). In the tutorial, a
    /// request to leave (502) shows the loading screen and hands the player to the open world at
    /// the next tick, as the exit does.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when an answer cannot be encoded.
    pub fn facet_message(
        &mut self,
        connection: &ConnectionRef,
        message: ClientFacetMsg,
        now_ms: u64,
    ) -> Result<(), MessageError> {
        let Some(&entity) = self.by_connection.get(connection) else {
            return Ok(());
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return Ok(());
        };
        match &message {
            ClientFacetMsg::CancelInteraction { interaction, .. } => {
                self.interaction_ended(entity, *interaction, now_ms);
                self.cancel_gather(entity, *interaction, now_ms);
                self.leave_station(entity, *interaction);
                self.give_weapon_back(entity, Stow::Gather, now_ms);
                self.close_container(entity);
                return Ok(());
            }
            ClientFacetMsg::SetReplicateStorage { storage, .. } => {
                if !self.show_stored_items(entity, storage) {
                    self.show_bag_items(entity, storage);
                }
                return Ok(());
            }
            ClientFacetMsg::TakeAllItems { .. }
                if !self.tutorials.contains_key(&number)
                    && self.take_all_open(entity, number, now_ms) =>
            {
                return Ok(());
            }
            ClientFacetMsg::AcceptObjective { objective, .. } => {
                self.accept_objective(entity, number, *objective, now_ms);
                return Ok(());
            }
            ClientFacetMsg::CompleteObjective { objective, .. } => {
                self.turn_in(entity, number, *objective);
                return Ok(());
            }
            ClientFacetMsg::SetCraftingDetails {
                header,
                recipe,
                quantity,
                fee,
                gear_score,
                perk_items,
                ingredients,
                ..
            } => {
                let choice = crafting::CraftChoice {
                    gear_score: u32::from(*gear_score),
                    perk_items: perk_items.clone(),
                    header: Some(*header),
                };
                self.craft(
                    entity,
                    number,
                    *recipe,
                    *quantity,
                    *fee,
                    ingredients,
                    &choice,
                    now_ms,
                );
                return Ok(());
            }
            ClientFacetMsg::CancelCrafting { header } => {
                self.craft_cancelled(entity, number, *header);
                return Ok(());
            }
            ClientFacetMsg::InitiateSalvage {
                bag_slot,
                instance,
                quantity,
                ..
            } => {
                let target = salvage::SalvageTarget {
                    bag_slot: *bag_slot,
                    instance: Some(*instance),
                    quantity: Some(*quantity),
                };
                self.salvage(entity, number, &[target], now_ms);
                return Ok(());
            }
            ClientFacetMsg::SalvageItems { bag_slots, .. } => {
                let targets: Vec<salvage::SalvageTarget> = bag_slots
                    .iter()
                    .map(|bag_slot| salvage::SalvageTarget {
                        bag_slot: *bag_slot,
                        instance: None,
                        quantity: None,
                    })
                    .collect();
                self.salvage(entity, number, &targets, now_ms);
                return Ok(());
            }
            ClientFacetMsg::SalvageByClass {
                class, rarities, ..
            } => {
                self.salvage_by_class(entity, number, *class, *rarities, now_ms);
                return Ok(());
            }
            // A first entry discovers a discoverable place; a later one charts it (retail: the
            // shrines 40293, 40295 and 40183 were charted at an entry after their discovery).
            ClientFacetMsg::TerritoryEntered { territory, .. } => {
                self.territory_changed(entity, *territory, true);
                if !self.discover_territory(entity, *territory) {
                    self.chart_area(entity, number, *territory, now_ms);
                }
                return Ok(());
            }
            ClientFacetMsg::TerritoryExited { territory, .. } => {
                self.territory_changed(entity, *territory, false);
                return Ok(());
            }
            // The fallback forms (6733, 6734) name the territory alone; retail sends
            // no answer, and the area form's discovery does not run.
            ClientFacetMsg::EnteredTerritoryFallback { territory, .. } => {
                self.territory_changed(entity, *territory, true);
                return Ok(());
            }
            ClientFacetMsg::ExitedTerritoryFallback { territory, .. } => {
                self.territory_changed(entity, *territory, false);
                return Ok(());
            }
            ClientFacetMsg::TogglePvpFlag { .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.pvp_flagged = !player.pvp_flagged;
                }
                return Ok(());
            }
            ClientFacetMsg::SetFaction { faction, .. } => {
                self.set_faction(entity, number, *faction, now_ms);
                return Ok(());
            }
            // A cutscene and the load of a new level put the weapon away; the flow answers them.
            ClientFacetMsg::RequestCutscene { .. } | ClientFacetMsg::AckLevelInfo { .. } => {
                self.put_weapon_away(connection, None);
            }
            ClientFacetMsg::SetObjectiveAutoPin { flags, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    let pin = flags.iter().any(|flag| *flag != 0);
                    player.auto_pin = pin;
                    if let Some(quest) = player.quest.as_mut() {
                        quest.set_auto_pin(pin);
                    }
                }
                return Ok(());
            }
            ClientFacetMsg::SetInputsThrottled { throttled, .. } => {
                if *throttled
                    && let Some(mut player) = self.world.get_mut::<Player>(entity)
                    && !player.throttle_cleared
                {
                    player.defer_throttle_clear = true;
                }
                return Ok(());
            }
            ClientFacetMsg::WhisperStarted { .. } => {}
            // The pins are the client's own: it sends a clear and the new list in the same
            // millisecond at the spawn and at each new quest (retail newchar 2026-09-18: `00`,
            // `00`, `01` and the instance at 9291 ms and at 450543 ms). An untrack at the clear
            // emptied the tracked list, and the tutorial lost its markers (runs 16 and 17).
            ClientFacetMsg::UpdateTrackedObjectives { .. } => return Ok(()),
            ClientFacetMsg::LookingThroughLoadout { looking, .. } => {
                if *looking {
                    self.mount_inventory_opened(entity, now_ms);
                    self.put_weapon_away(connection, Some(Stow::Inventory));
                } else {
                    self.give_weapon_back(entity, Stow::Inventory, now_ms);
                }
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.camera_held_until_ms = Some(if *looking {
                        u64::MAX
                    } else {
                        now_ms.saturating_add(LOADOUT_CAMERA_SETTLE_MS)
                    });
                    // The server answers the open and the close with the new
                    // `lookingThroughLoadout`; a repeated close changes nothing (retail newchar
                    // 2026-09-18: one 3935 per change, none for the closes that follow). The
                    // inventory's slayer slot starts in the newest input record (retail child 17)
                    // and its clear waits for the second close ([`slayer`]).
                    let started = own_state_counter(player.input_counter.unwrap_or(0));
                    let changed = std::mem::replace(&mut player.looking_through_loadout, *looking)
                        != *looking;
                    if changed {
                        player.loadout_change_due = true;
                    }
                    if !*looking {
                        player.slayer.close_loadout(started);
                    } else if changed {
                        player.slayer.open_loadout(started, now_ms);
                    }
                }
                return Ok(());
            }
            _ => {}
        }
        let tutorial = self.is_tutorial();
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return Ok(());
        };
        let ended_cutscene = player.flow.cutscene();
        let equipped = match message {
            ClientFacetMsg::EquipItem {
                bag_slot,
                equip_slot,
                quantity,
                instance,
                ..
            } => player.gear.as_mut().and_then(|gear| {
                gear.equip(bag_slot, equip_slot, quantity, instance, |item_id| {
                    self.content.is_item_disabled(item_id)
                })
                .inspect_err(|refusal| {
                    tracing::debug!(?refusal, bag_slot, equip_slot, "equip refused");
                })
                .ok()
            }),
            ClientFacetMsg::SwapItems {
                slot_a,
                slot_b,
                instance_a,
                instance_b,
                ..
            } => {
                if let Some(gear) = player.gear.as_mut()
                    && let Err(refusal) = gear.swap(slot_a, slot_b, instance_a, instance_b)
                {
                    tracing::debug!(?refusal, slot_a, slot_b, "swap refused");
                }
                None
            }
            ClientFacetMsg::UnequipItem { slot, bag_slot, .. } => {
                if let Some(gear) = player.gear.as_mut()
                    && let Err(refusal) = gear.unequip(slot, bag_slot)
                {
                    tracing::debug!(?refusal, slot, bag_slot, "unequip refused");
                }
                None
            }
            _ => None,
        };
        let backstory = player.backstory.clone();
        let answers = match message {
            ClientFacetMsg::ReturnToMainWorld { .. }
                if tutorial && player.leave_at_ms.is_none() =>
            {
                player.leave_at_ms = Some(now_ms.saturating_add(LEAVE_DELAY_MS));
                player.flow.leave()
            }
            ClientFacetMsg::TimeSyncRequest { header } => time_answers(&self.ids, header).to_vec(),
            _ => {
                let client_context = player.client_context;
                player.flow.answer(&message, client_context)
            }
        };
        send_now(&mut self.combat_messages, connection, &answers);
        if !self.tutorials.contains_key(&number) {
            let event = match message {
                ClientFacetMsg::CinematicStarted { cinematic, .. } => {
                    Some(Event::CinematicStarted(cinematic))
                }
                ClientFacetMsg::CinematicEnded { cinematic, .. } => {
                    Some(Event::CinematicEnded(cinematic))
                }
                ClientFacetMsg::CutsceneEnded { .. } => ended_cutscene.map(Event::CutsceneEnded),
                ClientFacetMsg::UiScreenOpened { screen, .. } => {
                    Some(Event::UiScreenOpened(screen))
                }
                ClientFacetMsg::EquipItem { .. } => equipped.as_deref().map(Event::Equipped),
                _ => None,
            };
            if let Some(event) = event {
                self.quest_event(number, event, now_ms);
            }
            return Ok(());
        }
        let Some(run) = self.tutorials.get_mut(&number) else {
            return Ok(());
        };
        run.client_message(&message, now_ms);
        let mobs = &mut self.mobs;
        let orders = match message {
            ClientFacetMsg::QteCompleted { .. } => run.lesson_done(mobs, now_ms),
            ClientFacetMsg::CinematicStarted { cinematic, .. } => {
                run.event(Event::CinematicStarted(cinematic), mobs, now_ms)
            }
            ClientFacetMsg::CinematicEnded { cinematic, .. } => {
                run.event(Event::CinematicEnded(cinematic), mobs, now_ms)
            }
            ClientFacetMsg::CutsceneEnded { .. } => ended_cutscene
                .map(|cutscene| run.event(Event::CutsceneEnded(cutscene), mobs, now_ms))
                .unwrap_or_default(),
            ClientFacetMsg::UiScreenOpened { screen, .. } => {
                run.event(Event::UiScreenOpened(screen), mobs, now_ms)
            }
            ClientFacetMsg::EquipItem { .. } => equipped
                .map(|item| run.event(Event::Equipped(&item), mobs, now_ms))
                .unwrap_or_default(),
            ClientFacetMsg::TakeAllItems { container, .. } if run.take_bag(&container, now_ms) => {
                let items = backstory
                    .as_deref()
                    .map(|backstory| self.content.kits.bag(backstory))
                    .unwrap_or_default();
                player.statics.queue(
                    &self.content.tutorial_level.statics,
                    &BAG_PREFIX,
                    bag_emptied(items.len()),
                );
                player
                    .extra_members
                    .push(interactor_target(facet_of(&BAG_PREFIX)));
                let mut orders: Vec<Order> = items
                    .iter()
                    .map(|item| Order::GiveItem(item.item_id.clone(), 1))
                    .collect();
                let item_ids: Vec<&str> = items.iter().map(|item| item.item_id.as_str()).collect();
                orders.extend(run.event(Event::ContainerLooted { items: &item_ids }, mobs, now_ms));
                orders
            }
            _ => Vec::new(),
        };
        self.carry_out(number, orders, now_ms);
        Ok(())
    }

    fn start_handoffs(&mut self, now_ms: u64) -> Vec<HandoffStart> {
        // The moves into and out of instances go first; their players are frozen then.
        let mut starts = self.instance_transfers(now_ms);
        if self.is_tutorial() {
            starts.extend(self.tutorial_exits(now_ms));
            return starts;
        }
        let Some(cells) = &self.cells else {
            return starts;
        };
        let margin = self.settings.handoff_margin_m;
        let quests = &self.quests;
        let mut query = self.world.query::<(&mut Player, &Body, &Vitals)>();
        for (mut player, body, vitals) in query.iter_mut(&mut self.world) {
            // A dead player stays until its respawn: the next sim could not revive it.
            if player.frozen
                || now_ms < player.no_handoff_before_ms
                || vitals.health.is_dead()
                || travel::moved_by_server(&player)
            {
                continue;
            }
            let position = body.motion.position();
            let target = cells
                .owners
                .handoff_target(cells.me, position, margin)
                .and_then(|owner| cells.sims.get(usize::from(owner)));
            let Some(target) = target else {
                continue;
            };
            player.frozen = true;
            let carried = quests_of(quests, now_ms, &player);
            starts.push(HandoffStart {
                target: target.clone(),
                offer: handoff_offer(&player, body, vitals, now_ms, None, carried),
            });
        }
        starts
    }

    /// The tutorial players at the exit or whose client asked to leave, handed to the open-world
    /// sim that owns the beach start with the level change (new-player-start note). Only a player
    /// whose client has spawned leaves; without a known open-world sim everyone stays.
    fn tutorial_exits(&mut self, now_ms: u64) -> Vec<HandoffStart> {
        let Some(target) = self.exit_target.clone() else {
            return Vec::new();
        };
        let tutorials = &self.tutorials;
        let kits = &self.content.kits;
        let time_ns = unix_ns(self.server_ms);
        let mut query = self.world.query::<(&mut Player, &Body, &Vitals)>();
        let mut starts = Vec::new();
        for (mut player, body, vitals) in query.iter_mut(&mut self.world) {
            // With the quest running, the client asks to leave after the last video; without
            // it, the exit area ends the tutorial.
            let at_exit =
                !tutorials.contains_key(&player.number) && at_tutorial_exit(body.motion.position());
            let leaves = player.leave_at_ms.is_some_and(|at| now_ms >= at);
            if player.frozen
                || now_ms < player.no_handoff_before_ms
                || !vitals.spawned
                || vitals.health.is_dead()
                || !(leaves || at_exit)
            {
                continue;
            }
            // The empty phasing goes first and the handoff a while later: sent in the same tick,
            // the open world's level change reached the client before it, and the client dropped
            // it (facets note; retail 6310 at 654271 ms, the level change at 654344 ms).
            if leaves {
                match player.phasing_left_ms {
                    None => {
                        player.phasing_left_ms = Some(now_ms);
                        let left = player.flow.left();
                        send_now(&mut self.combat_messages, &player.connection, &[left]);
                        continue;
                    }
                    Some(left_ms) if now_ms < left_ms.saturating_add(PHASING_BEFORE_HANDOFF_MS) => {
                        continue;
                    }
                    // Retail's client acknowledged the empty phasing 4 ms after it (6745).
                    Some(_) if !player.flow.phasing_acknowledged() => {
                        tracing::warn!(
                            player = player.number,
                            "handoff before the client acknowledged the empty phasing"
                        );
                    }
                    Some(_) => {}
                }
            }
            player.frozen = true;
            // The player leaves without the tutorial's sword, with its class's bag gear: a run
            // that skipped the beach bag (a skip of the tutorial) gets it here.
            let bag_taken = tutorials
                .get(&player.number)
                .is_some_and(encounter::Tutorial::bag_taken);
            let backstory = player.backstory.clone();
            let number = player.number;
            if let Some(gear) = player.gear.as_mut() {
                gear.remove(TUTORIAL_SWORD, time_ns);
                if !bag_taken {
                    for item in backstory
                        .as_deref()
                        .map(|backstory| kits.bag(backstory))
                        .unwrap_or_default()
                    {
                        if !gear.add(item, 1) {
                            tracing::warn!(player = number, item = %item.item_id, "bag item not given");
                        }
                    }
                }
            }
            starts.push(HandoffStart {
                target: target.clone(),
                offer: handoff_offer(
                    &player,
                    body,
                    vitals,
                    now_ms,
                    Some(OPEN_WORLD_START),
                    // The quest lists the beach sends go above the tutorial's: the client drops
                    // lower versions (live run 12: beach lists at 2 after the tutorial's 0x26).
                    PlayerQuests {
                        tracker_version: player.tracker_shown(),
                        ..PlayerQuests::default()
                    },
                ),
            });
        }
        starts
    }

    /// The clock of player `player`'s ALC states at server time `server_ms`: from its first own
    /// state, or from now before it has one, numbered by its client's input when it numbers it.
    const fn alc_clock(player: &Player, server_ms: u64) -> OwnClock {
        let spawn_ms = match player.own_spawned_ms {
            Some(spawned) => spawned,
            None => server_ms,
        };
        OwnClock::at(spawn_ms, server_ms, player.state_number)
    }

    /// What other players see of `player` now ([`replica`]).
    fn replica_source(player: &Player, vitals: &Vitals, server_ms: u64) -> ReplicaSource {
        let clock = Self::alc_clock(player, server_ms);
        let (items, hands, gear_version) = player.gear.as_ref().map_or_else(
            || (Vec::new(), Hands::default(), 0),
            |gear| {
                let shown = gear.visible_items().iter().map(ShownItem::of).collect();
                (shown, gear.hands(), gear.shown_version())
            },
        );
        ReplicaSource {
            character: *player.character.as_bytes(),
            look: player.look,
            health: vitals.health.current(),
            max_health: vitals.health.max(),
            view: player.view,
            number: player.state_number,
            action: player.slayer.current(),
            upper: player.slayer.remote_upper(),
            blocking: vitals.blocking(),
            spawned_ms: clock.spawn_ms,
            gear_version,
            items,
            hands,
            in_combat: player.combat.in_combat(),
            target: vitals.lock_target(),
            looking_through_loadout: player.looking_through_loadout,
            mount: player.mount.remote(),
        }
    }

    fn ghosts_out(&mut self) -> Vec<(BusId, Vec<Ghost>)> {
        let mut lists: BTreeMap<BusId, Vec<Ghost>> = BTreeMap::new();
        if let Some(cells) = &self.cells {
            let radius = self.settings.interest_radius_m;
            let mut query = self.world.query::<(&Player, &Body, &Vitals)>();
            // A dead player is no ghost: the mobs of other sims forget it too.
            for (player, body, vitals) in query
                .iter(&self.world)
                .filter(|(_, _, vitals)| !vitals.health.is_dead())
            {
                let position = body.motion.position();
                let ghost = Self::replica_source(player, vitals, self.server_ms).ghost();
                for owner in cells.owners.others_within(cells.me, position, radius) {
                    if let Some(sim) = cells.sims.get(usize::from(owner)) {
                        lists.entry(sim.id.clone()).or_default().push(Ghost {
                            entity: player.number,
                            position: Point::from(position),
                            mob: None,
                            player: Some(ghost),
                        });
                    }
                }
            }
            for (entity, position, state) in self.mobs.awake_states() {
                for owner in cells.owners.others_within(cells.me, position, radius) {
                    if let Some(sim) = cells.sims.get(usize::from(owner)) {
                        lists.entry(sim.id.clone()).or_default().push(Ghost {
                            entity,
                            position: Point::from(position),
                            mob: Some(state),
                            player: None,
                        });
                    }
                }
            }
        }
        for target in &self.ghost_targets {
            lists.entry(target.clone()).or_default();
        }
        self.ghost_targets = lists
            .iter()
            .filter(|(_, ghosts)| !ghosts.is_empty())
            .map(|(target, _)| target.clone())
            .collect();
        lists.into_iter().collect()
    }

    /// A bundle for every player that is not frozen, on `tick_threads` threads for a large world.
    fn bundles(&mut self, now_ms: u64, server_time: u64) -> Result<Bundled, MessageError> {
        let started = Instant::now();
        let mut bodies = self.world.query::<(&Player, &Body)>();
        let mut everyone: Vec<(u64, Position)> = bodies
            .iter(&self.world)
            .map(|(player, body)| (player.number, body.motion.position()))
            .collect();
        let own: HashSet<u64> = everyone.iter().map(|(number, _)| *number).collect();
        let mut cards: HashMap<u64, MobCard> = HashMap::new();
        // The state numbers and the actions move on to this tick: the own state and the players
        // who see it get the same.
        for mut player in self.world.query::<&mut Player>().iter_mut(&mut self.world) {
            if let Some(counter) = player.input_counter {
                player.state_number = Some(StateNumber::after(
                    player.state_number,
                    own_state_counter(counter),
                    server_time,
                ));
            }
            let clock = Self::alc_clock(&player, server_time);
            player
                .slayer
                .advance(clock.stamp().id, clock.since_spawn_ms);
        }
        // Every player as the players who see it get it this tick: ours, then the ghosts'.
        let mut sources: Vec<(u64, ReplicaSource)> = self
            .world
            .query::<(&Player, &Vitals)>()
            .iter(&self.world)
            .map(|(player, vitals)| {
                (
                    player.number,
                    Self::replica_source(player, vitals, server_time),
                )
            })
            .collect();
        for ghost in self.ghosts.values().flat_map(|ghosts| &ghosts.entities) {
            if ghost.mob.is_none() && !own.contains(&ghost.entity) {
                everyone.push((ghost.entity, ghost.position));
                if let Some(player) = &ghost.player {
                    sources.push((ghost.entity, ReplicaSource::of_ghost(player)));
                }
            }
        }
        let mut replicas: HashMap<u64, Replica> = HashMap::with_capacity(sources.len());
        let mut memory = HashMap::with_capacity(sources.len());
        let places: HashMap<u64, Position> = everyone.iter().copied().collect();
        for (number, source) in sources {
            let Some(&position) = places.get(&number) else {
                continue;
            };
            let seen = self.replica_memory.get(&number).copied();
            let (replica, now_seen) = Replica::new(source, position, seen, server_time);
            replicas.insert(number, replica);
            memory.insert(number, now_seen);
        }
        self.replica_memory = memory;
        // A dead mob is sent only with a prefab, whose death states play its death; without one
        // it is not sent (M6-B spec section 5.6).
        let content = Arc::clone(&self.content);
        let shown = |state: &MobGhost| {
            state.health > 0 || content.mob_looks.look(state.vitals_id, None).is_some()
        };
        let mobs: Vec<(u64, Position, MobGhost)> = self
            .mob_states()
            .filter(|(_, _, state)| shown(state))
            .collect();
        // A personal mob's timeline starts when its owner's client can see it: the first state
        // then carries the spawn fragment, as on retail (live 2026-09-12: a sailor whose first
        // state came after the spawn fragment ended stayed invisible).
        let spawned: HashSet<u64> = self
            .world
            .query::<(&Player, &Vitals)>()
            .iter(&self.world)
            .filter(|(_, vitals)| vitals.spawned)
            .map(|(player, _)| player.number)
            .collect();
        // Where each player is, for the facing of the mobs that fight it.
        let positions: HashMap<u64, Position> = everyone.iter().copied().collect();
        for (entity, position, state) in mobs {
            everyone.push((entity, position));
            if self.mobs.health(entity).is_some() {
                nw_messages::hold_raw_id(self.raw_ids, entity);
            }
            let owner = self.mobs.owner(entity);
            let mut card = card_of(entity, position, state, owner);
            card.asset = self.mobs.asset(entity);
            card.server_ms = server_time;
            // Only the tutorial's own mobs: every retail tutorial creature has its look target on
            // component 4; other prefabs use that component for other states.
            if let Some(target) = self.mobs.target(entity).filter(|_| owner.is_some()) {
                card.look_target_changed = self.look_targets.insert(entity, target) != Some(target);
            }
            card.look_target = self.look_targets.get(&entity).copied().map(facet_entity);
            let previous = self.mob_health_shown.insert(entity, state.health);
            card.health_changed = previous != Some(state.health);
            card.first_damage = card.health_changed
                && previous == Some(state.max_health)
                && state.health < state.max_health;
            if let Some(&phase) = self.boss_phases.get(&entity) {
                let shown = self.boss_phases_shown.insert(entity, phase);
                card.boss_phase = Some(phase);
                card.boss_phase_changed = shown != Some(phase);
            }
            let effects = self
                .mob_effects
                .get(&entity)
                .map(|held| held.remote_entries(&content.status_effects))
                .unwrap_or_default();
            let shown = self.mob_effects_shown.get(&entity);
            if shown.is_some_and(|(_, entries)| *entries != effects)
                || (shown.is_none() && !effects.is_empty())
            {
                let version = shown.map_or(0, |(version, _)| *version) + 1;
                self.mob_effects_shown.insert(entity, (version, effects));
                card.remote_effects_changed = true;
            }
            card.remote_effects = self.mob_effects_shown.get(&entity).cloned();
            card.skinned = self.skinned.contains_key(&entity);
            card.skinned_changed = card.skinned && self.skinned_shown.insert(entity);
            let unseen = owner.is_some_and(|owner| !spawned.contains(&owner));
            if let Some(look) = content
                .mob_looks
                .look(state.vitals_id, card.asset)
                .filter(|look| look.alc.is_some() && !unseen)
            {
                let target = self
                    .mobs
                    .target(entity)
                    .and_then(|target| positions.get(&target).copied());
                let pose = MobPose {
                    position,
                    fighting: self.mobs.fighting(entity).unwrap_or(false),
                    dead: state.health == 0,
                    target,
                    phase_two: self.mobs.phase_two(entity).is_some(),
                    tree: tree_pose(
                        &self.mobs,
                        &content.creature_animations,
                        entity,
                        look.asset.id,
                    ),
                };
                let frame = self.mob_timelines.advance(
                    entity,
                    server_time,
                    &pose,
                    self.creature_animations.get(&look.asset.id),
                );
                if frame.alert_started {
                    self.mobs.freeze(entity, now_ms + ALERT_STAND_MS);
                }
                if let Some(length_ms) = self.mob_timelines.take_reaction(entity) {
                    self.mobs.react(entity, now_ms, length_ms);
                }
                card.alc = Some(frame);
            }
            cards.insert(entity, card);
        }
        let shown: HashSet<u64> = cards.keys().copied().collect();
        self.mob_timelines.keep_only(&shown);
        self.mob_health_shown
            .retain(|entity, _| shown.contains(entity));
        self.boss_phases_shown
            .retain(|entity, _| shown.contains(entity));
        self.mob_effects_shown
            .retain(|entity, _| shown.contains(entity));
        self.look_targets.retain(|entity, _| shown.contains(entity));
        let mobs = &self.mobs;
        self.boss_phases
            .retain(|entity, _| mobs.health(*entity).is_some());
        // A corpse leaves the skinned ones when its mob lives again or is gone.
        self.skinned
            .retain(|entity, _| mobs.health(*entity) == Some(0));
        let skinned = &self.skinned;
        self.skinned_shown
            .retain(|entity| skinned.contains_key(entity));
        // The radius is checked at start to be a finite number above 0.
        let Some(mut grid) = SpatialGrid::new(self.settings.interest_radius_m) else {
            return Ok(Bundled::default());
        };
        for (number, position) in &everyone {
            grid.insert(*number, *position);
        }
        let cards_seconds = started.elapsed().as_secs_f64();
        let near = Neighbourhood {
            grid: &grid,
            positions: everyone.iter().copied().collect(),
            cards,
            mob_looks: &content.mob_looks,
            level: &content.tutorial_level,
            nodes: &self.nodes,
            stations: (!self.is_tutorial()).then_some(&self.stations),
            quest_objects: &content.quest_objects,
            view_radius_m: self.settings.interest_radius_m,
            bags: &self.bags,
            performances: &self.performances,
            progression: &content.progression,
            mutations: &content.mutations,
            mutation_unix_s: self.unix_s(),
            replicas,
            local_player_card: self.settings.local_player_card,
            now_ms,
            server_time,
        };
        let mut query = self.world.query::<(&mut Player, &Body, &Vitals)>();
        let mut players: Vec<(Mut<Player>, &Body, &Vitals)> =
            query.iter_mut(&mut self.world).collect();
        let threads = self
            .settings
            .tick_threads
            .min(players.len() / MIN_PLAYERS_PER_THREAD)
            .max(1);
        if threads == 1 {
            return near.bundles_of(&mut players).map(|bundled| Bundled {
                cards_seconds,
                ..bundled
            });
        }
        let chunk = players.len().div_ceil(threads);
        let parts: Vec<Result<Bundled, MessageError>> = std::thread::scope(|scope| {
            let handles: Vec<_> = players
                .chunks_mut(chunk)
                .map(|part| scope.spawn(|| near.bundles_of(part)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        });
        let mut bundled = Bundled {
            cards_seconds,
            ..Bundled::default()
        };
        for part in parts {
            let part = part?;
            bundled.outgoing.extend(part.outgoing);
            bundled.members += part.members;
        }
        Ok(bundled)
    }

    /// Refuses a new player that would break a world rule.
    fn check(&mut self, arrival: &Arrival) -> Result<(), WorldError> {
        if self.by_connection.contains_key(&arrival.connection) {
            return Err(WorldError::AlreadyJoined(arrival.connection.clone()));
        }
        if self.player_of(arrival.character).is_some() {
            return Err(WorldError::CharacterInWorld(arrival.character));
        }
        if self.players() >= self.settings.max_players as usize {
            return Err(WorldError::Full);
        }
        let position = arrival.position;
        if WorldPosition::from_metres(position.x, position.y, position.z).is_none() {
            return Err(WorldError::NotFinite);
        }
        if !self.may_enter(arrival.character) {
            return Err(WorldError::NotInInstance(arrival.character));
        }
        Ok(())
    }

    fn spawn(
        &mut self,
        mut arrival: Arrival,
        motion: Motion,
        replication: Replication,
        last_time_synch_ms: u64,
        handoffs: u64,
        no_handoff_before_ms: u64,
    ) {
        nw_messages::hold_raw_id(self.raw_ids, arrival.number);
        let kept_effects = std::mem::take(&mut arrival.progression.status_effects);
        let arrival_masteries = tradeskill::arrival_masteries(
            &arrival.progression.masteries,
            arrival.backstory.as_deref(),
            &self.content,
        );
        let entity = self
            .world
            .spawn((
                Player {
                    connection: arrival.connection.clone(),
                    character: arrival.character,
                    name: arrival.name,
                    look: arrival.look,
                    gear: match &arrival.items {
                        Some(items) => {
                            let mut gear = Gear::carried_in(
                                &self.content.kits,
                                arrival.character,
                                items,
                                arrival.drawn_weapon,
                                arrival.active_weapon,
                            );
                            gear.restore_inventory(&self.content.kits, &arrival.inventory);
                            if !arrival.saved {
                                gear.mark_unsaved();
                            }
                            Some(gear)
                        }
                        None => Gear::starter(
                            &self.content.kits,
                            arrival.character,
                            arrival.backstory.as_deref(),
                        )
                        .map(|gear| gear.with_tools(&self.content.kits, &self.content.start_tools)),
                    },
                    item_changes: storage_search::ItemChanges::default(),
                    backstory: arrival.backstory,
                    number: arrival.number,
                    bundles_sent: replication.bundles_sent,
                    last_time_synch_ms,
                    interest: Interest::new(
                        self.settings.interest_radius_m,
                        self.settings.max_visible_players,
                    ),
                    slots: replication.slots,
                    own_card_sent: false,
                    own_card_due_ms: arrival.own_card_due_ms,
                    own_card_bundle: None,
                    login: arrival.login,
                    own_card_sent_ms: 0,
                    others_due_ms: if arrival.spawned { Some(0) } else { None },
                    client_context: arrival.client_context,
                    flow: arrival.flow,
                    leave_at_ms: None,
                    phasing_left_ms: None,
                    moved: false,
                    move_facing: None,
                    push: None,
                    tracked: TrackedBody::new(motion.position()),
                    input_counter: None,
                    state_number: None,
                    view: None,
                    view_sent: None,
                    slayer: slayer::SlayerTrack::default(),
                    remote_plan: replica::RemotePlan::default(),
                    throttle_cleared: false,
                    defer_throttle_clear: false,
                    auto_pin: arrival.auto_pin,
                    pvp_flagged: arrival.pvp_flagged,
                    own_places: OwnPlaces::default(),
                    footing: Footing::SPAWN,
                    footing_sent: Footing::SPAWN,
                    teleport_step: None,
                    refused_since_ms: None,
                    last_grounded: Some(motion.position()),
                    camera_held_until_ms: None,
                    looking_through_loadout: false,
                    loadout_change_due: false,
                    own_spawned_ms: None,
                    teleports: 0,
                    quest: None,
                    done_quests: arrival.quests.done.clone(),
                    quests_unsaved: !arrival.saved,
                    tracker_floor: arrival.quests.tracker_version,
                    experience: arrival.progression.experience,
                    experience_changed: false,
                    coins: arrival.progression.coins,
                    coins_changed: false,
                    loot: loot_bag::PlayerLoot::with_limits(&arrival.progression.loot_limits),
                    progression_saved: arrival.saved.then(|| arrival.progression.clone()),
                    masteries: progression::Masteries::of(
                        &arrival_masteries,
                        &self.content.progression,
                        arrival.mastery_versions,
                    )
                    .with_points(
                        &arrival.progression.weapon_points,
                        &self.content.progression,
                    )
                    .with_territory_points(&arrival.progression.territory_points)
                    .with_slots(
                        &arrival.progression.ability_slots,
                        &self.content.progression,
                    ),
                    attributes: points::Attributes::of(
                        &arrival.progression.attributes,
                        arrival.attribute_version,
                    ),
                    meta: meta_achievements::MetaState::of(
                        &arrival.progression.meta_counters,
                        &arrival.progression.meta_completed,
                    ),
                    title: arrival.progression.title,
                    title_changed: arrival.progression.title.is_some(),
                    level_sent: None,
                    game_master: arrival.game_master,
                    account_in_probation: arrival.account_in_probation,
                    account: arrival.account,
                    entitlements: arrival.entitlements.clone(),
                    station: None,
                    station_interaction: None,
                    faction: arrival.progression.faction,
                    course: quick_course::PlayerCourse::default(),
                    recipe_cooldowns: RecipeCooldownCounts::from_counts(
                        arrival
                            .progression
                            .recipe_cooldowns
                            .iter()
                            .map(|count| CooldownCount {
                                recipe: count.recipe_id.clone(),
                                count: count.count,
                                ends_ns: count.ends_ns,
                            }),
                    ),
                    last_interaction: None,
                    open_interaction: None,
                    open_board: None,
                    board_version: 0,
                    interacting: false,
                    territories: Vec::new(),
                    last_zone: None,
                    travel: travel::TravelState::of(arrival.progression.travel.clone()),
                    strikes: quest_objects::Strikes::default(),
                    detected: HashSet::new(),
                    // The client gets the running ones with the first bundles.
                    cooldowns: arrival
                        .progression
                        .consumable_cooldowns
                        .iter()
                        .map(|cooldown| ConsumableCooldown {
                            consumable: cooldown.consumable,
                            end_us: cooldown.end_us,
                            group: cooldown.group,
                        })
                        .collect(),
                    cooldowns_changed: !arrival.progression.consumable_cooldowns.is_empty(),
                    item_effects: item_calls::ItemEffects::default(),
                    status_effects: status_effects::StatusEffects::default(),
                    fall: fall_damage::FallTrack::default(),
                    health_sent: None,
                    deaths_door_sent: false,
                    health_max_sent: None,
                    life_ms: 0,
                    stamina: Stamina::default(),
                    abilities: abilities::AbilityRun::default(),
                    attack_playing: None,
                    // The clock of the last tick: a sim ticks before its first admit.
                    mount: mount::PlayerMount::of(
                        &arrival.progression.mounts,
                        self.last_now_ms,
                        self.server_ms,
                    ),
                    fishing: fishing::PlayerFishing::continuing(arrival.fishing_sequence),
                    music: music::PlayerMusicState::of(&arrival.progression.music),
                    notifications: notifications::ShownNotifications::of(
                        &arrival.progression.notifications,
                    ),
                    combat: CombatTimer::default(),
                    list_version: FIRST_VERSION,
                    extra_members: Vec::new(),
                    social: social_states::PlayerSocial::default(),
                    gathers: 0,
                    cyclic_turns: HashMap::new(),
                    node_versions: HashMap::new(),
                    bag_versions: HashMap::new(),
                    ended_gather: None,
                    statics: StaticViews::default(),
                    assets: AssetViews::default(),
                    solids: None,
                    arena: instances::ArenaPlayer::of(arrival.progression.arena.clone()),
                    game_mode: game_mode::GameModePlayer::default(),
                    mutation_view: mutations::ScheduleView::default(),
                    frozen: false,
                    handoffs,
                    no_handoff_before_ms,
                },
                Body { motion },
                Vitals::new(arrival.health, arrival.spawned),
            ))
            .id();
        self.by_connection.insert(arrival.connection, entity);
        self.camp_arrived(entity);
        self.restore_consumable_effects(entity, &kept_effects);
    }

    fn leave_all(&mut self, connections: &[ConnectionRef]) -> Vec<Departure> {
        connections
            .iter()
            .filter_map(|connection| self.leave(connection))
            .collect()
    }

    fn player_of(&mut self, character: Uuid) -> Option<Entity> {
        let mut query = self.world.query::<(Entity, &Player)>();
        query
            .iter(&self.world)
            .find(|(_, player)| player.character == character)
            .map(|(entity, _)| entity)
    }

    /// The departure of `entity` with all its items (outside the tutorial).
    fn departure(&self, entity: Entity) -> Option<Departure> {
        let player = self.world.get::<Player>(entity)?;
        let body = self.world.get::<Body>(entity)?;
        let saves = !self.is_tutorial();
        let items = player.gear.as_ref().filter(|_| saves).map(Gear::carried);
        let saved_quests = saves.then(|| quests_of(&self.quests, self.last_now_ms, player));
        let progression = saves.then(|| player.progression());
        Some(departure_of(player, body, items, saved_quests, progression))
    }
}

/// Where `shove` moves a player at `from` away from or toward a mob at `mob_at`, on the line
/// between them: away at the player's height, toward the mob at the mob's (it lands on the
/// mob's floor); `None` when they stand on one spot.
fn shoved_to(from: Position, mob_at: Position, shove: Shove) -> Option<Position> {
    let distance = from.horizontal_distance(mob_at);
    if distance <= f32::EPSILON {
        return None;
    }
    let (east, north) = (
        (from.x - mob_at.x) / distance,
        (from.y - mob_at.y) / distance,
    );
    let (base, metres) = match shove {
        Shove::To(metres) => (mob_at, metres),
        Shove::Away(metres) => (from, metres),
    };
    Some(Position {
        x: base.x + east * metres,
        y: base.y + north * metres,
        z: base.z,
    })
}

/// Queues `messages` for the client of `connection` on the reliable message channel; a message
/// that cannot be encoded is logged and left out.
pub(crate) fn send_now(
    outgoing: &mut Vec<Outgoing>,
    connection: &ConnectionRef,
    messages: &[ServerMessage],
) {
    let envelopes: Vec<Vec<u8>> = messages
        .iter()
        .filter_map(|message| {
            message
                .envelope()
                .inspect_err(|error| tracing::warn!(%error, "message not sent"))
                .ok()
        })
        .collect();
    if !envelopes.is_empty() {
        outgoing.push(Outgoing {
            connection: connection.clone(),
            channel: MESSAGE_CHANNEL,
            reliable: true,
            envelopes,
        });
    }
}

/// The messages when mob `mob` picks player `number` as its target (2415); a sailor's tutorial
/// AI also saw the player (955; live-tutorial note, section 7).
fn targeted(number: u64, mob: u64, tutorial_ai: bool) -> Vec<ServerMessage> {
    use nw_messages::facet::component;
    let mut messages = vec![
        ServerMessage::SelectedAsTarget(SelectedAsTargetMsg(TargetMsg {
            header: nw_messages::FacetHeader::of(component::AI_TARGETABLE, number),
            creature: facet_entity(mob),
        })),
        ServerMessage::HomingOverride(nw_messages::HomingOverrideMsg::clear(number)),
    ];
    if tutorial_ai {
        messages.push(ServerMessage::PlayerSeenByAi(PlayerSeenByAiMsg {
            header: nw_messages::FacetHeader::of_child(
                component::TUTORIAL_AI,
                component::TUTORIAL_AI_CHILD,
                mob,
            ),
        }));
    }
    messages
}

/// The messages when mob `mob`, whose `CharacterComponent` is `collision`, dies near player
/// `number`: it drops the player as its target (3916) and stops colliding (1793).
fn died(number: u64, mob: u64, collision: u64) -> [ServerMessage; 2] {
    use nw_messages::facet::component;
    [
        ServerMessage::UnselectedAsTarget(UnselectedAsTargetMsg(TargetMsg {
            header: nw_messages::FacetHeader::of(component::AI_TARGETABLE, number),
            creature: facet_entity(mob),
        })),
        ServerMessage::DisableCollision(DisableCollisionMsg {
            header: nw_messages::FacetHeader::of(collision, mob),
        }),
    ]
}

/// The display name string id of the ruin chest (the retail answer).
const CHEST_DISPLAY_NAME: &[u8] = b"@LootContainer_AncientSmall_DisplayName";
/// The validation result of an accepted interaction.
const INTERACTION_ACCEPTED: u8 = 1;

/// The entity number whose facet entity is that of the static entity `target` (its first eight
/// bytes reversed, as for a player).
fn facet_number(target: &[u8; nw_messages::wire::RAW_ID_LEN]) -> u64 {
    let mut first = [0; 8];
    first.copy_from_slice(&target[..8]);
    u64::from_be_bytes(first)
}

/// The answer to player `number`'s gather `request` (3743; ftue-static-entities note, section
/// 5): `status` 1 accepts it, 2 refuses a busy one; the chest's accept names it (`name`), an
/// open-world node's names nothing.
fn gather_answer(
    number: u64,
    request: InteractionRequest,
    status: u8,
    name: &[u8],
) -> ServerMessage {
    ServerMessage::InteractionValidated(InteractionValidatedMsg {
        header: nw_messages::FacetHeader::of(nw_messages::facet::component::INTERACTOR, number),
        target: request.target,
        target_entity: request.target_entity,
        interaction: request.interaction,
        kind: request.kind,
        status,
        name: name.to_vec(),
        extra: 0,
        // An id of unknown meaning: one per player and interaction.
        id: *Uuid::new_v5(
            &Uuid::from_u64_pair(number, request.interaction),
            &request.target,
        )
        .as_bytes(),
    })
}

/// The end of player `number`'s gather `request`: the interaction ends (3746) and the chest's
/// gathering stops (2724).
fn gather_end(number: u64, request: InteractionRequest) -> [ServerMessage; 2] {
    [
        ServerMessage::InteractionEnded(InteractionEndedMsg {
            header: nw_messages::FacetHeader::of(nw_messages::facet::component::INTERACTOR, number),
        }),
        gather_interrupted(request),
    ]
}

/// The gathering of the object of `request` stops (2724, to its gatherable controller).
fn gather_interrupted(request: InteractionRequest) -> ServerMessage {
    ServerMessage::GatheringInterrupted(GatheringInterruptedMsg {
        header: nw_messages::FacetHeader::of(
            nw_messages::facet::component::CHEST_GATHERABLE_CONTROLLER,
            facet_number(&request.target),
        ),
    })
}

fn departure_of(
    player: &Player,
    body: &Body,
    items: Option<Vec<CarriedItem>>,
    quests: Option<PlayerQuests>,
    progression: Option<PlayerProgression>,
) -> Departure {
    let inventory = items
        .as_ref()
        .and(player.gear.as_ref())
        .map(Gear::inventory);
    Departure {
        connection: player.connection.clone(),
        character: player.character,
        position: body.motion.position(),
        moved_at_ms: body.motion.moved_ms(),
        quests,
        items,
        inventory,
        progression,
    }
}

/// The quests of `player` to save or carry: its running quests (from `quests`, at `now_ms`) and
/// its done list.
fn quests_of(
    quests: &HashMap<u64, Vec<ActiveQuest>>,
    now_ms: u64,
    player: &Player,
) -> PlayerQuests {
    PlayerQuests {
        active: quests
            .get(&player.number)
            .into_iter()
            .flatten()
            .map(|quest| {
                let run = QuestRun {
                    mission: quest.take,
                    ..quest.run.export(now_ms)
                };
                (quest.def.id.clone(), run)
            })
            .collect(),
        done: player.done_quests.clone(),
        tracker_version: player.tracker_shown(),
    }
}

impl SimWorld {
    /// How far a push of player `entity` from `from` toward `to` goes: over the floor the
    /// players walked, to the first wall of its ground (the level's and the solids it sees).
    fn push_reach(&self, entity: Entity, from: Position, to: Position) -> Position {
        let solids = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.solids.clone());
        match solids {
            Some(solids) => {
                self.walked_floor
                    .reach(from, to, &LevelGround::new(&*self.ground, solids))
            }
            None => self.walked_floor.reach(from, to, &*self.ground),
        }
    }
}

/// What mob `entity` of the creature slice `asset` shows from its tree, when it runs one: the
/// numbers of the action it plays in the creature's files (`table`), its tag state, and its
/// facing once it turned.
fn tree_pose(
    mobs: &Mobs,
    table: &CreatureAnimations,
    entity: u64,
    asset: [u8; ASSET_ID_LEN],
) -> Option<TreePose> {
    let mind = mobs.tree(entity)?;
    let creature = table.get(&asset);
    let entry =
        mobs.shown(entity)
            .zip(creature)
            .and_then(|((shown, action, fragment), creature)| {
                Some(TreeEntry {
                    fragment: creature.fragment(fragment)?,
                    action: creature.action(action)?,
                    serial: shown.serial,
                })
            });
    let tag_names: Vec<&str> = mind.tags().iter().map(|tag| mobs.ai().name(*tag)).collect();
    Some(TreePose {
        entry,
        tags: creature.map_or([0; FRAG_TAGS_LEN], |creature| {
            creature.tag_state(&[], &tag_names)
        }),
        facing: mind.turned().then_some(mind.yaw()),
    })
}

/// What the bundle step of one tick reads: everyone's position, in a grid, and the cards of the
/// living mobs.
struct Neighbourhood<'a> {
    grid: &'a SpatialGrid<u64>,
    positions: HashMap<u64, Position>,
    cards: HashMap<u64, MobCard>,
    mob_looks: &'a MobLooks,
    /// The tutorial level's static and asset entities.
    level: &'a FtueLevel,
    /// The open world's gather nodes.
    nodes: &'a Nodes,
    /// The level's crafting stations; none in the tutorial.
    stations: Option<&'a crafting::Stations>,
    /// The level's quest objects that the client knows by GDE id.
    quest_objects: &'a QuestObjects,
    /// How near a player a node, station or loot bag is shown: the interest radius.
    view_radius_m: f32,
    /// The loot bags.
    bags: &'a loot_bag::LootBags,
    /// The musical performances.
    performances: &'a music::Performances,
    /// The level table and the weapon masteries.
    progression: &'a Progression,
    /// The week's mutated expeditions.
    mutations: &'a MutationCatalog,
    /// Unix seconds of this tick, for the mutation week.
    mutation_unix_s: u64,
    /// Every player as the players who see it get it this tick.
    replicas: HashMap<u64, Replica>,
    local_player_card: bool,
    now_ms: u64,
    server_time: u64,
}

impl Neighbourhood<'_> {
    /// The bundles and time messages of `players`, in their order.
    fn bundles_of(
        &self,
        players: &mut [(Mut<Player>, &Body, &Vitals)],
    ) -> Result<Bundled, MessageError> {
        let mut outgoing = Vec::new();
        let mut counted = 0_u64;
        for (player, body, vitals) in players.iter_mut() {
            if player.frozen {
                continue;
            }
            let own = player.number;
            let own_position = body.motion.position();
            let viewer = self.replicas.get(&own);
            let others = self
                .grid
                .near(own_position)
                .filter(|(number, _)| *number != own)
                .map(|(number, position)| {
                    let pinned = viewer
                        .zip(self.replicas.get(&number))
                        .is_some_and(|(viewer, other)| other.fights(number, own, viewer));
                    (number, position, pinned)
                });
            let seen = player.interest.update(own_position, others);
            // Until the client has spawned, other entities share the offline cache with the
            // own player (local-player-check note). After the time-sync they still wait
            // [`OTHER_ENTITY_SETTLE_MS`] so the first InGame bundle is only the own group
            // (live 2026-09-24: 20 beach first-cards 17 ms after InGame failed
            // StateReplication 4585).
            let just_in_game = player.others_due_ms == Some(OTHER_ENTITY_SETTLE_PENDING);
            if just_in_game {
                player.others_due_ms = Some(self.now_ms.saturating_add(OTHER_ENTITY_SETTLE_MS));
            }
            let settling = player
                .others_due_ms
                .is_some_and(|due| due > 0 && self.now_ms < due);
            let hold_others = self.local_player_card && (!vitals.spawned || settling);
            // The first InGame bundle stays own-only. Live 2026-09-24 dumped
            // statics and nodes 33 ms after InGame and failed StateReplication
            // 6077. Bags and nodes go on later ticks; mobs wait the settle.
            let hold_world = self.local_player_card && (!vitals.spawned || just_in_game);
            let mut numbers: Vec<u64> = if positions_only() || hold_others {
                Vec::new()
            } else {
                seen.entered
                    .into_iter()
                    .chain(seen.stayed)
                    .filter(|number| self.cards.get(number).is_none_or(|card| card.seen_by(own)))
                    .collect()
            };
            if !positions_only() && vitals.spawned && !hold_world {
                numbers.extend(player.statics.numbers());
                numbers.extend(player.assets.numbers());
                numbers.extend(self.nodes.numbers_near(own_position, self.view_radius_m));
                if let Some(stations) = self.stations {
                    numbers.extend(stations.numbers_near(own_position, self.view_radius_m));
                }
                numbers.extend(quest_objects::numbers_near(
                    self.quest_objects,
                    own_position,
                    self.view_radius_m,
                ));
                numbers.extend(player.social.group_entity_number());
                numbers.extend(
                    self.performances
                        .numbers_near(own_position, self.view_radius_m),
                );
            }
            // A bag is born at the kill. It must card on that tick (retail).
            if !positions_only() && vitals.spawned {
                numbers.extend(self.bags.numbers_near(own_position, self.view_radius_m));
            }
            numbers.sort_unstable();
            player.slots.keep_only(&numbers);
            // The other players this viewer gets this tick, by priority ([`replica`]).
            let remote: Vec<RemoteCandidate<'_>> = numbers
                .iter()
                .filter_map(|number| {
                    let replica = self.replicas.get(number)?;
                    let position = self.positions.get(number)?;
                    Some(RemoteCandidate {
                        number: *number,
                        distance_m: own_position.horizontal_distance(*position),
                        replica,
                        // A player without a slot yet gets one, and its card, below.
                        new: !player.slots.assigned().contains_key(number),
                        refresh: player.bundles_sent.wrapping_add(*number) % CARD_REFRESH_BUNDLES
                            == 0,
                    })
                })
                .collect();
            let step = player.bundles_sent;
            let shown_players: HashSet<u64> =
                player.remote_plan.pick(step, &remote).into_iter().collect();
            // Until its client spawned the player, the client keeps the own states in its
            // offline portrayal cache and merges every later record of the slot into the cached
            // one; such a merge crashes it (player-component note, crashes 2 and 3). So until
            // then the own group goes out whole, with the card, and the same bundle goes
            // out again on the resend interval below (a fresh build each tick would merge
            // new records into the cached card); after the spawn only the position and the
            // changes go, every tick. Retail sent the own card again only until the client
            // spawned and on a level change, never after the spawn
            // (newchar-retail-2026-09-18: 27 cards in the 9 s before the spawn, 6 at the
            // tutorial exit, none in the 17.5 min after).
            let first_card = !player.own_card_sent
                && self.local_player_card
                && self.now_ms >= player.own_card_due_ms;
            if first_card {
                player.own_card_sent = true;
                player.own_spawned_ms = Some(self.server_time);
                player.life_ms = self.server_time;
                player.slayer.spawned();
            }
            let own_sent = first_card || vitals.spawned || !self.local_player_card;
            let own_card = if first_card {
                OwnCard::Spawn
            } else {
                OwnCard::Without
            };
            // A move by the server goes once the client has spawned, in three states, one per
            // bundle (retail; [`TeleportStep`]).
            let can_move = own_card != OwnCard::Spawn && vitals.spawned;
            let step = if can_move && std::mem::take(&mut player.moved) {
                player.teleports = player.teleports.wrapping_add(1);
                Some(TeleportStep::Place)
            } else if can_move {
                player.teleport_step.take()
            } else {
                None
            };
            let clock = SimWorld::alc_clock(player, self.server_time);
            let teleport = step.map(|step| {
                player.teleport_step = step.next();
                tracing::debug!(
                    player = own,
                    teleports = player.teleports,
                    since_spawn_ms = clock.since_spawn_ms,
                    ?step,
                    "own teleport"
                );
                OwnTeleport::of(player.teleports, clock, player.move_facing, step)
            });
            let mut groups: Vec<BundleGroup> = Vec::new();
            if own_sent {
                // The whole gear and achievements go with the first card; later the
                // achievements send their changes, and the gear its whole states again after a
                // change.
                let first = own_card == OwnCard::Spawn;
                let achievements = if first {
                    Some(player.flow.first_achievements())
                } else {
                    player.flow.take_achievement_changes()
                };
                let gear = player.gear.as_mut().and_then(|gear| {
                    if first {
                        Some(gear.first_states())
                    } else {
                        gear.take_change()
                    }
                });
                // Retail sends one storage search items (7048) per changed item before the
                // bundle of the change, with the player's last search ([`storage_search`]).
                let item_changes = match (&gear, &player.gear) {
                    (Some(_), Some(held)) => {
                        let carried = held.carried();
                        let stored = held.stored_stacks();
                        let totals = held.stored_totals();
                        player
                            .item_changes
                            .messages(own, &carried, &stored, &totals)
                    }
                    _ => Vec::new(),
                };
                if !item_changes.is_empty() {
                    outgoing.push(Outgoing {
                        connection: player.connection.clone(),
                        channel: MESSAGE_CHANNEL,
                        reliable: true,
                        envelopes: item_changes
                            .iter()
                            .map(ServerMessage::envelope)
                            .collect::<Result<_, _>>()?,
                    });
                }
                // The quest tracker: the whole quest with the first card, then its changes.
                let objectives = player.quest.as_mut().and_then(|quest| {
                    if first {
                        Some(quest.take_first_state())
                    } else {
                        quest.take_change()
                    }
                });
                // The experience and the level: whole with the first card, then after each
                // change; a change of level sends the new level with the experience (retail
                // 899 at each level up, progression-and-gm-mode note section 3).
                let progression = if first {
                    let level = self.progression.level_of(player.experience);
                    player.level_sent = Some(level);
                    Some(ProgressionState::login_at_level(level, player.experience))
                } else if std::mem::take(&mut player.experience_changed) {
                    let level = self.progression.level_of(player.experience);
                    let leveled = player.level_sent != Some(level);
                    player.level_sent = Some(level);
                    Some(if leveled {
                        ProgressionState::level_up(level, player.experience)
                    } else {
                        ProgressionState::experience(player.experience)
                    })
                } else {
                    None
                };
                // The weapon masteries and their point pools: whole with the first card, then
                // after each change.
                let (categorical_progression, progression_points) = if first {
                    let (masteries, pools) = player.masteries.first_states(self.progression);
                    (Some(masteries), Some(pools))
                } else {
                    player.masteries.take_change(self.progression)
                };
                // The attribute points and the mapped abilities (progression note, section 6):
                // whole with the first card, then after a change.
                let player_level = self.progression.level_of(player.experience);
                let point_members = [
                    player
                        .attributes
                        .take_state(&self.progression.points, player_level, first)
                        .map(|state| BundleMember {
                            component: ATTRIBUTE_COMPONENT,
                            state: MemberState::Attributes(state),
                        }),
                    player
                        .masteries
                        .take_abilities(self.progression, first)
                        .map(|state| BundleMember {
                            component: ABILITY_COMPONENT,
                            state: MemberState::Abilities(state),
                        }),
                    // The shown title (2474; title choice module).
                    std::mem::take(&mut player.title_changed).then(|| BundleMember {
                        component: nw_messages::SOCIAL_COMPONENT,
                        state: nw_messages::title_state(player.title.unwrap_or_default()),
                    }),
                ];
                // The coins: whole with the first card, then after each change (retail own c60
                // with each quest reward, beach-quests note).
                let currency = (first || std::mem::take(&mut player.coins_changed))
                    .then(|| CurrencyState::of(player.coins));
                // The cooldowns and a heal after a potion; an ability's cooldown goes in the map of
                // the key that started it, as one changed entry (ability-use-wire note).
                let consumables = std::mem::take(&mut player.cooldowns_changed).then(|| {
                    player.list_version += 1;
                    FullList {
                        version: Some(player.list_version),
                        elements: player.cooldowns.clone(),
                    }
                });
                let mut keys: [Option<CooldownMap>; nw_messages::MAX_MAPPED_SLOTS] =
                    Default::default();
                for (key, cooldown) in player.abilities.take_cooldown_changes() {
                    player.list_version += 1;
                    let change = CooldownChange {
                        cooldown,
                        version: player.list_version,
                    };
                    match keys.get_mut(key) {
                        Some(Some(CooldownMap::Changes(changes))) => changes.push(change),
                        Some(slot) => *slot = Some(CooldownMap::Changes(vec![change])),
                        None => {}
                    }
                }
                let cooldowns = (consumables.is_some() || keys.iter().any(Option::is_some))
                    .then_some(CooldownsState { keys, consumables });
                // The health goes with each card; between cards, a change of it goes as its own
                // vitals change (retail: 40 to 110 ms after each damaging 3601, flags 3 on the
                // first damage from full health).
                let health = vitals.health.current();
                // A death and a new life go even with a card: the client shows its death screen
                // and takes its moves again only on them (open-world note).
                // Death's door goes first with its own block, then its end with the dead block
                // and the time spent in it (status effects note 2026-10-31).
                let life = match player.health_sent {
                    Some(sent) if sent > 0 && health == 0 => {
                        player.deaths_door_sent = vitals.downed();
                        Some(if vitals.downed() {
                            VitalsData::player_at_deaths_door(player.life_ms)
                        } else {
                            VitalsData::player_dead(player.life_ms)
                        })
                    }
                    Some(0) if health == 0 && player.deaths_door_sent && !vitals.downed() => {
                        player.deaths_door_sent = false;
                        Some(VitalsData::player_dead_after_door(
                            player.life_ms,
                            vitals.deaths_door_ms.unwrap_or_default(),
                        ))
                    }
                    Some(0) if health > 0 => {
                        player.deaths_door_sent = false;
                        player.life_ms = self.server_time;
                        Some(VitalsData::player_alive(self.server_time))
                    }
                    _ => None,
                };
                // A new max goes as its own vitals change too (retail: `healthMax` alone after a
                // level up or a Constitution spend).
                let health_max = vitals.health.max();
                let max = (own_card == OwnCard::Without
                    && player
                        .health_max_sent
                        .is_some_and(|sent| sent != health_max))
                .then_some(health_max);
                let health_change = ((own_card == OwnCard::Without
                    && player.health_sent != Some(health))
                    || life.is_some()
                    || max.is_some())
                .then(|| HealthChange {
                    health,
                    first_damage: player.health_sent.is_some()
                        && player.health_sent == player.health_max_sent,
                    life,
                    max,
                });
                player.health_sent = Some(health);
                player.health_max_sent = Some(health_max);
                let stamina = player.stamina.take_change();
                player.combat.tick(self.now_ms);
                let combat_status = player.combat.take_change(self.now_ms, self.server_time);
                if let Some(state) = objectives.as_ref() {
                    tracing::debug!(
                        player = player.number,
                        first,
                        tracked = ?state.tracked.as_ref().map(|list| (list.version, list.elements.len())),
                        active = ?state.active.as_ref().map(|list| (list.version, list.elements.len())),
                        tasks = ?state.tasks.as_ref().map(|list| (list.version, list.elements.len())),
                        "objectives sent"
                    );
                }
                if let Some(state) = progression.as_ref() {
                    tracing::debug!(player = player.number, first, ?state, "progression sent");
                }
                let states = PlayerStates {
                    character_id: *player.character.as_bytes(),
                    look: player.look,
                    health: Some((vitals.health.current(), vitals.health.max())),
                    gear: gear.as_ref(),
                    achievements: achievements.as_ref(),
                    objectives: objectives.as_ref(),
                    progression,
                    categorical_progression: categorical_progression.as_ref(),
                    progression_points: progression_points.as_ref(),
                    currency,
                    health_change,
                    cooldowns: cooldowns.as_ref(),
                    stamina,
                    combat_status,
                    mana: player.abilities.mana.take_change(),
                };
                // Every own state carries the own clock, and the place whenever it changed, moving
                // or not, as retail's ([`OwnPlaces`]); not
                // while its moves are refused: that body is the last good one, behind the client
                // (live 2026-09-18: 529 refused moves while running and dodging). The client
                // needs the place: without it (run 22, 2026-09-26) it could not move on the ship and
                // hung in the air after the drop into the sea. The place is only this server's
                // estimate of the body, so the patched client does not correct itself toward it
                // (`scripts/patch-client.py`, own-movement correction): every correction pulled the
                // client and moved the camera the estimate follows (run 21).
                let plain_tick = own_card != OwnCard::Spawn && teleport.is_none();
                let hold_throttle = std::mem::take(&mut player.defer_throttle_clear);
                let tick = OwnTick {
                    clock,
                    with_position: player.refused_since_ms.is_none() || player.push.is_some(),
                    clear_throttle: plain_tick && !player.throttle_cleared && !hold_throttle,
                    slayer: player.slayer.take_due(),
                    // Retail sent `rotation` and `lookDir` in most own states; the lock holds
                    // only while the client has a current look (lock-on note, section 15.2).
                    view: player.view.filter(|view| Some(*view) != player.view_sent),
                    // Retail's server moves carry no slot 1: a change waits for a plain state.
                    footing: if plain_tick {
                        player.footing.change_since(player.footing_sent)
                    } else {
                        (None, None)
                    },
                };
                if tick.view.is_some() {
                    player.view_sent = tick.view;
                }
                if tick.clear_throttle {
                    player.throttle_cleared = true;
                }
                if let Some(mut group) = own_group(
                    own,
                    states,
                    own_position,
                    own_card,
                    teleport,
                    tick,
                    &mut player.own_places,
                )? {
                    if plain_tick {
                        player.footing_sent = player.footing;
                    }
                    for member in point_members.into_iter().flatten() {
                        put_member(&mut group.members, member);
                    }
                    if let Some(member) = player.status_effects.take_member(first) {
                        put_member(&mut group.members, member);
                    }
                    if let Some(member) = player.mount.take_own_member(first) {
                        put_member(&mut group.members, member);
                    }
                    if let Some(member) = player.fishing.take_own_member(first) {
                        put_member(&mut group.members, member);
                    }
                    if let Some(member) = player.music.take_own_member(first, self.server_time) {
                        put_member(&mut group.members, member);
                    }
                    if achievements.is_some() {
                        player.mount.achievements_changed();
                    }
                    if std::mem::take(&mut player.loadout_change_due) {
                        put_looking_through_loadout(
                            &mut group.members,
                            player.looking_through_loadout,
                        );
                    }
                    // Before the position, one member per component: appended after it, a
                    // second arena state (2406) followed the own ALC of the open-world arrival
                    // and the client failed to read the bundle (run 17, beach: stateSequence 9).
                    for member in player.extra_members.drain(..) {
                        put_member(&mut group.members, member);
                    }
                    groups.push(group);
                }
                // The weekly mutations (slot 2), once the client spawned and at each new week.
                if vitals.spawned {
                    groups.extend(mutations::schedule_group(
                        self.mutations,
                        self.mutation_unix_s,
                        &mut player.mutation_view,
                    )?);
                }
            }
            if let Some(level_info) = player.flow.level_info_due(self.now_ms) {
                outgoing.push(Outgoing {
                    connection: player.connection.clone(),
                    channel: MESSAGE_CHANNEL,
                    reliable: true,
                    envelopes: vec![level_info.envelope()?],
                });
            }
            let statics_ids = player.flow.phase_id().map(|phase_id| {
                let player_gde_ref = raw_id_of_entity(own);
                let mut player_facet = [0; FACET_LEN];
                player_facet.copy_from_slice(&player_gde_ref[..FACET_LEN]);
                player_facet.reverse();
                ftue_entities::SessionIds {
                    player_gde_ref,
                    player_facet,
                    phase_id,
                }
            });
            for number in numbers {
                if (music::PERFORMANCE_ENTITY_BASE..crafting::STATION_ENTITY_BASE).contains(&number)
                {
                    if let Some(slot) = player.slots.slot(number) {
                        let player = &mut **player;
                        groups.extend(self.performances.group(
                            number,
                            slot,
                            own,
                            own_position,
                            &mut player.music.seen,
                        ));
                    }
                    continue;
                }
                if (social_states::GROUP_ENTITY_BASE..music::PERFORMANCE_ENTITY_BASE)
                    .contains(&number)
                {
                    let player = &mut **player;
                    if let Some(slot) = player.slots.slot(number) {
                        groups.extend(player.social.group_entity_group(number, slot));
                    }
                    continue;
                }
                if (crafting::STATION_ENTITY_BASE..loot_bag::BAG_ENTITY_BASE).contains(&number) {
                    let group = self
                        .stations
                        .zip(player.slots.slot(number))
                        .and_then(|(stations, slot)| stations.group(number, slot));
                    if group.is_some() {
                        tracing::debug!(player = player.number, station = number, "station shown");
                    }
                    groups.extend(group);
                    continue;
                }
                if (loot_bag::BAG_ENTITY_BASE..GATHERABLE_ENTITY_BASE).contains(&number) {
                    if let Some(slot) = player.slots.slot(number) {
                        let player = &mut **player;
                        groups.extend(self.bags.group(number, slot, &mut player.bag_versions));
                    }
                    continue;
                }
                if (GATHERABLE_ENTITY_BASE..STATIC_ENTITY_BASE).contains(&number) {
                    if let Some(slot) = player.slots.slot(number) {
                        let player = &mut **player;
                        groups.extend(self.nodes.group(number, slot, &mut player.node_versions));
                    }
                    continue;
                }
                if number >= quest_objects::QUEST_OBJECT_ENTITY_BASE {
                    groups.extend(
                        player.slots.slot(number).and_then(|slot| {
                            quest_objects::group(self.quest_objects, number, slot)
                        }),
                    );
                    continue;
                }
                if number >= STATIC_ENTITY_BASE {
                    let (Some(ids), Some(slot)) = (statics_ids, player.slots.slot(number)) else {
                        continue;
                    };
                    groups.extend(if number >= ASSET_ENTITY_BASE {
                        player.assets.group(number, slot, self.level, &ids)
                    } else {
                        player
                            .statics
                            .group(number, slot, &self.level.statics, &ids)
                    });
                    continue;
                }
                let seen = match (self.cards.get(&number), self.positions.get(&number)) {
                    (Some(card), _) => Ok(card),
                    (None, Some(position)) => Err(*position),
                    (None, None) => continue,
                };
                // Every slot in use: the entity is not sent until one is free.
                let Some(slot) = player.slots.slot(number) else {
                    continue;
                };
                // Staggered by the entity number, so the cards of a crowd do not all come in one
                // bundle.
                let refresh = player.bundles_sent.wrapping_add(number) % CARD_REFRESH_BUNDLES == 0;
                let carded = slot.new || refresh;
                // A mob card, else a player's position.
                groups.extend(match seen {
                    Ok(card) => {
                        let look = self.mob_looks.look(card.state.vitals_id, card.asset);
                        // A mob with a prefab gets its card once, as on retail: the card makes
                        // the client load the creature slice, and a card again starts that
                        // load over, so the mob never shows (live 2026-09-12).
                        let carded = if look.is_some() { slot.new } else { carded };
                        let group = mob_group(slot.interest, card, look, carded)?;
                        if let Some(group) = &group {
                            log_mob_group(number, carded, group);
                        }
                        group
                    }
                    Err(position) => match self.replicas.get(&number) {
                        Some(replica) if shown_players.contains(&number) => {
                            replica.group(slot.interest, number, position, carded)?
                        }
                        Some(_) => None,
                        None => remote_group(
                            slot.interest,
                            number,
                            EntityKind::PLAYER,
                            position,
                            carded,
                        )?,
                    },
                });
            }
            counted += groups
                .iter()
                .map(|group| group.members.len() as u64)
                .sum::<u64>();
            let mut envelopes = Vec::new();
            for bundle in bundles(
                player.bundles_sent,
                player.client_context,
                &mut player.slots,
                groups,
            )? {
                player.bundles_sent = player.bundles_sent.saturating_add(1);
                envelopes.push(bundle);
            }
            // The own spawn card goes reliably, as retail's first bundle does, and is
            // kept: until the client spawns, the same bundle goes out again every
            // [`OWN_CARD_RESEND_MS`], because the client drops the bundles that reach
            // it while its level context is down (live 2026-09-22, the beach switch).
            if first_card {
                player.own_card_bundle = Some(envelopes.clone());
                player.own_card_sent_ms = self.now_ms;
            }
            let reliable = reliable_bundles() || first_card;
            for bundle in envelopes {
                outgoing.push(Outgoing {
                    connection: player.connection.clone(),
                    channel: REPLICATION_CHANNEL,
                    reliable,
                    envelopes: vec![bundle],
                });
            }
            let resend_due = player.login
                && !vitals.spawned
                && player.own_card_bundle.is_some()
                && self.now_ms.saturating_sub(player.own_card_sent_ms) >= OWN_CARD_RESEND_MS;
            if resend_due {
                player.own_card_sent_ms = self.now_ms;
                let card = player.own_card_bundle.clone().unwrap_or_default();
                for bundle in card {
                    outgoing.push(Outgoing {
                        connection: player.connection.clone(),
                        channel: REPLICATION_CHANNEL,
                        reliable: true,
                        envelopes: vec![bundle],
                    });
                }
            }
            if self.now_ms.saturating_sub(player.last_time_synch_ms) >= TIME_SYNCH_INTERVAL_MS {
                player.last_time_synch_ms = self.now_ms;
                outgoing.push(Outgoing {
                    connection: player.connection.clone(),
                    channel: MESSAGE_CHANNEL,
                    reliable: true,
                    envelopes: vec![
                        ServerMessage::TimeSynch(TimeSynchMsg::at_server_ms(self.server_time))
                            .envelope()?,
                    ],
                });
            }
        }
        Ok(Bundled {
            outgoing,
            members: counted,
            ..Bundled::default()
        })
    }
}

/// The offer that hands `player` to another sim, with its run of its open-world `quest`.
/// `enter_at` is `Some` for a player that leaves this level for another one (the end of the
/// tutorial): it appears there at that position, the taker sends the level change, and the
/// client's entity slots start empty.
fn handoff_offer(
    player: &Player,
    body: &Body,
    vitals: &Vitals,
    now_ms: u64,
    enter_at: Option<Position>,
    quests: PlayerQuests,
) -> HandoffOffer {
    let state = body.motion.export(now_ms);
    let enters = enter_at.is_some();
    HandoffOffer {
        connection: player.connection.clone(),
        character: player.character,
        name: player.name.clone(),
        entity: player.number,
        gde_ref: raw_id_of_entity(player.number),
        position: Point::from(enter_at.unwrap_or(state.position)),
        move_age_ms: state.move_age_ms,
        budget_m: state.budget_m,
        bundles_sent: player.bundles_sent,
        slots: if enters {
            Vec::new()
        } else {
            player
                .slots
                .assigned()
                .iter()
                .map(|(entity, interest)| (*entity, *interest))
                .collect()
        },
        pending_stops: if enters {
            Vec::new()
        } else {
            player.slots.pending_stops().to_vec()
        },
        time_synch_age_ms: now_ms.saturating_sub(player.last_time_synch_ms),
        handoffs: player.handoffs.saturating_add(1),
        health: vitals.health.current(),
        look: player.look.map(|look| look.choices()),
        backstory: player.backstory.clone(),
        phasing_sequence: player.flow.phasing_sequence(),
        client_context: player.client_context,
        enter_level: enters,
        items: player.gear.as_ref().map(Gear::carried),
        inventory: player
            .gear
            .as_ref()
            .map(Gear::inventory)
            .unwrap_or_default(),
        drawn_weapon: player.gear.as_ref().and_then(Gear::drawn),
        active_weapon: player.gear.as_ref().map(Gear::active),
        mastery_versions: player.masteries.versions(),
        attribute_version: player.attributes.version(),
        fishing_sequence: player.fishing.next_sequence(),
        game_master: player.game_master,
        account_in_probation: player.account_in_probation,
        account: player.account,
        entitlements: player.entitlements.iter().cloned().collect(),
        quests,
        progression: player.progression(),
        achievement_version: player.flow.achievement_version(),
        pvp_flagged: player.pvp_flagged,
        auto_pin: player.auto_pin,
    }
}

/// The items of `carried` as item tasks read them: item id, quantity and whether it is worn.
/// The territories (zones) of each area or point of interest the client named (`areas`).
fn player_territories(content: &PlayerContent, areas: &[u16]) -> Vec<u32> {
    let mut territories: Vec<u32> = areas
        .iter()
        .filter_map(|id| content.charting.territory_of(u32::from(*id)))
        .collect();
    territories.sort_unstable();
    territories.dedup();
    territories
}

/// The families of item `item_id`: its `ItemClass` tags and its weapon mastery
/// (`SwordAbilityTable`), for the family tasks of quests.
fn item_families(content: &PlayerContent, item_id: &str) -> Vec<String> {
    content
        .item_rolls
        .item(item_id)
        .map(|row| row.classes.clone())
        .unwrap_or_default()
        .into_iter()
        .chain(
            content
                .kill_rewards
                .weapon_mastery(item_id)
                .map(str::to_owned),
        )
        .collect()
}

/// Whether item `item_id` is of family `family` ([`item_families`]).
fn item_of_family(content: &PlayerContent, item_id: &str, family: &str) -> bool {
    item_families(content, item_id)
        .iter()
        .any(|seen| seen.eq_ignore_ascii_case(family))
}

fn held_items(carried: &[CarriedItem]) -> Vec<(&str, u32, bool)> {
    carried
        .iter()
        .map(|item| {
            (
                item.item_id.as_str(),
                item.quantity,
                matches!(item.place, ItemPlace::Worn(_)),
            )
        })
        .collect()
}

/// Takes `count` of `item_id` (any case) from `gear` at time `time_ns` for a quest's
/// `TaskRemoveItem` (its `TargetQty`). When the player holds no more than that, every stack goes
/// ([`Gear::remove`]: a worn weapon is put away as retail's tutorial sword). Else the bag gives
/// first and worn stacks give the rest one at a time (Inferred: retail's only capture of the task,
/// the tutorial sword, held one worn item and none in the bag).
fn remove_quest_item(gear: &mut Gear, item_id: &str, count: u32, time_ns: u64) {
    let carried: Vec<CarriedItem> = gear
        .carried()
        .into_iter()
        .filter(|item| item.item_id.eq_ignore_ascii_case(item_id))
        .collect();
    let held: u32 = carried.iter().map(|item| item.quantity).sum();
    if held <= count {
        let mut ids: Vec<&str> = carried.iter().map(|item| item.item_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            gear.remove(id, time_ns);
        }
        return;
    }
    let from_bag = gear.bag_count(item_id).min(count);
    gear.take(item_id, from_bag);
    let mut left = count - from_bag;
    for item in &carried {
        let ItemPlace::Worn(slot_id) = item.place else {
            continue;
        };
        let from_slot = left.min(item.quantity);
        for _ in 0..from_slot {
            gear.use_one(slot_id);
        }
        left -= from_slot;
    }
}

/// The sim of `table` that owns the cell at `position`; `None` when the table does not fit its
/// own size or no live sim owns that cell.
fn owner_in(table: &CellTable, position: Position) -> Option<SimSlot> {
    let cell = table.cell_size_m;
    let grid = CellGrid::new(table.columns as f32 * cell, table.rows as f32 * cell, cell).ok()?;
    let owners = CellOwners::from_cells(grid, table.owners.clone())?;
    let index = owners.owner_at(position)?;
    table.sims.get(usize::from(index)).cloned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nw_bus::CellTable;
    use nw_carrier::sender::MAX_CHUNK_PAYLOAD;
    use nw_content::gathering::{Gatherable, Gatherables};
    use nw_content::loot::{LootContext, LootRow, LootSource, LootTables, MobLoot, MobLootRow};
    use nw_messages::{
        ALC_TYPE_INDEX, GDE_METADATA_TYPE_INDEX, InputPosition, LOCAL_PLAYER_INTEREST,
        LOGIN_MESSAGE_TYPES, LOOK_CHOICES, MemberState, OPEN_WORLD_LEVEL, PAPERDOLL_TYPE_INDEX,
        PLAYER_APPEARANCE_TYPE_INDEX, PLAYER_EMPTY_COMPONENTS, POS_REL_CHILD,
        PlayerAppearanceState, ReplicatedStateBundle, SlotRead, SlotUpdate, VITALS_TYPE_INDEX,
        VitalsState,
    };
    use nw_reflect::{Envelope, encode_server_messages};
    use nw_sim_core::test_support::{FlatGround, NoGround};
    use nw_sim_core::{
        CellGrid, CellOwners, GroundCheck, MoveLimits, MoveRefusal, TUTORIAL_EXIT, TUTORIAL_START,
    };

    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB};
    use crate::combat::tests::test_combat;
    use crate::mobs::VitalsSpawn;

    /// How many of `item_id` `gear` holds in the bag and worn.
    fn bag_and_worn(gear: &Gear, item_id: &str) -> (u32, u32) {
        gear.carried()
            .iter()
            .filter(|item| item.item_id == item_id)
            .fold((0, 0), |(bag, worn), item| match item.place {
                ItemPlace::Bag(_) => (bag + item.quantity, worn),
                ItemPlace::Worn(_) => (bag, worn + item.quantity),
            })
    }

    #[test]
    fn a_quest_removal_takes_its_target_quantity_from_the_bag_first() {
        const IN_BAG: u32 = 2;
        const IN_QUICK_SLOT: u32 = 3;
        const TARGET_QTY: u32 = 4;
        let items = [
            CarriedItem {
                item_id: gear::tests::FLINT.to_owned(),
                quantity: TARGET_QTY,
                place: ItemPlace::Bag(0),
                state: nw_bus::ItemState::default(),
            },
            CarriedItem {
                item_id: gear::tests::POTION.to_owned(),
                quantity: IN_BAG,
                place: ItemPlace::Bag(1),
                state: nw_bus::ItemState::default(),
            },
            CarriedItem {
                item_id: gear::tests::POTION.to_owned(),
                quantity: IN_QUICK_SLOT,
                place: ItemPlace::Worn(QUICK_SLOT_1),
                state: nw_bus::ItemState::default(),
            },
        ];
        let mut gear = Gear::carried_in(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            &items,
            None,
            None,
        );
        // One of a stack (the `TargetQty` 1 of each `BearUrine_Gift` task), in any case.
        remove_quest_item(
            &mut gear,
            &gear::tests::FLINT.to_lowercase(),
            1,
            SERVER_TIME,
        );
        assert_eq!(bag_and_worn(&gear, gear::tests::FLINT), (TARGET_QTY - 1, 0));
        // The bag gives its two, the quick slot the rest.
        remove_quest_item(&mut gear, gear::tests::POTION, TARGET_QTY, SERVER_TIME);
        assert_eq!(
            bag_and_worn(&gear, gear::tests::POTION),
            (0, IN_BAG + IN_QUICK_SLOT - TARGET_QTY)
        );
        // A count above what is held takes what there is.
        remove_quest_item(&mut gear, gear::tests::FLINT, TARGET_QTY, SERVER_TIME);
        assert_eq!(bag_and_worn(&gear, gear::tests::FLINT), (0, 0));
    }

    #[test]
    fn a_quest_removal_of_all_the_player_holds_takes_the_worn_sword() {
        let mut gear = Gear::starter(
            &gear::tests::kits(),
            gear::tests::CHARACTER,
            Some(gear::tests::BACKSTORY),
        )
        .expect("gear");
        assert_eq!(bag_and_worn(&gear, gear::tests::SWORD), (0, 1));
        // `01_SoleSurvivor2_Tutorial_RemoveLongsword`: `TargetQty` 1 of the worn sword.
        remove_quest_item(&mut gear, gear::tests::SWORD, 1, SERVER_TIME);
        assert_eq!(bag_and_worn(&gear, gear::tests::SWORD), (0, 0));
    }

    /// The largest entity number that fits the 5-byte prefix code (35 bits).
    const LARGEST_NUMBER_SHIFT: u32 = 29;

    const RADIUS: f32 = 100.0;
    pub(super) const NOW: u64 = 10_000;
    pub(super) const SERVER_TIME: u64 = 7;
    /// A saved look: male, face05, skin01, hairstyle65m (player-appearance note).
    const SAVED_LOOK: [u8; LOOK_CHOICES] = [0, 5, 0, 65, 44, 4, 4, 11, 0, 6, 26, 14];
    /// 5 m/s with no margin and a burst of 1 s: at most 5 m in one move.
    const SPEED_MPS: f32 = 5.0;
    const BURST_SECONDS: f32 = 1.0;
    const GROUND_TOLERANCE_M: f32 = 2.0;
    const MAX_ABOVE_GROUND_M: f32 = 150.0;
    const LEVEL_M: f32 = 14_336.0;
    pub(super) const CELL_M: f32 = 2048.0;
    /// The first column of the second sim of two (columns 3 to 6).
    pub(super) const BORDER_X: f32 = 3.0 * CELL_M;
    const MARGIN_M: f32 = 5.0;
    const RETRY_MS: u64 = 1000;
    const GHOST_TTL_MS: u64 = 1000;
    pub(super) const MAX_PLAYERS: u32 = 3;
    pub(super) const MANY: usize = 1000;
    const TWO: usize = 2;
    const ONE_SECOND_MS: u64 = 1000;
    const A_BIT_LATER_MS: u64 = 100;
    const CLOSE_M: f32 = 0.01;
    const GENERATION: u64 = 5;
    const BUNDLES_BEFORE: u64 = 900;
    const GHOST_NUMBER: u64 = 99;
    const RESUME_BASE: u64 = 1 << 32;

    pub(super) fn slot(name: &str) -> SimSlot {
        SimSlot {
            id: BusId::try_from(name.to_owned()).expect("id"),
            generation: GENERATION,
        }
    }

    /// The sea height of the test level: below the test players.
    pub(super) const OCEAN_LEVEL_M: f32 = 20.0;

    pub(super) fn settings(max_visible_players: usize, max_players: u32) -> WorldSettings {
        WorldSettings {
            interest_radius_m: RADIUS,
            max_visible_players,
            max_players,
            limits: MoveLimits::new(
                SPEED_MPS,
                0.0,
                BURST_SECONDS,
                GROUND_TOLERANCE_M,
                MAX_ABOVE_GROUND_M,
            )
            .expect("limits"),
            bounds: grid().bounds(),
            handoff_margin_m: MARGIN_M,
            handoff_retry_ms: RETRY_MS,
            ghost_ttl_ms: GHOST_TTL_MS,
            tick_threads: 1,
            local_player_card: true,
            level: OPEN_WORLD_LEVEL,
            ocean_level_m: OCEAN_LEVEL_M,
            instance_role: false,
            instance_request_timeout_ms: INSTANCE_REQUEST_TIMEOUT_MS,
        }
    }

    /// The instance request wait of the test worlds.
    const INSTANCE_REQUEST_TIMEOUT_MS: u64 = 3000;

    fn world_of(name: &str, settings: WorldSettings) -> SimWorld {
        SimWorld::new(
            settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot(name),
            test_combat(),
            Arc::default(),
        )
    }

    pub(super) fn world() -> SimWorld {
        world_of("sim-a", settings(MANY, MAX_PLAYERS))
    }

    /// A spawn of the test mob type at (`x`, 10).
    pub(super) fn test_spawn(x: f32) -> VitalsSpawn {
        VitalsSpawn {
            vitals_id: nw_formats::objectstream::lowercase_crc32(TEST_MOB),
            level: TEST_LEVEL,
            position: [x, 10.0, 50.0],
            ..VitalsSpawn::default()
        }
    }

    /// A world of `sim-a` with the test mob spawns `spawns`.
    pub(super) fn world_with_spawns(spawns: Vec<VitalsSpawn>) -> SimWorld {
        world_with_spawns_in(settings(MANY, MAX_PLAYERS), spawns)
    }

    /// [`world_with_spawns`] with `settings`.
    pub(super) fn world_with_spawns_in(
        settings: WorldSettings,
        spawns: Vec<VitalsSpawn>,
    ) -> SimWorld {
        let mut combat = test_combat();
        combat.spawns = spawns.into();
        SimWorld::new(
            settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::default(),
        )
    }

    fn grid() -> CellGrid {
        CellGrid::new(LEVEL_M, LEVEL_M, CELL_M).expect("grid")
    }

    /// The table of `names` in column stripes.
    pub(super) fn table(names: &[&str]) -> CellTable {
        let indexes: Vec<u16> = (0..).take(names.len()).collect();
        let owners = CellOwners::columns(grid(), &indexes);
        CellTable {
            version: 1,
            cell_size_m: CELL_M,
            columns: grid().columns(),
            rows: grid().rows(),
            sims: names.iter().map(|name| slot(name)).collect(),
            owners: owners.cells().to_vec(),
        }
    }

    pub(super) fn connection(number: u64) -> ConnectionRef {
        ConnectionRef {
            gateway: BusId::try_from("gw-1".to_owned()).expect("id"),
            connection: number,
        }
    }

    fn at(x: f32) -> Position {
        Position {
            x,
            y: 10.0,
            z: 50.0,
        }
    }

    /// The character name of test player `number`.
    fn player_name(number: u64) -> String {
        format!("Player{number}")
    }

    pub(super) fn admit_at(number: u64, x: f32, login: bool) -> Admit {
        Admit {
            connection: connection(number),
            account: Uuid::new_v4(),
            character: Uuid::new_v4(),
            name: player_name(number),
            entity: number,
            position: Point {
                x,
                y: 10.0,
                z: 50.0,
            },
            login,
            handoffs: 0,
            bundles_sent: 0,
            look: None,
            backstory: None,
            items: None,
            inventory: PlayerInventory::default(),
            quests: PlayerQuests::default(),
            progression: PlayerProgression::default(),
            game_master: false,
            account_in_probation: false,
            entitlements: Vec::new(),
        }
    }

    pub(super) fn join(world: &mut SimWorld, number: u64, x: f32) {
        world
            .admit(&admit_at(number, x, true), NOW, SERVER_TIME)
            .expect("admit");
        // The combat tests treat a joined player as already in the world, so end its spawn
        // protection (a real client does this with its first time sync request).
        let entity = world.player_by_number(number).expect("player");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
    }

    #[test]
    fn a_shove_moves_the_player_and_holds_its_client_moves() {
        const MOB_X: f32 = CELL_M / 2.0;
        const SHOVE_MS: u64 = 200;
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + 4.0);
        tick(&mut world, NOW);
        let entity = world.player_by_number(1).expect("player");
        let at = |world: &SimWorld| {
            world
                .world
                .get::<Body>(entity)
                .expect("body")
                .motion
                .position()
        };
        let start = at(&world);
        let pull = Order::Shove {
            mob: crate::mobs::mob_entity(0),
            shove: Shove::To(2.0),
            length_ms: SHOVE_MS,
        };
        assert!(world.mob_order(entity, 1, &pull, NOW));
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .push
                .is_none(),
            "no push over floor nobody walked on"
        );
        // The players walked between the mob and the player: open floor.
        for step in 0..=16 {
            #[allow(clippy::cast_precision_loss, reason = "a few steps")]
            let x = MOB_X + step as f32 / 4.0;
            world.walked_floor.walked(Position { x, ..start });
        }
        assert!(world.mob_order(entity, 1, &pull, NOW));
        assert!(matches!(
            world.move_to(&connection(1), start, NOW + 1),
            Err(MoveError::Pushed)
        ));
        world.tick_pushes(NOW + SHOVE_MS / 2);
        assert!((at(&world).x - (MOB_X + 3.0)).abs() < 1e-3, "halfway");
        world.tick_pushes(NOW + SHOVE_MS);
        assert!((at(&world).x - (MOB_X + 2.0)).abs() < 1e-3, "2 m in front");
        assert!(
            world.world.get::<Player>(entity).expect("player").moved,
            "the end goes as a server move"
        );
        world.tick_pushes(NOW + SHOVE_MS + 1);
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .push
                .is_none()
        );
        assert!(!matches!(
            world.move_to(&connection(1), at(&world), NOW + SHOVE_MS + 2),
            Err(MoveError::Pushed)
        ));
    }

    #[test]
    fn a_dead_player_is_not_shoved_or_swung_at() {
        const MOB_X: f32 = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + 4.0);
        tick(&mut world, NOW);
        let entity = world.player_by_number(1).expect("player");
        let start = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        for step in 0..=16 {
            #[allow(clippy::cast_precision_loss, reason = "a few steps")]
            let x = MOB_X + step as f32 / 4.0;
            world.walked_floor.walked(Position { x, ..start });
        }
        let max = world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .max();
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .health
            .take(max);
        let mob = crate::mobs::mob_entity(0);
        let pull = Order::Shove {
            mob,
            shove: Shove::To(2.0),
            length_ms: 200,
        };
        assert!(world.mob_order(entity, 1, &pull, NOW));
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .push
                .is_none(),
            "no pull of a body"
        );
        let swings_before = world.scripted_swings.len();
        let swing = Order::Swing {
            mob,
            mob_move: encounter::tests::any_mob_move(),
        };
        assert!(world.mob_order(entity, 1, &swing, NOW));
        assert_eq!(
            world.scripted_swings.len(),
            swings_before,
            "no swing at a body"
        );
    }

    #[test]
    fn a_push_away_goes_further_from_the_mob() {
        let mob = Position {
            x: 0.0,
            y: 0.0,
            z: 22.0,
        };
        let player = Position {
            x: 3.0,
            y: 4.0,
            z: 22.5,
        };
        let pushed = shoved_to(player, mob, Shove::Away(5.0)).expect("pushed");
        assert!((pushed.horizontal_distance(mob) - 10.0).abs() < 1e-4);
        assert_eq!(pushed.z, player.z);
        assert_eq!(shoved_to(mob, mob, Shove::To(2.0)), None);
    }

    /// The entity numbers in the replication bundles of one connection this tick.
    fn seen_by(world: &SimWorld, outgoing: &[Outgoing], who: &ConnectionRef) -> Vec<u64> {
        positions_seen_by(world, outgoing, who)
            .into_iter()
            .map(|(number, _)| number)
            .collect()
    }

    /// The reliable messages of player `number` among `outgoing`.
    pub(super) fn messages_to(outgoing: &[Outgoing], number: u64) -> Vec<ServerMessage> {
        outgoing
            .iter()
            .filter(|out| {
                out.connection == connection(number)
                    && out.reliable
                    && out.channel == MESSAGE_CHANNEL
            })
            .flat_map(|out| &out.envelopes)
            .map(|bytes| {
                let envelope = Envelope::decode(bytes).expect("envelope");
                ServerMessage::from_envelope(&envelope)
                    .expect("known")
                    .expect("decodes")
            })
            .collect()
    }

    fn bundle_of(outgoing: &[Outgoing], who: &ConnectionRef) -> Option<ReplicatedStateBundle> {
        let message = outgoing
            .iter()
            .find(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)?;
        Some(bundle_in(&message.envelopes[0]))
    }

    fn bundle_in(bytes: &[u8]) -> ReplicatedStateBundle {
        let envelope = Envelope::decode(bytes).expect("envelope");
        let Some(Ok(ServerMessage::Bundle(bundle))) = ServerMessage::from_envelope(&envelope)
        else {
            panic!("expected a bundle");
        };
        bundle
    }

    /// The entity in slot `interest` of `who`, as this sim gave the slots.
    fn entity_in_slot(world: &SimWorld, who: &ConnectionRef, interest: u16) -> Option<u64> {
        let player = world.world.get::<Player>(*world.by_connection.get(who)?)?;
        if interest == LOCAL_PLAYER_INTEREST {
            return Some(player.number);
        }
        player
            .slots
            .assigned()
            .iter()
            .find(|(_, slot)| **slot == interest)
            .map(|(entity, _)| *entity)
    }

    /// The groups of `bundle` of `who`, each with the entity of its slot.
    fn groups_in(
        world: &SimWorld,
        who: &ConnectionRef,
        bundle: &ReplicatedStateBundle,
    ) -> Vec<(u64, SlotUpdate)> {
        SlotRead::of(bundle)
            .expect("read")
            .updates
            .into_iter()
            .map(|update| {
                let entity = update
                    .entity
                    .or_else(|| entity_in_slot(world, who, update.interest))
                    .expect("every slot is tied");
                (entity, update)
            })
            .collect()
    }

    /// The entity numbers and x positions in every replication bundle of one connection this tick.
    fn positions_seen_by(
        world: &SimWorld,
        outgoing: &[Outgoing],
        who: &ConnectionRef,
    ) -> Vec<(u64, f32)> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| {
                out.envelopes.iter().flat_map(|bytes| {
                    groups_in(world, who, &bundle_in(bytes))
                        .into_iter()
                        .filter_map(|(entity, update)| Some((entity, update.position?.x)))
                })
            })
            .collect()
    }

    pub(super) fn tick(world: &mut SimWorld, now: u64) -> TickOutput {
        world.tick(now, SERVER_TIME).expect("tick")
    }

    /// The mob's spawn card in the bundle of `who`: its slot and `GdeRef`.
    fn mob_card(
        world: &SimWorld,
        outgoing: &[Outgoing],
        who: &ConnectionRef,
        mob: u64,
    ) -> Option<(u16, [u8; nw_messages::RAW_ID_LEN])> {
        let bundle = bundle_of(outgoing, who)?;
        groups_in(world, who, &bundle)
            .into_iter()
            .find(|(entity, update)| *entity == mob && update.card.is_some())
            .and_then(|(_, update)| Some((update.interest, update.card?.gde_ref?)))
    }

    /// The slots that the bundle of `who` stops.
    fn stops_of(outgoing: &[Outgoing], who: &ConnectionRef) -> Vec<u16> {
        bundle_of(outgoing, who)
            .and_then(|bundle| bundle.index_block)
            .map(|block| {
                let stops = usize::try_from(block.stop_count).expect("count");
                block.indexes.into_iter().take(stops).collect()
            })
            .unwrap_or_default()
    }

    /// A world whose one mob, of spawn `index`, respawns `respawn_ms` after its death, with
    /// player 1 beside it. The spawns before it lie in the other sim's cells; each test takes its
    /// own index, so no other test's world gives its mob number another id.
    fn respawn_world(index: u32, respawn_ms: u64) -> SimWorld {
        let mut combat = test_combat();
        combat.mob_respawn_ms = respawn_ms;
        let elsewhere = LEVEL_M - CELL_M / 2.0;
        let mut spawns: Vec<VitalsSpawn> = (0..index).map(|_| test_spawn(elsewhere)).collect();
        spawns.push(test_spawn(CELL_M / 2.0));
        combat.spawns = spawns.into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::default(),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, CELL_M / 2.0 + 2.0);
        world
    }

    #[test]
    fn a_respawned_mob_is_a_new_spawn_with_a_new_id_and_card() {
        const RESPAWN_MS: u64 = 1_000;
        const TICK_MS: u64 = 33;
        const LETHAL: u32 = 100_000;
        const INDEX: u32 = 301;
        let mob = crate::mobs::mob_entity(INDEX);
        let who = connection(1);
        let mut world = respawn_world(INDEX, RESPAWN_MS);
        let first = tick(&mut world, NOW);
        let (slot_before, id_before) = mob_card(&world, &first.messages, &who, mob).expect("card");
        assert!(nw_messages::is_dynamic_raw_id(&id_before));
        world.apply_hit(mob, 1, LETHAL, 0, NOW);
        let mut now = NOW;
        let mut stops = Vec::new();
        let card = loop {
            now += TICK_MS;
            let output = tick(&mut world, now);
            stops.extend(stops_of(&output.messages, &who));
            if let Some(card) = mob_card(&world, &output.messages, &who, mob) {
                break card;
            }
            assert!(now < NOW + 2 * RESPAWN_MS, "the mob respawns with a card");
        };
        assert!(world.mobs.health(mob).is_some_and(|health| health > 0));
        let id_after = nw_messages::raw_id_of_entity(mob);
        assert_ne!(id_after, id_before, "a new id per spawn");
        assert_eq!(nw_messages::entity_of_raw_id(&id_before), None);
        assert!(stops.contains(&slot_before), "the old card's slot stops");
        // A stopped slot may serve the new spawn once its stop has gone out.
        let (_, card_id) = card;
        assert_eq!(card_id, id_after, "the new card names the new id");
    }

    #[test]
    fn players_and_mobs_that_leave_take_their_ids_along() {
        const CYCLES: u64 = 20;
        const INDEX: u32 = 302;
        /// Player numbers of this test only.
        const FIRST_PLAYER: u64 = 0x3f00_0000_0100;
        let mob = crate::mobs::mob_entity(INDEX);
        let mut world = respawn_world(INDEX, crate::config::DEFAULT_MOB_RESPAWN_MS);
        tick(&mut world, NOW);
        let mob_id = nw_messages::raw_id_of_entity(mob);
        let mut player_ids = Vec::new();
        for cycle in 0..CYCLES {
            let number = FIRST_PLAYER + cycle;
            join(&mut world, number, CELL_M / 2.0 + 3.0);
            tick(&mut world, NOW + cycle);
            player_ids.push(nw_messages::raw_id_of_entity(number));
            world.leave(&connection(number));
        }
        for id in &player_ids {
            assert_eq!(
                nw_messages::entity_of_raw_id(id),
                None,
                "a player's id goes with it"
            );
        }
        assert_eq!(nw_messages::entity_of_raw_id(&mob_id), Some(mob));
        // The mob's cell goes to another sim: the mob leaves with its id.
        world.set_table(&table(&["sim-b", "sim-a"])).expect("table");
        tick(&mut world, NOW + CYCLES);
        assert_eq!(
            nw_messages::entity_of_raw_id(&mob_id),
            None,
            "a mob's id goes with it"
        );
    }

    #[test]
    fn a_ghosts_id_is_held_while_it_is_sent_and_a_new_one_recards_it() {
        /// A player number of this test only.
        const GHOST: u64 = 0x3_f000_0001;
        let who = connection(1);
        let mut world = world();
        join(&mut world, 1, 0.0);
        let id = nw_messages::as_dynamic_raw_id([0x5c; nw_messages::RAW_ID_LEN]);
        let ghost = Ghost {
            entity: GHOST,
            position: Point {
                x: 10.0,
                y: 10.0,
                z: 50.0,
            },
            mob: None,
            player: None,
        };
        world.adopt_ghost_ids(&[(GHOST, id)]);
        world.set_ghosts(&slot("sim-b").id, vec![ghost], NOW);
        let first = tick(&mut world, NOW);
        assert_eq!(nw_messages::entity_of_raw_id(&id), Some(GHOST));
        let seen = seen_by(&world, &first.messages, &who);
        assert!(seen.contains(&GHOST), "{seen:?}");
        // Its owner respawned it: a new id, and a new card here.
        let renewed = nw_messages::as_dynamic_raw_id([0x6e; nw_messages::RAW_ID_LEN]);
        world.adopt_ghost_ids(&[(GHOST, renewed)]);
        world.set_ghosts(&slot("sim-b").id, vec![ghost], NOW + 33);
        let again = tick(&mut world, NOW + 33);
        assert_eq!(nw_messages::entity_of_raw_id(&id), None);
        assert!(
            !stops_of(&again.messages, &who).is_empty(),
            "the old slot stops"
        );
        // The other sim stops sending it: the id goes.
        world.set_ghosts(&slot("sim-b").id, Vec::new(), NOW + 66);
        tick(&mut world, NOW + 66);
        assert_eq!(nw_messages::entity_of_raw_id(&renewed), None);
    }

    #[test]
    fn the_cell_table_spawns_the_mobs_of_this_sims_cells() {
        let west = CELL_M / 2.0;
        let east = LEVEL_M - CELL_M / 2.0;
        let spawns = [west, west + 1.0, east].map(test_spawn).to_vec();
        let mut world = world_with_spawns(spawns);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let changes = world.take_mob_changes();
        assert_eq!((changes.spawned, changes.removed), (2, 0));
        let total: usize = world.mob_counts().iter().map(|(_, count)| count).sum();
        assert_eq!(total, 2);
        world.set_table(&table(&["sim-b", "sim-a"])).expect("table");
        let changes = world.take_mob_changes();
        assert_eq!((changes.spawned, changes.removed), (1, 2));
    }

    #[test]
    fn a_border_mob_is_a_ghost_next_door() {
        let mut world = world_with_spawns(vec![test_spawn(BORDER_X - 20.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 60.0);
        tick(&mut world, NOW);
        let ghosts = tick(&mut world, NOW + 33).ghosts;
        let (target, entities) = &ghosts[0];
        assert_eq!(target, &slot("sim-b").id);
        let mob = entities
            .iter()
            .find(|ghost| ghost.mob.is_some())
            .expect("the mob is a ghost");
        assert_eq!(mob.entity, crate::mobs::mob_entity(0));
        assert_eq!(mob.mob.map(|state| state.returning), Some(false));
    }

    /// The member classes of `entity` in every bundle of `who`, in order.
    fn classes_of(
        world: &SimWorld,
        outgoing: &[Outgoing],
        who: &ConnectionRef,
        entity: u64,
    ) -> Vec<u64> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| groups_in(world, who, &bundle_in(bytes)))
            .filter(|(number, _)| *number == entity)
            .flat_map(|(_, update)| update.members)
            .map(|member| member.state.type_index())
            .collect()
    }

    /// [`classes_of`] the bundles of the next tick.
    fn next_classes_of(
        world: &mut SimWorld,
        now: u64,
        who: &ConnectionRef,
        entity: u64,
    ) -> Vec<u64> {
        let messages = tick(world, now).messages;
        classes_of(world, &messages, who, entity)
    }

    const CARD_CLASSES: [u64; 2] = [GDE_METADATA_TYPE_INDEX, ALC_TYPE_INDEX];

    #[test]
    fn a_mob_entering_view_sends_its_card_and_alc_once() {
        let x = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(x)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 5.0);
        let mob = crate::mobs::mob_entity(0);
        assert_eq!(
            next_classes_of(&mut world, NOW, &connection(1), mob),
            CARD_CLASSES
        );
        assert_eq!(
            next_classes_of(&mut world, NOW + 33, &connection(1), mob),
            [ALC_TYPE_INDEX]
        );
    }

    #[test]
    fn an_open_world_mob_sends_no_look_target() {
        const TICKS: u64 = 10;
        let x = CELL_M / 2.0;
        // A prefab look, so its groups go the prefab way.
        let prefab = format!(
            r#"[{{"vitals_id":{},"vitals_name":null,"asset":"{SAILOR_ASSET_HEX}","sub_id":2,
                "alc_component":5,"vitals_component":9,"first_members":[],"death_members":[],
                "seen":1}}]"#,
            nw_formats::objectstream::lowercase_crc32(TEST_MOB)
        );
        let prefabs = nw_content::mob_prefabs::MobPrefabs::parse(&prefab).expect("table");
        let (mob_looks, _) = crate::player_content::MobLooks::of(&prefabs);
        let mut combat = test_combat();
        combat.spawns = vec![test_spawn(x)].into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                mob_looks,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 5.0);
        let mob = crate::mobs::mob_entity(0);
        let mut classes = Vec::new();
        for step in 0..TICKS {
            classes.extend(next_classes_of(
                &mut world,
                NOW + 33 * step,
                &connection(1),
                mob,
            ));
        }
        assert_eq!(world.mobs.target(mob), Some(1), "it fights the player");
        assert!(
            !classes.contains(&nw_messages::LOOK_TARGETING_TYPE_INDEX),
            "its prefab's component 4 may hold another state: {classes:?}"
        );
    }

    #[test]
    fn the_world_snapshot_has_players_and_awake_mobs() {
        const FAR_M: f32 = 500.0;
        let x = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(x), test_spawn(x + FAR_M)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 5.0);
        tick(&mut world, NOW);
        let (players, mobs, counts) = world.snapshot();
        assert_eq!(players.len(), 1);
        assert_eq!((players[0].entity, players[0].dead), (1, false));
        assert_eq!(players[0].name, player_name(1));
        assert_eq!(
            mobs.iter().map(|mob| mob.entity).collect::<Vec<_>>(),
            vec![crate::mobs::mob_entity(0)],
            "only the awake mob"
        );
        assert_eq!(counts[0], 1, "the far mob sleeps and is only counted");
        assert_eq!(counts.iter().sum::<u32>(), 2);
    }

    #[test]
    fn a_dead_player_is_marked() {
        const LETHAL: u32 = 100_000;
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, CELL_M / 2.0);
        world.apply_hit(1, crate::mobs::mob_entity(0), LETHAL, 0, NOW);
        assert!(world.snapshot().0[0].dead);
    }

    #[test]
    fn bundle_members_count_self_and_others() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        for number in 1..=3 {
            join(&mut world, number, CELL_M / 2.0 + number as f32);
        }
        // The world has no weekly mutations, so no scheduler group; the arrival's arena state
        // takes the place of the card's own.
        assert_eq!(
            tick(&mut world, NOW + OWN_CARD_DELAY_MS).bundle_members,
            9 * (2 + PLAYER_EMPTY_COMPONENTS.len() as u64),
            "3 bundles of 3 groups, each a card, the empty player components and a position"
        );
    }

    #[test]
    fn a_tick_times_each_of_its_parts_in_run_order() {
        let mut world = world_with_spawns(Vec::new());
        join(&mut world, 1, CELL_M / 2.0);
        let parts = tick(&mut world, NOW + OWN_CARD_DELAY_MS).parts;
        let labelled = parts.labelled();
        let labels: Vec<&str> = labelled.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                TickParts::GHOSTS,
                TickParts::MOBS,
                TickParts::GAMEPLAY,
                TickParts::CARDS,
                TickParts::BUNDLES
            ]
        );
        assert!(
            labelled.iter().all(|(_, seconds)| *seconds >= 0.0),
            "{parts:?}"
        );
        assert!(parts.bundles > 0.0, "the bundle of a player takes time");
    }

    #[test]
    fn before_its_client_spawned_the_own_player_goes_out_only_whole() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let own_groups = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .map(|update| update.card.is_some())
                .collect::<Vec<bool>>()
        };
        assert!(
            own_groups(&mut world, NOW).is_empty(),
            "no card on the login tick: the client has not applied LevelInfoChanged yet"
        );
        let card_at = NOW + OWN_CARD_DELAY_MS;
        assert_eq!(
            own_groups(&mut world, card_at),
            vec![true],
            "the whole card once the login has settled"
        );
        // The same card bundle goes out again every OWN_CARD_RESEND_MS until the spawn
        // (retail resends its first bundle; live 2026-09-22), and nothing else: not a
        // fresh build, not even a card refresh.
        let mut last_card_ms = card_at;
        for step in 1..=CARD_REFRESH_BUNDLES {
            let now = card_at + 33 * step;
            let groups = own_groups(&mut world, now);
            if now.saturating_sub(last_card_ms) >= OWN_CARD_RESEND_MS {
                assert_eq!(groups, vec![true], "the card again on the resend interval");
                last_card_ms = now;
            } else {
                assert!(
                    groups.is_empty(),
                    "nothing more for the own slot before the spawn, not even a card refresh"
                );
            }
        }
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        assert_eq!(
            own_groups(&mut world, card_at + 33 * (CARD_REFRESH_BUNDLES + 1)),
            vec![false],
            "the position every tick after the spawn"
        );
        // Retail never sent the own card again after the spawn (newchar-retail-2026-09-18).
        let after_spawn = card_at + 33 * (CARD_REFRESH_BUNDLES + 1);
        let carded = (1..=CARD_REFRESH_BUNDLES * 3)
            .flat_map(|step| own_groups(&mut world, after_spawn + 33 * step))
            .filter(|carded| *carded)
            .count();
        assert_eq!(carded, 0, "no own card again after the spawn");
    }

    #[test]
    fn each_changed_item_sends_an_empty_storage_search_before_its_bundle() {
        let mut world = world_with_kit_player(settings(MANY, MAX_PLAYERS).level, CELL_M / 2.0);
        let entity = world.player_by_number(1).expect("player");
        // The places of the storage searches and of the bundle among the outgoing messages.
        let sent = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            let mut searches = Vec::new();
            let mut first_bundle = None;
            for (index, out) in messages.iter().enumerate() {
                if out.connection != connection(1) {
                    continue;
                }
                if out.channel == REPLICATION_CHANNEL {
                    first_bundle.get_or_insert(index);
                }
                if out.channel == MESSAGE_CHANNEL {
                    let count = messages_to(std::slice::from_ref(out), 1)
                        .iter()
                        .filter(|message| matches!(message, ServerMessage::StorageSearchItems(_)))
                        .count();
                    searches.extend(std::iter::repeat_n(index, count));
                }
            }
            (searches, first_bundle)
        };
        let mut worn: Vec<String> = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(Gear::carried)
            .unwrap_or_default()
            .into_iter()
            .map(|stack| stack.item_id)
            .collect();
        worn.sort_unstable();
        worn.dedup();
        assert!(!worn.is_empty(), "the starter gear");
        let card_at = NOW + OWN_CARD_DELAY_MS;
        let (searches, bundle) = sent(&mut world, card_at);
        assert_eq!(searches.len(), worn.len(), "one per item of the first card");
        let bundle = bundle.expect("the card's bundle");
        assert!(searches.iter().all(|at| *at < bundle), "before the bundle");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        let (searches, _) = sent(&mut world, card_at + 33);
        assert!(searches.is_empty(), "no change, no search");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.as_mut().expect("gear");
            assert!(gear.remove(&worn[0], 0), "worn");
        }
        let (searches, _) = sent(&mut world, card_at + 66);
        assert_eq!(searches.len(), 1, "one removed item");
    }

    #[test]
    fn a_hit_on_the_player_sends_its_new_health_with_the_next_own_state() {
        const HIT: u32 = 10;
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        let health_changes = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Vitals(vitals) => vitals
                        .slot_0
                        .map(|slot| (slot.health_amount, slot.health_change_flags)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(
            health_changes(&mut world, card_at + 33).is_empty(),
            "no change"
        );
        let max = world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .max();
        world.apply_hit(1, crate::mobs::mob_entity(0), HIT, 0, card_at + 34);
        let left = attack::amount_of(max - HIT);
        assert_eq!(
            health_changes(&mut world, card_at + 66),
            vec![(Some(left), Some(nw_messages::FIRST_DAMAGE_FLAGS))],
            "the first damage from full health"
        );
        world.apply_hit(1, crate::mobs::mob_entity(0), HIT, 0, card_at + 67);
        assert_eq!(
            health_changes(&mut world, card_at + 99),
            vec![(Some(attack::amount_of(max - 2 * HIT)), None)]
        );
        assert!(
            health_changes(&mut world, card_at + 132).is_empty(),
            "sent once"
        );
    }

    #[test]
    fn the_own_state_carries_back_a_new_view_once() {
        /// The own `rotation` and `lookDir` children (retail sends them back in own states).
        const ROTATION_CHILD: usize = 11;
        const LOOK_DIR_CHILD: usize = 12;
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(1).expect("player");
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        world.heard_from(&connection(1));
        let own_children = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Alc(alc) => alc.slot0_children().ok(),
                    _ => None,
                })
                .flatten()
                .map(|(child, _)| child)
                .collect::<Vec<_>>()
        };
        own_children(&mut world, card_at + 33);
        let view = LookDir {
            yaw: 0.5,
            pitch: 0.1,
        };
        world.world.get_mut::<Player>(entity).expect("player").view = Some(view);
        let first = own_children(&mut world, card_at + 66);
        assert!(
            first.contains(&ROTATION_CHILD) && first.contains(&LOOK_DIR_CHILD),
            "the new view goes back: {first:?}"
        );
        let next = own_children(&mut world, card_at + 99);
        assert!(
            !next.contains(&ROTATION_CHILD) && !next.contains(&LOOK_DIR_CHILD),
            "an unchanged view is not sent again: {next:?}"
        );
    }

    #[test]
    fn an_extra_member_goes_before_the_own_position_once_per_component() {
        // Run 17, the beach: the arrival's arena state followed the own ALC as a second 2406
        // member of the card, and the client failed to read the bundle.
        let mut world = world();
        join(&mut world, 1, 0.0);
        let a = connection(1);
        let entity = world.by_connection[&a];
        let arena = nw_messages::PlayerArenaState::default()
            .member()
            .expect("arena state");
        let arena_component = arena.component;
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .extra_members
            .push(arena);
        let outgoing = tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        let bundle = bundle_of(&outgoing.messages, &a).expect("the own card");
        let known = nw_messages::BundlePayload::decode_known(&bundle.payload).expect("payload");
        assert_eq!(
            (&known.stopped_at, &known.failed),
            (&None, &None),
            "every member reads"
        );
        let own_alc = known
            .members
            .iter()
            .position(|(member, _)| member.state.type_index() == ALC_TYPE_INDEX)
            .expect("the own ALC");
        let arenas: Vec<usize> = known
            .members
            .iter()
            .enumerate()
            .filter(|(_, (member, _))| member.component == arena_component)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(arenas.len(), 1, "one arena state: {arenas:?}");
        assert!(
            arenas[0] < own_alc,
            "before the own ALC: {arenas:?} {own_alc}"
        );
    }

    #[test]
    fn a_held_player_sends_no_new_view() {
        // Run 17: the inventory camera's look turned the player at once and the client closed
        // its inventory; retail kept the rotation after an open.
        /// The own `rotation` and `lookDir` children.
        const ROTATION_CHILD: usize = 11;
        const LOOK_DIR_CHILD: usize = 12;
        let mut world = world();
        join(&mut world, 1, 0.0);
        let a = connection(1);
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .camera_held_until_ms = Some(NOW + ONE_SECOND_MS);
        let view = LookDir {
            yaw: 0.5,
            pitch: 0.1,
        };
        let _ = world.move_from_input(
            &a,
            &[camera_record(1, at, view, [0.0; 2], STANDING_SLOTS)],
            NOW,
        );
        let outgoing = tick(&mut world, NOW + 33);
        let children = alc_children(&states_of(&world, &outgoing.messages, &a, 1));
        let child = |index| children.iter().any(|(found, _)| *found == index);
        assert!(
            !child(ROTATION_CHILD) && !child(LOOK_DIR_CHILD),
            "the hold sends no look: {children:?}"
        );
    }

    #[test]
    fn a_blocked_mob_hit_sends_the_stamina_change_in_the_own_update() {
        // Live 2026-11-01: the server blocked (stamina 100 to 76), the stamina bar did not move.
        const MOB_X: f32 = CELL_M / 2.0;
        const TICK_MS: u64 = 33;
        const LONG_MS: u64 = 60_000;
        /// Slot value 1 bit 62: the block key (`InputRecord::blocking`).
        const BLOCK_BIT: u32 = 62;
        let mut world = world_with_spawns(vec![test_spawn(MOB_X)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, MOB_X + 1.5);
        let block = nw_messages::InputRecord {
            slots: nw_messages::InputSlots {
                count: 19,
                values: vec![0, 1 << BLOCK_BIT],
                bytes: [0; 4],
            },
            ..nw_messages::InputRecord::default()
        };
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        let mut now = card_at;
        let mut sent = Vec::new();
        while now < card_at + LONG_MS && sent.is_empty() {
            world.attack_input(&connection(1), std::slice::from_ref(&block), now);
            let messages = tick(&mut world, now).messages;
            sent = bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Stamina(stamina) => stamina.cur,
                    _ => None,
                })
                .filter(|cur| *cur < 100.0)
                .collect();
            now += TICK_MS;
        }
        assert!(!sent.is_empty(), "no stamina change went to the client");
    }

    #[test]
    fn a_death_and_a_respawn_go_to_the_own_vitals() {
        const LETHAL: u32 = 100_000;
        /// Far beyond the committed `min_respawn_ms`.
        const RESPAWN_AFTER_MS: u64 = 60_000;
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        let lives = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Vitals(vitals) => {
                        let slot = vitals.slot_0?;
                        Some((slot.health_amount, slot.vitals_data?.head))
                    }
                    // The respawn cooldown, as a health of -1 and head 0xff.
                    MemberState::PlayerComponent(state) => state
                        .on_death_respawn_cooldown
                        .map(|_| (Some(-1.0), u8::MAX)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        world.apply_hit(1, crate::mobs::mob_entity(0), LETHAL, 0, card_at + 1);
        assert_eq!(
            lives(&mut world, card_at + 33),
            vec![(Some(-1.0), u8::MAX), (Some(0.0), 1)],
            "the respawn cooldown and the dead block open the death screen"
        );
        let respawn_at = card_at + RESPAWN_AFTER_MS;
        world
            .request_respawn(&connection(1), None, respawn_at)
            .expect("respawn");
        let max = world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .max();
        assert_eq!(
            lives(&mut world, respawn_at + 33),
            vec![(Some(attack::amount_of(max)), 0)],
            "alive again at full health"
        );
    }

    #[test]
    fn a_refused_move_holds_the_own_position_until_a_move_is_taken() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        let own_positions = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Alc(alc) => Some(
                        alc.slot0_children()
                            .expect("walk")
                            .iter()
                            .any(|(child, _)| *child == POS_REL_CHILD),
                    ),
                    _ => None,
                })
                .collect::<Vec<bool>>()
        };
        let start = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let far = Position {
            x: start.x + 1_000.0,
            ..start
        };
        assert!(world.move_to(&connection(1), far, card_at + 1).is_err());
        assert_eq!(own_positions(&mut world, card_at + 33), vec![false]);
        let near = Position {
            x: start.x + 0.5,
            ..start
        };
        assert!(world.move_to(&connection(1), near, card_at + 300).is_ok());
        assert_eq!(own_positions(&mut world, card_at + 333), vec![true]);
    }

    #[test]
    fn other_entities_wait_until_the_client_spawned() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        world
            .admit(&admit_at(2, CELL_M / 2.0 + 1.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let messages = tick(&mut world, NOW + OWN_CARD_DELAY_MS).messages;
        let bundle = bundle_of(&messages, &connection(1)).expect("bundle");
        let updates = SlotRead::of(&bundle).expect("read").updates;
        assert_eq!(updates.len(), 1, "only the own player before spawn");
        assert!(
            updates.iter().all(SlotUpdate::is_local_player),
            "other entities share the offline cache until spawn"
        );
    }

    fn tutorial_world() -> SimWorld {
        let mut tutorial = settings(MANY, MAX_PLAYERS);
        tutorial.level = TUTORIAL_LEVEL;
        world_of("tut-1", tutorial)
    }

    fn level_info_in(outgoing: &Outgoing) -> nw_messages::LevelInfoChangedMsg {
        outgoing
            .envelopes
            .iter()
            .find_map(|bytes| {
                match ServerMessage::from_envelope(&Envelope::decode(bytes).expect("envelope")) {
                    Some(Ok(ServerMessage::LevelInfoChanged(level))) => Some(level),
                    _ => None,
                }
            })
            .expect("a level message")
    }

    /// A world of `settings` whose level has one crafting station at `at`, with players 1 (beside
    /// it) and 2 (beyond the interest radius) admitted at `x`s near it.
    fn station_world(settings: WorldSettings, at: [f32; 3]) -> SimWorld {
        let station = nw_world::stations::CraftingStation {
            id: STATION_ID,
            position: at,
            components: nw_world::stations::StationSlice {
                station_type: "Camp1".to_owned(),
                buildable: Some((1, 5)),
                ..nw_world::stations::StationSlice::default()
            },
            ..nw_world::stations::CraftingStation::default()
        };
        let mut world = SimWorld::new(
            settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            test_combat(),
            Arc::new(PlayerContent {
                stations: vec![station],
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        join(&mut world, 1, at[0] + 2.0);
        join(&mut world, 2, at[0] + RADIUS + 10.0);
        world
    }

    /// The MB campfire's GDE id.
    const STATION_ID: u64 = 0x4d80_5a8c_13e7_21ff;

    /// The station groups in the bundle of `who`: their card `GdeRef`s.
    fn station_cards(
        world: &SimWorld,
        outgoing: &[Outgoing],
        who: &ConnectionRef,
    ) -> Vec<Option<[u8; nw_messages::RAW_ID_LEN]>> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| groups_in(world, who, &bundle_in(bytes)))
            .filter(|(number, _)| *number == crafting::STATION_ENTITY_BASE)
            .map(|(_, update)| update.card.and_then(|card| card.gde_ref))
            .collect()
    }

    #[test]
    fn a_station_group_goes_out_once_to_the_players_that_see_it() {
        let mut station_ref = [0; nw_messages::RAW_ID_LEN];
        station_ref[..8].copy_from_slice(&STATION_ID.to_le_bytes());
        let mut world = station_world(settings(MANY, MAX_PLAYERS), [CELL_M / 2.0, 10.0, 50.0]);
        let first = tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        assert_eq!(
            station_cards(&world, &first.messages, &connection(1)),
            [Some(station_ref)],
            "a player beside the station gets its card"
        );
        assert!(
            station_cards(&world, &first.messages, &connection(2)).is_empty(),
            "a player beyond the interest radius gets none"
        );
        let second = tick(&mut world, NOW + OWN_CARD_DELAY_MS + 33);
        assert!(
            station_cards(&world, &second.messages, &connection(1)).is_empty(),
            "the group goes out once while the station stays in view"
        );
    }

    #[test]
    fn the_tutorial_sends_no_station() {
        let mut tutorial = settings(MANY, MAX_PLAYERS);
        tutorial.level = TUTORIAL_LEVEL;
        let start = nw_sim_core::TUTORIAL_START;
        let mut world = station_world(tutorial, [start.x, start.y, start.z]);
        let output = tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        assert!(station_cards(&world, &output.messages, &connection(1)).is_empty());
    }

    #[test]
    fn a_tutorial_sim_logs_players_into_the_tutorial_level() {
        let mut world = tutorial_world();
        let login = world
            .admit(
                &admit_at(1, nw_sim_core::TUTORIAL_START.x, true),
                NOW,
                SERVER_TIME,
            )
            .expect("admit")
            .expect("login messages");
        let level = level_info_in(&login);
        assert_eq!(level.level, TUTORIAL_LEVEL);
        assert_eq!(level.client_context, LOGIN_CLIENT_CONTEXT);
    }

    /// The messages of `outgoing` for `who` on the message channel.
    pub(super) fn messages_for(outgoing: &[Outgoing], who: &ConnectionRef) -> Vec<ServerMessage> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == MESSAGE_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .filter_map(|bytes| {
                ServerMessage::from_envelope(&Envelope::decode(bytes).expect("envelope"))
            })
            .map(|message| message.expect("message"))
            .collect()
    }

    #[test]
    fn the_tutorial_quest_unlocks_the_intro_the_level_is_answered_and_leaving_works() {
        const INTRO_UNLOCK_BIT: u32 = 14_804;
        let mut tutorial_settings = settings(MANY, MAX_PLAYERS);
        tutorial_settings.level = TUTORIAL_LEVEL;
        let mut world = SimWorld::new(
            tutorial_settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("tut-1"),
            test_combat(),
            Arc::new(PlayerContent {
                achievements: nw_content::achievements::AchievementIndexes::build([(
                    "FTUE_Intro2_Cinematic_Unlock".to_owned(),
                    INTRO_UNLOCK_BIT,
                )]),
                tutorial_quest: Some(encounter::tests::tree()),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let who = connection(1);
        world
            .admit(
                &admit_at(1, nw_sim_core::TUTORIAL_START.x, true),
                NOW,
                SERVER_TIME,
            )
            .expect("admit");
        let card_at = NOW + OWN_CARD_DELAY_MS;
        let bundle = bundle_of(&tick(&mut world, card_at).messages, &who).expect("card");
        let own = SlotRead::of(&bundle)
            .expect("read")
            .updates
            .into_iter()
            .find(SlotUpdate::is_local_player)
            .expect("own group");
        let achievements = own.members.iter().find_map(|member| match &member.state {
            MemberState::Achievement(state) => state.bits.clone(),
            _ => None,
        });
        let Some(nw_messages::AchievementBits::Full(bits)) = achievements else {
            panic!("the whole bitset with the card: {achievements:?}");
        };
        assert_ne!(
            bits.elements[(INTRO_UNLOCK_BIT / 8) as usize] & 1 << (INTRO_UNLOCK_BIT % 8),
            0,
            "the intro is unlocked"
        );
        world.heard_from(&who);

        let again = tick(&mut world, NOW + level_flow::LEVEL_INFO_AGAIN_MS).messages;
        assert!(
            messages_for(&again, &who)
                .iter()
                .any(|message| matches!(message, ServerMessage::LevelInfoChanged(_))),
            "the level message again"
        );
        let header = nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 1);
        world
            .facet_message(
                &who,
                ClientFacetMsg::AckLevelInfo {
                    header,
                    client_context: LOGIN_CLIENT_CONTEXT,
                },
                NOW,
            )
            .expect("ack");
        world
            .facet_message(
                &who,
                ClientFacetMsg::ReturnToMainWorld { header, flag: 0 },
                NOW,
            )
            .expect("leave");
        let staying = tick(&mut world, NOW + LEAVE_DELAY_MS - 1);
        assert!(staying.handoffs.is_empty(), "the last states go out first");
        let types = |output: &TickOutput| -> Vec<u64> {
            messages_for(&output.messages, &who)
                .iter()
                .map(ServerMessage::type_index)
                .collect()
        };
        assert_eq!(
            types(&staying),
            [
                nw_messages::facet::UPDATE_PHASING_TYPE_INDEX,
                nw_messages::facet::TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX,
            ],
            "the answer to the level, then the loading screen at once"
        );
        let left_at = NOW + level_flow::LEVEL_INFO_AGAIN_MS + 33;
        let output = tick(&mut world, left_at);
        let left = messages_for(&output.messages, &who);
        let [ServerMessage::UpdatePhasing(empty)] = left.as_slice() else {
            panic!("the empty phasing before the handoff: {left:?}");
        };
        assert!(empty.entries.is_empty());
        assert!(output.handoffs.is_empty(), "the phasing goes first");
        let output = tick(&mut world, left_at + PHASING_BEFORE_HANDOFF_MS);
        assert_eq!(output.handoffs.len(), 1, "a request leaves from anywhere");
        let offer = &output.handoffs[0].offer;
        assert!(offer.enter_level);
        assert_eq!(
            offer.phasing_sequence, 2,
            "the level change goes on from it"
        );
    }

    /// The sailor asset of the prefab table (mob-cards note, section 1.3).
    const SAILOR_ASSET_HEX: &str = "b7e33b294bcf57b8a476abdc3a31fae7";

    /// The mobs whose cards name `asset` in the bundles of `outgoing` for `who`.
    fn cards_of(
        outgoing: &[Outgoing],
        who: &ConnectionRef,
        asset: nw_messages::AssetId,
    ) -> Vec<u64> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| SlotRead::of(&bundle_in(bytes)).expect("read").updates)
            .flat_map(|update| update.members)
            .filter_map(|member| match member.state {
                MemberState::GdeMetadata(card) if card.asset == Some(asset) => card.entity_number(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_chest_gather_sends_its_own_gather_states() {
        const GATHER_LIMIT_MS: u64 = 3_000;
        let mut tutorial_settings = settings(MANY, MAX_PLAYERS);
        tutorial_settings.level = TUTORIAL_LEVEL;
        let mut world = SimWorld::new(
            tutorial_settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("tut-1"),
            encounter::tests::sailor_combat(),
            Arc::new(PlayerContent {
                kill_types: encounter::tests::kill_types(),
                tutorial_quest: Some(encounter::tests::tree()),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let who = connection(1);
        let mut admit = admit_at(1, TUTORIAL_START.x, true);
        admit.position = Point::from(nw_sim_core::TUTORIAL_START);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&who);
        let pressed_at = spawn_at + 33;
        let mut target = [0; nw_messages::wire::RAW_ID_LEN];
        target[..nw_content::ftue_entities::GDE_REF_PREFIX_LEN]
            .copy_from_slice(&encounter::CHEST_PREFIX);
        let request = InteractionRequest {
            target,
            target_entity: facet_of(&encounter::CHEST_PREFIX),
            interaction: 7,
            kind: 14,
            gatherable: None,
        };
        world.interact(&who, request, &[], pressed_at);
        // The own states of the next tick (a card refresh also carries empty ones).
        let own_states = |world: &mut SimWorld, now: u64| -> Vec<MemberState> {
            let messages = tick(world, now).messages;
            messages
                .iter()
                .filter(|out| out.connection == who && out.channel == REPLICATION_CHANNEL)
                .flat_map(|out| &out.envelopes)
                .flat_map(|bytes| groups_in(world, &who, &bundle_in(bytes)))
                .filter(|(number, _)| *number == 1)
                .flat_map(|(_, update)| update.members)
                .map(|member| member.state)
                .collect()
        };
        let gathering = |states: &[MemberState], running: bool| {
            states.iter().any(|state| {
                matches!(state, MemberState::Gathering(gather) if gather.is_gathering == Some(running))
            })
        };
        let started = own_states(&mut world, pressed_at + 33);
        assert!(
            started.contains(&interactor_target(request.target_entity).state),
            "the accept names the chest: {started:?}"
        );
        assert!(gathering(&started, true), "and runs the gather");
        for season in [
            nw_messages::seasons::seasons_rewards_initialized(),
            nw_messages::seasons::seasons_tracked_stats_initialized(),
        ] {
            assert!(
                started.contains(&season.state),
                "the client in game gets the season states that start its quest pins"
            );
        }
        let after: Vec<MemberState> = (2..GATHER_LIMIT_MS / 33)
            .flat_map(|step| own_states(&mut world, pressed_at + 33 * step))
            .collect();
        assert!(
            gathering(&after, false),
            "the end sends the done gather state"
        );
        let looted = after.iter().find_map(|state| match state {
            MemberState::LootTracker(tracker) => tracker
                .loot
                .as_ref()?
                .elements
                .iter()
                .find(|loot| loot.bag == request.target)
                .cloned(),
            _ => None,
        });
        let looted = looted.expect("the loot tracker lists the looted chest");
        assert_eq!(
            (looted.state, looted.kind, looted.privileged),
            (2, 0, false),
            "retail: taken, point-of-interest loot, not privileged"
        );
        let entity = world.by_connection[&who];
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!(player.gathers, 1, "one gather made");
    }

    #[test]
    fn interactions_and_the_inventory_put_the_drawn_weapon_away_and_give_it_back() {
        use nw_messages::client_input::{GATHER_KIND, TALK_KIND};
        use nw_messages::paperdoll::slot::MAIN_HAND_OPTION_1;
        /// Retail flint (`GatheringType` None) and an iron vein (`Mining`): by hand and with a
        /// tool.
        const FLINT_CRC: u32 = 0xa417_137f;
        const VEIN: &str = "IronVeinSmall";
        const NPC: [u8; nw_messages::wire::RAW_ID_LEN] = [0x6e; nw_messages::wire::RAW_ID_LEN];
        let gatherable = |id: &str, by_hand| Gatherable {
            id: id.to_owned(),
            gather_ms: ONE_SECOND_MS,
            respawn_ms: (ONE_SECOND_MS, ONE_SECOND_MS),
            loot_table: None,
            loot_container: false,
            tradeskill: None,
            tool_required: false,
            by_hand,
            game_event: None,
        };
        let gatherables =
            Gatherables::build([gatherable("SingleStone", true), gatherable(VEIN, false)]);
        let vein_crc = nw_formats::objectstream::lowercase_crc32(VEIN);
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            test_combat(),
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                gatherables,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let who = connection(1);
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.by_connection[&who];
        let drawn = |world: &SimWorld| {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref()?.drawn())
        };
        let draw = |world: &mut SimWorld| {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            assert!(player.gear.as_mut().expect("gear").draw_to_attack(1));
        };
        let request = |interaction, kind, gatherable| InteractionRequest {
            target: NPC,
            target_entity: interaction,
            interaction,
            kind,
            gatherable,
        };
        // A gather by hand: put away, back at the end of the interaction.
        draw(&mut world);
        world.interact(&who, request(1, GATHER_KIND, Some(FLINT_CRC)), &[], NOW);
        assert_eq!(drawn(&world), None, "the flint is taken by hand");
        world.interaction_over(&who, NOW);
        assert_eq!(drawn(&world), Some(MAIN_HAND_OPTION_1), "given back");
        // A gather with a tool keeps the weapon.
        world.interact(&who, request(2, GATHER_KIND, Some(vein_crc)), &[], NOW);
        assert_eq!(drawn(&world), Some(MAIN_HAND_OPTION_1));
        world.interaction_over(&who, NOW);
        // A talk puts it away for good.
        world.interact(&who, request(3, TALK_KIND, None), &[], NOW);
        assert_eq!(drawn(&world), None);
        world.interaction_over(&who, NOW);
        assert_eq!(drawn(&world), None, "a talk gives nothing back");
        // A revive and a trigger put it away for good; the exit of a dungeon keeps it.
        for (interaction, kind) in [(4, REVIVE_PLAYER_KIND), (5, TRIGGER_KIND)] {
            draw(&mut world);
            world.interact(&who, request(interaction, kind, None), &[], NOW);
            assert_eq!(drawn(&world), None, "kind {kind}");
            world.interaction_over(&who, NOW);
            assert_eq!(drawn(&world), None, "kind {kind} gives nothing back");
        }
        draw(&mut world);
        world.interact(
            &who,
            request(6, nw_messages::client_input::EXIT_DUNGEON_KIND, None),
            &[],
            NOW,
        );
        assert_eq!(drawn(&world), Some(MAIN_HAND_OPTION_1), "the exit keeps it");
        world.interaction_over(&who, NOW);
        let mut player = world.world.get_mut::<Player>(entity).expect("player");
        assert!(player.gear.as_mut().expect("gear").put_away(0, None));
        // The inventory: put away while open, back when it closes.
        draw(&mut world);
        let loadout = |looking| ClientFacetMsg::LookingThroughLoadout {
            header: nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 1),
            looking,
        };
        world.facet_message(&who, loadout(true), NOW).expect("open");
        assert_eq!(drawn(&world), None);
        world
            .facet_message(&who, loadout(false), NOW)
            .expect("closed");
        assert_eq!(drawn(&world), Some(MAIN_HAND_OPTION_1));
    }

    #[test]
    fn an_open_world_flint_gather_fills_the_bag_and_depletes_the_node() {
        /// Retail flint: `SingleStone` (CRC-32 of its lowercase id `a417137f`), 0.25 s, back
        /// after 60 to 90 s, one `FlintT1`.
        const FLINT: &str = "SingleStone";
        const FLINT_CRC: u32 = 0xa417_137f;
        const FLINT_GATHER_MS: u64 = 250;
        const FLINT_RESPAWN_MS: (u64, u64) = (60_000, 90_000);
        const ALWAYS: i64 = 100_000;
        /// The 0.25 s gather rounded up to the 33 ms tick, plus one tick.
        const GATHER_END_MS: u64 = 297;
        const INTERACTION: u64 = 7;
        const FLINT_ENTITY: u64 = 0x5b;
        const ACCEPTED: u8 = 1;
        const BUSY: u8 = 2;
        const DEPLETED: u8 = 8;
        const HEALTHY: u8 = 4;
        const TICK_MS: u64 = 33;
        let loot = |id: &str, cells: &[(u32, &str)], and_or: Option<&str>, max_roll| LootRow {
            id: id.to_owned(),
            and_or: and_or.map(str::to_owned),
            conditions: None,
            max_roll,
            trigger_limit_on_visit: false,
            cells: cells
                .iter()
                .map(|(index, cell)| (*index, (*cell).to_owned()))
                .collect(),
            ..LootRow::default()
        };
        let gatherables = Gatherables::build([Gatherable {
            id: FLINT.to_owned(),
            gather_ms: FLINT_GATHER_MS,
            respawn_ms: FLINT_RESPAWN_MS,
            loot_table: Some(FLINT.to_owned()),
            loot_container: false,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: None,
        }]);
        let loot = LootTables::build(LootSource {
            rows: vec![
                loot(FLINT, &[(1, gear::tests::FLINT)], Some("AND"), None),
                loot(&format!("{FLINT}_Qty"), &[(1, "1")], None, None),
                loot(&format!("{FLINT}_Probs"), &[(1, "0")], None, Some(ALWAYS)),
            ],
            bucket_items: Vec::new(),
        });
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                gatherables,
                loot,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let who = connection(1);
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&who);
        let request = InteractionRequest {
            target: [0x5b; nw_messages::wire::RAW_ID_LEN],
            target_entity: FLINT_ENTITY,
            interaction: INTERACTION,
            kind: nw_messages::client_input::GATHER_KIND,
            gatherable: Some(FLINT_CRC),
        };
        let answers = |messages: &[Outgoing]| -> Vec<(u8, Vec<u8>)> {
            messages_to(messages, 1)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::InteractionValidated(answer) => {
                        Some((answer.status, answer.name))
                    }
                    _ => None,
                })
                .collect()
        };
        let flint_in_bag = |world: &SimWorld| -> u32 {
            let entity = world.by_connection[&who];
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|item| item.item_id == gear::tests::FLINT)
                .map(|item| item.quantity)
                .sum()
        };
        let node_states = |world: &SimWorld, messages: &[Outgoing]| -> Vec<u8> {
            bundle_of(messages, &who)
                .map(|bundle| groups_in(world, &who, &bundle))
                .unwrap_or_default()
                .into_iter()
                .filter(|(number, _)| *number >= gathering::GATHERABLE_ENTITY_BASE)
                .flat_map(|(_, update)| update.members)
                .filter_map(|member| match member.state {
                    MemberState::GatherableController(state) => state.gatherable_state,
                    _ => None,
                })
                .collect()
        };

        let pressed_at = spawn_at + TICK_MS;
        world.interact(&who, request, &[], pressed_at);
        world.interact(&who, request, &[], pressed_at + TICK_MS);
        let accepted = tick(&mut world, pressed_at + TICK_MS).messages;
        assert_eq!(
            answers(&accepted),
            [(ACCEPTED, Vec::new())],
            "one accept with an empty name: a repeat starts nothing"
        );
        tick(&mut world, pressed_at + GATHER_END_MS - 1);
        assert_eq!(flint_in_bag(&world), 0, "the gather still runs");
        let ended = tick(&mut world, pressed_at + GATHER_END_MS).messages;
        assert!(
            messages_to(&ended, 1)
                .iter()
                .any(|message| matches!(message, ServerMessage::InteractionEnded(_))),
            "the end goes to the client"
        );
        assert_eq!(flint_in_bag(&world), 1, "one flint");
        let after_end = pressed_at + GATHER_END_MS + TICK_MS;
        assert_eq!(
            node_states(&world, &ended),
            [DEPLETED],
            "the node's card goes with the end"
        );
        // Inputs sent before the client took the end still hold the gather (retail: its cancel
        // came 2 to 23 ms after the 3746); they get no refusal (live run 9: 38 status-2 answers).
        world.interact(&who, request, &[], after_end);
        assert!(
            answers(&tick(&mut world, after_end).messages).is_empty(),
            "the ended gather is not refused"
        );

        world.interact(
            &who,
            InteractionRequest {
                interaction: INTERACTION + 1,
                ..request
            },
            &[],
            after_end,
        );
        let refused = tick(&mut world, after_end + TICK_MS).messages;
        assert_eq!(
            answers(&refused),
            [(BUSY, Vec::new())],
            "a depleted node is busy"
        );

        let grown_at = pressed_at + GATHER_END_MS + FLINT_RESPAWN_MS.1;
        let grown = tick(&mut world, grown_at).messages;
        assert_eq!(node_states(&world, &grown), [HEALTHY], "it grows back");
    }

    #[test]
    fn a_crate_gives_its_variants_loot_and_a_cancel_after_the_end_keeps_it() {
        use nw_content::gathering::{CrateLoot, CrateVariant};
        /// The retail start-beach armour chest (loot note): gatherable `LootLargeT1` (5 s,
        /// `FinalLootTable` `Empty`), its capital, the facet entity and name of the retail
        /// request, and its variant's table.
        const CHEST: &str = "LootLargeT1";
        const CHEST_GATHER_MS: u64 = 5_000;
        const CHEST_GUID: &str = "aabcf082-9526-2edf-760b-043d3db212c4";
        const CHEST_FACET: u64 = 0xdcb7_f4bf_a894_3c1b;
        const CHEST_TARGET: [u8; nw_messages::wire::RAW_ID_LEN] = [
            0x1b, 0x3c, 0x94, 0xa8, 0xbf, 0xf4, 0xb7, 0xdc, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        const CHEST_NAME: &[u8] = b"@LootContainer_ArmorChestBeach_DisplayName";
        const VARIANT: &str = "WeaponsChest_StarterBeach_1";
        const VARIANT_TABLE: &str = "BeachChestMB1";
        const EMPTY: &str = "Empty";
        /// The 5 s gather rounded up to the 33 ms tick, plus one tick.
        const GATHER_END_MS: u64 = 5_049;
        const INTERACTION: u64 = 11;
        const TICK_MS: u64 = 33;
        let gatherables = Gatherables::build([Gatherable {
            id: CHEST.to_owned(),
            gather_ms: CHEST_GATHER_MS,
            respawn_ms: (0, 0),
            loot_table: Some(EMPTY.to_owned()),
            loot_container: true,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: None,
        }]);
        let mut rows = loot_rows(VARIANT_TABLE, &[(gear::tests::POTION, "0")]);
        rows.extend(loot_rows(EMPTY, &[]));
        let crates = CrateLoot::build(
            vec![CrateVariant {
                variant_id: VARIANT.to_owned(),
                gatherable: CHEST.to_owned(),
                name: String::from_utf8(CHEST_NAME.to_vec()).expect("name"),
                table: VARIANT_TABLE.to_owned(),
            }],
            [(CHEST_GUID, VARIANT)],
            [],
        );
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                gatherables,
                loot: LootTables::build(LootSource {
                    rows,
                    bucket_items: Vec::new(),
                }),
                crates,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let who = connection(1);
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&who);
        let potions = |world: &SimWorld| -> u32 {
            world
                .world
                .get::<Player>(world.by_connection[&who])
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|item| item.item_id == gear::tests::POTION)
                .map(|item| item.quantity)
                .sum()
        };
        let request = InteractionRequest {
            target: CHEST_TARGET,
            target_entity: CHEST_FACET,
            interaction: INTERACTION,
            kind: GATHER_KIND,
            gatherable: Some(nw_formats::objectstream::lowercase_crc32(CHEST)),
        };
        let pressed_at = spawn_at + TICK_MS;
        world.interact(&who, request, CHEST_NAME, pressed_at);
        tick(&mut world, pressed_at);
        let before = potions(&world);
        tick(&mut world, pressed_at + GATHER_END_MS);
        assert_eq!(
            potions(&world),
            before + 1,
            "the variant's table, not Empty"
        );
        let cancel = ClientFacetMsg::CancelInteraction {
            header: nw_messages::FacetHeader::of(0, 1),
            target: CHEST_TARGET,
            target_entity: CHEST_FACET,
            interaction: INTERACTION,
        };
        let cancelled_at = pressed_at + GATHER_END_MS + TICK_MS;
        world
            .facet_message(&who, cancel.clone(), cancelled_at)
            .expect("cancel");
        let interrupted = tick(&mut world, cancelled_at).messages;
        assert_eq!(
            messages_to(&interrupted, 1)
                .iter()
                .filter(|message| matches!(message, ServerMessage::GatheringInterrupted(_)))
                .count(),
            1,
            "retail answers the cancel after the end with 2724"
        );
        assert_eq!(potions(&world), before + 1, "the cancel keeps the loot");
        world
            .facet_message(&who, cancel, cancelled_at + TICK_MS)
            .expect("cancel again");
        let again = tick(&mut world, cancelled_at + TICK_MS).messages;
        assert!(
            !messages_to(&again, 1)
                .iter()
                .any(|message| matches!(message, ServerMessage::GatheringInterrupted(_))),
            "once"
        );
    }

    /// The beach quests' items (item id, class), recipes, rewards and gatherables.
    const KNIFE: &str = "1hSkinningKnifeT1";
    const WOOD: &str = "WoodT1";
    const PORK: &str = "PorkT1";
    const RATION: &str = "FoodHealthRecoveryT1";
    const MEAT_POUCH: &str = "QuestReward_MB_Meat_Pouch";
    const MYSTIC_FEET: &str = "MysticArchetypeFeet_Upgrade";
    const RATION_RECIPE: &str = "Procedural_HealthRecoveryT1";
    /// The objectives (retail rows of build 25810808, with their quest-chain cells).
    const SKINNING_KNIFE: &str = "12_skinningknife";
    const BOAR_FEAST: &str = "12_boarfeast";
    const BOAR_FEAST_MYSTIC: &str = "12_boarfeast_mystic";

    /// A world with the beach quests, their content, one boar (the test mob) at `CELL_M / 2`
    /// and player 1 (a mystic, so it has `Archetype_Theurgist`) next to it, spawned, who turned
    /// in `12_getoffbeach`.
    fn beach_world() -> SimWorld {
        use crate::objectives::{KillTypes, TaskBook};
        use crate::player_content::{ObjectiveRow, QuestBook};
        use nw_content::achievements::AchievementIndexes;
        use nw_content::crafting::{Ingredient, IngredientKind, Recipe, Recipes};
        use nw_content::rewards::{Reward, Rewards};
        const ACHIEVEMENTS: [&str; 6] = [
            "S_getoffbeach_mb",
            "S_skinningknife_mb",
            "S_boarfeast_mb",
            "Archetype_Theurgist",
            "SwapActive_01_AelstromMB_01",
            "SwapActive_01_AelstromMB_03",
        ];
        let objective = |id: &str, root: &str, requirement: &str, event: &str| ObjectiveRow {
            id: id.to_owned(),
            root: root.to_owned(),
            requirement: Some(requirement.to_owned()),
            achievement: Some(if root.starts_with("12_skinning") {
                "S_skinningknife_mb".to_owned()
            } else {
                "S_boarfeast_mb".to_owned()
            }),
            turn_in: Some("01_UrdaMB_01".to_owned()),
            success_event: Some(event.to_owned()),
            ..ObjectiveRow::default()
        };
        let quests = QuestBook::build(
            [
                objective(
                    SKINNING_KNIFE,
                    "12_skinningknife_container",
                    "S_getoffbeach_mb",
                    "01_skinningknife_success",
                ),
                objective(
                    BOAR_FEAST,
                    "12_boarfeast_container",
                    "S_skinningknife_mb && !Archetype_Theurgist",
                    "01_boarfeast_success",
                ),
                ObjectiveRow {
                    reward_item: Some(gear::tests::POTION.to_owned()),
                    reward_quantity: Some(5),
                    ..objective(
                        BOAR_FEAST_MYSTIC,
                        "12_boarfeast_container",
                        "S_skinningknife_mb && Archetype_Theurgist",
                        "01_boarfeast_success_mystic",
                    )
                },
            ],
            TaskBook::new(crate::objectives::tests::beach_rows()),
        );
        let reward = |coins, experience, item: &str| Reward {
            coins,
            experience,
            item: Some((item.to_owned(), 1)),
            whisper: None,
        };
        let item = |name: &str| Ingredient {
            kind: IngredientKind::Item(name.to_owned()),
            quantity: 1,
        };
        let mut combat = test_combat();
        combat.spawns = vec![test_spawn(CELL_M / 2.0)].into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits_with(&[
                    (WOOD, "Resource+Wood"),
                    (KNIFE, "EquippableTool+Axe+SkinningKnife"),
                    (PORK, "Consumable+RawFood"),
                    (RATION, "Consumable+Food"),
                    (MEAT_POUCH, "Resource+LootContainer"),
                    (MYSTIC_FEET, "EquippableFeet+Armor+Light"),
                ]),
                achievements: AchievementIndexes::build(
                    (100..)
                        .zip(ACHIEVEMENTS)
                        .map(|(bit, id)| (id.to_owned(), bit)),
                ),
                archetype_unlocks: vec![(
                    gear::tests::BACKSTORY.to_owned(),
                    "Archetype_Theurgist".to_owned(),
                )],
                kill_types: KillTypes::build([(TEST_MOB.to_owned(), "Boar,Hostile".to_owned())]),
                quests,
                gatherables: Gatherables::build([Gatherable {
                    id: "Boar".to_owned(),
                    gather_ms: 15_000,
                    respawn_ms: (1_260_000, 1_800_000),
                    loot_table: None,
                    loot_container: false,
                    tradeskill: Some("Skinning".to_owned()),
                    tool_required: true,
                    by_hand: false,
                    game_event: None,
                }]),
                // The T1 skinning knife's gathering efficiency range (2 to 2): retail skinned
                // the boar in 7.5 s of its 15 s.
                tools: nw_content::gathering::ToolEfficiencies::build([(
                    KNIFE.to_owned(),
                    nw_content::gathering::ToolEfficiency {
                        efficiency: (2.0, 2.0),
                        gear_score: (0, 0),
                    },
                )]),
                recipes: Recipes::build(
                    [
                        Recipe {
                            id: KNIFE.to_owned(),
                            item: KNIFE.to_owned(),
                            quantity: 1,
                            ingredients: vec![item(gear::tests::FLINT), item(WOOD)],
                            stations: vec!["camp1".to_owned()],
                        },
                        Recipe {
                            id: RATION_RECIPE.to_owned(),
                            item: RATION.to_owned(),
                            quantity: 1,
                            ingredients: vec![Ingredient {
                                kind: IngredientKind::Category("FoodIngredientT1".to_owned()),
                                quantity: 1,
                            }],
                            stations: vec!["camp1".to_owned()],
                        },
                    ],
                    [(PORK.to_owned(), "Meats,FoodIngredientT1".to_owned())],
                ),
                rewards: Rewards::build([
                    (
                        "01_skinningknife_success".to_owned(),
                        reward(225, 275, MEAT_POUCH),
                    ),
                    (
                        "01_boarfeast_success_mystic".to_owned(),
                        reward(250, 350, MYSTIC_FEET),
                    ),
                ]),
                // Urda's Campfire (its retail GDE id, the beach-quests note's `GdeRef`).
                stations: vec![nw_world::stations::CraftingStation {
                    id: 0x4d80_5a8c_13e7_21ff,
                    components: nw_world::stations::StationSlice {
                        station_type: "Camp1".to_owned(),
                        name: "@ui_watcherscampfire".to_owned(),
                        ..nw_world::stations::StationSlice::default()
                    },
                    ..nw_world::stations::CraftingStation::default()
                }],
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0 + 1.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        world.heard_from(&connection(1));
        let entity = world.by_connection[&connection(1)];
        let rules = world.achievement_rules.clone();
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .flow
            .set_achievement(&rules, "S_getoffbeach_mb");
        world
    }

    #[test]
    fn the_skinning_knife_and_boar_feast_quests_run_from_talk_to_turn_in() {
        use nw_formats::objectstream::lowercase_crc32;
        use nw_messages::FacetHeader;
        /// Retail Urda's Campfire and the talk name (beach-quests note).
        const CAMPFIRE: [u8; nw_messages::wire::RAW_ID_LEN] =
            0xff21_e713_8c5a_804d_0000_0000_0000_0000_u128.to_be_bytes();
        const NPC: [u8; nw_messages::wire::RAW_ID_LEN] = [0x27; nw_messages::wire::RAW_ID_LEN];
        const KNIFE_SLOT: u32 = 35;
        const CRAFT_MS: u64 = 1_057;
        const TICK_MS: u64 = 33;
        const SKIN_MS: u64 = 7_500;
        const LETHAL: u32 = 100_000;
        let mut world = beach_world();
        let who = connection(1);
        let entity = world.by_connection[&who];
        let mut now = NOW + OWN_CARD_DELAY_MS;
        let step = |world: &mut SimWorld, now: &mut u64, ms: u64| {
            *now += ms;
            tick(world, *now).messages
        };
        let request = |target, kind, interaction| InteractionRequest {
            target,
            target_entity: 0x1234,
            interaction,
            kind,
            gatherable: (kind == nw_messages::client_input::GATHER_KIND)
                .then(|| lowercase_crc32("Boar")),
        };
        let answers = |messages: &[Outgoing]| -> Vec<(u8, u8, Vec<u8>)> {
            messages_to(messages, 1)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::InteractionValidated(answer) => {
                        Some((answer.kind, answer.status, answer.name))
                    }
                    _ => None,
                })
                .collect()
        };
        let crafted = |messages: &[Outgoing]| -> Vec<(Vec<u8>, u32)> {
            messages_to(messages, 1)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::CraftProgress(progress) => {
                        Some((progress.recipe, progress.quantity))
                    }
                    _ => None,
                })
                .collect()
        };
        let header = FacetHeader::of(0, 1);
        let holds = |world: &SimWorld, item: &str| -> u32 {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|stack| stack.item_id == item)
                .map(|stack| stack.quantity)
                .sum()
        };
        let has = |world: &SimWorld, achievement: &str| {
            world.world.get::<Player>(entity).is_some_and(|player| {
                player
                    .flow
                    .has_achievement(&world.achievement_rules, achievement)
            })
        };
        let wallet = |world: &SimWorld| {
            let player = world.world.get::<Player>(entity).expect("player");
            (player.coins, player.experience)
        };
        let facet = |world: &mut SimWorld, message: ClientFacetMsg, at: u64| {
            world.facet_message(&who, message, at).expect("facet");
        };
        let accept = |objective: &str| ClientFacetMsg::AcceptObjective {
            header,
            objective: lowercase_crc32(objective),
        };
        let complete = |objective: &str| ClientFacetMsg::CompleteObjective {
            header,
            objective: lowercase_crc32(objective),
        };
        let craft = |recipe: &str, items: &[&str]| ClientFacetMsg::SetCraftingDetails {
            header,
            recipe: lowercase_crc32(recipe),
            quantity: 1,
            fee: 0,
            gear_score: 0,
            perk_items: Vec::new(),
            unknown: 0,
            ingredients: items
                .iter()
                .map(|item| nw_messages::CraftIngredient {
                    kind: nw_messages::facet::INGREDIENT_ITEM,
                    quantity: 1,
                    item: lowercase_crc32(item),
                })
                .collect(),
        };

        // Aelstrom: the talk is accepted, then the quest.
        // The client repeats the request in each input; retail answers it once.
        let talk_request = request(NPC, nw_messages::client_input::TALK_KIND, 1);
        world.interact(&who, talk_request, &[], now);
        world.interact(&who, talk_request, &[], now);
        let talk = step(&mut world, &mut now, TICK_MS);
        world.interact(&who, talk_request, &[], now);
        assert_eq!(
            answers(&talk),
            [(
                nw_messages::client_input::TALK_KIND,
                1,
                b"@01_Reese".to_vec()
            )]
        );
        assert!(
            answers(&step(&mut world, &mut now, TICK_MS)).is_empty(),
            "a held talk is answered once"
        );
        facet(&mut world, accept(SKINNING_KNIFE), now);
        assert_eq!(
            world
                .quests_of_player(1)
                .first()
                .map(|quest| quest.def.id.as_str()),
            Some(SKINNING_KNIFE)
        );
        assert!(has(&world, "SwapActive_01_AelstromMB_01"), "its first task");
        let accepted = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.quest.clone()?.take_change())
            .expect("the accept state");
        assert!(
            accepted
                .active
                .is_some_and(|active| active.elements.len() == 1),
            "the journal gets the quest as active"
        );

        // Flint and wood, then the knife at Urda's Campfire.
        world.give_loot(
            1,
            vec![(gear::tests::FLINT.to_owned(), 1), (WOOD.to_owned(), 1)],
            now,
        );
        step(&mut world, &mut now, TICK_MS);
        let station = nw_messages::client_input::CRAFTING_STATION_KIND;
        world.interact(&who, request(CAMPFIRE, station, 2), &[], now);
        world.interact(&who, request(CAMPFIRE, station, 2), &[], now);
        let opened = step(&mut world, &mut now, TICK_MS);
        assert_eq!(
            answers(&opened),
            [(station, 1, b"@ui_watcherscampfire".to_vec())]
        );
        facet(&mut world, craft(KNIFE, &[gear::tests::FLINT, WOOD]), now);
        assert_eq!(
            holds(&world, gear::tests::FLINT),
            1,
            "the request takes nothing (retail: the bag changes with the result)"
        );
        assert_eq!(holds(&world, KNIFE), 0, "not yet made");
        let made = step(&mut world, &mut now, CRAFT_MS);
        assert_eq!(
            holds(&world, gear::tests::FLINT),
            0,
            "taken with the result"
        );
        assert_eq!(
            crafted(&made),
            [(KNIFE.as_bytes().to_vec(), 1), (Vec::new(), 0)],
            "the craft, then idle"
        );
        assert_eq!(holds(&world, KNIFE), 1);
        step(&mut world, &mut now, TICK_MS);
        assert!(
            world
                .quests_of_player(1)
                .first()
                .is_some_and(|quest| quest.run.finished()),
            "the knife ends the tasks"
        );
        assert!(world.quests.contains_key(&1), "it waits for its turn-in");
        facet(&mut world, ClientFacetMsg::CancelCrafting { header }, now);

        // The turn-in at Urda: achievement, coins, experience and the meat pouch.
        facet(&mut world, complete(BOAR_FEAST), now);
        assert!(
            world.quests.contains_key(&1),
            "another quest's turn-in does nothing"
        );
        facet(&mut world, complete(SKINNING_KNIFE), now);
        assert!(!world.quests.contains_key(&1));
        assert!(has(&world, "S_skinningknife_mb"));
        assert_eq!(wallet(&world), (225, 275));
        assert_eq!(holds(&world, MEAT_POUCH), 1);

        // The boar feast: the mystic's variant only.
        facet(&mut world, accept(BOAR_FEAST), now);
        assert!(!world.quests.contains_key(&1), "not offered to a mystic");
        facet(&mut world, accept(BOAR_FEAST_MYSTIC), now);
        assert!(world.quests.contains_key(&1));
        let boar = crate::mobs::mob_entity(0);
        world.apply_hit(boar, 1, LETHAL, 0, now);
        let corpse = raw_id_of_entity(boar);
        let gather = nw_messages::client_input::GATHER_KIND;
        world.interact(&who, request(corpse, gather, 3), &[], now);
        let refused = step(&mut world, &mut now, TICK_MS);
        assert_eq!(
            answers(&refused),
            [(gather, 2, Vec::new())],
            "no knife equipped"
        );
        let knife = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(Gear::carried)
            .unwrap_or_default()
            .into_iter()
            .find_map(|stack| match stack.place {
                ItemPlace::Bag(bag_slot) if stack.item_id == KNIFE => Some(u32::from(bag_slot)),
                _ => None,
            })
            .expect("the knife in the bag");
        let character = world.world.get::<Player>(entity).expect("player").character;
        facet(
            &mut world,
            ClientFacetMsg::EquipItem {
                header,
                bag_slot: knife,
                equip_slot: KNIFE_SLOT,
                quantity: 1,
                instance: gear::instance_id(character, KNIFE),
                flag: 0,
            },
            now,
        );
        world.interact(&who, request(corpse, gather, 4), &[], now);
        let accepted = step(&mut world, &mut now, TICK_MS);
        assert_eq!(answers(&accepted), [(gather, 1, Vec::new())]);
        step(&mut world, &mut now, SKIN_MS);
        assert_eq!(holds(&world, PORK), 1, "the quest drop");
        let shown = |world: &SimWorld| world.mobs.corpses().any(|(mob, _)| mob == boar);
        assert!(shown(&world), "a skinned corpse stays a while");
        step(
            &mut world,
            &mut now,
            nw_content::corpse_times::SKINNED_CORPSE_MS,
        );
        assert!(!shown(&world), "then it goes");

        // The Light Ration at the campfire, then the turn-in at Aelstrom.
        world.interact(&who, request(CAMPFIRE, station, 5), &[], now);
        facet(&mut world, craft(RATION_RECIPE, &[PORK]), now);
        step(&mut world, &mut now, CRAFT_MS);
        assert_eq!(holds(&world, PORK), 0, "the pork is cooked");
        assert_eq!(holds(&world, RATION), 1);
        step(&mut world, &mut now, TICK_MS);
        facet(&mut world, complete(BOAR_FEAST_MYSTIC), now);
        assert!(!world.quests.contains_key(&1));
        assert!(has(&world, "S_boarfeast_mb"));
        assert_eq!(wallet(&world), (225 + 250, 275 + 350));
        assert_eq!(holds(&world, gear::tests::POTION), 5);
        assert_eq!(holds(&world, MYSTIC_FEET), 1);
        let done = world
            .world
            .get::<Player>(entity)
            .map(|player| player.done_quests.clone())
            .unwrap_or_default();
        assert_eq!(done, [SKINNING_KNIFE, BOAR_FEAST_MYSTIC]);
    }

    #[test]
    fn the_wreck_steps_run_from_the_accept_to_the_turn_in() {
        use crate::objectives::TaskBook;
        use crate::objectives::tests::{
            MANIFEST_LORE, NODULE_EVENT, TREASURE, TREASURE_GATHERABLE, WHISPER_EVENT, WRECK_POI,
            WRECK_ROOT, wreck_rows,
        };
        use crate::player_content::{ObjectiveRow, QuestBook};
        use nw_content::achievements::AchievementIndexes;
        use nw_content::poi_tags::PoiTags;
        use nw_content::rewards::{Reward, Rewards};
        use nw_formats::objectstream::lowercase_crc32;
        use nw_messages::FacetHeader;
        use nw_messages::client_input::GATHER_KIND;
        use nw_world::records::GatherablePoint;
        const QUEST: &str = "12_pillageship";
        const WRECK_TERRITORY: u16 = 30_129;
        const WHISPER: &str = "01_IsabellasFirstWhisper_Isabella_01";
        const NODULE: &str = "MSQ_Grace_CorruptedNodule_1";
        const NODULE_LOOT: &str = "IO_MSQ_Grace_CorruptedNodule_1";
        /// Retail nodule A: its capital and GDE id (newchar 2026-09-18).
        const NODULE_CAPITAL: &str = "64f2702e-9c3b-a5a5-1dae-1b0d80c5666b";
        const NODULE_ID: u64 = 0x795c_6b23_1cfe_c3cb;
        const MANIFEST: &[u8] = b"@00_Aeternum_Title_31_01";
        const TREASURE_GATHER_MS: u64 = 1_000;
        const NODULE_RESPAWN_MS: u64 = 33_000;
        const TICK_MS: u64 = 33;
        const SWING_GAP_MS: u64 = 800;
        const X: f32 = CELL_M / 2.0 + 1.0;
        const BLUNDERBUSS: &str = "BlunderbussT2";
        const BLUNDERBUSS_STANCE: &str = "2H_Ranged,Blunderbuss";
        /// The input counters of the two swings.
        const FIRST_PRESS: u16 = 10;
        const SECOND_PRESS: u16 = 20;
        let gatherable = |id: &str, event: &str, gather_ms, respawn_ms| Gatherable {
            id: id.to_owned(),
            gather_ms,
            respawn_ms: (respawn_ms, respawn_ms),
            loot_table: None,
            loot_container: false,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: Some(event.to_owned()),
        };
        let gatherables = Gatherables::build([
            gatherable(
                TREASURE_GATHERABLE,
                "MSQ_Treasure_A_interact",
                TREASURE_GATHER_MS,
                0,
            ),
            Gatherable {
                loot_table: Some(NODULE_LOOT.to_owned()),
                ..gatherable(NODULE, NODULE_EVENT, 0, NODULE_RESPAWN_MS)
            },
        ]);
        // The nodule stands where the player stands.
        let nodule = GatherablePoint {
            gatherable_id: lowercase_crc32(NODULE),
            position: [X, 10.0, 50.0],
            ..GatherablePoint::default()
        };
        let capital = nw_world::records::Capital {
            id: NODULE_CAPITAL.to_owned(),
            position: nodule.position,
            ..nw_world::records::Capital::default()
        };
        let quest_objects = QuestObjects::build(
            &[nodule],
            &gatherables,
            &nw_content::gathering::DetectionSpheres::default(),
            &crate::objectives::counted_game_events(&wreck_rows()),
            &[capital],
        );
        // The kit sword shoots as the retail blunderbuss: its stance's hit scan reaches 14 m.
        let mut source = crate::combat::test_support::test_source();
        let sword = source.weapons[0].1;
        source.weapons.push((
            BLUNDERBUSS.to_owned(),
            nw_content::combat::WeaponRow {
                spread_shot: true,
                stance: nw_content::player_attacks::stance_key(BLUNDERBUSS_STANCE),
                ..sword
            },
        ));
        source
            .item_stats
            .push((gear::tests::SWORD.to_owned(), BLUNDERBUSS.to_owned()));
        let shooting = nw_content::combat::CombatData::build(
            source,
            &nw_content::combat::CombatRows::default(),
        )
        .expect("data")
        .with_player_attacks(crate::combat::test_support::test_player_attacks());
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            crate::config::tests::committed().combat_settings(
                Arc::new(shooting),
                Arc::from([]),
                crate::combat::tests::test_mob_ai(),
            ),
            Arc::new(PlayerContent {
                kits: gear::tests::kits_with(&[(TREASURE, "Resource")]),
                achievements: AchievementIndexes::build([(MANIFEST_LORE.to_owned(), 100)]),
                quests: QuestBook::build(
                    [ObjectiveRow {
                        id: QUEST.to_owned(),
                        root: WRECK_ROOT.to_owned(),
                        turn_in: Some("01_GraceMB_02".to_owned()),
                        ..ObjectiveRow::default()
                    }],
                    TaskBook::new(wreck_rows()),
                ),
                gatherables,
                rewards: Rewards::build([(
                    WHISPER_EVENT.to_owned(),
                    Reward {
                        whisper: Some(WHISPER.to_owned()),
                        ..Reward::default()
                    },
                )]),
                poi_tags: PoiTags::build([(u32::from(WRECK_TERRITORY), WRECK_POI.to_owned())]),
                quest_objects,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let mut admit = admit_at(1, X, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let who = connection(1);
        let entity = world.by_connection[&who];
        let mut now = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, now);
        world.heard_from(&who);
        world.start_quest(1, QUEST, now).expect("accepted");
        let mut step = |world: &mut SimWorld, ms: u64| {
            now += ms;
            (tick(world, now).messages, now)
        };
        let header = FacetHeader::of(0, 1);
        let territory = |entered| {
            let (area, area_entity, extra, territory) = ([0; 16], 0, 0, WRECK_TERRITORY);
            if entered {
                ClientFacetMsg::TerritoryEntered {
                    header,
                    area,
                    area_entity,
                    extra,
                    territory,
                    unknown: 0,
                }
            } else {
                ClientFacetMsg::TerritoryExited {
                    header,
                    area,
                    area_entity,
                    extra,
                    territory,
                }
            }
        };
        let done = |world: &SimWorld, task: &str| {
            world
                .quests_of_player(1)
                .first()
                .is_some_and(|quest| quest.run.is_done(task))
        };
        let holds = |world: &SimWorld, item: &str| -> u32 {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|stack| stack.item_id == item)
                .map(|stack| stack.quantity)
                .sum()
        };
        let request = |kind, interaction, gatherable| InteractionRequest {
            target: [0x31; nw_messages::wire::RAW_ID_LEN],
            target_entity: 0x1234,
            interaction,
            kind,
            gatherable,
        };

        let (_, at) = step(&mut world, TICK_MS);
        assert!(!done(&world, "GoToWreck"), "not at the wreck yet");
        world
            .facet_message(&who, territory(true), at)
            .expect("facet");
        let (_, at) = step(&mut world, TICK_MS);
        assert!(done(&world, "GoToWreck"), "the client says it is in");

        // The manifest: the read is accepted with its title, and its lore fires the whisper.
        world.interact(&who, request(LORE_KIND, 1, None), MANIFEST, at);
        let (read, at) = step(&mut world, TICK_MS);
        let read = messages_to(&read, 1);
        assert!(
            read.iter().any(|message| matches!(message,
                ServerMessage::InteractionValidated(answer)
                    if answer.kind == LORE_KIND && answer.status == 1 && answer.name == MANIFEST)),
            "{read:?}"
        );
        assert!(done(&world, "ShipsManifest"));
        assert!(
            read.iter().any(|message| matches!(message,
                ServerMessage::StartWhisper(start) if start.whisper == lowercase_crc32(WHISPER))),
            "the whisper plays: {read:?}"
        );

        // Two swings break the nodule; a shot breaks it again once it is back. The client
        // repeats the first press in its next input: it does not count again.
        let here = InputPosition::from_metres(X, 10.0, 50.0).expect("finite");
        let press = |counter| nw_messages::ClientInputMsg::attack(here, counter).records;
        world.swing_at_quest_objects(&who, &press(FIRST_PRESS), at);
        world.swing_at_quest_objects(&who, &press(FIRST_PRESS), at + TICK_MS);
        assert!(
            world.quests_of_player(1)[0]
                .run
                .counts_game_event(NODULE_EVENT)
        );
        assert!(!done(&world, "Nodules"), "one swing so far");
        let (hit, at) = step(&mut world, SWING_GAP_MS);
        let hit = messages_to(&hit, 1);
        assert!(
            hit.iter().any(|message| matches!(message,
                ServerMessage::DamageDealt(dealt) if dealt.u64_c == NODULE_ID)),
            "the swing's 2071 names the nodule: {hit:?}"
        );
        assert!(
            hit.iter().any(|message| matches!(message,
            ServerMessage::GatheringInterrupted(ended) if ended.header
                == FacetHeader {
                    target: 0x21a8_8912_921f_9348,
                    entity: NODULE_ID,
                })),
            "the retail 2724 of nodule A: {hit:?}"
        );
        world.swing_at_quest_objects(&who, &press(SECOND_PRESS), at);
        let tracked = world
            .world
            .get::<Player>(entity)
            .map(|player| player.loot.opened_kind(&crafting::gde_ref_of(NODULE_ID)))
            .expect("player");
        assert_eq!(
            tracked,
            Some((nw_messages::LOOT_QUEST_OBJECT, lowercase_crc32(NODULE_LOOT))),
            "the break lists it in the loot tracker"
        );
        let names_the_nodule = world
            .world
            .get::<Player>(entity)
            .expect("player")
            .extra_members
            .iter()
            .any(|member| {
                matches!(&member.state, MemberState::Gathering(state)
                    if state.gatherable == Some(NODULE_ID) && state.status.is_none())
            });
        assert!(
            names_the_nodule,
            "retail 12-5: the break's gather state names the nodule"
        );
        let (_, at) = step(&mut world, SWING_GAP_MS);
        world.shoot_at_quest_objects(&who, at);
        assert!(!done(&world, "Nodules"), "broken until it grows back");
        let (_, at) = step(&mut world, NODULE_RESPAWN_MS);
        world.shoot_at_quest_objects(&who, at);
        assert!(done(&world, "Nodules"));

        // The treasure: a gather of its quest object drops it.
        let crc = Some(lowercase_crc32(TREASURE_GATHERABLE));
        world.interact(&who, request(GATHER_KIND, 2, crc), &[], at);
        step(&mut world, TICK_MS);
        step(&mut world, TREASURE_GATHER_MS + TICK_MS);
        assert_eq!(holds(&world, TREASURE), 1);
        assert!(done(&world, "Treasure"));

        let (_, at) = step(&mut world, TICK_MS);
        assert!(done(&world, "LeaveTrigger"), "far from the area");
        assert!(
            !world.quests_of_player(1)[0].run.finished(),
            "still at the wreck"
        );
        world
            .facet_message(&who, territory(false), at)
            .expect("facet");
        let (_, at) = step(&mut world, TICK_MS);
        assert!(world.quests_of_player(1)[0].run.finished());

        // The turn-in takes the treasure back and refreshes the schedule.
        let complete = ClientFacetMsg::CompleteObjective {
            header,
            objective: lowercase_crc32(QUEST),
        };
        world.facet_message(&who, complete, at).expect("facet");
        let (turned_in, _) = step(&mut world, TICK_MS);
        assert!(!world.quests.contains_key(&1));
        assert_eq!(holds(&world, TREASURE), 0, "given back");
        assert!(
            messages_to(&turned_in, 1)
                .iter()
                .any(|message| matches!(message, ServerMessage::GameModeSchedulerRefreshed(_)))
        );
    }

    #[test]
    fn the_coins_go_out_with_the_card_and_after_a_change() {
        let mut world = beach_world();
        let entity = world.by_connection[&connection(1)];
        let coins_sent = |world: &mut SimWorld, now: u64| -> Vec<u64> {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| groups_in(world, &connection(1), &bundle))
                .unwrap_or_default()
                .into_iter()
                .filter(|(number, _)| *number == 1)
                .flat_map(|(_, update)| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Currency(state) => state.coins,
                    _ => None,
                })
                .collect()
        };
        let later = NOW + 2 * OWN_CARD_DELAY_MS;
        assert!(
            coins_sent(&mut world, later).is_empty(),
            "no change, no coins"
        );
        if let Some(mut player) = world.world.get_mut::<Player>(entity) {
            player.coins = 375;
            player.coins_changed = true;
        }
        assert_eq!(coins_sent(&mut world, later + 33), [375]);
        let player = world.world.get::<Player>(entity).expect("player");
        let body = world.world.get::<Body>(entity).expect("body");
        let vitals = world.world.get::<Vitals>(entity).expect("vitals");
        let offer = handoff_offer(player, body, vitals, later, None, PlayerQuests::default());
        assert_eq!(offer.progression.coins, 375, "a handoff carries them");
    }

    #[test]
    fn the_ship_fight_shows_its_sailors_to_their_player_and_brings_two_more() {
        let prefab = format!(
            r#"[{{"vitals_id":{},"vitals_name":null,"asset":"{SAILOR_ASSET_HEX}","sub_id":2,
                "alc_component":5,"vitals_component":9,"first_members":[],"death_members":[],
                "seen":1}}]"#,
            encounter::tests::sailor_vitals()
        );
        let prefabs = nw_content::mob_prefabs::MobPrefabs::parse(&prefab).expect("table");
        let (mob_looks, _) = crate::player_content::MobLooks::of(&prefabs);
        let mut tutorial_settings = settings(MANY, MAX_PLAYERS);
        tutorial_settings.level = TUTORIAL_LEVEL;
        let mut world = SimWorld::new(
            tutorial_settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("tut-1"),
            encounter::tests::sailor_combat(),
            Arc::new(PlayerContent {
                mob_looks,
                kill_types: encounter::tests::kill_types(),
                tutorial_quest: Some(encounter::tests::tree()),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let asset = world
            .content
            .mob_looks
            .look(encounter::tests::sailor_vitals(), None)
            .expect("sailor look")
            .asset;
        let sailor_cards =
            |outgoing: &[Outgoing], who: &ConnectionRef| cards_of(outgoing, who, asset);
        let (owner, other) = (connection(1), connection(2));
        for number in [1, 2] {
            let mut admit = admit_at(number, TUTORIAL_START.x, true);
            admit.position = Point::from(nw_sim_core::TUTORIAL_START);
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        }
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&owner);
        world.heard_from(&other);
        let output = tick(&mut world, spawn_at + 33);
        assert!(
            sailor_cards(&output.messages, &owner).is_empty(),
            "the first sailor comes later, as on retail"
        );
        let card_at = NOW + encounter::FIRST_SAILOR_DELAY_MS;
        let output = tick(&mut world, card_at + 33);
        let first = sailor_cards(&output.messages, &owner);
        assert_eq!(first.len(), 1, "the owner sees its waiting sailor");
        assert!(
            sailor_cards(&output.messages, &other)
                .iter()
                .all(|sailor| !first.contains(sailor)),
            "another player does not see it"
        );
        let mut again = 0;
        for step in 2..=CARD_REFRESH_BUNDLES + 1 {
            let output = tick(&mut world, card_at + 33 * step);
            again += sailor_cards(&output.messages, &owner)
                .iter()
                .filter(|sailor| **sailor == first[0])
                .count();
        }
        assert_eq!(again, 0, "a mob with a prefab gets its card once");
        let card_at = card_at + 33 * CARD_REFRESH_BUNDLES;
        let header = nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 1);
        world
            .facet_message(
                &owner,
                ClientFacetMsg::QteCompleted {
                    header,
                    sequence: 0,
                },
                card_at,
            )
            .expect("lesson done");
        let sailor = world.mobs.get_mut(first[0]).expect("sailor");
        let max = sailor.health.max();
        sailor.health.take(max);
        let mut seen = Vec::new();
        let last_step = 5 + encounter::LAST_SAILOR_DELAY_MS / 33;
        for step in 2..last_step {
            let output = tick(&mut world, card_at + 33 * step);
            seen.extend(sailor_cards(&output.messages, &owner));
        }
        seen.retain(|sailor| *sailor != first[0]);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 2, "two more sailors come to their owner");
    }

    #[test]
    fn the_tutorial_exit_moves_a_spawned_player_into_the_open_world() {
        let mut tutorial = tutorial_world();
        assert!(
            tutorial
                .set_table(&table(&["sim-a", "sim-b"]))
                .expect("table")
                .is_empty(),
            "a tutorial sim keeps its players whatever the open-world table says"
        );
        /// The tracker version the tutorial left (live run 12: 0x26).
        const TUTORIAL_TRACKER_VERSION: u64 = 0x26;
        let mut at_exit = admit_at(1, TUTORIAL_EXIT.x, true);
        at_exit.position = Point::from(TUTORIAL_EXIT);
        at_exit.quests = PlayerQuests {
            tracker_version: TUTORIAL_TRACKER_VERSION,
            ..PlayerQuests::default()
        };
        tutorial.admit(&at_exit, NOW, SERVER_TIME).expect("admit");
        let card_at = NOW + OWN_CARD_DELAY_MS;
        assert!(
            tick(&mut tutorial, card_at).handoffs.is_empty(),
            "a player whose client has not spawned stays"
        );
        tutorial.heard_from(&connection(1));
        let handoffs = tick(&mut tutorial, card_at + 33).handoffs;
        assert_eq!(handoffs.len(), 1);
        let start = &handoffs[0];
        assert_eq!(start.target, slot("sim-b"), "the owner of the beach start");
        let offer = &start.offer;
        assert!(offer.enter_level);
        assert_eq!(offer.position, Point::from(OPEN_WORLD_START));
        assert_eq!(offer.client_context, LOGIN_CLIENT_CONTEXT);
        assert!(offer.slots.is_empty());
        assert_eq!(
            offer.quests.tracker_version, TUTORIAL_TRACKER_VERSION,
            "the beach lists go above the tutorial's"
        );

        let mut open = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        open.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let change = open
            .take(offer, card_at, SERVER_TIME)
            .expect("take")
            .expect("level change messages");
        let level = level_info_in(&change);
        assert_eq!(level.level, OPEN_WORLD_LEVEL);
        assert_eq!(level.client_context, LOGIN_CLIENT_CONTEXT + 1);
        let messages = tick(&mut open, card_at + OWN_CARD_DELAY_MS).messages;
        let bundle = bundle_of(&messages, &connection(1)).expect("the new spawn card");
        assert_eq!(
            bundle.byte_3,
            LOGIN_CLIENT_CONTEXT + 1,
            "bundles carry the new context"
        );
        assert_eq!(
            bundle.leading,
            Some(FIRST_LOGIN_BUNDLE_SEQUENCE),
            "a new level starts the bundle sequence as a login does"
        );
        assert!(
            SlotRead::of(&bundle)
                .expect("read")
                .updates
                .iter()
                .any(|update| update.is_local_player() && update.card.is_some()),
            "the player spawns again in the new level"
        );
    }

    #[test]
    fn a_client_message_after_the_card_ends_the_hold() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let own_updates = |world: &mut SimWorld, now: u64| {
            let messages = tick(world, now).messages;
            bundle_of(&messages, &connection(1))
                .map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .unwrap_or_default()
                .iter()
                .filter(|update| update.is_local_player())
                .count()
        };
        world.heard_from(&connection(1));
        let card_at = NOW + OWN_CARD_DELAY_MS;
        assert_eq!(own_updates(&mut world, card_at), 1, "the card");
        assert_eq!(
            own_updates(&mut world, card_at + 33),
            0,
            "a message before the card does not end the hold"
        );
        world.heard_from(&connection(1));
        for step in 2..=4 {
            assert_eq!(
                own_updates(&mut world, card_at + 33 * step),
                1,
                "the position every tick once the client spawned"
            );
        }
    }

    #[test]
    fn a_move_before_the_spawn_keeps_the_hold_and_the_card_goes_again() {
        // Live 2026-09-22 (the beach switch): the client sent movement on the new
        // connection before the level had loaded; the move ended the hold, the mob cards
        // reached a client with no level context, and the first bundle it read after the
        // load failed. A move is not the spawn: the card bundle goes again until the
        // time sync request, and no other entity goes before it.
        let mut world = world_with_spawns(vec![test_spawn(CELL_M / 2.0 + 5.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let who = connection(1);
        let card_at = NOW + OWN_CARD_DELAY_MS;
        let first = tick(&mut world, card_at);
        let card_bundle = bundle_of(&first.messages, &who).expect("the card bundle");
        assert!(
            first
                .messages
                .iter()
                .filter(|out| out.connection == who && out.channel == REPLICATION_CHANNEL)
                .all(|out| out.reliable),
            "the card bundle goes reliably"
        );
        let entity = world.by_connection[&who];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let point = nw_messages::ClientInputMsg::position(
            InputPosition::from_metres(at.x, at.y, at.z).expect("finite"),
        );
        let _ = world.move_from_input(&who, &point.records, card_at + 33);
        let held = tick(&mut world, card_at + 33);
        assert_eq!(held.bundle_members, 0, "a move does not end the hold");
        let resent = tick(&mut world, card_at + OWN_CARD_RESEND_MS);
        assert_eq!(
            bundle_of(&resent.messages, &who).expect("the card again"),
            card_bundle,
            "the same card bundle goes out again"
        );
        world.heard_from(&who);
        let just_spawned = tick(&mut world, card_at + OWN_CARD_RESEND_MS + 33);
        let carded_at_spawn = just_spawned
            .messages
            .iter()
            .filter(|out| out.connection == who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| SlotRead::of(&bundle_in(bytes)).expect("read").updates)
            .any(|update| !update.is_local_player() && update.card.is_some());
        assert!(
            !carded_at_spawn,
            "other entities wait after the time sync (live 2026-09-24: \
             statics 33 ms after InGame failed StateReplication 6077; \
             20 first-cards failed 4585)"
        );
        let streamed = tick(
            &mut world,
            card_at + OWN_CARD_RESEND_MS + 33 + OTHER_ENTITY_SETTLE_MS,
        );
        let carded = streamed
            .messages
            .iter()
            .filter(|out| out.connection == who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| SlotRead::of(&bundle_in(bytes)).expect("read").updates)
            .any(|update| !update.is_local_player() && update.card.is_some());
        assert!(carded, "the mob card goes after the settle");
    }

    #[test]
    fn the_own_card_names_the_login_character() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let admit = admit_at(1, CELL_M / 2.0, true);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let messages = tick(&mut world, NOW + OWN_CARD_DELAY_MS).messages;
        let bundle = bundle_of(&messages, &connection(1)).expect("bundle");
        let own = SlotRead::of(&bundle)
            .expect("read")
            .updates
            .into_iter()
            .find(SlotUpdate::is_local_player)
            .expect("own group");
        let character_id = own.members.iter().find_map(|member| match &member.state {
            MemberState::PlayerComponent(state) => state.character_id,
            _ => None,
        });
        assert_eq!(
            character_id,
            Some(*admit.character.as_bytes()),
            "the client sees its local player only when characterId is its login character"
        );
    }

    #[test]
    fn the_own_card_carries_the_saved_look_and_the_health() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.look = Some(SAVED_LOOK);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let max = world.combat.data.player_base_health();
        let messages = tick(&mut world, NOW + OWN_CARD_DELAY_MS).messages;
        let bundle = bundle_of(&messages, &connection(1)).expect("bundle");
        let own = SlotRead::of(&bundle)
            .expect("read")
            .updates
            .into_iter()
            .find(SlotUpdate::is_local_player)
            .expect("own group");
        let state_of = |class: u64| {
            own.members
                .iter()
                .find(|member| member.state.type_index() == class)
                .map(|member| member.state.clone())
        };
        assert_eq!(
            state_of(PLAYER_APPEARANCE_TYPE_INDEX),
            Some(MemberState::Appearance(PlayerAppearanceState::of(
                PlayerLook::of_choices(SAVED_LOOK)
            ))),
            "the saved look, not the default mesh"
        );
        assert_eq!(
            state_of(VITALS_TYPE_INDEX),
            Some(MemberState::Vitals(VitalsState::player(max, max))),
            "full health of the player vitals row, so the bar is not 00/00"
        );
    }

    /// The states that `who` got of entity `entity` in every replication bundle of `outgoing`.
    fn states_of(
        world: &SimWorld,
        outgoing: &[Outgoing],
        who: &ConnectionRef,
        entity: u64,
    ) -> Vec<MemberState> {
        outgoing
            .iter()
            .filter(|out| out.connection == *who && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| groups_in(world, who, &bundle_in(bytes)))
            .filter(|(number, _)| *number == entity)
            .flat_map(|(_, update)| update.members)
            .map(|member| member.state)
            .collect()
    }

    /// The slot-0 ALC children of the remote states among `states`.
    fn alc_children(states: &[MemberState]) -> Vec<(usize, Vec<u8>)> {
        states
            .iter()
            .filter_map(|state| match state {
                MemberState::Alc(alc) => alc.slot0_children().ok(),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Two players 5 m apart, both past their cards.
    fn two_players() -> SimWorld {
        let mut world = world();
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 5.0);
        tick(&mut world, NOW);
        world
    }

    /// An input record with block held.
    fn block_record(counter: u16) -> InputRecord {
        const BLOCK: u64 = 1 << 62;
        InputRecord {
            u16_after_mask: counter,
            slots: nw_messages::client_input::InputSlots {
                count: 19,
                values: vec![0, BLOCK],
                bytes: [128; 4],
            },
            ..InputRecord::default()
        }
    }

    #[test]
    fn a_player_in_a_fight_stays_in_sight_over_nearer_players() {
        const ONE: usize = 1;
        let mut world = world_of("sim-a", settings(ONE, MAX_PLAYERS));
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 5.0);
        join(&mut world, 3, 40.0);
        let first = tick(&mut world, NOW);
        assert_eq!(
            seen_by(&world, &first.messages, &connection(1)),
            [1, 2],
            "the nearest"
        );
        for number in [1, 3] {
            let entity = world.player_by_number(number).expect("player");
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .combat
                .hit(NOW + 1, false);
        }
        // The own player's later places are offsets: only the others carry a pos10 now.
        let fight = tick(&mut world, NOW + 33);
        assert_eq!(
            seen_by(&world, &fight.messages, &connection(1)),
            [3],
            "the one it fights"
        );
    }

    #[test]
    fn another_player_sees_the_stance_facing_and_block_in_the_next_tick() {
        /// The ALC children of facing, look direction and slayer state (replication note).
        const FACING: usize = 11;
        const LOOK_DIR: usize = 12;
        const SLAYER_STATE: usize = 13;
        /// The slayer state of a move from standing still (movement study).
        const START_STATE: u8 = 0x1a;
        const VIEW: LookDir = LookDir {
            yaw: 1.0,
            pitch: 0.3,
        };
        let mut world = two_players();
        let (a, b) = (connection(1), connection(2));
        let input_at = NOW + 33;
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let _ = world.move_from_input(
            &a,
            &[camera_record(1, at, VIEW, [0.0; 2], FORWARD_SLOTS)],
            input_at,
        );
        world.attack_input(&a, &[block_record(1)], input_at);
        let outgoing = tick(&mut world, input_at);
        let seen = states_of(&world, &outgoing.messages, &b, 1);
        let alc = alc_children(&seen);
        let child = |index| {
            alc.iter()
                .find(|(c, _)| *c == index)
                .map(|(_, b)| b.clone())
        };
        assert_eq!(
            child(SLAYER_STATE),
            Some(vec![START_STATE]),
            "the start of its move"
        );
        assert_eq!(
            child(FACING),
            Some(nw_messages::alc::rotation_bytes(VIEW.yaw)),
            "the facing"
        );
        assert_eq!(
            child(LOOK_DIR),
            Some(VIEW.bytes()),
            "where it looks, with the pitch"
        );
        assert!(
            seen.iter().any(|state| matches!(
                state,
                MemberState::DamageReceiver(receiver) if receiver.is_block_active == Some(true)
            )),
            "the block: {seen:?}"
        );
    }

    #[test]
    fn own_states_take_the_number_of_the_newest_input_record_ahead_of_the_client() {
        // Retail newchar capture (243164 to 243197 ms): records 6497 and 6498 went together, and
        // the next own state was `0=62` (6498), skipping 6497; it repeated unchanged until the
        // next record. Ours is that record's number [`OWN_STATE_LEAD`] ahead.
        const RECORDS: [u16; 2] = [6497, 6498];
        let own_id = own_state_counter(RECORDS[1]).to_be_bytes();
        /// The ALC children of the number and time (sailor-movement note).
        const ID_REL: usize = 0;
        const TIME_REL: usize = 1;
        const ID_ABS: usize = 25;
        const TIME_ABS: usize = 26;
        let mut world = two_players();
        let a = connection(1);
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let view = LookDir {
            yaw: 0.0,
            pitch: 0.3,
        };
        let newest_first: Vec<InputRecord> = RECORDS
            .iter()
            .rev()
            .map(|counter| camera_record(*counter, at, view, [0.0; 2], STANDING_SLOTS))
            .collect();
        let _ = world.move_from_input(&a, &newest_first, NOW + 33);
        let stamp_of = |world: &SimWorld, outgoing: &TickOutput| {
            let own = alc_children(&states_of(world, &outgoing.messages, &a, 1));
            let child = |index| {
                own.iter()
                    .find(|(child, _)| *child == index)
                    .map(|(_, bytes)| bytes.clone())
                    .expect("child")
            };
            (
                [child(ID_ABS)[0], child(ID_REL)[0]],
                [child(TIME_ABS)[0], child(TIME_REL)[0]],
            )
        };
        let first = tick(&mut world, NOW + 33);
        let (id, time) = stamp_of(&world, &first);
        assert_eq!(id, own_id, "the newest record's number");
        let again = tick(&mut world, NOW + 66);
        assert_eq!(
            stamp_of(&world, &again),
            (id, time),
            "no new record: the same state"
        );
        let next = camera_record(RECORDS[1] + 1, at, view, [0.0; 2], STANDING_SLOTS);
        let _ = world.move_from_input(&a, &[next], NOW + 99);
        let later = tick(&mut world, NOW + 99);
        assert_eq!(
            stamp_of(&world, &later).0,
            own_state_counter(RECORDS[1] + 1).to_be_bytes(),
            "the next record's"
        );
    }

    #[test]
    fn a_client_that_numbers_no_records_gets_numbers_on_the_clock() {
        // The test client and the load-test bots send counter 0 and no view angles.
        let mut world = two_players();
        let a = connection(1);
        let spawned = NOW + OWN_CARD_DELAY_MS;
        world
            .tick(spawned, SERVER_TIME + OWN_CARD_DELAY_MS)
            .expect("spawn");
        let id_of = |world: &SimWorld, outgoing: &TickOutput| {
            alc_children(&states_of(world, &outgoing.messages, &a, 1))
                .into_iter()
                .find(|(child, _)| *child == 0)
                .map(|(_, bytes)| bytes[0])
                .expect("id")
        };
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let point = nw_messages::ClientInputMsg::position(
            InputPosition::from_metres(at.x, at.y, at.z).expect("finite"),
        );
        let _ = world.move_from_input(&a, &point.records, spawned + 33);
        let first = world
            .tick(spawned + 33, SERVER_TIME + OWN_CARD_DELAY_MS + 33)
            .expect("tick");
        let later = world
            .tick(spawned + 99, SERVER_TIME + OWN_CARD_DELAY_MS + 99)
            .expect("tick");
        assert_eq!(
            id_of(&world, &later).wrapping_sub(id_of(&world, &first)),
            2,
            "one number per 33 ms"
        );
    }

    #[test]
    fn the_first_own_state_takes_the_input_number_and_the_sequence_time_runs_on() {
        // Retail: every own state is numbered with the client's newest record, the first one
        // too (2 in most logins), and its slayer slots started the state before.
        const BEFORE_SPAWN: [u16; 2] = [41, 40];
        const NEXT: u16 = 42;
        /// The ALC children of the number, the sequence time and the slayer start.
        const ID_REL: usize = 0;
        const SEQUENCE_TIME_REL: usize = 2;
        const STARTED_IN: usize = 14;
        let mut world = world();
        join(&mut world, 1, 0.0);
        let a = connection(1);
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let view = LookDir {
            yaw: 0.0,
            pitch: 0.3,
        };
        let records: Vec<InputRecord> = BEFORE_SPAWN
            .iter()
            .map(|counter| camera_record(*counter, at, view, [0.0; 2], STANDING_SLOTS))
            .collect();
        let _ = world.move_from_input(&a, &records, NOW);
        let spawned = NOW + OWN_CARD_DELAY_MS;
        let card = tick(&mut world, spawned);
        let first = alc_children(&states_of(&world, &card.messages, &a, 1));
        let [_, newest] = own_state_counter(BEFORE_SPAWN[0]).to_be_bytes();
        assert!(first.contains(&(ID_REL, vec![newest])), "{first:?}");
        assert!(first.contains(&(
            STARTED_IN,
            own_state_counter(BEFORE_SPAWN[1]).to_be_bytes().to_vec()
        )));
        let next = camera_record(NEXT, at, view, [0.0; 2], STANDING_SLOTS);
        let _ = world.move_from_input(&a, &[next], spawned + 33);
        let later = tick(&mut world, spawned + 33);
        let own = alc_children(&states_of(&world, &later.messages, &a, 1));
        let [_, next_id] = own_state_counter(NEXT).to_be_bytes();
        assert!(own.contains(&(ID_REL, vec![next_id])), "{own:?}");
        assert!(
            own.contains(&(SEQUENCE_TIME_REL, vec![0x83])),
            "the first state's two steps and one record more: {own:?}"
        );
    }

    #[test]
    fn the_own_state_and_another_player_get_the_action_then_its_follow_up() {
        /// The slayer sequence child (slayer note).
        const SEQUENCE: usize = 15;
        /// Retail newchar capture: record 5568 started 5055, and the server followed with 4603.
        const STARTED: u64 = 5055;
        const FOLLOW_UP: u64 = 4603;
        const RECORD: u16 = 5568;
        /// The slayer start child (slayer note).
        const STARTED_IN: usize = 14;
        let mut world = two_players();
        let sequences = crate::combat::test_support::test_player_sequences();
        let data = std::sync::Arc::clone(&world.combat.data);
        world.combat.data =
            std::sync::Arc::new(data.as_ref().clone().with_player_sequences(sequences));
        let (a, b) = (connection(1), connection(2));
        let entity = world.by_connection[&a];
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let view = LookDir {
            yaw: 0.0,
            pitch: 0.3,
        };
        /// The attack-key release (value 2, bit 31). This follow-up starts at that release.
        const RELEASE_SLOT: usize = 2;
        const RELEASE_BIT: u32 = 31;
        let mut attack = InputRecord {
            row_1: Some([STARTED, 0x002c_000a, 0]),
            ..camera_record(RECORD, at, view, [0.0; 2], STANDING_SLOTS)
        };
        attack.slots.values[RELEASE_SLOT] |= 1 << RELEASE_BIT;
        let _ = world.move_from_input(&a, &[attack], NOW + 33);
        let sequence_bytes = |sequence: u64| {
            nw_messages::wire::Writer::new()
                .prefixed(sequence)
                .expect("prefix")
                .finish()
        };
        let sequences = |world: &SimWorld, outgoing: &[Outgoing], who: &ConnectionRef| {
            alc_children(&states_of(world, outgoing, who, 1))
                .into_iter()
                .filter(|(child, _)| *child == SEQUENCE)
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>()
        };
        let first = tick(&mut world, NOW + 33);
        assert!(
            sequences(&world, &first.messages, &a).contains(&sequence_bytes(STARTED)),
            "the own state echoes the action"
        );
        let own = alc_children(&states_of(&world, &first.messages, &a, 1));
        assert!(
            own.contains(&(STARTED_IN, own_state_counter(RECORD).to_be_bytes().to_vec())),
            "started in the state of its record: {own:?}"
        );
        assert!(
            sequences(&world, &first.messages, &b).contains(&sequence_bytes(STARTED)),
            "the other player sees it in the same tick"
        );
        let next = tick(&mut world, NOW + 66);
        let followed = sequences(&world, &next.messages, &a);
        assert!(
            followed.contains(&sequence_bytes(FOLLOW_UP)),
            "the server's follow-up: {followed:?}"
        );
        assert!(sequences(&world, &next.messages, &b).contains(&sequence_bytes(FOLLOW_UP)));
    }

    #[test]
    fn a_sheathed_tap_starts_the_light_attack_and_keeps_it() {
        /// The slayer sequence child (slayer note).
        const SEQUENCE: usize = 15;
        /// The standing light of `1H_Melee,Sword` in the mined table.
        const SWORD_LIGHT: u64 = 495;
        const PRESS_COUNTER: u16 = 0x6ba;
        const RELEASE_COUNTER: u16 = 0x6bd;
        const NEXT_COUNTER: u16 = 0x6be;
        const ATTACK_SLOT: usize = 2;
        const ATTACK_PRESS_BIT: u32 = 40;
        const ATTACK_RELEASE_BIT: u32 = 31;
        let mut source = crate::combat::test_support::test_source();
        source.item_stats.push((
            gear::tests::SWORD.to_owned(),
            nw_content::combat::DEFAULT_WEAPON_ID.to_owned(),
        ));
        let data = nw_content::combat::CombatData::build(
            source,
            &nw_content::combat::CombatRows::default(),
        )
        .expect("combat")
        .with_player_attacks(crate::combat::test_support::test_player_attacks())
        .with_player_sequences(crate::combat::test_support::test_player_sequences());
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        combat.data = Arc::new(data);
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let a = connection(1);
        let entity = world.by_connection[&a];
        world
            .world
            .get_mut::<Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        let at = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let view = LookDir {
            yaw: 0.0,
            pitch: 0.3,
        };
        let attack_record = |counter, bit| {
            let mut slots = STANDING_SLOTS;
            slots[ATTACK_SLOT] |= 1_u64 << bit;
            camera_record(counter, at, view, [0.0; 2], slots)
        };
        let records = [
            attack_record(RELEASE_COUNTER, ATTACK_RELEASE_BIT),
            attack_record(PRESS_COUNTER, ATTACK_PRESS_BIT),
        ];
        let _ = world.move_from_input(&a, &records, NOW + OWN_CARD_DELAY_MS + 33);
        world.attack_input(&a, &records, NOW + OWN_CARD_DELAY_MS + 33);
        let sequence_bytes = |sequence: u64| {
            nw_messages::wire::Writer::new()
                .prefixed(sequence)
                .expect("prefix")
                .finish()
        };
        let sequences = |world: &SimWorld, outgoing: &[Outgoing], who: &ConnectionRef| {
            alc_children(&states_of(world, outgoing, who, 1))
                .into_iter()
                .filter(|(child, _)| *child == SEQUENCE)
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>()
        };
        let first = tick(&mut world, NOW + OWN_CARD_DELAY_MS + 33);
        assert!(
            sequences(&world, &first.messages, &a).contains(&sequence_bytes(SWORD_LIGHT)),
            "the tap starts the light attack: {:?}",
            sequences(&world, &first.messages, &a)
        );
        let _ = world.move_from_input(
            &a,
            &[camera_record(
                NEXT_COUNTER,
                at,
                view,
                [0.0; 2],
                STANDING_SLOTS,
            )],
            NOW + OWN_CARD_DELAY_MS + 66,
        );
        let later = tick(&mut world, NOW + OWN_CARD_DELAY_MS + 66);
        assert!(
            sequences(&world, &later.messages, &a).contains(&sequence_bytes(SWORD_LIGHT)),
            "the next own state keeps the light: {:?}",
            sequences(&world, &later.messages, &a)
        );
    }

    #[test]
    fn another_player_sees_the_hit_and_the_death_in_the_same_tick() {
        const HIT: u32 = 100;
        const LETHAL: u32 = 100_000;
        let mut world = two_players();
        let b = connection(2);
        let entity = world.player_by_number(1).expect("player");
        let max = world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .max();
        world.apply_hit(1, crate::mobs::mob_entity(0), HIT, 0, NOW + 1);
        let hit = tick(&mut world, NOW + 33);
        let health = |states: &[MemberState]| {
            states.iter().find_map(|state| match state {
                MemberState::Vitals(vitals) => Some(vitals.slot_0?),
                _ => None,
            })
        };
        let after = health(&states_of(&world, &hit.messages, &b, 1)).expect("the hit");
        assert_eq!(after.health_amount, Some(attack::amount_of(max - HIT)));
        world.apply_hit(1, crate::mobs::mob_entity(0), LETHAL, 0, NOW + 34);
        let death = tick(&mut world, NOW + 66);
        let dead = health(&states_of(&world, &death.messages, &b, 1)).expect("the death");
        assert_eq!(dead.health_amount, Some(0.0));
        assert!(dead.vitals_data.is_some_and(|data| data.is_dead()));
    }

    #[test]
    fn another_player_sees_a_weapon_drawn() {
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        for (number, x) in [(1, CELL_M / 2.0), (2, CELL_M / 2.0 + 5.0)] {
            let mut admit = admit_at(number, x, true);
            admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            let entity = world.player_by_number(number).expect("player");
            world
                .world
                .get_mut::<Vitals>(entity)
                .expect("vitals")
                .spawned = true;
        }
        tick(&mut world, NOW + OWN_CARD_DELAY_MS);
        let entity = world.player_by_number(1).expect("player");
        world
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| Some(player.gear.as_mut()?.draw(0, NOW, 0)))
            .expect("drawn");
        let outgoing = tick(&mut world, NOW + OWN_CARD_DELAY_MS + 33);
        let seen = states_of(&world, &outgoing.messages, &connection(2), 1);
        assert!(
            seen.iter()
                .any(|state| matches!(state, MemberState::Paperdoll(_))),
            "the paperdoll with the drawn weapon: {seen:?}"
        );
    }

    #[test]
    fn a_player_of_the_sim_next_door_is_seen_whole() {
        const GHOST: u64 = 9;
        let mut world = world_of("sim-a", settings(MANY, MAX_PLAYERS));
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 2.0);
        let ghost = Ghost {
            entity: GHOST,
            position: Point {
                x: BORDER_X + 1.0,
                y: 10.0,
                z: 50.0,
            },
            mob: None,
            player: Some(PlayerGhost {
                look: Some(SAVED_LOOK),
                action: Some(nw_bus::GhostSlayer {
                    state: Some(0x1a),
                    started: Some(0x28c5),
                    sequence: 2794,
                    second_started: None,
                    second_playing: None,
                    second_stopped: false,
                }),
                ..replica::tests::source().ghost()
            }),
        };
        world.set_ghosts(&slot("sim-b").id, vec![ghost], NOW);
        let card = tick(&mut world, NOW);
        let carded = states_of(&world, &card.messages, &connection(1), GHOST);
        assert!(
            carded.iter().any(|state| matches!(
                state,
                MemberState::Appearance(look) if look.look == Some(PlayerLook::of_choices(SAVED_LOOK))
            )),
            "its look: {carded:?}"
        );
        world.set_ghosts(&slot("sim-b").id, vec![ghost], NOW + 33);
        let next = tick(&mut world, NOW + 33);
        let alc = alc_children(&states_of(&world, &next.messages, &connection(1), GHOST));
        assert!(
            alc.iter().any(|(child, _)| *child == 13),
            "its stance: {alc:?}"
        );
    }

    #[test]
    fn a_crowd_goes_out_by_priority_within_the_budget() {
        const CROWD: u64 = 80;
        let mut world = world_of("sim-a", settings(MANY, CROWD as u32));
        for number in 1..=CROWD {
            #[allow(clippy::cast_precision_loss, reason = "a small crowd")]
            join(&mut world, number, number as f32 * 0.2);
        }
        tick(&mut world, NOW);
        // Past the cards: the viewer gets at most the budget of others in one tick, the ones it
        // waited longest for next.
        let viewer = connection(1);
        let others_in = |world: &SimWorld, outgoing: &[Outgoing]| -> HashSet<u64> {
            outgoing
                .iter()
                .filter(|out| out.connection == viewer && out.channel == REPLICATION_CHANNEL)
                .flat_map(|out| &out.envelopes)
                .flat_map(|bytes| groups_in(world, &viewer, &bundle_in(bytes)))
                .map(|(number, _)| number)
                .filter(|number| *number != 1)
                .collect()
        };
        let mut seen = HashSet::new();
        for step in 1..=4_u64 {
            let outgoing = tick(&mut world, NOW + 33 * step).messages;
            let others = others_in(&world, &outgoing);
            assert!(
                others.len() <= replica::REMOTE_GROUPS_PER_TICK,
                "{} others in one tick",
                others.len()
            );
            seen.extend(others);
        }
        assert!(
            seen.len() > replica::REMOTE_GROUPS_PER_TICK,
            "everyone in turn"
        );
    }

    #[test]
    fn the_own_card_wears_the_starter_kit_and_a_handoff_keeps_it() {
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let messages = tick(&mut world, NOW + OWN_CARD_DELAY_MS).messages;
        let bundle = bundle_of(&messages, &connection(1)).expect("bundle");
        let own = SlotRead::of(&bundle)
            .expect("read")
            .updates
            .into_iter()
            .find(SlotUpdate::is_local_player)
            .expect("own group");
        let expected = Gear::starter(
            &gear::tests::kits(),
            admit.character,
            Some(gear::tests::BACKSTORY),
        )
        .expect("gear")
        .states();
        let paperdoll = own
            .members
            .iter()
            .find(|member| member.state.type_index() == PAPERDOLL_TYPE_INDEX)
            .map(|member| member.state.clone());
        assert_eq!(
            paperdoll,
            Some(MemberState::Paperdoll(Box::new(expected.paperdoll)))
        );
        let player = world
            .world
            .query::<&Player>()
            .iter(&world.world)
            .next()
            .expect("player");
        assert_eq!(
            player.backstory.as_deref(),
            Some(gear::tests::BACKSTORY),
            "a handoff offer carries it on"
        );
    }

    /// A world of `level` with the test kits and one player wearing its starter kit.
    fn world_with_kit_player(level: LevelSpec, x: f32) -> SimWorld {
        kit_world_with(level, admit_at(1, x, true))
    }

    /// A world of `level` with the test kits and the player of `admit`, of [`gear::tests::BACKSTORY`].
    fn kit_world_with(level: LevelSpec, mut admit: Admit) -> SimWorld {
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut level_settings = settings(MANY, MAX_PLAYERS);
        level_settings.level = level;
        let mut world = SimWorld::new(
            level_settings,
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        world
    }

    #[test]
    fn a_player_leaves_the_tutorial_without_the_sword_and_with_its_class_bag() {
        let mut at_exit = admit_at(1, TUTORIAL_EXIT.x, true);
        at_exit.position = Point::from(TUTORIAL_EXIT);
        let mut world = kit_world_with(TUTORIAL_LEVEL, at_exit);
        let carries = |world: &mut SimWorld, item_id: &str| {
            world
                .world
                .query::<&Player>()
                .iter(&world.world)
                .next()
                .and_then(|player| player.gear.as_ref())
                .expect("gear")
                .carried()
                .iter()
                .filter(|item| item.item_id == item_id)
                .count()
        };
        assert_eq!(
            carries(&mut world, gear::tests::SWORD),
            1,
            "the login kit's sword"
        );
        assert_eq!(carries(&mut world, gear::tests::STAFF), 0);
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        world.heard_from(&connection(1));
        assert_eq!(
            tick(&mut world, card_at + 33).handoffs.len(),
            1,
            "a player at the exit without the tutorial quest (a skip) leaves"
        );
        assert_eq!(
            carries(&mut world, gear::tests::SWORD),
            0,
            "the sword stays behind"
        );
        assert_eq!(
            carries(&mut world, gear::tests::STAFF),
            1,
            "the beach bag it never opened gives the class weapon"
        );
    }

    fn give_flint(world: &mut SimWorld) {
        let flint = gear::tests::kits()
            .item(gear::tests::FLINT)
            .expect("flint")
            .clone();
        let mut query = world.world.query::<&mut Player>();
        let mut player = query.iter_mut(&mut world.world).next().expect("player");
        assert!(player.gear.as_mut().expect("gear").add(&flint, 1));
    }

    #[test]
    fn items_are_saved_when_they_change_and_at_a_leave() {
        let mut world = world_with_kit_player(settings(MANY, MAX_PLAYERS).level, CELL_M / 2.0);
        assert_eq!(world.positions()[0].items, None, "nothing changed yet");
        give_flint(&mut world);
        let saved = world.positions()[0].items.clone().expect("changed items");
        assert!(saved.iter().any(|item| item.item_id == gear::tests::FLINT));
        assert_eq!(world.positions()[0].items, None, "taken once");
        let departure = world.leave(&connection(1)).expect("left");
        assert_eq!(departure.items, Some(saved), "a leave saves every item");
    }

    #[test]
    fn the_tutorial_saves_no_items() {
        let mut world = world_with_kit_player(TUTORIAL_LEVEL, nw_sim_core::TUTORIAL_START.x);
        give_flint(&mut world);
        assert_eq!(world.positions()[0].items, None);
        assert_eq!(world.leave(&connection(1)).expect("left").items, None);
    }

    /// A loot table `id` whose entries `cells` (item, threshold of 100,000) drop alone.
    fn loot_rows(id: &str, cells: &[(&str, &str)]) -> Vec<LootRow> {
        const MAX_ROLL: i64 = 100_000;
        let row = |id: String, cells: Vec<(u32, String)>, and_or: Option<&str>, max_roll| LootRow {
            id,
            and_or: and_or.map(str::to_owned),
            conditions: None,
            max_roll,
            trigger_limit_on_visit: false,
            cells: cells.into_iter().collect(),
            ..LootRow::default()
        };
        let column = |value: &dyn Fn(&(&str, &str)) -> String| -> Vec<(u32, String)> {
            (1..).zip(cells.iter().map(value)).collect()
        };
        vec![
            row(
                id.to_owned(),
                column(&|(item, _)| (*item).to_owned()),
                Some("AND"),
                None,
            ),
            row(format!("{id}_Qty"), column(&|_| "1".to_owned()), None, None),
            row(
                format!("{id}_Probs"),
                column(&|(_, threshold)| (*threshold).to_owned()),
                None,
                Some(MAX_ROLL),
            ),
        ]
    }

    /// A world whose test mob type drops loot table `loot_table` of `tables`, with player 1 (the
    /// test kits) at the mob.
    fn loot_world(loot_table: &str, tables: LootTables) -> SimWorld {
        loot_world_at(loot_table, tables, None)
    }

    /// [`loot_world`] with the test mob type's `LootDropChance` `chance`.
    fn loot_world_at(loot_table: &str, tables: LootTables, chance: Option<f64>) -> SimWorld {
        let mut combat = test_combat();
        combat.spawns = vec![test_spawn(CELL_M / 2.0)].into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                loot: tables,
                mob_loot: MobLoot::build([MobLootRow {
                    vitals_id: TEST_MOB.to_owned(),
                    table: Some(loot_table.to_owned()),
                    tags: None,
                    chance,
                }]),
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        world
    }

    /// The loot table of [`limited_flint_world`].
    const FLINT_TABLE: &str = "BoarLoot";
    /// A weekly reset, `LimitExpireSeconds` and `MaxLimitMult` of the flint's limit.
    const WEEKLY_EXPIRE_S: i64 = 604_800;

    /// A [`loot_world`] whose table always drops the flint, which a weekly loot limit of 1
    /// holds (retail clock: Tuesday 09:00 UTC).
    fn limited_flint_world() -> SimWorld {
        use nw_content::loot_limits::{LimitRow, LootLimitRules, LootLimits, ResetClock, Weekday};
        let tables = LootTables::build(LootSource {
            rows: loot_rows(FLINT_TABLE, &[(gear::tests::FLINT, "0")]),
            bucket_items: Vec::new(),
        });
        let mut world = loot_world(FLINT_TABLE, tables);
        Arc::make_mut(&mut world.content).loot_limits = Some(LootLimitRules {
            limits: LootLimits::build([LimitRow {
                id: gear::tests::FLINT,
                count_limit: 1,
                expire_seconds: WEEKLY_EXPIRE_S,
                max_limit_mult: WEEKLY_EXPIRE_S,
                replicated: true,
            }]),
            clock: ResetClock {
                hour_utc: 9,
                weekly_day: Weekday::Tuesday,
            },
        });
        world
    }

    /// The loot limit counts of the loot tracker members waiting for player `number`.
    fn limit_members(
        world: &mut SimWorld,
        number: u64,
    ) -> Vec<Vec<nw_messages::loot::LootLimitEntry>> {
        let entity = world.player_by_number(number).expect("player");
        world
            .world
            .get::<Player>(entity)
            .expect("player")
            .extra_members
            .iter()
            .filter_map(|member| match &member.state {
                MemberState::LootTracker(state) => {
                    state.limits.as_ref().map(|limits| limits.elements.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_loot_limit_holds_back_an_item_after_its_count() {
        let mut world = limited_flint_world();
        let mob = crate::mobs::mob_entity(0);
        world.drop_loot(mob, 1, NOW);
        assert_eq!(
            world.bags.counts(),
            (1, 1),
            "the first kill drops the flint"
        );
        let shown = limit_members(&mut world, 1);
        assert_eq!(shown.len(), 1, "the bag and the count go in one member");
        assert_eq!(
            (shown[0][0].key, shown[0][0].count),
            (
                nw_formats::objectstream::lowercase_crc32(gear::tests::FLINT),
                1
            )
        );
        assert_eq!(
            shown[0][0].first_ns, shown[0][0].ends_ns,
            "at its limit the count shows its end (retail newchar entry)"
        );
        world.drop_loot(mob, 1, NOW);
        assert_eq!(
            world.bags.counts(),
            (2, 1),
            "the limit of 1 is reached: the second roll gives nothing to loot"
        );
        assert_eq!(
            limit_members(&mut world, 1).len(),
            1,
            "a roll that counted nothing sends no new count"
        );
    }

    /// The progression of a character that saved one: experience, coins, an achievement bit
    /// and the flint's count, which ends a week after the test's server time.
    fn saved_progression() -> PlayerProgression {
        const ACHIEVEMENT_BIT: u32 = 15_317;
        let counted_ns = gear::unix_ns(SERVER_TIME);
        let week_ns = u64::try_from(WEEKLY_EXPIRE_S).expect("week") * NS_PER_SECOND;
        // Still ahead of this server time, so arrival keeps the saved refresh.
        let refresh_ns = counted_ns + week_ns;
        PlayerProgression {
            experience: 1_250,
            coins: 375,
            achievements: vec![ACHIEVEMENT_BIT],
            loot_limits: vec![nw_bus::LootLimitCount {
                limit_id: gear::tests::FLINT.to_lowercase(),
                count: 1,
                counted_ns,
                ends_ns: refresh_ns,
            }],
            arena: nw_bus::PlayerArena {
                next_base_refresh_ns: refresh_ns,
                next_mutated_refresh_ns: refresh_ns,
                next_group_trial_refresh_ns: refresh_ns,
                ..nw_bus::PlayerArena::default()
            },
            masteries: BTreeMap::new(),
            attributes: Vec::new(),
            weapon_points: Vec::new(),
            travel: nw_bus::PlayerTravel::default(),
            ..PlayerProgression::default()
        }
    }

    /// Nanoseconds in one second.
    const NS_PER_SECOND: u64 = 1_000_000_000;

    #[test]
    fn a_login_keeps_the_saved_progression_and_a_save_round_takes_its_changes() {
        const JOINER: u64 = 2;
        const MORE_COINS: u64 = 25;
        let mut world = limited_flint_world();
        world.server_ms = SERVER_TIME;
        let mut admit = admit_at(JOINER, CELL_M / 2.0, true);
        admit.progression = saved_progression();
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.player_by_number(JOINER).expect("player");
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!((player.experience, player.coins), (1_250, 375));
        assert_eq!(player.progression(), saved_progression());
        assert_eq!(
            limit_members(&mut world, JOINER).len(),
            1,
            "the login state lists the counts"
        );
        let unsaved = |world: &mut SimWorld| {
            world
                .positions()
                .into_iter()
                .find(|departure| departure.character == admit.character)
                .expect("departure")
                .progression
        };
        assert_eq!(unsaved(&mut world), None, "the database has it");
        world.drop_loot(crate::mobs::mob_entity(0), JOINER, NOW);
        assert_eq!(
            world.bags.counts(),
            (1, 0),
            "the saved count holds the flint back after the login"
        );
        world.world.get_mut::<Player>(entity).expect("player").coins += MORE_COINS;
        let changed = unsaved(&mut world).expect("a change");
        assert_eq!(changed.coins, 375 + MORE_COINS);
        assert_eq!(unsaved(&mut world), None, "taken once");
        let departure = world.leave(&admit.connection).expect("left");
        assert_eq!(
            departure.progression,
            Some(changed),
            "a logout saves the whole progression"
        );
    }

    #[test]
    fn a_handoff_carries_the_progression_and_the_taker_saves_it() {
        let mut old = world();
        old.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let mut admit = admit_at(1, BORDER_X + 50.0, true);
        admit.progression = saved_progression();
        old.admit(&admit, NOW, SERVER_TIME).expect("admit");
        tick(&mut old, NOW);
        let offer = tick_offer(&mut old);
        assert_eq!(
            offer.progression,
            saved_progression(),
            "the offer carries it"
        );
        let mut new = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        new.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        new.take(&offer, NOW, SERVER_TIME).expect("take");
        let taken = new.player_by_number(1).expect("player");
        assert_eq!(
            new.world
                .get::<Player>(taken)
                .expect("player")
                .progression(),
            saved_progression(),
            "without a level change the taker keeps the carried achievements"
        );
        assert_eq!(
            new.positions()[0].progression,
            Some(saved_progression()),
            "the sender may not have saved it: the taker's first round does"
        );
        assert_eq!(new.positions()[0].progression, None);
    }

    #[test]
    fn a_handoff_s_items_and_quests_are_saved_by_the_taker_and_its_flags_stay() {
        // The sender saves nothing at a handoff: a change of the last seconds before it (the
        // tutorial's whole kit, which the tutorial never saves) is in the database only when the
        // taker's first round saves it. The client sends its PvP toggle and auto-pin setting
        // once; the taker keeps them.
        let mut old = world();
        old.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        old.admit(&admit_at(1, BORDER_X + 50.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = old.player_by_number(1).expect("player");
        {
            let mut player = old.world.get_mut::<Player>(entity).expect("player");
            player.pvp_flagged = true;
            player.auto_pin = false;
        }
        tick(&mut old, NOW);
        let offer = tick_offer(&mut old);
        assert!(
            offer.pvp_flagged && !offer.auto_pin,
            "the offer carries both"
        );
        let mut new = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        new.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        new.take(&offer, NOW, SERVER_TIME).expect("take");
        let taken = new.player_by_number(1).expect("player");
        let player = new.world.get::<Player>(taken).expect("player");
        assert!(player.pvp_flagged, "still flagged");
        assert!(!player.auto_pin, "still no auto-pin");
        let first = new.positions().remove(0);
        assert_eq!(
            first.items, offer.items,
            "the taker's first round saves the items"
        );
        assert_eq!(first.quests, Some(offer.quests.clone()), "and the quests");
        let second = new.positions().remove(0);
        assert_eq!(
            (second.items, second.quests),
            (None, None),
            "once, until they change"
        );
    }

    #[test]
    fn the_remote_config_multiplies_experience_and_caps_the_wallet() {
        use super::encounter::Order;
        use nw_content::gameplay_config::{GameplayConfig, XpMultipliers};
        use nw_content::rewards::KillExperience;
        /// `RewardData` level 1 `KillExperience` (content bundle 25810808).
        const LEVEL_ONE_KILL: u32 = 10;
        /// Retail: 20 per tutorial kill (tutorial note, section 8).
        const RETAIL_KILL: u32 = 20;
        /// Below two quest rewards of 225 coins.
        const CAP: u64 = 300;
        let mut world = beach_world();
        let content = Arc::make_mut(&mut world.content);
        content.gameplay = GameplayConfig {
            xp: XpMultipliers {
                character: 2.0,
                progression: HashMap::new(),
            },
            wallet_cap: Some(CAP),
            ..GameplayConfig::default()
        };
        content.kill_experience = KillExperience::build([(1, i64::from(LEVEL_ONE_KILL))]);
        let wallet = |world: &SimWorld| {
            let entity = world.by_connection[&connection(1)];
            let player = world.world.get::<Player>(entity).expect("player");
            (player.coins, player.experience)
        };
        let (coins, experience) = wallet(&world);
        world.carry_out(1, vec![Order::KillExperience(1)], NOW);
        assert_eq!(wallet(&world), (coins, experience + RETAIL_KILL));
        world.fire_game_event(1, "01_skinningknife_success", NOW);
        assert_eq!(
            wallet(&world),
            ((coins + 225).min(CAP), experience + RETAIL_KILL + 2 * 275),
            "a quest reward's experience is doubled too"
        );
        world.fire_game_event(1, "01_skinningknife_success", NOW);
        assert_eq!(wallet(&world).0, CAP, "the wallet stops at its cap");
    }

    #[test]
    fn ten_thousand_kills_drop_a_bag_each_with_the_simulated_loot() {
        const KILLS: u32 = 10_000;
        const TABLE: &str = "WolfLoot";
        /// An AND entry drops when the roll reaches it: 30 % of the rolls.
        const POTION_CHANCE: f64 = 0.3;
        /// One percentage point.
        const MAX_GAP: f64 = 0.01;
        let tables = LootTables::build(LootSource {
            rows: loot_rows(TABLE, &[(gear::tests::POTION, "70000")]),
            bucket_items: Vec::new(),
        });
        let mut world = loot_world(TABLE, tables.clone());
        let entity = world.by_connection[&connection(1)];
        for _ in 0..KILLS {
            world.drop_loot(crate::mobs::mob_entity(0), 1, NOW);
            if let Some(mut player) = world.world.get_mut::<Player>(entity) {
                player.extra_members.clear();
            }
        }
        let (bags, owned) = world.bags.counts();
        assert_eq!(bags, KILLS as usize, "every kill drops a bag");
        let kill_rate = owned as f64 / f64::from(KILLS);
        // The simulator with the sim's own dice: the kills roll the same numbers.
        let mut dice = Dice::new(test_combat().seed);
        let levels = LootContext {
            enemy: Some(nw_content::loot::loot_level(TEST_LEVEL)),
            content: Some(nw_content::loot::loot_level(TEST_LEVEL)),
            player: None,
            poi: None,
            tags: &[],
            reached_limits: &[],
        };
        let simulated = (0..KILLS)
            .filter(|_| {
                !tables
                    .roll(TABLE, levels, &mut |below| dice.below(below))
                    .items
                    .is_empty()
            })
            .count();
        assert_eq!(owned, simulated, "the kills roll as the simulator does");
        assert!((kill_rate - POTION_CHANCE).abs() <= MAX_GAP, "{kill_rate}");
    }

    #[test]
    fn a_kill_rolls_its_table_at_its_types_loot_drop_chance() {
        const KILLS: u32 = 10_000;
        const TABLE: &str = "SailorLoot";
        /// The corrupted sailors' `LootDropChance` (retail: 11.1 % of 3,020 kills rolled loot).
        const DROP_CHANCE: f64 = 0.1;
        const MAX_GAP: f64 = 0.01;
        let tables = LootTables::build(LootSource {
            rows: loot_rows(TABLE, &[(gear::tests::POTION, "0")]),
            bucket_items: Vec::new(),
        });
        // The config's loot drop multiplier: 1.0 keeps the data's chance, 2.0 doubles it.
        for multiplier in [1.0, 2.0] {
            let mut world = loot_world_at(TABLE, tables.clone(), Some(DROP_CHANCE));
            world.combat.loot_drop_multiplier = multiplier;
            let entity = world.by_connection[&connection(1)];
            for _ in 0..KILLS {
                world.drop_loot(crate::mobs::mob_entity(0), 1, NOW);
                if let Some(mut player) = world.world.get_mut::<Player>(entity) {
                    player.extra_members.clear();
                }
            }
            let (bags, owned) = world.bags.counts();
            assert_eq!(bags, KILLS as usize, "every kill still drops a bag");
            let rate = owned as f64 / f64::from(KILLS);
            assert!(
                (rate - DROP_CHANCE * multiplier).abs() <= MAX_GAP,
                "multiplier {multiplier}: {rate}"
            );
        }
    }

    #[test]
    fn a_killed_mobs_bag_lists_shows_and_gives_its_items() {
        const TABLE: &str = "BoarLoot";
        const INTERACTION: u64 = 9;
        const ACCEPTED: u8 = 1;
        const REFUSED: u8 = 2;
        const TICK_MS: u64 = 33;
        let tables = LootTables::build(LootSource {
            rows: loot_rows(TABLE, &[(gear::tests::FLINT, "0")]),
            bucket_items: Vec::new(),
        });
        let mut world = loot_world(TABLE, tables);
        let who = connection(1);
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&who);
        let own_states = |world: &SimWorld, messages: &[Outgoing]| -> Vec<MemberState> {
            bundle_of(messages, &who)
                .map(|bundle| groups_in(world, &who, &bundle))
                .unwrap_or_default()
                .into_iter()
                .filter(|(number, _)| *number == 1)
                .flat_map(|(_, update)| update.members)
                .map(|member| member.state)
                .collect()
        };
        let tracked = |states: &[MemberState]| -> Vec<nw_messages::TrackedLoot> {
            states
                .iter()
                .filter_map(|state| match state {
                    MemberState::LootTracker(tracker) => tracker.loot.clone(),
                    _ => None,
                })
                .flat_map(|list| list.elements)
                .collect()
        };
        let flint_in_bag = |world: &SimWorld| -> u32 {
            world
                .world
                .get::<Player>(world.by_connection[&who])
                .and_then(|player| player.gear.as_ref())
                .map(Gear::carried)
                .unwrap_or_default()
                .iter()
                .filter(|item| item.item_id == gear::tests::FLINT)
                .map(|item| item.quantity)
                .sum()
        };

        let killed_at = spawn_at + TICK_MS;
        world.drop_loot(crate::mobs::mob_entity(0), 1, killed_at);
        let dropped = tick(&mut world, killed_at).messages;
        let entries = tracked(&own_states(&world, &dropped));
        assert_eq!(entries.len(), 1, "the tracker lists the bag");
        assert_eq!(entries[0].state, nw_messages::LOOT_ROLLED);
        let bag = entries[0].bag;
        let cards: Vec<nw_messages::GdeMetadataReplicatedState> = bundle_of(&dropped, &who)
            .map(|bundle| groups_in(&world, &who, &bundle))
            .unwrap_or_default()
            .into_iter()
            .flat_map(|(_, update)| update.members)
            .filter_map(|member| match member.state {
                MemberState::GdeMetadata(card) => Some(card),
                _ => None,
            })
            .filter(|card| card.gde_ref == Some(bag))
            .collect();
        assert_eq!(cards.len(), 1, "the bag's card goes with the kill");

        world
            .facet_message(
                &who,
                ClientFacetMsg::SetReplicateStorage {
                    header: nw_messages::FacetHeader::of(0, 1),
                    storage: bag,
                },
                killed_at + TICK_MS,
            )
            .expect("storage");
        let shown = tick(&mut world, killed_at + TICK_MS).messages;
        let storages: Vec<nw_messages::StoredItems> = own_states(&world, &shown)
            .into_iter()
            .filter_map(|state| match state {
                MemberState::GlobalStorage(storage) => storage.storages,
                _ => None,
            })
            .flat_map(|list| list.elements)
            .collect();
        assert_eq!(storages.len(), 1);
        assert_eq!(storages[0].storage, bag);
        let flint_index = gear::tests::kits()
            .item(gear::tests::FLINT)
            .expect("flint")
            .item_index;
        assert_eq!(
            storages[0]
                .items
                .iter()
                .map(|item| (item.item_index, item.quantity))
                .collect::<Vec<_>>(),
            [(flint_index, 1)]
        );

        let request = InteractionRequest {
            target: bag,
            target_entity: statics::facet_of(&bag[..8].try_into().expect("prefix")),
            interaction: INTERACTION,
            kind: LOOT_KIND,
            gatherable: None,
        };
        let opened_at = killed_at + 2 * TICK_MS;
        world.interact(&who, request, b"@ui_loot", opened_at);
        let taken = tick(&mut world, opened_at).messages;
        let answers: Vec<(u8, Vec<u8>)> = messages_to(&taken, 1)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::InteractionValidated(answer) => Some((answer.status, answer.name)),
                _ => None,
            })
            .collect();
        assert_eq!(answers, [(ACCEPTED, b"@inv_DroppedItems".to_vec())]);
        assert_eq!(flint_in_bag(&world), 1, "the item went into the bag");
        let states = own_states(&world, &taken);
        assert_eq!(
            tracked(&states)
                .iter()
                .map(|loot| loot.state)
                .collect::<Vec<_>>(),
            [nw_messages::LOOT_TAKEN]
        );
        assert!(states.iter().any(|state| matches!(
            state,
            MemberState::GlobalStorage(storage)
                if storage.storages.as_ref().is_some_and(|list| list.elements.is_empty())
        )));

        world.interact(
            &who,
            InteractionRequest {
                interaction: INTERACTION + 1,
                ..request
            },
            &[],
            opened_at + TICK_MS,
        );
        let again = tick(&mut world, opened_at + TICK_MS).messages;
        assert!(
            messages_to(&again, 1).iter().any(|message| matches!(
                message,
                ServerMessage::InteractionValidated(answer) if answer.status == REFUSED
            )),
            "a taken bag is refused"
        );
        assert_eq!(flint_in_bag(&world), 1, "taken once");
    }

    const QUEST: &str = "Q_FetchFlint";
    const REPEATABLE_QUEST: &str = "Q_Daily";
    /// The timer of the test quests' one task, in seconds.
    const QUEST_SECONDS: f64 = 1.0;
    const QUEST_TIMER_MS: u64 = 1_000;
    /// Side quests (`Type` `Journey`).
    const SIDE_QUEST: &str = "Q_Truffles";
    const OTHER_SIDE_QUEST: &str = "Q_Honey";
    const JOURNEY: &str = "Journey";
    /// A quest whose only task is a storage interact (the vault step of `12A_MSQ_03`).
    const VAULT_QUEST: &str = "Q_Vault";
    /// A quest whose one step tracks [`QUEST`] (`TaskObjective`, as the weapon quest chains).
    const TRACKER_QUEST: &str = "Q_Tracker";
    const STORAGE_TAG: &str = "SettlementStorage";
    /// The retail Monarch's Bluffs settlement capital and the facet entity its storage
    /// interaction names (vault capture 2026-09-27).
    const SETTLEMENT_GUID: &str = "f7763200-249e-87c4-1784-526151dd3c56";
    const SETTLEMENT_FACET: u64 = 0xE0F2_6061_7543_BB93;
    const STORAGE_INTERACTION: u64 = 0x4007_a812_6cc6_f492;
    /// A town board mission (retail 3819 of `CookingProjectId_1_FishingSalmonLargeT1_1`) on the
    /// template `CM_procurer_acquire_X_items`, here with flint.
    const MISSION: &str = "CookingProjectId_1_FishingSalmonLargeT1_1";
    const MISSION_CRC: u32 = 0x469e_c58c;
    const MISSION_TEMPLATE: &str = "CM_procurer_acquire_X_items";
    const MISSION_TERRITORY: u16 = 16;
    const MISSION_FLINT: u32 = 3;
    /// The retail town board's raw id (board capture 2026-06-13).
    const BOARD: [u8; nw_messages::wire::RAW_ID_LEN] =
        [75, 101, 126, 26, 104, 224, 108, 159, 0, 0, 0, 0, 0, 0, 0, 0];

    /// A world with the test kits, two one-timer quests (one repeatable, both giving flint) and
    /// one player wearing its kit.
    fn quest_world() -> SimWorld {
        quest_world_with(nw_content::gameplay_config::GameplayConfig::default())
    }

    /// The quest world with the remote config keys `gameplay`.
    fn quest_world_with(gameplay: nw_content::gameplay_config::GameplayConfig) -> SimWorld {
        quest_world_of(gameplay, nw_content::interact_tags::InteractTags::default())
    }

    /// The quest world with the remote config keys `gameplay` and the level objects' interact
    /// tags `interact_tags`.
    fn quest_world_of(
        gameplay: nw_content::gameplay_config::GameplayConfig,
        interact_tags: nw_content::interact_tags::InteractTags,
    ) -> SimWorld {
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook};
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            duration: (kind == "TaskTimer").then_some(QUEST_SECONDS),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let objective = |id: &str, root: &str, repeatable: bool| ObjectiveRow {
            id: id.to_owned(),
            root: root.to_owned(),
            reward_item: Some(gear::tests::FLINT.to_owned()),
            reward_quantity: Some(2),
            repeatable,
            ..ObjectiveRow::default()
        };
        let side = |id: &str| ObjectiveRow {
            kind: JOURNEY.to_owned(),
            ..objective(id, "q_root", false)
        };
        let quests = QuestBook::build(
            [
                objective(QUEST, "q_root", false),
                objective(REPEATABLE_QUEST, "q_root", true),
                side(SIDE_QUEST),
                side(OTHER_SIDE_QUEST),
                objective(VAULT_QUEST, "q_vault_root", false),
                objective(TRACKER_QUEST, "q_track_root", false),
                ObjectiveRow {
                    id: MISSION_TEMPLATE.to_owned(),
                    root: "m_root".to_owned(),
                    kind: "Mission".to_owned(),
                    ..ObjectiveRow::default()
                },
            ],
            TaskBook::new(vec![
                task("q_root", "ConsecutiveTaskContainer", &["q_wait"]),
                task("q_wait", "TaskTimer", &[]),
                task("q_vault_root", "ConsecutiveTaskContainer", &["q_vault"]),
                task("q_track_root", "ConsecutiveTaskContainer", &["q_track"]),
                TaskRow {
                    item_name: Some(QUEST.to_owned()),
                    ..task("q_track", "TaskObjective", &[])
                },
                TaskRow {
                    interact_tag: Some(STORAGE_TAG.to_owned()),
                    ..task("q_vault", "TaskInteract", &[])
                },
                task(
                    "m_root",
                    "ConsecutiveTaskContainer",
                    &["m_have", "m_return"],
                ),
                TaskRow {
                    target_qty: Some(1),
                    ..task("m_have", "TaskHaveAndReturnItems", &[])
                },
                TaskRow {
                    interact_tag: Some("ReturnToGiver".to_owned()),
                    ..task("m_return", "TaskInteract", &[])
                },
            ]),
        )
        .with_missions([crate::player_content::MissionRow {
            id: MISSION.to_owned(),
            objective: MISSION_TEMPLATE.to_owned(),
            territory: MISSION_TERRITORY,
            items: Some(gear::tests::FLINT.to_owned()),
            items_count: Some(i64::from(MISSION_FLINT)),
            ..crate::player_content::MissionRow::default()
        }]);
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                quests,
                gameplay,
                interact_tags,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        tick(&mut world, NOW);
        world
    }

    fn flint_of(world: &mut SimWorld) -> u32 {
        world
            .departures()
            .pop()
            .and_then(|departure| departure.items)
            .unwrap_or_default()
            .iter()
            .filter(|stack| stack.item_id == gear::tests::FLINT)
            .map(|stack| stack.quantity)
            .sum()
    }

    #[test]
    fn a_quest_object_s_detection_sphere_sets_off_its_event_for_a_player_on_the_quest() {
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook, QuestObjects};
        use nw_content::gathering::DetectionSpheres;
        use nw_formats::objectstream::lowercase_crc32;
        use nw_world::records::GatherablePoint;
        /// Retail `9401_befriendhorse`: the torch, its game event and its sphere (build 25810808).
        const TORCH: &str = "94_BeastTorch";
        const TORCH_EVENT: &str = "94_BeastTorch_interact";
        const TORCH_RADIUS_M: f32 = 9.0;
        const TORCH_QUEST: &str = "q_torch";
        /// The torch stands 20 m east of the player's admit place.
        const TORCH_X: f32 = CELL_M / 2.0 + 20.0;
        /// 8.9 m from the torch, as the retail player at 133.69 s.
        const INSIDE_X: f32 = TORCH_X - 8.9;
        const TICK_MS: u64 = 33;
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let quests = QuestBook::build(
            [ObjectiveRow {
                id: TORCH_QUEST.to_owned(),
                root: "t_root".to_owned(),
                ..ObjectiveRow::default()
            }],
            TaskBook::new(vec![
                task("t_root", "ConsecutiveTaskContainer", &["t_torch"]),
                TaskRow {
                    kill_enemy_type: Some("94_BeastTorch_Interact".to_owned()),
                    target_qty: Some(1),
                    ..task("t_torch", "TaskGameEvent", &[])
                },
            ]),
        );
        let gatherables = Gatherables::build([Gatherable {
            id: TORCH.to_owned(),
            gather_ms: 4_000,
            respawn_ms: (10_000, 15_000),
            loot_table: None,
            loot_container: true,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: Some(TORCH_EVENT.to_owned()),
        }]);
        let quest_objects = QuestObjects::build(
            &[GatherablePoint {
                gatherable_id: lowercase_crc32(TORCH),
                position: [TORCH_X, 10.0, 50.0],
                ..GatherablePoint::default()
            }],
            &gatherables,
            &DetectionSpheres::build([(TORCH.to_owned(), TORCH_RADIUS_M)]),
            &HashSet::from([TORCH_EVENT.to_lowercase()]),
            &[],
        );
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                quests,
                gatherables,
                quest_objects,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        tick(&mut world, NOW);
        world.start_quest(1, TORCH_QUEST, NOW).expect("start");
        tick(&mut world, NOW + TICK_MS);
        let torch_done = |world: &SimWorld| world.quests_of_player(1)[0].run.is_done("t_torch");
        assert!(!torch_done(&world), "20 m away: outside the sphere");
        let entity = world.by_connection[&connection(1)];
        let inside = Position {
            x: INSIDE_X,
            y: 10.0,
            z: 50.0,
        };
        world.place_player(entity, inside, None, Placement::Stand, NOW + TICK_MS);
        tick(&mut world, NOW + 2 * TICK_MS);
        assert!(
            torch_done(&world),
            "the way into the sphere set off the torch's event"
        );
    }

    #[test]
    fn a_quest_s_success_event_counts_for_another_quest() {
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook};
        /// Retail `09A_S5_SavePrivateer` waits for the success event of the quest before it.
        const SUCCESS: &str = "09A_S5_Success";
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let quests = QuestBook::build(
            [
                ObjectiveRow {
                    id: "q_first".to_owned(),
                    root: "t_first".to_owned(),
                    success_event: Some(SUCCESS.to_owned()),
                    ..ObjectiveRow::default()
                },
                ObjectiveRow {
                    id: "q_after".to_owned(),
                    root: "t_after".to_owned(),
                    ..ObjectiveRow::default()
                },
            ],
            TaskBook::new(vec![
                task("t_first", "ConsecutiveTaskContainer", &[]),
                task("t_after", "ConsecutiveTaskContainer", &["t_wait"]),
                TaskRow {
                    kill_enemy_type: Some(SUCCESS.to_owned()),
                    target_qty: Some(1),
                    ..task("t_wait", "TaskGameEvent", &[])
                },
            ]),
        );
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            test_combat(),
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                quests,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        tick(&mut world, NOW);
        world.start_quest(1, "q_after", NOW).expect("start");
        world.start_quest(1, "q_first", NOW).expect("start");
        world.complete_quest(1, "q_first");
        let after = world
            .quests_of_player(1)
            .iter()
            .find(|quest| quest.def.id == "q_after")
            .expect("the quest after");
        assert!(after.run.is_done("t_wait"), "the success event counted");
    }

    #[test]
    fn a_food_s_status_effect_fires_its_game_event_for_the_quest() {
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook};
        use nw_content::item_rules::Consumable;
        use nw_content::status_effects::{EffectSourceRow, StatusEffectRules, StatusEffectSource};
        use nw_formats::objectstream::lowercase_crc32;
        /// Retail mount food (build 25810808): `MountConsum_GruelT0` adds this effect, whose
        /// `GameEventID` the step `9401_TutorialFood` counts.
        const FOOD_EFFECT: &str = "Status_Mount_GrantRidingXP_Small";
        const FOOD_EVENT: &str = "Mount_GrantRidingXP_Small";
        const FOOD_QUEST: &str = "q_feed";
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let quests = QuestBook::build(
            [ObjectiveRow {
                id: FOOD_QUEST.to_owned(),
                root: "t_root".to_owned(),
                ..ObjectiveRow::default()
            }],
            TaskBook::new(vec![
                task("t_root", "ConsecutiveTaskContainer", &["t_feed"]),
                TaskRow {
                    kill_enemy_type: Some(FOOD_EVENT.to_owned()),
                    target_qty: Some(1),
                    ..task("t_feed", "TaskGameEvent", &[])
                },
            ]),
        );
        let status_effects = StatusEffectRules::build(StatusEffectSource {
            effects: vec![EffectSourceRow {
                status_id: FOOD_EFFECT.to_owned(),
                game_event: Some(FOOD_EVENT.to_owned()),
                ..EffectSourceRow::default()
            }],
            ..StatusEffectSource::default()
        });
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            test_combat(),
            Arc::new(PlayerContent {
                kits: gear::tests::kits(),
                quests,
                status_effects,
                ..PlayerContent::default()
            }),
        );
        world.set_table(&table(&["sim-a"])).expect("table");
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        tick(&mut world, NOW);
        world.start_quest(1, FOOD_QUEST, NOW).expect("start");
        let entity = world.by_connection[&connection(1)];
        let food = Consumable {
            id: "MountConsum_GruelT0".to_owned(),
            cooldown: None,
            effects: Vec::new(),
            disabled_in_combat: false,
            status_effects: vec![lowercase_crc32(FOOD_EFFECT)],
            potency: 1.0,
            duration_override_ms: None,
            use_tag: None,
            mount_only: false,
            required_inactive_limit: None,
            remove_categories: Vec::new(),
        };
        world.add_consumable_effects(entity, &food, NOW);
        assert!(
            world.quests_of_player(1)[0].run.is_done("t_feed"),
            "the food's effect fired its game event"
        );
    }

    #[test]
    fn a_quest_starts_runs_out_gives_its_reward_and_is_done() {
        let mut world = quest_world();
        assert_eq!(
            world.start_quest(1, "NoSuchQuest", NOW),
            Err(QuestRefusal::NoQuest)
        );
        assert_eq!(world.start_quest(1, "q_fetchflint", NOW), Ok(()));
        assert_eq!(
            world.start_quest(1, QUEST, NOW),
            Err(QuestRefusal::Active),
            "not twice at once"
        );
        let saved = world.positions()[0]
            .quests
            .clone()
            .expect("a start is saved");
        assert_eq!(saved.active.first().map(|(id, _)| id.as_str()), Some(QUEST));
        assert_eq!(world.positions()[0].quests, None, "taken once");
        world.tick_quests(NOW + QUEST_TIMER_MS);
        assert_eq!(flint_of(&mut world), 2, "the reward is in the bag");
        let done = world.positions()[0]
            .quests
            .clone()
            .expect("the end is saved");
        assert!(done.active.is_empty());
        assert_eq!(done.done, [QUEST]);
        assert_eq!(world.start_quest(1, QUEST, NOW), Err(QuestRefusal::Done));
        assert_eq!(world.start_quest(1, REPEATABLE_QUEST, NOW), Ok(()));
    }

    #[test]
    fn an_objective_task_starts_its_quest_and_ends_with_it() {
        let mut world = quest_world();
        world.start_quest(1, TRACKER_QUEST, NOW).expect("tracker");
        let saved = world.positions()[0].quests.clone().expect("saved");
        let running: Vec<&str> = saved.active.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            running,
            [TRACKER_QUEST, QUEST],
            "the step started its quest"
        );
        world.tick_quests(NOW + QUEST_TIMER_MS);
        world.tick_quests(NOW + QUEST_TIMER_MS + 1);
        let done = world.positions()[0].quests.clone().expect("the ends");
        assert!(done.active.is_empty(), "both ended: {:?}", done.active);
        assert_eq!(done.done, [QUEST, TRACKER_QUEST]);
    }

    #[test]
    fn an_objective_task_of_a_quest_done_before_is_done_at_once() {
        let mut world = quest_world();
        world.start_quest(1, QUEST, NOW).expect("main");
        world.tick_quests(NOW + QUEST_TIMER_MS);
        world
            .start_quest(1, TRACKER_QUEST, NOW + QUEST_TIMER_MS)
            .expect("tracker");
        world.tick_quests(NOW + QUEST_TIMER_MS + 1);
        let done = world.positions()[0].quests.clone().expect("the ends");
        assert!(done.active.is_empty(), "{:?}", done.active);
        assert_eq!(done.done, [QUEST, TRACKER_QUEST]);
    }

    #[test]
    fn a_player_runs_many_quests_and_each_ends_on_its_own() {
        let mut world = quest_world();
        world.start_quest(1, QUEST, NOW).expect("main");
        world.accept_quest(1, SIDE_QUEST, None, NOW).expect("side");
        world
            .accept_quest(1, OTHER_SIDE_QUEST, None, NOW)
            .expect("other side");
        let saved = world.positions()[0].quests.clone().expect("saved");
        let running: Vec<&str> = saved.active.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(running, [QUEST, SIDE_QUEST, OTHER_SIDE_QUEST]);
        world.tick_quests(NOW + QUEST_TIMER_MS);
        assert_eq!(flint_of(&mut world), 6, "three rewards");
        let done = world.positions()[0].quests.clone().expect("the ends");
        assert!(done.active.is_empty());
        assert_eq!(done.done.len(), 3);
    }

    #[test]
    fn a_full_journal_refuses_an_accept_but_never_a_chain_start() {
        let layer = serde_json::json!({"javelin": {"objectives-max-journey": 1}});
        let stack =
            nw_content::gameplay_config::KeyStack::merge([layer.as_object().expect("object")]);
        let mut world = quest_world_with(
            nw_content::gameplay_config::GameplayConfig::from_stack(&stack).expect("keys"),
        );
        world
            .accept_quest(1, SIDE_QUEST, None, NOW)
            .expect("the first side quest");
        assert_eq!(
            world.accept_quest(1, OTHER_SIDE_QUEST, None, NOW),
            Err(QuestRefusal::Full),
            "a second Journey is over the limit of 1"
        );
        assert_eq!(
            world.accept_quest(1, QUEST, None, NOW),
            Ok(()),
            "a quest of another kind has its own limit"
        );
        assert_eq!(
            world.start_quest(1, OTHER_SIDE_QUEST, NOW),
            Ok(()),
            "the next step of a chain starts even with a full journal"
        );
    }

    #[test]
    fn an_inn_check_in_on_the_open_s_interaction_id_is_answered_once() {
        use travel::kind::{OPEN_INN, SET_SPAWN_POINT};
        /// Retail `12a_msq_02_inntutorial`: the inn's open and its check in carry one id.
        const INN_INTERACTION: u64 = 16_624_921_653_968_059_714;
        const INN: [u8; nw_messages::wire::RAW_ID_LEN] = [0x23; nw_messages::wire::RAW_ID_LEN];
        let mut world = world();
        let who = connection(1);
        world
            .admit(&admit_at(1, CELL_M / 2.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let spawn_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, spawn_at);
        world.heard_from(&who);
        let request = |kind| InteractionRequest {
            target: INN,
            target_entity: nw_messages::facet_of_raw_id(&INN),
            interaction: INN_INTERACTION,
            kind,
            gatherable: None,
        };
        for kind in [OPEN_INN, OPEN_INN, SET_SPAWN_POINT, SET_SPAWN_POINT] {
            world.interact(&who, request(kind), b"@inn", spawn_at);
        }
        let answered: Vec<u8> = messages_to(&tick(&mut world, spawn_at + 1).messages, 1)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::InteractionValidated(answer) => Some(answer.kind),
                _ => None,
            })
            .collect();
        assert_eq!(answered, [OPEN_INN, SET_SPAWN_POINT]);
    }

    #[test]
    fn a_storage_interact_ends_the_vault_step_when_the_interaction_ends() {
        let storage = [STORAGE_TAG.to_owned()];
        let tags = nw_content::interact_tags::InteractTags::build(
            &HashMap::new(),
            [(SETTLEMENT_GUID, None, storage.as_slice())],
            [],
        );
        let mut world =
            quest_world_of(nw_content::gameplay_config::GameplayConfig::default(), tags);
        world.start_quest(1, VAULT_QUEST, NOW).expect("start");
        let who = connection(1);
        let request = InteractionRequest {
            target: [0; nw_messages::wire::RAW_ID_LEN],
            target_entity: SETTLEMENT_FACET,
            interaction: STORAGE_INTERACTION,
            kind: item_calls::OPEN_STORAGE_KIND,
            gatherable: None,
        };
        world.interact(&who, request, b"@ui_global_storage", NOW);
        assert_eq!(
            world.quests_of_player(1).len(),
            1,
            "the step waits for the interaction's end"
        );
        let cancel = ClientFacetMsg::CancelInteraction {
            header: nw_messages::FacetHeader::of(0, 1),
            target: [0; nw_messages::wire::RAW_ID_LEN],
            target_entity: SETTLEMENT_FACET,
            interaction: STORAGE_INTERACTION,
        };
        world.facet_message(&who, cancel, NOW).expect("cancel");
        assert!(
            world.quests_of_player(1).is_empty(),
            "the vault step ended the quest"
        );
        assert_eq!(flint_of(&mut world), 2, "its reward");
    }

    #[test]
    fn a_town_board_mission_runs_its_template_and_ends_at_its_turn_in() {
        use nw_messages::{FacetHeader, ObjectiveInteractorClientMsg};
        let mut world = quest_world();
        let who = connection(1);
        let board = InteractionRequest {
            target: BOARD,
            target_entity: nw_messages::facet_of_raw_id(&BOARD),
            interaction: STORAGE_INTERACTION,
            kind: nw_messages::client_input::TOWN_BOARD_KIND,
            gatherable: None,
        };
        world.interact(&who, board, b"@ui_territory_board_town_project", NOW);
        let take = ObjectiveInteractorClientMsg::SelectCommunityGoal {
            header: FacetHeader::of(0, 1),
            mission: MISSION_CRC,
            project: 0,
        };
        world.board_call(&who, take, NOW);
        let running: Vec<&str> = world
            .quests_of_player(1)
            .iter()
            .map(|quest| quest.def.id.as_str())
            .collect();
        assert_eq!(running, [MISSION], "the mission runs under its own id");
        let entity = world.player_by_number(1).expect("player");
        let record = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.quest.as_ref().map(QuestTracker::first_state))
            .and_then(|state| state.active)
            .and_then(|active| active.elements.first().cloned())
            .expect("the record");
        assert_eq!(
            record.objective,
            nw_formats::objectstream::lowercase_crc32(MISSION_TEMPLATE)
        );
        assert_eq!(record.mission, MISSION_CRC);
        assert_eq!(record.destination, BOARD);
        assert_eq!(record.origin_territory, MISSION_TERRITORY);
        assert!(record.accept_time > 0);
        world.give_loot(1, vec![(gear::tests::FLINT.to_owned(), MISSION_FLINT)], NOW);
        world.tick_quests(NOW);
        let instance = world
            .world
            .get::<Player>(entity)
            .and_then(|player| {
                player
                    .quest
                    .as_ref()
                    .map(|journal| journal.instance(MISSION))
            })
            .expect("instance");
        let turn_in = ObjectiveInteractorClientMsg::CompleteObjective {
            header: FacetHeader::of(0, 1),
            instance,
        };
        world.board_call(&who, turn_in, NOW);
        assert!(world.quests_of_player(1).is_empty(), "the turn-in ends it");
        assert_eq!(flint_of(&mut world), 0, "its items go back");
    }

    #[test]
    fn a_repeated_quest_is_not_active_and_done_at_once() {
        let mut world = quest_world();
        world.start_quest(1, REPEATABLE_QUEST, NOW).expect("start");
        world.tick_quests(NOW + QUEST_TIMER_MS);
        world.start_quest(1, REPEATABLE_QUEST, NOW).expect("again");
        let saved = world
            .departures()
            .pop()
            .and_then(|departure| departure.quests)
            .expect("quests");
        assert_eq!(
            saved.active.first().map(|(id, _)| id.as_str()),
            Some(REPEATABLE_QUEST)
        );
        assert!(
            !saved.done.iter().any(|id| id == REPEATABLE_QUEST),
            "the database keeps one row per objective"
        );
    }

    #[test]
    fn a_saved_quest_resumes_at_login_and_a_changed_tree_drops_it() {
        let mut world = quest_world();
        world.start_quest(1, QUEST, NOW).expect("start");
        let saved = world
            .departures()
            .pop()
            .and_then(|departure| departure.quests);
        let saved = saved.expect("quests");
        world.leave(&connection(1)).expect("left");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        admit.quests = saved.clone();
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let resumed = world
            .departures()
            .pop()
            .and_then(|departure| departure.quests)
            .and_then(|quests| quests.active.into_iter().next())
            .expect("the run goes on");
        let (id, run) = saved.active.first().cloned().expect("active");
        assert_eq!((resumed.0, resumed.1.tasks), (id, run.tasks));
        world.leave(&connection(1)).expect("left");
        let (_, run) = saved.active.first().cloned().expect("active");
        admit.quests = PlayerQuests {
            active: vec![(
                QUEST.to_owned(),
                QuestRun {
                    tasks: Vec::new(),
                    ..run
                },
            )],
            ..PlayerQuests::default()
        };
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let dropped = world
            .departures()
            .pop()
            .and_then(|departure| departure.quests);
        assert!(dropped.is_none_or(|quests| quests.active.is_empty()));
    }

    #[test]
    fn a_new_quest_lists_above_the_tracker_versions_of_the_sim_before() {
        /// The tracker version the tutorial sim left (live run 10: 0x2c).
        const TUTORIAL_TRACKER_VERSION: u64 = 0x2c;
        let mut world = quest_world();
        world.leave(&connection(1)).expect("left");
        let mut admit = admit_at(1, CELL_M / 2.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        admit.quests = PlayerQuests {
            tracker_version: TUTORIAL_TRACKER_VERSION,
            ..PlayerQuests::default()
        };
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let handed = world
            .departures()
            .pop()
            .and_then(|departure| departure.quests)
            .expect("quests");
        assert_eq!(
            handed.tracker_version, TUTORIAL_TRACKER_VERSION,
            "a handoff with no quest keeps the version"
        );
        world.start_quest(1, QUEST, NOW).expect("start");
        let shown: Vec<u64> = world
            .world
            .query::<&Player>()
            .iter(&world.world)
            .filter_map(|player| player.quest.as_ref().map(QuestTracker::version))
            .collect();
        assert!(
            shown
                .iter()
                .all(|version| *version > TUTORIAL_TRACKER_VERSION)
                && !shown.is_empty(),
            "the client drops lists below the ones it has: {shown:?}"
        );
    }

    #[test]
    fn a_taken_player_keeps_its_look() {
        let (mut old, _, _) = border_world();
        tick(&mut old, NOW);
        let mut offer = tick_offer(&mut old);
        offer.look = Some(SAVED_LOOK);
        let mut new = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        new.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        new.take(&offer, NOW, SERVER_TIME).expect("take");
        let looks: Vec<Option<PlayerLook>> = new
            .world
            .query::<&Player>()
            .iter(&new.world)
            .map(|player| player.look)
            .collect();
        assert_eq!(looks, vec![Some(PlayerLook::of_choices(SAVED_LOOK))]);
    }

    #[test]
    fn without_the_card_knob_the_own_player_is_its_position_every_tick() {
        let mut world = world_of(
            "sim-a",
            WorldSettings {
                local_player_card: false,
                ..settings(MANY, MAX_PLAYERS)
            },
        );
        world
            .admit(&admit_at(1, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        for step in 0..2 {
            let messages = tick(&mut world, NOW + 33 * step).messages;
            let bundle = bundle_of(&messages, &connection(1)).expect("bundle");
            let own: Vec<bool> = SlotRead::of(&bundle)
                .expect("read")
                .updates
                .into_iter()
                .filter(SlotUpdate::is_local_player)
                .map(|update| update.card.is_some())
                .collect();
            assert_eq!(own, vec![false], "no own card, the position every tick");
        }
    }

    #[test]
    fn a_lost_card_comes_again_within_a_second() {
        let x = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(x)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 5.0);
        let mob = crate::mobs::mob_entity(0);
        tick(&mut world, NOW);
        let cards = (1..=CARD_REFRESH_BUNDLES)
            .filter(|step| {
                next_classes_of(&mut world, NOW + 33 * step, &connection(1), mob)
                    .contains(&GDE_METADATA_TYPE_INDEX)
            })
            .count();
        assert_eq!(cards, 1, "once more in the next second of bundles");
    }

    #[test]
    fn a_dead_mob_leaves_the_bundles_and_returns_with_a_new_card() {
        const LETHAL: u32 = 100_000;
        const MOB_RESPAWN_MS: u64 = crate::config::DEFAULT_MOB_RESPAWN_MS;
        let x = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(x)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 50.0);
        let mob = crate::mobs::mob_entity(0);
        tick(&mut world, NOW);
        world.apply_hit(mob, 1, LETHAL, 0, NOW);
        assert!(next_classes_of(&mut world, NOW + 33, &connection(1), mob).is_empty());
        let back = tick(&mut world, NOW + MOB_RESPAWN_MS).messages;
        assert_eq!(classes_of(&world, &back, &connection(1), mob), CARD_CLASSES);
    }

    #[test]
    fn a_mob_that_returns_to_view_gets_a_new_spawn_card() {
        let x = CELL_M / 2.0;
        let mut combat = test_combat();
        combat.spawns = vec![test_spawn(x)].into();
        let mut world = SimWorld::new(
            settings(1, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(NoGround),
            slot("sim-a"),
            combat,
            Arc::default(),
        );
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, x + 5.0);
        join(&mut world, 2, x + 4.0);
        let mob = crate::mobs::mob_entity(0);
        assert!(
            next_classes_of(&mut world, NOW, &connection(1), mob).is_empty(),
            "the nearer player takes the one place"
        );
        world.leave(&connection(2));
        assert_eq!(
            next_classes_of(&mut world, NOW + 33, &connection(1), mob),
            CARD_CLASSES
        );
        join(&mut world, 2, x + 4.0);
        assert!(next_classes_of(&mut world, NOW + 66, &connection(1), mob).is_empty());
        world.leave(&connection(2));
        assert_eq!(
            next_classes_of(&mut world, NOW + 99, &connection(1), mob),
            CARD_CLASSES,
            "a new card after it was out of view"
        );
    }

    #[test]
    fn a_ghost_mob_is_in_the_bundles() {
        let mut world = world_with_spawns(Vec::new());
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 5.0);
        let mob = crate::mobs::mob_entity(3);
        let ghost = Ghost {
            entity: mob,
            position: Point {
                x: BORDER_X + 5.0,
                y: 10.0,
                z: 50.0,
            },
            mob: Some(MobGhost {
                health: 900,
                max_health: 1000,
                returning: false,
                vitals_id: 1,
                category_id: 2,
                level: 3,
            }),
            player: None,
        };
        world.set_ghosts(&slot("sim-b").id, vec![ghost], NOW);
        assert_eq!(
            next_classes_of(&mut world, NOW, &connection(1), mob),
            CARD_CLASSES
        );
    }

    #[test]
    fn a_player_wakes_the_mobs_near_it() {
        let x = CELL_M / 2.0;
        let mut world = world_with_spawns(vec![test_spawn(x)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        world.tick(NOW, SERVER_TIME).expect("tick");
        assert_eq!(world.mob_counts()[0], (MobState::Asleep, 1));
        join(&mut world, 1, x + 5.0);
        world.tick(NOW + 33, SERVER_TIME).expect("tick");
        assert_eq!(world.mob_counts()[0], (MobState::Asleep, 0));
    }

    /// The slot values of a retail record that holds no key, and of one that holds forward
    /// (clean running capture).
    const STANDING_SLOTS: [u64; 10] = [
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0x0402_0000_0800_0000,
        0x0200_0000_0000_0000,
        0,
    ];
    const FORWARD_SLOTS: [u64; 10] = [
        0x0800_0000_4000,
        0,
        0,
        0,
        0,
        0,
        0x1_0000_0000,
        0x0402_0000_0800_0000,
        0x0200_0000_4000_0000,
        0,
    ];
    /// The boom of the test clients' camera.
    const TEST_BOOM_M: f32 = 4.0;

    /// Input record `counter` holding `slots`, of a client whose camera looks along `view` from
    /// [`TEST_BOOM_M`] behind its body at `body`, which moves at `velocity`.
    fn camera_record(
        counter: u16,
        body: Position,
        view: LookDir,
        velocity: [f32; 2],
        slots: [u64; 10],
    ) -> InputRecord {
        use nw_sim_core::camera_check::{CAMERA_LAG_S, CAMERA_RISE_M, SHOULDER_RIGHT_M};
        let (sin, cos) = view.yaw.sin_cos();
        let level = TEST_BOOM_M * view.pitch.cos();
        let camera = InputPosition::from_metres(
            body.x + level * sin + SHOULDER_RIGHT_M * cos - CAMERA_LAG_S * velocity[0],
            body.y - level * cos + SHOULDER_RIGHT_M * sin - CAMERA_LAG_S * velocity[1],
            body.z + CAMERA_RISE_M + TEST_BOOM_M * view.pitch.sin(),
        )
        .expect("finite");
        InputRecord {
            u16_after_mask: counter,
            bit_5: Some(f32::from(attack::INPUT_RECORD_MS) / 1_000.0),
            bit_6: Some([view.yaw, view.pitch]),
            bit_7: Some(camera),
            slots: nw_messages::client_input::InputSlots {
                count: 19,
                values: slots.to_vec(),
                bytes: [128; 4],
            },
            ..InputRecord::default()
        }
    }

    #[test]
    fn records_that_arrive_together_each_move_the_body_by_their_keys() {
        // A client runs east (yaw -90°), two records per input, newest first, as live: the
        // server's body follows the client's own run record by record.
        const EAST: LookDir = LookDir {
            yaw: -std::f32::consts::FRAC_PI_2,
            pitch: 0.2,
        };
        const RECORDS: u16 = 16;
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, CELL_M / 2.0);
        let entity = world.by_connection[&a];
        let start = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let mut client = nw_sim_core::Character::new(start);
        let mut sent = Vec::new();
        let mut last = start;
        for counter in 1..=RECORDS {
            client.step(
                nw_sim_core::CharacterInput {
                    intent: nw_sim_core::Intent::of_keys(true, false, false, false),
                    yaw: EAST.yaw,
                    dt_s: f32::from(attack::INPUT_RECORD_MS) / 1_000.0,
                },
                &NoGround,
                OCEAN_LEVEL_M,
            );
            sent.insert(
                0,
                camera_record(
                    counter,
                    client.position(),
                    EAST,
                    client.velocity(),
                    FORWARD_SLOTS,
                ),
            );
            if counter % 2 == 1 {
                continue;
            }
            let arrived =
                NOW + ONE_SECOND_MS + u64::from(counter) * u64::from(attack::INPUT_RECORD_MS);
            let (result, body) = world
                .move_from_input(&a, &sent[..2], arrived)
                .expect("new records");
            assert!(result.is_ok(), "record {counter}: {result:?}");
            assert!(
                body.horizontal_distance(client.position()) < 0.05,
                "record {counter}: {body:?} vs {:?}",
                client.position()
            );
            last = body;
        }
        assert!(last.x > start.x + 1.0, "it ran east: {last:?}");
        // An old copy moves nothing.
        assert_eq!(
            world.move_from_input(&a, &sent[2..4], NOW + 2 * ONE_SECOND_MS),
            None
        );
    }

    #[test]
    fn a_running_player_gets_its_place_in_every_own_state_and_a_still_one_none() {
        // Live 2026-09-18: with no own place while running, Thorpe's intro never started (no
        // 6703); retail sends the place in every state of a run (clean running capture).
        const NORTH: LookDir = LookDir {
            yaw: 0.0,
            pitch: 0.2,
        };
        const RUN_STATES: u16 = 20;
        const STILL_STATES: u16 = 30;
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, CELL_M / 2.0);
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        world.heard_from(&a);
        let entity = world.by_connection[&a];
        let start = world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position();
        let mut client = nw_sim_core::Character::new(start);
        let has_place = |states: &[MemberState]| {
            alc_children(states)
                .iter()
                .any(|(child, _)| *child == POS_REL_CHILD)
        };
        let mut placed = Vec::new();
        for counter in 1..=RUN_STATES + STILL_STATES {
            let run = counter <= RUN_STATES;
            client.step(
                nw_sim_core::CharacterInput {
                    intent: nw_sim_core::Intent::of_keys(run, false, false, false),
                    yaw: NORTH.yaw,
                    dt_s: f32::from(attack::INPUT_RECORD_MS) / 1_000.0,
                },
                &NoGround,
                OCEAN_LEVEL_M,
            );
            let slots = if run { FORWARD_SLOTS } else { STANDING_SLOTS };
            let now = card_at + u64::from(counter) * u64::from(attack::INPUT_RECORD_MS);
            let record = camera_record(counter, client.position(), NORTH, client.velocity(), slots);
            let _ = world.move_from_input(&a, &[record], now);
            let outgoing = tick(&mut world, now);
            placed.push(has_place(&states_of(&world, &outgoing.messages, &a, 1)));
        }
        assert!(
            placed[..usize::from(RUN_STATES)]
                .iter()
                .all(|placed| *placed),
            "every state of the run: {placed:?}"
        );
        assert!(
            placed[placed.len() - 10..].iter().all(|placed| !placed),
            "none once it stands: {placed:?}"
        );
    }

    /// The `ftue_v2` terrain under the tutorial ship (the deck is in no height map), m.
    const SHIP_TERRAIN_M: f32 = 50.0;
    /// The tutorial start standing on the test deck.
    const TUTORIAL_START_ON_SHIP: Position = nw_sim_core::TUTORIAL_START_FLOOR;
    /// Half the side of the test deck, m.
    const DECK_HALF_M: f32 = 20.0;
    /// Half the thickness of the test deck, m.
    const DECK_HALF_THICKNESS_M: f32 = 0.2;
    /// The `ftue_v2` terrain under the tutorial's move into the water, m.
    const WATER_SEABED_M: f32 = 10.82;
    /// The view of the tutorial tests' client.
    const TUTORIAL_VIEW: LookDir = LookDir {
        yaw: 0.0,
        pitch: 0.2,
    };
    /// Records that the tutorial tests' client shows the server's own place late.
    const CLIENT_LAG_RECORDS: usize = 3;
    /// Input records of the tutorial tests: the start video, a run, a rest.
    const VIDEO_RECORDS: u16 = 30;
    const RUN_RECORDS: u16 = 40;
    const REST_RECORDS: u16 = 60;

    /// The tutorial ship as the level collision gives it: a deck around the tutorial start with
    /// its top at the retail start floor, over the `ftue_v2` terrain.
    fn ship_ground() -> Arc<dyn Ground + Send + Sync> {
        use nw_world::collision::{Body, Geometry, IDENTITY_AFFINE, LevelCollision};
        use nw_world::collision_index::CollisionIndex;
        let mut place = IDENTITY_AFFINE;
        place[0][3] = TUTORIAL_START.x;
        place[1][3] = TUTORIAL_START.y;
        place[2][3] = nw_sim_core::TUTORIAL_START_FLOOR.z - DECK_HALF_THICKNESS_M;
        let collision = CollisionIndex::new(LevelCollision {
            level: TUTORIAL_LEVEL.folder.to_owned(),
            geometries: vec![Geometry::Box {
                half_extents: [DECK_HALF_M, DECK_HALF_M, DECK_HALF_THICKNESS_M],
            }],
            bodies: vec![Body { geometry: 0, place }],
            ..LevelCollision::default()
        });
        Arc::new(nw_sim_core::LevelGround::new(
            FlatGround(SHIP_TERRAIN_M),
            Arc::new(collision),
        ))
    }

    /// A tutorial world on `ground` with player 1 at the tutorial start.
    fn tutorial_world_on(ground: Arc<dyn Ground + Send + Sync>) -> SimWorld {
        tutorial_world_with(ground, Arc::default())
    }

    /// A tutorial world on `ground` with `content` and player 1 at the tutorial start.
    fn tutorial_world_with(
        ground: Arc<dyn Ground + Send + Sync>,
        content: Arc<PlayerContent>,
    ) -> SimWorld {
        let mut tutorial_settings = settings(MANY, MAX_PLAYERS);
        tutorial_settings.level = TUTORIAL_LEVEL;
        let mut combat = test_combat();
        combat.spawns = Vec::new().into();
        let mut world = SimWorld::new(
            tutorial_settings,
            WorldIds::random(0).expect("ids"),
            ground,
            slot("tut-1"),
            combat,
            content,
        );
        world.set_table(&table(&["tut-1"])).expect("table");
        let mut admit = admit_at(1, TUTORIAL_START.x, true);
        admit.position = Point::from(TUTORIAL_START);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        world
    }

    /// The body of player 1.
    fn body_of_first(world: &SimWorld) -> Position {
        world
            .world
            .get::<Body>(world.by_connection[&connection(1)])
            .expect("body")
            .motion
            .position()
    }

    /// The server's body of player 1 after `record` of `counter`, received at its own time.
    fn after_record(world: &mut SimWorld, counter: u16, record: InputRecord) -> Position {
        let now = NOW + u64::from(counter) * u64::from(attack::INPUT_RECORD_MS);
        let (result, body) = world
            .move_from_input(&connection(1), &[record], now)
            .expect("a new record");
        assert!(result.is_ok(), "record {counter}: {result:?}");
        body
    }

    #[test]
    fn a_new_player_stands_on_the_ship_s_deck_not_above_it() {
        // Run 13: the start 1.67 m above the deck (the client's camera point) held the player in
        // the air over the deck for the whole ship fight.
        let mut world = tutorial_world_on(ship_ground());
        let deck = nw_sim_core::TUTORIAL_START_FLOOR.z;
        // The start video: no camera follows the body.
        for counter in 1..=VIDEO_RECORDS {
            let record = InputRecord {
                bit_7: None,
                ..camera_record(
                    counter,
                    TUTORIAL_START,
                    TUTORIAL_VIEW,
                    [0.0; 2],
                    STANDING_SLOTS,
                )
            };
            let body = after_record(&mut world, counter, record);
            assert!(
                (body.z - deck).abs() < CLOSE_M,
                "record {counter}: {body:?}"
            );
        }
        // A run over the deck of the level collision: the client's own body runs on the deck,
        // its camera trailing it.
        let mut body = body_of_first(&world);
        let ground = ship_ground();
        let mut client = nw_sim_core::Character::new(body);
        for counter in VIDEO_RECORDS + 1..=VIDEO_RECORDS + RUN_RECORDS {
            client.step(
                nw_sim_core::CharacterInput {
                    intent: nw_sim_core::Intent::of_keys(true, false, false, false),
                    yaw: TUTORIAL_VIEW.yaw,
                    dt_s: f32::from(attack::INPUT_RECORD_MS) / 1_000.0,
                },
                &*ground,
                OCEAN_LEVEL_M,
            );
            let record = camera_record(
                counter,
                client.position(),
                TUTORIAL_VIEW,
                client.velocity(),
                FORWARD_SLOTS,
            );
            body = after_record(&mut world, counter, record);
            assert!(
                (body.z - deck).abs() < CLOSE_M,
                "record {counter}: {body:?}"
            );
        }
        assert!(body.y > TUTORIAL_START.y + 1.0, "it ran: {body:?}");
    }

    #[test]
    fn a_shown_fog_wall_stops_a_push_and_a_hidden_one_does_not() {
        use nw_content::ftue_entities::AssetEntity;
        use nw_world::collision::{Body, DeferredSlice, Geometry, IDENTITY_AFFINE, LevelCollision};
        /// The fog wall 3 m east of the start, 0.5 m thick and 10 m long, 4 m high.
        const WALL_EAST_M: f32 = 3.0;
        let start = TUTORIAL_START_ON_SHIP;
        let wall = [start.x + WALL_EAST_M, start.y, start.z];
        let mut place = IDENTITY_AFFINE;
        place[0][3] = wall[0];
        place[1][3] = wall[1];
        place[2][3] = wall[2] + 2.0;
        let collision = LevelCollision {
            level: TUTORIAL_LEVEL.folder.to_owned(),
            geometries: vec![Geometry::Box {
                half_extents: [0.25, 5.0, 2.0],
            }],
            bodies: Vec::new(),
            deferred: vec![DeferredSlice {
                slice: [7; 16],
                sub_id: 2,
                origin: wall,
                bodies: vec![Body { geometry: 0, place }],
            }],
            volumes: Vec::new(),
        };
        let mut content = PlayerContent::default();
        content.tutorial_level.assets = vec![AssetEntity {
            asset: [7; 16],
            asset_sub_id: 2,
            retail_gde_ref: [0; 16],
            position: Some(wall),
            spawns: Vec::new(),
            changes: Vec::new(),
            despawns: Vec::new(),
        }];
        content.asset_solids = crate::asset_solids::AssetSolids::of(
            &collision,
            &Arc::new(CollisionIndex::new(collision.clone())),
            &content.tutorial_level.assets,
        );
        let mut world = tutorial_world_with(ship_ground(), Arc::new(content));
        let entity = world.player_by_number(1).expect("player");
        let mut metres = 0.0;
        while metres <= 2.0 * WALL_EAST_M {
            world.walked_floor.walked(Position {
                x: start.x + metres,
                ..start
            });
            metres += 0.25;
        }
        let to = Position {
            x: start.x + 2.0 * WALL_EAST_M,
            ..start
        };
        world.carry_out(1, vec![Order::Asset(0, Step::Show)], NOW);
        let stopped = world.push_reach(entity, start, to);
        assert!(stopped.x < wall[0], "the wall stops it: {stopped:?}");
        world.carry_out(1, vec![Order::Asset(0, Step::Hide)], NOW);
        assert!(
            (world.push_reach(entity, start, to).x - to.x).abs() < 1e-3,
            "no wall once hidden"
        );
    }

    #[test]
    fn the_move_into_the_water_drops_the_player_into_the_sea() {
        // Run 13: after the move the server's body stayed at the place 7.6 m above the sea, the
        // client hung there with it, and the walked floor kept it there.
        let mut world = tutorial_world_on(Arc::new(FlatGround(WATER_SEABED_M)));
        let water = encounter::WATER_AT;
        world.carry_out(1, vec![Order::MoveTo(water, encounter::WATER_FACING)], NOW);
        assert_eq!(body_of_first(&world), water);
        let swim = nw_sim_core::character::swim_floor(OCEAN_LEVEL_M);
        let mut shown: std::collections::VecDeque<Position> =
            std::iter::repeat_n(water, CLIENT_LAG_RECORDS).collect();
        let mut body = water;
        let mut landed = None;
        for counter in 1..=REST_RECORDS {
            shown.push_back(body);
            let client = shown.pop_front().expect("lagged places");
            let record = camera_record(counter, client, TUTORIAL_VIEW, [0.0; 2], STANDING_SLOTS);
            let next = after_record(&mut world, counter, record);
            assert!(next.z <= body.z, "record {counter}: it rose to {next:?}");
            body = next;
            if landed.is_none() && (body.z - swim).abs() < CLOSE_M {
                landed = Some(counter);
            }
        }
        assert!(landed.is_some(), "never in the water: {body:?}");
        assert!((body.z - swim).abs() < CLOSE_M, "it swims: {body:?}");
        assert_eq!(
            world.walked_floor.floor_at(water),
            None,
            "the place in the air is no floor"
        );
        // Run 15: the client stood at the swim height until its own state said deep water.
        let entity = world.player_by_number(1).expect("player");
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!(
            player.footing.water,
            nw_sim_core::WaterDepth::Shallow,
            "the next own state tells its client to swim"
        );
        assert_ne!(player.footing, player.footing_sent, "a change to send");
    }

    #[test]
    fn a_move_inside_the_limits_is_seen_and_a_fast_one_is_refused() {
        let mut world = world();
        let (a, b) = (connection(1), connection(2));
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 10.0);
        assert_eq!(
            world.move_to(&a, at(SPEED_MPS), NOW + ONE_SECOND_MS),
            Ok(GroundCheck::NoGround)
        );
        let outgoing = tick(&mut world, NOW + ONE_SECOND_MS).messages;
        let (_, seen_x) = positions_seen_by(&world, &outgoing, &b)[1];
        assert!((seen_x - SPEED_MPS).abs() < CLOSE_M);
        let far = at(SPEED_MPS * 10.0);
        let later = NOW + ONE_SECOND_MS + A_BIT_LATER_MS;
        assert_eq!(
            world.move_to(&a, far, later),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        let outgoing = tick(&mut world, later).messages;
        assert!(
            positions_seen_by(&world, &outgoing, &a)
                .iter()
                .all(|(entity, _)| *entity != 1),
            "no own position while its moves are refused"
        );
        let seen_by_b: Vec<_> = outgoing
            .iter()
            .filter(|out| out.connection == b && out.channel == REPLICATION_CHANNEL)
            .flat_map(|out| &out.envelopes)
            .flat_map(|bytes| groups_in(&world, &b, &bundle_in(bytes)))
            .filter(|(entity, _)| *entity == 1)
            .collect();
        assert!(!seen_by_b.is_empty(), "b sees a");
        // An unmoved player is an offset from the anchor b has (the last good move), with no new
        // pos10; a new one would be that move.
        assert!(
            seen_by_b
                .iter()
                .filter_map(|(_, update)| update.position)
                .all(|position| (position.x - SPEED_MPS).abs() < CLOSE_M),
            "others see the last good move"
        );
        assert_eq!(
            world.move_to(&connection(9), at(1.0), later),
            Err(MoveError::NotInWorld)
        );
        let departure = world.leave(&a).expect("left");
        assert!((departure.position.x - SPEED_MPS).abs() < CLOSE_M);
    }

    #[test]
    fn a_departure_carries_the_time_of_the_last_good_move() {
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        assert_eq!(world.positions()[0].moved_at_ms, NOW);
        world
            .move_to(&a, at(SPEED_MPS), NOW + ONE_SECOND_MS)
            .expect("move");
        let refused = world.move_to(&a, at(SPEED_MPS * 10.0), NOW + ONE_SECOND_MS + 1);
        assert!(refused.is_err());
        let departure = world.leave(&a).expect("left");
        assert_eq!(departure.moved_at_ms, NOW + ONE_SECOND_MS);
    }

    #[test]
    fn a_server_move_goes_out_in_three_states_in_a_row() {
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        let card_at = NOW + OWN_CARD_DELAY_MS;
        tick(&mut world, card_at);
        world.heard_from(&a);
        let entity = world.player_by_number(1).expect("player");
        let step = |world: &SimWorld| {
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .teleport_step
        };
        world.world.get_mut::<Player>(entity).expect("player").moved = true;
        let mut steps = Vec::new();
        for n in 1..=4 {
            tick(&mut world, card_at + 33 * n);
            steps.push(step(&world));
        }
        assert_eq!(
            steps,
            [
                Some(TeleportStep::Confirm),
                Some(TeleportStep::Hold),
                None,
                None
            ],
            "Place, then Confirm, then Hold, one per bundle"
        );
    }

    #[test]
    fn a_client_without_view_angles_moves_to_each_of_its_points() {
        // The test client and the load-test bots send one point per input and number no
        // records (counter 0 in each).
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        for (step, x) in [1.0, 2.0, 3.0_f32].into_iter().enumerate() {
            let target = at(x);
            let input = nw_messages::ClientInputMsg::position(
                InputPosition::from_metres(target.x, target.y, target.z).expect("finite"),
            );
            let arrived = NOW + ONE_SECOND_MS * (1 + u64::try_from(step).expect("small"));
            let (result, body) = world
                .move_from_input(&a, &input.records, arrived)
                .expect("a point");
            assert!(result.is_ok(), "{result:?}");
            assert!(body.horizontal_distance(target) < CLOSE_M, "{body:?}");
        }
    }

    #[test]
    fn the_inventory_open_and_close_go_back_to_the_client_as_looking_through_loadout() {
        const TICK_MS: u64 = 33;
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        tick(&mut world, NOW);
        let loadout = |looking| ClientFacetMsg::LookingThroughLoadout {
            header: nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 1),
            looking,
        };
        let looking_of = |states: Vec<MemberState>| -> Vec<bool> {
            states
                .into_iter()
                .filter_map(|state| match state {
                    MemberState::PlayerComponent(state) => state.looking_through_loadout,
                    _ => None,
                })
                .collect()
        };
        // Retail newchar 2026-09-18: `04 10 01` 37 ms after the open, `04 10 00` 36 ms after
        // the close; the client closed an inventory the server never answered (live run 15).
        let mut at = NOW;
        for (looking, sent) in [(true, vec![true]), (false, vec![false]), (false, vec![])] {
            at += TICK_MS;
            world
                .facet_message(&a, loadout(looking), at)
                .expect("loadout");
            let out = tick(&mut world, at);
            assert_eq!(
                looking_of(states_of(&world, &out.messages, &a, 1)),
                sent,
                "after {looking}"
            );
        }
        let out = tick(&mut world, at + TICK_MS);
        assert!(
            looking_of(states_of(&world, &out.messages, &a, 1)).is_empty(),
            "once per change"
        );
    }

    #[test]
    fn the_inventory_camera_moves_no_body_until_it_glided_back() {
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        let loadout = |looking| ClientFacetMsg::LookingThroughLoadout {
            header: nw_messages::FacetHeader::of(nw_messages::facet::component::PLAYER, 1),
            looking,
        };
        // The camera alone, one record per input: the body is the camera point.
        let sample = |record: u16, x: f32| {
            let camera = at(x);
            [InputRecord {
                u16_after_mask: record,
                bit_7: InputPosition::from_metres(camera.x, camera.y, camera.z),
                ..InputRecord::default()
            }]
        };
        let opened = NOW + ONE_SECOND_MS;
        world
            .facet_message(&a, loadout(true), opened)
            .expect("opened");
        // Live 2026-09-18: the inventory camera glided 3.6 m down toward the body.
        let (result, body) = world
            .move_from_input(&a, &sample(1, 1.0), opened + 1)
            .expect("a new record");
        assert_eq!(result, Ok(GroundCheck::Checked));
        assert_eq!(body, at(0.0), "the body stays while the inventory is open");
        assert_eq!(world.positions()[0].position, at(0.0));
        let closed = opened + ONE_SECOND_MS;
        world
            .facet_message(&a, loadout(false), closed)
            .expect("closed");
        let gliding = closed + LOADOUT_CAMERA_SETTLE_MS - 1;
        let _ = world.move_from_input(&a, &sample(2, 1.0), gliding);
        assert_eq!(
            world.positions()[0].position,
            at(0.0),
            "the camera still glides back"
        );
        let settled = closed + LOADOUT_CAMERA_SETTLE_MS;
        let (result, _) = world
            .move_from_input(&a, &sample(3, 1.0), settled)
            .expect("a new record");
        assert!(result.is_ok(), "{result:?}");
        let moved = world.positions()[0].position;
        assert!(
            moved.horizontal_distance(at(1.0)) < CLOSE_M && (moved.z - at(1.0).z).abs() < CLOSE_M,
            "moves count again: {moved:?}"
        );
    }

    #[test]
    fn a_move_back_never_goes_under_the_terrain() {
        /// The terrain height: the test players stand on it.
        const TERRAIN_M: f32 = 50.0;
        /// Live 2026-09-18: an estimate 1.8 m under the beach was accepted (tolerance 2 m).
        const UNDER_M: f32 = 1.8;
        /// Then the client stood 3 m under it, refused.
        const DEEP_M: f32 = 3.0;
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(FlatGround(TERRAIN_M)),
            slot("sim-a"),
            test_combat(),
            Arc::default(),
        );
        let a = connection(1);
        join(&mut world, 1, 0.0);
        let entity = world.player_by_number(1).expect("player");
        let under = Position {
            z: TERRAIN_M - UNDER_M,
            ..at(1.0)
        };
        let first = NOW + ONE_SECOND_MS;
        assert!(
            world.move_to(&a, under, first).is_ok(),
            "within the tolerance"
        );
        let deep = Position {
            z: TERRAIN_M - DEEP_M,
            ..at(1.0)
        };
        let refused = first + 1;
        assert_eq!(
            world.move_to(&a, deep, refused),
            Err(MoveError::Refused(MoveRefusal::OffGround))
        );
        assert_eq!(
            world.move_to(&a, deep, refused + REFUSED_RESYNC_MS),
            Err(MoveError::Refused(MoveRefusal::OffGround))
        );
        assert!(world.world.get::<Player>(entity).expect("player").moved);
        assert_eq!(
            world.positions()[0].position,
            at(0.0),
            "back to the last spot on the terrain, not to the one under it"
        );
    }

    #[test]
    fn refused_moves_for_a_while_move_the_player_back() {
        let mut world = world();
        let a = connection(1);
        join(&mut world, 1, 0.0);
        let entity = world.player_by_number(1).expect("player");
        let refused_since_ms = |world: &SimWorld| {
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .refused_since_ms
        };
        let moved = |world: &SimWorld| world.world.get::<Player>(entity).expect("player").moved;
        let far = at(SPEED_MPS * 10.0);
        let first = NOW + 1;
        assert_eq!(
            world.move_to(&a, far, first),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        assert_eq!(refused_since_ms(&world), Some(first));
        assert!(
            !moved(&world),
            "one refused move only keeps the last good position"
        );
        let before = first + REFUSED_RESYNC_MS - 1;
        assert_eq!(
            world.move_to(&a, far, before),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        assert!(
            !moved(&world),
            "a short row of refused moves does not move the player"
        );
        let over = first + REFUSED_RESYNC_MS;
        assert_eq!(
            world.move_to(&a, far, over),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        assert!(
            moved(&world),
            "then the next own state moves the player back"
        );
        assert_eq!(refused_since_ms(&world), None, "the move-back ends the row");
        assert_eq!(
            world.move_to(&a, far, over + 1),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        assert_eq!(refused_since_ms(&world), Some(over + 1), "a new row starts");
        assert_eq!(
            world.move_to(&a, at(SPEED_MPS), over + REFUSED_RESYNC_MS),
            Ok(GroundCheck::NoGround)
        );
        assert_eq!(refused_since_ms(&world), None, "a good move ends the row");
    }

    #[test]
    fn an_admit_uses_the_given_entity_number() {
        let mut world = world();
        let login = world
            .admit(&admit_at(77, 0.0, true), NOW, SERVER_TIME)
            .expect("admit")
            .expect("login messages");
        assert_eq!(login.channel, MESSAGE_CHANNEL);
        assert!(login.reliable);
        /// 4970 follows the seven login messages.
        const LEFT_QUEUE_MESSAGE: usize = 1;
        assert_eq!(
            login.envelopes.len(),
            LOGIN_MESSAGE_TYPES.len() + LEFT_QUEUE_MESSAGE
        );
        let outgoing = tick(&mut world, NOW + OWN_CARD_DELAY_MS).messages;
        assert_eq!(seen_by(&world, &outgoing, &connection(77)), vec![77]);
    }

    #[test]
    fn a_spawn_tells_the_client_whether_its_account_is_in_probation() {
        for in_probation in [false, true] {
            let mut world = world_with_spawns(Vec::new());
            world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
            let admit = Admit {
                account_in_probation: in_probation,
                ..admit_at(1, CELL_M / 2.0, true)
            };
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            tick(&mut world, NOW + OWN_CARD_DELAY_MS);
            world.heard_from(&connection(1));
            let entity = world.player_by_number(1).expect("player");
            let player = world.world.get::<Player>(entity).expect("player");
            assert!(
                player.extra_members.iter().any(|member| matches!(
                    &member.state,
                    MemberState::PlayerComponent(state)
                        if state.account_in_probation == Some(in_probation)
                )),
                "{in_probation}"
            );
        }
    }

    #[test]
    fn a_login_saved_in_the_air_arrives_on_the_ground() {
        const GROUND_M: f32 = 20.0;
        let mut world = SimWorld::new(
            settings(MANY, MAX_PLAYERS),
            WorldIds::random(0).expect("ids"),
            Arc::new(FlatGround(GROUND_M)),
            slot("sim-a"),
            test_combat(),
            Arc::default(),
        );
        let mut in_the_air = admit_at(1, 0.0, true);
        in_the_air.position.z = GROUND_M + MAX_ABOVE_GROUND_M + 1.0;
        world.admit(&in_the_air, NOW, SERVER_TIME).expect("admit");
        let departure = world.leave(&connection(1)).expect("left");
        assert_eq!(departure.position.z, GROUND_M);
    }

    #[test]
    fn a_resume_sends_no_login_messages() {
        let mut world = world();
        let login = world
            .admit(&admit_at(5, 0.0, false), NOW, SERVER_TIME)
            .expect("admit");
        assert_eq!(login, None);
        assert_eq!(world.players(), 1);
    }

    #[test]
    fn a_resume_of_a_player_that_is_here_keeps_it() {
        let mut world = world();
        let first = admit_at(1, 0.0, true);
        world.admit(&first, NOW, SERVER_TIME).expect("admit");
        world
            .move_to(&connection(1), at(SPEED_MPS), NOW + ONE_SECOND_MS)
            .expect("move");
        let resume = Admit {
            login: false,
            position: Point {
                x: 100.0,
                y: 10.0,
                z: 50.0,
            },
            ..first.clone()
        };
        assert_eq!(
            world.admit(&resume, NOW, SERVER_TIME).expect("resume"),
            None
        );
        assert_eq!(world.players(), 1);
        assert_eq!(world.positions()[0].position, at(SPEED_MPS));
        let other_entity = Admit {
            entity: 2,
            ..resume
        };
        assert!(world.admit(&other_entity, NOW, SERVER_TIME).is_err());
    }

    #[test]
    fn a_resumed_player_counts_its_handoffs_from_the_given_base() {
        let mut world = world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        let resumed = Admit {
            handoffs: RESUME_BASE,
            ..admit_at(1, BORDER_X + 50.0, false)
        };
        world.admit(&resumed, NOW, SERVER_TIME).expect("admit");
        let output = tick(&mut world, NOW);
        assert_eq!(output.handoffs[0].offer.handoffs, RESUME_BASE + 1);
    }

    #[test]
    fn a_resumed_player_goes_on_with_the_bundle_sequence() {
        let mut world = world();
        let resumed = Admit {
            bundles_sent: BUNDLES_BEFORE,
            ..admit_at(1, 0.0, false)
        };
        world.admit(&resumed, NOW, SERVER_TIME).expect("admit");
        let output = tick(&mut world, NOW);
        let bundle = bundle_of(&output.messages, &connection(1)).expect("bundle");
        assert_eq!(bundle.leading, Some(BUNDLES_BEFORE));
    }

    #[test]
    fn a_full_sim_refuses() {
        let mut world = world();
        for number in 1..=u64::from(MAX_PLAYERS) {
            join(&mut world, number, 0.0);
        }
        assert!(matches!(
            world.admit(&admit_at(9, 0.0, true), NOW, SERVER_TIME),
            Err(WorldError::Full)
        ));
    }

    #[test]
    fn players_see_themselves_first_and_others_inside_the_radius() {
        let mut world = world();
        let (a, b, c) = (connection(1), connection(2), connection(3));
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 50.0);
        join(&mut world, 3, 500.0);
        let outgoing = tick(&mut world, NOW).messages;
        assert_eq!(seen_by(&world, &outgoing, &a), vec![1, 2]);
        assert_eq!(seen_by(&world, &outgoing, &b), vec![2, 1]);
        assert_eq!(seen_by(&world, &outgoing, &c), vec![3]);
        assert!(
            outgoing
                .iter()
                .all(|out| out.channel != REPLICATION_CHANNEL || !out.reliable)
        );
    }

    /// What every connection saw in one tick: its bundle members by entity number and x.
    fn seen_by_all(world: &SimWorld, outgoing: &[Outgoing]) -> BTreeMap<u64, Vec<(u64, f32)>> {
        outgoing
            .iter()
            .filter(|out| out.channel == REPLICATION_CHANNEL)
            .map(|out| {
                (
                    out.connection.connection,
                    positions_seen_by(world, outgoing, &out.connection),
                )
            })
            .collect()
    }

    #[test]
    fn the_bundle_step_on_many_threads_gives_the_same_bundles() {
        const PLAYERS: u64 = 300;
        /// Each player sees its neighbours within the radius on both sides (four or five), so
        /// every bundle differs; a closer spacing only makes the cards of a debug build slow.
        const SPACING_M: f32 = 40.0;
        let crowd = |threads| {
            let mut world = world_of(
                "sim-a",
                WorldSettings {
                    tick_threads: threads,
                    ..settings(MANY, PLAYERS as u32)
                },
            );
            for number in 1..=PLAYERS {
                join(&mut world, number, number as f32 * SPACING_M);
            }
            let messages = tick(&mut world, NOW).messages;
            seen_by_all(&world, &messages)
        };
        let one = crowd(1);
        assert_eq!(one.len(), PLAYERS as usize);
        assert_eq!(crowd(4), one);
    }

    #[test]
    fn only_the_nearest_are_in_the_bundle() {
        let mut world = world_of("sim-a", settings(TWO, MAX_PLAYERS + 1));
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 30.0);
        join(&mut world, 3, 10.0);
        join(&mut world, 4, 20.0);
        let outgoing = tick(&mut world, NOW).messages;
        assert_eq!(seen_by(&world, &outgoing, &connection(1)), vec![1, 3, 4]);
    }

    #[test]
    fn a_player_who_leaves_is_gone_from_the_next_bundles() {
        let mut world = world();
        let (a, b) = (connection(1), connection(2));
        let admit = admit_at(2, 10.0, true);
        join(&mut world, 1, 0.0);
        world.admit(&admit, NOW, SERVER_TIME).expect("b");
        let departure = world.leave(&b).expect("left");
        assert_eq!(departure.connection, b);
        assert_eq!(departure.character, admit.character);
        assert_eq!(departure.position, at(10.0));
        assert_eq!(departure.moved_at_ms, NOW);
        let outgoing = tick(&mut world, NOW).messages;
        assert_eq!(seen_by(&world, &outgoing, &a), vec![1]);
        assert_eq!(world.leave(&b), None);
    }

    #[test]
    fn a_connection_or_character_joins_once() {
        let mut world = world();
        let first = admit_at(1, 0.0, true);
        world.admit(&first, NOW, SERVER_TIME).expect("first");
        assert!(matches!(
            world.admit(&admit_at(1, 0.0, true), NOW, SERVER_TIME),
            Err(WorldError::AlreadyJoined(_))
        ));
        let same_character = Admit {
            connection: connection(2),
            entity: 2,
            ..first
        };
        assert!(matches!(
            world.admit(&same_character, NOW, SERVER_TIME),
            Err(WorldError::CharacterInWorld(_))
        ));
    }

    #[test]
    fn a_time_synch_goes_out_every_three_seconds() {
        let mut world = world();
        join(&mut world, 1, 0.0);
        let synchs = |output: TickOutput| {
            output
                .messages
                .iter()
                .filter(|out| out.channel == MESSAGE_CHANNEL)
                .flat_map(|out| &out.envelopes)
                .filter(|bytes| {
                    matches!(
                        ServerMessage::from_envelope(&Envelope::decode(bytes).expect("envelope")),
                        Some(Ok(ServerMessage::TimeSynch(_)))
                    )
                })
                .count()
        };
        assert_eq!(synchs(tick(&mut world, NOW + 1)), 0);
        assert_eq!(synchs(tick(&mut world, NOW + TIME_SYNCH_INTERVAL_MS)), 1);
        assert_eq!(
            synchs(tick(&mut world, NOW + TIME_SYNCH_INTERVAL_MS + 1)),
            0
        );
    }

    #[test]
    fn each_lookup_call_is_answered_at_once() {
        let mut world = world();
        join(&mut world, 1, 0.0);
        let contracts =
            nw_messages::FacetHeader::of(nw_messages::contracts::CONTRACTS_COMPONENT, 1);
        let lobbies = nw_messages::FacetHeader::of(0xe6db_502a_b7ab_2bb2, 1);
        let lookup = [7; nw_messages::wire::RAW_ID_LEN];
        world.lookup(
            &connection(1),
            &LookupRequest::Contracts {
                header: contracts,
                query: nw_messages::contracts::ParticipantQuery {
                    participant: nw_messages::contracts::Participant::NONE,
                    open_filter: nw_messages::contracts::open_filter::OPEN,
                    role_filter: nw_messages::contracts::role_filter::CREATOR,
                    page: 0,
                    page_size: 10,
                    kinds: vec![1],
                    tail: Vec::new(),
                },
                lookup,
            },
        );
        world.lookup(
            &connection(1),
            &LookupRequest::LobbyCounts {
                header: lobbies,
                activity: 1,
            },
        );
        world.lookup(
            &connection(2),
            &LookupRequest::SubArmies { header: lobbies },
        );
        let sent = world.take_messages();
        let answers = messages_to(&sent, 1);
        assert!(matches!(
            &answers[..],
            [ServerMessage::LobbyCounts(counts)] if counts.header == lobbies
                && counts.counts == vec![0; nw_messages::lookups::LOBBY_COUNT_SLOTS]
        ));
        assert!(
            messages_to(&sent, 2).is_empty(),
            "no answer for a connection without a player"
        );
        let jobs = world.take_economy_jobs();
        assert!(
            matches!(
                &jobs[..],
                [economy::EconomyJob::CharacterLookup { caller, wanted }]
                    if caller.header == contracts && caller.request == lookup && wanted.open
            ),
            "the trading post answers the contracts lookup from the world database"
        );
    }

    #[test]
    fn a_position_that_is_not_finite_cannot_join() {
        let mut world = world();
        assert!(matches!(
            world.admit(&admit_at(1, f32::NAN, true), NOW, SERVER_TIME),
            Err(WorldError::NotFinite)
        ));
        assert_eq!(world.players(), 0);
    }

    #[test]
    fn a_gateway_that_stops_takes_its_players_out() {
        let mut world = world();
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 0.0);
        let gateway = BusId::try_from("gw-1".to_owned()).expect("id");
        let mut gone: Vec<u64> = world
            .leave_gateway(&gateway)
            .into_iter()
            .map(|departure| departure.connection.connection)
            .collect();
        gone.sort_unstable();
        assert_eq!(gone, vec![1, 2]);
        assert_eq!(world.players(), 0);
        assert_eq!(world.positions(), Vec::new());
    }

    #[test]
    fn many_visible_players_go_out_in_bundles_that_fit_one_datagram() {
        const PLAYERS: u64 = 100;
        let mut world = world_of("sim-a", settings(MANY, PLAYERS as u32));
        let viewer = connection(1);
        for number in 1..=PLAYERS {
            join(&mut world, number, 1.0);
        }
        let outgoing = tick(&mut world, NOW).messages;
        let bundles = outgoing
            .iter()
            .filter(|out| out.connection == viewer && out.channel == REPLICATION_CHANNEL)
            .count();
        assert!(bundles > 1);
        for out in outgoing
            .iter()
            .filter(|out| out.channel == REPLICATION_CHANNEL)
        {
            let payload = encode_server_messages(&[&out.envelopes[0]]).expect("payload");
            assert!(payload.len() <= MAX_CHUNK_PAYLOAD, "{}", payload.len());
        }
        // The largest bundle: the largest sequence, entity numbers and slots, every group with
        // its card, and as many stops as one bundle takes.
        let largest_number = u64::MAX >> LARGEST_NUMBER_SHIFT;
        let mut slots = Slots::restored(
            (0..MAX_BUNDLE_MEMBERS as u64)
                .map(|offset| (offset, u16::MAX - MAX_BUNDLE_MEMBERS as u16 + offset as u16))
                .collect(),
            Vec::new(),
        );
        slots.keep_only(&[]);
        let full: Vec<BundleGroup> = (0..MAX_BUNDLE_MEMBERS as u64)
            .filter_map(|offset| {
                remote_group(
                    u16::MAX - offset as u16,
                    largest_number - offset,
                    EntityKind::PLAYER,
                    at(0.0),
                    true,
                )
                .expect("encode")
            })
            .collect();
        // The largest sequence that leaves room for the next bundles of this tick.
        let sequence = largest_number - MAX_BUNDLE_MEMBERS as u64;
        let envelopes =
            members::bundles(sequence, LOGIN_CLIENT_CONTEXT, &mut slots, full).expect("bundles");
        for envelope in &envelopes {
            let payload = encode_server_messages(&[envelope]).expect("payload");
            assert!(payload.len() <= MAX_CHUNK_PAYLOAD, "{}", payload.len());
        }
    }

    /// A world of the first of two sims, with a player 50 m inside the second sim's cells and
    /// a viewer near it on this side.
    fn border_world() -> (SimWorld, ConnectionRef, ConnectionRef) {
        let mut world = world();
        assert!(
            world
                .set_table(&table(&["sim-a", "sim-b"]))
                .expect("table")
                .is_empty()
        );
        join(&mut world, 1, BORDER_X + 50.0);
        join(&mut world, 2, BORDER_X - 20.0);
        (world, connection(1), connection(2))
    }

    #[test]
    fn a_player_past_the_margin_is_handed_off_and_frozen() {
        let (mut world, mover, viewer) = border_world();
        let first = tick(&mut world, NOW);
        assert_eq!(first.handoffs.len(), 1);
        let start = &first.handoffs[0];
        assert_eq!(start.target, slot("sim-b"));
        assert_eq!(start.offer.connection, mover);
        assert_eq!(start.offer.entity, 1);
        assert_eq!(start.offer.handoffs, 1);
        assert_eq!(start.offer.position.x, BORDER_X + 50.0);
        assert!(bundle_of(&first.messages, &mover).is_none(), "frozen");
        assert_eq!(seen_by(&world, &first.messages, &viewer), vec![2, 1]);
        assert_eq!(
            world.move_to(&mover, at(BORDER_X + 51.0), NOW + ONE_SECOND_MS),
            Err(MoveError::Frozen)
        );
        assert!(tick(&mut world, NOW + 1).handoffs.is_empty());
        let near = world_with_one_near_the_border();
        assert!(near.is_empty());
    }

    /// A player 3 m past the border (inside the margin) is not handed off.
    fn world_with_one_near_the_border() -> Vec<HandoffStart> {
        let mut world = world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X + MARGIN_M - 2.0);
        tick(&mut world, NOW).handoffs
    }

    #[test]
    fn an_answered_handoff_moves_or_keeps_the_player() {
        let (mut world, mover, _) = border_world();
        tick(&mut world, NOW);
        assert_eq!(world.handoff_done(&mover, false, NOW), HandoffEnd::Kept);
        assert_eq!(
            world.move_to(&mover, at(BORDER_X + 51.0), NOW + A_BIT_LATER_MS),
            Ok(GroundCheck::NoGround)
        );
        assert!(tick(&mut world, NOW + A_BIT_LATER_MS).handoffs.is_empty());
        let again = tick(&mut world, NOW + RETRY_MS);
        assert_eq!(again.handoffs.len(), 1);
        assert_eq!(
            world.handoff_done(&mover, true, NOW + RETRY_MS),
            HandoffEnd::Moved
        );
        assert_eq!(world.players(), 1);
        assert_eq!(world.handoff_done(&mover, true, NOW), HandoffEnd::Gone);
    }

    #[test]
    fn a_taken_player_keeps_its_number_sequence_and_budget() {
        let (mut old, mover, _) = border_world();
        tick(&mut old, NOW);
        let mut offer = tick_offer(&mut old);
        assert_eq!(offer.name, player_name(1), "the name crosses");
        // The client tied a slot to the viewer before the handoff.
        offer.slots = vec![(2, slots::FIRST_REMOTE_INTEREST)];
        offer.bundles_sent = BUNDLES_BEFORE;
        let mut new = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        new.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        new.take(&offer, NOW, SERVER_TIME).expect("take");
        assert_eq!(
            new.positions()[0].moved_at_ms,
            NOW - offer.move_age_ms,
            "the move time crosses as an age"
        );
        assert_eq!(new.snapshot().0[0].name, offer.name);
        let output = tick(&mut new, NOW);
        let bundle = bundle_of(&output.messages, &mover).expect("bundle");
        assert_eq!(bundle.leading, Some(BUNDLES_BEFORE));
        assert_eq!(
            bundle.index_block.map(|block| block.indexes),
            Some(vec![slots::FIRST_REMOTE_INTEREST]),
            "the taker does not see the viewer, so it stops the viewer's slot"
        );
        assert_eq!(seen_by(&new, &output.messages, &mover), vec![1]);
        let reach = BORDER_X + 50.0 + offer.budget_m;
        assert_eq!(
            new.move_to(&mover, at(reach + 1.0), NOW),
            Err(MoveError::Refused(MoveRefusal::TooFast))
        );
        assert_eq!(
            new.move_to(&mover, at(reach - 0.5), NOW),
            Ok(GroundCheck::NoGround)
        );
        assert!(matches!(
            new.take(&offer, NOW, SERVER_TIME),
            Err(WorldError::AlreadyJoined(_))
        ));
        assert!(
            !new.cancel(&mover, offer.handoffs + 1),
            "a cancel of another attempt"
        );
        assert!(new.cancel(&mover, offer.handoffs));
        assert_eq!(new.players(), 0);
        assert!(!new.cancel(&mover, offer.handoffs));
    }

    #[test]
    fn a_taken_player_keeps_its_raw_id() {
        /// A number no other test of this process gives a raw id: the taker stands for another
        /// process, where the player has no id yet.
        const TAKEN: u64 = 0x3d00_0000_0001;
        let (mut old, mover, _) = border_world();
        tick(&mut old, NOW);
        let mut offer = tick_offer(&mut old);
        assert_eq!(
            offer.gde_ref,
            raw_id_of_entity(offer.entity),
            "the giver's id crosses"
        );
        // An id the taker's process never made, of the dynamic form.
        let mut carried = offer.gde_ref;
        carried[0] ^= 0x02;
        carried[nw_messages::RAW_ID_LEN - 1] ^= 0xff;
        offer.entity = TAKEN;
        offer.gde_ref = carried;
        let mut new = world_of("sim-b", settings(MANY, MAX_PLAYERS));
        new.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        new.take(&offer, NOW, SERVER_TIME).expect("take");
        assert_eq!(raw_id_of_entity(TAKEN), carried);
        let output = tick(&mut new, NOW);
        let bundle = bundle_of(&output.messages, &mover).expect("bundle");
        let own_card = SlotRead::of(&bundle)
            .expect("read")
            .updates
            .into_iter()
            .find_map(|update| update.card.filter(|_| update.is_local_player()))
            .expect("the own card");
        assert_eq!(
            own_card.gde_ref,
            Some(carried),
            "the client keeps knowing it by that id"
        );
    }

    /// The offer of the frozen player of a border world.
    fn tick_offer(world: &mut SimWorld) -> HandoffOffer {
        let mover = connection(1);
        world.handoff_done(&mover, false, NOW);
        tick(world, NOW + RETRY_MS).handoffs[0].offer.clone()
    }

    #[test]
    fn ghosts_are_seen_and_expire() {
        let mut world = world();
        join(&mut world, 1, 0.0);
        let ghost = |entity, x| Ghost {
            entity,
            position: Point {
                x,
                y: 10.0,
                z: 50.0,
            },
            mob: None,
            player: None,
        };
        world.set_ghosts(
            &slot("sim-b").id,
            vec![
                ghost(GHOST_NUMBER, 10.0),
                ghost(1, 20.0),
                ghost(7, f32::NAN),
            ],
            NOW,
        );
        let outgoing = tick(&mut world, NOW).messages;
        assert_eq!(
            positions_seen_by(&world, &outgoing, &connection(1)),
            vec![(1, 0.0), (GHOST_NUMBER, 10.0)]
        );
        let later = tick(&mut world, NOW + GHOST_TTL_MS + 1).messages;
        assert_eq!(seen_by(&world, &later, &connection(1)), vec![1]);
    }

    #[test]
    fn players_near_a_border_become_ghosts_of_the_neighbour() {
        let mut world = world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 50.0);
        join(&mut world, 2, 100.0);
        let ghosts = tick(&mut world, NOW).ghosts;
        assert_eq!(ghosts.len(), 1);
        let (target, entities) = &ghosts[0];
        assert_eq!(target, &slot("sim-b").id);
        let numbers: Vec<u64> = entities.iter().map(|ghost| ghost.entity).collect();
        assert_eq!(numbers, vec![1]);
    }

    #[test]
    fn an_empty_ghost_list_follows_a_full_one() {
        let mut world = world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, BORDER_X - 50.0);
        assert_eq!(tick(&mut world, NOW).ghosts.len(), 1);
        world.leave(&connection(1));
        let cleared = tick(&mut world, NOW + 1).ghosts;
        assert_eq!(cleared, vec![(slot("sim-b").id, Vec::new())]);
        assert!(tick(&mut world, NOW + 2).ghosts.is_empty());
    }

    #[test]
    fn a_sim_missing_from_the_table_drops_its_players() {
        let mut world = world();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, 0.0);
        join(&mut world, 2, 0.0);
        let mut restarted = table(&["sim-a", "sim-b"]);
        restarted.sims[0].generation = GENERATION + 1;
        assert_eq!(world.set_table(&restarted).expect("table").len(), 2);
        assert_eq!(world.players(), 0);
        join(&mut world, 3, BORDER_X + 50.0);
        assert!(
            tick(&mut world, NOW).handoffs.is_empty(),
            "no table, no handoff"
        );
    }

    #[test]
    fn the_same_table_again_is_not_applied_again() {
        // Run 13: the director sends its table every second; applying it walked every spawn and
        // made a wake scan of every player again.
        let mut world = world_with_spawns(vec![test_spawn(CELL_M / 2.0)]);
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        join(&mut world, 1, CELL_M / 2.0);
        tick(&mut world, NOW);
        let (_, scans) = world.mob_work();
        world.set_table(&table(&["sim-a", "sim-b"])).expect("table");
        tick(&mut world, NOW + 33);
        assert_eq!(world.mob_work().1, scans, "no new wake scan");
        world.set_table(&table(&["sim-b", "sim-a"])).expect("table");
        let changes = world.take_mob_changes();
        assert_eq!(changes.removed, 1, "a changed table still applies");
    }

    #[test]
    fn a_table_of_another_size_is_ignored() {
        let mut world = world();
        let mut wrong = table(&["sim-a", "sim-b"]);
        wrong.owners.pop();
        assert_eq!(world.set_table(&wrong), Err(TableError::WrongSize));
        join(&mut world, 1, BORDER_X + 50.0);
        assert!(tick(&mut world, NOW).handoffs.is_empty());
    }
}
