// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tutorial of one player (live-tutorial note): its quest's task tree, its fights and its
//! move into the water.
//!
//! The quest `01_SoleSurvivor2` runs as a task tree ([`crate::objectives`]); the achievements it
//! sets unlock the client's videos, cutscenes, hints and doors. Beside the tree, the server runs
//! what the level's encounters do on retail:
//!
//! - The ship fight (`ftue_v2_enc_boatwreck`): one corrupted sailor waits near the player until
//!   the client ends its quick-time lesson (6638), then fights; when it dies, two more come.
//! - While the ship-sinking video plays, the player moves into the water (retail: 13.5 s after
//!   the video started, with no loading screen).
//! - Later fights start when tasks of the tree are done: the legionnaire after the ruin door, the
//!   signifier at the first fight area, the corruption entity and its swarmers at the climb, and
//!   Thorpe after the fountain cutscene.
//! - Thorpe is not killed by a hit on retail (tutorial-ai note, section 6): at 60 % health he
//!   stops for an intermission, heals to full and comes back; at 50 % of his second health bar
//!   his scripted death comes, and the client starts the next cutscene. His death is no kill of
//!   the quest.
//! - The island's static entities (ftue-static-entities note) show after the move into the water.
//! - The beach bag: after the client takes all its items (2939), retail completes two of its
//!   delayed events and removes it.
//! - The ruin chest: the client starts the interaction in its input; the server accepts it, and
//!   after the gather time ends it and gives two health potions.
//!
//! Every mob of the tutorial is personal: only its player sees it. A dead mob's body stays for
//! its kind's retail time, then it goes.
//!
//! How the mobs fight: a mob whose slice names a tree Server.pak has runs it (the risen
//! `corruptionentity_base` and the swarmers `corruptionswarmer_base`; `crate::mob_ai`). Three
//! have no tree in Server.pak (behavior-tree study, section 4.1, Observed: the club sailors'
//! `drowned/ftue/drowned_qte_basebehavior`, the legionnaires' `legion/legion_ftue/
//! legion_ftue_basebehavior`, Thorpe's `quest_npcs/ftue_thorpe_v2/ftue_thorpe_v2_basebehavior`;
//! the trace replay showed that no tree in the pak plays what they played, trace finding b):
//! they keep the paces measured from the retail tutorial below.

use std::sync::Arc;

use nw_formats::objectstream::lowercase_crc32;
use nw_sim_core::{Gait, Position, TUTORIAL_START};

use nw_content::ftue_entities::{FtueLevel, GDE_REF_PREFIX_LEN};
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{ClientFacetMsg, InteractionRequest};

use super::asset_script::{AssetScript, SpellCast, Step, Trigger};
use crate::mobs::{
    Capsule, MobMove, MobReach, Mobs, MoveFragment, MoveHit, Pace, PaceAttack, PhaseTwo,
    VitalsSpawn,
};
use crate::objectives::{Effect, Event, KillTypes, ObjectiveRun, ObjectiveTree, TaskProgress};

/// When the first sailor comes after the quest starts (retail: its card came 15.9 s after the
/// player's first own state, 316 to 16198 ms, when the encounter's `Wave_01` spawns the mariner;
/// a card sent at once came before the client could show it, and the sailor stayed unseen).
pub(super) const FIRST_SAILOR_DELAY_MS: u64 = 15_882;
/// The level of every tutorial mob (retail cards).
const TUTORIAL_MOB_LEVEL: u32 = 1;
/// How long a dead mob's body stays, by kind (retail: from its 3916 to its slot's stop; sailors
/// 5105, 5105 and 5136 ms; legionnaire 6055, signifier 6087; risen 7041; swarmers 7101-7123).
const SAILOR_CORPSE_MS: u64 = 5_105;
const LEGION_CORPSE_MS: u64 = 6_055;
const RISEN_CORPSE_MS: u64 = 7_041;
const SWARMER_CORPSE_MS: u64 = 7_101;
/// The video during which the player moves into the water.
const SHIP_SINKING_VIDEO: &str = "ftue_intro3";
/// Time from the start of that video to the move (retail 13.5 s).
const WATER_MOVE_DELAY_MS: u64 = 13_500;
/// Where the player comes into the water (retail own position after the move).
pub(super) const WATER_AT: Position = Position {
    x: 793.84,
    y: 337.42,
    z: 26.16,
};
/// Where the player faces in the water (retail own state 118546: rotation `66f6`, yaw 1.434 rad).
pub(super) const WATER_FACING: f32 = 1.434;
/// Thorpe's intro cutscene (6703 and 6706 `c723c42d`): the client asks for it as it comes
/// into his arena.
const THORPE_INTRO_CUTSCENE: u32 = 0xc723_c42d;
/// Time from the client's start of that cutscene (6706) to the server's move of the player into
/// the arena (retail: 1,730 ms in newchar-linux 2026-09-12, 1,810 ms in newchar-retail
/// 2026-09-18; their middle). The client does not move its player in the cutscene: without the
/// move the server kept the player on the ledge 8.85 m away, where Thorpe then fought (live
/// 2026-09-26).
const ARENA_MOVE_DELAY_MS: u64 = 1_770;
/// Where the move puts the player: `TeleDest01` of `ftue_v2_teleportplayer_thorpe01` (the
/// encounter's `FTUE_V2_TeleportPlayer_Thorpe01__spawner`), the retail own position of both
/// runs (pos10 `43eff6644364b2a11ca4`).
pub(super) const ARENA_AT: Position = Position {
    x: 479.92,
    y: 228.70,
    z: 23.07,
};
/// Where the player faces after the move (retail rotation `407f7f54`, both runs).
pub(super) const ARENA_FACING: f32 = -2.654;
/// Thorpe's intermission starts when his health falls to this part of his maximum (retail
/// `Boss_Phase` entry: health 60 %; he stopped at 59.5 %).
const CAPTAIN_INTERMISSION_FRACTION: f64 = 0.6;
/// Thorpe's boss phase in his intermission (retail 403 at 493283).
const CAPTAIN_INTERMISSION_PHASE: u32 = 1;
/// Thorpe's boss phase at his end (retail 403 at 546981, with his death).
const CAPTAIN_END_PHASE: u32 = 3;
/// When he heals to full after the intermission starts (retail: 493283 to 499917 ms). Until then
/// hits on him do no damage (retail: five 2071 with amount 0 from 494210 to 499506).
const CAPTAIN_INTERMISSION_MS: u64 = 6_634;
/// How long he stands in his phase transition before he fights again (retail fragment
/// `Phase_Transition_1_to_2` from 493283 to 503682 ms).
const CAPTAIN_TRANSITION_MS: u64 = 10_399;
/// After the intermission, Thorpe's scripted death comes at this part of his maximum (retail
/// `Boss_Phase` entry: health 50 %; he ended at 48.1 %).
const CAPTAIN_END_FRACTION: f64 = 0.5;
/// The first `GdeRef` bytes of the beach bag (`FTUE_Satchel`).
pub(super) const BAG_PREFIX: [u8; GDE_REF_PREFIX_LEN] =
    [0x2f, 0xb4, 0x00, 0x65, 0x01, 0x28, 0x98, 0xd5];
/// The bag's change groups in the table: its first and second delayed event completed.
const BAG_FIRST_EVENT_CHANGE: usize = 0;
const BAG_SECOND_EVENT_CHANGE: usize = 1;
/// Retail times from the take to those changes and to the bag's removal.
const BAG_FIRST_EVENT_MS: u64 = 134;
const BAG_SECOND_EVENT_MS: u64 = 1282;
const BAG_GONE_MS: u64 = 1298;
/// The first `GdeRef` bytes of the ruin chest (gatherable `LootContainer_AncientSmall`).
pub(super) const CHEST_PREFIX: [u8; GDE_REF_PREFIX_LEN] =
    [0xef, 0xa5, 0xa0, 0x9f, 0x81, 0x35, 0x70, 0x58];
/// The chest's change groups in the table: in use, then free again.
const CHEST_BUSY_CHANGE: usize = 0;
const CHEST_FREE_CHANGE: usize = 1;
/// Retail time from the server's accept to the end of the gather.
const CHEST_GATHER_MS: u64 = 2023;
/// The place the loot tracker gives the looted chest (retail newchar 2026-09-18, at 216.82 s).
pub(super) const CHEST_AT: Position = point(663.0765, 414.5738, 26.66415);
/// How long the loot tracker keeps the chest looted (retail newchar: reset 3,600 s after the
/// loot).
pub(super) const CHEST_LOCKOUT_MS: u64 = 3_600_000;
/// The lore the player learns when the quest ends (retail achievement bits 21-25, 27-31 and
/// 54-60 at 653960; xp-potion-exit note, section 3.2).
const LEAVING_LORE: [&str; 17] = [
    "Lore_Unlock_DeathandRebirthLore1a",
    "Lore_Unlock_DeathandRebirthLore1b",
    "Lore_Unlock_DeathandRebirthLore1c",
    "Lore_Unlock_DeathandRebirthLore1d",
    "Lore_Unlock_DeathandRebirthLore2",
    "Lore_Unlock_DeathandRebirthLore4a",
    "Lore_Unlock_DeathandRebirthLore4b",
    "Lore_Unlock_DeathandRebirthLore4c",
    "Lore_Unlock_DeathandRebirthLore4d",
    "Lore_Unlock_SealedCommission",
    "Lore_Unlock_GettingStarted_All",
    "Lore_Unlock_Mechanics_All",
    "Lore_Unlock_Crafting_All",
    "Lore_Unlock_TradingPost_All",
    "Lore_Unlock_Progression_All",
    "Lore_Unlock_Companies_All",
    "Lore_Unlock_Combat_All",
];
/// What the chest gives (retail: two health potions).
const CHEST_ITEM: &str = "PotionHealthT1";
const CHEST_QUANTITY: u32 = 2;

/// What a tutorial mob is to the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A sailor of the ship fight.
    Sailor,
    /// A mob of a later fight.
    Foe,
    /// Thorpe, the captain.
    Captain,
}

/// One mob a fight spawns.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SpawnPoint {
    /// Its vitals row.
    vitals: &'static str,
    /// Its vitals category row; `None` sends category 0 (retail Thorpe card).
    category: Option<&'static str>,
    at: Position,
    role: Role,
    /// Its `CharacterComponent` constant, for the collision call at its death (live-tutorial
    /// note, section 7).
    collision: u64,
    /// How it fights without a tree: its measured pace; `None` for a mob that runs its tree.
    pace: Option<Pace>,
    /// When it comes after its fight starts.
    delay_ms: u64,
    /// How long its body stays after its death; `None`: until the player leaves the tutorial.
    corpse_ms: Option<u64>,
}

