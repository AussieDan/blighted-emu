// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mobs of one sim (M6-A spec section 6). A sim spawns the vitals spawns of the world file
//! that lie in its cells; a mob belongs to the sim of its spawn and never hands off, because its
//! leash keeps it near the spawn.
//!
//! Each mob's mind is the game's own (behavior-tree study 2026-09-18):
//! - a mob whose creature slice names a tree Server.pak has runs that tree
//!   ([`crate::mob_ai::TreeMind`]); so does the beach first mate, whose missing tree the AI data
//!   builds from held trees (`content_tool::stand_in_trees`);
//! - the three tutorial mobs whose trees Server.pak lacks (FTUE club sailor, FTUE legionnaire,
//!   tutorial Thorpe; study section 4.1) fight with the paces measured from the retail tutorial
//!   (`nw_sim_core::ChaseBrain`, set by the tutorial's encounter script);
//! - any other mob has no tree and no measured script: it stands at its spawn.
//!
//! Only awake mobs think. Each tick, the mobs within the wake radius of a player wake; an awake
//! mob without a target and with no player within the wake radius for the sleep time falls
//! asleep. A grid of the spawn points, in buckets of the wake radius plus the largest leash,
//! finds the mobs near a player. A player scans again only after it moved [`WAKE_RESCAN_M`] since
//! its last scan, or when the cells change. Awake tree mobs think on the sim's tick threads.
//!
//! A personal mob (a tutorial encounter's) belongs to one player: only that player sees it and is
//! its target, it may be held idle until its fight starts, it stays awake, and it does not
//! respawn: its encounter removes it.
//!
//! A GM mob (admin studio spec section 8.2) is spawned by a game master at any point: every player
//! sees and fights it, it stays awake, and it respawns at its spawn until the GM removes it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use nw_ai::data::SliceTriggerRef;
use nw_ai::value::NameId;
use nw_bus::{MOB_ENTITY_BASE, MOB_STATES, MobGhost, SnapMob};
use nw_content::combat::CombatData;
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_sim_core::{
    BrainAction, ChaseBrain, ChaseSettings, Dice, Gait, Ground, Health, MobAttack, MobState,
    Position, Seen, SpatialGrid, TUTORIAL_FOLDER, mob_max_health, walk_within,
};
pub use nw_world::records::VitalsSpawn;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::combat::CombatSettings;
use crate::mob_ai::{
    COMBAT_LEASH_KEY, HeldEffects, MobAi, RemoteFact, Shown, SpawnAi, Strike, Summon, Surroundings,
    TaggedMob, TreeMind, floor_line, height_after_move,
};
use crate::objectives::KillTypes;

/// A mob farther than this from its spawn does not fall asleep: its tree walks it home first
/// (Observed: a retail beach corrupted sailor 29 m from where it started walked home with
/// `Reset_Move` while the player was 75 m away; live 2026-10-29: Janszoon fell asleep 51 m
/// from his spawn when the player left and stood there). Our engineering limit: past the beach
/// mobs' 6 m `WanderDistanceMax`.
const AWAY_FROM_HOME_M: f32 = 10.0;
/// A player scans for mobs to wake again after moving this far (about 2 s of running); far less
/// than the wake radius, so a mob wakes before the player can reach it.
pub const WAKE_RESCAN_M: f32 = 10.0;
/// The starts of the vitals categories of the encounters that the open map does not run
/// ([`without_unrun_encounters`]): the invasion's mobs (`VitalsCategoryData` `Invasion_Bomber`,
/// `Invasion_Grunt`, `Invasion_Sniper`, ...) and the corrupted breaches' (`Darkness`,
/// `DarknessAI`).
const UNRUN_ENCOUNTER_CATEGORIES: [&str; 2] = ["Invasion_", "Darkness"];

/// The first mob's number after [`MOB_ENTITY_BASE`]. A number whose low 32 bits are all zero
/// gives a `GdeRef` that the client takes but never shows: in every live run the first spawned
/// mob (`MOB_ENTITY_BASE` itself, the tutorial's sailor 1) stayed invisible while it fought,
/// and the mobs after it, with the same card bytes, showed (2026-09-14).
const FIRST_MOB_OFFSET: u64 = 1;
/// The category CRC of a spawn whose spawner names none (the CRC-32 of the empty id).
const NO_SPAWN_CATEGORY: u32 = 0;

/// Mobs per thread below which the minds run on fewer threads (our engineering limit: a thread
/// costs about as much to start as a few hundred mind steps on flat ground, but on the level's
/// floors a step that checks its walk lines costs far more; live hunt 2026-10-31: about 1,000
/// awake mobs ran on one thread at 512, with a mob tick of 33 to 100 ms).
pub const MIN_MOBS_PER_THREAD: usize = 128;

/// How long the grid of the awake mobs for the group aggro serves before it is made again (our
/// engineering limit), and how far a mob may have moved since (a run of 7 m/s for a second,
/// with room).
const AGGRO_GRID_MS: u64 = 1_000;
const AGGRO_GRID_DRIFT_M: f32 = 10.0;

/// The attack factor of a mob that hits with its damage windows: the window's damage table row
/// carries the attack (Observed: the measured tutorial paces fit `DmgCoef` of their rows within
/// 1 %: sailor 0.2968 against `Damned_1H_Club_Light` 0.3; with the island's armor share 0.5612
/// against the config's 0.6006, legionnaire 0.3057 against `Legionnaire_LightStab` 0.33, risen
/// 0.2319 against `Attack` 0.25, swarmer 0.3236 against `Attack_Crawler_Swipe` 0.35, Thorpe
/// 0.2604 against `Attack_Stab_02_P1` 0.28).
pub const ROW_CARRIES_ATTACK: f64 = 1.0;

/// The entity number of the mob of spawn `index`.
pub const fn mob_entity(index: u32) -> u64 {
    MOB_ENTITY_BASE + FIRST_MOB_OFFSET + index as u64
}

/// The spawn index of a mob's entity number; `None` for a player's number.
pub fn spawn_index(entity: u64) -> Option<u32> {
    entity
        .checked_sub(MOB_ENTITY_BASE + FIRST_MOB_OFFSET)
        .and_then(|index| u32::try_from(index).ok())
}

/// What a mob thinks with.
#[derive(Debug, Clone)]
enum Mind {
    /// Its slice's behavior tree.
    Tree(Box<TreeMind>),
    /// The measured pace of a tutorial mob without a tree.
    Chase(Box<ChaseBrain>),
    /// No tree and no measured script: it stands.
    Still,
}

/// The grid actions of the Patrol sub-tree's waypoint tasks (`POI_Waypoint_Loop_CAGE_Task`,
/// `POI_Waypoint_CAGE_Task`): they play the waypoint's task as their fragment (Observed: retail
/// boars sent fragment `POI_Graze_LOOP` with action `POI_Waypoint_Loop`, newchar 2026-09-18).
const WAYPOINT_TASK_ACTIONS: [&str; 2] = ["POI_Waypoint_Loop", "POI_Waypoint"];

/// One mob.
#[derive(Debug, Clone)]
pub struct Mob {
    /// Where it is.
    pub position: Position,
    /// Its health.
    pub health: Health,
    /// Its attack.
    pub attack: MobAttack,
    mind: Mind,
    /// When it died, while it waits for its respawn.
    dead_since_ms: Option<u64>,
    /// It thinks: it is in the awake set (a flag beside the ordered set, so that the tick finds
    /// the awake mobs in one walk over the live ones).
    awake: bool,
    /// When a player was last near it.
    near_ms: u64,
    /// It stands on the floor under its spawn: the world file's spawn heights are not on it
    /// (live login-world-entry-01: a boar spawned 1.42 m under the terrain stayed in it until
    /// it moved).
    grounded: bool,
    /// Its corpse went (`SimWorld::bury_corpses`): it is not shown until its respawn.
    buried: bool,
}

impl Mob {
    /// Its state for the snapshot.
    pub fn state(&self, ai: &MobAi) -> MobState {
        if self.dead_since_ms.is_some() || self.health.is_dead() {
            return MobState::Dead;
        }
        match &self.mind {
            Mind::Tree(mind) if mind.returning(ai) => MobState::Return,
            Mind::Tree(mind) if mind.attacking() => MobState::Attack,
            Mind::Tree(mind) if mind.target().is_some() => MobState::Chase,
            Mind::Tree(_) | Mind::Still => MobState::Idle,
            Mind::Chase(brain) => brain.state(),
        }
    }

    /// The entity it fights.
    pub fn target(&self) -> Option<u64> {
        match &self.mind {
            Mind::Tree(mind) => mind.target(),
            Mind::Chase(brain) => brain.target(),
            Mind::Still => None,
        }
    }

    /// Whether it walks back to its spawn and takes no damage.
    pub fn returning(&self, ai: &MobAi) -> bool {
        match &self.mind {
            Mind::Tree(mind) => mind.returning(ai),
            Mind::Chase(brain) => brain.returning(),
            Mind::Still => false,
        }
    }

    /// Whether it fights: it has a target.
    pub fn fighting(&self) -> bool {
        match &self.mind {
            Mind::Tree(mind) => mind.target().is_some(),
            Mind::Chase(brain) => brain.fighting(),
            Mind::Still => false,
        }
    }

    /// The tree mind, for a mob that runs a tree.
    pub fn tree(&self) -> Option<&TreeMind> {
        match &self.mind {
            Mind::Tree(mind) => Some(mind),
            Mind::Chase(_) | Mind::Still => None,
        }
    }
}

/// A mob that hits a target this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct MobAttackIntent {
    /// The mob's entity number.
    pub mob: u64,
    /// The target's entity number.
    pub target: u64,
    /// Where the target was when the swing started: the mob aims there.
    pub target_at: Position,
    /// The move it swings, when its pace has moves.
    pub mob_move: Option<MobMove>,
    /// The damage window it hits with, for a mob that runs a tree.
    pub strike: Option<Strike>,
}

/// One of `moves` at random; `None` when there are none.
fn pick(moves: &[MobMove], dice: &mut Dice) -> Option<MobMove> {
    let count = u32::try_from(moves.len()).ok().filter(|count| *count > 0)?;
    moves.get(usize::try_from(dice.below(count)).ok()?).copied()
}

/// What changes of cells did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MobChanges {
    /// Mobs that spawned.
    pub spawned: usize,
    /// Mobs that left with their cell.
    pub removed: usize,
    /// Newly owned spawns without a mob type or level row.
    pub unspawned: usize,
}

/// How far past the distance where a paced mob starts its swing the swing still lands (retail
/// hits landed 0.77 to 2.19 m from the player's body; sailors start at 1.8 m).
pub const SWING_HIT_MARGIN_M: f32 = 1.0;

/// The item tag of the hits of a mob whose rows the tables lack (retail: the sailors' and risen's
/// tag).
pub const DEFAULT_HIT_TAG: u8 = 6;

/// The stamina a blocked swing takes when the mob's weapon row is missing (Inferred: the retail
/// signifier's light stab, a level 1 mob, took 14.6).
pub const DEFAULT_BLOCK_STAMINA: f32 = 14.6;

/// The upright capsule of a body: its radius and height (collision note: the `CharacterComponent`
/// capsules of the creature slices).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capsule {
    /// Its radius.
    pub radius_m: f32,
    /// Its height, from its feet.
    pub height_m: f32,
}

/// The body of a mob without a measured capsule: the legionnaire's, risen's and swarmer's
/// `CharacterComponent` capsule (collision note, section 2). The capsules of other slices are not
/// in the AI data (their `CharacterComponent` is not read): Unknown.
pub const DEFAULT_MOB_CAPSULE: Capsule = Capsule {
    radius_m: 0.5,
    height_m: 1.8,
};

/// How a mob's swing reaches its target (collision note, section 3: each retail creature lunged
/// forward in its swing, and every retail mob hit landed 0.79 to 1.65 m from the player).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobReach {
    /// The longest lunge.
    pub lunge_max_m: f32,
    /// It lunges only at a target within this angle of where it aimed at the swing start
    /// (radians).
    pub lunge_turn_rad: f32,
    /// How far the hit shape reaches: from the body's middle for a pace, from the front of the
    /// body for a damage window.
    pub reach_m: f32,
}

/// One full-body fragment of a creature: its fragment and action numbers (its controller
/// definitions) and the last byte of a retail entry of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveFragment {
    /// The fragment number.
    pub fragment: u16,
    /// The action number.
    pub action: u16,
    /// The last byte of the retail entry.
    pub tail: u8,
}

/// One hit of a mob move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveHit {
    /// When it lands, after the move starts.
    pub at_ms: u64,
    /// Its `DamageData` row, in the table of its pace's attack ([`PaceAttack::table`]): its
    /// coefficient, damage type, affix and `BlockStaminaDmgMod`.
    pub row: &'static str,
}

/// The attack of a paced mob: its damage row, of the damage table its slice names, and the
/// weapon item its slice's paperdoll holds, whose `BlockStaminaDamage` a blocked hit takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaceAttack {
    /// The unique id of the damage table.
    pub table: &'static str,
    /// The row of its plain swing, and the row its pace's attack factor was fitted to.
    pub row: &'static str,
    /// The weapon item.
    pub weapon: &'static str,
}

/// An attack that a mob picks from its pace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobMove {
    /// The fragment it plays.
    pub fragment: MoveFragment,
    /// How long the fragment plays.
    pub length_ms: u64,
    /// Its hits.
    pub hits: &'static [MoveHit],
}

/// The second phase of a boss.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseTwo {
    /// The moves it picks from then.
    pub moves: &'static [MobMove],
    /// Its damage then, as a multiple of the first phase's.
    pub damage_mult: f64,
}

/// How a tutorial mob without a tree fights: its pace, measured from the retail tutorial. A
/// paced mob stands on the floor height of its spawn point; the height map does not move it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// Its attack factor.
    pub attack_factor: f64,
    /// Time between two swings: a random time in this range, rolled after each swing. Its first
    /// swing also waits the shortest gap after its fight starts.
    pub swing_gap_ms: (u64, u64),
    /// Its chase speed.
    pub speed_mps: f32,
    /// When a swing hits, after it starts.
    pub hit_delay_ms: u64,
    /// Its attack: the damage row of its swings and the weapon a block's stamina comes from.
    pub attack: PaceAttack,
    /// How it moves and when it swings.
    pub gait: Gait,
    /// How far it sees.
    pub sight_m: f32,
    /// The item scale of its hits on a player: the share of the raw damage that the player's
    /// armor let through.
    pub hit_scale: f32,
    /// Its body.
    pub capsule: Capsule,
    /// How its swing reaches; `None`: it lands within its gait's swing start plus
    /// [`SWING_HIT_MARGIN_M`].
    pub reach: Option<MobReach>,
    /// The attacks it picks from, at random; empty: one swing of [`Pace::hit_delay_ms`]. With
    /// moves, the swing gap is the time after a move's end.
    pub moves: &'static [MobMove],
    /// Its second phase, for a boss.
    pub phase_two: Option<PhaseTwo>,
}

/// A mob spawned for one player.
#[derive(Debug, Clone)]
struct Personal {
    spawn: VitalsSpawn,
    owner: u64,
    /// The mob sees no player until its fight starts.
    held: bool,
    pace: Option<Pace>,
    /// The gap before its next swing, when paced.
    swing_gap_ms: u64,
    /// Until this time the mob does nothing (a scripted pause).
    frozen_until_ms: u64,
    /// Hits do no damage until this time.
    absorb_until_ms: u64,
    /// It fights in its pace's second phase.
    in_phase_two: bool,
    /// The slice asset of its retail card, when its vitals id has more than one look.
    asset: Option<[u8; ASSET_ID_LEN]>,
    /// The leash of its chase: its slice's `CombatLeashDistance`.
    leash_m: f32,
}

/// The mobs of one sim.
pub struct Mobs {
    spawns: Arc<[VitalsSpawn]>,
    /// Personal mobs by index; their indexes follow the spawn list.
    personal: FxHashMap<u32, Personal>,
    /// GM mobs by index, with their spawns; their indexes follow the spawn list too.
    gm: BTreeMap<u32, VitalsSpawn>,
    /// Mobs a tree spawned (`ExecuteSpawn`) by index; their indexes follow the spawn list too.
    summoned: FxHashMap<u32, Summoned>,
    /// The design overrides of each spawn of the spawn list (empty: none).
    overrides: Arc<[Option<SpawnAi>]>,
    next_personal: u32,
    data: Arc<CombatData>,
    ai: Arc<MobAi>,
    wake_radius_m: f32,
    sleep_after_ms: u64,
    respawn_ms: u64,
    /// Looked up many times per awake mob on every tick, so with a fast hash.
    live: FxHashMap<u32, Mob>,
    owned: FxHashSet<u32>,
    awake: BTreeSet<u32>,
    spawn_grid: Option<SpatialGrid<u32>>,
    /// The awake mobs by place for the group aggro, and when it was made.
    aggro_grid: Option<(u64, SpatialGrid<u32>)>,
    dice: Dice,
    brain_runs: u64,
    /// Where each player last scanned for mobs to wake.
    scanned: HashMap<u64, Position>,
    wake_scans: u64,
    /// Mobs that left since the last [`Mobs::take_left`]: removed, or gone with their cell.
    left: Vec<u64>,
    /// Mobs that respawned since the last [`Mobs::take_respawned`].
    respawned: Vec<u64>,
    /// Mobs that fell asleep since the last [`Mobs::take_slept`].
    slept: Vec<u64>,
    /// The encounter events the mobs' trees sent since the last
    /// [`Mobs::take_encounter_events`]: (mob, event id).
    encounter_events: Vec<(u64, String)>,
    /// The slice triggers the mobs ran since the last [`Mobs::take_triggers`]: (mob, trigger).
    triggers: Vec<(u64, SliceTriggerRef)>,
    /// The status effects the sim holds on each mob ([`Mobs::set_held_effects`]).
    held: FxHashMap<u64, HeldEffects>,
    /// The status effects the mobs' trees put on them (`true`) or took off since the last
    /// [`Mobs::take_status_changes`]: (mob, effect id, put on).
    status_changes: Vec<(u64, String, bool)>,
}

/// Metres between mobs spawned together (a GM command, a tree's summon), on a ring around the
/// point.
pub(crate) const SPAWN_SPACING_M: f32 = 2.0;

/// `count` points on a ring around (`x`, `y`), [`SPAWN_SPACING_M`] apart; one point is the
/// centre itself.
pub(crate) fn ring(x: f32, y: f32, count: u32) -> Vec<(f32, f32)> {
    if count == 1 {
        return vec![(x, y)];
    }
    let steps = count as f32;
    let radius = SPAWN_SPACING_M * steps / std::f32::consts::TAU;
    (0..count)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / steps;
            (x + radius * angle.cos(), y + radius * angle.sin())
        })
        .collect()
}

/// The spawns that a sim of `level` spawns. Every mob of the tutorial is a fight of its quest,
/// which the tutorial script spawns for each player (its level file also lists the island's
/// fights as plain spawns); every other level keeps all its spawns.
pub fn spawns_of_level(level: &str, spawns: &[VitalsSpawn]) -> Arc<[VitalsSpawn]> {
    if level == TUTORIAL_FOLDER {
        return Arc::from([]);
    }
    spawns.iter().cloned().collect()
}

/// Which spawns a sim leaves to others.
#[derive(Debug, Clone, Copy)]
pub struct SpawnFilter<'a> {
    /// An instance sim whose level has a dungeon script: the script spawns its encounters' mobs
    /// as their stages run, so the encounter spawns (`IsEncounter`) are left out.
    pub scripted: bool,
    /// The open map: the encounter spawns of the encounters it does not run are left out
    /// (see [`is_unrun`]).
    pub open_map: bool,
    /// The vitals categories, for the open map's rule.
    pub kill_types: &'a KillTypes,
}