/// How the tutorial mobs fight (tutorial-ai and creature-animation session notes, live capture
/// 2026-09-12). The attack factor makes one landed hit as large as retail's on a player in
/// starter armor (keeping 0.6006): level 1 base damage 145.52 x the type's damage mod x factor x
/// 0.6006 = the retail 3601 amount. The swing gap is the AI's attack cooldown plus the attack's
/// length; the speed is its combat run speed; the hit delay is when retail's hit landed after the
/// attack started.
///
/// A pace's attack names the creature's damage table (its slice names it), the row of its plain
/// swing and the weapon item of its slice's paperdoll: a blocked hit takes the weapon's
/// `BlockStaminaDamage` x the row's `BlockStaminaDmgMod` x (1 - the blocker's `BlockStability` /
/// 100), the hit's 3601 tag is the row's damage type (combat audit, 2026-09-18).
const SAILOR_ATTACK: PaceAttack = PaceAttack {
    table: "DamnedDamageTable_Damned_FTUE",
    row: "Damned_1H_Club_Primary",
    weapon: "1hClubT2FTUE",
};
const LEGION_ATTACK: PaceAttack = PaceAttack {
    table: "LegionLegionnaireDamageTable",
    row: "Legionnaire_LightStab",
    weapon: "Legion_1HSpear",
};
/// Thorpe's: his attack factor fits his phase-one stab, `Attack_Stab_02_P1` (25.6 at 457997).
const THORPE_ATTACK: PaceAttack = PaceAttack {
    table: "DamnedCommanderFireDamageTable_FTUE",
    row: "Attack_Stab_02_P1",
    weapon: "1hSwordT2DamnedCommanderThorpeFire",
};
/// The body capsules of the paced tutorial creatures (collision note, section 2: their slices'
/// `CharacterComponent`).
const SAILOR_CAPSULE: Capsule = Capsule {
    radius_m: 0.4,
    height_m: 1.82,
};
const ISLAND_CAPSULE: Capsule = Capsule {
    radius_m: 0.5,
    height_m: 1.8,
};
/// Thorpe's body: radius 0.7 m (Observed); height Inferred: the slice's 1.4 m is the height
/// between the two round ends, so the whole body stands 1.4 + 2 x 0.7 = 2.8 m (a 2.5 m boss).
const THORPE_CAPSULE: Capsule = Capsule {
    radius_m: 0.7,
    height_m: 2.8,
};
/// The lunges of their swings (collision note, section 3: the `Homing` and `CAGE-Damage` clips of
/// their attacks; the reach of the hit shape in front of the body is Inferred from the clips'
/// shapes, and every retail mob hit landed inside it plus the player's radius).
const SAILOR_REACH: MobReach = MobReach {
    lunge_max_m: 2.0,
    lunge_turn_rad: std::f32::consts::FRAC_PI_4,
    reach_m: 1.4,
};
const LEGION_REACH: MobReach = MobReach {
    lunge_max_m: 2.0,
    lunge_turn_rad: std::f32::consts::FRAC_PI_4,
    reach_m: 2.0,
};
/// Thorpe's stab: its hit box reaches 1.5 m in front of his root (Observed). His behaviour tree
/// keeps attacking while the target is nearer than 6 m (Observed, Server.pak
/// `ftue_thorpe_combatbehavior`), so his moves step in up to 4 m (Inferred: 6 m less his reach
/// and the player's radius).
const THORPE_REACH: MobReach = MobReach {
    lunge_max_m: 4.0,
    lunge_turn_rad: std::f32::consts::PI,
    reach_m: 1.5,
};
/// The 3601 item scale of the sailors' hits (swing note: 0.6006 on every sailor hit).
const SAILOR_HIT_SCALE: f32 = 0.6006;
/// The 3601 item scale of the legionnaires' and Thorpe's hits (0.5612 on all of the island's).
const ISLAND_HIT_SCALE: f32 = 0.5612;
/// Ship sailors (damage mod 0.5): both landed hits did 12.97; cooldown 1-2 s + attack 1.5-2.0 s;
/// run 5.5 m/s; hit at 456 ms. Its swing: the club primary (Inferred: no blocked sailor hit is in
/// the capture).
///
/// Gait (ship-mobs note, section 2.1): swings started at 0.89-1.81 m; runs only closed gaps of
/// 4-12 m; nearer it walked (`WalkToTarget` max 1.25 m/s).
const SAILOR_PACE: Pace = Pace {
    attack_factor: 0.2968,
    swing_gap_ms: (2_500, 4_000),
    speed_mps: 5.5,
    hit_delay_ms: 456,
    attack: SAILOR_ATTACK,
    gait: Gait {
        swing_start_m: 1.8,
        run_beyond_m: 4.0,
        walk_speed_mps: 1.25,
    },
    // Its sailor slice ambient vision 30 m (tutorial-ai-behavior note).
    sight_m: 30.0,
    hit_scale: SAILOR_HIT_SCALE,
    capsule: SAILOR_CAPSULE,
    reach: Some(SAILOR_REACH),
    moves: &[],
    phase_two: None,
};
/// Legionnaire and signifier (damage mod 0.7): 18.70 (262336); cooldown 1-1.5 s + stab about
/// 0.9-1.2 s; run 5.5 m/s; hit at 627 ms. A blocked light stab took 14.6 stamina (266620).
///
/// Gait (tutorial-ai-behavior note, section 3): it attacks at 1.5 m and stabs to 3 m (Inferred:
/// runs beyond 3 m); strafe max 1.25 m/s. Without a target it stands at its spawn (retail; live
/// 2026-09-18, a wander took it into the ruin wall).
const LEGION_PACE: Pace = Pace {
    attack_factor: 0.3057,
    swing_gap_ms: (1_900, 2_700),
    speed_mps: 5.5,
    hit_delay_ms: 627,
    attack: LEGION_ATTACK,
    gait: Gait {
        swing_start_m: 1.5,
        run_beyond_m: 3.0,
        walk_speed_mps: 1.25,
    },
    // Its legion slice ambient vision 12 m (tutorial-ai-behavior note).
    sight_m: 12.0,
    hit_scale: ISLAND_HIT_SCALE,
    capsule: ISLAND_CAPSULE,
    reach: Some(LEGION_REACH),
    moves: &[],
    phase_two: None,
};
/// One hit of a Thorpe move at `at_ms` after its start, of the row `row` of his table
/// (`javelindata_damagetable_damned_commander_ftue`): its coefficient over his attack's, its
/// damage type, its `AI_FireDMG` affix and its `BlockStaminaDmgMod`. His first phase swings the
/// `_P1` rows, which have no fire affix; his second phase the plain rows.
const fn thorpe_hit(at_ms: u64, row: &'static str) -> MoveHit {
    MoveHit { at_ms, row }
}

const fn fragment(fragment: u16, action: u16, tail: u8) -> MoveFragment {
    MoveFragment {
        fragment,
        action,
        tail,
    }
}

/// Thorpe's attacks (boss note, from `commander_controllerdefs.xml` and the retail fight): the
/// fragment, its length, and when each hit landed after it started.
const THORPE_STAB_01: MobMove = MobMove {
    fragment: fragment(58, 28, 0xa5),
    length_ms: 1_434,
    hits: &[thorpe_hit(595, "Attack_Stab_01_P1")],
};
const THORPE_STAB_02: MobMove = MobMove {
    fragment: fragment(59, 29, 0x85),
    length_ms: 1_528,
    hits: &[thorpe_hit(811, "Attack_Stab_02_P1")],
};
const THORPE_SWIPE_L: MobMove = MobMove {
    fragment: fragment(48, 30, 0x6d),
    length_ms: 2_040,
    hits: &[thorpe_hit(1_020, "Attack_SwipeL_01_P1")],
};
const THORPE_SWIPE_R: MobMove = MobMove {
    fragment: fragment(50, 33, 0xd1),
    length_ms: 2_180,
    hits: &[thorpe_hit(944, "Attack_SwipeR_01_P1")],
};
/// Three hits: two `Attack_Flurry_Combo` and the `Attack_Flurry_Finisher`.
const THORPE_FLURRY: MobMove = MobMove {
    fragment: fragment(53, 19, 0x8e),
    length_ms: 2_800,
    hits: &[
        thorpe_hit(781, "Attack_Flurry_Combo"),
        thorpe_hit(1_249, "Attack_Flurry_Combo"),
        thorpe_hit(1_915, "Attack_Flurry_Finisher"),
    ],
};
/// No retail overhead slash landed, so its hit comes when its damage window opens: the
/// `CAGE-Damage` clip of `Attack_Overhead_Slash` in `npc_commander_1h_swordanims.adb` starts
/// 1.038 s into the fragment (row `Attack_Overhead_Slash`). The retail hits of his other moves
/// landed 0 to 117 ms after their windows opened in the same file (stab 01 at 0.594 s: 595 ms;
/// flurry at 0.725, 1.219 and 1.860 s: 781, 1249 and 1915 ms).
const THORPE_OVERHEAD: MobMove = MobMove {
    fragment: fragment(54, 23, 0x2f),
    length_ms: 2_233,
    hits: &[thorpe_hit(1_038, "Attack_Overhead_Slash")],
};
/// His first phase: the stabs and swipes of the retail fight.
const THORPE_MOVES: [MobMove; 4] = [
    THORPE_STAB_01,
    THORPE_STAB_02,
    THORPE_SWIPE_L,
    THORPE_SWIPE_R,
];
/// His second-phase stab and right swipe: the rows with fire.
const THORPE_STAB_02_FIRE: MobMove = MobMove {
    hits: &[thorpe_hit(811, "Attack_Stab_02")],
    ..THORPE_STAB_02
};
const THORPE_SWIPE_R_FIRE: MobMove = MobMove {
    hits: &[thorpe_hit(944, "Attack_SwipeR_01")],
    ..THORPE_SWIPE_R
};
/// His second phase: retail swung the flurry 4 times in 7 attacks, so it is in twice.
const THORPE_PHASE_TWO_MOVES: [MobMove; 5] = [
    THORPE_STAB_02_FIRE,
    THORPE_SWIPE_R_FIRE,
    THORPE_FLURRY,
    THORPE_FLURRY,
    THORPE_OVERHEAD,
];
/// Thorpe's second phase: status `AI_FTUE_Thorpe_Phase_2` adds 1.2 to his damage (retail: stab
/// 42.259 + 14.086 = 0.28 x 2.2 of 91.47).
const THORPE_PHASE_TWO: PhaseTwo = PhaseTwo {
    moves: &THORPE_PHASE_TWO_MOVES,
    damage_mult: 2.2,
};

/// His shoulder charge's outro: an `Attack_Upwards_Slash` (0.38, half fire) 117 ms in (retail
/// 506949 to 507066, 38.235 + 38.235).
const THORPE_CHARGE_OUTRO: MobMove = MobMove {
    fragment: fragment(68, 27, 0x77),
    length_ms: 1_504,
    hits: &[thorpe_hit(117, "Attack_Upwards_Slash")],
};
/// His phase transition, combat tell and charge (boss note, `commander_controllerdefs.xml`).
const THORPE_PHASE_TRANSITION: MoveFragment = fragment(66, 47, 0x2d);
const THORPE_COMBAT_TELL: MoveFragment = fragment(76, 12, 0x44);
const THORPE_CHARGE_START: MoveFragment = fragment(55, 24, 0xda);
const THORPE_CHARGE_LOOP: MoveFragment = fragment(56, 25, 0x04);
/// The tell starts this long after the transition ends (retail 503682 to 503750) and plays
/// 1367 ms; the charge start plays 1364 ms, its loop 468 ms.
const TELL_AFTER_TRANSITION_MS: u64 = 68;
const TELL_MS: u64 = 1_367;
const CHARGE_START_MS: u64 = 1_364;
const CHARGE_LOOP_MS: u64 = 468;
/// His charge speed (retail 25.7 m/s) and where it stops before the player (Inferred: the
/// retail loop ended about 2 m from the player).
const CHARGE_SPEED_MPS: f32 = 25.7;
const CHARGE_STOP_M: f32 = 2.0;
/// Milliseconds per second.
const MS_PER_SECOND_F32: f32 = 1_000.0;
/// The shockwave of his transition: 7748 ms after it starts it pushes the player 5 m away.
const SHOCKWAVE_AT_MS: u64 = 7_748;
const SHOCKWAVE_PUSH_M: f32 = 5.0;
/// How long a grab pull of the player takes (retail grab pulls: 200 to 300 ms).
const GRAB_PULL_MS: u64 = 282;
/// How long the shockwave's push of the player takes: retail moved the own body from the 2218
/// at 501031 until 501596, its last step of 1 cm or more (newchar 2026-09-12; 90 % of the way
/// at +271 ms, 99 % at +503 ms).
const SHOCKWAVE_PUSH_MS: u64 = 565;
/// His force grab: its fragment and length (retail 2200 to 2232 ms).
const THORPE_FORCE_GRAB: MoveFragment = fragment(47, 14, 0xb8);
const GRAB_MS: u64 = 2_210;
/// The start of the capture's first grab fragment, which the level table's grab spells follow.
const RETAIL_FIRST_GRAB_MS: u64 = 454_984;
/// His first grab may come this long into his fight (retail 3.6 s after the fountain cutscene
/// ended); later ones at least [`GRAB_INTERVAL_MS`] apart (retail 11.3 and 12.2 s).
const GRAB_FIRST_MS: u64 = 3_617;
const GRAB_INTERVAL_MS: u64 = 11_250;
/// He grabs only a player this far away (retail grabs started at 6.6 to 13 m).
const GRAB_MIN_DISTANCE_M: f32 = 6.5;
/// The spell slices of the grab: `thorpe_force_grab_attach` and `thorpe_force_grab`.
const GRAB_SPELLS: [[u8; ASSET_ID_LEN]; 2] = [
    [
        0xd0, 0xb2, 0x9f, 0x87, 0x6d, 0x90, 0x5f, 0xc9, 0x90, 0xf9, 0x73, 0x9c, 0x2e, 0x16, 0x9e,
        0x2b,
    ],
    [
        0x1e, 0xe9, 0x6b, 0x79, 0x10, 0x9f, 0x50, 0x18, 0xa1, 0x9e, 0x46, 0x4b, 0x4b, 0x5b, 0x21,
        0xda,
    ],
];
/// The pull: when the spell's first change comes (retail 456719, 1735 ms into the first grab),
/// to 2 m in front of him (retail 1.1 to 3.9 m).
const GRAB_PULL_AT_MS: u64 = 1_735;
const GRAB_PULL_STOP_M: f32 = 2.0;

/// Thorpe (damage mod 1.125): 25.6 (457997). His AI cools down 1.0-1.35 s after each attack
/// (behavior tree); run 4.5 m/s.
///
/// Gait (boss note): he started his attacks 1.1 to 3.4 m from the player and closed the gap in
/// a straight line at 1.33 to 1.76 m/s; he runs beyond 9 m.
const THORPE_PACE: Pace = Pace {
    attack_factor: 0.2604,
    swing_gap_ms: (1_000, 1_350),
    speed_mps: 4.5,
    hit_delay_ms: 611,
    attack: THORPE_ATTACK,
    gait: Gait {
        swing_start_m: 3.4,
        run_beyond_m: 9.0,
        walk_speed_mps: 1.4,
    },
    // Its Thorpe slice ambient vision 20 m (tutorial-ai-behavior note).
    sight_m: 20.0,
    hit_scale: ISLAND_HIT_SCALE,
    capsule: THORPE_CAPSULE,
    reach: Some(THORPE_REACH),
    moves: &THORPE_MOVES,
    phase_two: Some(THORPE_PHASE_TWO),
};

/// How long after its spawn a mob that picks the player at once announces it: the client must
/// have made the mob first (facets note: retail 955 came 629 to 634 ms after the sailors' cards;
/// sent in the spawn tick live 2026-09-16, the client could not find the mob).
const SPAWN_PICK_DELAY_MS: u64 = 600;

/// The `CharacterComponent` constants of the tutorial mobs' prefabs.
const SAILOR_COLLISION: u64 = 0x2d2e_b7bf_87ae_c03f;
const LEGION_COLLISION: u64 = 0x8d6e_b762_2ca4_b0f0;
const SWARMER_COLLISION: u64 = 0x1283_1ae7_e65b_8fe9;
const CORRUPTION_COLLISION: u64 = 0x4885_e5b5_6a03_82e8;

const fn point(x: f32, y: f32, z: f32) -> Position {
    Position { x, y, z }
}

/// The floor heights of the tutorial fights (height note, retail ALC z from the second state on;
/// the cards carried 0.4 to 1.0 m more and the creatures dropped within 100 ms). Each creature
/// keeps its floor: the ship's deck and the ruins are not in the height map.
///
/// The ship's deck: the three sailors' medians 89.38 to 89.45, the player 89.42.
const SHIP_DECK_Z: f32 = 89.42;
/// The ruin top of the first legionnaire (26.37 to 26.88; card 27.35).
const RUIN_TOP_Z: f32 = 26.45;
/// The courtyard of the second legionnaire (20.26 to 20.42; card 20.99).
const COURTYARD_Z: f32 = 20.33;
/// The floor of the risen and the swarmers (21.32 to 21.41; cards 21.74 and 21.81).
const CORRUPTION_FLOOR_Z: f32 = 21.36;
/// Thorpe's arena floor where he spawns (22.21 for y below 227.5; card 22.25).
const THORPE_FLOOR_Z: f32 = 22.21;

const SAILOR_VITALS: &str = "Corrupted_Sailor_1H_Club_FTUE";
const SAILOR_CATEGORY: Option<&str> = Some("Corrupted_Sailor_1H_Club");
/// The first sailor (retail spawn point `sp01`).
const FIRST_SAILOR: SpawnPoint = SpawnPoint {
    vitals: SAILOR_VITALS,
    category: SAILOR_CATEGORY,
    at: point(467.617, 846.008, SHIP_DECK_Z),
    role: Role::Sailor,
    collision: SAILOR_COLLISION,
    pace: Some(SAILOR_PACE),
    delay_ms: 0,
    corpse_ms: Some(SAILOR_CORPSE_MS),
};
/// When the last sailor comes after the first one died (the wave's slice delays are 0 s and 1 s,
/// Observed: cards 55 ms and 1058 ms after the first sailor's 3916).
pub(super) const LAST_SAILOR_DELAY_MS: u64 = 1_000;
/// The two later sailors (retail `sp01` and `sp02` of the second wave).
const LATER_SAILORS: [SpawnPoint; 2] = [
    SpawnPoint {
        at: point(468.734, 847.221, SHIP_DECK_Z),
        ..FIRST_SAILOR
    },
    SpawnPoint {
        at: point(468.734, 845.17, SHIP_DECK_Z),
        delay_ms: LAST_SAILOR_DELAY_MS,
        ..FIRST_SAILOR
    },
];
const LEGION_VITALS: &str = "Legion_Legionnaire_FTUE";
const LEGION_CATEGORY: Option<&str> = Some("Legion_Legionnaire");
const CORRUPTION_CATEGORY: Option<&str> = Some("Corruption_Entity");
const SWARMER_VITALS: &str = "Corruption_Entity_Crawler_FTUE";
const SWARMER: SpawnPoint = SpawnPoint {
    vitals: SWARMER_VITALS,
    category: CORRUPTION_CATEGORY,
    at: point(580.06, 383.2, CORRUPTION_FLOOR_Z),
    role: Role::Foe,
    collision: SWARMER_COLLISION,
    pace: None,
    delay_ms: 1_315,
    corpse_ms: Some(SWARMER_CORPSE_MS),
};
/// A fight of the tutorial: the task that starts it, where a player who dies in it comes back,
/// and its mobs.
struct Fight {
    after: &'static str,
    /// Where the player stood when the fight started (Observed: the client's position at the
    /// task in three live runs, 2026-09-18; the median). Retail's tutorial has one respawn place
    /// per encounter, its `FTUE_V2_TeleportPlayer` destinations, whose level positions this
    /// server does not read (Unknown): this is the evidence-based choice.
    respawn: Position,
    spawns: &'static [SpawnPoint],
}

/// The fights that start when a task is done, with the retail cards' positions.
const FIGHTS_AFTER: [Fight; 4] = [
    Fight {
        after: "01_SoleSurvivor2_UnlockDoor",
        respawn: point(720.59, 362.05, 19.59),
        spawns: &[SpawnPoint {
            vitals: LEGION_VITALS,
            category: LEGION_CATEGORY,
            at: point(668.27, 385.16, RUIN_TOP_Z),
            role: Role::Foe,
            collision: LEGION_COLLISION,
            pace: Some(LEGION_PACE),
            // Retail card 179393, 4200 ms after the door (175193).
            delay_ms: 4_200,
            corpse_ms: Some(LEGION_CORPSE_MS),
        }],
    },
    Fight {
        after: "01_SoleSurvivor2_FTUE_First_Fight_Trigger",
        respawn: point(697.25, 379.50, 24.34),
        spawns: &[SpawnPoint {
            vitals: LEGION_VITALS,
            category: LEGION_CATEGORY,
            at: point(621.6, 419.5, COURTYARD_Z),
            role: Role::Foe,
            collision: LEGION_COLLISION,
            pace: Some(LEGION_PACE),
            // Retail card 202592, 5782 ms after the trigger (196810).
            delay_ms: 5_782,
            corpse_ms: Some(LEGION_CORPSE_MS),
        }],
    },
    Fight {
        after: "01_SoleSurvivor2_Combat02_Trigger_Failsafe_ForClimb",
        respawn: point(606.66, 412.75, 20.03),
        spawns: &[
            SpawnPoint {
                vitals: "Corruption_Entity",
                category: CORRUPTION_CATEGORY,
                at: point(579.62, 381.91, CORRUPTION_FLOOR_Z),
                role: Role::Foe,
                collision: CORRUPTION_COLLISION,
                pace: None,
                // Retail cards after the trigger (282975): risen +417 ms, swarmers +1315,
                // +2316 and +3313 ms, each about 50 ms after a spawner timer ends.
                delay_ms: 417,
                corpse_ms: Some(RISEN_CORPSE_MS),
            },
            SWARMER,
            SpawnPoint {
                at: point(580.67, 379.57, CORRUPTION_FLOOR_Z),
                delay_ms: 2_316,
                ..SWARMER
            },
            SpawnPoint {
                at: point(578.21, 380.97, CORRUPTION_FLOOR_Z),
                delay_ms: 3_313,
                ..SWARMER
            },
        ],
    },
    Fight {
        after: "01_SoleSurvivor2_FTUE_TF_cutsceneComplete",
        // The arena entry after the cutscene.
        respawn: point(476.47, 234.96, 26.02),
        spawns: &[SpawnPoint {
            vitals: "Damned_Commander_FTUE",
            category: None,
            at: point(490.25, 209.89, THORPE_FLOOR_Z),
            role: Role::Captain,
            collision: LEGION_COLLISION,
            pace: Some(THORPE_PACE),
            delay_ms: 0,
            // His body stays until the player leaves the tutorial: retail never stopped his
            // slot after his 3916 (newchar 2026-09-12: 546969 to the 502 at 653910; newchar
            // 2026-09-18: 437309 to the 502 at 448027).
            corpse_ms: None,
        }],
    },
];

/// The spawn of `point`.
fn spawn_of(point: &SpawnPoint) -> VitalsSpawn {
    VitalsSpawn {
        vitals_id: lowercase_crc32(point.vitals),
        category_id: point.category.map_or(0, lowercase_crc32),
        level: TUTORIAL_MOB_LEVEL,
        position: [point.at.x, point.at.y, point.at.z],
        is_encounter: true,
        ..VitalsSpawn::default()
    }
}

/// Where Thorpe's script stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptainPhase {
    /// His first health bar.
    First,
    /// He stands still until `until_ms`, then heals to full.
    Intermission { until_ms: u64 },
    /// His second health bar.
    Second,
}

/// Where the ship fight stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipWave {
    /// The first sailor waits for the end of the quick-time lesson.
    Waiting,
    /// The first sailor fights.
    First,
    /// The two later sailors fight, or the fight is over.
    Later,
}

/// What the tutorial asks of the server for its player.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Order {
    /// Set this achievement.
    Achievement(String),
    /// Remove this many of this item.
    RemoveItem(String, u32),
    /// Give this many of this item.
    GiveItem(String, u32),
    /// The player's potions of this item become a new stack of this many in quick slot 1, and
    /// its cooldowns end (retail when the quest ends).
    RefillPotions(String, u32),
    /// Move the player here, facing `facing` (yaw, radians).
    MoveTo(Position, f32),
    /// The player gets the experience of a kill of a mob of this level (`RewardData`
    /// `KillExperience`, times `javelin.xp-multipliers.character`; retail: 20 per legion or
    /// corrupted kill, 120 in all, xp-potion-exit note, section 1).
    KillExperience(u32),
    /// Clear this achievement.
    LockAchievement(String),
    /// Show the island's static entities.
    ShowIsland,
    /// Send this change group of the static entity with this `GdeRef` start.
    StaticChange([u8; GDE_REF_PREFIX_LEN], usize),
    /// Remove the static entity with this `GdeRef` start.
    HideStatic([u8; GDE_REF_PREFIX_LEN]),
    /// Accept this gather.
    AcceptGather(InteractionRequest),
    /// End this gather.
    EndGather(InteractionRequest),
    /// This mob picked the player as its target; a sailor's tutorial AI also saw it.
    Targeted {
        /// The mob's entity number.
        mob: u64,
        /// Whether it has the tutorial AI (the sailors).
        tutorial_ai: bool,
    },
    /// A group of this asset entity of the level table goes out.
    Asset(usize, Step),
    /// This boss mob is in boss phase `phase` now.
    BossPhase {
        /// The mob's entity number.
        mob: u64,
        /// Its phase.
        phase: u32,
    },
    /// This mob died: it drops the player as its target and stops colliding.
    Died {
        /// The mob's entity number.
        mob: u64,
        /// Its `CharacterComponent` constant.
        collision: u64,
    },
    /// This mob plays this move fragment for this long.
    PlayMove {
        /// The mob's entity number.
        mob: u64,
        /// The fragment.
        fragment: MoveFragment,
        /// How long it plays.
        length_ms: u64,
    },
    /// This mob swings this move at the player.
    Swing {
        /// The mob's entity number.
        mob: u64,
        /// The move.
        mob_move: MobMove,
    },
    /// The server moves the player for `length_ms` as `shove` says from mob `mob`.
    Shove {
        /// The mob's entity number.
        mob: u64,
        /// Where to.
        shove: Shove,
        /// How long the move takes.
        length_ms: u64,
    },
    /// A creature's blow reaches the player without damage (2218).
    Blow,
}