/// The spawns of `spawns` that `filter` keeps, with their design overrides. `ai` holds the
/// overrides of `spawns` in spawn order ([`crate::mob_ai::resolve_ai`]) and a mob finds its own
/// by its spawn index ([`Mobs::set_ai`]), so a spawn left out takes its overrides with it: else
/// every later mob runs the profile and patrol path of another spawn (live hunt 2026-10-31: mobs
/// at their spawn reset their fight on every think, their path 3.7 to 4.6 km away).
pub fn sim_spawns(
    spawns: &[VitalsSpawn],
    ai: &[Option<SpawnAi>],
    filter: SpawnFilter<'_>,
) -> (Arc<[VitalsSpawn]>, Arc<[Option<SpawnAi>]>) {
    let (kept, overrides): (Vec<VitalsSpawn>, Vec<Option<SpawnAi>>) = spawns
        .iter()
        .enumerate()
        .filter(|(_, spawn)| {
            let scripted = filter.scripted && spawn.is_encounter;
            let unrun = filter.open_map && is_unrun(spawn, filter.kill_types);
            !(scripted || unrun)
        })
        .map(|(index, spawn)| (spawn.clone(), ai.get(index).cloned().flatten()))
        .unzip();
    (kept.into(), overrides.into())
}

/// Whether `spawn` is an encounter spawn of an encounter that the open map does not run (a
/// vitals row in an `UNRUN_ENCOUNTER_CATEGORIES` category). Observed (data, build 25810808),
/// each an encounter spawn and named by no task:
/// - the invasion's 29,235: the forts' siege encounters (`Siege_Fort_BrimstoneSands`, ...), up
///   to 2,226 at one point; the open map runs no invasion (Inferred: a war game mode of its own).
/// - the breaches' 29,664: each place of a breach (`Enc_Darkness_...`) holds its mob seven
///   times, once for each tier of levels 25 to 70 (`corruption_swarmer_dt1` to `_dt6`, `_dt1`
///   twice), of which one runs at a time (Inferred); the open map runs no breach.
///
/// With the categories that start so, 59,214 of the open map's 115,965 spawns are left out (sim
/// log 2026-10-31). Awake, one fort's 2,226 ran the sim's mob tick to 175 ms (live hunt). Retail
/// showed no mob of an encounter spawn either: general-other-01, 711 to 732 s, the player within
/// 25 m of the `loot_goblin` of `Enc_WorldEvent_LootGoblin_00` and no goblin in the capture.
fn is_unrun(spawn: &VitalsSpawn, kill_types: &KillTypes) -> bool {
    spawn.is_encounter
        && kill_types
            .of(spawn.vitals_id)
            .is_some_and(|(_, categories)| {
                categories
                    .iter()
                    .any(|category| is_unrun_encounter(category))
            })
}

/// Whether vitals category `category` starts with one of `UNRUN_ENCOUNTER_CATEGORIES` (case
/// aside).
fn is_unrun_encounter(category: &str) -> bool {
    UNRUN_ENCOUNTER_CATEGORIES.iter().any(|start| {
        category
            .get(..start.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(start))
    })
}

fn position_of(spawn: &VitalsSpawn) -> Position {
    let [x, y, z] = spawn.position;
    Position { x, y, z }
}

impl MobChanges {
    /// Adds the counts of `other`.
    pub const fn add(&mut self, other: Self) {
        self.spawned += other.spawned;
        self.removed += other.removed;
        self.unspawned += other.unspawned;
    }
}

/// What one mob did in a tick.
struct Step {
    index: u32,
    hits: Vec<MobAttackIntent>,
    acquired: Option<u64>,
    sleeps: bool,
    /// Its tree ran.
    ticked: bool,
    /// The encounter events its tree sent.
    encounter_events: Vec<NameId>,
    /// The mobs its tree spawned.
    summons: Vec<Summon>,
    /// The slice triggers it ran.
    triggers: Vec<SliceTriggerRef>,
    /// The triggers it ran on other mobs: (mob, trigger name).
    remote_triggers: Vec<(u64, NameId)>,
    /// The facts it set on other mobs.
    remote_facts: Vec<RemoteFact>,
    /// The orders it gave other mobs: (mob, order).
    orders: Vec<(u64, u32)>,
    /// The status effects its tree put on it or took off.
    status_changes: Vec<(NameId, bool)>,
}

/// A mob that a tree spawned.
#[derive(Debug, Clone, PartialEq)]
struct Summoned {
    spawn: VitalsSpawn,
    /// The creature slice it shows (its tree's `ExecuteSpawn` slice).
    asset: [u8; ASSET_ID_LEN],
}

/// What every mind reads in a tick.
struct TickView<'a> {
    ai: &'a MobAi,
    players: &'a SpatialGrid<u64>,
    personal: &'a FxHashMap<u32, Personal>,
    ground: &'a (dyn Ground + Sync),
    wake_m: f32,
    now_ms: u64,
    /// The mobs with networked tags.
    tagged: &'a [TaggedMob],
    /// The status effects on the mobs.
    held: &'a FxHashMap<u64, HeldEffects>,
}

impl TickView<'_> {
    /// The players mob `index` at `at` may take as targets: those within the wake radius; a
    /// personal mob's owner only, once it is not held.
    fn seen(&self, index: u32, at: Position) -> Vec<Seen> {
        let personal = self.personal.get(&index);
        self.players
            .near(at)
            .filter(|(_, position)| position.horizontal_distance(at) <= self.wake_m)
            .filter(|(entity, _)| personal.is_none_or(|mob| !mob.held && mob.owner == *entity))
            .map(|(entity, position)| Seen { entity, position })
            .collect()
    }

    /// One step of the tree mob `index`.
    fn think(&self, index: u32, mob: &mut Mob) -> Step {
        let mut step = Step {
            index,
            hits: Vec::new(),
            acquired: None,
            sleeps: false,
            ticked: false,
            encounter_events: Vec::new(),
            summons: Vec::new(),
            triggers: Vec::new(),
            remote_triggers: Vec::new(),
            remote_facts: Vec::new(),
            orders: Vec::new(),
            status_changes: Vec::new(),
        };
        if let Mind::Tree(mind) = &mob.mind
            && !mind.due(self.now_ms)
        {
            return step;
        }
        let seen = self.seen(index, mob.position);
        step.sleeps = seen.is_empty();
        if !step.sleeps {
            mob.near_ms = self.now_ms;
        }
        let Mind::Tree(mind) = &mut mob.mind else {
            return step;
        };
        if mind.target().is_some() {
            mob.near_ms = self.now_ms;
        }
        let around = Surroundings {
            now_ms: self.now_ms,
            seen: &seen,
            ground: self.ground,
            // A share of a health bar.
            #[allow(clippy::cast_precision_loss, reason = "a share of a health bar")]
            health_share: mob.health.current() as f32 / mob.health.max().max(1) as f32,
            entity: mob_entity(index),
            tagged: self.tagged,
            held: self.held.get(&mob_entity(index)),
        };
        if !mob.grounded {
            mob.position.z = height_after_move(self.ground, mob.position);
            mob.grounded = true;
        }
        let thought = mind.step(self.ai, &mut mob.position, &around);
        step.sleeps &= mind.target().is_none();
        if thought.healed {
            mob.health.refill();
            tracing::debug!(mob = mob_entity(index), "mob healed at the end of its reset");
        }
        step.acquired = thought.acquired;
        step.ticked = thought.ticked;
        step.encounter_events = thought.encounter_events;
        step.summons = thought.summons;
        step.triggers = thought.triggers;
        step.remote_triggers = thought.remote_triggers;
        step.remote_facts = thought.remote_facts;
        step.orders = thought.orders;
        step.status_changes = mind.take_status_changes();
        step.hits = thought
            .hits
            .into_iter()
            .map(|hit| MobAttackIntent {
                mob: mob_entity(index),
                target: hit.target,
                target_at: hit.target_at,
                mob_move: None,
                strike: Some(hit.strike),
            })
            .collect();
        step
    }
}

impl Mobs {
    /// No mob yet; the spawns of the cells come with [`Mobs::set_cells`].
    pub fn new(spawns: Arc<[VitalsSpawn]>, combat: &CombatSettings) -> Self {
        let next_personal = u32::try_from(spawns.len()).unwrap_or(u32::MAX);
        Self {
            spawns,
            personal: FxHashMap::default(),
            gm: BTreeMap::new(),
            summoned: FxHashMap::default(),
            overrides: Arc::from([]),
            next_personal,
            data: Arc::clone(&combat.data),
            ai: Arc::clone(&combat.ai),
            wake_radius_m: combat.wake_radius_m,
            sleep_after_ms: combat.sleep_after_ms,
            respawn_ms: combat.mob_respawn_ms,
            live: FxHashMap::default(),
            owned: FxHashSet::default(),
            awake: BTreeSet::new(),
            spawn_grid: None,
            aggro_grid: None,
            dice: Dice::new(combat.seed),
            brain_runs: 0,
            scanned: HashMap::new(),
            wake_scans: 0,
            left: Vec::new(),
            respawned: Vec::new(),
            slept: Vec::new(),
            encounter_events: Vec::new(),
            triggers: Vec::new(),
            held: FxHashMap::default(),
            status_changes: Vec::new(),
        }
    }

    /// The shared AI.
    pub fn ai(&self) -> &MobAi {
        &self.ai
    }