/// Where the server moves the player, from a mob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Shove {
    /// To this distance in front of the mob (a pull).
    To(f32),
    /// This far further away from the mob (a push).
    Away(f32),
}

/// A living tutorial mob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fighter {
    entity: u64,
    vitals_id: u32,
    role: Role,
    collision: u64,
    /// The batch it spawned with.
    batch: usize,
    /// Its pick of the player (2415) went out.
    announced: bool,
    /// When a mob that picks the player at its spawn announces it; `None` for a mob that picks
    /// the player when it first sees it.
    announce_at_ms: Option<u64>,
    /// How long its body stays after its death; `None`: until the player leaves the tutorial.
    corpse_ms: Option<u64>,
}

/// The tutorial of one player.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Tutorial {
    owner: u64,
    run: ObjectiveRun,
    wave: ShipWave,
    /// Where Thorpe's script stands.
    captain: CaptainPhase,
    /// The end of the quest gave its rewards.
    rewarded: bool,
    fighters: Vec<Fighter>,
    /// Dead mobs and when their bodies go; `None`: when the player leaves.
    corpses: Vec<(u64, Option<u64>)>,
    /// When the player moves into the water; `None` before the video or after the move.
    water_at_ms: Option<u64>,
    moved_to_water: bool,
    bag_taken: bool,
    /// Whether a chest gather runs.
    gathering: bool,
    /// The interaction id of the last accepted gather: the client repeats its last input records
    /// in later messages, so one press comes up to three times.
    last_interaction: Option<u64>,
    chest_looted: bool,
    /// Orders due later, with their times.
    later: Vec<(u64, Order)>,
    /// The level's asset entities.
    assets: AssetScript,
    /// Mobs spawned so far.
    spawned: usize,
    /// Mobs of a started fight that come later: when, their batch, and where.
    coming: Vec<(u64, usize, SpawnPoint)>,
    /// The vitals id and card asset of each retail mob, in spawn order: the tutorial spawns its
    /// mobs in the retail order, so the mob of each order place gets that retail mob's look (the
    /// two legionnaires share a vitals id but not a look).
    retail_looks: Vec<(u32, [u8; ASSET_ID_LEN])>,
    /// The spawn order places of each batch of mobs.
    batches: Vec<Vec<usize>>,
    /// The time of the call being handled.
    now_ms: u64,
    /// When the first sailor comes; `None` once it came.
    first_sailor_at_ms: Option<u64>,
    /// Thorpe casts or charges until this time: he starts no other script then.
    captain_busy_until_ms: u64,
    /// When Thorpe may grab next; `None` before his fight.
    next_grab_ms: Option<u64>,
    /// Thorpe's force grabs so far.
    grabs: u32,
    /// Thorpe's charge runs from the first to the second time.
    charge: Option<(u64, u64)>,
    /// The time of the last tick of Thorpe's script.
    captain_ticked_ms: u64,
    /// The player is dead: Thorpe neither charges nor grabs it.
    player_dead: bool,
    /// Where the player comes back after a death: the ship's deck, the water after the ship
    /// sank, then the start of the newest fight (the death screen: "You will gain access to a
    /// choice of respawn locations later in the game").
    respawn_at: Position,
}

impl Tutorial {
    /// The tutorial of player `owner` at `now_ms` in the level of `level`: the quest of `tree`
    /// starts and the first sailor waits in `mobs`. The orders of the quest's first tasks and of
    /// the level's first entities.
    pub(super) fn start(
        owner: u64,
        tree: Arc<ObjectiveTree>,
        level: &FtueLevel,
        mobs: &mut Mobs,
        now_ms: u64,
    ) -> (Self, Vec<Order>) {
        let (run, effects) = ObjectiveRun::start(tree, now_ms);
        let mut tutorial = Self {
            owner,
            run,
            wave: ShipWave::Waiting,
            captain: CaptainPhase::First,
            rewarded: false,
            fighters: Vec::new(),
            corpses: Vec::new(),
            water_at_ms: None,
            moved_to_water: false,
            bag_taken: false,
            gathering: false,
            last_interaction: None,
            chest_looted: false,
            later: Vec::new(),
            assets: AssetScript::of(level),
            spawned: 0,
            coming: Vec::new(),
            retail_looks: level
                .mobs
                .iter()
                .map(|mob| (mob.vitals_id, mob.asset))
                .collect(),
            batches: Vec::new(),
            now_ms,
            first_sailor_at_ms: Some(now_ms.saturating_add(FIRST_SAILOR_DELAY_MS)),
            captain_busy_until_ms: 0,
            next_grab_ms: None,
            grabs: 0,
            charge: None,
            captain_ticked_ms: now_ms,
            player_dead: false,
            respawn_at: TUTORIAL_START,
        };
        tutorial.assets.on(Trigger::Start, now_ms);
        let mut orders = Vec::new();
        tracing::info!(player = owner, "tutorial: the quest starts");
        tutorial.apply(effects, mobs, &mut orders);
        (tutorial, orders)
    }

    /// The first sailor comes when it is due at `now_ms`, or at once when `now`: it waits, held,
    /// for the end of the quick-time lesson.
    fn first_sailor_comes(&mut self, mobs: &mut Mobs, now: bool) {
        if self
            .first_sailor_at_ms
            .is_some_and(|at| now || self.now_ms >= at)
        {
            self.first_sailor_at_ms = None;
            self.spawn_batch(&[FIRST_SAILOR], mobs, true);
            tracing::info!(
                player = self.owner,
                "tutorial: the first sailor comes and waits"
            );
        }
    }

    /// Spawns the mobs of `points` as one batch, held idle when `held`; a mob that fights at once
    /// targets the player [`SPAWN_PICK_DELAY_MS`] later (`announce_targets`).
    fn spawn_batch(&mut self, points: &[SpawnPoint], mobs: &mut Mobs, held: bool) {
        let batch = self.batches.len();
        self.batches.push(Vec::new());
        for point in points {
            if point.delay_ms == 0 {
                self.spawn(point, batch, mobs, held);
            } else {
                let at_ms = self.now_ms.saturating_add(point.delay_ms);
                self.coming.push((at_ms, batch, *point));
            }
        }
    }

    /// Spawns the mobs whose time came, in the order they are due.
    fn spawn_coming(&mut self, mobs: &mut Mobs) {
        let now_ms = self.now_ms;
        let (mut due, coming): (Vec<_>, Vec<_>) = std::mem::take(&mut self.coming)
            .into_iter()
            .partition(|(at_ms, _, _)| now_ms >= *at_ms);
        self.coming = coming;
        due.sort_by_key(|(at_ms, _, _)| *at_ms);
        for (_, batch, point) in due {
            self.spawn(&point, batch, mobs, false);
        }
    }

    fn spawn(&mut self, point: &SpawnPoint, batch: usize, mobs: &mut Mobs, held: bool) {
        let spawn = spawn_of(point);
        let vitals_id = spawn.vitals_id;
        let asset = self
            .retail_looks
            .get(self.spawned)
            .filter(|(retail_vitals, _)| *retail_vitals == vitals_id)
            .map(|(_, asset)| *asset);
        let Some(entity) =
            mobs.spawn_personal(spawn, self.owner, held, point.pace, asset, self.now_ms)
        else {
            tracing::warn!(vitals = point.vitals, "tutorial mob without combat rows");
            return;
        };
        let order = self.spawned;
        self.spawned += 1;
        self.batches[batch].push(order);
        self.assets.on(Trigger::MobSpawned(order), self.now_ms);
        // Retail: the sailors and Thorpe pick the player 0.3-0.6 s after their cards; the island's
        // foes when they first see it (legionnaire 25 s after its card with the player 3.7-8.1 m
        // away, signifier 56 s and 9.8 m, the risen group 7-9 s): see `announce_targets`.
        let picks_at_spawn = !held && point.role != Role::Foe;
        self.fighters.push(Fighter {
            entity,
            vitals_id,
            role: point.role,
            collision: point.collision,
            batch,
            announced: false,
            announce_at_ms: picks_at_spawn.then_some(self.now_ms + SPAWN_PICK_DELAY_MS),
            corpse_ms: point.corpse_ms,
        });
        // Thorpe fights the player from his spawn (retail 2415 at 451137 with the player 26.5 m
        // away, beyond the brain's sight).
        if point.role == Role::Captain && !held {
            mobs.provoke(entity, self.owner, self.now_ms);
        }
    }

    /// The pick of the player (2415) of each mob whose spawn pick is due, or whose brain first
    /// took the player as its target.
    fn announce_targets(&mut self, mobs: &Mobs, orders: &mut Vec<Order>) {
        for fighter in &mut self.fighters {
            let due = fighter.announce_at_ms.map_or_else(
                || mobs.target(fighter.entity) == Some(self.owner),
                |at| self.now_ms >= at,
            );
            if !fighter.announced && due {
                fighter.announced = true;
                orders.push(Order::Targeted {
                    mob: fighter.entity,
                    tutorial_ai: fighter.role == Role::Sailor,
                });
            }
        }
    }

    /// The quick-time lesson ended at `now_ms`: the first sailor fights and targets the player.
    pub(super) fn lesson_done(&mut self, mobs: &mut Mobs, now_ms: u64) -> Vec<Order> {
        if self.wave != ShipWave::Waiting {
            return Vec::new();
        }
        self.now_ms = now_ms;
        // A lesson that ends before the first sailor's time brings it at once.
        let mut orders = Vec::new();
        self.first_sailor_comes(mobs, true);
        mobs.release(self.owner, now_ms);
        self.wave = ShipWave::First;
        tracing::info!(player = self.owner, "tutorial: the first sailor fights");
        for fighter in &mut self.fighters {
            if fighter.role == Role::Sailor {
                fighter.announced = true;
                orders.push(Order::Targeted {
                    mob: fighter.entity,
                    tutorial_ai: true,
                });
            }
        }
        orders
    }

    /// The player takes all items of container `container` at `now_ms`: whether it is the beach
    /// bag and still held them (once per tutorial). The bag then goes as on retail.
    pub(super) fn take_bag(&mut self, container: &[u8; RAW_ID_LEN], now_ms: u64) -> bool {
        if container[..GDE_REF_PREFIX_LEN] != BAG_PREFIX || self.bag_taken {
            return false;
        }
        self.bag_taken = true;
        self.later.extend([
            (
                now_ms + BAG_FIRST_EVENT_MS,
                Order::StaticChange(BAG_PREFIX, BAG_FIRST_EVENT_CHANGE),
            ),
            (
                now_ms + BAG_SECOND_EVENT_MS,
                Order::StaticChange(BAG_PREFIX, BAG_SECOND_EVENT_CHANGE),
            ),
            (now_ms + BAG_GONE_MS, Order::HideStatic(BAG_PREFIX)),
        ]);
        true
    }

    /// The client starts interaction `request` at `now_ms`: a use of the chest is accepted, and
    /// the gather ends after the retail gather time, with the potions the first time.
    pub(super) fn interact(&mut self, request: InteractionRequest, now_ms: u64) -> Vec<Order> {
        if request.target[..GDE_REF_PREFIX_LEN] != CHEST_PREFIX
            || self.gathering
            || self.last_interaction == Some(request.interaction)
        {
            return Vec::new();
        }
        self.gathering = true;
        self.last_interaction = Some(request.interaction);
        let done_at = now_ms + CHEST_GATHER_MS;
        self.later.push((done_at, Order::EndGather(request)));
        self.later.push((
            done_at,
            Order::StaticChange(CHEST_PREFIX, CHEST_FREE_CHANGE),
        ));
        if !std::mem::replace(&mut self.chest_looted, true) {
            self.later.push((
                done_at,
                Order::GiveItem(CHEST_ITEM.to_owned(), CHEST_QUANTITY),
            ));
        }
        tracing::info!(player = self.owner, "tutorial: the chest is opened");
        vec![
            Order::AcceptGather(request),
            Order::StaticChange(CHEST_PREFIX, CHEST_BUSY_CHANGE),
        ]
    }

    /// The client sent `message` at `now_ms`: the level's asset entities that wait for it, and
    /// the move into Thorpe's arena after his intro cutscene started.
    pub(super) fn client_message(&mut self, message: &ClientFacetMsg, now_ms: u64) {
        if let ClientFacetMsg::CutsceneStarted {
            cutscene: THORPE_INTRO_CUTSCENE,
            ..
        } = *message
        {
            self.later.push((
                now_ms.saturating_add(ARENA_MOVE_DELAY_MS),
                Order::MoveTo(ARENA_AT, ARENA_FACING),
            ));
        }
        let call = |message_type, id| Trigger::Client { message_type, id };
        let trigger = match *message {
            ClientFacetMsg::CinematicStarted { cinematic, .. } => call(
                nw_messages::facet::CINEMATIC_STARTED_TYPE_INDEX,
                Some(cinematic),
            ),
            ClientFacetMsg::CinematicEnded { cinematic, .. } => call(
                nw_messages::facet::CINEMATIC_ENDED_TYPE_INDEX,
                Some(cinematic),
            ),
            ClientFacetMsg::RequestCutscene { cutscene, .. } => call(
                nw_messages::facet::REQUEST_CUTSCENE_TYPE_INDEX,
                Some(cutscene),
            ),
            ClientFacetMsg::CutsceneStarted { cutscene, .. } => call(
                nw_messages::facet::CUTSCENE_STARTED_TYPE_INDEX,
                Some(cutscene),
            ),
            ClientFacetMsg::CutsceneEnded { .. } => {
                call(nw_messages::facet::CUTSCENE_ENDED_TYPE_INDEX, None)
            }
            ClientFacetMsg::QteCompleted { sequence, .. } => {
                call(nw_messages::facet::QTE_COMPLETED_TYPE_INDEX, Some(sequence))
            }
            ClientFacetMsg::TakeAllItems { .. } => {
                call(nw_messages::facet::TAKE_ALL_ITEMS_TYPE_INDEX, None)
            }
            ClientFacetMsg::EquipItem { .. } => {
                call(nw_messages::facet::EQUIP_ITEM_TYPE_INDEX, None)
            }
            ClientFacetMsg::ReturnToMainWorld { .. } => {
                call(nw_messages::facet::RETURN_TO_MAIN_WORLD_TYPE_INDEX, None)
            }
            ClientFacetMsg::TriggerAreaPassthrough { entity, .. } => {
                let mut prefix = [0; GDE_REF_PREFIX_LEN];
                prefix.copy_from_slice(&entity[..GDE_REF_PREFIX_LEN]);
                Trigger::Passthrough(prefix)
            }
            ClientFacetMsg::AckLevelInfo { .. }
            | ClientFacetMsg::UiScreenOpened { .. }
            | ClientFacetMsg::TimeSyncRequest { .. }
            | ClientFacetMsg::LookingThroughLoadout { .. }
            | ClientFacetMsg::SwapItems { .. }
            | ClientFacetMsg::UnequipItem { .. }
            | ClientFacetMsg::CancelInteraction { .. }
            | ClientFacetMsg::SetReplicateStorage { .. }
            | ClientFacetMsg::AcceptObjective { .. }
            | ClientFacetMsg::CompleteObjective { .. }
            | ClientFacetMsg::SetCraftingDetails { .. }
            | ClientFacetMsg::CancelCrafting { .. }
            | ClientFacetMsg::InitiateSalvage { .. }
            | ClientFacetMsg::SalvageItems { .. }
            | ClientFacetMsg::SalvageByClass { .. }
            | ClientFacetMsg::TerritoryEntered { .. }
            | ClientFacetMsg::TerritoryExited { .. }
            | ClientFacetMsg::EnteredTerritoryFallback { .. }
            | ClientFacetMsg::ExitedTerritoryFallback { .. }
            | ClientFacetMsg::TogglePvpFlag { .. }
            | ClientFacetMsg::SetFaction { .. }
            | ClientFacetMsg::UpdateTrackedObjectives { .. }
            | ClientFacetMsg::AbandonObjective { .. }
            | ClientFacetMsg::SetInputsThrottled { .. }
            | ClientFacetMsg::SetObjectiveAutoPin { .. } => {
                return;
            }
            ClientFacetMsg::WhisperStarted { whisper, .. } => {
                self.assets.on(
                    Trigger::Client {
                        message_type: nw_messages::facet::WHISPER_STARTED_TYPE_INDEX,
                        id: Some(whisper),
                    },
                    now_ms,
                );
                return;
            }
        };
        self.assets.on(trigger, now_ms);
    }

    /// The server set achievement `bit` at `now_ms`.
    pub(super) fn achievement(&mut self, bit: u32, now_ms: u64) {
        self.assets.on(Trigger::Achievement(bit), now_ms);
    }

    /// The orders of the quest's waiting item tasks that the player's items `held` (item id,
    /// quantity, worn) already meet, at `now_ms`.
    pub(super) fn sync_held(
        &mut self,
        held: &[(&str, u32, bool)],
        mobs: &mut Mobs,
        now_ms: u64,
    ) -> Vec<Order> {
        self.now_ms = now_ms;
        let mut orders = Vec::new();
        let effects = self.run.sync_held(held, now_ms);
        self.apply(effects, mobs, &mut orders);
        orders
    }

    /// The orders of game event `event` at `now_ms`.
    pub(super) fn event(&mut self, event: Event<'_>, mobs: &mut Mobs, now_ms: u64) -> Vec<Order> {
        self.now_ms = now_ms;
        let sinking = lowercase_crc32(SHIP_SINKING_VIDEO);
        let mut orders = Vec::new();
        match event {
            Event::CinematicStarted(video) if video == sinking && !self.moved_to_water => {
                self.water_at_ms = Some(now_ms.saturating_add(WATER_MOVE_DELAY_MS));
            }
            Event::CinematicEnded(video) if video == sinking => self.move_to_water(&mut orders),
            _ => {}
        }
        let effects = self.run.event(event, now_ms);
        self.apply(effects, mobs, &mut orders);
        orders
    }

    fn move_to_water(&mut self, orders: &mut Vec<Order>) {
        if !std::mem::replace(&mut self.moved_to_water, true) {
            self.water_at_ms = None;
            self.respawn_at = WATER_AT;
            orders.push(Order::MoveTo(WATER_AT, WATER_FACING));
            orders.push(Order::ShowIsland);
            self.assets.on(Trigger::WaterMove, self.now_ms);
            tracing::info!(player = self.owner, "tutorial: into the water");
        }
    }

    /// Where the player comes back after a death in the tutorial.
    pub(super) const fn respawn_at(&self) -> Position {
        self.respawn_at
    }

    /// Whether the player lives: a dead player is no target of the script.
    pub(super) const fn player_alive(&mut self, alive: bool) {
        self.player_dead = !alive;
    }