    /// The mobs that left since the last call (removed, or gone with their cell), by entity
    /// number; their ids end with them.
    pub fn take_left(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.left)
    }

    /// The mobs that respawned since the last call, by entity number: each is a new spawn with a
    /// new id and card, as on retail (GDE id note).
    pub fn take_respawned(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.respawned)
    }

    /// The mobs that fell asleep since the last call, by entity number: when a player comes near
    /// again each spawns afresh for its clients, as on retail (mobs note 2026-10-31: a beach
    /// sailor left alone showed the `Spawn` fragment again when the player came back, 960.76 s,
    /// with no death; Inferred: its id and place stay).
    pub fn take_slept(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.slept)
    }

    /// The encounter events the mobs' trees sent since the last call: (mob, event id).
    pub fn take_encounter_events(&mut self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.encounter_events)
    }

    /// The slice triggers the mobs ran since the last call: (mob, trigger).
    pub fn take_triggers(&mut self) -> Vec<(u64, SliceTriggerRef)> {
        std::mem::take(&mut self.triggers)
    }

    /// The archetype of a mob of `spawn` shown with slice asset `asset` (else its vitals id's
    /// most seen look).
    fn archetype(&self, spawn: &VitalsSpawn, asset: Option<[u8; ASSET_ID_LEN]>) -> Option<u32> {
        self.ai.archetype_for(spawn.vitals_id, asset)
    }

    /// The mob of spawn `index` (`spawn`, shown with `asset`), or `None` without its rows.
    fn make(
        &self,
        index: u32,
        spawn: &VitalsSpawn,
        asset: Option<[u8; ASSET_ID_LEN]>,
    ) -> Option<Mob> {
        let kind = self.data.mob_type(spawn.vitals_id)?;
        let level = self.data.level(spawn.level)?;
        let at = position_of(spawn);
        let overrides = self.overrides.get(index as usize).and_then(Option::as_ref);
        let mind = self
            .archetype(spawn, asset)
            .and_then(|archetype| {
                // Spawn facing: the world file keeps no spawn rotation (Unknown): north.
                TreeMind::new(&self.ai, archetype, at, 0.0, u64::from(index), overrides)
            })
            .map_or(Mind::Still, |mind| Mind::Tree(Box::new(mind)));
        Some(Mob {
            position: at,
            health: Health::full(mob_max_health(
                level.base_max_health,
                kind.health_mod,
                kind.category_health_mod,
            )),
            attack: MobAttack {
                base_damage: level.base_damage,
                damage_mod: kind.damage_mod,
                factor: ROW_CARRIES_ATTACK,
            },
            mind,
            dead_since_ms: None,
            awake: false,
            near_ms: 0,
            grounded: false,
            buried: false,
        })
    }

    /// Spawns a personal mob of `spawn` for player `owner` at `now_ms`, held idle when `held`,
    /// shown with slice asset `asset` when given; its entity number, or `None` without its rows.
    /// A mob whose slice has a tree runs it; `pace` is the measured script of a tutorial mob
    /// without one (a paced mob that fights at once waits one interval before its first hit).
    pub fn spawn_personal(
        &mut self,
        spawn: VitalsSpawn,
        owner: u64,
        held: bool,
        pace: Option<Pace>,
        asset: Option<[u8; ASSET_ID_LEN]>,
        now_ms: u64,
    ) -> Option<u64> {
        let index = self.next_personal;
        let mut mob = self.make(index, &spawn, asset)?;
        let archetype = self.archetype(&spawn, asset);
        let leash_m = archetype
            .and_then(|archetype| self.ai.config_number(archetype, COMBAT_LEASH_KEY))
            // A leash of tens of metres.
            .map_or(f32::MAX, |leash| leash as f32);
        if let Some(pace) = pace {
            mob.attack.factor = pace.attack_factor;
            let mut brain = ChaseBrain::new(position_of(&spawn), pace.sight_m);
            if !held {
                brain.wait_to_attack(now_ms);
            }
            mob.mind = Mind::Chase(Box::new(brain));
        }
        let swing_gap_ms = pace.map_or(0, |pace| pace.swing_gap_ms.0);
        self.next_personal = index.checked_add(1)?;
        self.live.insert(index, mob);
        self.wake_index(index);
        self.personal.insert(
            index,
            Personal {
                spawn,
                owner,
                held,
                pace,
                swing_gap_ms,
                frozen_until_ms: 0,
                absorb_until_ms: 0,
                in_phase_two: false,
                asset,
                leash_m,
            },
        );
        Some(mob_entity(index))
    }

    /// The design overrides of the spawn list, in spawn order; set before the first cells.
    pub fn set_ai(&mut self, overrides: Arc<[Option<SpawnAi>]>) {
        self.overrides = overrides;
    }

    /// Spawns a GM mob of `spawn`; its entity number, or `None` without its rows.
    pub fn spawn_gm(&mut self, spawn: VitalsSpawn) -> Option<u64> {
        let index = self.next_personal;
        let mob = self.make(index, &spawn, None)?;
        self.next_personal = index.checked_add(1)?;
        self.live.insert(index, mob);
        self.wake_index(index);
        self.gm.insert(index, spawn);
        Some(mob_entity(index))
    }

    /// Removes the GM mob `entity`; `false` when it is not one.
    pub fn remove_gm(&mut self, entity: u64) -> bool {
        let Some(index) = spawn_index(entity).filter(|index| self.gm.remove(index).is_some())
        else {
            return false;
        };
        self.live.remove(&index);
        self.awake.remove(&index);
        self.left.push(entity);
        true
    }

    /// Every live mob (spawned, alive or waiting for its respawn), by entity number.
    pub fn live_entities(&self) -> Vec<u64> {
        self.live.keys().map(|index| mob_entity(*index)).collect()
    }

    /// The GM mobs, by entity number.
    pub fn gm_mobs(&self) -> Vec<u64> {
        self.gm.keys().map(|index| mob_entity(*index)).collect()
    }

    /// The GM mobs whose spawn point `owns` refuses.
    pub fn gm_mobs_outside(&self, owns: impl Fn(Position) -> bool) -> Vec<u64> {
        self.gm
            .iter()
            .filter(|(_, spawn)| !owns(position_of(spawn)))
            .map(|(index, _)| mob_entity(*index))
            .collect()
    }

    /// Lets the held personal mobs of `owner` see it at `now_ms`; a paced one waits one interval
    /// before its first hit.
    pub fn release(&mut self, owner: u64, now_ms: u64) {
        for (index, personal) in self
            .personal
            .iter_mut()
            .filter(|(_, mob)| mob.owner == owner && mob.held)
        {
            personal.held = false;
            if let Some(Mob {
                mind: Mind::Chase(brain),
                ..
            }) = self.live.get_mut(index)
            {
                brain.wait_to_attack(now_ms);
            }
        }
    }

    /// Removes the personal mob `entity`.
    pub fn remove_personal(&mut self, entity: u64) {
        if let Some(index) = spawn_index(entity)
            && self.personal.remove(&index).is_some()
        {
            self.live.remove(&index);
            self.awake.remove(&index);
            self.left.push(entity);
        }
    }

    /// The owner of mob `entity` when it is personal.
    pub fn owner(&self, entity: u64) -> Option<u64> {
        self.personal
            .get(&spawn_index(entity)?)
            .map(|personal| personal.owner)
    }

    /// The slice asset of personal mob `entity`'s retail card, when it has one.
    pub fn asset(&self, entity: u64) -> Option<[u8; ASSET_ID_LEN]> {
        let index = spawn_index(entity)?;
        self.personal
            .get(&index)
            .and_then(|personal| personal.asset)
            .or_else(|| self.summoned.get(&index).map(|summoned| summoned.asset))
    }

    /// The state of mob `entity`, when it lives here.
    pub fn state(&self, entity: u64) -> Option<MobState> {
        let index = spawn_index(entity)?;
        let mob = self.live.get(&index)?;
        let state = mob.state(&self.ai);
        Some(if state == MobState::Dead || self.awake.contains(&index) {
            state
        } else {
            MobState::Asleep
        })
    }

    /// Whether mob `entity` fights a target, when it lives here.
    pub fn fighting(&self, entity: u64) -> Option<bool> {
        Some(self.live.get(&spawn_index(entity)?)?.fighting())
    }

    /// The tree mind of mob `entity`, when it runs a tree.
    pub fn tree(&self, entity: u64) -> Option<&TreeMind> {
        self.live.get(&spawn_index(entity)?)?.tree()
    }

    /// What mob `entity` plays, with the names of its action and fragment, when it runs a tree
    /// and plays an action.
    pub fn shown(&self, entity: u64) -> Option<(Shown, &str, &str)> {
        let mind = self.tree(entity)?;
        let shown = mind.shown()?;
        let action = self.ai.cage_action(shown.action)?;
        let action_name = self.ai.name(action.action);
        let fragment = mind
            .waypoint_task()
            .filter(|_| WAYPOINT_TASK_ACTIONS.contains(&action_name))
            .unwrap_or(action.fragment);
        Some((shown, action_name, self.ai.name(fragment)))
    }

    /// Hits on personal mob `entity` do no damage until `until_ms`.
    pub fn absorb(&mut self, entity: u64, until_ms: u64) {
        if let Some(personal) = spawn_index(entity).and_then(|index| self.personal.get_mut(&index))
        {
            personal.absorb_until_ms = until_ms;
        }
    }

    /// Whether hits on mob `entity` do no damage at `now_ms`.
    pub fn absorbing(&self, entity: u64, now_ms: u64) -> bool {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index))
            .is_some_and(|personal| now_ms < personal.absorb_until_ms)
    }

    /// Personal mob `entity` stands still and does nothing until `until_ms`.
    pub fn freeze(&mut self, entity: u64, until_ms: u64) {
        if let Some(personal) = spawn_index(entity).and_then(|index| self.personal.get_mut(&index))
        {
            personal.frozen_until_ms = until_ms;
        }
    }

    /// When a swing of mob `entity` hits, after the swing starts: its pace's delay, else 0 (a
    /// damage window hits when it comes).
    pub fn hit_delay_ms(&self, entity: u64) -> u64 {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index)?.pace)
            .map_or(0, |pace| pace.hit_delay_ms)
    }

    /// The attack of mob `entity`, when it has a pace.
    pub fn attack(&self, entity: u64) -> Option<PaceAttack> {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index)?.pace)
            .map(|pace| pace.attack)
    }

    /// Personal mob `entity` fights in its pace's second phase from now on.
    pub fn enter_phase_two(&mut self, entity: u64) {
        if let Some(personal) = spawn_index(entity).and_then(|index| self.personal.get_mut(&index))
        {
            personal.in_phase_two = true;
        }
    }

    /// The second phase of mob `entity`, while it fights in it.
    pub fn phase_two(&self, entity: u64) -> Option<PhaseTwo> {
        let personal = self.personal.get(&spawn_index(entity)?)?;
        personal.pace?.phase_two.filter(|_| personal.in_phase_two)
    }

    /// The body of mob `entity`: its pace's, else [`DEFAULT_MOB_CAPSULE`].
    pub fn capsule(&self, entity: u64) -> Capsule {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index)?.pace)
            .map_or(DEFAULT_MOB_CAPSULE, |pace| pace.capsule)
    }

    /// How the swing of mob `entity` reaches, when its pace says.
    pub fn reach(&self, entity: u64) -> Option<MobReach> {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index)?.pace)
            .and_then(|pace| pace.reach)
    }

    /// Mob `entity` lunges up to `max_m` toward `to` in its swing, inside its leash; where it is
    /// then.
    pub fn lunge(&mut self, entity: u64, to: Position, max_m: f32) -> Option<Position> {
        let index = spawn_index(entity)?;
        let (spawn, leash_m) = match self.personal.get(&index) {
            Some(personal) => (position_of(&personal.spawn), personal.leash_m),
            None => {
                let spawn = self.spawn_of(index)?;
                let leash = self.leash_of(spawn);
                (position_of(spawn), leash)
            }
        };
        let mob = self.live.get_mut(&index)?;
        mob.position = walk_within(mob.position, to, max_m, spawn, leash_m);
        Some(mob.position)
    }

    /// Mob `entity` lunges as [`Self::lunge`] over `ground`: its feet follow the floor and a wall
    /// or a step too high stops the lunge where it started (live 2026-10-29: Janszoon's charge
    /// up the slope at La Roussette ran half under the ground, the lunge kept his start height).
    /// A paced mob keeps its height, as its chase does.
    pub fn lunge_over(
        &mut self,
        entity: u64,
        to: Position,
        max_m: f32,
        ground: &dyn Ground,
    ) -> Option<Position> {
        let index = spawn_index(entity)?;
        let from = self.live.get(&index)?.position;
        let end = self.lunge(entity, to, max_m)?;
        if self
            .personal
            .get(&index)
            .is_some_and(|personal| personal.pace.is_some())
        {
            return Some(end);
        }
        let landed = floor_line(ground, from, end, None).map_or(from, |on_floor| Position {
            z: height_after_move(ground, on_floor),
            ..on_floor
        });
        let mob = self.live.get_mut(&index)?;
        mob.position = landed;
        Some(landed)
    }

    /// The combat leash of the slice of `spawn`; no limit without one.
    fn leash_of(&self, spawn: &VitalsSpawn) -> f32 {
        self.archetype(spawn, None)
            .and_then(|archetype| self.ai.config_number(archetype, COMBAT_LEASH_KEY))
            // A leash of tens of metres.
            .map_or(f32::MAX, |leash| leash as f32)
    }

    /// How far from paced mob `entity` its swing lands on a player: where its gait starts a
    /// swing, plus [`SWING_HIT_MARGIN_M`]; `None` for a mob without a pace.
    pub fn hit_reach_m(&self, entity: u64) -> Option<f32> {
        let pace = self.personal.get(&spawn_index(entity)?)?.pace?;
        Some(pace.gait.swing_start_m + SWING_HIT_MARGIN_M)
    }

    /// The item scale of the hits of mob `entity`: its pace's, else `default_scale`.
    pub fn hit_scale(&self, entity: u64, default_scale: f32) -> f32 {
        spawn_index(entity)
            .and_then(|index| self.personal.get(&index)?.pace)
            .map_or(default_scale, |pace| pace.hit_scale)
    }

    /// The level of mob `entity`, when it lives here.
    pub fn level(&self, entity: u64) -> Option<u32> {
        self.spawn_of(spawn_index(entity)?).map(|spawn| spawn.level)
    }

    /// The `vitals_id` (CRC-32 of the lowercase `VitalsID`) of mob `entity`, when it lives here.
    pub fn vitals_of(&self, entity: u64) -> Option<u32> {
        self.spawn_of(spawn_index(entity)?)
            .map(|spawn| spawn.vitals_id)
    }

    /// The spawn category of mob `entity` (CRC-32 of the lowercase id its spawner names,
    /// `Barbary_Lion_16BS12`); `None` for a spawn with none.
    pub fn category_of(&self, entity: u64) -> Option<u32> {
        self.spawn_of(spawn_index(entity)?)
            .map(|spawn| spawn.category_id)
            .filter(|category| *category != NO_SPAWN_CATEGORY)
    }

    /// The entity mob `entity` chases or hits, when it lives here.
    pub fn target(&self, entity: u64) -> Option<u64> {
        self.live.get(&spawn_index(entity)?)?.target()
    }

    /// The health of mob `entity`, when it lives here.
    pub fn health(&self, entity: u64) -> Option<u32> {
        self.live
            .get(&spawn_index(entity)?)
            .map(|mob| mob.health.current())
    }

    /// Where mob `entity` stands and the point of its spawn, when it lives here.
    pub fn places(&self, entity: u64) -> Option<(Position, Position)> {
        let index = spawn_index(entity)?;
        let at = self.live.get(&index)?.position;
        Some((at, position_of(self.spawn_of(index)?)))
    }

    /// The spawn of mob `index`: a world spawn, a personal, GM or summoned one.
    fn spawn_of(&self, index: u32) -> Option<&VitalsSpawn> {
        self.spawns
            .get(index as usize)
            .or_else(|| self.personal.get(&index).map(|personal| &personal.spawn))
            .or_else(|| self.gm.get(&index))
            .or_else(|| self.summoned.get(&index).map(|summoned| &summoned.spawn))
    }

    /// Mob `owner`'s tree spawned `summon`: its mobs, of the summoner's level, stand at the
    /// summon's place (else the summoner's) on a ring, awake. A slice without a vitals id in the
    /// mob prefab table (no retail card of it) spawns nothing: the client could not show it.
    /// A summoned mob never respawns; it goes with its corpse, or when it is left alone
    /// (Inferred: no retail capture shows what becomes of the adds of a boss that resets).
    fn summon(&mut self, owner: u32, summon: Summon) {
        let Some(vitals_id) = self.ai.summoned_vitals(&summon.asset) else {
            tracing::debug!(
                owner = mob_entity(owner),
                "summon of a slice without a card"
            );
            return;
        };
        let (Some(level), Some(home)) = (
            self.spawn_of(owner).map(|spawn| spawn.level),
            self.live.get(&owner).map(|mob| mob.position),
        ) else {
            return;
        };
        let at = summon.at.unwrap_or(home);
        for (x, y) in ring(at.x, at.y, summon.count) {
            let spawn = VitalsSpawn {
                vitals_id,
                level,
                position: [x, y, at.z],
                ..VitalsSpawn::default()
            };
            let index = self.next_personal;
            let Some(mob) = self.make(index, &spawn, Some(summon.asset)) else {
                return;
            };
            let Some(next) = index.checked_add(1) else {
                return;
            };
            self.next_personal = next;
            self.live.insert(index, mob);
            self.wake_index(index);
            self.summoned.insert(
                index,
                Summoned {
                    spawn,
                    asset: summon.asset,
                },
            );
        }
    }

    /// Removes summoned mob `index` for good.
    fn unsummon(&mut self, index: u32) {
        if self.summoned.remove(&index).is_some() {
            self.live.remove(&index);
            self.awake.remove(&index);
            self.left.push(mob_entity(index));
        }
    }

    /// Spawns the mobs of the spawns that `owns`, and removes the others.
    pub fn set_cells(&mut self, owns: impl Fn(Position) -> bool) -> MobChanges {
        let mut changes = MobChanges::default();
        self.scanned.clear();
        let spawns = Arc::clone(&self.spawns);
        for (index, spawn) in (0_u32..).zip(spawns.iter()) {
            if owns(position_of(spawn)) {
                if !self.owned.insert(index) {
                    continue;
                }
                match self.make(index, spawn, None) {
                    Some(mob) => {
                        // A patrolling mob walks away from its spawn, where the wake scan looks:
                        // it stays awake.
                        self.live.insert(index, mob);
                        if self.patrols(index) {
                            self.wake_index(index);
                        }
                        changes.spawned += 1;
                    }
                    None => changes.unspawned += 1,
                }
            } else if self.owned.remove(&index) {
                self.awake.remove(&index);
                if self.live.remove(&index).is_some() {
                    changes.removed += 1;
                    self.left.push(mob_entity(index));
                }
            }
        }
        // The buckets hold every mob a player's wake radius can reach: a mob stays within its
        // slice's leash of its spawn.
        let leash_m = self
            .live
            .keys()
            .filter_map(|index| self.spawn_of(*index))
            .map(|spawn| self.leash_of(spawn))
            .filter(|leash| leash.is_finite())
            .fold(0.0_f32, f32::max);
        self.spawn_grid = SpatialGrid::new(self.wake_radius_m + leash_m).map(|mut grid| {
            for index in self.live.keys() {
                if let Some(spawn) = self.spawn_of(*index) {
                    grid.insert(*index, position_of(spawn));
                }
            }
            grid
        });
        changes
    }

    /// Whether the spawn `index` has a design patrol route: a path drawn in a design file, which
    /// may lead far from the spawn. A spawner's waypoints (a route with tasks) stay by the spawn
    /// (Observed: within 7 m on the beach graze and patrol spawners), inside the reach of the
    /// wake scan at the spawn, so such a mob sleeps as the others do.
    fn patrols(&self, index: u32) -> bool {
        self.overrides
            .get(index as usize)
            .and_then(Option::as_ref)
            .and_then(|overrides| overrides.route.as_ref())
            .is_some_and(|route| route.tasks.is_empty())
    }

    /// One tick at `now_ms` with the living players and player ghosts near this sim's mobs, on
    /// up to `threads` threads. Returns the hits the mobs make.
    pub fn tick(
        &mut self,
        players: &[Seen],
        now_ms: u64,
        ground: &(dyn Ground + Sync),
        threads: usize,
    ) -> Vec<MobAttackIntent> {
        let wake_m = self.wake_radius_m;
        let Some(mut player_grid) = SpatialGrid::new(wake_m) else {
            return Vec::new();
        };
        for player in players {
            player_grid.insert(player.entity, player.position);
        }
        self.respawn_due(now_ms);
        self.wake_near(players);
        let tagged = self.tagged_mobs();
        let mut chasers: Vec<u32> = Vec::new();
        let steps: Vec<Step> = {
            let mut trees: Vec<(u32, &mut Mob)> = Vec::new();
            for (index, mob) in &mut self.live {
                if !mob.awake || mob.dead_since_ms.is_some() || mob.health.is_dead() {
                    continue;
                }
                // A stunned paced mob waits as a frozen one (a tree mob's own step holds it,
                // [`TreeMind::step`]).
                let stunned = matches!(mob.mind, Mind::Chase(_))
                    && self
                        .held
                        .get(&mob_entity(*index))
                        .is_some_and(|held| !held.control.may_fight());
                if stunned
                    || self
                        .personal
                        .get(index)
                        .is_some_and(|personal| now_ms < personal.frozen_until_ms)
                {
                    if let Mind::Chase(brain) = &mut mob.mind {
                        // A frozen mob's step clock waits with it (live 2026-09-14: each sailor
                        // jumped 5.5 m in one state after its alert, a second of run at once).
                        brain.stood_until(now_ms);
                    }
                    continue;
                }
                match mob.mind {
                    Mind::Tree(_) | Mind::Still => trees.push((*index, mob)),
                    Mind::Chase(_) => chasers.push(*index),
                }
            }
            self.brain_runs += u64::try_from(chasers.len()).unwrap_or(u64::MAX);
            let view = TickView {
                ai: &self.ai,
                players: &player_grid,
                personal: &self.personal,
                ground,
                wake_m,
                now_ms,
                tagged: &tagged,
                held: &self.held,
            };
            let threads = threads.min(trees.len() / MIN_MOBS_PER_THREAD).max(1);
            if threads == 1 {
                trees
                    .into_iter()
                    .map(|(index, mob)| view.think(index, mob))
                    .collect()
            } else {
                let chunk = trees.len().div_ceil(threads);
                std::thread::scope(|scope| {
                    let handles: Vec<_> = trees
                        .chunks_mut(chunk)
                        .map(|part| {
                            let view = &view;
                            scope.spawn(move || {
                                part.iter_mut()
                                    .map(|(index, mob)| view.think(*index, mob))
                                    .collect::<Vec<Step>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|handle| {
                            handle
                                .join()
                                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                        })
                        .collect()
                })
            }
        };
        let mut intents = Vec::new();
        let mut acquired = Vec::new();
        for step in steps {
            self.brain_runs += u64::from(step.ticked);
            for event in step.encounter_events {
                let name = self.ai.name(event).to_owned();
                self.encounter_events.push((mob_entity(step.index), name));
            }
            for summon in step.summons {
                self.summon(step.index, summon);
            }
            let mob = mob_entity(step.index);
            self.triggers
                .extend(step.triggers.into_iter().map(|trigger| (mob, trigger)));
            for (target, trigger) in step.remote_triggers {
                self.trigger_remote(target, trigger);
            }
            for fact in step.remote_facts {
                if let Some(mind) = self.tree_mut(fact.mob) {
                    mind.set_remote_fact(&fact);
                }
            }
            for (target, order) in step.orders {
                if let Some(mind) = self.tree_mut(target) {
                    mind.assign_order(order);
                }
            }
            for (effect, on) in step.status_changes {
                let effect = self.ai.name(effect).to_owned();
                self.status_changes.push((mob, effect, on));
            }
            intents.extend(step.hits);
            if let Some(target) = step.acquired {
                acquired.push((step.index, target));
            }
            if step.sleeps {
                self.sleep_if_alone(step.index, now_ms);
            }
        }
        self.share_aggro(&acquired, now_ms);
        for index in chasers {
            intents.extend(self.chase(index, &player_grid, wake_m, now_ms));
        }
        intents
    }

    /// The awake living tree mobs whose trees gave them networked tags.
    fn tagged_mobs(&self) -> Vec<TaggedMob> {
        self.awake
            .iter()
            .filter_map(|index| {
                let mob = self.live.get(index)?;
                let mind = mob.tree().filter(|mind| !mind.nw_tags().is_empty())?;
                (mob.dead_since_ms.is_none() && !mob.health.is_dead()).then(|| TaggedMob {
                    entity: mob_entity(*index),
                    position: mob.position,
                    tags: mind.nw_tags().to_vec(),
                })
            })
            .collect()
    }

    /// The tree of living mob `entity`.
    fn tree_mut(&mut self, entity: u64) -> Option<&mut TreeMind> {
        let mob = self.live.get_mut(&spawn_index(entity)?)?;
        if mob.dead_since_ms.is_some() || mob.health.is_dead() {
            return None;
        }
        match &mut mob.mind {
            Mind::Tree(mind) => Some(mind),
            Mind::Chase(_) | Mind::Still => None,
        }
    }

    /// Another mob's tree runs the slice triggers named `trigger` on mob `target`
    /// (`TriggerRemoteEntity`): each goes to the clients that show `target` as a 3135.
    fn trigger_remote(&mut self, target: u64, trigger: NameId) {
        let name = self.ai.name(trigger);
        let found: Vec<SliceTriggerRef> = self
            .live
            .get(&spawn_index(target).unwrap_or(u32::MAX))
            .and_then(Mob::tree)
            .and_then(|mind| self.ai.arch(mind.archetype()))
            .map(|arch| {
                arch.triggers_named(name, |id| self.ai.name(id))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        if found.is_empty() {
            tracing::debug!(
                target,
                trigger = name,
                "remote trigger of no slice component"
            );
        }
        self.triggers
            .extend(found.into_iter().map(|trigger| (target, trigger)));
    }

    /// The status effects the sim holds on each mob, for the mobs' next tick.
    pub fn set_held_effects(&mut self, held: FxHashMap<u64, HeldEffects>) {
        self.held = held;
    }

    /// The status effects the mobs' trees put on them (`true`) or took off since the last call:
    /// (mob, effect id, put on).
    pub fn take_status_changes(&mut self) -> Vec<(u64, String, bool)> {
        std::mem::take(&mut self.status_changes)
    }

    /// Respawns the dead world and GM mobs whose respawn time came.
    fn respawn_due(&mut self, now_ms: u64) {
        let due: Vec<u32> = self
            .live
            .iter()
            .filter(|(index, mob)| {
                !self.personal.contains_key(index)
                    && !self.summoned.contains_key(index)
                    && mob.dead_since_ms.is_some_and(|since| {
                        now_ms.saturating_sub(since) >= self.respawn_ms_of(**index)
                    })
            })
            .map(|(index, _)| *index)
            .collect();
        for index in due {
            let Some(spawn) = self.spawn_of(index).cloned() else {
                continue;
            };
            if let Some(mut mob) = self.make(index, &spawn, None) {
                mob.awake = self.awake.contains(&index);
                self.live.insert(index, mob);
                self.respawned.push(mob_entity(index));
            }
        }
    }

    /// The respawn time of spawn `index`: its design override's, else the sim's.
    fn respawn_ms_of(&self, index: u32) -> u64 {
        self.overrides
            .get(index as usize)
            .and_then(Option::as_ref)
            .and_then(|overrides| overrides.respawn_ms)
            .unwrap_or(self.respawn_ms)
    }

    /// Wakes the mobs near the players that moved far enough since their last scan.
    fn wake_near(&mut self, players: &[Seen]) {
        let wake_m = self.wake_radius_m;
        let mut scanned = HashMap::with_capacity(players.len());
        if let Some(spawn_grid) = &self.spawn_grid {
            for player in players {
                if let Some(last) = self
                    .scanned
                    .get(&player.entity)
                    .filter(|last| last.horizontal_distance(player.position) < WAKE_RESCAN_M)
                {
                    scanned.insert(player.entity, *last);
                    continue;
                }
                scanned.insert(player.entity, player.position);
                self.wake_scans += 1;
                for (index, _) in spawn_grid.near(player.position) {
                    let near = self.live.get(&index).is_some_and(|mob| {
                        mob.position.horizontal_distance(player.position) <= wake_m
                    });
                    if near && let Some(mob) = self.live.get_mut(&index) {
                        mob.awake = true;
                        self.awake.insert(index);
                    }
                }
            }
        }
        self.scanned = scanned;
    }

    /// Mob `index`, alone this tick (no player near, no target), falls asleep after the sleep
    /// time since a player was last near it; personal, GM and patrolling mobs never sleep, and a
    /// living mob stays awake while it is more than [`AWAY_FROM_HOME_M`] from its spawn, so its
    /// tree walks it home.
    fn sleep_if_alone(&mut self, index: u32, now_ms: u64) {
        let away = self.live.get(&index).is_some_and(|mob| {
            mob.dead_since_ms.is_none()
                && self.spawn_of(index).is_some_and(|spawn| {
                    mob.position.horizontal_distance(position_of(spawn)) > AWAY_FROM_HOME_M
                })
        });
        let stays = away
            || self.personal.contains_key(&index)
            || self.gm.contains_key(&index)
            || self.patrols(index);
        if stays {
            return;
        }
        if let Some(mob) = self.live.get_mut(&index)
            && now_ms.saturating_sub(mob.near_ms) >= self.sleep_after_ms
        {
            mob.awake = false;
            self.awake.remove(&index);
            self.slept.push(mob_entity(index));
            self.unsummon(index);
        }
    }

    /// The mobs that took a target this tick share it with the mobs of their slice's group aggro
    /// (the slice's `m_groupAggroDistance` and `m_groupAggroShareCap`: the boar's 20 m and 5;
    /// Inferred: awake tree mobs without a target within that distance take the same target, up
    /// to the cap).
    fn share_aggro(&mut self, acquired: &[(u32, u64)], now_ms: u64) {
        if acquired.is_empty() {
            return;
        }
        let stale = self
            .aggro_grid
            .as_ref()
            .is_none_or(|(made_ms, _)| now_ms.saturating_sub(*made_ms) >= AGGRO_GRID_MS);
        if stale {
            let reach_m = self
                .ai
                .data()
                .archetypes
                .iter()
                .filter_map(|arch| arch.group_aggro_distance)
                .fold(0.0_f32, f32::max)
                + AGGRO_GRID_DRIFT_M;
            self.aggro_grid = SpatialGrid::new(reach_m).map(|mut grid| {
                for index in &self.awake {
                    if let Some(mob) = self.live.get(index) {
                        grid.insert(*index, mob.position);
                    }
                }
                (now_ms, grid)
            });
        }
        let Some((_, grid)) = &self.aggro_grid else {
            return;
        };
        for &(index, target) in acquired {
            let Some((at, distance_m, cap)) = self.live.get(&index).and_then(|mob| {
                let arch = self.ai.arch(mob.tree()?.archetype())?;
                Some((
                    mob.position,
                    arch.group_aggro_distance?,
                    arch.group_aggro_cap?,
                ))
            }) else {
                continue;
            };
            let mut near: Vec<(u32, f32)> = grid
                .near(at)
                .filter(|(other, _)| *other != index)
                .filter_map(|(other, _)| {
                    let mob = self.live.get(&other).filter(|mob| mob.awake)?;
                    let distance = mob.position.horizontal_distance(at);
                    (distance <= distance_m).then_some((other, distance))
                })
                .collect();
            // The nearest first.
            near.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut shared = 0;
            for (other, _) in near {
                if shared >= cap {
                    break;
                }
                if let Some(Mob {
                    mind: Mind::Tree(mind),
                    ..
                }) = self.live.get_mut(&other)
                    && mind.target().is_none()
                {
                    mind.share_target(target);
                    shared += 1;
                }
            }
        }
    }

    /// One step of the paced mob `index`: its swings.
    fn chase(
        &mut self,
        index: u32,
        players: &SpatialGrid<u64>,
        wake_m: f32,
        now_ms: u64,
    ) -> Option<MobAttackIntent> {
        let personal = self.personal.get(&index)?;
        let pace = personal.pace?;
        let settings = ChaseSettings {
            leash_radius_m: personal.leash_m,
            attack_interval_ms: personal.swing_gap_ms,
            run_speed_mps: pace.speed_mps,
            gait: pace.gait,
        };
        let mob = self.live.get_mut(&index)?;
        let at = mob.position;
        let seen: Vec<Seen> = players
            .near(at)
            .filter(|(_, position)| position.horizontal_distance(at) <= wake_m)
            .filter(|(entity, _)| !personal.held && personal.owner == *entity)
            .map(|(entity, position)| Seen { entity, position })
            .collect();
        let Mind::Chase(brain) = &mut mob.mind else {
            return None;
        };
        match brain.step(at, &seen, now_ms, &settings) {
            // A paced mob keeps the floor height of its spawn: the retail tutorial creatures
            // stayed on one floor per fight, a ruin or a deck that the height map does not
            // hold (height note: 0.3 to 6 m above it, Thorpe's arena with no terrain).
            BrainAction::MoveTo(to) => mob.position = Position { z: at.z, ..to },
            BrainAction::Home => {
                mob.position = position_of(&personal.spawn);
                mob.health.refill();
            }
            BrainAction::None => {}
            BrainAction::Attack(target) => {
                let target_at = seen
                    .iter()
                    .find(|player| player.entity == target)
                    .map_or(at, |player| player.position);
                let in_phase_two = personal.in_phase_two;
                let moves = pace
                    .phase_two
                    .filter(|_| in_phase_two)
                    .map_or(pace.moves, |phase| phase.moves);
                let mob_move = pick(moves, &mut self.dice);
                let (shortest, longest) = pace.swing_gap_ms;
                let spread = u32::try_from(longest.saturating_sub(shortest)).unwrap_or(u32::MAX);
                let length_ms = mob_move.map_or(0, |picked: MobMove| picked.length_ms);
                let gap =
                    length_ms + shortest + u64::from(self.dice.below(spread.saturating_add(1)));
                if let Some(personal) = self.personal.get_mut(&index) {
                    personal.swing_gap_ms = gap;
                }
                return Some(MobAttackIntent {
                    mob: mob_entity(index),
                    target,
                    target_at,
                    mob_move,
                    strike: None,
                });
            }
        }
        None
    }

    /// `attacker` at `from` hit mob `entity` with `threat` at `now_ms`: whether the hit counts
    /// (a mob in a leash reset or dead takes none). It wakes.
    pub fn hurt(
        &mut self,
        entity: u64,
        attacker: u64,
        from: Position,
        threat: u64,
        now_ms: u64,
    ) -> bool {
        let Some(index) = spawn_index(entity) else {
            return false;
        };
        let ai = Arc::clone(&self.ai);
        let Some(mob) = self.live.get_mut(&index) else {
            return false;
        };
        if mob.dead_since_ms.is_some() || mob.health.is_dead() {
            return false;
        }
        let counts = match &mut mob.mind {
            Mind::Tree(mind) => mind.hurt(&ai, attacker, from, threat),
            Mind::Chase(brain) => brain.hurt(attacker, threat, now_ms),
            Mind::Still => true,
        };
        if counts {
            mob.awake = true;
            self.awake.insert(index);
        }
        counts
    }

    /// The dead world and GM mobs whose corpse is still shown, with when they died.
    pub fn corpses(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.live.iter().filter_map(|(index, mob)| {
            let died_ms = mob.dead_since_ms.filter(|_| !mob.buried)?;
            (!self.personal.contains_key(index)).then(|| (mob_entity(*index), died_ms))
        })
    }

    /// The corpse of dead mob `entity` goes: it is not shown again until its respawn.
    pub fn bury(&mut self, entity: u64) {
        let Some(index) = spawn_index(entity) else {
            return;
        };
        if let Some(mob) = self
            .live
            .get_mut(&index)
            .filter(|mob| mob.dead_since_ms.is_some())
        {
            mob.buried = true;
            self.unsummon(index);
        }
    }

    /// Mob `entity` died at `now_ms`: it waits for its respawn (a personal mob waits for its
    /// encounter to remove it).
    pub fn died(&mut self, entity: u64, now_ms: u64) {
        let Some(index) = spawn_index(entity) else {
            return;
        };
        let ai = Arc::clone(&self.ai);
        if let Some(mob) = self.live.get_mut(&index) {
            mob.dead_since_ms = Some(now_ms);
            let at = mob.position;
            match &mut mob.mind {
                Mind::Tree(mind) => mind.died(&ai, at, now_ms),
                Mind::Chase(brain) => brain.died(),
                Mind::Still => {}
            }
        }
    }

    /// A hit reaction of `length_ms` of mob `entity` starts at `now_ms`.
    pub fn react(&mut self, entity: u64, now_ms: u64, length_ms: u64) {
        let ai = Arc::clone(&self.ai);
        if let Some(mob) = spawn_index(entity).and_then(|index| self.live.get_mut(&index))
            && let Mind::Tree(mind) = &mut mob.mind
        {
            mind.react(&ai, mob.position, now_ms, length_ms);
        }
    }

    /// Mob `entity` takes `target` as its target when it has none (the tutorial's scripted
    /// pick: retail Thorpe fought the player from his spawn, 26.5 m away).
    pub fn provoke(&mut self, entity: u64, target: u64, now_ms: u64) {
        let ai = Arc::clone(&self.ai);
        if let Some(mob) = spawn_index(entity).and_then(|index| self.live.get_mut(&index)) {
            let at = mob.position;
            match &mut mob.mind {
                Mind::Tree(mind) => {
                    mind.hurt(&ai, target, at, 0);
                }
                Mind::Chase(brain) => {
                    brain.hurt(target, 0, now_ms);
                }
                Mind::Still => {}
            }
        }
    }

    /// Every mob forgets `player` (it left, died or handed off).
    pub fn forget(&mut self, player: u64) {
        for index in &self.awake {
            if let Some(mob) = self.live.get_mut(index) {
                match &mut mob.mind {
                    Mind::Tree(mind) => mind.forget(player),
                    Mind::Chase(brain) => brain.forget(player),
                    Mind::Still => {}
                }
            }
        }
    }

    /// Every awake mob: entity number, position and state (M6-A spec section 6.3, M6-B spec
    /// section 5); the ghosts for other sims and the mobs of the bundles both come from here.
    pub fn awake_states(&self) -> impl Iterator<Item = (u64, Position, MobGhost)> + '_ {
        self.awake.iter().filter_map(|index| {
            let mob = self.live.get(index).filter(|mob| !mob.buried)?;
            let spawn = self.spawn_of(*index)?;
            Some((
                mob_entity(*index),
                mob.position,
                MobGhost {
                    health: mob.health.current(),
                    max_health: mob.health.max(),
                    returning: mob.returning(&self.ai),
                    vitals_id: spawn.vitals_id,
                    category_id: spawn.category_id,
                    level: spawn.level,
                },
            ))
        })
    }

    /// The mob with entity number `entity`.
    pub fn get_mut(&mut self, entity: u64) -> Option<&mut Mob> {
        self.live.get_mut(&spawn_index(entity)?)
    }

    /// The mob with entity number `entity`.
    pub fn get(&self, entity: u64) -> Option<&Mob> {
        self.live.get(&spawn_index(entity)?)
    }

    /// Marks the mob `entity` awake (a hit woke it).
    pub fn wake(&mut self, entity: u64) {
        if let Some(index) = spawn_index(entity) {
            self.wake_index(index);
        }
    }

    /// Mob `index` thinks from now on, when it lives.
    fn wake_index(&mut self, index: u32) {
        if let Some(mob) = self.live.get_mut(&index) {
            mob.awake = true;
            self.awake.insert(index);
        }
    }

    /// Live mobs.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether this sim has no mob.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// The admin snapshot: every awake mob, and mobs by state (admin tools spec section 5), in
    /// one walk over the awake mobs.
    pub fn snapshot(&self) -> (Vec<SnapMob>, [u32; MOB_STATES]) {
        let mut counts = self.asleep_counts();
        let mobs = self
            .awake_mobs()
            .map(|(index, mob)| {
                let state = mob.state(&self.ai);
                tally(&mut counts, state);
                SnapMob {
                    entity: mob_entity(index),
                    x: mob.position.x,
                    y: mob.position.y,
                    z: mob.position.z,
                    state: state_index(state),
                    health: mob.health.current(),
                    max_health: mob.health.max(),
                    target_entity: mob.target(),
                }
            })
            .collect();
        let counts = counts.map(|(_, count)| u32::try_from(count).unwrap_or(u32::MAX));
        (mobs, counts)
    }

    /// Mobs by state, in one walk over the awake mobs.
    pub fn counts(&self) -> [(MobState, usize); MobState::ALL.len()] {
        let mut counts = self.asleep_counts();
        for (_, mob) in self.awake_mobs() {
            tally(&mut counts, mob.state(&self.ai));
        }
        counts
    }

    /// Every state at 0 but the asleep mobs: every live mob that is not awake.
    fn asleep_counts(&self) -> [(MobState, usize); MobState::ALL.len()] {
        let asleep = self.live.len() - self.awake.len();
        MobState::ALL.map(|state| (state, if state == MobState::Asleep { asleep } else { 0 }))
    }

    fn awake_mobs(&self) -> impl Iterator<Item = (u32, &Mob)> + '_ {
        self.awake
            .iter()
            .filter_map(|index| self.live.get(index).map(|mob| (*index, mob)))
    }

    /// Tree runs and paced steps since the start.
    pub const fn brain_runs(&self) -> u64 {
        self.brain_runs
    }

    /// Wake scans of players since the start.
    pub const fn wake_scans(&self) -> u64 {
        self.wake_scans
    }
}

/// Counts one awake mob in `state`; asleep mobs are counted as the mobs that are not awake.
fn tally(counts: &mut [(MobState, usize); MobState::ALL.len()], state: MobState) {
    if state == MobState::Asleep {
        return;
    }
    if let Some((_, count)) = counts.iter_mut().find(|(each, _)| *each == state) {
        *count += 1;
    }
}

/// The position of `state` in [`MobState::ALL`].
fn state_index(state: MobState) -> u8 {
    MobState::ALL
        .iter()
        .position(|each| *each == state)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use nw_content::status_effects::Control;
    use nw_formats::objectstream::lowercase_crc32;
    use nw_sim_core::{Ground, MobState};
    use nw_world::WorldFile;
    use nw_world::records::{AiBehaviour, AiProfile, SpawnAi as DesignSpawn};

    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB};
    use crate::combat::tests::test_combat;
    use crate::mob_ai::test_support::{
        TEST_ATTACK_S, TEST_COOLDOWN_S, TEST_GROUP_AGGRO_M, TEST_HIT_S, TEST_LEASH_M, TEST_MOB_ROW,
        TEST_MOB_TABLE, TEST_REACH_M, TEST_SIGHT_M,
    };
    use crate::mob_ai::{SliceVitals, resolve_ai};

    pub(super) const HEIGHT: f32 = 37.5;
    pub(super) const PLAYER: u64 = 3;
    pub(super) const TICK_MS: u64 = 33;
    const FAR_M: f32 = 5000.0;
    const MS_PER_S: f32 = 1000.0;
    pub(super) const SPAWN_X: f32 = 100.0;
    pub(super) const ROW_Y: f32 = 1000.0;
    /// The test archetype's run: `RunToTarget` at most 7 m/s (the boar's numbers).
    const RUN_MAX_MPS: f32 = 7.0;
    const NEAR_ZERO_M: f32 = 0.05;

    pub(super) struct Flat;

    impl Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(HEIGHT)
        }
    }

    pub(super) fn spawn_at(x: f32, id: &str, level: u32) -> VitalsSpawn {
        VitalsSpawn {
            vitals_id: lowercase_crc32(id),
            level,
            position: [x, ROW_Y, HEIGHT],
            ..VitalsSpawn::default()
        }
    }

    fn mobs(spawns: Vec<VitalsSpawn>) -> Mobs {
        Mobs::new(spawns.into(), &test_combat())
    }

    pub(super) fn player_at(x: f32) -> Seen {
        Seen {
            entity: PLAYER,
            position: Position {
                x,
                y: ROW_Y,
                z: HEIGHT,
            },
        }
    }

    pub(super) fn every(_: Position) -> bool {
        true
    }

    fn tick(mobs: &mut Mobs, players: &[Seen], now: u64) -> Vec<MobAttackIntent> {
        mobs.tick(players, now, &Flat, 1)
    }

    fn ms(seconds: f32) -> u64 {
        (seconds * MS_PER_S).round() as u64
    }

    pub(super) fn at(mobs: &Mobs, index: u32) -> Position {
        mobs.get(mob_entity(index)).expect("mob").position
    }

    /// Ticks from `from` to `to` with `players`; the hits.
    fn run(mobs: &mut Mobs, players: &[Seen], from: u64, to: u64) -> Vec<(u64, MobAttackIntent)> {
        let mut hits = Vec::new();
        for now in (from..=to).step_by(TICK_MS as usize) {
            hits.extend(tick(mobs, players, now).into_iter().map(|hit| (now, hit)));
        }
        hits
    }

    /// Mobs of `spawns` whose spawns have the design profiles `profiles` (by spawn, in order).
    fn designed(spawns: Vec<VitalsSpawn>, profiles: Vec<Option<AiProfile>>) -> Mobs {
        let world = WorldFile {
            ai_profiles: profiles
                .iter()
                .enumerate()
                .filter_map(|(index, profile)| Some((index.to_string(), profile.clone()?)))
                .collect::<BTreeMap<_, _>>(),
            vitals_ai: profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| DesignSpawn {
                    profile: profile.as_ref().map(|_| index.to_string()),
                    path: None,
                })
                .collect(),
            ..WorldFile::default()
        };
        let mut mobs = mobs(spawns);
        mobs.set_ai(resolve_ai(&world, mobs.ai()));
        mobs.set_cells(every);
        mobs
    }

    #[test]
    fn only_a_design_route_keeps_its_mob_awake() {
        use crate::mob_ai::{Route, RouteTask, SpawnAi};
        let routed = |tasks| {
            Some(SpawnAi {
                route: Some(Arc::new(Route {
                    points: vec![position_of(&spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL))],
                    tasks,
                    ..Route::default()
                })),
                ..SpawnAi::default()
            })
        };
        let graze = RouteTask {
            task: 0,
            duration_s: [-1.0, -1.0],
            align: false,
        };
        let mut mobs = mobs(vec![
            spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL),
            spawn_at(SPAWN_X + 1.0, TEST_MOB, TEST_LEVEL),
        ]);
        mobs.set_ai(Arc::from([routed(Vec::new()), routed(vec![Some(graze)])]));
        assert!(
            mobs.patrols(0),
            "a design patrol may walk far from its spawn"
        );
        assert!(!mobs.patrols(1), "a spawner's waypoints stay by the spawn");
    }

    #[test]
    fn the_tutorial_spawns_only_its_script_and_other_levels_keep_their_spawns() {
        let encounter = VitalsSpawn {
            is_encounter: true,
            ..spawn_at(0.0, TEST_MOB, TEST_LEVEL)
        };
        let standing = spawn_at(1.0, TEST_MOB, TEST_LEVEL);
        let spawns = [encounter.clone(), standing.clone()];
        assert!(spawns_of_level(TUTORIAL_FOLDER, &spawns).is_empty());
        assert_eq!(
            &*spawns_of_level("newworld_vitaeeterna", &spawns),
            &spawns[..]
        );
    }

    /// The overrides of spawn `marker`: its sight is the marker, so a test can tell whose they
    /// are.
    fn marked_ai(marker: f32) -> Option<crate::mob_ai::SpawnAi> {
        Some(crate::mob_ai::SpawnAi {
            sight_m: Some(marker),
            ..crate::mob_ai::SpawnAi::default()
        })
    }

    #[test]
    fn a_scripted_instance_leaves_its_encounter_spawns_to_the_script() {
        let encounter = VitalsSpawn {
            is_encounter: true,
            ..spawn_at(0.0, TEST_MOB, TEST_LEVEL)
        };
        let standing = spawn_at(1.0, TEST_MOB, TEST_LEVEL);
        let spawns = [encounter, standing.clone()];
        let ai = [marked_ai(0.0), marked_ai(1.0)];
        let kill_types = KillTypes::default();
        let filter = |scripted| SpawnFilter {
            scripted,
            open_map: false,
            kill_types: &kill_types,
        };
        let (all, all_ai) = sim_spawns(&spawns, &ai, filter(false));
        assert_eq!((&*all, &*all_ai), (&spawns[..], &ai[..]));
        let (kept, kept_ai) = sim_spawns(&spawns, &ai, filter(true));
        assert_eq!(&*kept, &[standing][..]);
        assert_eq!(
            &*kept_ai,
            &[marked_ai(1.0)][..],
            "the kept spawn keeps its own AI"
        );
    }

    #[test]
    fn the_open_map_leaves_out_the_encounters_it_does_not_run() {
        const SNIPER: &str = "invasion_sniper";
        const SWARMER: &str = "corruption_swarmer_dt1";
        let kill_types = KillTypes::build([
            (
                SNIPER.to_owned(),
                "Invasion_Sniper,Hostile,Corrupted".to_owned(),
            ),
            (
                SWARMER.to_owned(),
                "Corruption_Swarmer_DT1,Hostile,Corrupted,darkness".to_owned(),
            ),
            (TEST_MOB.to_owned(), "Boar,Hostile".to_owned()),
        ]);
        let encounter = |x: f32, id: &str| VitalsSpawn {
            is_encounter: true,
            ..spawn_at(x, id, TEST_LEVEL)
        };
        let standing_swarmer = spawn_at(2.0, SWARMER, TEST_LEVEL);
        let quest_boar = encounter(3.0, TEST_MOB);
        let spawns = [
            encounter(0.0, SNIPER),
            encounter(1.0, SWARMER),
            standing_swarmer.clone(),
            quest_boar.clone(),
        ];
        let ai = [
            marked_ai(0.0),
            marked_ai(1.0),
            marked_ai(2.0),
            marked_ai(3.0),
        ];
        let filter = |open_map| SpawnFilter {
            scripted: false,
            open_map,
            kill_types: &kill_types,
        };
        let (kept, kept_ai) = sim_spawns(&spawns, &ai, filter(true));
        assert_eq!(
            &*kept,
            &[standing_swarmer, quest_boar][..],
            "a plain spawn and another encounter's stay; the category's case does not count"
        );
        assert_eq!(
            &*kept_ai,
            &[marked_ai(2.0), marked_ai(3.0)][..],
            "each kept spawn keeps its own profile and path, not the next one's"
        );
        let (all, all_ai) = sim_spawns(&spawns, &ai, filter(false));
        assert_eq!(
            (&*all, &*all_ai),
            (&spawns[..], &ai[..]),
            "an instance keeps them"
        );
    }

    #[test]
    fn spawns_without_overrides_get_none() {
        let spawns = [spawn_at(0.0, TEST_MOB, TEST_LEVEL)];
        let kill_types = KillTypes::default();
        let filter = SpawnFilter {
            scripted: false,
            open_map: true,
            kill_types: &kill_types,
        };
        let (_, ai) = sim_spawns(&spawns, &[], filter);
        assert_eq!(&*ai, &[None][..], "a level whose spawns have no design AI");
    }

    #[test]
    fn a_tree_mob_strikes_a_player_in_reach_with_its_window_and_cooldown() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let player = player_at(SPAWN_X + TEST_REACH_M - 1.0);
        let strikes = run(&mut mobs, &[player], 0, 6_000);
        assert!(strikes.len() >= 2, "{strikes:?}");
        let (first_ms, first) = &strikes[0];
        assert_eq!(first.target, PLAYER);
        assert_eq!(first.mob_move, None);
        let strike = first.strike.as_ref().expect("a damage window");
        assert_eq!(
            (strike.table.as_str(), strike.row.as_str()),
            (TEST_MOB_TABLE, TEST_MOB_ROW)
        );
        assert!(*first_ms >= ms(TEST_HIT_S) && *first_ms <= ms(TEST_HIT_S) + TICK_MS);
        let gap = strikes[1].0 - first_ms;
        let expected = ms(TEST_ATTACK_S + TEST_COOLDOWN_S);
        assert!(
            gap >= expected && gap <= expected + 2 * TICK_MS,
            "its attack, then its cooldown: {gap} ms"
        );
        assert_eq!(
            mobs.state(mob_entity(0)),
            Some(MobState::Chase),
            "between swings"
        );
    }

    #[test]
    fn a_tree_mob_runs_on_the_ground_to_its_target_at_its_config_speed() {
        const START_M: f32 = 15.0;
        let mut spawn = spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL);
        spawn.position[2] = 0.0;
        let mut mobs = mobs(vec![spawn]);
        mobs.set_cells(every);
        let player = player_at(SPAWN_X + START_M);
        let mut last = at(&mobs, 0);
        let mut top_mps: f32 = 0.0;
        let mut struck = None;
        for now in (0..10_000).step_by(TICK_MS as usize) {
            if !tick(&mut mobs, &[player], now).is_empty() {
                struck = Some(now);
                break;
            }
            let here = at(&mobs, 0);
            top_mps = top_mps.max(here.horizontal_distance(last) * MS_PER_S / TICK_MS as f32);
            last = here;
        }
        assert!(struck.is_some(), "it reached and struck");
        assert!(
            top_mps <= RUN_MAX_MPS + NEAR_ZERO_M && top_mps > RUN_MAX_MPS / 2.0,
            "{top_mps}"
        );
        assert_eq!(at(&mobs, 0).z, HEIGHT, "it stands on the ground");
        assert!(at(&mobs, 0).horizontal_distance(player.position) <= TEST_REACH_M);
    }

    /// A stun the sim holds on a tree mob keeps it where it stands and its tree still; a root
    /// keeps it in place while its tree runs; once they end it runs to its target.
    #[test]
    fn a_stunned_or_rooted_tree_mob_stays_where_it_stands() {
        const START_M: f32 = 15.0;
        const STEPS: u64 = 30;
        let mut spawn = spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL);
        spawn.position[2] = 0.0;
        let mut mobs = mobs(vec![spawn]);
        mobs.set_cells(every);
        let player = player_at(SPAWN_X + START_M);
        let held = |control| {
            FxHashMap::from_iter([(
                mob_entity(0),
                HeldEffects {
                    control,
                    ..HeldEffects::default()
                },
            )])
        };
        let start = at(&mobs, 0);
        let mut now = 0;
        let mut steps = |mobs: &mut Mobs| {
            for _ in 0..STEPS {
                tick(mobs, &[player], now);
                now += TICK_MS;
            }
        };
        mobs.set_held_effects(held(Control {
            stunned: true,
            ..Control::default()
        }));
        steps(&mut mobs);
        let moved = |mobs: &Mobs| at(mobs, 0).horizontal_distance(start);
        assert!(moved(&mobs) < f32::EPSILON, "stunned");
        assert_eq!(mobs.target(mob_entity(0)), None, "its tree waits");
        mobs.set_held_effects(held(Control {
            no_dodge: true,
            ..Control::default()
        }));
        steps(&mut mobs);
        assert!(moved(&mobs) < f32::EPSILON, "rooted");
        assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER), "it thinks");
        mobs.set_held_effects(FxHashMap::default());
        steps(&mut mobs);
        assert!(moved(&mobs) > 0.0, "free, it runs");
    }

    #[test]
    fn a_mob_spawned_under_the_floor_stands_on_it_at_its_first_step() {
        // Live login-world-entry-01: a boar spawn 1.42 m under the terrain.
        const UNDER_M: f32 = 1.42;
        let mut spawn = spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL);
        spawn.position[2] = HEIGHT - UNDER_M;
        let mut mobs = mobs(vec![spawn]);
        mobs.set_cells(every);
        run(&mut mobs, &[player_at(SPAWN_X + 10.0)], 0, 0);
        assert_eq!(at(&mobs, 0).z, HEIGHT);
    }

    #[test]
    fn a_buried_corpse_is_not_shown_until_its_respawn() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let entity = mob_entity(0);
        run(&mut mobs, &[player_at(SPAWN_X + 10.0)], 0, 0);
        assert_eq!(mobs.corpses().count(), 0, "it lives");
        let mob = mobs.get_mut(entity).expect("mob");
        mob.health.take(mob.health.max());
        mobs.died(entity, 1);
        assert_eq!(mobs.corpses().collect::<Vec<_>>(), [(entity, 1)]);
        assert!(mobs.awake_states().any(|(mob, ..)| mob == entity));
        mobs.bury(entity);
        assert_eq!(mobs.corpses().count(), 0);
        assert!(!mobs.awake_states().any(|(mob, ..)| mob == entity));
    }

    /// A tree's `ExecuteSpawn` (retail `medusa_minionsummon_attackbehavior`: `Spawn_Boar`, one
    /// boar slice): its mobs stand awake on a ring around the summoner with the summoner's level
    /// and show the summoned slice; a dead one never respawns and goes with its corpse, and a
    /// slice without a card spawns nothing.
    #[test]
    fn a_summon_spawns_its_slice_beside_the_summoner_once() {
        const ADDS: u32 = 3;
        const SLICE: [u8; ASSET_ID_LEN] = [7; ASSET_ID_LEN];
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.ai = Arc::new(
            MobAi::empty()
                .with_slice_vitals(SliceVitals::from([(SLICE, lowercase_crc32(TEST_MOB))])),
        );
        mobs.set_cells(every);
        let before = mobs.live_entities().len();
        mobs.summon(
            0,
            Summon {
                asset: [9; ASSET_ID_LEN],
                count: ADDS,
                at: None,
            },
        );
        assert_eq!(mobs.live_entities().len(), before, "no card, no mob");
        mobs.summon(
            0,
            Summon {
                asset: SLICE,
                count: ADDS,
                at: None,
            },
        );
        let adds: Vec<u64> = mobs
            .live_entities()
            .into_iter()
            .filter(|entity| *entity != mob_entity(0))
            .collect();
        assert_eq!(adds.len(), ADDS as usize);
        for add in &adds {
            assert_eq!(mobs.asset(*add), Some(SLICE), "the summoned slice's card");
            let index = spawn_index(*add).expect("index");
            assert_eq!(
                mobs.spawn_of(index).map(|spawn| spawn.level),
                Some(TEST_LEVEL)
            );
            let at = mobs.live[&index].position;
            assert!(
                (at.x - SPAWN_X).abs() <= SPAWN_SPACING_M * ADDS as f32,
                "beside it"
            );
            assert!(mobs.awake.contains(&index));
        }
        let add = adds[0];
        let mob = mobs.get_mut(add).expect("add");
        mob.health.take(mob.health.max());
        mobs.died(add, 1);
        mobs.respawn_due(u64::MAX);
        assert!(
            mobs.live.contains_key(&spawn_index(add).expect("index")),
            "no respawn"
        );
        mobs.bury(add);
        assert!(!mobs.live_entities().contains(&add), "gone with its corpse");
        assert!(mobs.take_left().contains(&add));
    }

    /// Ground that rises [`SLOPE_RISE`] metres per metre east of [`SPAWN_X`].
    struct Slope;

    const SLOPE_RISE: f32 = 0.3;

    impl Ground for Slope {
        fn height_at(&self, x: f32, _: f32) -> Option<f32> {
            Some(HEIGHT + (x - SPAWN_X) * SLOPE_RISE)
        }
    }

    #[test]
    fn a_lunge_up_a_slope_lands_on_the_slope() {
        // Live 2026-10-29: Janszoon's charge up the slope ran half under the ground.
        const LUNGE_M: f32 = 3.0;
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let target = player_at(SPAWN_X + 2.0 * LUNGE_M).position;
        let landed = mobs
            .lunge_over(mob_entity(0), target, LUNGE_M, &Slope)
            .expect("lunged");
        assert!((landed.x - (SPAWN_X + LUNGE_M)).abs() < 1e-3, "{landed:?}");
        assert!(
            (landed.z - Slope.height_at(landed.x, landed.y).expect("ground")).abs() < 1e-3,
            "on the slope: {landed:?}"
        );
        assert_eq!(at(&mobs, 0), landed);
    }

    #[test]
    fn a_mob_on_a_deck_keeps_its_height() {
        const DECK: f32 = 90.0;
        let mut spawn = spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL);
        spawn.position[2] = DECK;
        let mut mobs = mobs(vec![spawn]);
        mobs.set_cells(every);
        let mut player = player_at(SPAWN_X + 10.0);
        player.position.z = DECK;
        run(&mut mobs, &[player], 0, 1_000);
        assert!(at(&mobs, 0).x > SPAWN_X, "it ran");
        assert_eq!(at(&mobs, 0).z, DECK, "the ground is far below the deck");
    }

    #[test]
    fn a_tree_mob_past_its_leash_walks_home_untouchable_and_heals() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let entity = mob_entity(0);
        let mob = mobs.get_mut(entity).expect("mob");
        mob.health.take(mob.health.max() / 2);
        mob.position.x += TEST_LEASH_M + 5.0;
        let far = player_at(SPAWN_X + 2.0 * TEST_LEASH_M);
        tick(&mut mobs, &[far], 0);
        assert_eq!(mobs.state(entity), Some(MobState::Return));
        let from = Position {
            x: SPAWN_X,
            y: ROW_Y,
            z: HEIGHT,
        };
        assert!(
            !mobs.hurt(entity, PLAYER, from, 1, TICK_MS),
            "a reset takes no hit"
        );
        let mut now = TICK_MS;
        while mobs.state(entity) == Some(MobState::Return) && now < 20_000 {
            tick(&mut mobs, &[far], now);
            now += TICK_MS;
        }
        assert!(
            at(&mobs, 0).horizontal_distance(from) < 1.0,
            "home: {:?}",
            at(&mobs, 0)
        );
        let mob = mobs.get(entity).expect("mob");
        assert_eq!(
            mob.health.current(),
            mob.health.max(),
            "the reset healed it"
        );
        assert_eq!(mobs.state(entity), Some(MobState::Idle));
    }

    #[test]
    fn a_tree_mob_without_a_target_stands_at_its_spawn() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let unseen = player_at(SPAWN_X + TEST_SIGHT_M + 5.0);
        run(&mut mobs, &[unseen], 0, 30_000);
        assert_eq!(at(&mobs, 0).x, SPAWN_X, "its tree does not wander");
        assert_eq!(mobs.state(mob_entity(0)), Some(MobState::Idle));
    }

    #[test]
    fn trees_think_at_the_retail_rates() {
        const SECONDS: u64 = 3;
        let rates = crate::mob_ai::RETAIL_AI_RATES;
        // Standing without a target: one run per idle interval.
        let mut idle = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        idle.set_cells(every);
        run(&mut idle, &[player_at(SPAWN_X + 30.0)], 0, SECONDS * 1_000);
        let expected = SECONDS * 1_000 / rates.idle_interval_ms;
        assert!(
            idle.brain_runs().abs_diff(expected) <= 1,
            "{}",
            idle.brain_runs()
        );
        // Fighting: every tick.
        let mut fighting = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        fighting.set_cells(every);
        run(
            &mut fighting,
            &[player_at(SPAWN_X + 2.0)],
            0,
            SECONDS * 1_000,
        );
        let ticks = SECONDS * 1_000 / TICK_MS;
        assert!(
            fighting.brain_runs() + 2 >= ticks,
            "{}",
            fighting.brain_runs()
        );
        assert_eq!(rates.interval_ms(crate::mob_ai::Activity::Moving, 10.0), 98);
        assert_eq!(
            rates.interval_ms(crate::mob_ai::Activity::Moving, 25.0),
            218
        );
        assert_eq!(
            rates.interval_ms(crate::mob_ai::Activity::Moving, 45.0),
            333
        );
    }

    #[test]
    fn many_mobs_think_the_same_on_threads() {
        const MOBS: u32 = 4 * MIN_MOBS_PER_THREAD as u32;
        const THREADS: usize = 4;
        const SPACING_M: f32 = 2.0;
        let spawns: Vec<VitalsSpawn> = (0..MOBS)
            .map(|index| spawn_at(SPAWN_X + index as f32 * SPACING_M, TEST_MOB, TEST_LEVEL))
            .collect();
        let player = player_at(SPAWN_X + 30.0);
        let positions = |threads: usize| {
            let mut mobs = mobs(spawns.clone());
            mobs.set_cells(every);
            let mut hits = 0;
            for now in (0..2_000).step_by(TICK_MS as usize) {
                hits += mobs.tick(&[player], now, &Flat, threads).len();
            }
            let places: Vec<(u32, u32)> = (0..MOBS)
                .map(|index| {
                    let at = at(&mobs, index);
                    (at.x.to_bits(), at.y.to_bits())
                })
                .collect();
            (places, hits)
        };
        let one = positions(1);
        assert!(one.1 > 0, "the mobs near the player fought");
        assert_eq!(positions(THREADS), one);
    }

    #[test]
    fn a_mob_that_takes_a_target_shares_it_with_its_group() {
        // The second mob is passive: it takes only shared targets and attackers.
        let mut mobs = designed(
            vec![
                spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL),
                spawn_at(SPAWN_X - TEST_GROUP_AGGRO_M / 2.0, TEST_MOB, TEST_LEVEL),
                // In sight of the player, beyond the group distance of the first.
                spawn_at(SPAWN_X + 2.0 * TEST_GROUP_AGGRO_M, TEST_MOB, TEST_LEVEL),
            ],
            vec![
                None,
                Some(AiProfile {
                    behaviour: AiBehaviour::Idle,
                    ..AiProfile::default()
                }),
                Some(AiProfile {
                    behaviour: AiBehaviour::Idle,
                    ..AiProfile::default()
                }),
            ],
        );
        let player = player_at(SPAWN_X + TEST_SIGHT_M / 2.0);
        run(&mut mobs, &[player], 0, 3 * TICK_MS);
        assert_eq!(
            mobs.target(mob_entity(0)),
            Some(PLAYER),
            "it sees the player"
        );
        assert_eq!(mobs.target(mob_entity(1)), Some(PLAYER), "shared");
        assert_eq!(
            mobs.target(mob_entity(2)),
            None,
            "passive, and too far from the first"
        );
    }

    #[test]
    fn a_mob_notices_a_player_close_beside_it_out_of_its_sight_cone() {
        // The beach sailor's ambient vision: 90 deg (`archnav.txt`); the test mob faces +y and
        // the player stands east of it, 90 deg aside.
        const SAILOR_CONE_DEG: f32 = 90.0;
        let mut data = crate::mob_ai::test_support::test_ai_data();
        data.archetypes[0].ambient_vision = Some(nw_ai::data::Vision {
            angle_deg: SAILOR_CONE_DEG,
            distance_m: TEST_SIGHT_M,
        });
        let ai = crate::mob_ai::MobAi::new(
            data,
            crate::mob_ai::AiRates::default(),
            [(
                lowercase_crc32(TEST_MOB),
                crate::mob_ai::test_support::TEST_ASSET,
            )]
            .into_iter()
            .collect(),
        );
        let combat = crate::combat::CombatSettings {
            ai: Arc::new(ai),
            ..test_combat()
        };
        let aside = |distance_m: f32| {
            let mut mobs = Mobs::new(
                vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)].into(),
                &combat,
            );
            mobs.set_cells(every);
            run(
                &mut mobs,
                &[player_at(SPAWN_X + distance_m)],
                0,
                3 * TICK_MS,
            );
            mobs.target(mob_entity(0))
        };
        assert_eq!(
            aside(crate::mob_ai::IMMEDIATE_VICINITY_M - 0.5),
            Some(PLAYER),
            "near: noticed"
        );
        assert_eq!(aside(TEST_SIGHT_M / 2.0), None, "out of the cone");
    }

    #[test]
    fn design_profiles_override_the_slice_values() {
        const LEASH_M: f32 = 20.0;
        const INTERVAL_MS: u64 = 5_000;
        const SPEED_MPS: f32 = 3.0;
        const SIGHT_M: f32 = 5.0;
        let mut mobs = designed(
            vec![
                spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL),
                spawn_at(SPAWN_X + FAR_M, TEST_MOB, TEST_LEVEL),
                spawn_at(SPAWN_X + 2.0 * FAR_M, TEST_MOB, TEST_LEVEL),
            ],
            vec![
                Some(AiProfile {
                    behaviour: AiBehaviour::Guard,
                    leash_radius_m: Some(LEASH_M),
                    attack_interval_ms: Some(INTERVAL_MS),
                    ..AiProfile::default()
                }),
                Some(AiProfile {
                    speed_mps: Some(SPEED_MPS),
                    sight_m: Some(SIGHT_M),
                    ..AiProfile::default()
                }),
                Some(AiProfile {
                    behaviour: AiBehaviour::Idle,
                    ..AiProfile::default()
                }),
            ],
        );
        let ai = Arc::clone(&mobs.ai);
        let data = ai.data();
        let fact = |mobs: &Mobs, index: u32, key: &str| {
            mobs.tree(mob_entity(index))
                .and_then(|mind| mind.brain().fact(data.key(key)?))
        };
        assert_eq!(
            fact(&mobs, 0, "CombatLeashDistance"),
            Some(nw_ai::value::Value::Float(LEASH_M))
        );
        assert_eq!(
            fact(&mobs, 0, "Enable_Wander"),
            Some(nw_ai::value::Value::Bool(false))
        );
        assert_eq!(
            fact(&mobs, 2, "Is_Passive"),
            Some(nw_ai::value::Value::Bool(true))
        );
        // The guard strikes with its own cooldown.
        let strikes = run(&mut mobs, &[player_at(SPAWN_X + 2.0)], 0, 12_000);
        let first: Vec<u64> = strikes
            .iter()
            .filter(|(_, hit)| hit.mob == mob_entity(0))
            .map(|(at, _)| *at)
            .collect();
        let gap = first[1] - first[0];
        let expected = ms(TEST_ATTACK_S) + INTERVAL_MS;
        assert!(gap >= expected && gap <= expected + 2 * TICK_MS, "{gap}");
        // Its leash: past 20 m it resets.
        let entity = mob_entity(0);
        mobs.get_mut(entity).expect("mob").position.x += LEASH_M + 1.0;
        tick(&mut mobs, &[], 12_033);
        assert_eq!(mobs.state(entity), Some(MobState::Return));
        // The second sees only 5 m and runs at most 3 m/s.
        let second = SPAWN_X + FAR_M;
        run(
            &mut mobs,
            &[player_at(second + SIGHT_M + 1.0)],
            13_000,
            15_000,
        );
        assert_eq!(mobs.target(mob_entity(1)), None, "beyond its sight");
        let start = at(&mobs, 1);
        run(
            &mut mobs,
            &[player_at(second + SIGHT_M - 1.0)],
            15_033,
            16_033,
        );
        let moved = at(&mobs, 1).horizontal_distance(start);
        assert!(moved > 0.0 && moved <= SPEED_MPS + NEAR_ZERO_M, "{moved}");
        // The third is passive: it fights only when hurt.
        let third = SPAWN_X + 2.0 * FAR_M;
        let near_third = player_at(third + 2.0);
        assert!(run(&mut mobs, &[near_third], 16_066, 18_000).is_empty());
        mobs.hurt(mob_entity(2), PLAYER, near_third.position, 1, 18_033);
        assert!(!run(&mut mobs, &[near_third], 18_066, 20_000).is_empty());
    }

    #[test]
    fn a_reaction_stops_the_swing_and_holds_the_tree() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let entity = mob_entity(0);
        let player = player_at(SPAWN_X + 2.0);
        tick(&mut mobs, &[player], 0);
        assert!(mobs.tree(entity).is_some_and(TreeMind::attacking));
        const REACTION_MS: u64 = 800;
        mobs.react(entity, TICK_MS, REACTION_MS);
        assert!(
            !mobs.tree(entity).is_some_and(TreeMind::attacking),
            "the swing is lost"
        );
        assert!(run(&mut mobs, &[player], 2 * TICK_MS, REACTION_MS).is_empty());
        assert!(!run(&mut mobs, &[player], REACTION_MS + TICK_MS, 4_000).is_empty());
    }

    #[test]
    fn a_gm_mob_is_everyones_stays_awake_and_can_be_removed() {
        const LATER_MS: u64 = 600_000;
        let mut mobs = mobs(Vec::new());
        let entity = mobs
            .spawn_gm(spawn_at(10.0, TEST_MOB, TEST_LEVEL))
            .expect("gm mob");
        assert_eq!(mobs.owner(entity), None, "every player sees it");
        assert_eq!(mobs.gm_mobs(), [entity]);
        assert!(mobs.health(entity).is_some());
        tick(&mut mobs, &[], 0);
        let runs = mobs.brain_runs;
        tick(&mut mobs, &[], LATER_MS);
        assert!(mobs.brain_runs > runs, "it stays awake with no player near");
        assert!(mobs.remove_gm(entity));
        assert!(!mobs.remove_gm(entity));
        assert_eq!(mobs.health(entity), None);
        assert!(mobs.gm_mobs().is_empty());
    }

    #[test]
    fn a_gm_mob_without_rows_is_refused() {
        let mut mobs = mobs(Vec::new());
        assert_eq!(
            mobs.spawn_gm(spawn_at(10.0, "no_such_mob", TEST_LEVEL)),
            None
        );
    }

    #[test]
    fn a_dead_world_mob_respawns_after_its_time() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let entity = mob_entity(0);
        let mob = mobs.get_mut(entity).expect("mob");
        mob.health.take(mob.health.max());
        mobs.died(entity, 0);
        assert_eq!(mobs.state(entity), Some(MobState::Dead));
        let respawn_ms = test_combat().mob_respawn_ms;
        tick(&mut mobs, &[], respawn_ms - 1);
        assert!(mobs.take_respawned().is_empty());
        tick(&mut mobs, &[], respawn_ms);
        assert_eq!(mobs.take_respawned(), [entity]);
        assert_eq!(at(&mobs, 0).x, SPAWN_X);
        let mob = mobs.get(entity).expect("mob");
        assert_eq!(mob.health.current(), mob.health.max());
    }

    #[test]
    fn mobs_of_owned_cells_spawn_and_lost_cells_remove_them() {
        let mut mobs = mobs(vec![
            spawn_at(100.0, TEST_MOB, TEST_LEVEL),
            spawn_at(900.0, TEST_MOB, TEST_LEVEL),
        ]);
        let west = mobs.set_cells(|position| position.x < 500.0);
        assert_eq!((west.spawned, west.removed), (1, 0));
        assert!(mobs.get_mut(mob_entity(0)).is_some());
        let east = mobs.set_cells(|position| position.x >= 500.0);
        assert_eq!((east.spawned, east.removed), (1, 1));
        assert!(mobs.get_mut(mob_entity(0)).is_none());
        assert!(mobs.get_mut(mob_entity(1)).is_some());
        assert_eq!(mobs.len(), 1);
    }

    #[test]
    fn a_spawn_without_rows_is_not_spawned_and_counted() {
        let mut mobs = mobs(vec![
            spawn_at(100.0, "Nobody", TEST_LEVEL),
            spawn_at(200.0, TEST_MOB, 0),
            spawn_at(300.0, TEST_MOB, TEST_LEVEL),
        ]);
        let first = mobs.set_cells(every);
        assert_eq!((first.spawned, first.unspawned), (1, 2));
        let again = mobs.set_cells(every);
        assert_eq!((again.spawned, again.unspawned), (0, 0), "counted once");
    }

    #[test]
    fn a_mob_without_a_tree_stands() {
        const OTHER_MOB: &str = "Test_Treeless";
        let mut combat = test_combat();
        let mut source = crate::combat::test_support::test_source();
        source
            .base_types
            .push((OTHER_MOB.to_owned(), nw_content::combat::MobType::PLAIN));
        combat.data = Arc::new(
            CombatData::build(source, &nw_content::combat::CombatRows::default()).expect("rows"),
        );
        let mut mobs = Mobs::new(
            vec![spawn_at(SPAWN_X, OTHER_MOB, TEST_LEVEL)].into(),
            &combat,
        );
        mobs.set_cells(every);
        assert!(mobs.tree(mob_entity(0)).is_none(), "no slice look: no tree");
        assert!(run(&mut mobs, &[player_at(SPAWN_X + 1.0)], 0, 5_000).is_empty());
        assert_eq!(at(&mobs, 0).x, SPAWN_X);
    }

    #[test]
    fn mob_numbers_hold_the_spawn_index() {
        assert_eq!(mob_entity(5), MOB_ENTITY_BASE + FIRST_MOB_OFFSET + 5);
        // No mob number has its low 32 bits all zero (the client does not show such a mob).
        for index in [0, 1, 5, u32::MAX - 1] {
            assert_ne!(mob_entity(index) & u64::from(u32::MAX), 0, "index {index}");
        }
        assert_eq!(spawn_index(MOB_ENTITY_BASE), None, "below the first mob");
        assert_eq!(spawn_index(mob_entity(5)), Some(5));
        assert_eq!(spawn_index(42), None);
        /// The most bits of an entity number in a bundle member (the 5-byte prefix code).
        const BUNDLE_NUMBER_BITS: u32 = 35;
        /// Far more spawns than any level has.
        const MANY_SPAWNS: u32 = 10_000_000;
        assert!(mob_entity(MANY_SPAWNS) < 1 << BUNDLE_NUMBER_BITS);
        let mut mobs = mobs(vec![spawn_at(100.0, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let mob = mobs.get_mut(mob_entity(0)).expect("mob");
        assert_eq!(mob.health.max(), 1000, "level health times the plain mod");
    }

    #[test]
    fn only_awake_mobs_think() {
        let mut mobs = mobs(vec![
            spawn_at(100.0, TEST_MOB, TEST_LEVEL),
            spawn_at(100.0 + FAR_M, TEST_MOB, TEST_LEVEL),
            spawn_at(100.0 + 2.0 * FAR_M, TEST_MOB, TEST_LEVEL),
        ]);
        mobs.set_cells(every);
        tick(&mut mobs, &[], 0);
        assert_eq!(mobs.brain_runs(), 0, "no player, no thought");
        tick(&mut mobs, &[player_at(150.0)], TICK_MS);
        assert_eq!(mobs.brain_runs(), 1, "only the mob near the player");
        let sleep_ms = test_combat().sleep_after_ms;
        run(&mut mobs, &[player_at(FAR_M * 4.0)], 2 * TICK_MS, sleep_ms);
        assert_eq!(
            mobs.state(mob_entity(0)),
            Some(MobState::Idle),
            "awake until its sleep time"
        );
        // It falls asleep at its next tree tick after the sleep time.
        let next_tick_ms = sleep_ms + crate::mob_ai::RETAIL_AI_RATES.idle_interval_ms;
        run(
            &mut mobs,
            &[player_at(FAR_M * 4.0)],
            sleep_ms + TICK_MS,
            next_tick_ms + TICK_MS,
        );
        assert_eq!(mobs.state(mob_entity(0)), Some(MobState::Asleep));
        assert!(
            mobs.take_slept().contains(&mob_entity(0)),
            "its clients see it spawn again"
        );
        let awake = mobs
            .counts()
            .into_iter()
            .find(|(state, _)| *state == MobState::Asleep);
        assert_eq!(awake, Some((MobState::Asleep, 3)));
    }

    #[test]
    fn a_mob_away_from_home_does_not_fall_asleep() {
        const AWAY_M: f32 = 30.0;
        let mut mobs = mobs(vec![
            spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL),
            spawn_at(SPAWN_X + FAR_M, TEST_MOB, TEST_LEVEL),
        ]);
        mobs.set_cells(every);
        tick(
            &mut mobs,
            &[player_at(SPAWN_X), player_at(SPAWN_X + FAR_M)],
            0,
        );
        mobs.get_mut(mob_entity(1)).expect("mob").position.x += AWAY_M;
        let sleep_ms = test_combat().sleep_after_ms;
        let next_tick_ms = sleep_ms + crate::mob_ai::RETAIL_AI_RATES.idle_interval_ms;
        let nobody = [player_at(FAR_M * 4.0)];
        run(&mut mobs, &nobody, TICK_MS, next_tick_ms + TICK_MS);
        assert_eq!(mobs.state(mob_entity(0)), Some(MobState::Asleep), "at home");
        assert_ne!(mobs.state(mob_entity(1)), Some(MobState::Asleep), "away");
    }

    #[test]
    fn the_snapshot_lists_awake_mobs_and_counts_every_state() {
        let mut mobs = mobs(vec![
            spawn_at(100.0, TEST_MOB, TEST_LEVEL),
            spawn_at(100.0 + FAR_M, TEST_MOB, TEST_LEVEL),
            spawn_at(100.0 + 2.0 * FAR_M, TEST_MOB, TEST_LEVEL),
        ]);
        mobs.set_cells(every);
        tick(&mut mobs, &[player_at(150.0)], TICK_MS);
        let (awake, counts) = mobs.snapshot();
        assert_eq!(
            awake.iter().map(|mob| mob.entity).collect::<Vec<_>>(),
            vec![mob_entity(0)]
        );
        let expected = mobs
            .counts()
            .map(|(_, count)| u32::try_from(count).expect("count"));
        assert_eq!(counts, expected);
        assert_eq!(counts.iter().sum::<u32>(), 3);
        assert_eq!(counts[usize::from(state_index(MobState::Asleep))], 2);
    }

    /// Admin tools spec 1.3 B3: run with
    /// `cargo test --release -p sim snapshot_speed -- --ignored --nocapture`.
    #[test]
    #[ignore = "release-mode timing of the admin snapshot"]
    fn snapshot_speed_of_many_awake_mobs() {
        /// Awake mobs of the busiest sim in the live panel exit test (one sim, 1,000 bots).
        const AWAKE: u32 = 23_000;
        const SPACING_M: f32 = 0.004;
        const ROUNDS: u32 = 100;
        const LIMIT: Duration = Duration::from_micros(500);
        let spawns = (0..AWAKE)
            .map(|index| spawn_at(100.0 + index as f32 * SPACING_M, TEST_MOB, TEST_LEVEL))
            .collect();
        let mut mobs = mobs(spawns);
        mobs.set_cells(every);
        mobs.tick(&[player_at(150.0)], TICK_MS, &Flat, 1);
        assert_eq!(
            mobs.snapshot().0.len(),
            AWAKE as usize,
            "every mob is awake"
        );
        let started = Instant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(mobs.snapshot());
        }
        let average = started.elapsed() / ROUNDS;
        println!("snapshot of {AWAKE} awake mobs: {average:?}");
        assert!(average < LIMIT, "{average:?}");
    }

    #[test]
    fn a_paced_mob_with_moves_swings_them_and_waits_for_their_end() {
        const COOLDOWN_MS: u64 = 1_000;
        const FIRST: MobMove = MobMove {
            fragment: MoveFragment {
                fragment: 58,
                action: 28,
                tail: 0xa5,
            },
            length_ms: 2_000,
            hits: &[],
        };
        const SECOND: MobMove = MobMove {
            length_ms: 500,
            ..FIRST
        };
        const FIRST_MOVES: [MobMove; 1] = [FIRST];
        const SECOND_MOVES: [MobMove; 1] = [SECOND];
        const PACE: Pace = Pace {
            attack_factor: 0.25,
            swing_gap_ms: (COOLDOWN_MS, COOLDOWN_MS),
            speed_mps: 5.5,
            hit_delay_ms: 0,
            attack: TEST_ATTACK,
            gait: GAIT,
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: DEFAULT_MOB_CAPSULE,
            reach: None,
            moves: &FIRST_MOVES,
            phase_two: Some(PhaseTwo {
                moves: &SECOND_MOVES,
                damage_mult: 2.2,
            }),
        };
        const START: u64 = 1_000;
        let mut mobs = mobs(Vec::new());
        let boss = mobs
            .spawn_personal(
                spawn_at(100.0, TEST_MOB, TEST_LEVEL),
                PLAYER,
                false,
                Some(PACE),
                None,
                START,
            )
            .expect("rows");
        let mut swings = Vec::new();
        let mut now = START;
        while swings.len() < 2 {
            for intent in tick(&mut mobs, &[player_at(101.0)], now) {
                swings.push((now, intent.mob_move));
            }
            now += TICK_MS;
        }
        assert_eq!(swings[0].1, Some(FIRST));
        assert!(swings[1].0 - swings[0].0 >= FIRST.length_ms + COOLDOWN_MS);
        assert_eq!(mobs.phase_two(boss), None);
        mobs.enter_phase_two(boss);
        assert!(mobs.phase_two(boss).is_some());
        let next = (0..)
            .map(|tick_number| now + tick_number * TICK_MS)
            .find_map(|at| tick(&mut mobs, &[player_at(101.0)], at).first().cloned())
            .expect("a swing");
        assert_eq!(next.mob_move, Some(SECOND));
    }

    /// The test paces' attack.
    const TEST_ATTACK: PaceAttack = PaceAttack {
        table: "TestTable",
        row: "Test_Attack",
        weapon: "TestWeapon",
    };
    /// A pace's gait: swings at 3 m, else runs.
    const GAIT: Gait = Gait {
        swing_start_m: 3.0,
        run_beyond_m: 3.0,
        walk_speed_mps: 5.5,
    };

    #[test]
    fn a_paced_mob_waits_one_interval_after_its_fight_starts_and_between_hits() {
        const GAP_MS: u64 = 6_000;
        const SPREAD_MS: u64 = 1_000;
        const PACE: Pace = Pace {
            attack_factor: 0.25,
            swing_gap_ms: (GAP_MS, GAP_MS + SPREAD_MS),
            speed_mps: 5.5,
            hit_delay_ms: 456,
            attack: TEST_ATTACK,
            gait: GAIT,
            sight_m: 20.0,
            hit_scale: 1.0,
            capsule: DEFAULT_MOB_CAPSULE,
            reach: None,
            moves: &[],
            phase_two: None,
        };
        const START: u64 = 1_000;
        let mut mobs = mobs(Vec::new());
        let held = mobs
            .spawn_personal(
                spawn_at(100.0, TEST_MOB, TEST_LEVEL),
                PLAYER,
                true,
                Some(PACE),
                None,
                0,
            )
            .expect("rows");
        assert_eq!(mobs.get_mut(held).map(|mob| mob.attack.factor), Some(0.25));
        mobs.release(PLAYER, START);
        let mut hits = Vec::new();
        let mut now = START;
        while now < START + 3 * PACE.swing_gap_ms.0 {
            if !tick(&mut mobs, &[player_at(101.0)], now).is_empty() {
                hits.push(now);
            }
            now += TICK_MS;
        }
        assert!(hits.len() >= 2, "{hits:?}");
        assert!(hits[0] >= START + PACE.swing_gap_ms.0, "{hits:?}");
        assert!(hits[1] - hits[0] >= PACE.swing_gap_ms.0, "{hits:?}");
        let at_once = mobs
            .spawn_personal(
                spawn_at(100.0, TEST_MOB, TEST_LEVEL),
                PLAYER,
                false,
                Some(PACE),
                None,
                now,
            )
            .expect("rows");
        let ticks_to_try = 3 * PACE.swing_gap_ms.0 / TICK_MS;
        let first = (0..ticks_to_try)
            .map(|tick_number| now + tick_number * TICK_MS)
            .find(|at| {
                tick(&mut mobs, &[player_at(101.0)], *at)
                    .iter()
                    .any(|intent| intent.mob == at_once)
            })
            .expect("it hits");
        assert!(first >= now + PACE.swing_gap_ms.0);
    }

    #[test]
    fn a_player_that_stands_still_does_not_rescan() {
        let mut mobs = mobs(vec![spawn_at(100.0, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        tick(&mut mobs, &[player_at(150.0)], 0);
        assert_eq!(mobs.wake_scans(), 1);
        tick(
            &mut mobs,
            &[player_at(150.0 + WAKE_RESCAN_M - 1.0)],
            TICK_MS,
        );
        assert_eq!(mobs.wake_scans(), 1, "still near its last scan");
        tick(
            &mut mobs,
            &[player_at(150.0 + WAKE_RESCAN_M + 1.0)],
            2 * TICK_MS,
        );
        assert_eq!(mobs.wake_scans(), 2, "moved far enough");
        mobs.set_cells(|_| false);
        mobs.set_cells(every);
        tick(
            &mut mobs,
            &[player_at(150.0 + WAKE_RESCAN_M + 1.0)],
            3 * TICK_MS,
        );
        assert_eq!(mobs.wake_scans(), 3, "new mobs: everyone scans again");
    }

    #[test]
    fn a_mob_forgets_a_player_who_left() {
        let mut mobs = mobs(vec![spawn_at(SPAWN_X, TEST_MOB, TEST_LEVEL)]);
        mobs.set_cells(every);
        let player = player_at(SPAWN_X + 2.0);
        tick(&mut mobs, &[player], 0);
        assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER));
        mobs.forget(PLAYER);
        tick(&mut mobs, &[], TICK_MS);
        assert_eq!(mobs.target(mob_entity(0)), None);
    }

    #[test]
    fn the_spawn_list_is_shared() {
        let spawns: Arc<[VitalsSpawn]> = vec![spawn_at(100.0, TEST_MOB, TEST_LEVEL)].into();
        let mobs = Mobs::new(Arc::clone(&spawns), &test_combat());
        assert_eq!(Arc::strong_count(&spawns), 2);
        assert_eq!(mobs.len(), 0, "nothing spawns before the cells are known");
    }
}

/// Mobs that run the game's own trees (the AI data file of `local-data/content`; run
/// `content-tool ai-data` first).
#[cfg(all(test, feature = "local-data"))]
mod retail_tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use nw_ai::AiData;
    use nw_content::client_build::load_client_build;
    use nw_content::combat::{CombatRows, MobType};
    use nw_formats::objectstream::lowercase_crc32;

    use super::tests::{Flat, HEIGHT, PLAYER, SPAWN_X, TICK_MS, at, every, player_at, spawn_at};
    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, test_source};
    use crate::combat::tests::test_combat;
    use crate::mob_ai::{AiRates, LookAssets};

    const CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
    /// The slice assets of the beach boar, the tutorial's swarmer and the risen (their
    /// creature animation rows).
    const BOAR: &str = "db96d765b50459d98c9e04adaa9c9485";
    const SWARMER: &str = "e06d1bb7311f5acb9857cc20464ce4df";
    const RISEN: &str = "a8b60bc7feaf574cbe7ae3c2d8a48dec";

    fn asset(hex: &str) -> [u8; ASSET_ID_LEN] {
        hex::decode(hex)
            .expect("hex")
            .try_into()
            .expect("an asset id")
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn retail_data() -> AiData {
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        AiData::load(&repo().join("local-data/content"), &build)
            .expect("run `content-tool ai-data` first")
    }

    /// Mobs of the vitals ids `kinds`, each shown with its slice asset, on the retail trees.
    fn retail_mobs(spawns: Vec<VitalsSpawn>, kinds: &[(&str, [u8; ASSET_ID_LEN])]) -> Mobs {
        let looks: LookAssets = kinds
            .iter()
            .map(|(id, asset)| (lowercase_crc32(id), *asset))
            .collect();
        let mut source = test_source();
        source.base_types.extend(
            kinds
                .iter()
                .map(|(id, _)| ((*id).to_owned(), MobType::PLAIN)),
        );
        let mut combat = test_combat();
        combat.data = Arc::new(CombatData::build(source, &CombatRows::default()).expect("rows"));
        combat.ai = Arc::new(MobAi::new(retail_data(), AiRates::default(), looks));
        let mut mobs = Mobs::new(spawns.into(), &combat);
        mobs.set_cells(every);
        mobs
    }

    /// The names of the fragments mob `entity` started, in order.
    fn played(mobs: &Mobs, entity: u64, seen: &mut Option<u32>, out: &mut Vec<String>) {
        if let Some((shown, _, fragment)) = mobs.shown(entity)
            && *seen != Some(shown.serial)
        {
            *seen = Some(shown.serial);
            out.push(fragment.to_owned());
        }
    }

    #[test]
    fn the_beach_boar_wanders_inside_its_wander_distance() {
        const BOAR_ID: &str = "Retail_Boar";
        // Slice `boar`: `WanderDistanceMax` 25 m; its `Wander` nav config arrives within 0.1 m.
        const WANDER_MAX_M: f32 = 25.0;
        const MINUTE_MS: u64 = 60_000;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, BOAR_ID, TEST_LEVEL)],
            &[(BOAR_ID, asset(BOAR))],
        );
        // Out of its ambient sight (12 m) and in the wake radius.
        let watcher = player_at(SPAWN_X + 2.0 * WANDER_MAX_M);
        let spawn = at(&mobs, 0);
        let mut farthest: f32 = 0.0;
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..MINUTE_MS).step_by(TICK_MS as usize) {
            mobs.tick(&[watcher], now, &Flat, 1);
            farthest = farthest.max(at(&mobs, 0).horizontal_distance(spawn));
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        assert!(farthest > 1.0, "its tree wanders: {fragments:?}");
        assert!(farthest <= WANDER_MAX_M + 1.0, "{farthest}");
        assert!(
            fragments.iter().any(|fragment| fragment == "Move"),
            "{fragments:?}"
        );
        assert_eq!(
            mobs.target(mob_entity(0)),
            None,
            "a passive boar takes no target by sight"
        );
        assert_eq!(at(&mobs, 0).z, HEIGHT);
    }

    #[test]
    fn the_tutorials_swarmer_and_risen_fight_with_their_trees() {
        const SWARMER_ID: &str = "Corruption_Entity_Crawler_FTUE";
        const RISEN_ID: &str = "Corruption_Entity";
        const FIGHT_MS: u64 = 20_000;
        const OWNER: u64 = PLAYER;
        let mut mobs = retail_mobs(
            Vec::new(),
            &[(SWARMER_ID, asset(SWARMER)), (RISEN_ID, asset(RISEN))],
        );
        let swarmer = mobs
            .spawn_personal(
                spawn_at(SPAWN_X, SWARMER_ID, TEST_LEVEL),
                OWNER,
                false,
                None,
                None,
                0,
            )
            .expect("swarmer");
        let risen = mobs
            .spawn_personal(
                spawn_at(SPAWN_X, RISEN_ID, TEST_LEVEL),
                OWNER,
                false,
                None,
                None,
                0,
            )
            .expect("risen");
        // In front of them: a spawn faces north (the world file keeps no spawn rotation), and
        // their ambient vision cones are 65 and 75 degrees wide (slices).
        let player = Seen {
            entity: PLAYER,
            position: Position {
                y: super::tests::ROW_Y + 4.0,
                ..player_at(SPAWN_X).position
            },
        };
        let mut strikes: Vec<(u64, MobAttackIntent)> = Vec::new();
        let mut fragments = [Vec::new(), Vec::new()];
        let mut serials = [None, None];
        for now in (0..FIGHT_MS).step_by(TICK_MS as usize) {
            strikes.extend(
                mobs.tick(&[player], now, &Flat, 1)
                    .into_iter()
                    .map(|hit| (now, hit)),
            );
            for (index, mob) in [swarmer, risen].into_iter().enumerate() {
                played(&mobs, mob, &mut serials[index], &mut fragments[index]);
            }
        }
        // Retail (trace finding b; capture fragments of these slices): the swarmer slashed with
        // `Attack_Crawler_Slash_R`/`_L` from `CorruptedSwarmerDamageTable`, the risen swiped and
        // smashed from `CorruptionEntityDamageTable`.
        let tables = |mob: u64| -> Vec<(String, String)> {
            strikes
                .iter()
                .filter(|(_, hit)| hit.mob == mob)
                .filter_map(|(_, hit)| hit.strike.as_ref())
                .map(|strike| (strike.table.clone(), strike.row.clone()))
                .collect()
        };
        let swarmer_hits = tables(swarmer);
        assert!(!swarmer_hits.is_empty(), "{:?}", fragments[0]);
        assert!(
            swarmer_hits
                .iter()
                .all(|(table, _)| table == "CorruptedSwarmerDamageTable")
        );
        assert!(
            fragments[0]
                .iter()
                .any(|fragment| fragment.starts_with("Attack_Crawler_Slash")),
            "{:?}",
            fragments[0]
        );
        let risen_hits = tables(risen);
        assert!(!risen_hits.is_empty(), "{:?}", fragments[1]);
        assert!(
            risen_hits
                .iter()
                .all(|(table, _)| table == "CorruptionEntityDamageTable")
        );
        assert!(
            fragments[1]
                .iter()
                .any(|fragment| fragment.starts_with("Attack_Stretch")),
            "{:?}",
            fragments[1]
        );
    }

    #[test]
    fn janszoon_fights_a_player_in_front_of_him() {
        // Live 2026-10-29: First Mate Janszoon took a combat stance next to the player and never
        // swung (retail newchar: `Attack_Far_2H_Axe_01`, `Attack_Close_2H_Axe_04`). His slice's
        // `DifficultyTier` picks his attack tree; his weapon's database holds the hits.
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        const FIGHT_MS: u64 = 20_000;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, JANSZOON_ID, TEST_LEVEL)],
            &[(JANSZOON_ID, asset(JANSZOON))],
        );
        let player = Seen {
            entity: PLAYER,
            position: Position {
                y: super::tests::ROW_Y + 2.0,
                ..player_at(SPAWN_X).position
            },
        };
        // His damage table's rows of the great axe attacks and of the anchor slam
        // (`javelindata_damagetable_damned_firstmate`).
        const AXE_ROW_PREFIX: &str = "Damned_2H_GreatAxe_Melee_";
        const SLAM_ROW_SUFFIX: &str = "_FirstMate_AnchorSlam";
        let mut rows = Vec::new();
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..FIGHT_MS).step_by(TICK_MS as usize) {
            rows.extend(
                mobs.tick(&[player], now, &Flat, 1)
                    .into_iter()
                    .filter_map(|hit| hit.strike.map(|strike| (strike.table, strike.row))),
            );
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER), "{fragments:?}");
        assert!(!rows.is_empty(), "{fragments:?}");
        assert!(
            rows.iter()
                .all(|(table, row)| table == "DamnedFirstMateDamageTable"
                    && (row.starts_with(AXE_ROW_PREFIX) || row.ends_with(SLAM_ROW_SUFFIX))),
            "{rows:?}"
        );
    }

    #[test]
    fn janszoon_slams_and_mixes_his_attacks_on_a_player_next_to_him() {
        // Retail (Linux main quest playthrough, part 1): `Attack_Combo_GreatAxeman_3` at
        // 813217 ms, `Officer_AnchorSlam` at 819480 ms. Before the client's keep rule (a
        // UtilitySelector keeps its last pick only while it is in the executing plan), one
        // random pick of `Attack_Close_2H_Axe_04` came back for minutes.
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        const FIGHT_MS: u64 = 180_000;
        const SLAM: &str = "Officer_AnchorSlam";
        const HEAVY: &str = "Attack_Close_2H_Axe_02";
        const ATTACK_PREFIX: &str = "Attack_";
        const MIN_ATTACK_KINDS: usize = 4;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, JANSZOON_ID, TEST_LEVEL)],
            &[(JANSZOON_ID, asset(JANSZOON))],
        );
        let player = Seen {
            entity: PLAYER,
            position: Position {
                y: super::tests::ROW_Y + 2.0,
                ..player_at(SPAWN_X).position
            },
        };
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..FIGHT_MS).step_by(TICK_MS as usize) {
            mobs.tick(&[player], now, &Flat, 1);
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        assert!(
            fragments.iter().any(|fragment| fragment == SLAM),
            "{fragments:?}"
        );
        let heavies = fragments
            .iter()
            .filter(|fragment| *fragment == HEAVY)
            .count();
        assert!(heavies > 1, "the heavy attack comes back: {fragments:?}");
        let kinds: std::collections::HashSet<&String> = fragments
            .iter()
            .filter(|fragment| fragment.starts_with(ATTACK_PREFIX))
            .collect();
        assert!(kinds.len() >= MIN_ATTACK_KINDS, "{fragments:?}");
    }

    #[test]
    fn janszoon_walks_home_after_the_player_leaves() {
        // Live 2026-10-29: after the player got away, Janszoon stayed where the fight ended.
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        const AGGRO_MS: u64 = 3_000;
        const FIGHT_MS: u64 = 10_000;
        const AWAY_MS: u64 = 40_000;
        /// The player runs this far from his spawn and he follows.
        const DRAWN_M: f32 = 15.0;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, JANSZOON_ID, TEST_LEVEL)],
            &[(JANSZOON_ID, asset(JANSZOON))],
        );
        let spawn = at(&mobs, 0);
        let player_at_x = |x: f32| Seen {
            entity: PLAYER,
            position: Position {
                x,
                y: super::tests::ROW_Y + 2.0,
                z: HEIGHT,
            },
        };
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..FIGHT_MS).step_by(TICK_MS as usize) {
            let x = if now < AGGRO_MS {
                SPAWN_X
            } else {
                SPAWN_X + DRAWN_M
            };
            mobs.tick(&[player_at_x(x)], now, &Flat, 1);
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        let drawn = at(&mobs, 0).horizontal_distance(spawn);
        for now in (FIGHT_MS..FIGHT_MS + AWAY_MS).step_by(TICK_MS as usize) {
            mobs.tick(&[], now, &Flat, 1);
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        let home = at(&mobs, 0).horizontal_distance(spawn);
        assert!(drawn > 2.0, "the fight drew him off: {drawn} {fragments:?}");
        assert!(home < 2.0, "home: {home} m from spawn; {fragments:?}");
    }

    #[test]
    fn janszoon_leashes_home_past_his_combat_leash() {
        // Live 2026-10-29: Janszoon chased the player 51 m from his spawn (his
        // `CombatLeashDistance` is 45 m) and stayed there.
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        const AGGRO_MS: u64 = 3_000;
        const RUN_MS: u64 = 60_000;
        /// Past his 45 m leash.
        const FAR_M: f32 = 60.0;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, JANSZOON_ID, TEST_LEVEL)],
            &[(JANSZOON_ID, asset(JANSZOON))],
        );
        let spawn = at(&mobs, 0);
        let player_at_x = |x: f32| Seen {
            entity: PLAYER,
            position: Position {
                x,
                y: super::tests::ROW_Y + 2.0,
                z: HEIGHT,
            },
        };
        let mut farthest: f32 = 0.0;
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..AGGRO_MS + RUN_MS).step_by(TICK_MS as usize) {
            let x = if now < AGGRO_MS {
                SPAWN_X
            } else {
                SPAWN_X + FAR_M
            };
            mobs.tick(&[player_at_x(x)], now, &Flat, 1);
            farthest = farthest.max(at(&mobs, 0).horizontal_distance(spawn));
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        let home = at(&mobs, 0).horizontal_distance(spawn);
        assert!(
            farthest > 45.0,
            "he chased past his leash: {farthest}; {fragments:?}"
        );
        assert!(home < 2.0, "home: {home} m from spawn; {fragments:?}");
    }

    #[test]
    fn janszoon_walks_home_after_a_chase_past_his_leash() {
        // Live 2026-10-29 (run 6): Janszoon chased the running player to 46.8 m from his spawn
        // (his `CombatLeashDistance` is 45 m), the player got away, and he stood there 5 minutes.
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        const AGGRO_MS: u64 = 3_000;
        /// The player runs away at about the run speed until this time, then is gone.
        const RUN_UNTIL_MS: u64 = 16_000;
        const AFTER_MS: u64 = 60_000;
        const PLAYER_SPEED_MPS: f32 = 4.5;
        let mut mobs = retail_mobs(
            vec![spawn_at(SPAWN_X, JANSZOON_ID, TEST_LEVEL)],
            &[(JANSZOON_ID, asset(JANSZOON))],
        );
        let spawn = at(&mobs, 0);
        let mut farthest: f32 = 0.0;
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..RUN_UNTIL_MS + AFTER_MS).step_by(TICK_MS as usize) {
            // Seconds of a test run.
            #[allow(clippy::cast_precision_loss)]
            let run_s = now.saturating_sub(AGGRO_MS) as f32 / 1_000.0;
            let player = Seen {
                entity: PLAYER,
                position: Position {
                    x: SPAWN_X + PLAYER_SPEED_MPS * run_s,
                    y: super::tests::ROW_Y + 2.0,
                    z: HEIGHT,
                },
            };
            let seen: &[Seen] = if now < RUN_UNTIL_MS { &[player] } else { &[] };
            mobs.tick(seen, now, &Flat, 1);
            farthest = farthest.max(at(&mobs, 0).horizontal_distance(spawn));
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        let home = at(&mobs, 0).horizontal_distance(spawn);
        assert!(farthest > 20.0, "he chased: {farthest}; {fragments:?}");
        assert!(
            home < 2.0,
            "home: {home} m from spawn, farthest {farthest}; {fragments:?}"
        );
    }

    /// The open world level.
    const OPEN_WORLD_LEVEL: &str = "newworld_vitaeeterna";
    /// Janszoon's spawn on the beach of the real level (live 2026-10-29).
    const JANSZOON_SPAWN: [f32; 3] = [7056.03, 3137.94, 41.63];
    /// How near a level spawn is to [`JANSZOON_SPAWN`] to be his (the level's: 1.7 m).
    const JANSZOON_SPAWN_MATCH_M: f32 = 3.0;

    /// The real ground of Janszoon's level.
    type RealGround = nw_sim_core::LevelGround<Arc<nw_world::WorldFile>>;

    /// Janszoon at his beach spawn and the real ground of his level.
    fn janszoon_on_real_ground() -> (Mobs, RealGround) {
        const JANSZOON_ID: &str = "Corrupted_Sailor_StarterBeach_Named_00";
        const JANSZOON: &str = "7214ec6831b058f2a3b60f9b5c285e6c";
        mob_on_real_ground(JANSZOON_ID, JANSZOON, JANSZOON_SPAWN)
    }

    /// The mob of vitals id `vitals_id`, shown with slice asset `slice`, at `position` of the
    /// open world, and the real ground of that level.
    fn mob_on_real_ground(vitals_id: &str, slice: &str, position: [f32; 3]) -> (Mobs, RealGround) {
        use nw_sim_core::LevelGround;
        use nw_world::WorldFile;
        use nw_world::collision::LevelCollision;
        use nw_world::collision_index::CollisionIndex;
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let content = repo().join("local-data/content");
        let world = WorldFile::load(
            &nw_world::world_file::world_file_path(&content, &build, OPEN_WORLD_LEVEL),
            &build,
        )
        .expect("world file");
        let collision = LevelCollision::load(
            &content.join(format!("collision-{build}-{OPEN_WORLD_LEVEL}.bin")),
            &build,
        )
        .expect("collision file");
        let index = Arc::new(CollisionIndex::new(LevelCollision {
            deferred: Vec::new(),
            ..collision
        }));
        let spawn = VitalsSpawn {
            vitals_id: lowercase_crc32(vitals_id),
            level: TEST_LEVEL,
            position,
            ..VitalsSpawn::default()
        };
        (
            retail_mobs(vec![spawn], &[(vitals_id, asset(slice))]),
            LevelGround::new(Arc::new(world), index),
        )
    }

    /// A player on the real ground at `distance_m` from Janszoon's spawn, toward `bearing_rad`.
    fn player_near_janszoon(ground: &RealGround, distance_m: f32, bearing_rad: f32) -> Seen {
        let x = JANSZOON_SPAWN[0] + distance_m * bearing_rad.cos();
        let y = JANSZOON_SPAWN[1] + distance_m * bearing_rad.sin();
        Seen {
            entity: PLAYER,
            position: Position {
                x,
                y,
                z: ground.height_at(x, y).unwrap_or(JANSZOON_SPAWN[2]),
            },
        }
    }

    #[test]
    fn janszoon_walks_home_over_la_roussette_after_a_chase() {
        // Live 2026-10-29 (run 6), on the real ground: Janszoon chased the player from his spawn
        // toward (7086, 3179) and stood where the chase ended: his spawn is 2.3 m above that
        // place, beyond `ResetToHome`'s 2 m vertical threshold.
        const AWAY: [f32; 2] = [7086.0, 3179.0];
        const AGGRO_MS: u64 = 3_000;
        const RUN_UNTIL_MS: u64 = 16_000;
        const AFTER_MS: u64 = 60_000;
        const PLAYER_SPEED_MPS: f32 = 4.5;
        const FIRST_STEP_M: f32 = 2.0;
        let (mut mobs, ground) = janszoon_on_real_ground();
        let home = at(&mobs, 0);
        let bearing = (AWAY[1] - JANSZOON_SPAWN[1]).atan2(AWAY[0] - JANSZOON_SPAWN[0]);
        let mut farthest: f32 = 0.0;
        let mut fragments = Vec::new();
        let mut serial = None;
        for now in (0..RUN_UNTIL_MS + AFTER_MS).step_by(TICK_MS as usize) {
            // Seconds of a test run.
            #[allow(clippy::cast_precision_loss)]
            let run_m =
                FIRST_STEP_M + PLAYER_SPEED_MPS * now.saturating_sub(AGGRO_MS) as f32 / 1_000.0;
            let player = player_near_janszoon(&ground, run_m, bearing);
            let seen: &[Seen] = if now < RUN_UNTIL_MS { &[player] } else { &[] };
            mobs.tick(seen, now, &ground, 1);
            farthest = farthest.max(at(&mobs, 0).horizontal_distance(home));
            played(&mobs, mob_entity(0), &mut serial, &mut fragments);
        }
        let back = at(&mobs, 0).horizontal_distance(home);
        assert!(farthest > 20.0, "he chased: {farthest}; {fragments:?}");
        assert!(
            back < 2.0,
            "home: {back} m from spawn, farthest {farthest}; {fragments:?}"
        );
    }

    #[test]
    fn janszoon_keeps_a_player_who_leads_him_downhill_inside_his_leash() {
        // Live 2026-10-31: the player led Janszoon toward (7075.4, 3155.1); he reset 25.5 m
        // from his waypoint and 2.1 m below it with the player 1.3 m away and reachable (tree
        // node 154, `CheckWaypointDistanceLeash`), his `CombatLeashDistance` 45 m.
        const TOWARD: [f32; 2] = [7075.4, 3155.1];
        const LEAD_M: f32 = 1.5;
        const WALK_MPS: f32 = 2.0;
        const OUT_M: f32 = 30.0;
        const HOLD_MS: u64 = 10_000;
        let (mut mobs, ground) = janszoon_on_real_ground();
        // His spawn's route of the real level: three points by his spawn, 41.9 to 42.5 m high.
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let content = repo().join("local-data/content");
        let mut world = nw_world::WorldFile::load(
            &nw_world::world_file::world_file_path(&content, &build, "newworld_vitaeeterna"),
            &build,
        )
        .expect("world file");
        let spawn = world
            .vitals
            .iter()
            .position(|vitals| {
                (vitals.position[0] - JANSZOON_SPAWN[0])
                    .hypot(vitals.position[1] - JANSZOON_SPAWN[1])
                    < JANSZOON_SPAWN_MATCH_M
            })
            .expect("his spawn");
        world.vitals_ai = vec![world.vitals_ai[spawn].clone()];
        let ai = MobAi::new(retail_data(), AiRates::default(), LookAssets::default());
        mobs.set_ai(crate::mob_ai::resolve_ai(&world, &ai));
        let route = world.vitals_ai[0]
            .path
            .as_ref()
            .and_then(|path| world.paths.get(path))
            .expect("his route");
        let waypoint_z = route
            .points
            .iter()
            .map(|point| point[2])
            .fold(f32::MIN, f32::max);
        let home = at(&mobs, 0);
        let bearing = (TOWARD[1] - JANSZOON_SPAWN[1]).atan2(TOWARD[0] - JANSZOON_SPAWN[0]);
        let mut trace = (None, Vec::new());
        let close = in_front_of_janszoon(&mobs, &ground, JANSZOON_IN_FACE_M);
        run_janszoon(
            &mut mobs,
            &ground,
            |_| Some(close),
            (0, JANSZOON_AGGRO_MS),
            &mut trace,
        );
        assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER), "aggro");
        let walk_ms = (OUT_M / WALK_MPS * 1_000.0) as u64;
        let mut lowest: f32 = 0.0;
        for now in
            (JANSZOON_AGGRO_MS..JANSZOON_AGGRO_MS + walk_ms + HOLD_MS).step_by(TICK_MS as usize)
        {
            // Seconds of a test run.
            #[allow(clippy::cast_precision_loss)]
            let walked =
                (WALK_MPS * now.saturating_sub(JANSZOON_AGGRO_MS) as f32 / 1_000.0).min(OUT_M);
            let player = player_near_janszoon(&ground, walked + LEAD_M, bearing);
            mobs.tick(&[player], now, &ground, 1);
            played(&mobs, mob_entity(0), &mut trace.0, &mut trace.1);
            lowest = lowest.min(at(&mobs, 0).z - waypoint_z);
            assert_eq!(
                mobs.target(mob_entity(0)),
                Some(PLAYER),
                "kept at {now} ms, {} m from home, {} m off its height; {:?}",
                at(&mobs, 0).horizontal_distance(home),
                at(&mobs, 0).z - home.z,
                trace.1
            );
        }
        assert!(
            at(&mobs, 0).horizontal_distance(home) > 20.0,
            "he followed: {:?}",
            trace.1
        );
        assert!(
            lowest < -2.0,
            "the chase went more than 2 m below home: {lowest}"
        );
    }

    #[test]
    fn the_beach_sword_sailor_walks_home_from_every_side() {
        // Hunt test 2026-10-31 (1 run of 4): the named sword sailor of spawn `575fea06` (the
        // crc of its vitals id) stood 18.7 m from its spawn and reset every 2.8 s for minutes
        // without a step home. It is drawn out on each bearing, then the player leaves, or
        // stays beside it and hits it every second as a hunt bot does.
        const SAILOR_ID: &str = "Corrupted_Sailor_1H_Sword_Named_00";
        const SAILOR: &str = "9cdc9dfea35f575fadc0fca2f7f0d29b";
        const SPAWN_XY: [f32; 2] = [7170.98, 3146.57];
        const SPAWN_MATCH_M: f32 = 3.0;
        const OUT_M: f32 = 18.7;
        const LEAD_M: f32 = 1.5;
        const WALK_MPS: f32 = 3.0;
        const AFTER_MS: u64 = 90_000;
        const BEARINGS: u16 = 8;
        const HOME_M: f32 = 3.0;
        const HIT_EVERY_MS: u64 = 1_000;
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let content = repo().join("local-data/content");
        let mut world = nw_world::WorldFile::load(
            &nw_world::world_file::world_file_path(&content, &build, OPEN_WORLD_LEVEL),
            &build,
        )
        .expect("world file");
        let spawn = world
            .vitals
            .iter()
            .position(|vitals| {
                vitals.vitals_id == lowercase_crc32(SAILOR_ID)
                    && (vitals.position[0] - SPAWN_XY[0]).hypot(vitals.position[1] - SPAWN_XY[1])
                        < SPAWN_MATCH_M
            })
            .expect("its spawn");
        let position = world.vitals[spawn].position;
        world.vitals_ai = vec![world.vitals_ai[spawn].clone()];
        let mut stuck = Vec::new();
        for (stay, step) in [false, true]
            .into_iter()
            .flat_map(|stay| (0..BEARINGS).map(move |step| (stay, step)))
        {
            let bearing = std::f32::consts::TAU * f32::from(step) / f32::from(BEARINGS);
            let (mut mobs, ground) = mob_on_real_ground(SAILOR_ID, SAILOR, position);
            let ai = MobAi::new(retail_data(), AiRates::default(), LookAssets::default());
            mobs.set_ai(crate::mob_ai::resolve_ai(&world, &ai));
            let home = at(&mobs, 0);
            let mut trace = (None, Vec::new());
            let close = in_front_of_janszoon(&mobs, &ground, JANSZOON_IN_FACE_M);
            run_janszoon(
                &mut mobs,
                &ground,
                |_| Some(close),
                (0, JANSZOON_AGGRO_MS),
                &mut trace,
            );
            if mobs.target(mob_entity(0)) != Some(PLAYER) {
                stuck.push(format!(
                    "bearing {bearing}, player stays {stay}: no aggro {:?}",
                    trace.1
                ));
                continue;
            }
            let walk_ms = (OUT_M / WALK_MPS * 1_000.0) as u64;
            let mut now = JANSZOON_AGGRO_MS;
            while now < JANSZOON_AGGRO_MS + walk_ms
                && at(&mobs, 0).horizontal_distance(home) < OUT_M
            {
                // Seconds of a test run.
                #[allow(clippy::cast_precision_loss)]
                let walked = WALK_MPS * now.saturating_sub(JANSZOON_AGGRO_MS) as f32 / 1_000.0;
                let x = home.x + (walked + LEAD_M) * bearing.cos();
                let y = home.y + (walked + LEAD_M) * bearing.sin();
                let player = Seen {
                    entity: PLAYER,
                    position: Position {
                        x,
                        y,
                        z: ground.height_at(x, y).unwrap_or(home.z),
                    },
                };
                mobs.tick(&[player], now, &ground, 1);
                played(&mobs, mob_entity(0), &mut trace.0, &mut trace.1);
                now += TICK_MS;
            }
            let out = at(&mobs, 0);
            let mut nearest = out.horizontal_distance(home);
            for later in (now..now + AFTER_MS).step_by(TICK_MS as usize) {
                let beside = |mobs: &Mobs| {
                    let here = at(mobs, 0);
                    let (x, y) = (
                        here.x + LEAD_M * bearing.cos(),
                        here.y + LEAD_M * bearing.sin(),
                    );
                    Seen {
                        entity: PLAYER,
                        position: Position {
                            x,
                            y,
                            z: ground.height_at(x, y).unwrap_or(here.z),
                        },
                    }
                };
                let seen: Vec<Seen> = if stay {
                    vec![beside(&mobs)]
                } else {
                    Vec::new()
                };
                if stay && later % HIT_EVERY_MS < TICK_MS {
                    mobs.provoke(mob_entity(0), PLAYER, later);
                }
                mobs.tick(&seen, later, &ground, 1);
                played(&mobs, mob_entity(0), &mut trace.0, &mut trace.1);
                nearest = nearest.min(at(&mobs, 0).horizontal_distance(home));
            }
            if nearest > HOME_M {
                stuck.push(format!(
                    "bearing {bearing}, player stays {stay}: out at ({}, {}, {}) {} m, nearest home {nearest} m, now ({}, {}, {}); {:?}",
                    out.x,
                    out.y,
                    out.z,
                    out.horizontal_distance(home),
                    at(&mobs, 0).x,
                    at(&mobs, 0).y,
                    at(&mobs, 0).z,
                    trace.1.iter().rev().take(20).collect::<Vec<_>>()
                ));
            }
        }
        assert!(stuck.is_empty(), "{stuck:#?}");
    }

    /// A player on the real ground `distance_m` in front of Janszoon (yaw 0 faces +y).
    fn in_front_of_janszoon(mobs: &Mobs, ground: &RealGround, distance_m: f32) -> Seen {
        let here = at(mobs, 0);
        let yaw = mobs.tree(mob_entity(0)).expect("tree mob").yaw();
        let x = here.x - distance_m * yaw.sin();
        let y = here.y + distance_m * yaw.cos();
        Seen {
            entity: PLAYER,
            position: Position {
                x,
                y,
                z: ground.height_at(x, y).unwrap_or(here.z),
            },
        }
    }

    /// Ticks Janszoon from `from_ms` to `to_ms` with `player` (none when `None`), recording
    /// the fragments he plays; the strikes of his hits.
    fn run_janszoon(
        mobs: &mut Mobs,
        ground: &RealGround,
        player: impl Fn(&Mobs) -> Option<Seen>,
        (from_ms, to_ms): (u64, u64),
        trace: &mut (Option<u32>, Vec<String>),
    ) -> Vec<crate::mob_ai::Strike> {
        let mut strikes = Vec::new();
        for now in (from_ms..to_ms).step_by(TICK_MS as usize) {
            let seen: Vec<Seen> = player(mobs).into_iter().collect();
            strikes.extend(
                mobs.tick(&seen, now, ground, 1)
                    .into_iter()
                    .filter_map(|hit| hit.strike),
            );
            played(mobs, mob_entity(0), &mut trace.0, &mut trace.1);
        }
        strikes
    }

    /// The ambient vision of his slice: a 120 degree cone of 5 m.
    const JANSZOON_IN_FACE_M: f32 = 1.5;
    /// How long he may take to aggro on a player in his face (his tree evaluates each second).
    const JANSZOON_AGGRO_MS: u64 = 3_000;

    #[test]
    fn janszoon_aggroes_on_a_player_in_his_face_on_the_beach() {
        // Live 2026-10-30: the player stood right in front of Janszoon and he did not aggro.
        // He wanders near his spawn while alone; the player steps in front of him at each time.
        const ALONE_MS: [u64; 4] = [0, 10_000, 20_000, 40_000];
        for alone_ms in ALONE_MS {
            let (mut mobs, ground) = janszoon_on_real_ground();
            let mut trace = (None, Vec::new());
            run_janszoon(&mut mobs, &ground, |_| None, (0, alone_ms), &mut trace);
            let player = in_front_of_janszoon(&mobs, &ground, JANSZOON_IN_FACE_M);
            let window = (alone_ms, alone_ms + JANSZOON_AGGRO_MS);
            run_janszoon(&mut mobs, &ground, |_| Some(player), window, &mut trace);
            assert_eq!(
                mobs.target(mob_entity(0)),
                Some(PLAYER),
                "alone {alone_ms} ms: {:?}",
                trace.1
            );
        }
    }

    #[test]
    fn janszoon_throws_his_anchor_at_a_player_who_backs_off() {
        // Retail newchar capture 1246273 ms: Janszoon threw his anchor (`Officer_AnchorToss`)
        // at the player 8.7 m away; the stand-in tree had none (live 2026-10-30).
        const TOSS: &str = "Officer_AnchorToss";
        const RETAIL_TOSS_M: f32 = 8.7;
        const THROW_WITHIN_MS: u64 = 10_000;
        let (mut mobs, ground) = janszoon_on_real_ground();
        let mut trace = (None, Vec::new());
        let close = in_front_of_janszoon(&mobs, &ground, JANSZOON_IN_FACE_M);
        run_janszoon(
            &mut mobs,
            &ground,
            |_| Some(close),
            (0, JANSZOON_AGGRO_MS),
            &mut trace,
        );
        assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER), "{:?}", trace.1);
        let away = in_front_of_janszoon(&mobs, &ground, RETAIL_TOSS_M);
        let window = (JANSZOON_AGGRO_MS, JANSZOON_AGGRO_MS + THROW_WITHIN_MS);
        let strikes = run_janszoon(&mut mobs, &ground, |_| Some(away), window, &mut trace);
        assert!(
            trace.1.iter().any(|fragment| fragment == TOSS),
            "{:?}",
            trace.1
        );
        // His own damage table holds the toss's rows: the windup's damage window, and the
        // anchor his weapon's ranged profile throws (retail: the 40 of `Anchor_Toss`).
        let row_of = |row: &str| {
            strikes
                .iter()
                .find(|strike| strike.table == "DamnedFirstMateDamageTable" && strike.row == row)
        };
        assert!(row_of("Anchor_Toss_Windup").is_some(), "{strikes:?}");
        let thrown = row_of("Anchor_Toss").and_then(|strike| strike.ranged.as_ref());
        assert_eq!(
            thrown.map(|clip| (clip.attack.as_str(), clip.slice.as_deref())),
            Some((
                "Officer_Anchor",
                Some(
                    "slices/characters/spellslices/corrupted_firstmate_anchor_projectile.dynamicslice"
                )
            )),
            "{strikes:?}"
        );
    }

    #[test]
    fn janszoon_keeps_a_player_ten_meters_away_and_aggroes_again_later() {
        // Live 2026-10-30: the player backed off about 10 yards and Janszoon gave up and ran
        // home (his combat leash is 45 m); later he did not aggro on the player in his face.
        const BACKED_OFF_M: f32 = 10.0;
        const HOLD_MS: u64 = 15_000;
        const GONE_MS: u64 = 60_000;
        const BEARINGS: u16 = 8;
        for step in 0..BEARINGS {
            let bearing = std::f32::consts::TAU * f32::from(step) / f32::from(BEARINGS);
            let (mut mobs, ground) = janszoon_on_real_ground();
            let mut trace = (None, Vec::new());
            let close = in_front_of_janszoon(&mobs, &ground, JANSZOON_IN_FACE_M);
            run_janszoon(
                &mut mobs,
                &ground,
                |_| Some(close),
                (0, JANSZOON_AGGRO_MS),
                &mut trace,
            );
            assert_eq!(mobs.target(mob_entity(0)), Some(PLAYER), "{:?}", trace.1);
            let away = player_near_janszoon(&ground, BACKED_OFF_M, bearing);
            let mut now = JANSZOON_AGGRO_MS;
            while now < JANSZOON_AGGRO_MS + HOLD_MS {
                run_janszoon(
                    &mut mobs,
                    &ground,
                    |_| Some(away),
                    (now, now + TICK_MS),
                    &mut trace,
                );
                now += TICK_MS;
                assert_eq!(
                    mobs.target(mob_entity(0)),
                    Some(PLAYER),
                    "bearing {bearing}, {now} ms: {:?}",
                    trace.1
                );
            }
            run_janszoon(
                &mut mobs,
                &ground,
                |_| None,
                (now, now + GONE_MS),
                &mut trace,
            );
            now += GONE_MS;
            let window = (now, now + JANSZOON_AGGRO_MS);
            run_janszoon(
                &mut mobs,
                &ground,
                |mobs| Some(in_front_of_janszoon(mobs, &ground, JANSZOON_IN_FACE_M)),
                window,
                &mut trace,
            );
            assert_eq!(
                mobs.target(mob_entity(0)),
                Some(PLAYER),
                "bearing {bearing}, back in his face: {:?}",
                trace.1
            );
        }
    }

    /// Run with `cargo test --release -p sim --features local-data hundred_thousand -- --ignored
    /// --nocapture`. The layout: 100,000 mobs on a square of 10 km (32 m apart) and 10,000
    /// players spread on it (100 m apart), as on the open world map (14 km): each player has a
    /// few mobs in sight, every mob a few players in its wake radius. Every mob type with a tree
    /// in Server.pak takes its turn.
    #[test]
    #[ignore = "release-mode timing of 100,000 mobs on the retail trees"]
    fn a_hundred_thousand_mobs_think_within_the_tick_budget() {
        /// The scale goal: 10,000 players and 100,000 mobs on one machine.
        const MOBS: usize = 100_000;
        const PLAYERS: usize = 10_000;
        const MOB_SPACING_M: f32 = 32.0;
        const PLAYER_SPACING_M: f32 = 100.0;
        const MOB_ROW: usize = 316;
        const PLAYER_ROW: usize = 100;
        const TICKS: u64 = 300;
        /// One server tick.
        const BUDGET: Duration = Duration::from_millis(33);
        let data = retail_data();
        let with_tree: Vec<[u8; ASSET_ID_LEN]> = data
            .creatures
            .iter()
            .filter(|creature| {
                data.archetypes[creature.archetype as usize]
                    .trees
                    .iter()
                    .any(|tree| tree.tree.is_some())
            })
            .map(|creature| creature.asset)
            .collect();
        let names: Vec<String> = (0..with_tree.len())
            .map(|index| format!("Scale_Mob_{index}"))
            .collect();
        let kinds: Vec<(&str, [u8; ASSET_ID_LEN])> = names
            .iter()
            .map(String::as_str)
            .zip(with_tree.iter().copied())
            .collect();
        let grid_at = |index: usize, row: usize, spacing: f32| {
            (
                SPAWN_X + (index % row) as f32 * spacing,
                ROW_Y_OFFSET + (index / row) as f32 * spacing,
            )
        };
        let spawns: Vec<VitalsSpawn> = (0..MOBS)
            .map(|index| {
                let (x, y) = grid_at(index, MOB_ROW, MOB_SPACING_M);
                VitalsSpawn {
                    vitals_id: lowercase_crc32(kinds[index % kinds.len()].0),
                    level: TEST_LEVEL,
                    position: [x, y, HEIGHT],
                    ..VitalsSpawn::default()
                }
            })
            .collect();
        let mut mobs = retail_mobs(spawns, &kinds);
        let players: Vec<Seen> = (0..PLAYERS)
            .map(|index| {
                let (x, y) = grid_at(index, PLAYER_ROW, PLAYER_SPACING_M);
                Seen {
                    entity: index as u64 + 1,
                    position: Position {
                        x: x + PLAYER_SPACING_M / 2.0,
                        y,
                        z: HEIGHT,
                    },
                }
            })
            .collect();
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let mut worst = Duration::ZERO;
        let mut total = Duration::ZERO;
        let mut measured = 0;
        for tick_number in 0..TICKS {
            let started = Instant::now();
            mobs.tick(&players, tick_number * TICK_MS, &Flat, threads);
            let took = started.elapsed();
            // The first second wakes every mob and makes every first plan.
            if tick_number * TICK_MS >= 1_000 {
                worst = worst.max(took);
                total += took;
                measured += 1;
            }
        }
        let average = total / measured;
        let (_, counts) = mobs.snapshot();
        println!(
            "{MOBS} mobs, {PLAYERS} players, {threads} threads: tick average {average:?}, worst {worst:?}, states {counts:?} (asleep, idle, chase, attack, return, dead), tree runs {}",
            mobs.brain_runs()
        );
        assert!(average < BUDGET, "{average:?}");
    }

    /// The rows of the scale test start here.
    const ROW_Y_OFFSET: f32 = 1_000.0;
}