    /// One tick at `now_ms` with the player at `position`: deaths count as kills, the next
    /// sailors come, Thorpe ends, bodies go, timers and later orders run, and areas are checked.
    pub(super) fn tick(
        &mut self,
        mobs: &mut Mobs,
        kill_types: &KillTypes,
        position: Position,
        now_ms: u64,
    ) -> Vec<Order> {
        self.now_ms = now_ms;
        let mut orders = Vec::new();
        self.first_sailor_comes(mobs, false);
        self.spawn_coming(mobs);
        self.end_captain(mobs, now_ms, &mut orders);
        self.captain_moves(mobs, position, now_ms, &mut orders);
        self.announce_targets(mobs, &mut orders);
        let (dead, living): (Vec<Fighter>, Vec<Fighter>) = self
            .fighters
            .iter()
            .partition(|fighter| mobs.health(fighter.entity).is_none_or(|health| health == 0));
        self.fighters = living;
        for fighter in &dead {
            tracing::info!(
                player = self.owner,
                mob = fighter.entity,
                "tutorial: a mob died"
            );
            self.corpses.push((
                fighter.entity,
                fighter.corpse_ms.map(|ms| now_ms.saturating_add(ms)),
            ));
            orders.push(Order::Died {
                mob: fighter.entity,
                collision: fighter.collision,
            });
            let batch_dead = !self
                .fighters
                .iter()
                .any(|living| living.batch == fighter.batch)
                && !self
                    .coming
                    .iter()
                    .any(|(_, batch, _)| *batch == fighter.batch);
            if batch_dead {
                for order in self.batches[fighter.batch].clone() {
                    self.assets.on(Trigger::BatchDead(order), now_ms);
                }
            }
            // Thorpe's end is scripted, not a kill.
            if fighter.role == Role::Captain {
                continue;
            }
            // Retail gives experience for the island's foes only, not for the sailors.
            if fighter.role == Role::Foe {
                orders.push(Order::KillExperience(TUTORIAL_MOB_LEVEL));
            }
            if let Some((vitals_id, categories)) = kill_types.of(fighter.vitals_id) {
                let effects = self.run.event(
                    Event::Killed {
                        vitals_id,
                        categories,
                        // The tutorial's fighters come from its script, not from point spawners.
                        spawn_categories: &[],
                    },
                    now_ms,
                );
                self.apply(effects, mobs, &mut orders);
            }
        }
        if self.wave == ShipWave::First && dead.iter().any(|dead| dead.role == Role::Sailor) {
            self.wave = ShipWave::Later;
            self.spawn_batch(&LATER_SAILORS, mobs, false);
            tracing::info!(player = self.owner, "tutorial: two more sailors come");
        }
        self.corpses.retain(|(mob, gone_at_ms)| {
            let stays = gone_at_ms.is_none_or(|gone_at_ms| now_ms < gone_at_ms);
            if !stays {
                mobs.remove_personal(*mob);
            }
            stays
        });
        if self.water_at_ms.is_some_and(|at| now_ms >= at) {
            self.move_to_water(&mut orders);
        }
        let effects = self.run.tick(now_ms);
        self.apply(effects, mobs, &mut orders);
        let effects = self.run.event(Event::Moved(position), now_ms);
        self.apply(effects, mobs, &mut orders);
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.later)
            .into_iter()
            .partition(|(at, _)| now_ms >= *at);
        self.later = later;
        for (_, order) in due {
            if matches!(order, Order::EndGather(_)) {
                self.gathering = false;
            }
            orders.push(order);
        }
        orders.extend(
            self.assets
                .take_due(now_ms)
                .into_iter()
                .map(|(asset, step)| Order::Asset(asset, step)),
        );
        orders
    }

    /// Runs Thorpe's script at `now_ms`: the intermission at 60 % health, the full heal after
    /// it, and the end at 50 % of his second health bar; the intermission and the end set his
    /// boss phase.
    fn end_captain(&mut self, mobs: &mut Mobs, now_ms: u64, orders: &mut Vec<Order>) {
        let captains: Vec<u64> = self
            .fighters
            .iter()
            .filter(|fighter| fighter.role == Role::Captain)
            .map(|fighter| fighter.entity)
            .collect();
        for captain in captains {
            let Some(mob) = mobs.get_mut(captain) else {
                continue;
            };
            let (current, max) = (mob.health.current(), mob.health.max());
            let below =
                |fraction: f64| current > 0 && f64::from(current) <= f64::from(max) * fraction;
            match self.captain {
                CaptainPhase::First if below(CAPTAIN_INTERMISSION_FRACTION) => {
                    let until_ms = now_ms + CAPTAIN_INTERMISSION_MS;
                    self.captain = CaptainPhase::Intermission { until_ms };
                    mobs.absorb(captain, until_ms);
                    let busy_until_ms = self.transition(captain, now_ms, orders);
                    mobs.freeze(captain, busy_until_ms);
                    orders.push(Order::BossPhase {
                        mob: captain,
                        phase: CAPTAIN_INTERMISSION_PHASE,
                    });
                    tracing::info!(player = self.owner, "tutorial: Thorpe's intermission");
                }
                CaptainPhase::Intermission { until_ms } if now_ms >= until_ms => {
                    mob.health.refill();
                    mobs.enter_phase_two(captain);
                    self.captain = CaptainPhase::Second;
                }
                CaptainPhase::Second if below(CAPTAIN_END_FRACTION) => {
                    mob.health.take(current);
                    // A scripted end: the body neither chases nor swings any more.
                    mobs.died(captain, now_ms);
                    orders.push(Order::BossPhase {
                        mob: captain,
                        phase: CAPTAIN_END_PHASE,
                    });
                    tracing::info!(player = self.owner, "tutorial: Thorpe is beaten");
                }
                _ => {}
            }
        }
    }

    /// Thorpe's phase transition starts at `now_ms` (retail 493283): its fragment plays for the
    /// whole transition; his shockwave knocks the player away (retail 501031: a 2218 with no
    /// damage and a 5 m push); then he tells and charges (503750 to 507066). Until when he is
    /// busy.
    fn transition(&mut self, captain: u64, now_ms: u64, orders: &mut Vec<Order>) -> u64 {
        let play = |fragment, length_ms| Order::PlayMove {
            mob: captain,
            fragment,
            length_ms,
        };
        orders.push(play(THORPE_PHASE_TRANSITION, CAPTAIN_TRANSITION_MS));
        let shockwave_ms = now_ms + SHOCKWAVE_AT_MS;
        self.later.push((shockwave_ms, Order::Blow));
        self.later.push((
            shockwave_ms,
            Order::Shove {
                mob: captain,
                shove: Shove::Away(SHOCKWAVE_PUSH_M),
                length_ms: SHOCKWAVE_PUSH_MS,
            },
        ));
        let tell_ms = now_ms + CAPTAIN_TRANSITION_MS + TELL_AFTER_TRANSITION_MS;
        let charge_ms = tell_ms + TELL_MS;
        let loop_ms = charge_ms + CHARGE_START_MS;
        let outro_ms = loop_ms + CHARGE_LOOP_MS;
        self.later
            .push((tell_ms, play(THORPE_COMBAT_TELL, TELL_MS)));
        self.later
            .push((charge_ms, play(THORPE_CHARGE_START, CHARGE_START_MS)));
        self.later
            .push((loop_ms, play(THORPE_CHARGE_LOOP, CHARGE_LOOP_MS)));
        self.later.push((
            outro_ms,
            Order::Swing {
                mob: captain,
                mob_move: THORPE_CHARGE_OUTRO,
            },
        ));
        self.charge = Some((loop_ms, outro_ms));
        self.captain_busy_until_ms = outro_ms + THORPE_CHARGE_OUTRO.length_ms;
        self.captain_busy_until_ms
    }

    /// Thorpe's charge and force grabs at `now_ms`, with the player at `position`. In his charge
    /// loop he runs at the player at 25.7 m/s (retail 506481 to 506949, 12 m). He grabs a player
    /// at least [`GRAB_MIN_DISTANCE_M`] away, at most once per [`GRAB_INTERVAL_MS`] (retail
    /// 454984, 467215, 478467 and 517247): his grab fragment plays, the spell slices come with
    /// him and the player's place, and the spell pulls the player in front of him.
    fn captain_moves(
        &mut self,
        mobs: &mut Mobs,
        position: Position,
        now_ms: u64,
        orders: &mut Vec<Order>,
    ) {
        let last_tick_ms = std::mem::replace(&mut self.captain_ticked_ms, now_ms);
        // A dead player is no target; after its respawn the first grab waits again.
        if self.player_dead {
            self.charge = None;
            self.next_grab_ms = None;
            return;
        }
        let Some(captain) = self
            .fighters
            .iter()
            .find(|fighter| fighter.role == Role::Captain)
            .map(|fighter| fighter.entity)
        else {
            return;
        };
        let Some(at) = mobs
            .get_mut(captain)
            .filter(|mob| !mob.health.is_dead())
            .map(|mob| mob.position)
        else {
            return;
        };
        if let Some((start_ms, end_ms)) = self.charge {
            if now_ms >= end_ms {
                self.charge = None;
            } else if now_ms >= start_ms {
                // He runs only from the loop's start, not from the tick before it.
                let running_ms = now_ms.saturating_sub(last_tick_ms.max(start_ms));
                #[allow(clippy::cast_precision_loss, reason = "one tick of milliseconds")]
                let step_m = CHARGE_SPEED_MPS * running_ms as f32 / MS_PER_SECOND_F32;
                let left_m = (position.horizontal_distance(at) - CHARGE_STOP_M).max(0.0);
                mobs.lunge(captain, position, step_m.min(left_m));
            }
        }
        let next_grab_ms = *self.next_grab_ms.get_or_insert(now_ms + GRAB_FIRST_MS);
        let fighting = matches!(self.captain, CaptainPhase::First | CaptainPhase::Second);
        if !fighting
            || now_ms < next_grab_ms
            || now_ms < self.captain_busy_until_ms
            || position.horizontal_distance(at) < GRAB_MIN_DISTANCE_M
        {
            return;
        }
        self.grabs += 1;
        self.next_grab_ms = Some(now_ms + GRAB_INTERVAL_MS);
        self.captain_busy_until_ms = now_ms + GRAB_MS;
        mobs.freeze(captain, now_ms + GRAB_MS);
        orders.push(Order::PlayMove {
            mob: captain,
            fragment: THORPE_FORCE_GRAB,
            length_ms: GRAB_MS,
        });
        let cast = SpellCast {
            caster: captain,
            at_bits: [position.x.to_bits(), position.y.to_bits()],
            number: self.grabs,
        };
        for spell in &GRAB_SPELLS {
            self.assets.cast(spell, cast, now_ms, RETAIL_FIRST_GRAB_MS);
        }
        self.later.push((
            now_ms + GRAB_PULL_AT_MS,
            Order::Shove {
                mob: captain,
                shove: Shove::To(GRAB_PULL_STOP_M),
                length_ms: GRAB_PULL_MS,
            },
        ));
        tracing::info!(
            player = self.owner,
            grab = self.grabs,
            "tutorial: Thorpe's force grab"
        );
    }

    /// Turns `effects` into orders, and starts the fights of the tasks they finish.
    fn apply(&mut self, effects: Vec<Effect>, mobs: &mut Mobs, orders: &mut Vec<Order>) {
        for effect in effects {
            match effect {
                Effect::Achievement(id) => orders.push(Order::Achievement(id)),
                Effect::LockAchievement(id) => orders.push(Order::LockAchievement(id)),
                Effect::RemoveItem(id, count) => orders.push(Order::RemoveItem(id, count)),
                Effect::GiveItem(id, count) => orders.push(Order::GiveItem(id, count)),
                // The tutorial's tree fires no game event (build 25810808).
                Effect::GameEvent(event) => {
                    tracing::debug!(player = self.owner, %event, "tutorial game event");
                }
                // The tutorial's tree starts no objective (build 25810808).
                Effect::StartObjective(objective) => {
                    tracing::debug!(player = self.owner, %objective, "tutorial objective start");
                }
                Effect::Done(task) => {
                    let fights = FIGHTS_AFTER
                        .iter()
                        .filter(|fight| fight.after.eq_ignore_ascii_case(&task));
                    for fight in fights {
                        self.respawn_at = fight.respawn;
                        self.spawn_batch(fight.spawns, mobs, false);
                    }
                    tracing::debug!(player = self.owner, %task, "tutorial task done");
                }
            }
        }
        if self.run.finished() && !std::mem::replace(&mut self.rewarded, true) {
            orders.extend(
                LEAVING_LORE
                    .iter()
                    .map(|lore| Order::Achievement((*lore).to_owned())),
            );
            orders.push(Order::RefillPotions(CHEST_ITEM.to_owned(), CHEST_QUANTITY));
            tracing::info!(player = self.owner, "tutorial: the quest is done");
        }
    }

    /// Whether the player took the beach bag's items.
    pub(super) const fn bag_taken(&self) -> bool {
        self.bag_taken
    }

    /// The quest's task list for the quest tracker.
    pub(super) fn progress(&self) -> Vec<TaskProgress> {
        self.run.progress()
    }

    /// Whether the quest is done.
    pub(super) fn finished(&self) -> bool {
        self.run.finished()
    }

    /// Removes every mob and body of the tutorial (the player left).
    pub(super) fn end(self, mobs: &mut Mobs) {
        let fighters = self.fighters.into_iter().map(|fighter| fighter.entity);
        let corpses = self.corpses.into_iter().map(|(mob, _)| mob);
        for mob in fighters.chain(corpses) {
            mobs.remove_personal(mob);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use nw_content::combat::{CombatData, CombatRows, LevelRow, MobType};

    use super::*;
    use crate::combat::test_support::{TEST_LEVEL_DAMAGE, TEST_LEVEL_HEALTH, test_source};
    use crate::config::tests::committed;
    use crate::objectives::TaskRow;
    use crate::player_content::TUTORIAL_QUEST_ROOT;
    use nw_sim_core::MobState;

    const OWNER: u64 = 1;
    const NOW: u64 = 1000;
    const RETAIL_SAILOR_VITALS: u32 = 0x8869_82c9;
    const RETAIL_SAILOR_CATEGORY: u32 = 0xa452_daaa;
    const RETAIL_LEGION_CATEGORY: u32 = 0x9ccc_23f1;
    const SHIP: Position = point(478.5, 849.7, 89.59);
    const CHEST_LOOT_TASK: &str = "01_SoleSurvivor2_Tutorial_TakeChestLoot";
    const CORRUPTED: &str = "Corrupted";

    /// A mob move of the script, for tests of its orders elsewhere.
    pub(crate) const fn any_mob_move() -> MobMove {
        THORPE_STAB_01
    }

    /// The vitals id of the ship's sailors.
    pub(crate) fn sailor_vitals() -> u32 {
        lowercase_crc32(SAILOR_VITALS)
    }

    /// Combat settings whose rows know every tutorial mob at its level.
    pub(crate) fn sailor_combat() -> crate::combat::CombatSettings {
        let mut source = test_source();
        let vitals = [
            SAILOR_VITALS,
            LEGION_VITALS,
            "Corruption_Entity",
            SWARMER_VITALS,
            "Damned_Commander_FTUE",
        ];
        for vitals in vitals {
            source.base_types.push((vitals.to_owned(), MobType::PLAIN));
        }
        source.levels.push((
            TUTORIAL_MOB_LEVEL,
            LevelRow {
                base_max_health: TEST_LEVEL_HEALTH,
                base_damage: TEST_LEVEL_DAMAGE,
                physical_armor_rating: 0.0,
                elemental_armor_rating: 0.0,
                gear_score: 0,
            },
        ));
        let data = CombatData::build(source, &CombatRows::default()).expect("rows");
        committed().combat_settings(
            Arc::new(data),
            Arc::from([]),
            crate::combat::tests::test_mob_ai(),
        )
    }

    /// Every tutorial mob is corrupted.
    pub(crate) fn kill_types() -> KillTypes {
        KillTypes::build(
            [SAILOR_VITALS, LEGION_VITALS, SWARMER_VITALS]
                .into_iter()
                .map(|vitals| (vitals.to_owned(), format!("{vitals},Hostile,{CORRUPTED}"))),
        )
    }

    fn task(task_id: &str, kind: &str) -> TaskRow {
        TaskRow {
            task_id: task_id.to_owned(),
            kind: kind.to_owned(),
            ..TaskRow::default()
        }
    }

    /// A tree in the shape of the tutorial: the intro unlock, one then two corrupted kills, the
    /// third video unlock, the sword removal, the door, and the chest's potions.
    pub(crate) fn tree() -> Arc<ObjectiveTree> {
        let rows = vec![
            TaskRow {
                sub_tasks: [
                    "Intro",
                    "Sailor",
                    "Sailors",
                    "Intro3",
                    "Sword",
                    "01_SoleSurvivor2_UnlockDoor",
                    CHEST_LOOT_TASK,
                ]
                .map(str::to_owned)
                .to_vec(),
                ..task(TUTORIAL_QUEST_ROOT, "ConsecutiveTaskContainer")
            },
            TaskRow {
                item_name: Some("FTUE_Intro2_Cinematic_Unlock".to_owned()),
                ..task("Intro", "TaskUnlockAchievement")
            },
            TaskRow {
                kill_enemy_type: Some(CORRUPTED.to_owned()),
                ..task("Sailor", "TaskKillContribution")
            },
            TaskRow {
                kill_enemy_type: Some(CORRUPTED.to_owned()),
                target_qty: Some(2),
                ..task("Sailors", "TaskKillContribution")
            },
            TaskRow {
                item_name: Some("FTUE_Intro3_Cinematic_Unlock".to_owned()),
                ..task("Intro3", "TaskUnlockAchievement")
            },
            TaskRow {
                item_name: Some("1hSwordT2_FTUE".to_owned()),
                ..task("Sword", "TaskRemoveItem")
            },
            TaskRow {
                item_name: Some("FTUE_AncientRuins_Door".to_owned()),
                ..task("01_SoleSurvivor2_UnlockDoor", "TaskUnlockAchievement")
            },
            TaskRow {
                item_name: Some(CHEST_ITEM.to_owned()),
                ..task(CHEST_LOOT_TASK, "TaskHaveAndReturnItems")
            },
        ];
        Arc::new(ObjectiveTree::build(&rows, TUTORIAL_QUEST_ROOT).expect("tree"))
    }

    fn kill_all(mobs: &mut Mobs, tutorial: &Tutorial) {
        for fighter in &tutorial.fighters {
            let mob = mobs.get_mut(fighter.entity).expect("mob");
            let max = mob.health.max();
            mob.health.take(max);
        }
    }

    #[test]
    fn the_mob_rows_are_the_retail_ids() {
        let spawn = spawn_of(&FIRST_SAILOR);
        assert_eq!(
            (spawn.vitals_id, spawn.category_id),
            (RETAIL_SAILOR_VITALS, RETAIL_SAILOR_CATEGORY)
        );
        assert_eq!(
            spawn_of(&FIGHTS_AFTER[0].spawns[0]).category_id,
            RETAIL_LEGION_CATEGORY
        );
        assert_eq!(spawn_of(&FIGHTS_AFTER[3].spawns[0]).category_id, 0);
    }

    #[test]
    fn the_quest_runs_the_ship_fight_and_the_later_fights() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, orders) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        assert_eq!(
            orders,
            [Order::Achievement(
                "FTUE_Intro2_Cinematic_Unlock".to_owned()
            )]
        );
        assert!(tutorial.fighters.is_empty(), "the first sailor comes later");
        tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + FIRST_SAILOR_DELAY_MS - 1,
        );
        assert!(tutorial.fighters.is_empty());
        tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + FIRST_SAILOR_DELAY_MS,
        );
        let first = tutorial.fighters[0].entity;
        assert_eq!(mobs.owner(first), Some(OWNER));
        assert_eq!(
            tutorial.lesson_done(&mut mobs, NOW),
            [Order::Targeted {
                mob: first,
                tutorial_ai: true
            }],
            "the first sailor targets the player"
        );
        assert!(tutorial.lesson_done(&mut mobs, NOW).is_empty(), "once");
        kill_all(&mut mobs, &tutorial);
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW);
        assert_eq!(tutorial.fighters.len(), 1, "the next sailor comes at once");
        let second = tutorial.fighters[0].entity;
        assert_eq!(
            orders,
            [Order::Died {
                mob: first,
                collision: SAILOR_COLLISION
            }],
            "the client must have the new sailor before its pick"
        );
        let picked = |mob| {
            [Order::Targeted {
                mob,
                tutorial_ai: true,
            }]
        };
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + SPAWN_PICK_DELAY_MS);
        assert_eq!(orders, picked(second));
        let third_at = NOW + LATER_SAILORS[1].delay_ms;
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, third_at);
        assert_eq!(
            tutorial.fighters.len(),
            2,
            "and the last one a second later"
        );
        assert!(orders.is_empty());
        let third = tutorial.fighters[1].entity;
        let picked_at = third_at + SPAWN_PICK_DELAY_MS;
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, picked_at);
        assert_eq!(orders, picked(third));
        kill_all(&mut mobs, &tutorial);
        assert_eq!(
            tutorial.respawn_at(),
            TUTORIAL_START,
            "a death on the ship: the deck"
        );
        let door_at = picked_at + 1;
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, door_at);
        assert_eq!(
            tutorial.respawn_at(),
            FIGHTS_AFTER[0].respawn,
            "a death in the legionnaire's fight: where it started"
        );
        assert_eq!(
            orders,
            [
                Order::Died {
                    mob: second,
                    collision: SAILOR_COLLISION
                },
                Order::Died {
                    mob: third,
                    collision: SAILOR_COLLISION
                },
                Order::Achievement("FTUE_Intro3_Cinematic_Unlock".to_owned()),
                Order::RemoveItem("1hSwordT2_FTUE".to_owned(), 1),
                Order::Achievement("FTUE_AncientRuins_Door".to_owned()),
            ]
        );
        let legion_at = door_at + FIGHTS_AFTER[0].spawns[0].delay_ms;
        tutorial.tick(&mut mobs, &kill_types(), SHIP, legion_at - 1);
        assert!(tutorial.fighters.is_empty(), "the legionnaire comes later");
        tutorial.tick(&mut mobs, &kill_types(), SHIP, legion_at);
        assert_eq!(
            tutorial.fighters.len(),
            1,
            "the door brings the legionnaire"
        );
        let legionnaire = tutorial.fighters[0].entity;
        // It picks the player only when it sees it, within its 12 m (retail 2415 25 s after its
        // card).
        let at = |metres: f32| nw_sim_core::Seen {
            entity: OWNER,
            position: Position {
                x: FIGHTS_AFTER[0].spawns[0].at.x + metres,
                ..FIGHTS_AFTER[0].spawns[0].at
            },
        };
        let ground = nw_sim_core::test_support::NoGround;
        mobs.tick(&[at(15.0)], legion_at + 1, &ground, 1);
        let unseen = tutorial.tick(&mut mobs, &kill_types(), SHIP, legion_at + 1);
        assert!(
            !unseen
                .iter()
                .any(|order| matches!(order, Order::Targeted { .. })),
            "out of sight: {unseen:?}"
        );
        mobs.tick(&[at(5.0)], legion_at + 2, &ground, 1);
        let seen = tutorial.tick(&mut mobs, &kill_types(), SHIP, legion_at + 2);
        let picked = Order::Targeted {
            mob: legionnaire,
            tutorial_ai: false,
        };
        assert!(seen.contains(&picked), "in sight: {seen:?}");
        mobs.tick(&[at(5.0)], legion_at + 3, &ground, 1);
        assert!(
            !tutorial
                .tick(&mut mobs, &kill_types(), SHIP, legion_at + 3)
                .contains(&picked),
            "once"
        );
        assert_eq!(mobs.health(first), None, "the first sailor's body went");
        assert!(mobs.health(second).is_some(), "the body stays a while");
        tutorial.tick(
            &mut mobs,
            &kill_types(),
            SHIP,
            door_at + SAILOR_CORPSE_MS - 1,
        );
        assert!(mobs.health(second).is_some(), "until the retail time");
        tutorial.tick(&mut mobs, &kill_types(), SHIP, door_at + SAILOR_CORPSE_MS);
        assert_eq!(mobs.health(second), None, "then it goes");
        kill_all(&mut mobs, &tutorial);
        let orders = tutorial.tick(
            &mut mobs,
            &kill_types(),
            SHIP,
            door_at + SAILOR_CORPSE_MS + 1,
        );
        assert!(
            orders.contains(&Order::KillExperience(TUTORIAL_MOB_LEVEL)),
            "an island foe gives experience: {orders:?}"
        );
        let orders = tutorial.event(
            Event::ItemGained {
                item: CHEST_ITEM,
                quantity: 1,
            },
            &mut mobs,
            NOW,
        );
        assert!(tutorial.run.finished());
        let mut rewards: Vec<Order> = LEAVING_LORE
            .iter()
            .map(|lore| Order::Achievement((*lore).to_owned()))
            .collect();
        rewards.push(Order::RefillPotions(CHEST_ITEM.to_owned(), CHEST_QUANTITY));
        assert_eq!(orders, rewards, "the end gives the lore and fresh potions");
        assert!(
            tutorial
                .event(
                    Event::ItemGained {
                        item: CHEST_ITEM,
                        quantity: 1,
                    },
                    &mut mobs,
                    NOW
                )
                .is_empty(),
            "once"
        );
        tutorial.end(&mut mobs);
        assert_eq!(mobs.health(legionnaire), None);
    }

    #[test]
    fn the_risen_group_comes_one_by_one_at_the_retail_times() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        tutorial.spawn_batch(FIGHTS_AFTER[2].spawns, &mut mobs, false);
        assert!(tutorial.fighters.is_empty(), "none at the trigger");
        let vitals: Vec<u32> = FIGHTS_AFTER[2]
            .spawns
            .iter()
            .map(|point| lowercase_crc32(point.vitals))
            .collect();
        for (count, point) in FIGHTS_AFTER[2].spawns.iter().enumerate() {
            tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + point.delay_ms - 1);
            assert_eq!(tutorial.fighters.len(), count, "not before its time");
            tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + point.delay_ms);
            assert_eq!(tutorial.fighters.len(), count + 1);
        }
        let spawned: Vec<u32> = tutorial
            .fighters
            .iter()
            .map(|fighter| fighter.vitals_id)
            .collect();
        assert_eq!(spawned, vitals, "the risen first, then the swarmers");
        assert!(
            tutorial.fighters.iter().all(|fighter| fighter.batch == 0),
            "one batch"
        );
    }

    #[test]
    fn the_player_moves_into_thorpes_arena_after_his_cutscene_starts() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        let started = |cutscene| ClientFacetMsg::CutsceneStarted {
            header: nw_messages::FacetHeader {
                target: 0,
                entity: 0,
            },
            cutscene,
        };
        let arena_move = Order::MoveTo(ARENA_AT, ARENA_FACING);
        tutorial.client_message(&started(THORPE_INTRO_CUTSCENE.wrapping_add(1)), NOW);
        tutorial.client_message(&started(THORPE_INTRO_CUTSCENE), NOW);
        let early = tutorial.tick(
            &mut mobs,
            &kill_types(),
            SHIP,
            NOW + ARENA_MOVE_DELAY_MS - 1,
        );
        assert!(!early.contains(&arena_move));
        let due = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + ARENA_MOVE_DELAY_MS);
        assert_eq!(
            due.iter().filter(|order| **order == arena_move).count(),
            1,
            "once, for Thorpe's cutscene only"
        );
        // The retail rotation `407f7f54` is the same turn as the facing's encoding.
        assert_eq!(
            nw_messages::alc::rotation_bytes(ARENA_FACING),
            [0x47, 0xab],
            "q = (0, 0, -0.9705, 0.2412)"
        );
    }

    #[test]
    fn the_player_moves_into_the_water_during_the_sinking_video() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        let sinking = lowercase_crc32(SHIP_SINKING_VIDEO);
        tutorial.event(Event::CinematicStarted(sinking), &mut mobs, NOW);
        let early = tutorial.tick(
            &mut mobs,
            &kill_types(),
            SHIP,
            NOW + WATER_MOVE_DELAY_MS - 1,
        );
        assert!(!early.contains(&Order::MoveTo(WATER_AT, WATER_FACING)));
        let due = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + WATER_MOVE_DELAY_MS);
        assert!(due.contains(&Order::MoveTo(WATER_AT, WATER_FACING)));
        let ended = tutorial.event(Event::CinematicEnded(sinking), &mut mobs, NOW);
        assert!(
            !ended.contains(&Order::MoveTo(WATER_AT, WATER_FACING)),
            "once only"
        );
    }

    #[test]
    fn the_chest_gives_its_potions_once_after_the_gather_time() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        let mut target = [0; RAW_ID_LEN];
        target[..GDE_REF_PREFIX_LEN].copy_from_slice(&CHEST_PREFIX);
        let request = InteractionRequest {
            target,
            target_entity: 1,
            interaction: 2,
            kind: 14,
            gatherable: None,
        };
        assert_eq!(
            tutorial.interact(request, NOW),
            [
                Order::AcceptGather(request),
                Order::StaticChange(CHEST_PREFIX, CHEST_BUSY_CHANGE),
            ]
        );
        assert!(
            tutorial.interact(request, NOW).is_empty(),
            "one gather at a time"
        );
        let early = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + CHEST_GATHER_MS - 1);
        assert!(!early.contains(&Order::EndGather(request)));
        let done = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + CHEST_GATHER_MS);
        assert!(done.contains(&Order::EndGather(request)));
        assert!(done.contains(&Order::GiveItem(CHEST_ITEM.to_owned(), CHEST_QUANTITY)));
        assert!(
            tutorial.interact(request, NOW + CHEST_GATHER_MS).is_empty(),
            "a repeated record of the same press starts nothing"
        );
        let press_again = InteractionRequest {
            interaction: request.interaction + 1,
            ..request
        };
        assert!(
            !tutorial
                .interact(press_again, NOW + CHEST_GATHER_MS)
                .is_empty()
        );
        let again = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + 2 * CHEST_GATHER_MS);
        assert!(again.contains(&Order::EndGather(press_again)));
        assert!(
            !again.contains(&Order::GiveItem(CHEST_ITEM.to_owned(), CHEST_QUANTITY)),
            "the potions come once"
        );
        let other = InteractionRequest {
            target: [0; RAW_ID_LEN],
            ..request
        };
        assert!(tutorial.interact(other, NOW).is_empty());
    }

    #[test]
    fn the_bag_goes_after_its_take() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        let mut bag = [0; RAW_ID_LEN];
        bag[..GDE_REF_PREFIX_LEN].copy_from_slice(&BAG_PREFIX);
        assert!(
            !tutorial.take_bag(&[0; RAW_ID_LEN], NOW),
            "another container"
        );
        assert!(tutorial.take_bag(&bag, NOW));
        assert!(!tutorial.take_bag(&bag, NOW), "the bag gives once");
        let orders = tutorial.tick(&mut mobs, &kill_types(), SHIP, NOW + BAG_GONE_MS);
        assert_eq!(
            orders,
            [
                Order::StaticChange(BAG_PREFIX, BAG_FIRST_EVENT_CHANGE),
                Order::StaticChange(BAG_PREFIX, BAG_SECOND_EVENT_CHANGE),
                Order::HideStatic(BAG_PREFIX),
            ]
        );
    }

    /// A tutorial with Thorpe in his fight.
    fn thorpe_fight() -> (Tutorial, Mobs, u64) {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        tutorial.spawn_batch(FIGHTS_AFTER[3].spawns, &mut mobs, false);
        let thorpe = tutorial.fighters[0].entity;
        (tutorial, mobs, thorpe)
    }

    #[test]
    fn thorpe_grabs_a_far_player_and_pulls_it_in_front_of_him() {
        let (mut tutorial, mut mobs, thorpe) = thorpe_fight();
        let kill_types = KillTypes::default();
        let at = mobs.get_mut(thorpe).expect("Thorpe").position;
        let near = Position {
            x: at.x + 3.0,
            ..at
        };
        let far = Position {
            x: at.x + 10.0,
            ..at
        };
        let grabs = |orders: &[Order]| {
            orders
                .iter()
                .filter(|order| {
                    matches!(order, Order::PlayMove { fragment, .. } if *fragment == THORPE_FORCE_GRAB)
                })
                .count()
        };
        assert_eq!(grabs(&tutorial.tick(&mut mobs, &kill_types, far, NOW)), 0);
        let first = NOW + GRAB_FIRST_MS;
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, near, first)),
            0,
            "not a near player"
        );
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, first + 1)),
            1
        );
        let pull = tutorial.tick(&mut mobs, &kill_types, far, first + 1 + GRAB_PULL_AT_MS);
        assert!(pull.contains(&Order::Shove {
            mob: thorpe,
            shove: Shove::To(GRAB_PULL_STOP_M),
            length_ms: GRAB_PULL_MS,
        }));
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, first + GRAB_INTERVAL_MS)),
            0,
            "one grab per interval"
        );
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, first + 1 + GRAB_INTERVAL_MS)),
            1
        );
    }

    #[test]
    fn thorpe_does_not_grab_a_dead_player_and_waits_again_after_its_respawn() {
        let (mut tutorial, mut mobs, thorpe) = thorpe_fight();
        let kill_types = KillTypes::default();
        let at = mobs.get_mut(thorpe).expect("Thorpe").position;
        let far = Position {
            x: at.x + 10.0,
            ..at
        };
        let grabs = |orders: &[Order]| {
            orders
                .iter()
                .filter(|order| {
                    matches!(order, Order::PlayMove { fragment, .. } if *fragment == THORPE_FORCE_GRAB)
                })
                .count()
        };
        tutorial.tick(&mut mobs, &kill_types, far, NOW);
        let due = NOW + GRAB_FIRST_MS + 1;
        tutorial.player_alive(false);
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, due)),
            0,
            "no grab of a dead player"
        );
        tutorial.player_alive(true);
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, due + 1)),
            0,
            "the first grab waits again after the respawn"
        );
        assert_eq!(
            grabs(&tutorial.tick(&mut mobs, &kill_types, far, due + 1 + GRAB_FIRST_MS)),
            1
        );
    }

    #[test]
    fn thorpe_transitions_knocks_the_player_away_then_charges() {
        let (mut tutorial, mut mobs, thorpe) = thorpe_fight();
        let at = mobs.get_mut(thorpe).expect("Thorpe").position;
        let near = Position {
            x: at.x + 3.0,
            ..at
        };
        let max = mobs.get_mut(thorpe).expect("Thorpe").health.max();
        mobs.get_mut(thorpe).expect("Thorpe").health.take(max / 2);
        let kill_types = KillTypes::default();
        let orders = tutorial.tick(&mut mobs, &kill_types, near, NOW);
        assert!(orders.contains(&Order::PlayMove {
            mob: thorpe,
            fragment: THORPE_PHASE_TRANSITION,
            length_ms: CAPTAIN_TRANSITION_MS,
        }));
        let shockwave = tutorial.tick(&mut mobs, &kill_types, near, NOW + SHOCKWAVE_AT_MS);
        assert!(shockwave.contains(&Order::Blow));
        assert!(shockwave.contains(&Order::Shove {
            mob: thorpe,
            shove: Shove::Away(SHOCKWAVE_PUSH_M),
            length_ms: SHOCKWAVE_PUSH_MS,
        }));
        let tell_ms = NOW + CAPTAIN_TRANSITION_MS + TELL_AFTER_TRANSITION_MS;
        let tell = tutorial.tick(&mut mobs, &kill_types, near, tell_ms);
        assert!(tell.contains(&Order::PlayMove {
            mob: thorpe,
            fragment: THORPE_COMBAT_TELL,
            length_ms: TELL_MS,
        }));
        let far = Position {
            x: at.x + 12.0,
            ..at
        };
        let loop_ms = tell_ms + TELL_MS + CHARGE_START_MS;
        tutorial.tick(&mut mobs, &kill_types, far, loop_ms);
        let charged = tutorial.tick(&mut mobs, &kill_types, far, loop_ms + 100);
        let ran = mobs.get_mut(thorpe).expect("Thorpe").position;
        assert!(
            (ran.horizontal_distance(at) - CHARGE_SPEED_MPS * 0.1).abs() < 0.01,
            "he runs 2.57 m in 100 ms"
        );
        assert!(charged.is_empty());
        let outro = tutorial.tick(&mut mobs, &kill_types, far, loop_ms + CHARGE_LOOP_MS);
        assert!(outro.contains(&Order::Swing {
            mob: thorpe,
            mob_move: THORPE_CHARGE_OUTRO,
        }));
    }

    #[test]
    fn thorpe_pauses_heals_and_ends_at_half_of_his_second_bar() {
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let (mut tutorial, _) =
            Tutorial::start(OWNER, tree(), &FtueLevel::default(), &mut mobs, NOW);
        tutorial.spawn_batch(FIGHTS_AFTER[3].spawns, &mut mobs, false);
        let thorpe = tutorial.fighters[0].entity;
        let max = mobs.get_mut(thorpe).expect("Thorpe").health.max();
        let hurt_to = |mobs: &mut Mobs, share: f64| {
            let mob = mobs.get_mut(thorpe).expect("Thorpe");
            let keep = (f64::from(max) * share) as u32;
            let current = mob.health.current();
            mob.health.take(current - keep);
        };
        hurt_to(&mut mobs, 0.55);
        let orders = tutorial.tick(&mut mobs, &KillTypes::default(), SHIP, NOW);
        assert!(orders.contains(&Order::BossPhase {
            mob: thorpe,
            phase: CAPTAIN_INTERMISSION_PHASE
        }));
        assert!(matches!(
            tutorial.captain,
            CaptainPhase::Intermission { .. }
        ));
        assert!(mobs.absorbing(thorpe, NOW), "no damage in his intermission");
        assert!(!mobs.absorbing(thorpe, NOW + CAPTAIN_INTERMISSION_MS));
        assert!(
            mobs.health(thorpe).is_some_and(|health| health > 0),
            "no death yet"
        );
        tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + CAPTAIN_INTERMISSION_MS,
        );
        assert_eq!(mobs.health(thorpe), Some(max), "healed to full");
        hurt_to(&mut mobs, 0.45);
        let orders = tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + CAPTAIN_INTERMISSION_MS + 1,
        );
        assert_eq!(mobs.health(thorpe), Some(0));
        assert_eq!(
            mobs.state(thorpe),
            Some(MobState::Dead),
            "his body stops at his scripted end"
        );
        assert!(orders.contains(&Order::BossPhase {
            mob: thorpe,
            phase: CAPTAIN_END_PHASE
        }));
        tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + CAPTAIN_INTERMISSION_MS + 2,
        );
        assert!(
            tutorial
                .fighters
                .iter()
                .all(|fighter| fighter.entity != thorpe)
        );
        // His body stays to the end of the tutorial: retail still had his slot 106941 ms after
        // his 3916, when the player left (newchar 2026-09-12).
        const RETAIL_BODY_SEEN_MS: u64 = 106_941;
        tutorial.tick(
            &mut mobs,
            &KillTypes::default(),
            SHIP,
            NOW + CAPTAIN_INTERMISSION_MS + 2 + RETAIL_BODY_SEEN_MS,
        );
        assert!(mobs.get(thorpe).is_some(), "his body stays");
        tutorial.end(&mut mobs);
        assert!(mobs.get(thorpe).is_none(), "it goes with the tutorial");
    }

    #[test]
    fn thorpe_overhead_hits_when_its_damage_window_opens() {
        // `npc_commander_1h_swordanims.adb`, `Attack_Overhead_Slash`: `CAGE-Damage` 1.038 to
        // 1.233 s, row `Attack_Overhead_Slash`.
        const WINDOW_MS: (u64, u64) = (1_038, 1_233);
        let [hit] = THORPE_OVERHEAD.hits else {
            panic!("one hit");
        };
        assert_eq!(hit.at_ms, WINDOW_MS.0);
        assert!(hit.at_ms < WINDOW_MS.1 && WINDOW_MS.1 < THORPE_OVERHEAD.length_ms);
        assert_eq!(hit.row, "Attack_Overhead_Slash");
    }

    /// Its spawn's point and a player `metres` east of it.
    fn east_of(point: &SpawnPoint, metres: f32) -> nw_sim_core::Seen {
        nw_sim_core::Seen {
            entity: OWNER,
            position: Position {
                x: point.at.x + metres,
                ..point.at
            },
        }
    }

    #[test]
    fn the_legionnaire_stands_at_its_spawn_until_it_sees_the_player() {
        // Retail: the legionnaire stood on its ruin top until it saw the player (25 s after its
        // card); live 2026-09-18 a wander walked it into the ruin wall. Its tree is not in
        // Server.pak: it fights with its measured pace, which has no wander.
        const MINUTE_MS: u64 = 60_000;
        let point = &FIGHTS_AFTER[0].spawns[0];
        let mut mobs = Mobs::new(Vec::new().into(), &sailor_combat());
        let legionnaire = mobs
            .spawn_personal(spawn_of(point), OWNER, false, point.pace, None, NOW)
            .expect("rows");
        assert!(mobs.tree(legionnaire).is_none(), "no tree: its pace");
        let ground = nw_sim_core::test_support::NoGround;
        let unseen = east_of(point, LEGION_PACE.sight_m + 3.0);
        for now in (NOW..NOW + MINUTE_MS).step_by(33) {
            mobs.tick(&[unseen], now, &ground, 1);
        }
        let (at, spawn) = mobs.places(legionnaire).expect("alive");
        assert_eq!(at, spawn, "it stood at its spawn");
        let seen = east_of(point, LEGION_PACE.sight_m - 3.0);
        for now in (NOW + MINUTE_MS..NOW + MINUTE_MS + 1_000).step_by(33) {
            mobs.tick(&[seen], now, &ground, 1);
        }
        let (moved, _) = mobs.places(legionnaire).expect("alive");
        assert!(
            moved.x > spawn.x,
            "it runs at the player it sees: {moved:?}"
        );
    }
}
