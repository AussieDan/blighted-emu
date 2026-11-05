// SPDX-License-Identifier: AGPL-3.0-or-later
//! Objective task trees: the server script of a quest (live-tutorial note, section 0 and
//! appendix A).
//!
//! An objective's root task is a tree of `ObjectiveTasks` rows. A container task runs its
//! children: one after the other (`ConsecutiveTaskContainer`), all at once until one is done
//! (`PartialTaskContainer`), or all at once until all are done (`SimpleTaskContainer`). A leaf
//! task is done at once (an achievement unlock, an item removal, a game event it fires), after a
//! time (a timer), on a game event (a video, a cutscene, a kill, an area, an item, an equip, a UI
//! screen, the gather of a quest object), or while the player is so (in or out of a point of
//! interest, with an achievement). The client plays the scenes on its own when the server sets the
//! achievements of the tree.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nw_bus::{QuestRun, QuestTask};
use nw_content::Tables;
use nw_formats::objectstream::lowercase_crc32;
use nw_sim_core::Position;

use crate::player_content::requirement_met;

/// The separator of the `POITag`s of one `TaskGoToPOITag` cell.
const POI_TAG_SEPARATOR: char = ',';
/// How near a player must come to a trigger area's `OverrideLocation` when [`TRIGGER_VOLUMES`]
/// holds no volume of its `TP_TargetTag` (Unknown: the open-world quest volumes, such as the
/// beach's `TriggerVol_12_Spawn_A`, were not found in the region capitals, the beach region's
/// `region.slicedata` or the 2,321 trigger, volume, quest and tutorial slices of the paks; the
/// open-world start trigger fired with the player 17.2 m from it at its spawn).
const TRIGGER_RADIUS_M: f32 = 18.0;
/// Half of a box side.
const HALF: f32 = 0.5;
/// A rotation quaternion turns by twice the angle of its (z, w) pair.
const QUATERNION_HALF_ANGLES: f32 = 2.0;

/// The shape of a quest trigger volume.
#[derive(Debug, Clone, Copy, PartialEq)]
enum VolumeShape {
    /// `SphereShapeComponent` `Radius`.
    Sphere { radius_m: f32 },
    /// `BoxShapeComponent` `Dimensions`: its full size along its own x, y and z.
    Box { size_m: [f32; 3] },
}

/// A quest trigger volume of the level: the `TriggerAreaComponent` slice whose tag component
/// names `tag`, the place and turn of its capital, and its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TriggerVolume {
    tag: &'static str,
    at: Position,
    /// The z and w of the capital's rotation (its x and y are 0: a turn about z).
    turn: [f32; 2],
    shape: VolumeShape,
}

impl TriggerVolume {
    /// Whether `position` is in the volume.
    fn contains(&self, position: Position) -> bool {
        let (dx, dy, dz) = (
            position.x - self.at.x,
            position.y - self.at.y,
            position.z - self.at.z,
        );
        match self.shape {
            VolumeShape::Sphere { radius_m } => {
                dx.mul_add(dx, dy.mul_add(dy, dz * dz)) <= radius_m * radius_m
            }
            VolumeShape::Box {
                size_m: [length, width, height],
            } => {
                let [z, w] = self.turn;
                let (sin, cos) = (QUATERNION_HALF_ANGLES * z.atan2(w)).sin_cos();
                let along = dx.mul_add(cos, dy * sin);
                let across = dy.mul_add(cos, -dx * sin);
                along.abs() <= length * HALF
                    && across.abs() <= width * HALF
                    && dz.abs() <= height * HALF
            }
        }
    }
}

const fn volume(
    tag: &'static str,
    [x, y, z]: [f32; 3],
    turn: [f32; 2],
    shape: VolumeShape,
) -> TriggerVolume {
    TriggerVolume {
        tag,
        at: Position { x, y, z },
        turn,
        shape,
    }
}

/// The quest trigger volumes of the tutorial level (`ftue_v2`): the 27 `AncientIvory_Trigger`
/// slices of `quest_lore.capitals.json` (`coatgen/71abcdfc/ancientivory_trigger_*.dynamicslice`,
/// `SharedDataStrm` paks, build 25810808), each a `TriggerAreaComponent` with one sphere or box
/// shape on its root entity (identity local transform; every capital scale is 1 within 2e-6).
/// The retail tutorial player entered them in the quest's order (newchar 2026-09-12 and
/// 2026-09-18: `FTUE_First_Fight_Trigger` 4.5 m from its `OverrideLocation`).
const TRIGGER_VOLUMES: [TriggerVolume; 27] = [
    volume(
        "FTUE_Cave_Midpoint_Trigger",
        [465.319, 266.99216, 35.863567],
        [-0.13052656, 0.9914448],
        VolumeShape::Box {
            size_m: [20.0, 5.0, 7.0],
        },
    ),
    volume(
        "FTUE_Cave_Trigger",
        [479.59375, 294.95087, 36.52487],
        [-0.13052656, 0.9914448],
        VolumeShape::Box {
            size_m: [10.0, 5.0, 7.0],
        },
    ),
    volume(
        "FTUE_Chest_Discovery_Trigger",
        [663.01825, 407.27808, 27.548208],
        [0.99809176, 0.06174778],
        VolumeShape::Box {
            size_m: [18.0, 3.0, 5.0],
        },
    ),
    volume(
        "FTUE_Climb_Trigger",
        [595.99335, 385.81693, 20.533539],
        [-0.15677612, 0.9876342],
        VolumeShape::Sphere { radius_m: 15.0 },
    ),
    volume(
        "FTUE_Dash_Trigger",
        [755.5905, 342.64047, 20.0],
        [-0.819152, 0.5735765],
        VolumeShape::Box {
            size_m: [20.0, 5.0, 5.0],
        },
    ),
    volume(
        "FTUE_EnterRuins_00_Trigger",
        [714.24896, 379.6992, 24.04622],
        [-0.40252402, 0.91540945],
        VolumeShape::Box {
            size_m: [12.0, 15.0, 5.0],
        },
    ),
    volume(
        "FTUE_EnterRuins_01_Trigger",
        [701.63873, 378.4184, 24.04622],
        [-0.7933534, 0.6087614],
        VolumeShape::Box {
            size_m: [40.0, 2.0, 2.0],
        },
    ),
    volume(
        "FTUE_First_Fight_Trigger",
        [690.17737, 381.5598, 27.37356],
        [-0.7933534, 0.6087614],
        VolumeShape::Sphere { radius_m: 7.0 },
    ),
    volume(
        "FTUE_First_Fight_Trigger_Failsafe",
        [660.8932, 396.27426, 27.548208],
        [0.9812974, -0.19249813],
        VolumeShape::Box {
            size_m: [18.0, 3.0, 5.0],
        },
    ),
    volume(
        "FTUE_Intro2_Trigger",
        [479.1835, 850.0, 90.0],
        [0.0, 1.0],
        VolumeShape::Box {
            size_m: [3.0, 7.0, 3.0],
        },
    ),
    volume(
        "FTUE_Intro3_Trigger",
        [487.829, 541.0021, 89.3834],
        [0.0, 1.0],
        VolumeShape::Box {
            size_m: [1000.0, 1000.0, 1000.0],
        },
    ),
    volume(
        "FTUE_Jump_Trigger",
        [542.4708, 367.10342, 31.975632],
        [-0.7372772, 0.6755904],
        VolumeShape::Box {
            size_m: [30.0, 6.0, 13.0],
        },
    ),
    volume(
        "FTUE_Jump_Trigger_Failsafe",
        [522.6977, 368.83334, 40.202774],
        [-0.7372772, 0.6755904],
        VolumeShape::Box {
            size_m: [30.0, 6.0, 13.0],
        },
    ),
    volume(
        "FTUE_PCCM_Trigger",
        [479.2432, 850.0, 90.0],
        [0.0, 1.0],
        VolumeShape::Box {
            size_m: [3.0, 7.0, 3.0],
        },
    ),
    volume(
        "FTUE_Rez_Trigger",
        [484.23718, 220.87, 21.362934],
        [0.0, 1.0],
        VolumeShape::Sphere { radius_m: 28.0 },
    ),
    volume(
        "FTUE_RuinStairs_Trigger",
        [637.0443, 420.1288, 23.500929],
        [-0.7708795, 0.63698095],
        VolumeShape::Box {
            size_m: [20.0, 4.0, 5.0],
        },
    ),
    volume(
        "FTUE_RuinStairs_Trigger_Failsafe",
        [629.9816, 423.27362, 22.134699],
        [-0.7247116, 0.68905234],
        VolumeShape::Box {
            size_m: [20.0, 4.0, 5.0],
        },
    ),
    volume(
        "FTUE_RuinsInventory_Trigger",
        [718.30383, 369.91855, 23.163818],
        [0.9659258, -0.25881904],
        VolumeShape::Box {
            size_m: [7.0, 3.0, 3.0],
        },
    ),
    volume(
        "FTUE_S1_Trigger",
        [793.7658, 337.43307, 19.69],
        [0.0, 1.0],
        VolumeShape::Sphere { radius_m: 30.0 },
    ),
    volume(
        "FTUE_Second_Fight_Trigger_Failsafe",
        [599.5421, 401.24615, 21.760458],
        [-0.16506681, 0.98628235],
        VolumeShape::Box {
            size_m: [20.0, 4.0, 5.0],
        },
    ),
    volume(
        "FTUE_SeeCave_Trigger",
        [484.1753, 321.1511, 32.516506],
        [-0.05231581, 0.9986306],
        VolumeShape::Box {
            size_m: [10.0, 5.0, 7.0],
        },
    ),
    volume(
        "FTUE_Skill_Trigger_Failsafe",
        [564.84326, 371.00024, 26.859451],
        [-0.5185991, 0.85501754],
        VolumeShape::Box {
            size_m: [20.0, 3.0, 5.0],
        },
    ),
    volume(
        "FTUE_Stairs_Trigger",
        [487.09857, 349.11597, 38.47406],
        [-0.05576592, 0.9984439],
        VolumeShape::Box {
            size_m: [10.0, 5.0, 7.0],
        },
    ),
    volume(
        "FTUE_TF_Trigger",
        [477.72824, 232.44313, 22.363],
        [0.9659258, -0.25881904],
        VolumeShape::Box {
            size_m: [30.0, 3.0, 18.0],
        },
    ),
    volume(
        "FTUE_TR_Trigger",
        [725.4063, 358.0765, 20.0],
        [0.9659258, -0.25881904],
        VolumeShape::Box {
            size_m: [10.0, 3.0, 9.0],
        },
    ),
    volume(
        "FTUE_Third_Fight_Trigger_Failsafe",
        [569.30365, 373.3245, 26.298578],
        [-0.5185991, 0.85501754],
        VolumeShape::Box {
            size_m: [20.0, 3.0, 5.0],
        },
    ),
    volume(
        "FTUE_Vista_Trigger",
        [516.0, 371.0, 45.56475],
        [0.0, 1.0],
        VolumeShape::Box {
            size_m: [3.0, 20.0, 12.0],
        },
    ),
];

/// Open-world quest volumes whose task rows carry no `OverrideLocation`, at the retail player's
/// place when the step after them began (no pak holds these server volumes, see
/// [`TRIGGER_RADIUS_M`]). Without a place such a task never ended: live 2026-10-01, the
/// `12_demonstorm2` quest stopped before its turn-in at `TriggerVol_12_Whisper6`.
const OPEN_WORLD_AREAS: [(&str, Position); 2] = [
    // Retail `12-9_demonstorm2` (2026-09-27): the client asked for the next whisper
    // (`01_Whisper_Beach_06`, 5235) here 169.8 s in, after the cave; `POITag` `watchtower`.
    (
        "TriggerVol_12_Whisper6",
        Position {
            x: 7299.863,
            y: 2952.437,
            z: 70.25296,
        },
    ),
    // Retail `12a_msq_02_inntutorial`: the client asked for the inn whisper that follows this
    // area (5235) here 70.9 s in, at the accept by the town gate; 6.6 m from the
    // `Intro_Prydwen_IGC_Trigger` location that `12_raisealarm` gives as the other way in.
    (
        "12A_LeaveLochnir",
        Position {
            x: 7422.975,
            y: 3624.002,
            z: 104.22217,
        },
    ),
];

/// Where a trigger area task waits for the player.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TriggerArea {
    /// The level's volume of its tag.
    Volume(TriggerVolume),
    /// [`TRIGGER_RADIUS_M`] around its `OverrideLocation`, for a tag without a known volume.
    Around(Position),
}

impl TriggerArea {
    /// The area of a task of tag `tag` and `OverrideLocation` `location`: the tag's volume, else
    /// around the location, else around the tag's retail place ([`OPEN_WORLD_AREAS`]); `None`
    /// without any.
    fn of(tag: Option<&str>, location: Option<Position>) -> Option<Self> {
        tag.and_then(|tag| {
            TRIGGER_VOLUMES
                .iter()
                .find(|volume| volume.tag.eq_ignore_ascii_case(tag))
        })
        .map(|volume| Self::Volume(*volume))
        .or_else(|| location.map(Self::Around))
        .or_else(|| {
            tag.and_then(|tag| {
                OPEN_WORLD_AREAS
                    .iter()
                    .find(|(known, _)| known.eq_ignore_ascii_case(tag))
            })
            .map(|(_, at)| Self::Around(*at))
        })
    }

    /// Whether `position` is in the area.
    fn contains(&self, position: Position) -> bool {
        match self {
            Self::Volume(volume) => volume.contains(position),
            Self::Around(at) => at.horizontal_distance(position) <= TRIGGER_RADIUS_M,
        }
    }
}

/// Joins an NPC id to the state number in a conversation state id without its NPC
/// (`NPC_16195_State_3`).
const CONVERSATION_STATE_MARK: &str = "_State_";
/// Separates the numbers of an `OverrideLocation` cell.
const LOCATION_SEPARATOR: char = ' ';
/// Milliseconds per second of a `TaskTimer` duration.
const MS_PER_SECOND: f64 = 1000.0;
/// Separates the categories of a `VitalsCategories` cell.
const CATEGORY_SEPARATOR: char = ',';
/// Joins a kill type to the rest of a longer vitals id (`Legion_Legionnaire` names
/// `Legion_Legionnaire_FTUE`).
const ID_PART_SEPARATOR: char = '_';
/// The chance of a quest drop when its task names none, in percent.
const ALWAYS_PERCENT: u32 = 100;
/// The lowercase `StayActive` cells of a task that runs on when done (the sheets write `1` or
/// `true`).
const STAY_ACTIVE_CELLS: [&str; 2] = ["1", "true"];
/// The `TriggerAreaType` of an area task done when the player leaves its volume (retail sheets:
/// `Enter` or `Leave`; an empty cell is `Enter`).
const LEAVE_AREA_TYPE: &str = "Leave";

/// What a task waits for.
#[derive(Debug, Clone, PartialEq)]
enum TaskKind {
    /// Runs its children one after the other.
    Consecutive,
    /// Runs its children at once; done when one is done.
    Partial,
    /// Runs its children at once; done when all are done.
    Simple,
    /// Sets an achievement and is done.
    UnlockAchievement(String),
    /// Clears an achievement and is done.
    LockAchievement(String),
    /// Sets an achievement and is done; the achievement is cleared when the objective ends
    /// (`TaskTemporaryUnlockAchievement`; client: "unlocks an achievement when task is added, and
    /// locks it when the task is removed (progress begins completed, nothing progresses this
    /// task)").
    TemporaryUnlock(String),
    /// Done when the player's achievements meet the `ItemName` expression: one name, or names
    /// joined by `&&` and `||` with `!` (90 retail tasks, `95_s01_05_CROW_CanonAndCoin`,
    /// `Objective_Artifact_*`; [`crate::player_content::requirement_met`]).
    HasAchievement(String),
    /// Removes `TargetQty` of an item from the player and is done (all 104 `TaskRemoveItem`
    /// rows of build 25810808 carry a `TargetQty` of 1 to 7; the four
    /// `13A_SQ_28_BearUrine_Gift_BearPee*` tasks each take one of the same item).
    RemoveItem { item: String, count: u32 },
    /// Done after a time.
    Timer { duration_ms: u64 },
    /// Done when the cinematic video of this id starts.
    CinematicStart(u32),
    /// Done when the cinematic video of this id ends.
    CinematicComplete(u32),
    /// Done when the in-game cutscene of this id ends.
    CutsceneComplete(u32),
    /// Done when the player comes into the area, or goes out of it when it waits for a
    /// `leave`; without an area, never.
    TriggerArea {
        /// The area.
        area: Option<TriggerArea>,
        /// It waits for the player to leave the volume (`TriggerAreaType` Leave).
        leave: bool,
        /// The `POITag` of an area whose row has no `OverrideLocation` and whose volume is not
        /// known: the area is also done while the player is in that point of interest. Live
        /// 2026-10-01: `TriggerVol_12_Whisper6` (POI `watchtower`, the campsite of Aelstrom and
        /// Urda) was entered by another path than retail's, away from its retail place.
        place: Option<String>,
    },
    /// Done after `count` kills of mobs of the type; with a `place` (a `POITag`), only kills
    /// while the player is in it count (`EnforceLocationRequirement`).
    KillContribution {
        enemy: String,
        count: u32,
        place: Option<String>,
    },
    /// Done when the player gets the item; without one, when it loots a container.
    HaveItem(Option<String>),
    /// Done when the player holds `count` of the item, in the bag or worn (`TaskHaveItems`, and
    /// a mission's `TaskHaveAndReturnItems`); `returned`: the items go back at the turn-in.
    /// While it waits, `drop` drops the item from a skinned corpse or a quest object, and a
    /// container the player opens in its `chest`'s place drops it at the chest's chance.
    HaveItems {
        item: String,
        count: u32,
        returned: bool,
        drop: Option<QuestDrop>,
        chest: Option<ChestDrop>,
    },
    /// Done after the player gathered `count` of the item; a corpse of `drop`'s enemy that the
    /// player skins, or a quest object of `drop`'s gatherable that it gathers, drops it while it
    /// waits (`TaskGatherItems`). The turn-in takes a `returned` item back
    /// (`TaskGatherAndReturnItems`).
    GatherItems {
        item: String,
        count: u32,
        drop: Option<QuestDrop>,
        returned: bool,
    },
    /// Done when the player crafts the recipe of this id (`TaskCraftRecipe`).
    CraftRecipe(String),
    /// Done after `count` crafts of any of these recipe ids (`TASKCRAFTMULTIPLERECIPES`: its
    /// `ItemDropVC` lists them, the season 8 boxes `WeaponsBoxT4,ArmorBoxT4,...`). Each craft
    /// counts once; whether a batch counts its quantity is Unknown.
    CraftAnyRecipe { recipes: Vec<String>, count: u32 },
    /// Never done: a `TaskGameEvent` with no game event (`KillEnemyType`), which nothing fires.
    /// Designers use it as a map pin or a never-true wait beside the real step in a partial
    /// container (`95A_S02_T1_RiseoftheBeast_PinTask`, `99F_MSQ_ER_ReuniteFamily_UsePortal_IMPOSSIBLE`;
    /// beach-quests note: the slice-swap timer branch of `12_skinningknife` waits on
    /// `neverbetrue`, so the knife ends its partial container).
    Never,
    /// Done when the player equips the item.
    EquipItem(String),
    /// Done when the client opens the UI screen of this id.
    OpenUiScreen(u32),
    /// Done when the client finishes the conversation topic of this id (`TaskConversationTopic`:
    /// the `UIScreenTag` is the topic's `ConversationTopicID`, else the `TaskID` is; retail
    /// completes it at the 5397 that names it, conversation-notify note section 1.3). Observed:
    /// the tag is a `ConversationTopicID` in 418 of the 433 rows, the `TaskID` in 2; the live
    /// client's 5397 for `12_demonstorm_02_TalkTo_Grace` named `0308314e`, the CRC-32 of its tag
    /// `01_Grace_Goodbye_MB` (login-world-entry-01).
    ConversationTopic(u32),
    /// Done while the player is in a point of interest of one of these `POITag`s
    /// (`TaskGoToPOITag`; the cell can list several, `,`-separated: `95A_S02_VoicesoftheDesert`
    /// names six crow's nests).
    GoToPoi(Vec<String>),
    /// Done while the player is out of every point of interest of this `POITag`
    /// (`TaskExitPOITag`).
    ExitPoi(String),
    /// Fires the game event of this id and is done (`TaskTriggerGameEvent`: its whisper plays on
    /// the client).
    TriggerGameEvent(String),
    /// Done after `count` game events of this id (`TaskGameEvent` with a `KillEnemyType`: the
    /// gathers of a quest object).
    GameEvent { event: String, count: u32 },
    /// Done after `count` interacts with an object whose interact tag is `tag` (`TaskInteract`;
    /// client: "When a player performs interact that has an InteractTag that matches this
    /// string, we will increment progress"). Retail: the vault step of `12A_MSQ_03` was done 49
    /// ms after the storage interaction ended (vault capture 2026-09-27, 57.548 s and 57.597 s).
    Interact { tag: String, count: u32 },
    /// Done after the player gathered `count` of the item from gatherables (`TaskRequireGatherItems`;
    /// client: "only increments progress when the player actually gathers something, not just
    /// when they already have the items in their inventory").
    RequireGather { item: String, count: u32 },
    /// Done after `count` gathers of the cyclic gatherable `cyclic` (a `VariantID`) in its state
    /// `state` (`TaskGatherCyclicState`, `ItemDropProbability` the state; client: "increments
    /// progress when the player gathers the specified cyclic gatherable state"). Retail
    /// (`09a_m13_attunestaff`): the gather of `09A_M13_ArcaneRitual_0` (state 0) began at
    /// 159.051 s and its step was done at 160.135 s, the gather of state 1 at 166.851 s and its
    /// step at 167.919 s.
    CyclicState {
        cyclic: String,
        state: u32,
        count: u32,
    },
    /// Done after `count` uses of this item (`TaskUseItems`; client: "The player needs to use the
    /// specific item"): a quest potion (`09A_S0_SporeElixir`).
    UseItems { item: String, count: u32 },
    /// Starts the objective of this id and is done when it ends (`TaskObjective`; client: "adds
    /// an additional objective to the player, and tracks it through until completion"): the
    /// steps of the weapon quest chains and the faction hand-offs.
    Objective(String),
    /// Done while the player is in territory `territory` (`TaskGoToTerritory`; client: "Variant
    /// of TaskGoTo that sends a player to a territory"), and in the point of interest `poi` when
    /// it has one (`TaskGoToPOITagOnTerritory`: "a poi on a specific territory").
    GoToTerritory { territory: u32, poi: Option<String> },
    /// Done when the player starts a talk with NPC `npc` (`TaskConversationState`; client: "Task
    /// that completes when the player enters a particular conversation state"). Retail
    /// (`12a_side_07_forgering`): the talk with `@12_JeongIseul` started at 220.660 s, the server
    /// accepted it at 220.684 s and the step was done at 220.696 s.
    ConversationState(String),
    /// Done while the player's level is at least `target` (`TaskHaveLevel`; client: "Succeeds if
    /// player is/achieves target level. If ItemName is provided for target Categorical
    /// Progression Id, then target level within that progression succeeds"): the character's
    /// level without a `progression`, else that progression's rank, or its points with
    /// `points` (`ShouldCheckPoints`).
    HaveLevel {
        progression: Option<String>,
        target: u32,
        points: bool,
    },
    /// Done once the player stayed `duration_ms` in its `POITag` (`PatrolTask`; client: "Variant of
    /// TaskGoTo that completes once the player is in the location of a given duration"). A
    /// leave starts the stay again (Inferred: "is in the location" for the whole time).
    Patrol { place: String, duration_ms: u64 },
    /// Done when the player enters the game mode of this id (`TaskGameMode`; client: "Task that
    /// progresses when the player enters the target Game Mode").
    GameMode(String),
    /// Gives the player `count` of the item when it starts, and is done (`TaskGiveItem`;
    /// client: "Gives the player an item and may remove it"). `taken`: the item goes back when
    /// the quest ends (`TaskGiveAndTakeItem`; client: "Is removed upon completion"; Inferred: at
    /// the quest's end, since the task itself ends when it gives).
    GiveItem {
        item: String,
        count: u32,
        taken: bool,
    },
    /// Done after `count` of this step of fishing (`TaskFishingEquip`, `TaskFishingCast`,
    /// `TaskFishingHook`, `TaskFishingReel`: the steps the fishing tutorial quests teach).
    Fishing { step: FishingStep, count: u32 },
    /// Done after `count` catches that [`FishCatchRule`] takes (`TaskFishingCatchFish`,
    /// `TaskFishingCatchAnyFish`, `TaskFishingCatchLength`, `TaskFishingCatchWeight`,
    /// `TaskFishingHotspotAny`).
    CatchFish { rule: FishCatchRule, count: u32 },
    /// Done after the player salvaged `count` of the item (`TaskSalvageItem`).
    Salvage { item: String, count: u32 },
    /// Done after `count` crafts of an item of this family (`TaskCraftWeaponFamily`: its
    /// `CraftingRecipeID` names the weapon mastery, `SwordAbilityTable`, of any sword).
    CraftFamily { family: String, count: u32 },
    /// Done while the player holds `count` items of this family (`TaskHaveItemsWithWeaponFamily`:
    /// a weapon mastery; `TaskHaveItemsWithItemFamily`: an `ItemClass` tag, `FishingTreasure`).
    HaveFamily { family: String, count: u32 },
    /// Done after `count` finished performances of this song (`TaskPerformSong`: its
    /// `ItemName` is the `SongId`, `WaltzEasy`), near its `OverrideLocation` when it has one
    /// (`13_Summer_09_PlayWithBand_playLocation1`).
    PerformSong {
        song: String,
        near: Option<TriggerArea>,
        count: u32,
    },
    /// Done when the player finishes an instance of this game mode (`TaskCompleteDungeon`:
    /// `DungeonGreatCleave00`): every encounter of its script done.
    CompleteGameMode(String),
    /// Counts the sections of this quick course the player passes and is done at its finish
    /// (`TaskQuickCourse`: its `ItemName` is the `QuickCourseId`, `EF_Tour_0`; retail
    /// `ef_tour_0-Hermit-s-Rally` ended the course with 6065 at the finish node).
    QuickCourse(String),
    /// Done when the player passes section `section` of this quick course
    /// (`TASKQUICKCOURSECHECKPOINT`: hidden, its `MinGearScore` the section, Inferred: 8 and 7
    /// on `17ZQ_QuickCourse_Silkfoot_Aster` of 9 sections, 9 on `MarAmar_Werewolf` of 10).
    QuickCourseSection { course: String, section: u32 },
    /// Done while the player owns `count` houses (`TaskHouseScore`: `1225_0201_d_01_PurchaseHouse`,
    /// "purchase a house", target 1; a new house's score is 0, retail 3692, so the target counts
    /// houses: Inferred, no captured character had the quest).
    OwnHouses(u32),
    /// Done when the player picks a faction (`TaskChooseFaction`: its `ItemName` is the faction
    /// representative, `9814_Beyza`, whose `Faction_Selection` service sends the pick; retail
    /// `Hive_join_yellow_faction_quest` completed the objective in the bundle after the 3161).
    ChooseFaction,
    /// Done after the player dies `count` times (`TaskDie`).
    Die { count: u32 },
    /// A task whose row names nothing to wait for (an empty cell its type needs: the item of
    /// `01_SoleSurvivor_GiveItem1`, the place of ten `TaskGoToPOITag` rows): done at once, as
    /// retail runs `01_SoleSurvivor_GiveItem1` (the real tutorial run test below).
    Empty,
    /// A task type this server does not run yet (puzzle slayer scripts, emotes, PvP control
    /// points): never done, so its quest waits and pays no reward; its start logs it.
    NotRun(String),
}

/// A step of fishing that a fishing task counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FishingStep {
    /// The pole came out (`FishingEquipped`).
    Equip,
    /// The bobber landed in water (`CastHitWater`).
    Cast,
    /// The hook was set on a fish (`HookHit`).
    Hook,
    /// The player reeled a hooked fish in (`ReelingActive`).
    Reel,
}

/// Which catches a fishing catch task counts.
#[derive(Debug, Clone, PartialEq)]
pub struct FishCatchRule {
    /// The item caught; `None`: any fish.
    item: Option<String>,
    /// The least length and weight (`TargetThreshold`).
    min_length: Option<f32>,
    min_weight: Option<f32>,
    /// Only catches at a hotspot count.
    hotspot: bool,
    /// The quest fish the task's item is: it bites while the task waits.
    drop: Option<FishDrop>,
}

impl FishCatchRule {
    /// Any fish anywhere.
    const ANY: Self = Self {
        item: None,
        min_length: None,
        min_weight: None,
        hotspot: false,
        drop: None,
    };

    /// Whether it takes `catch`.
    fn takes(&self, catch: &FishCaught<'_>) -> bool {
        self.item
            .as_deref()
            .is_none_or(|item| item.eq_ignore_ascii_case(catch.item))
            && self.min_length.is_none_or(|least| catch.length >= least)
            && self.min_weight.is_none_or(|least| catch.weight >= least)
            && (!self.hotspot || catch.hotspot)
    }
}

/// The quest fish of a catch task: a bite in its water and territory is the task's item at its
/// chance (`ItemDropProbability`). Quest fish (`0223_0101_HookedSquid`, `16_Side_11_TastyFish`)
/// have `FishingCatchables` rows and no loot table row: only the task makes them bite.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FishDrop {
    percent: u32,
    /// The water tag (`LootTag`); `None`: any water.
    water: Option<String>,
    /// The territory (`TerritoryID`); `None`: any.
    territory: Option<u32>,
}

/// The item a mission task adds when the player opens a container (a loot crate) in its place
/// (`ChestDropProbability`, a mission's `TaskHaveAndReturnChestDropProbabilityOverride`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChestDrop {
    /// Its chance in percent.
    percent: u32,
    /// The `POITag` the container must be in; `None`: anywhere.
    place: Option<String>,
}

/// The item a gather task adds when the player skins a corpse of its enemy or gathers a quest
/// object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QuestDrop {
    /// `ItemDropVC`: the enemy type, as a kill task names one, or the `GatherableID` of a quest
    /// object (`MSQ_Treasure_A`).
    enemy: String,
    /// `ItemDropProbability`, in percent.
    percent: u32,
}

/// One task of a tree.
#[derive(Debug, Clone, PartialEq)]
struct Task {
    id: String,
    kind: TaskKind,
    stay_active: bool,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// The task tree of one objective, built from the content.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveTree {
    /// Depth first; the root is task 0.
    tasks: Vec<Task>,
    /// Each task's number on the wire: its place when the tree is walked children first
    /// (objectives-state note, section 4: the root is last).
    wire_index: Vec<u8>,
}

/// The `ObjectiveTasks` fields one task needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskRow {
    /// `TaskID`.
    pub task_id: String,
    /// `Type`.
    pub kind: String,
    /// `ItemName`.
    pub item_name: Option<String>,
    /// `TargetQty`.
    pub target_qty: Option<i64>,
    /// `Duration` in seconds.
    pub duration: Option<f64>,
    /// `OverrideLocation`: "x y z".
    pub override_location: Option<String>,
    /// `KillEnemyType`.
    pub kill_enemy_type: Option<String>,
    /// `UIScreenTag`.
    pub ui_screen_tag: Option<String>,
    /// `SubTask` ids in order.
    pub sub_tasks: Vec<String>,
    /// `TP_TargetTag`: the trigger volume of an area task.
    pub target_tag: Option<String>,
    /// `CraftingRecipeID`.
    pub crafting_recipe_id: Option<String>,
    /// `ItemDropVC`.
    pub item_drop_vc: Option<String>,
    /// `ItemDropProbability`.
    pub item_drop_probability: Option<i64>,
    /// `POITag`: the point of interest of a go-to or exit task.
    pub poi_tag: Option<String>,
    /// `TriggerAreaType`: `Enter` or `Leave` for an area task.
    pub trigger_area_type: Option<String>,
    /// `FailOnCondition`. On a `TaskHaveAndReturnItems` with no `ItemName`, this is the weapon
    /// drop the task belongs to (`2hGreatSwordDropT2` on the greatsword take).
    pub fail_on_condition: Option<String>,
    /// `InteractTag`.
    pub interact_tag: Option<String>,
    /// `GameModeId`.
    pub game_mode_id: Option<String>,
    /// `ShouldCheckPoints`.
    pub should_check_points: bool,
    /// A mission's task whose items go back at the turn-in (`MissionData` overrides; retail: two
    /// item changes 31 to 35 ms after each 2461 turn-in, board capture 2026-06-13).
    pub returned_on_turn_in: bool,
    /// `ChestDropProbability`.
    pub chest_drop_probability: Option<i64>,
    /// `EnforceLocationRequirement`: the task counts only in its `POITag`.
    pub enforce_location: bool,
    /// `StayActive`: the task runs on when it is done.
    pub stay_active: bool,
    /// `TerritoryID`: the territory of a go-to-territory task (`0` names none).
    pub territory_id: Option<u32>,
    /// `LootTag`: the water (`Fresh` or `Salt`) a fishing task's quest fish bites in.
    pub loot_tag: Option<String>,
    /// `TargetThreshold`: the least length or weight of a fish a fishing task counts.
    pub target_threshold: Option<f64>,
    /// `MinGearScore`: of a quick course checkpoint task, the section it waits for.
    pub min_gear_score: Option<i64>,
}

/// Every task row with an index by lowercase task id, made once for the trees of many
/// objectives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskBook {
    rows: Vec<TaskRow>,
    by_id: HashMap<String, usize>,
}

impl TaskBook {
    /// The book of `rows`.
    pub fn new(rows: Vec<TaskRow>) -> Self {
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.task_id.to_lowercase(), index))
            .collect();
        Self { rows, by_id }
    }

    /// The tree under task `root`, as [`ObjectiveTree::build`].
    pub fn tree(&self, root: &str) -> Option<ObjectiveTree> {
        self.tree_with(root, |_| None)
    }

    fn row(&self, id: &str) -> Option<&TaskRow> {
        self.by_id
            .get(&id.to_lowercase())
            .and_then(|index| self.rows.get(*index))
    }

    /// The tree under task `root` with each row that `changed` gives a new value for replaced
    /// by it (a mission's overrides of its objective template).
    pub fn tree_with(
        &self,
        root: &str,
        changed: impl Fn(&TaskRow) -> Option<TaskRow>,
    ) -> Option<ObjectiveTree> {
        let mut replaced = HashMap::new();
        let mut pending = vec![root.to_lowercase()];
        while let Some(id) = pending.pop() {
            let Some(row) = self.row(&id) else {
                continue;
            };
            if replaced.contains_key(&id) {
                continue;
            }
            pending.extend(row.sub_tasks.iter().map(|child| child.to_lowercase()));
            replaced.insert(id, changed(row));
        }
        ObjectiveTree::build_with(
            &|id| {
                let id = id.to_lowercase();
                match replaced.get(&id) {
                    Some(Some(row)) => Some(row),
                    _ => self.row(&id),
                }
            },
            root,
        )
    }
}

/// The task rows of `tables`.
pub fn task_rows(tables: &Tables) -> Vec<TaskRow> {
    tables
        .objective_tasks
        .iter()
        .flat_map(|table| &table.rows)
        .map(|row| TaskRow {
            task_id: row.task_id.clone(),
            kind: row.r#type.clone(),
            item_name: row.item_name.clone(),
            target_qty: row.target_qty,
            duration: row.duration,
            override_location: row.override_location.clone(),
            kill_enemy_type: row.kill_enemy_type.clone(),
            ui_screen_tag: row.ui_screen_tag.clone(),
            sub_tasks: row.sub_task.values().cloned().collect(),
            target_tag: row.tp_target_tag.clone(),
            crafting_recipe_id: row.crafting_recipe_id.clone(),
            item_drop_vc: row.item_drop_vc.clone(),
            item_drop_probability: row.item_drop_probability,
            poi_tag: row.poi_tag.clone(),
            trigger_area_type: row.trigger_area_type.clone(),
            fail_on_condition: row.fail_on_condition.clone(),
            interact_tag: row.interact_tag.clone(),
            game_mode_id: row.game_mode_id.clone(),
            should_check_points: row.should_check_points.unwrap_or_default(),
            returned_on_turn_in: false,
            chest_drop_probability: row.chest_drop_probability,
            enforce_location: row.enforce_location_requirement.unwrap_or_default(),
            territory_id: row
                .territory_id
                .as_deref()
                .and_then(|cell| cell.trim().parse().ok())
                .filter(|territory| *territory != 0),
            stay_active: row
                .stay_active
                .as_deref()
                .is_some_and(|cell| STAY_ACTIVE_CELLS.contains(&cell.to_lowercase().as_str())),
            loot_tag: row.loot_tag.clone(),
            target_threshold: row.target_threshold,
            min_gear_score: row.min_gear_score,
        })
        .collect()
}

/// The game events (lowercase ids) that a `TaskGameEvent` of `rows` counts.
pub fn counted_game_events(rows: &[TaskRow]) -> HashSet<String> {
    rows.iter()
        .filter_map(|row| match kind_of(row) {
            TaskKind::GameEvent { event, .. } => Some(event.to_lowercase()),
            _ => None,
        })
        .collect()
}

/// A location cell "x y z"; `None` when it is not three numbers.
fn location_of(cell: &str) -> Option<Position> {
    let mut numbers = cell
        .split(LOCATION_SEPARATOR)
        .filter(|part| !part.is_empty())
        .map(str::parse::<f32>);
    let position = Position {
        x: numbers.next()?.ok()?,
        y: numbers.next()?.ok()?,
        z: numbers.next()?.ok()?,
    };
    numbers.next().is_none().then_some(position)
}

/// The mark before a weapon id in an archetype item (`SwordsmanArchetype_2hGreatsword`).
const ARCHETYPE_MARK: &str = "archetype_";
/// A one-handed weapon id starts with this (`1hRapierDropT2`).
const ONE_HAND_MARK: &str = "1h";
/// A two-handed weapon id starts with this (`2hGreatSwordDropT2`).
const TWO_HAND_MARK: &str = "2h";
/// The drop-tier tail of a weapon take's `FailOnCondition` (`DropT2`).
const DROP_TIER_MARK: &str = "dropt";
/// The lost-tier tail of the same kind of id (`LostT2`).
const LOST_TIER_MARK: &str = "lostt";

/// The item a have-and-return task watches. A row with `ItemName` watches that item. A row with
/// only `FailOnCondition` watches that drop id (the seven tutorial weapon takes). A row with
/// neither is done by any container loot.
fn have_item_of(row: &TaskRow) -> Option<String> {
    filled_cell(&row.item_name).or_else(|| filled_cell(&row.fail_on_condition))
}

fn filled_cell(cell: &Option<String>) -> Option<String> {
    cell.clone().filter(|text| !text.is_empty())
}

/// The weapon family of `id`, without hand, archetype, or tier (`greatsword` from
/// `SwordsmanArchetype_2hGreatsword` and from `2hGreatSwordDropT2`). `None` when `id` is not a
/// handed weapon id.
fn weapon_family(id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase();
    let core = lower
        .rsplit_once(ARCHETYPE_MARK)
        .map_or(lower.as_str(), |(_, rest)| rest);
    let core = core.trim_end_matches(|letter: char| letter.is_ascii_digit());
    let core = core
        .strip_suffix(DROP_TIER_MARK)
        .or_else(|| core.strip_suffix(LOST_TIER_MARK))
        .unwrap_or(core);
    let family = core
        .strip_prefix(ONE_HAND_MARK)
        .or_else(|| core.strip_prefix(TWO_HAND_MARK))?;
    let family = family.trim_matches('_');
    (!family.is_empty()).then(|| family.to_owned())
}

/// Whether `task` is a simple container whose children are all weapon takes.
fn weapon_take_group(tree: &ObjectiveTree, task: &Task) -> bool {
    !task.children.is_empty()
        && task
            .children
            .iter()
            .all(|child| have_family(tree, *child).is_some())
}

/// The weapon family of a have-item task.
fn have_family(tree: &ObjectiveTree, index: usize) -> Option<String> {
    match &tree.tasks.get(index)?.kind {
        TaskKind::HaveItem(Some(id)) => weapon_family(id),
        _ => None,
    }
}

/// The kind of `row`. The type cell's case varies (Observed: `TASKUNLOCKACHIEVEMENT` and
/// `TasklockAchievement` in the objective task sheets), so it is matched without case.
fn kind_of(row: &TaskRow) -> TaskKind {
    let item = || row.item_name.clone().unwrap_or_default();
    let crc = || lowercase_crc32(&item());
    let count = row
        .target_qty
        .and_then(|qty| u32::try_from(qty).ok())
        .unwrap_or(1)
        .max(1);
    let filled = |cell: &Option<String>| cell.clone().filter(|text| !text.is_empty());
    let drop = || {
        filled(&row.item_drop_vc).map(|enemy| QuestDrop {
            enemy,
            percent: row
                .item_drop_probability
                .and_then(|percent| u32::try_from(percent).ok())
                .unwrap_or(ALWAYS_PERCENT),
        })
    };
    let place = || row.enforce_location.then(|| filled(&row.poi_tag)).flatten();
    let chest = || {
        row.chest_drop_probability
            .and_then(|percent| u32::try_from(percent).ok())
            .filter(|percent| *percent > 0)
            .map(|percent| ChestDrop {
                percent,
                place: place(),
            })
    };
    let gather = |returned| TaskKind::GatherItems {
        item: item(),
        count,
        drop: drop(),
        returned,
    };
    let fishing = |step| TaskKind::Fishing { step, count };
    let catch = |rule| TaskKind::CatchFish { rule, count };
    // Lengths and weights are a few tens: the cast loses nothing.
    let threshold = || row.target_threshold.map(|threshold| threshold as f32);
    match row.kind.to_ascii_lowercase().as_str() {
        "consecutivetaskcontainer" => TaskKind::Consecutive,
        "partialtaskcontainer" => TaskKind::Partial,
        "simpletaskcontainer" => TaskKind::Simple,
        "taskunlockachievement" => TaskKind::UnlockAchievement(item()),
        "tasklockachievement" => TaskKind::LockAchievement(item()),
        "tasktemporaryunlockachievement" => TaskKind::TemporaryUnlock(item()),
        "taskachievement" => TaskKind::HasAchievement(item()),
        "taskremoveitem" => TaskKind::RemoveItem {
            item: item(),
            count,
        },
        "tasktimer" => TaskKind::Timer {
            // Durations are a few seconds: the cast cannot overflow.
            duration_ms: (row.duration.unwrap_or_default().max(0.0) * MS_PER_SECOND) as u64,
        },
        "taskcinematicstart" => TaskKind::CinematicStart(crc()),
        "taskcinematiccomplete" => TaskKind::CinematicComplete(crc()),
        "taskcutscenecomplete" => TaskKind::CutsceneComplete(crc()),
        "tasktriggerarea" => {
            let location = row.override_location.as_deref().and_then(location_of);
            let area = TriggerArea::of(row.target_tag.as_deref(), location);
            let leave = row
                .trigger_area_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case(LEAVE_AREA_TYPE));
            let known_volume = matches!(area, Some(TriggerArea::Volume(_)));
            // Without a location or a tutorial volume, the area is its POI, else the level's
            // quest volume of its tag ([`nw_world::collision::QuestVolume`]: the sim counts the
            // player in that tag while inside).
            let poi = (!leave).then(|| filled(&row.poi_tag)).flatten();
            TaskKind::TriggerArea {
                area,
                leave,
                place: (location.is_none() && !known_volume)
                    .then(|| poi.or_else(|| filled(&row.target_tag)))
                    .flatten(),
            }
        }
        "taskkillcontribution" => TaskKind::KillContribution {
            enemy: row.kill_enemy_type.clone().unwrap_or_default(),
            count,
            place: place(),
        },
        "taskhaveandreturnitems" if row.returned_on_turn_in => {
            have_item_of(row).map_or(TaskKind::Empty, |item| TaskKind::HaveItems {
                item,
                count,
                returned: true,
                drop: drop(),
                chest: chest(),
            })
        }
        "taskhaveandreturnitems" => TaskKind::HaveItem(have_item_of(row)),
        "taskhaveitems" => TaskKind::HaveItems {
            item: item(),
            count,
            returned: false,
            drop: None,
            chest: None,
        },
        "taskgatheritems" => gather(false),
        "taskgatherandreturnitems" => gather(true),
        "taskcraftrecipe" => {
            TaskKind::CraftRecipe(row.crafting_recipe_id.clone().unwrap_or_default())
        }
        "taskcraftmultiplerecipes" => {
            let recipes = categories_of(row.item_drop_vc.as_deref().unwrap_or_default());
            if recipes.is_empty() {
                TaskKind::Empty
            } else {
                TaskKind::CraftAnyRecipe { recipes, count }
            }
        }
        "taskgameevent" => filled(&row.kill_enemy_type).map_or(TaskKind::Never, |event| {
            TaskKind::GameEvent { event, count }
        }),
        "tasktriggergameevent" => {
            filled(&row.kill_enemy_type).map_or(TaskKind::Empty, TaskKind::TriggerGameEvent)
        }
        // A go-to with only a location (`03_SQ_04_WulfstanGlyphs_GoToKazu`) is done near it.
        "taskgotopoitag" => match (
            filled(&row.poi_tag),
            row.override_location.as_deref().and_then(location_of),
        ) {
            (Some(poi), _) => TaskKind::GoToPoi(
                poi.split(POI_TAG_SEPARATOR)
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            (None, Some(location)) => TaskKind::TriggerArea {
                area: Some(TriggerArea::Around(location)),
                leave: false,
                place: None,
            },
            (None, None) => TaskKind::Empty,
        },
        "taskexitpoitag" => filled(&row.poi_tag).map_or(TaskKind::Empty, TaskKind::ExitPoi),
        "taskequipitem" => TaskKind::EquipItem(item()),
        "taskgamemode" => {
            filled(&row.game_mode_id).map_or(TaskKind::Empty, TaskKind::GameMode)
        }
        "taskconversationstate" => filled(&row.item_name)
            .or_else(|| {
                filled(&row.ui_screen_tag).and_then(|state| {
                    state
                        .rsplit_once(CONVERSATION_STATE_MARK)
                        .map(|(npc, _)| npc.to_owned())
                })
            })
            .map_or(TaskKind::Empty, TaskKind::ConversationState),
        "taskgototerritory" => {
            row.territory_id
                .map_or(TaskKind::Empty, |territory| TaskKind::GoToTerritory {
                    territory,
                    poi: None,
                })
        }
        "taskgotopoitagonterritory" => match (row.territory_id, filled(&row.poi_tag)) {
            (Some(territory), Some(poi)) => TaskKind::GoToTerritory {
                territory,
                poi: Some(poi),
            },
            _ => TaskKind::Empty,
        },
        "taskuseitems" => filled(&row.item_name).map_or(TaskKind::Empty, |item| {
            TaskKind::UseItems { item, count }
        }),
        "taskobjective" => {
            filled(&row.item_name).map_or(TaskKind::Empty, TaskKind::Objective)
        }
        "taskrequiregatheritems" => TaskKind::RequireGather {
            item: item(),
            count,
        },
        "taskgathercyclicstate" => {
            filled(&row.item_name).map_or(TaskKind::Empty, |cyclic| TaskKind::CyclicState {
                cyclic,
                state: row
                    .item_drop_probability
                    .and_then(|state| u32::try_from(state).ok())
                    .unwrap_or_default(),
                count,
            })
        }
        "taskhavelevel" => TaskKind::HaveLevel {
            progression: filled(&row.item_name),
            target: row
                .target_qty
                .and_then(|target| u32::try_from(target).ok())
                .unwrap_or_default(),
            points: row.should_check_points,
        },
        // A patrol without its place is a PvP mission template (`PVP_task_patrol`): not run.
        "patroltask" => filled(&row.poi_tag).map_or(TaskKind::NotRun(row.kind.clone()), |place| {
            TaskKind::Patrol {
                place,
                // Durations are whole minutes or less: the cast cannot overflow.
                duration_ms: (row.duration.unwrap_or_default().max(0.0) * MS_PER_SECOND) as u64,
            }
        }),
        // A give with an interact tag and no item (`12A_MSQ_24_DefeatGawain_CollectCrownItem`)
        // is done by the interact with its object.
        "taskgiveitem" => match (filled(&row.item_name), filled(&row.interact_tag)) {
            (Some(item), _) => TaskKind::GiveItem {
                item,
                count,
                taken: false,
            },
            (None, Some(tag)) => TaskKind::Interact { tag, count },
            (None, None) => TaskKind::Empty,
        },
        // A carried item that a container or a mob drops is waited for as an item task; one
        // without a drop is given at the start. Both go back when the quest ends.
        "taskgiveandtakeitem" => match filled(&row.item_name) {
            None => TaskKind::Empty,
            Some(item) if chest().is_some() || drop().is_some() => TaskKind::HaveItems {
                item,
                count,
                returned: true,
                drop: drop(),
                chest: chest(),
            },
            Some(item) => TaskKind::GiveItem {
                item,
                count,
                taken: true,
            },
        },
        "taskinteract" => filled(&row.interact_tag).map_or(TaskKind::Empty, |tag| {
            TaskKind::Interact { tag, count }
        }),
        "taskconversationtopic" => {
            let topic = filled(&row.ui_screen_tag).unwrap_or_else(|| row.task_id.clone());
            TaskKind::ConversationTopic(lowercase_crc32(&topic))
        }
        "taskopenuiscreen" => {
            TaskKind::OpenUiScreen(lowercase_crc32(row.ui_screen_tag.as_deref().unwrap_or("")))
        }
        "taskfishingequip" => fishing(FishingStep::Equip),
        "taskfishingcast" => fishing(FishingStep::Cast),
        "taskfishinghook" => fishing(FishingStep::Hook),
        "taskfishingreel" => fishing(FishingStep::Reel),
        "taskfishingcatchfish" => catch(FishCatchRule {
            item: filled(&row.item_name),
            drop: filled(&row.item_name)
                .and(row.item_drop_probability)
                .and_then(|percent| u32::try_from(percent).ok())
                .filter(|percent| *percent > 0)
                .map(|percent| FishDrop {
                    percent,
                    water: filled(&row.loot_tag),
                    territory: row.territory_id,
                }),
            ..FishCatchRule::ANY
        }),
        "taskfishingcatchanyfish" => catch(FishCatchRule::ANY),
        "taskfishingcatchlength" => catch(FishCatchRule {
            min_length: threshold(),
            ..FishCatchRule::ANY
        }),
        "taskfishingcatchweight" => catch(FishCatchRule {
            min_weight: threshold(),
            ..FishCatchRule::ANY
        }),
        "taskfishinghotspotany" => catch(FishCatchRule {
            hotspot: true,
            ..FishCatchRule::ANY
        }),
        "tasksalvageitem" => filled(&row.item_name).map_or(TaskKind::Empty, |item| {
            TaskKind::Salvage { item, count }
        }),
        "taskcraftweaponfamily" => {
            filled(&row.crafting_recipe_id).map_or(TaskKind::Empty, |family| {
                TaskKind::CraftFamily { family, count }
            })
        }
        "taskhaveitemswithweaponfamily" | "taskhaveitemswithitemfamily" => filled(&row.item_name)
            .map_or(TaskKind::Empty, |family| TaskKind::HaveFamily {
                family,
                count,
            }),
        "taskchoosefaction" => TaskKind::ChooseFaction,
        "taskquickcourse" => {
            filled(&row.item_name).map_or(TaskKind::Empty, TaskKind::QuickCourse)
        }
        "taskquickcoursecheckpoint" => match (
            filled(&row.item_name),
            row.min_gear_score
                .and_then(|section| u32::try_from(section).ok()),
        ) {
            (Some(course), Some(section)) => TaskKind::QuickCourseSection { course, section },
            _ => TaskKind::Empty,
        },
        "taskhousescore" => TaskKind::OwnHouses(count),
        "taskdie" => TaskKind::Die { count },
        "taskcompletedungeon" => {
            filled(&row.game_mode_id).map_or(TaskKind::Empty, TaskKind::CompleteGameMode)
        }
        "taskperformsong" => {
            filled(&row.item_name).map_or(TaskKind::Empty, |song| TaskKind::PerformSong {
                song,
                near: row
                    .override_location
                    .as_deref()
                    .and_then(location_of)
                    .map(TriggerArea::Around),
                count,
            })
        }
        // A refining recipe is a crafting recipe of a refining station (`GypsumOrb` at the kiln).
        "taskrefinerecipe" => {
            filled(&row.crafting_recipe_id).map_or(TaskKind::Empty, TaskKind::CraftRecipe)
        }
        _ => TaskKind::NotRun(row.kind.clone()),
    }
}

impl ObjectiveTree {
    /// The tree under task `root` of `rows`; `None` when `root` is not a row. A sub-task that is
    /// not a row, or that is already one of its own parents, is left out.
    pub fn build(rows: &[TaskRow], root: &str) -> Option<Self> {
        let by_id: HashMap<String, &TaskRow> = rows
            .iter()
            .map(|row| (row.task_id.to_lowercase(), row))
            .collect();
        Self::build_with(&|id| by_id.get(&id.to_lowercase()).copied(), root)
    }

    /// The tree under task `root`, with `find` giving the row of a task id.
    fn build_with<'r>(find: &dyn Fn(&str) -> Option<&'r TaskRow>, root: &str) -> Option<Self> {
        let mut tree = Self {
            tasks: Vec::new(),
            wire_index: Vec::new(),
        };
        let mut path = Vec::new();
        tree.add(find, root, None, &mut path)?;
        tree.wire_index = vec![0; tree.tasks.len()];
        let mut next = 0;
        tree.number_children_first(0, &mut next);
        Some(tree)
    }

    /// Numbers task `index` after its children, from `next` on (the wire number is a `u8`; a
    /// tree of more than 256 tasks wraps).
    fn number_children_first(&mut self, index: usize, next: &mut usize) {
        for child in self.tasks[index].children.clone() {
            self.number_children_first(child, next);
        }
        self.wire_index[index] = *next as u8;
        *next += 1;
    }

    fn add<'r>(
        &mut self,
        find: &dyn Fn(&str) -> Option<&'r TaskRow>,
        id: &str,
        parent: Option<usize>,
        path: &mut Vec<String>,
    ) -> Option<usize> {
        let key = id.to_lowercase();
        if path.contains(&key) {
            return None;
        }
        let row = find(&key)?;
        let index = self.tasks.len();
        self.tasks.push(Task {
            id: row.task_id.clone(),
            kind: kind_of(row),
            stay_active: row.stay_active,
            parent,
            children: Vec::new(),
        });
        path.push(key);
        let children: Vec<usize> = row
            .sub_tasks
            .iter()
            .filter_map(|child| self.add(find, child, Some(index), path))
            .collect();
        path.pop();
        self.tasks[index].children = children;
        Some(index)
    }

    /// Tasks in the tree.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// The items its gather-and-return tasks take back at the turn-in, with their counts.
    pub fn returned_items(&self) -> Vec<(String, u32)> {
        self.tasks
            .iter()
            .filter_map(|task| match &task.kind {
                TaskKind::GatherItems {
                    item,
                    count,
                    returned: true,
                    ..
                }
                | TaskKind::HaveItems {
                    item,
                    count,
                    returned: true,
                    ..
                }
                | TaskKind::GiveItem {
                    item,
                    count,
                    taken: true,
                } => Some((item.clone(), *count)),
                _ => None,
            })
            .collect()
    }
}

/// A game event that can complete tasks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event<'a> {
    /// The player died.
    Died,
    /// A cinematic video started (CRC-32 of its lowercase id).
    CinematicStarted(u32),
    /// A cinematic video ended.
    CinematicEnded(u32),
    /// An in-game cutscene ended (CRC-32 of its lowercase name).
    CutsceneEnded(u32),
    /// The player killed a mob with these vitals id and categories.
    Killed {
        /// Its vitals id.
        vitals_id: &'a str,
        /// Its `VitalsCategories`.
        categories: &'a [String],
        /// Its spawner's category and that category's groups (CRC-32s of the lowercase ids).
        spawn_categories: &'a [u32],
    },
    /// The player is here.
    Moved(Position),
    /// The player got this many of this item.
    ItemGained {
        /// The item id.
        item: &'a str,
        /// How many.
        quantity: u32,
    },
    /// The player took all items of a container. `items` are the item ids that came out of it.
    ContainerLooted {
        /// The item ids.
        items: &'a [&'a str],
    },
    /// The player equipped this item.
    Equipped(&'a str),
    /// The client opened this UI screen (CRC-32 of its lowercase name).
    UiScreenOpened(u32),
    /// The client finished this conversation topic (CRC-32 of its lowercase id).
    TopicFinished(u32),
    /// The player got this achievement.
    AchievementSet(&'a str),
    /// The player crafted the recipe of this id.
    Crafted(&'a str),
    /// The game event of this id happened for the player: the gather of a quest object, or one
    /// that a task fired.
    GameEvent(&'a str),
    /// The player ended an interact with an object that has this interact tag.
    Interacted(&'a str),
    /// The player entered the game mode of this id (an instance of an expedition or a quest).
    GameModeEntered(&'a str),
    /// The player started a talk with the NPC of this id.
    Talked(&'a str),
    /// The player used one of this item.
    ItemUsed(&'a str),
    /// The player's objective of this id ended (now, or before it was asked for).
    ObjectiveDone(&'a str),
    /// The player got this many of this item from a gather.
    Gathered {
        /// The item id.
        item: &'a str,
        /// How many.
        quantity: u32,
    },
    /// The player gathered the cyclic gatherable of this variant id in this state.
    CyclicGathered {
        /// The cyclic gatherable's `VariantID`.
        cyclic: &'a str,
        /// Its state at the gather, from 0.
        state: u32,
    },
    /// The player's fishing took this step.
    Fishing(FishingStep),
    /// The player caught a fish.
    FishCaught(FishCaught<'a>),
    /// The player salvaged this many of this item.
    Salvaged {
        /// The item id.
        item: &'a str,
        /// How many.
        quantity: u32,
    },
    /// The player crafted an item of these families (its `ItemClass` tags and weapon mastery).
    CraftedFamilies(&'a [String]),
    /// The player finished an instance of the game mode of this id.
    GameModeCompleted(&'a str),
    /// The player joined or changed to this faction (`nw_content::factions`).
    FactionChosen(u8),
    /// The player passed section `section` (its index) of this quick course of `sections`; the
    /// last one is its finish.
    QuickCourseSection {
        /// The `QuickCourseId`.
        course: &'a str,
        /// The section's index.
        section: u32,
        /// The course's sections.
        sections: u32,
    },
    /// The player finished a performance of this song here.
    SongPerformed {
        /// The `SongId`.
        song: &'a str,
        /// Where the player stood.
        at: Position,
    },
}

/// A fish the player caught.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FishCaught<'a> {
    /// Its item id.
    pub item: &'a str,
    /// Its length, as its `FishingCatchables` range.
    pub length: f32,
    /// Its weight, as its `FishingCatchables` range.
    pub weight: f32,
    /// It bit at a hotspot.
    pub hotspot: bool,
}

/// Whether an item (its id) is of a family (an `ItemClass` tag or a weapon mastery).
pub type OfFamily<'a> = dyn Fn(&str, &str) -> bool + 'a;

/// The player's level of a progression id (`None`: the character), as its rank or its points.
pub type LevelOf<'a> = dyn Fn(Option<&str>, bool) -> Option<u32> + 'a;

/// What the player is now, for the tasks done while it is so.
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    /// Its items: id, quantity, worn.
    pub held: &'a [(&'a str, u32, bool)],
    /// The `POITag`s of the points of interest it is in; `None` when the server does not know
    /// them, so no go-to or exit task is judged.
    pub places: Option<&'a [&'a str]>,
    /// Whether it has an achievement; `None` when the server does not know.
    pub achieved: Option<&'a dyn Fn(&str) -> bool>,
    /// Its level: the character's (counted from 1) without a progression id, else that
    /// progression's rank, or its points when asked; `None` when the server does not know it.
    pub level: Option<&'a LevelOf<'a>>,
    /// The territories (zones) it is in; `None` when the server does not know them.
    pub territories: Option<&'a [u32]>,
    /// Whether an item (its id) is of a family (an `ItemClass` tag or a weapon mastery); `None`
    /// when the server does not know, so no family task is judged.
    pub of_family: Option<&'a OfFamily<'a>>,
    /// How many houses it owns; `None` when the server does not know, so no house task is
    /// judged.
    pub houses: Option<u32>,
}

/// Where a task the quest tracker shows stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// It runs.
    Running,
    /// It is done.
    Done,
    /// It is done and runs on (`StayActive`). Retail sent `CompleteActive` for each of 738 done
    /// `StayActive` tasks and `Complete` for none (40 open-world captures).
    DoneActive,
    /// It stopped undone: a branch of a partial container that another branch finished.
    Stopped,
}

/// A task the quest tracker shows: a task that is done, or that runs and has progress, or that
/// stopped after it had some (objectives-state note, section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskProgress {
    /// Its number on the wire (children first).
    pub index: u8,
    /// CRC-32 of the lowercase id.
    pub task: u32,
    /// Children done for a container; kills or items so far for a counting task; the sections of
    /// a ride; 1 for another task that is done.
    pub progress: u32,
    /// Where it stands.
    pub status: TaskStatus,
}

/// What the tree asks of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Set this achievement.
    Achievement(String),
    /// Clear this achievement.
    LockAchievement(String),
    /// Remove this many of this item from the player.
    RemoveItem(String, u32),
    /// Give the player this many of this item.
    GiveItem(String, u32),
    /// Fire this game event: its whisper plays and its reward goes to the player.
    GameEvent(String),
    /// Start the objective of this id for the player, as a chain step.
    StartObjective(String),
    /// This task is done.
    Done(String),
}

/// The state of one task in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskState {
    /// Not started, or stopped because a partial container ended.
    Idle,
    /// Waiting: kills or items so far, and a timer's end.
    Active { progress: u32, due_ms: Option<u64> },
    /// Done.
    Done,
}

/// One player's run of an objective tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveRun {
    tree: Arc<ObjectiveTree>,
    states: Vec<TaskState>,
    /// The tasks with progress, in the order they first had it (retail keeps an entry's place
    /// and adds new ones at the end).
    listed: Vec<usize>,
    /// The lowercase `POITag`s the player was in at the last [`ObjectiveRun::sync`]; `None`
    /// before one that knew them.
    places: Option<Vec<String>>,
    /// The sections of each done `TaskQuickCourse`, its tracker's last progress.
    course_sections: Vec<(usize, u32)>,
}

/// Whether `enemy` names a mob of `vitals_id`, `categories` and spawn categories
/// `spawn_categories` (CRC-32s of the lowercase ids, [`KillTypes::spawn_categories`]): one of
/// its categories or its vitals id, or the start of its vitals id (live-tutorial note:
/// `Legion_Legionnaire` counts `Legion_Legionnaire_FTUE`), or its spawner's category or that
/// category's group (`Barbary_Lion_16BS12`, `Periville`). Case does not matter.
fn kills_count(
    enemy: &str,
    vitals_id: &str,
    categories: &[String],
    spawn_categories: &[u32],
) -> bool {
    let names = |name: &str| {
        name.eq_ignore_ascii_case(enemy)
            || name.len() > enemy.len()
                && name.is_char_boundary(enemy.len())
                && name[..enemy.len()].eq_ignore_ascii_case(enemy)
                && name[enemy.len()..].starts_with(ID_PART_SEPARATOR)
    };
    names(vitals_id)
        || categories.iter().any(|category| names(category))
        || spawn_categories.contains(&lowercase_crc32(enemy))
}

/// The categories of a `VitalsCategories` cell (also the recipe list of a multiple recipe task).
pub fn categories_of(cell: &str) -> Vec<String> {
    cell.split(CATEGORY_SEPARATOR)
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The vitals id and categories of every mob type, by the CRC-32 of its lowercase vitals id: what
/// a kill task matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillTypes {
    by_vitals: HashMap<u32, (String, Vec<String>)>,
    /// The group of each vitals category (`GroupVitalsCategoryID`), by CRC-32 of the lowercase
    /// ids.
    groups: HashMap<u32, u32>,
}

impl KillTypes {
    /// The kill types of `VitalsBaseData` in `tables`, with the category groups of
    /// `VitalsCategoryData`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .vitals_base_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    let categories = row.vitals_categories.clone().unwrap_or_default();
                    (row.vitals_id.clone(), categories)
                }),
        )
        .with_groups(
            tables
                .vitals_category_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let group = row
                        .group_vitals_category_id
                        .clone()
                        .filter(|group| !group.is_empty())?;
                    Some((row.vitals_category_id.clone(), group))
                }),
        )
    }

    /// These kill types with the category groups `groups` (category id, group id).
    pub fn with_groups(mut self, groups: impl IntoIterator<Item = (String, String)>) -> Self {
        self.groups = groups
            .into_iter()
            .map(|(category, group)| (lowercase_crc32(&category), lowercase_crc32(&group)))
            .collect();
        self
    }

    /// The CRC-32s of spawn category `spawn_category` and of its groups, the group's group and
    /// on; empty without one.
    pub fn spawn_categories(&self, spawn_category: Option<u32>) -> Vec<u32> {
        let mut found = Vec::new();
        let mut next = spawn_category;
        while let Some(category) = next.filter(|category| !found.contains(category)) {
            found.push(category);
            next = self.groups.get(&category).copied();
        }
        found
    }

    /// The kill types of `rows`: (vitals id, `VitalsCategories` cell).
    pub fn build(rows: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            by_vitals: rows
                .into_iter()
                .map(|(vitals_id, cell)| {
                    let categories = categories_of(&cell);
                    (lowercase_crc32(&vitals_id), (vitals_id, categories))
                })
                .collect(),
            groups: HashMap::new(),
        }
    }

    /// The vitals id and categories of vitals id `vitals_id`.
    pub fn of(&self, vitals_id: u32) -> Option<(&str, &[String])> {
        self.by_vitals
            .get(&vitals_id)
            .map(|(id, categories)| (id.as_str(), categories.as_slice()))
    }
}

impl ObjectiveRun {
    /// A run of `tree`, started at `now_ms`; the effects of its first tasks.
    pub fn start(tree: Arc<ObjectiveTree>, now_ms: u64) -> (Self, Vec<Effect>) {
        let states = vec![TaskState::Idle; tree.tasks.len()];
        let mut run = Self {
            tree,
            states,
            listed: Vec::new(),
            places: None,
            course_sections: Vec::new(),
        };
        let mut effects = Vec::new();
        if !run.states.is_empty() {
            run.activate(0, now_ms, &mut effects);
        }
        (run, effects)
    }

    /// The run as a handoff carries it at `now_ms`.
    pub fn export(&self, now_ms: u64) -> QuestRun {
        QuestRun {
            tasks: self
                .states
                .iter()
                .map(|state| match *state {
                    TaskState::Idle => QuestTask::Idle,
                    TaskState::Active { progress, due_ms } => QuestTask::Active {
                        progress,
                        due_in_ms: due_ms.map(|due| due.saturating_sub(now_ms)),
                    },
                    TaskState::Done => QuestTask::Done,
                })
                .collect(),
            listed: self
                .listed
                .iter()
                .filter_map(|index| u32::try_from(*index).ok())
                .collect(),
            mission: None,
        }
    }

    /// The run of `tree` that `run` carried, going on at `now_ms`; `None` when `run` does not
    /// fit `tree` (another tree or a task out of it).
    pub fn resume(tree: Arc<ObjectiveTree>, run: &QuestRun, now_ms: u64) -> Option<Self> {
        if run.tasks.len() != tree.tasks.len() {
            return None;
        }
        let listed = run
            .listed
            .iter()
            .map(|index| {
                usize::try_from(*index)
                    .ok()
                    .filter(|index| *index < tree.tasks.len())
            })
            .collect::<Option<Vec<usize>>>()?;
        let states = run
            .tasks
            .iter()
            .map(|task| match *task {
                QuestTask::Idle => TaskState::Idle,
                QuestTask::Active {
                    progress,
                    due_in_ms,
                } => TaskState::Active {
                    progress,
                    due_ms: due_in_ms.map(|left| now_ms.saturating_add(left)),
                },
                QuestTask::Done => TaskState::Done,
            })
            .collect();
        // A save keeps no ride's sections: a done ride shows 1 after a load.
        Some(Self {
            tree,
            states,
            listed,
            places: None,
            course_sections: Vec::new(),
        })
    }

    /// Whether the whole tree is done.
    pub fn finished(&self) -> bool {
        self.states.first() == Some(&TaskState::Done)
    }

    /// The achievements its started temporary unlock tasks set, which its end clears.
    pub fn temporary_unlocks(&self) -> Vec<String> {
        self.tree
            .tasks
            .iter()
            .zip(&self.states)
            .filter_map(|(task, state)| match &task.kind {
                TaskKind::TemporaryUnlock(achievement) if *state == TaskState::Done => {
                    Some(achievement.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Whether task `id` is done.
    pub fn is_done(&self, id: &str) -> bool {
        self.tree
            .tasks
            .iter()
            .zip(&self.states)
            .any(|(task, state)| *state == TaskState::Done && task.id.eq_ignore_ascii_case(id))
    }

    /// Whether a waiting task counts game event `event`.
    pub fn counts_game_event(&self, event: &str) -> bool {
        self.tree
            .tasks
            .iter()
            .zip(&self.states)
            .any(|(task, state)| {
                matches!(state, TaskState::Active { .. })
                    && matches!(&task.kind, TaskKind::GameEvent { event: wanted, .. }
                        if wanted.eq_ignore_ascii_case(event))
            })
    }

    /// Whether task `id` waits.
    pub fn is_active(&self, id: &str) -> bool {
        self.tree
            .tasks
            .iter()
            .zip(&self.states)
            .any(|(task, state)| {
                matches!(state, TaskState::Active { .. }) && task.id.eq_ignore_ascii_case(id)
            })
    }

    /// The tasks the quest tracker shows, in the order they first had progress.
    pub fn progress(&self) -> Vec<TaskProgress> {
        self.listed
            .iter()
            .map(|index| {
                let task = &self.tree.tasks[*index];
                // A listed task that is idle had progress, then its partial container stopped it.
                let status = match self.states[*index] {
                    TaskState::Done if task.stay_active => TaskStatus::DoneActive,
                    TaskState::Done => TaskStatus::Done,
                    TaskState::Active { .. } => TaskStatus::Running,
                    TaskState::Idle => TaskStatus::Stopped,
                };
                let progress = match (&task.kind, self.states[*index]) {
                    (TaskKind::Consecutive | TaskKind::Partial | TaskKind::Simple, _) => {
                        task.children
                            .iter()
                            .filter(|child| self.states[**child] == TaskState::Done)
                            .count() as u32
                    }
                    (
                        TaskKind::KillContribution { count, .. }
                        | TaskKind::HaveItems { count, .. }
                        | TaskKind::GatherItems { count, .. }
                        | TaskKind::GameEvent { count, .. }
                        | TaskKind::Interact { count, .. }
                        | TaskKind::RequireGather { count, .. }
                        | TaskKind::CyclicState { count, .. }
                        | TaskKind::CraftAnyRecipe { count, .. }
                        | TaskKind::UseItems { count, .. }
                        | TaskKind::Die { count },
                        TaskState::Done,
                    ) => *count,
                    (TaskKind::QuickCourse(_), TaskState::Done) => self
                        .course_sections
                        .iter()
                        .find(|(task, _)| task == index)
                        .map_or(1, |(_, sections)| *sections),
                    (_, TaskState::Active { progress, .. }) => progress,
                    _ => 1,
                };
                TaskProgress {
                    index: self.tree.wire_index[*index],
                    task: lowercase_crc32(&task.id),
                    progress,
                    status,
                }
            })
            .collect()
    }

    /// Completes the waiting item tasks that the player's items `held` (item id, quantity,
    /// worn) already meet, as [`ObjectiveRun::sync`] with nothing else known.
    pub fn sync_held(&mut self, held: &[(&str, u32, bool)], now_ms: u64) -> Vec<Effect> {
        self.sync(
            &Facts {
                held,
                places: None,
                achieved: None,
                level: None,
                territories: None,
                of_family: None,
                houses: None,
            },
            now_ms,
        )
    }

    /// Completes the waiting tasks that what the player is (`facts`) already meets: a have-item
    /// task by any stack of its item, a have-items task by its count of the item, an equip task
    /// by its item worn, a go-to task while the player is in its point of interest, an exit task
    /// while it is out of it, an achievement task by its achievement; a have-items task short of
    /// its count shows how many the player holds. Events count only for tasks that wait when
    /// they come; an item the player took before its task started would else never count
    /// (beach-ruins-chest note: the chest's potions taken before `ChestDiscovery`), nor would a
    /// lore note it read before.
    pub fn sync(&mut self, facts: &Facts<'_>, now_ms: u64) -> Vec<Effect> {
        if let Some(places) = facts.places {
            self.places = Some(places.iter().map(|place| place.to_lowercase()).collect());
        }
        self.sync_patrols(now_ms);
        let mut effects = Vec::new();
        for index in 0..self.states.len() {
            let TaskState::Active { progress, due_ms } = self.states[index] else {
                continue;
            };
            let holds = |item: &str| -> u32 {
                facts
                    .held
                    .iter()
                    .filter(|(id, _, _)| id.eq_ignore_ascii_case(item))
                    .map(|(_, quantity, _)| *quantity)
                    .sum()
            };
            let inside = |tag: &str| {
                facts
                    .places
                    .map(|places| places.iter().any(|place| place.eq_ignore_ascii_case(tag)))
            };
            let tree = Arc::clone(&self.tree);
            let met = match &tree.tasks[index].kind {
                TaskKind::HaveItem(Some(item)) => holds(item) > 0,
                TaskKind::HaveItems { item, count, .. } => {
                    let holding = holds(item);
                    if holding < *count && holding != progress {
                        self.states[index] = TaskState::Active {
                            progress: holding,
                            due_ms,
                        };
                        self.list(index);
                    }
                    holding >= *count
                }
                TaskKind::EquipItem(item) => facts
                    .held
                    .iter()
                    .any(|(id, _, worn)| *worn && id.eq_ignore_ascii_case(item)),
                TaskKind::GoToPoi(tags) => tags.iter().any(|tag| inside(tag) == Some(true)),
                TaskKind::GoToTerritory { territory, poi } => {
                    facts
                        .territories
                        .is_some_and(|territories| territories.contains(territory))
                        && poi.as_deref().is_none_or(|tag| inside(tag) == Some(true))
                }
                TaskKind::TriggerArea {
                    place: Some(tag),
                    leave,
                    ..
                } => inside(tag) == Some(!*leave),
                TaskKind::ExitPoi(tag) => inside(tag) == Some(false),
                TaskKind::HasAchievement(expression) => facts
                    .achieved
                    .is_some_and(|achieved| requirement_met(expression, achieved)),
                TaskKind::OwnHouses(count) => facts.houses.is_some_and(|houses| houses >= *count),
                TaskKind::HaveLevel {
                    progression,
                    target,
                    points,
                } => facts
                    .level
                    .and_then(|level| level(progression.as_deref(), *points))
                    .is_some_and(|level| level >= *target),
                TaskKind::HaveFamily { family, count } => {
                    facts.of_family.is_some_and(|of_family| {
                        let holding: u32 = facts
                            .held
                            .iter()
                            .filter(|(id, _, _)| of_family(id, family))
                            .map(|(_, quantity, _)| *quantity)
                            .sum();
                        if holding < *count && holding != progress {
                            self.states[index] = TaskState::Active {
                                progress: holding,
                                due_ms,
                            };
                            self.list(index);
                        }
                        holding >= *count
                    })
                }
                _ => false,
            };
            if met {
                self.complete(index, now_ms, &mut effects);
            }
        }
        effects
    }

    /// The items the waiting gather tasks drop for a corpse of `vitals_id` and `categories`
    /// that the player skins: each item and its chance in percent.
    pub fn quest_drops(
        &self,
        vitals_id: &str,
        categories: &[String],
        spawn_categories: &[u32],
    ) -> Vec<(String, u32)> {
        self.drops(|drop| kills_count(&drop.enemy, vitals_id, categories, spawn_categories))
    }

    /// The items the waiting gather tasks drop for a quest object of gatherable `gatherable`
    /// that the player gathers: each item and its chance in percent.
    pub fn gather_drops(&self, gatherable: &str) -> Vec<(String, u32)> {
        self.drops(|drop| drop.enemy.eq_ignore_ascii_case(gatherable))
    }

    /// The items of the waiting gather and item tasks whose drop `drops`, with their chances.
    fn drops(&self, drops: impl Fn(&QuestDrop) -> bool) -> Vec<(String, u32)> {
        self.waiting()
            .filter_map(|kind| match kind {
                TaskKind::GatherItems {
                    item,
                    drop: Some(drop),
                    ..
                }
                | TaskKind::HaveItems {
                    item,
                    drop: Some(drop),
                    ..
                } if drops(drop) => Some((item.clone(), drop.percent)),
                _ => None,
            })
            .collect()
    }

    /// The quick courses the waiting course tasks ask the player to ride.
    pub fn courses(&self) -> Vec<&str> {
        self.waiting()
            .filter_map(|kind| match kind {
                TaskKind::QuickCourse(course) => Some(course.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The quest fish the waiting catch tasks make bite in water `water` (its tag) for a player
    /// in `territories`: each item and its chance in percent.
    pub fn fish_drops(&self, water: &str, territories: &[u32]) -> Vec<(String, u32)> {
        self.waiting()
            .filter_map(|kind| match kind {
                TaskKind::CatchFish {
                    rule:
                        FishCatchRule {
                            item: Some(item),
                            drop: Some(drop),
                            ..
                        },
                    ..
                } if drop
                    .water
                    .as_deref()
                    .is_none_or(|wanted| wanted.eq_ignore_ascii_case(water))
                    && drop
                        .territory
                        .is_none_or(|territory| territories.contains(&territory)) =>
                {
                    Some((item.clone(), drop.percent))
                }
                _ => None,
            })
            .collect()
    }

    /// The items the waiting item tasks drop for a container the player opens (a loot crate)
    /// where it is now: each item and its chance in percent.
    pub fn chest_drops(&self) -> Vec<(String, u32)> {
        self.waiting()
            .filter_map(|kind| match kind {
                TaskKind::HaveItems {
                    item,
                    chest: Some(chest),
                    ..
                } if self.is_in(chest.place.as_deref()) => Some((item.clone(), chest.percent)),
                _ => None,
            })
            .collect()
    }

    /// Starts the stay of each running patrol task whose place the player is in, and ends it
    /// for each one it left: [`ObjectiveRun::tick`] ends a stay that lasted.
    fn sync_patrols(&mut self, now_ms: u64) {
        let tree = Arc::clone(&self.tree);
        for (index, task) in tree.tasks.iter().enumerate() {
            let TaskKind::Patrol { place, duration_ms } = &task.kind else {
                continue;
            };
            let TaskState::Active { progress, due_ms } = self.states[index] else {
                continue;
            };
            let inside = self.is_in(Some(place));
            let due_ms = match (inside, due_ms) {
                (true, None) => Some(now_ms.saturating_add(*duration_ms)),
                (true, due) => due,
                (false, _) => None,
            };
            self.states[index] = TaskState::Active { progress, due_ms };
        }
    }

    /// The kinds of the tasks that run.
    fn waiting(&self) -> impl Iterator<Item = &TaskKind> {
        self.tree
            .tasks
            .iter()
            .zip(&self.states)
            .filter(|(_, state)| matches!(state, TaskState::Active { .. }))
            .map(|(task, _)| &task.kind)
    }

    /// Whether the player is in `place` (a `POITag`; `None`: anywhere). Without a known place
    /// a place task does not count.
    fn is_in(&self, place: Option<&str>) -> bool {
        place.is_none_or(|place| {
            self.places
                .as_ref()
                .is_some_and(|places| places.iter().any(|known| known.eq_ignore_ascii_case(place)))
        })
    }

    /// Lists task `index` in the tracker once it has progress.
    fn list(&mut self, index: usize) {
        if !self.listed.contains(&index) {
            self.listed.push(index);
        }
    }

    /// The effects of `event` at `now_ms`.
    pub fn event(&mut self, event: Event<'_>, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        // Only tasks waiting before the event take it: a task it starts does not.
        let waiting: Vec<usize> = (0..self.states.len())
            .filter(|index| matches!(self.states[*index], TaskState::Active { .. }))
            .collect();
        for index in waiting {
            // A task may have been closed by an earlier one in this loop.
            let TaskState::Active { progress, due_ms } = self.states[index] else {
                continue;
            };
            // A ride's task counts each section passed, the finish ends it (Observed: retail
            // Hermit's Rally, `EF_Rally_0` progress 1 at the start gate to 7, then 8 and
            // Complete at the finish, each within 5 ms of the course state). A new ride counts
            // from its own start again (Inferred: no failed retail ride was captured).
            if let (
                TaskKind::QuickCourse(course),
                Event::QuickCourseSection {
                    course: seen,
                    section,
                    sections,
                },
            ) = (&self.tree.tasks[index].kind, event)
            {
                if course.eq_ignore_ascii_case(seen) {
                    let passed = section.saturating_add(1);
                    if passed >= sections {
                        self.course_sections.push((index, passed));
                        self.complete(index, now_ms, &mut effects);
                    } else {
                        self.states[index] = TaskState::Active {
                            progress: passed,
                            due_ms,
                        };
                        self.list(index);
                    }
                }
                continue;
            }
            // Whether the event counts, how many it counts and how many the task needs.
            let (matches, step, count) = match (&self.tree.tasks[index].kind, event) {
                (TaskKind::CinematicStart(id), Event::CinematicStarted(seen))
                | (TaskKind::CinematicComplete(id), Event::CinematicEnded(seen))
                | (TaskKind::CutsceneComplete(id), Event::CutsceneEnded(seen))
                | (TaskKind::OpenUiScreen(id), Event::UiScreenOpened(seen))
                | (TaskKind::ConversationTopic(id), Event::TopicFinished(seen)) => {
                    (*id == seen, 1, 1)
                }
                (
                    TaskKind::KillContribution {
                        enemy,
                        count,
                        place,
                    },
                    Event::Killed {
                        vitals_id,
                        categories,
                        spawn_categories,
                    },
                ) => (
                    kills_count(enemy, vitals_id, categories, spawn_categories)
                        && self.is_in(place.as_deref()),
                    1,
                    *count,
                ),
                (
                    TaskKind::TriggerArea {
                        area: Some(area),
                        leave,
                        ..
                    },
                    Event::Moved(position),
                ) => (area.contains(position) != *leave, 1, 1),
                (TaskKind::HaveItem(Some(wanted)), Event::ItemGained { item, .. })
                | (TaskKind::EquipItem(wanted), Event::Equipped(item))
                | (TaskKind::CraftRecipe(wanted), Event::Crafted(item)) => {
                    (wanted.eq_ignore_ascii_case(item), 1, 1)
                }
                (
                    TaskKind::GatherItems {
                        item: wanted,
                        count,
                        ..
                    },
                    Event::ItemGained { item, quantity },
                ) => (wanted.eq_ignore_ascii_case(item), quantity, *count),
                (TaskKind::HaveItem(None), Event::ContainerLooted { .. }) => (true, 1, 1),
                (TaskKind::Die { count }, Event::Died) => (true, 1, *count),
                (TaskKind::HasAchievement(wanted), Event::AchievementSet(set)) => {
                    (wanted.eq_ignore_ascii_case(set), 1, 1)
                }
                (
                    TaskKind::GameEvent {
                        event: wanted,
                        count,
                    },
                    Event::GameEvent(seen),
                ) => (wanted.eq_ignore_ascii_case(seen), 1, *count),
                (TaskKind::Interact { tag, count }, Event::Interacted(seen)) => {
                    (tag.eq_ignore_ascii_case(seen), 1, *count)
                }
                (
                    TaskKind::RequireGather {
                        item: wanted,
                        count,
                    },
                    Event::Gathered { item, quantity },
                ) => (wanted.eq_ignore_ascii_case(item), quantity, *count),
                (
                    TaskKind::CyclicState {
                        cyclic: wanted,
                        state: wanted_state,
                        count,
                    },
                    Event::CyclicGathered { cyclic, state },
                ) => (
                    wanted.eq_ignore_ascii_case(cyclic) && *wanted_state == state,
                    1,
                    *count,
                ),
                (
                    TaskKind::UseItems {
                        item: wanted,
                        count,
                    },
                    Event::ItemUsed(item),
                ) => (wanted.eq_ignore_ascii_case(item), 1, *count),
                (TaskKind::CraftAnyRecipe { recipes, count }, Event::Crafted(recipe)) => (
                    recipes
                        .iter()
                        .any(|wanted| wanted.eq_ignore_ascii_case(recipe)),
                    1,
                    *count,
                ),
                (TaskKind::Fishing { step, count }, Event::Fishing(seen)) => {
                    (*step == seen, 1, *count)
                }
                (TaskKind::CatchFish { rule, count }, Event::FishCaught(caught)) => {
                    (rule.takes(&caught), 1, *count)
                }
                (
                    TaskKind::Salvage {
                        item: wanted,
                        count,
                    },
                    Event::Salvaged { item, quantity },
                ) => (wanted.eq_ignore_ascii_case(item), quantity, *count),
                (TaskKind::CraftFamily { family, count }, Event::CraftedFamilies(families)) => (
                    families
                        .iter()
                        .any(|seen| seen.eq_ignore_ascii_case(family)),
                    1,
                    *count,
                ),
                (
                    TaskKind::PerformSong { song, near, count },
                    Event::SongPerformed { song: seen, at },
                ) => (
                    song.eq_ignore_ascii_case(seen)
                        && near.as_ref().is_none_or(|area| area.contains(at)),
                    1,
                    *count,
                ),
                (TaskKind::GameMode(mode), Event::GameModeEntered(seen))
                | (TaskKind::ConversationState(mode), Event::Talked(seen))
                | (TaskKind::Objective(mode), Event::ObjectiveDone(seen))
                | (TaskKind::CompleteGameMode(mode), Event::GameModeCompleted(seen)) => {
                    (mode.eq_ignore_ascii_case(seen), 1, 1)
                }
                (TaskKind::ChooseFaction, Event::FactionChosen(_)) => (true, 1, 1),
                (
                    TaskKind::QuickCourseSection { course, section },
                    Event::QuickCourseSection {
                        course: seen,
                        section: passed,
                        ..
                    },
                ) => (
                    course.eq_ignore_ascii_case(seen) && passed >= *section,
                    1,
                    1,
                ),
                _ => (false, 1, 1),
            };
            if !matches {
                continue;
            }
            let progress = progress.saturating_add(step);
            if progress >= count {
                self.complete(index, now_ms, &mut effects);
            } else {
                self.states[index] = TaskState::Active { progress, due_ms };
                self.list(index);
            }
        }
        if let Event::ContainerLooted { items } = event {
            self.complete_looted_weapon_group(items, now_ms, &mut effects);
        }
        effects
    }

    /// Completes a simple container of weapon takes when the loot includes one weapon of that
    /// container. Retail (newchar-retail-2026-09-18, objectives 3857 at 173.857 s): the swordsman
    /// bag holds `SwordsmanArchetype_2hGreatsword`, and `Weapon2_container` plus its three takes
    /// (`TakeFireStaff`, `TakeVoidGauntlet`, `TakeGreatSword`) all complete together. The other
    /// group stays open. The fire-staff and void-gauntlet takes are not items in that bag; the
    /// group completes as one when any of its drops matches a looted weapon.
    fn complete_looted_weapon_group(
        &mut self,
        items: &[&str],
        now_ms: u64,
        effects: &mut Vec<Effect>,
    ) {
        let families: Vec<String> = items
            .iter()
            .filter_map(|item| weapon_family(item))
            .collect();
        if families.is_empty() {
            return;
        }
        let tree = Arc::clone(&self.tree);
        let groups: Vec<usize> = tree
            .tasks
            .iter()
            .enumerate()
            .filter(|(index, task)| {
                matches!(self.states[*index], TaskState::Active { .. })
                    && matches!(task.kind, TaskKind::Simple)
                    && weapon_take_group(&tree, task)
                    && task.children.iter().any(|child| {
                        have_family(&tree, *child).is_some_and(|family| families.contains(&family))
                    })
            })
            .map(|(index, _)| index)
            .collect();
        for group in groups {
            let children = tree.tasks[group].children.clone();
            for child in children {
                if matches!(self.states[child], TaskState::Active { .. }) {
                    self.complete(child, now_ms, effects);
                }
            }
        }
    }

    /// The effects of the timers due at `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        for index in 0..self.states.len() {
            if let TaskState::Active {
                due_ms: Some(due), ..
            } = self.states[index]
                && now_ms >= due
            {
                self.complete(index, now_ms, &mut effects);
            }
        }
        effects
    }

    fn activate(&mut self, index: usize, now_ms: u64, effects: &mut Vec<Effect>) {
        let task = &self.tree.tasks[index];
        let due_ms = match task.kind {
            TaskKind::Timer { duration_ms } => Some(now_ms.saturating_add(duration_ms)),
            _ => None,
        };
        self.states[index] = TaskState::Active {
            progress: 0,
            due_ms,
        };
        let tree = Arc::clone(&self.tree);
        let task = &tree.tasks[index];
        match &task.kind {
            TaskKind::Consecutive => match task.children.first() {
                Some(first) => self.activate(*first, now_ms, effects),
                None => self.complete(index, now_ms, effects),
            },
            TaskKind::Partial | TaskKind::Simple => {
                if task.children.is_empty() {
                    self.complete(index, now_ms, effects);
                }
                for child in &task.children {
                    // A partial container may be done by an earlier child already.
                    if matches!(self.states[index], TaskState::Active { .. }) {
                        self.activate(*child, now_ms, effects);
                    }
                }
            }
            TaskKind::UnlockAchievement(achievement) | TaskKind::TemporaryUnlock(achievement) => {
                effects.push(Effect::Achievement(achievement.clone()));
                self.complete(index, now_ms, effects);
            }
            TaskKind::RemoveItem { item, count } => {
                effects.push(Effect::RemoveItem(item.clone(), *count));
                self.complete(index, now_ms, effects);
            }
            TaskKind::LockAchievement(achievement) => {
                effects.push(Effect::LockAchievement(achievement.clone()));
                self.complete(index, now_ms, effects);
            }
            TaskKind::TriggerGameEvent(event) => {
                effects.push(Effect::GameEvent(event.clone()));
                self.complete(index, now_ms, effects);
            }
            TaskKind::Objective(objective) => {
                effects.push(Effect::StartObjective(objective.clone()));
            }
            TaskKind::GiveItem { item, count, .. } => {
                effects.push(Effect::GiveItem(item.clone(), *count));
                self.complete(index, now_ms, effects);
            }
            TaskKind::Empty => self.complete(index, now_ms, effects),
            TaskKind::NotRun(kind) => {
                tracing::info!(task = index, kind = %kind, "a quest waits on a task type the server does not run");
            }
            _ => {}
        }
    }

    fn complete(&mut self, index: usize, now_ms: u64, effects: &mut Vec<Effect>) {
        self.states[index] = TaskState::Done;
        self.list(index);
        let tree = Arc::clone(&self.tree);
        let task = &tree.tasks[index];
        effects.push(Effect::Done(task.id.clone()));
        let Some(parent) = task.parent else {
            return;
        };
        self.list(parent);
        if !matches!(self.states[parent], TaskState::Active { .. }) {
            return;
        }
        let siblings = &tree.tasks[parent].children;
        match tree.tasks[parent].kind {
            TaskKind::Consecutive => {
                let next = siblings
                    .iter()
                    .skip_while(|sibling| **sibling != index)
                    .nth(1);
                match next {
                    Some(next) => self.activate(*next, now_ms, effects),
                    None => self.complete(parent, now_ms, effects),
                }
            }
            TaskKind::Partial => {
                for sibling in siblings {
                    self.stop(*sibling);
                }
                self.complete(parent, now_ms, effects);
            }
            TaskKind::Simple
                if siblings
                    .iter()
                    .all(|sibling| self.states[*sibling] == TaskState::Done) =>
            {
                self.complete(parent, now_ms, effects);
            }
            _ => {}
        }
    }

    /// Stops task `index` and its children unless done.
    fn stop(&mut self, index: usize) {
        if self.states[index] == TaskState::Done {
            return;
        }
        self.states[index] = TaskState::Idle;
        let tree = Arc::clone(&self.tree);
        for child in &tree.tasks[index].children {
            self.stop(*child);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const NOW: u64 = 1000;
    const TIMER_S: f64 = 2.0;
    const TIMER_MS: u64 = 2000;
    const AREA: Position = Position {
        x: 755.87,
        y: 342.49,
        z: 20.9,
    };

    fn row(task_id: &str, kind: &str) -> TaskRow {
        TaskRow {
            task_id: task_id.to_owned(),
            kind: kind.to_owned(),
            ..TaskRow::default()
        }
    }

    fn with_item(task_id: &str, kind: &str, item: &str) -> TaskRow {
        TaskRow {
            item_name: Some(item.to_owned()),
            ..row(task_id, kind)
        }
    }

    fn with_condition(task_id: &str, kind: &str, condition: &str) -> TaskRow {
        TaskRow {
            fail_on_condition: Some(condition.to_owned()),
            ..row(task_id, kind)
        }
    }

    fn container(task_id: &str, kind: &str, children: &[&str]) -> TaskRow {
        TaskRow {
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..row(task_id, kind)
        }
    }

    #[test]
    fn a_game_mode_task_counts_its_entry_and_a_give_task_gives_its_item() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Give", "Enter"]),
            TaskRow {
                target_qty: Some(2),
                ..with_item("Give", "TaskGiveItem", "WorkOrder_T1")
            },
            TaskRow {
                game_mode_id: Some("DungeonAmrine".to_owned()),
                ..row("Enter", "TaskGameMode")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, effects) = ObjectiveRun::start(tree, 0);
        assert!(effects.contains(&Effect::GiveItem("WorkOrder_T1".to_owned(), 2)));
        assert!(run.is_done("Give"), "a give task is done once it gave");
        run.event(Event::GameModeEntered("DungeonEbonscale00"), 0);
        assert!(!run.is_done("Enter"), "another game mode");
        run.event(Event::GameModeEntered("dungeonamrine"), 0);
        assert!(run.finished(), "entering its game mode ends it");
    }

    #[test]
    fn an_achievement_task_reads_its_expression() {
        /// The retail form of `95_s01_05_CROW_CanonAndCoin` (build 25810808): either of two.
        const EITHER: &str = "95_CROW_Cannon || 95_CROW_Coin";
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Either"]),
            with_item("Either", "TaskAchievement", EITHER),
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let synced = |achieved: &dyn Fn(&str) -> bool| {
            let (mut run, _) = ObjectiveRun::start(Arc::clone(&tree), 0);
            run.sync(
                &Facts {
                    held: &[],
                    places: None,
                    achieved: Some(achieved),
                    level: None,
                    territories: None,
                    of_family: None,
                    houses: None,
                },
                0,
            );
            run.is_done("Either")
        };
        assert!(!synced(&|_| false), "neither");
        assert!(synced(&|id| id == "95_CROW_Coin"), "the second one");
    }

    #[test]
    fn a_poi_task_with_a_tag_list_is_done_in_any_of_them() {
        /// Retail `95A_S02_VoicesoftheDesert` (build 25810808), the first two of its six.
        const NESTS: &str = "CrowsNest_50426,CrowsNest_30334";
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Nest"]),
            TaskRow {
                poi_tag: Some(NESTS.to_owned()),
                ..container("Nest", "TaskGoToPOITag", &[])
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let synced = |places: &[&str]| {
            let (mut run, _) = ObjectiveRun::start(Arc::clone(&tree), 0);
            run.sync(
                &Facts {
                    held: &[],
                    places: Some(places),
                    achieved: None,
                    level: None,
                    territories: None,
                    of_family: None,
                    houses: None,
                },
                0,
            );
            run.is_done("Nest")
        };
        assert!(!synced(&["CrowsNest_99999"]), "another nest");
        assert!(synced(&["crowsnest_30334"]), "the second of the list");
    }

    #[test]
    fn a_ride_task_counts_each_section_and_ends_at_the_finish() {
        /// Retail Hermit's Rally: `EF_Tour_0`, 8 sections (start, 6 checkpoints, finish).
        const COURSE: &str = "EF_Tour_0";
        const SECTIONS: u32 = 8;
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Ride"]),
            TaskRow {
                target_qty: Some(1),
                ..with_item("Ride", "TaskQuickCourse", COURSE)
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let ride = |run: &ObjectiveRun| {
            run.progress()
                .into_iter()
                .find(|task| task.task == lowercase_crc32("Ride"))
                .map(|task| (task.progress, task.status))
        };
        let pass = |run: &mut ObjectiveRun, course: &str, section| {
            run.event(
                Event::QuickCourseSection {
                    course,
                    section,
                    sections: SECTIONS,
                },
                0,
            );
        };
        pass(&mut run, "ef_tour_0", 0);
        assert_eq!(ride(&run), Some((1, TaskStatus::Running)), "the start gate");
        pass(&mut run, COURSE, 1);
        assert_eq!(ride(&run), Some((2, TaskStatus::Running)));
        pass(&mut run, "EF_Tour_1", 2);
        assert_eq!(ride(&run), Some((2, TaskStatus::Running)), "another course");
        pass(&mut run, COURSE, 0);
        assert_eq!(ride(&run), Some((1, TaskStatus::Running)), "a new ride");
        pass(&mut run, COURSE, SECTIONS - 1);
        assert_eq!(
            ride(&run),
            Some((SECTIONS, TaskStatus::Done)),
            "retail: 8, Complete"
        );
        assert!(run.finished());
    }

    #[test]
    fn a_carried_item_is_given_or_dropped_and_goes_back_at_the_end() {
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["Letter", "Relic"]),
            with_item("Letter", "TaskGiveAndTakeItem", "1612_0201_MalatestaLetter"),
            TaskRow {
                item_drop_vc: Some("Bandit".to_owned()),
                item_drop_probability: Some(100),
                ..with_item("Relic", "TaskGiveAndTakeItem", "1605_0301_BlessedRelic")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (run, effects) = ObjectiveRun::start(Arc::clone(&tree), 0);
        assert!(effects.contains(&Effect::GiveItem("1612_0201_MalatestaLetter".to_owned(), 1)));
        assert_eq!(
            run.quest_drops("Bandit_Thug", &[], &[]),
            [("1605_0301_BlessedRelic".to_owned(), 100)]
        );
        assert_eq!(
            tree.returned_items(),
            [
                ("1612_0201_MalatestaLetter".to_owned(), 1),
                ("1605_0301_BlessedRelic".to_owned(), 1)
            ]
        );
    }

    #[test]
    fn a_patrol_task_needs_a_whole_stay_in_its_place() {
        const STAY_MS: u64 = 90_000;
        let rows = vec![TaskRow {
            poi_tag: Some("poi_30037_factionarea".to_owned()),
            duration: Some(90.0),
            ..row("Patrol", "PatrolTask")
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Patrol").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let inside = ["poi_30037_factionarea"];
        let outside: [&str; 0] = [];
        let facts = |places| Facts {
            held: &[],
            places: Some(places),
            achieved: None,
            level: None,
            territories: None,
            of_family: None,
            houses: None,
        };
        run.sync(&facts(&inside), 0);
        run.sync(&facts(&outside), STAY_MS / 2);
        run.tick(STAY_MS);
        assert!(!run.finished(), "a leave starts the stay again");
        run.sync(&facts(&inside), STAY_MS);
        run.tick(2 * STAY_MS - 1);
        assert!(!run.finished());
        run.tick(2 * STAY_MS);
        assert!(run.finished(), "a whole stay ends it");
    }

    #[test]
    fn an_area_of_a_level_volume_counts_inside_it_and_a_leave_outside_it() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Enter", "Leave"]),
            TaskRow {
                target_tag: Some("99F_StealBoat_GoToCauseway".to_owned()),
                ..row("Enter", "TaskTriggerArea")
            },
            TaskRow {
                target_tag: Some("12A_LeaveLochnir".to_owned()),
                trigger_area_type: Some(LEAVE_AREA_TYPE.to_owned()),
                ..row("Leave", "TaskTriggerArea")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let facts = |places| Facts {
            held: &[],
            places: Some(places),
            achieved: None,
            level: None,
            territories: None,
            of_family: None,
            houses: None,
        };
        run.sync(&facts(&["12a_leavelochnir"]), 0);
        assert!(!run.is_done("Enter"), "not in the causeway volume");
        run.sync(
            &facts(&["99F_StealBoat_GoToCauseway", "12A_LeaveLochnir"]),
            0,
        );
        assert!(run.is_done("Enter"), "in its volume");
        assert!(!run.is_done("Leave"), "still in Lochnir");
        run.sync(&facts(&[]), 0);
        assert!(run.finished(), "out of Lochnir");
    }

    #[test]
    fn a_have_level_task_waits_for_the_character_or_progression_level() {
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["Level", "Fishing"]),
            TaskRow {
                target_qty: Some(25),
                ..row("Level", "TaskHaveLevel")
            },
            TaskRow {
                target_qty: Some(100),
                ..with_item("Fishing", "TaskHaveLevel", "Fishing")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let levels = |character: u32, fishing: u32| {
            move |progression: Option<&str>, _points: bool| match progression {
                None => Some(character),
                Some(_) => Some(fishing),
            }
        };
        let low = levels(24, 100);
        let facts = Facts {
            held: &[],
            places: None,
            achieved: None,
            level: Some(&low),
            territories: None,
            of_family: None,
            houses: None,
        };
        run.sync(&facts, 0);
        assert!(!run.is_done("Level") && run.is_done("Fishing"));
        let high = levels(25, 100);
        let facts = Facts {
            level: Some(&high),
            territories: None,
            of_family: None,
            houses: None,
            ..facts
        };
        run.sync(&facts, 0);
        assert!(run.finished(), "level 25 is reached");
    }

    #[test]
    fn a_conversation_state_task_is_done_by_a_talk_with_its_npc() {
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["Iseul", "Npc"]),
            TaskRow {
                ui_screen_tag: Some("12_01_JeongIseul".to_owned()),
                ..with_item("Iseul", "TaskConversationState", "12_JeongIseul")
            },
            TaskRow {
                ui_screen_tag: Some("NPC_16195_State_3".to_owned()),
                ..row("Npc", "TASKCONVERSATIONSTATE")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(Event::Talked("12_Grace"), 0);
        assert!(!run.is_done("Iseul"));
        run.event(Event::Talked("12_JeongIseul"), 0);
        assert!(run.is_done("Iseul"));
        run.event(Event::Talked("NPC_16195"), 0);
        assert!(run.finished());
    }

    #[test]
    fn a_require_gather_task_counts_gathers_only() {
        let rows = vec![TaskRow {
            target_qty: Some(2),
            ..with_item("Herbs", "TaskRequireGatherItems", "HerbT1")
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Herbs").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(
            Event::ItemGained {
                item: "HerbT1",
                quantity: 5,
            },
            0,
        );
        assert!(!run.finished(), "a loot or a trade is no gather");
        run.event(
            Event::Gathered {
                item: "HerbT1",
                quantity: 1,
            },
            0,
        );
        assert!(!run.finished());
        run.event(
            Event::Gathered {
                item: "herbt1",
                quantity: 1,
            },
            0,
        );
        assert!(run.finished(), "two gathered");
    }

    #[test]
    fn a_temporary_unlock_sets_its_achievement_at_once_and_names_it_for_the_end() {
        // Client rows of `1225_0101a` (Truffle Fever's down payment): a slice swap, then a talk.
        const SWAP: &str = "SwapActive_1225_CampfirePropsMB_On";
        let rows = vec![
            container(
                "Root",
                "ConsecutiveTaskContainer",
                &["Swap", "Later", "Talk"],
            ),
            with_item("Swap", "TaskTemporaryUnlockAchievement", SWAP),
            with_item(
                "Later",
                "TaskTemporaryUnlockAchievement",
                "SwapActive_Later",
            ),
            with_item("Talk", "TaskConversationState", "1225_Npc"),
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (run, effects) = ObjectiveRun::start(tree, 0);
        assert_eq!(
            achievements(&effects),
            [SWAP, "SwapActive_Later"],
            "set when added, done at once"
        );
        assert!(!run.finished(), "the talk waits");
        assert_eq!(run.temporary_unlocks(), [SWAP, "SwapActive_Later"]);
    }

    #[test]
    fn a_territory_task_waits_for_its_territory_and_its_point_of_interest() {
        // Client row `Quest_03190_Task_8`: Windsward's settlement on territory 12.
        const SETTLEMENT: &str = "Windsward_Settlement_01";
        let rows = vec![TaskRow {
            territory_id: Some(12),
            poi_tag: Some(SETTLEMENT.to_owned()),
            ..row("Go", "TASKGOTOPOITAGONTERRITORY")
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Go").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let mut sync = |territories: &[u32], places: &[&str]| {
            run.sync(
                &Facts {
                    held: &[],
                    places: Some(places),
                    achieved: None,
                    level: None,
                    territories: Some(territories),
                    of_family: None,
                    houses: None,
                },
                0,
            );
            run.finished()
        };
        assert!(!sync(&[6], &[SETTLEMENT]), "another territory");
        assert!(!sync(&[12], &[]), "not at the settlement");
        assert!(sync(&[12], &[SETTLEMENT]));
    }

    #[test]
    fn a_use_items_task_counts_uses_of_its_item() {
        // Client row `09A_S0_UsePotion`.
        let rows = vec![TaskRow {
            target_qty: Some(2),
            ..with_item("Use", "TaskUseItems", "09A_S0_SporeElixir")
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Use").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(Event::ItemUsed("09A_S0_SporeElixir"), 0);
        run.event(Event::ItemUsed("FoodT1"), 0);
        assert!(!run.finished(), "food is another item");
        run.event(Event::ItemUsed("09a_s0_sporeelixir"), 0);
        assert!(run.finished(), "two uses");
    }

    #[test]
    fn fishing_step_tasks_count_their_own_step() {
        // Client rows `C91_FQ_02_01` to `C91_FQ_02_04`: equip once, then 3 casts, hooks and reels.
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Equip", "Cast"]),
            row("Equip", "TaskFishingEquip"),
            TaskRow {
                target_qty: Some(3),
                ..row("Cast", "TaskFishingCast")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(Event::Fishing(FishingStep::Cast), 0);
        assert!(run.is_active("Equip"), "a cast before the pole is out");
        run.event(Event::Fishing(FishingStep::Equip), 0);
        assert!(run.is_done("Equip"));
        for _ in 0..2 {
            run.event(Event::Fishing(FishingStep::Cast), 0);
        }
        run.event(Event::Fishing(FishingStep::Hook), 0);
        assert!(!run.finished(), "two casts and a hook");
        run.event(Event::Fishing(FishingStep::Cast), 0);
        assert!(run.finished(), "three casts");
    }

    #[test]
    fn catch_tasks_count_the_fish_their_rule_takes() {
        // Client rows `C91_FQ_06_02` (8 fish of length 20 or more), `C91_FQ_08_02` (5 of weight
        // 30 or more), `C91_FQ_04_02` (3 at hotspots) and `C91_FQ_05_01` (15 of any).
        let rows = vec![
            container(
                "Root",
                "SimpleTaskContainer",
                &["Long", "Heavy", "Spot", "Any"],
            ),
            TaskRow {
                target_threshold: Some(20.0),
                ..row("Long", "TaskFishingCatchLength")
            },
            TaskRow {
                target_threshold: Some(30.0),
                ..row("Heavy", "TaskFishingCatchWeight")
            },
            row("Spot", "TaskFishingHotspotAny"),
            TaskRow {
                target_qty: Some(2),
                ..row("Any", "TaskFishingCatchAnyFish")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let fish = |length, weight, hotspot| {
            Event::FishCaught(FishCaught {
                item: "FishingSalmonMediumT1",
                length,
                weight,
                hotspot,
            })
        };
        run.event(fish(19.5, 29.0, false), 0);
        assert!(run.is_active("Long") && run.is_active("Heavy") && run.is_active("Spot"));
        assert!(run.is_active("Any"), "one of two");
        run.event(fish(20.0, 31.0, true), 0);
        assert!(
            run.finished(),
            "long, heavy, at a hotspot and the second fish"
        );
    }

    #[test]
    fn a_quest_fish_bites_in_its_water_and_territory_while_its_task_waits() {
        // Client row `1323_0101_d_01_TaskFishingCatchFish`: 3 slimy boots at 50% in territory 13;
        // `C91_FQ_03_01`: a snail at 25% in fresh water.
        const BOOT: &str = "1323_0101_SlimyBoot";
        const SNAIL: &str = "FishingSnailSmallT2";
        let rows = vec![
            container(
                "Root",
                "SimpleTaskContainer",
                &["Boot", "Snail", "Sturgeon"],
            ),
            TaskRow {
                item_drop_probability: Some(50),
                territory_id: Some(13),
                target_qty: Some(3),
                ..with_item("Boot", "TaskFishingCatchFish", BOOT)
            },
            TaskRow {
                item_drop_probability: Some(25),
                loot_tag: Some("Fresh".to_owned()),
                ..with_item("Snail", "TaskFishingCatchFish", SNAIL)
            },
            with_item(
                "Sturgeon",
                "TaskFishingCatchFish",
                "FishingSummerMedleySturgeonSmallT3",
            ),
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        assert_eq!(run.fish_drops("Salt", &[13]), [(BOOT.to_owned(), 50)]);
        assert_eq!(
            run.fish_drops("Fresh", &[12]),
            [(SNAIL.to_owned(), 25)],
            "a sturgeon without a chance is no quest fish"
        );
        run.event(
            Event::FishCaught(FishCaught {
                item: SNAIL,
                length: 1.0,
                weight: 1.0,
                hotspot: false,
            }),
            0,
        );
        assert!(run.is_done("Snail"));
        assert!(
            run.fish_drops("Fresh", &[12]).is_empty(),
            "its task is done"
        );
    }

    #[test]
    fn salvage_and_family_tasks_count_their_item_and_family() {
        // Client rows `C91_FQ_03_02_TaskSalvageSnail`, `06A_TS_02_ForgeSword_craftRecipe` and
        // `06A_TS_02_ForgeSword_have_1hSwordT2`.
        const SWORD: &str = "SwordAbilityTable";
        let rows = vec![
            container(
                "Root",
                "ConsecutiveTaskContainer",
                &["Salvage", "Forge", "Have"],
            ),
            with_item("Salvage", "TaskSalvageItem", "FishingSnailSmallT2"),
            TaskRow {
                crafting_recipe_id: Some(SWORD.to_owned()),
                ..with_item("Forge", "TaskCraftWeaponFamily", "1hSwordT2")
            },
            with_item("Have", "TaskHaveItemsWithWeaponFamily", SWORD),
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(
            Event::Salvaged {
                item: "fishingsnailsmallt2",
                quantity: 1,
            },
            0,
        );
        assert!(run.is_done("Salvage"));
        run.event(Event::CraftedFamilies(&["Weapon".to_owned()]), 0);
        assert!(run.is_active("Forge"), "not a sword");
        run.event(
            Event::CraftedFamilies(&["Weapon".to_owned(), SWORD.to_owned()]),
            0,
        );
        assert!(run.is_done("Forge"));
        let of_family = |item: &str, family: &str| item == "1hSword_T2" && family == SWORD;
        const HELD: [(&str, u32, bool); 1] = [("1hSword_T2", 1, false)];
        let facts = |held: &'static [(&'static str, u32, bool)]| Facts {
            held,
            places: None,
            achieved: None,
            level: None,
            territories: None,
            of_family: Some(&of_family),
            houses: None,
        };
        run.sync(&facts(&[("IronIngotT2", 4, false)]), 0);
        assert!(run.is_active("Have"), "no sword held");
        run.sync(&facts(&HELD), 0);
        assert!(run.finished());
    }

    #[test]
    fn a_house_task_waits_for_an_owned_house_and_a_faction_task_for_a_pick() {
        // Client rows `1225_0201_d_01_PurchaseHouse` and
        // `FactionIntro_Covenant_Recruitment_JoinFaction`.
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["House", "Join"]),
            TaskRow {
                target_qty: Some(1),
                ..row("House", "TaskHouseScore")
            },
            with_item("Join", "TaskChooseFaction", "9814_Beyza"),
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let facts = |houses| Facts {
            held: &[],
            places: None,
            achieved: None,
            level: None,
            territories: None,
            of_family: None,
            houses,
        };
        run.sync(&facts(None), 0);
        run.sync(&facts(Some(0)), 0);
        assert!(run.is_active("House"), "no house yet");
        run.sync(&facts(Some(1)), 0);
        assert!(!run.is_active("House"), "a house bought");
        assert!(run.is_active("Join"));
        run.event(Event::FactionChosen(3), 0);
        assert!(run.finished());
    }

    #[test]
    fn a_song_task_counts_its_song_near_its_place_and_a_dungeon_task_its_end() {
        // Client rows `13_Summer_09_PlayWithBand_playLocation1` and
        // `S04_GT_EXPD_RPT_04_KillIceBoss`.
        const STAGE: Position = Position {
            x: 9308.0,
            y: 2827.0,
            z: 76.0,
        };
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["Play", "Boss"]),
            TaskRow {
                override_location: Some("9308 2827 76".to_owned()),
                ..with_item("Play", "TaskPerformSong", "FlourishEasy")
            },
            TaskRow {
                game_mode_id: Some("DungeonGreatCleave00".to_owned()),
                ..row("Boss", "TaskCompleteDungeon")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let far = Position {
            x: STAGE.x + TRIGGER_RADIUS_M + 1.0,
            ..STAGE
        };
        run.event(
            Event::SongPerformed {
                song: "FlourishEasy",
                at: far,
            },
            0,
        );
        run.event(
            Event::SongPerformed {
                song: "WaltzEasy",
                at: STAGE,
            },
            0,
        );
        assert!(
            run.is_active("Play"),
            "away from the stage, or another song"
        );
        run.event(
            Event::SongPerformed {
                song: "flourisheasy",
                at: STAGE,
            },
            0,
        );
        assert!(run.is_done("Play"));
        run.event(Event::GameModeEntered("DungeonGreatCleave00"), 0);
        assert!(run.is_active("Boss"), "entered, not finished");
        run.event(Event::GameModeCompleted("DungeonGreatCleave00"), 0);
        assert!(run.finished());
    }

    #[test]
    fn a_go_to_with_only_a_location_and_a_give_with_only_an_interact_wait_for_them() {
        // Client rows `03_SQ_04_WulfstanGlyphs_GoToKazu` and
        // `12A_MSQ_24_DefeatGawain_CollectCrownItem`; a refine task is a craft of its recipe.
        const KAZU: Position = Position {
            x: 10193.93,
            y: 7940.685,
            z: 471.42,
        };
        let rows = vec![
            container("Root", "SimpleTaskContainer", &["Kazu", "Crown", "Kiln"]),
            TaskRow {
                override_location: Some("10193.929688 7940.685059 471.424255".to_owned()),
                ..row("Kazu", "TaskGoToPOITag")
            },
            TaskRow {
                interact_tag: Some("IO_MB_MSQ2_DefeatGawain_Crown".to_owned()),
                ..row("Crown", "TaskGiveItem")
            },
            TaskRow {
                crafting_recipe_id: Some("GypsumOrb".to_owned()),
                ..row("Kiln", "TaskRefineRecipe")
            },
        ];
        let tree = Arc::new(TaskBook::new(rows).tree("Root").expect("tree"));
        let (mut run, effects) = ObjectiveRun::start(tree, 0);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::GiveItem(..))),
            "the crown task gives nothing: {effects:?}"
        );
        assert!(run.is_active("Kazu") && run.is_active("Crown") && run.is_active("Kiln"));
        run.event(Event::Moved(KAZU), 0);
        run.event(Event::Interacted("io_mb_msq2_defeatgawain_crown"), 0);
        run.event(Event::Crafted("GypsumOrb"), 0);
        assert!(run.finished());
    }

    #[test]
    fn a_multiple_recipe_task_counts_crafts_of_any_of_its_recipes() {
        // Client row `Quest_03194_Task_18`: 5 crafts of the T3 boxes.
        let rows = vec![TaskRow {
            item_drop_vc: Some("WeaponsBoxT3,ArmorBoxT3,JewelryBoxT3".to_owned()),
            target_qty: Some(2),
            ..row("Boxes", "TASKCRAFTMULTIPLERECIPES")
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Boxes").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        run.event(Event::Crafted("WeaponsBoxT3"), 0);
        run.event(Event::Crafted("IngotT3"), 0);
        assert!(!run.finished(), "an ingot is not a box");
        run.event(Event::Crafted("armorboxt3"), 0);
        assert!(run.finished(), "two boxes");
    }

    #[test]
    fn a_cyclic_state_task_counts_a_gather_in_its_state_and_stays_active() {
        // Client row `99C_MSQ_03_AncientBridge_interact_puzzle1_stone2`: state 2, StayActive.
        const STONE: &str = "Cyclic_99C_MSQ_puzzle1_stone2";
        let rows = vec![TaskRow {
            item_drop_probability: Some(2),
            stay_active: true,
            ..with_item("Stone", "TaskGatherCyclicState", STONE)
        }];
        let tree = Arc::new(TaskBook::new(rows).tree("Stone").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        for state in [0, 1] {
            run.event(
                Event::CyclicGathered {
                    cyclic: STONE,
                    state,
                },
                0,
            );
        }
        assert!(!run.finished(), "states 0 and 1 are not the target");
        run.event(
            Event::CyclicGathered {
                cyclic: "cyclic_99c_msq_puzzle1_stone2",
                state: 2,
            },
            0,
        );
        assert!(run.finished(), "the third turn is state 2");
        assert_eq!(
            run.progress().first().map(|task| task.status),
            Some(TaskStatus::DoneActive),
            "retail: CompleteActive"
        );
    }

    /// A mission's tasks at its place: a kill and an item from containers there.
    fn placed_rows() -> Vec<TaskRow> {
        const PLACE: &str = "AE_Elite_50119";
        vec![
            container("Root", "SimpleTaskContainer", &["Kill", "Items"]),
            TaskRow {
                kill_enemy_type: Some("Wolf".to_owned()),
                poi_tag: Some(PLACE.to_owned()),
                enforce_location: true,
                ..row("Kill", "TaskKillContribution")
            },
            TaskRow {
                item_name: Some("FastenersT4".to_owned()),
                poi_tag: Some(PLACE.to_owned()),
                enforce_location: true,
                chest_drop_probability: Some(100),
                returned_on_turn_in: true,
                ..row("Items", "TaskHaveAndReturnItems")
            },
        ]
    }

    #[test]
    fn a_placed_task_counts_only_in_its_place() {
        let book = TaskBook::new(placed_rows());
        let tree = Arc::new(book.tree("Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, 0);
        let wolf = Event::Killed {
            vitals_id: "Wolf_Grey",
            categories: &[],
            spawn_categories: &[],
        };
        let outside: [&str; 0] = [];
        let facts = |places| Facts {
            held: &[],
            places: Some(places),
            achieved: None,
            level: None,
            territories: None,
            of_family: None,
            houses: None,
        };
        run.sync(&facts(&outside), 0);
        run.event(wolf, 0);
        assert!(
            !run.is_done("Kill"),
            "a kill outside the place does not count"
        );
        assert!(run.chest_drops().is_empty(), "no container drop outside");
        run.sync(&facts(&["ae_elite_50119"]), 0);
        assert_eq!(run.chest_drops(), [("FastenersT4".to_owned(), 100)]);
        run.event(wolf, 0);
        assert!(run.is_done("Kill"), "inside it counts");
        assert_eq!(run.tree.returned_items(), [("FastenersT4".to_owned(), 1)]);
    }

    /// A small tree in the shape of the tutorial's start: an unlock, a video start, a timer then
    /// an unlock, a video end, two kills of a category, a weapon pick in a partial container of
    /// simple containers, an area, and a cycle that is left out.
    fn rows() -> Vec<TaskRow> {
        vec![
            container(
                "Root",
                "ConsecutiveTaskContainer",
                &[
                    "Unlock", "Start", "Wait", "End", "Kills", "Pick", "Area", "Root",
                ],
            ),
            with_item("Unlock", "TaskUnlockAchievement", "Intro_Unlock"),
            with_item("Start", "TaskCinematicStart", "FTUE_Intro2"),
            container("Wait", "ConsecutiveTaskContainer", &["Timer", "Unlock2"]),
            TaskRow {
                duration: Some(TIMER_S),
                ..row("Timer", "TaskTimer")
            },
            with_item("Unlock2", "TaskUnlockAchievement", "Crew_Unlock"),
            with_item("End", "TaskCinematicComplete", "FTUE_Intro2"),
            TaskRow {
                kill_enemy_type: Some("Legion_Legionnaire".to_owned()),
                target_qty: Some(2),
                ..row("Kills", "TaskKillContribution")
            },
            container("Pick", "PartialTaskContainer", &["Soldier", "Destroyer"]),
            container("Soldier", "SimpleTaskContainer", &["Sword", "Hatchet"]),
            with_item("Sword", "TaskEquipItem", "SoldierArchetype_1hLongsword"),
            with_item("Hatchet", "TaskEquipItem", "SoldierArchetype_1hHatchet"),
            container("Destroyer", "SimpleTaskContainer", &["Axe", "Hammer"]),
            with_item("Axe", "TaskEquipItem", "DestroyerArchetype_2hGreataxe"),
            with_item("Hammer", "TaskEquipItem", "DestroyerArchetype_2hWarhammer"),
            TaskRow {
                override_location: Some("755.87 342.49 20.90".to_owned()),
                ..row("Area", "TaskTriggerArea")
            },
        ]
    }

    fn achievements(effects: &[Effect]) -> Vec<&str> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Achievement(name) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_cycle_is_left_out_of_the_tree() {
        let tree = ObjectiveTree::build(&rows(), "root").expect("tree");
        assert_eq!(tree.len(), rows().len());
        assert!(ObjectiveTree::build(&rows(), "Nothing").is_none());
    }

    #[test]
    fn the_tracker_lists_tasks_with_progress_numbered_children_first() {
        let tree = Arc::new(ObjectiveTree::build(&rows(), "Root").expect("tree"));
        // Children first: Unlock 0, Start 1, Timer 2, Unlock2 3, Wait 4, End 5, Kills 6,
        // Sword 7, Hatchet 8, Soldier 9, Axe 10, Hammer 11, Destroyer 12, Pick 13, Area 14,
        // Root 15.
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        let entry = |index, task: &str, progress, done: bool| TaskProgress {
            index,
            task: lowercase_crc32(task),
            progress,
            status: if done {
                TaskStatus::Done
            } else {
                TaskStatus::Running
            },
        };
        assert_eq!(
            run.progress(),
            [entry(0, "Unlock", 1, true), entry(15, "Root", 1, false)],
            "the done unlock and its container, as the retail first card"
        );
        let categories = categories_of("Legion_Legionnaire");
        run.event(Event::CinematicStarted(lowercase_crc32("FTUE_Intro2")), NOW);
        run.tick(NOW + TIMER_MS);
        run.event(Event::CinematicEnded(lowercase_crc32("FTUE_Intro2")), NOW);
        run.event(
            Event::Killed {
                vitals_id: "Legion_Legionnaire_FTUE",
                categories: &categories,
                spawn_categories: &[],
            },
            NOW,
        );
        let progress = run.progress();
        assert!(
            progress.contains(&entry(6, "Kills", 1, false)),
            "{progress:?}"
        );
        assert!(progress.contains(&entry(4, "Wait", 2, true)));
        assert!(progress.contains(&entry(15, "Root", 4, false)));
    }

    #[test]
    fn a_task_type_is_read_in_any_case() {
        // Retail sheets: `TASKUNLOCKACHIEVEMENT` and `TasklockAchievement`.
        assert_eq!(
            kind_of(&with_item("Up", "TASKUNLOCKACHIEVEMENT", "Pushed")),
            TaskKind::UnlockAchievement("Pushed".to_owned())
        );
        assert_eq!(
            kind_of(&with_item("Down", "TasklockAchievement", "Pushed")),
            TaskKind::LockAchievement("Pushed".to_owned())
        );
    }

    #[test]
    fn a_lock_clears_an_achievement_and_a_wait_ends_when_one_is_set() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Lock", "Wait"]),
            with_item(
                "Lock",
                "TaskLockAchievement",
                "SwapActive_01_CaveCorruption",
            ),
            with_item("Wait", "TaskAchievement", "Watched_IGC_Aelstrom_MB"),
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, effects) = ObjectiveRun::start(tree, NOW);
        assert!(effects.contains(&Effect::LockAchievement(
            "SwapActive_01_CaveCorruption".to_owned()
        )));
        assert!(
            run.event(Event::AchievementSet("Other"), NOW).is_empty(),
            "another achievement"
        );
        run.event(Event::AchievementSet("watched_igc_aelstrom_mb"), NOW);
        assert!(run.finished());
    }

    #[test]
    fn a_task_type_the_server_does_not_run_holds_its_quest() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Puzzle"]),
            with_item("Puzzle", "TaskSlayerScript", "PuzzleMSQGong"),
        ];
        assert_eq!(
            kind_of(&rows[1]),
            TaskKind::NotRun("TaskSlayerScript".to_owned())
        );
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.event(Event::Died, NOW);
        assert!(!run.finished(), "no fake success: the quest waits");
    }

    #[test]
    fn a_die_task_counts_the_player_s_deaths() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Die"]),
            TaskRow {
                target_qty: Some(2),
                ..row("Die", "TaskDie")
            },
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.event(Event::Died, NOW);
        assert!(!run.finished(), "one of two deaths");
        run.event(Event::Died, NOW);
        assert!(run.finished());
    }

    #[test]
    fn a_conversation_topic_task_ends_when_its_topic_is_finished() {
        const TOPIC: &str = "12A_MSQ_17_LochnirsNotes_TalkLochnir";
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &[TOPIC]),
            row(TOPIC, "TaskConversationTopic"),
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.event(Event::TopicFinished(lowercase_crc32("another_topic")), NOW);
        assert!(!run.finished(), "another topic");
        run.event(Event::TopicFinished(lowercase_crc32(TOPIC)), NOW);
        assert!(run.finished());
    }

    #[test]
    fn a_conversation_topic_task_with_a_screen_tag_ends_at_the_tags_topic() {
        // Retail data: `12_demonstorm_02_TalkTo_Grace`, `UIScreenTag` `01_Grace_Goodbye_MB`; the
        // live client finished topic `0308314e`, the CRC-32 of the tag.
        const TASK: &str = "12_demonstorm_02_TalkTo_Grace";
        const TAG: &str = "01_Grace_Goodbye_MB";
        const CLIENT_TOPIC: u32 = 0x0308_314e;
        assert_eq!(lowercase_crc32(TAG), CLIENT_TOPIC);
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &[TASK]),
            TaskRow {
                ui_screen_tag: Some(TAG.to_owned()),
                ..row(TASK, "TaskConversationTopic")
            },
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.event(Event::TopicFinished(lowercase_crc32(TASK)), NOW);
        assert!(!run.finished(), "the task id is not the topic");
        run.event(Event::TopicFinished(CLIENT_TOPIC), NOW);
        assert!(run.finished());
    }

    #[test]
    fn the_tree_runs_on_events_in_order() {
        let tree = Arc::new(ObjectiveTree::build(&rows(), "Root").expect("tree"));
        let (mut run, effects) = ObjectiveRun::start(tree, NOW);
        assert_eq!(achievements(&effects), ["Intro_Unlock"]);
        // The video end waits for its start and the timer.
        let intro = lowercase_crc32("FTUE_Intro2");
        assert!(run.event(Event::CinematicEnded(intro), NOW).is_empty());
        run.event(Event::CinematicStarted(intro), NOW);
        assert!(achievements(&run.tick(NOW + TIMER_MS - 1)).is_empty());
        assert_eq!(achievements(&run.tick(NOW + TIMER_MS)), ["Crew_Unlock"]);
        run.event(Event::CinematicEnded(intro), NOW);
        assert!(run.is_done("end"));
        let categories = categories_of("Legion_Legionnaire_FTUE,Hostile,Corrupted");
        let kill = Event::Killed {
            vitals_id: "Legion_Legionnaire_FTUE",
            categories: &categories,
            spawn_categories: &[],
        };
        run.event(kill, NOW);
        assert!(!run.is_done("Kills"));
        run.event(kill, NOW);
        assert!(run.is_done("Kills"));
        // One weapon of one archetype does not finish the pick; both do.
        run.event(Event::Equipped("DestroyerArchetype_2hGreataxe"), NOW);
        assert!(!run.is_done("Pick"));
        let effects = run.event(Event::Equipped("DestroyerArchetype_2hWarhammer"), NOW);
        assert!(effects.contains(&Effect::Done("Pick".to_owned())));
        // The other archetype's weapons no longer count.
        assert!(
            run.event(Event::Equipped("SoldierArchetype_1hHatchet"), NOW)
                .is_empty()
        );
        let far = Position {
            x: AREA.x + TRIGGER_RADIUS_M + 1.0,
            ..AREA
        };
        assert!(run.event(Event::Moved(far), NOW).is_empty());
        run.event(Event::Moved(AREA), NOW);
        assert!(run.finished());
    }

    #[test]
    fn a_run_goes_on_after_a_handoff_with_its_timer_and_kills() {
        const LATER: u64 = NOW + 50_000;
        let tree = Arc::new(ObjectiveTree::build(&rows(), "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(Arc::clone(&tree), NOW);
        let intro = lowercase_crc32("FTUE_Intro2");
        run.event(Event::CinematicStarted(intro), NOW);
        let carried = run.export(NOW + 1);
        let mut resumed = ObjectiveRun::resume(Arc::clone(&tree), &carried, LATER).expect("fits");
        assert_eq!(resumed.progress(), run.progress());
        // The timer keeps what was left of it on the new clock.
        let left = TIMER_MS - 1;
        assert!(achievements(&resumed.tick(LATER + left - 1)).is_empty());
        assert_eq!(achievements(&resumed.tick(LATER + left)), ["Crew_Unlock"]);
        let other = Arc::new(ObjectiveTree::build(&rows(), "Pick").expect("tree"));
        assert!(
            ObjectiveRun::resume(other, &carried, LATER).is_none(),
            "another tree"
        );
        let mut broken = carried;
        broken.listed.push(u32::MAX);
        assert!(
            ObjectiveRun::resume(tree, &broken, LATER).is_none(),
            "a task out of the tree"
        );
    }

    #[test]
    fn items_held_before_their_task_starts_count() {
        let rows = vec![
            container(
                "Root",
                "ConsecutiveTaskContainer",
                &["Wait", "Potions", "Equip"],
            ),
            TaskRow {
                duration: Some(TIMER_S),
                ..row("Wait", "TaskTimer")
            },
            with_item("Potions", "TaskHaveAndReturnItems", "PotionHealthT1"),
            with_item("Equip", "TaskEquipItem", "PotionHealthT1"),
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        // The potions came while the timer ran: the event found no waiting task.
        run.event(
            Event::ItemGained {
                item: "PotionHealthT1",
                quantity: 1,
            },
            NOW,
        );
        run.tick(NOW + TIMER_MS);
        assert!(run.is_active("Potions"));
        run.sync_held(&[("PotionHealthT1", 1, false)], NOW + TIMER_MS);
        assert!(run.is_done("Potions"), "held in the bag");
        assert!(run.is_active("Equip"), "not worn yet");
        run.sync_held(&[("PotionHealthT1", 1, false)], NOW + TIMER_MS);
        assert!(run.is_active("Equip"));
        run.sync_held(&[("PotionHealthT1", 1, true)], NOW + TIMER_MS);
        assert!(run.is_done("Equip"), "worn in its quick slot");
    }

    #[test]
    fn a_removal_asks_for_its_target_quantity() {
        const FEATHERS: u32 = 3;
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Feathers", "Lute"]),
            TaskRow {
                target_qty: Some(i64::from(FEATHERS)),
                ..with_item("Feathers", "TaskRemoveItem", "95a_s02_PeacockFeather_item")
            },
            with_item("Lute", "TaskRemoveItem", "95a_s02_LuteofPhoebos_item"),
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (run, effects) = ObjectiveRun::start(tree, NOW);
        assert!(run.finished());
        assert!(effects.contains(&Effect::RemoveItem(
            "95a_s02_PeacockFeather_item".to_owned(),
            FEATHERS
        )));
        // A row without a `TargetQty` removes one.
        assert!(effects.contains(&Effect::RemoveItem(
            "95a_s02_LuteofPhoebos_item".to_owned(),
            1
        )));
    }

    /// Retail (newchar-retail-2026-09-18 and newchar-linux-2026-09-12, objectives state 3857):
    /// the bag take (2939) completed `EquipWeapon_partial` and the `MSQ_Tutorial_Inventory` unlock
    /// 66 and 56 ms after it; the archetype's equip tasks stayed active until its weapons were
    /// worn.
    #[test]
    fn the_bag_take_ends_the_weapon_pick_and_the_equips_wait_for_the_weapons() {
        let rows = vec![
            container(
                "LootingRuin",
                "ConsecutiveTaskContainer",
                &["EquipWeapon_partial", "Equip_Tutorials"],
            ),
            container(
                "EquipWeapon_partial",
                "PartialTaskContainer",
                &["Weapon1_container", "Weapon2_container"],
            ),
            container(
                "Weapon1_container",
                "SimpleTaskContainer",
                &["TakeLongsword", "TakeGreatAxe"],
            ),
            with_condition(
                "TakeLongsword",
                "TaskHaveAndReturnItems",
                "1hLongswordDropT2",
            ),
            with_condition("TakeGreatAxe", "TaskHaveAndReturnItems", "2hGreatAxeDropT2"),
            container(
                "Weapon2_container",
                "SimpleTaskContainer",
                &["TakeFireStaff", "TakeVoidGauntlet", "TakeGreatSword"],
            ),
            with_condition(
                "TakeFireStaff",
                "TaskHaveAndReturnItems",
                "2hStaffFireDropT2",
            ),
            with_condition(
                "TakeVoidGauntlet",
                "TaskHaveAndReturnItems",
                "1hGauntletVoidDropT2",
            ),
            with_condition(
                "TakeGreatSword",
                "TaskHaveAndReturnItems",
                "2hGreatSwordDropT2",
            ),
            container(
                "Equip_Tutorials",
                "ConsecutiveTaskContainer",
                &["Inventory", "EquipWeapon_partial_redux", "UnlockDoor"],
            ),
            with_item(
                "Inventory",
                "TaskUnlockAchievement",
                "MSQ_Tutorial_Inventory",
            ),
            container(
                "EquipWeapon_partial_redux",
                "PartialTaskContainer",
                &["WeaponSwordsman_container"],
            ),
            container(
                "WeaponSwordsman_container",
                "SimpleTaskContainer",
                &["EquipGreatSword", "EquipBlunderbuss"],
            ),
            with_item(
                "EquipGreatSword",
                "TaskEquipItem",
                "SwordsmanArchetype_2hGreatsword",
            ),
            with_item(
                "EquipBlunderbuss",
                "TaskEquipItem",
                "SwordsmanArchetype_2hBlunderbuss",
            ),
            with_item(
                "UnlockDoor",
                "TaskUnlockAchievement",
                "FTUE_AncientRuins_Door",
            ),
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "LootingRuin").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        // The swordsman bag (backstory `Archetype_Swordsman`): the greatsword is the only take
        // that matches a weapon group. Retail completes that whole group.
        const BAG: &[&str] = &[
            "SwordsmanArchetypeHead",
            "SwordsmanArchetype_2hGreatsword",
            "SwordsmanArchetype_2hBlunderbuss",
        ];
        let effects = run.event(Event::ContainerLooted { items: BAG }, NOW);
        assert!(run.is_done("Weapon2_container"));
        assert!(run.is_done("TakeFireStaff"));
        assert!(run.is_done("TakeVoidGauntlet"));
        assert!(run.is_done("TakeGreatSword"));
        assert!(!run.is_done("Weapon1_container"));
        assert!(!run.is_done("TakeLongsword"));
        assert!(run.is_done("EquipWeapon_partial"));
        assert_eq!(achievements(&effects), ["MSQ_Tutorial_Inventory"]);
        // The weapons came into the bag: not worn.
        let in_bag = [
            ("SwordsmanArchetype_2hGreatsword", 1, false),
            ("SwordsmanArchetype_2hBlunderbuss", 1, false),
        ];
        assert!(achievements(&run.sync_held(&in_bag, NOW)).is_empty());
        assert!(run.is_active("EquipGreatSword"));
        run.event(Event::Equipped("SwordsmanArchetype_2hGreatsword"), NOW);
        assert!(run.is_active("EquipBlunderbuss"));
        let effects = run.event(Event::Equipped("SwordsmanArchetype_2hBlunderbuss"), NOW);
        assert_eq!(achievements(&effects), ["FTUE_AncientRuins_Door"]);
        assert!(run.finished());
    }

    #[test]
    fn a_tagged_area_fires_in_its_level_volume() {
        // Retail own places before and at the entry into each volume (newchar 2026-09-12:
        // 196744 and 196760 ms; 184192 and 184210 ms).
        let place = |x, y, z| Position { x, y, z };
        let cases = [
            (
                "FTUE_First_Fight_Trigger",
                "692.4614 381.3468 26.4687",
                place(696.92, 379.35, 26.22),
                place(696.69, 379.49, 26.42),
            ),
            (
                "FTUE_EnterRuins_00_Trigger",
                "717.6479 371.0387 23.0589",
                place(715.87, 372.99, 23.12),
                place(715.78, 373.07, 23.12),
            ),
        ];
        for (tag, location, before, inside) in cases {
            let rows = vec![
                container("Root", "ConsecutiveTaskContainer", &["Area"]),
                TaskRow {
                    override_location: Some(location.to_owned()),
                    target_tag: Some(tag.to_owned()),
                    ..row("Area", "TaskTriggerArea")
                },
            ];
            let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
            let (mut run, _) = ObjectiveRun::start(tree, NOW);
            run.event(Event::Moved(before), NOW);
            assert!(!run.is_done("Area"), "{tag}: not in it yet");
            run.event(Event::Moved(inside), NOW);
            assert!(run.is_done("Area"), "{tag}: in it");
        }
    }

    #[test]
    fn an_open_world_area_without_a_location_waits_at_its_retail_place() {
        // `12_demonstorm2_03_05_container3`: the area of `TriggerVol_12_Whisper6` has no
        // `OverrideLocation`, and its whisper follows it. Retail `12-9_demonstorm2`: the player
        // was at the campsite turn-in 3,093 m north and at the watchtower for the whisper.
        const WHISPER: &str = "01_Whisper_Beach_06_event";
        let place = |x, y, z| Position { x, y, z };
        let cases = [
            (
                "TriggerVol_12_Whisper6",
                place(7306.5674, 3093.327, 44.940018),
                place(7299.863, 2952.437, 70.25296),
            ),
            (
                "12A_LeaveLochnir",
                place(7381.4175, 3732.41, 112.64821),
                place(7422.975, 3624.002, 104.22217),
            ),
        ];
        for (tag, away, at) in cases {
            let rows = vec![
                container("Root", "ConsecutiveTaskContainer", &["Area", "Whisper"]),
                TaskRow {
                    target_tag: Some(tag.to_owned()),
                    trigger_area_type: Some("Enter".to_owned()),
                    ..row("Area", "TaskTriggerArea")
                },
                TaskRow {
                    kill_enemy_type: Some(WHISPER.to_owned()),
                    ..row("Whisper", "TaskTriggerGameEvent")
                },
            ];
            let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
            let (mut run, _) = ObjectiveRun::start(tree, NOW);
            assert!(run.event(Event::Moved(away), NOW).is_empty(), "{tag}: away");
            assert!(!run.is_done("Area"), "{tag}: not there yet");
            let effects = run.event(Event::Moved(at), NOW);
            assert!(run.is_done("Area"), "{tag}: at its retail place");
            assert!(
                effects.contains(&Effect::GameEvent(WHISPER.to_owned())),
                "{tag}: the whisper after it fires"
            );
        }
    }

    #[test]
    fn an_area_without_a_location_is_done_in_its_point_of_interest() {
        // Live 2026-10-01: the player came back to the campsite of Aelstrom and Urda (POI
        // `watchtower`, territory 30117) by a path away from the retail place of
        // `TriggerVol_12_Whisper6`, and the quest waited there for ever.
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Area"]),
            TaskRow {
                target_tag: Some("TriggerVol_12_Whisper6".to_owned()),
                poi_tag: Some("watchtower".to_owned()),
                trigger_area_type: Some("Enter".to_owned()),
                ..row("Area", "TaskTriggerArea")
            },
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        let facts = |places| Facts {
            held: &[],
            places: Some(places),
            achieved: None,
            level: None,
            territories: None,
            of_family: None,
            houses: None,
        };
        run.sync(&facts(&["drowned_cavern_30122"]), NOW);
        assert!(!run.is_done("Area"), "elsewhere");
        run.sync(&facts(&["watchtower"]), NOW);
        assert!(run.is_done("Area"), "in its point of interest");
    }

    #[test]
    fn an_area_with_a_location_waits_for_its_place_not_its_point_of_interest() {
        // `12_demonstorm2_03_01_TaskTriggerArea_whisper`: a location and the POI of the cave.
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Area"]),
            TaskRow {
                target_tag: Some("TriggerVol_12_CaveWhisper".to_owned()),
                override_location: Some("7290.2793 2900.3755 51.2266".to_owned()),
                poi_tag: Some("drowned_cavern_30122".to_owned()),
                ..row("Area", "TaskTriggerArea")
            },
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.sync(
            &Facts {
                held: &[],
                places: Some(&["drowned_cavern_30122"]),
                achieved: None,
                level: None,
                territories: None,
                of_family: None,
                houses: None,
            },
            NOW,
        );
        assert!(!run.is_done("Area"), "its location decides");
    }

    #[test]
    fn a_box_volume_turns_with_its_capital_and_a_sphere_is_round() {
        // `FTUE_TF_Trigger`: 30 x 3 x 18 m, turned 210 degrees.
        let tf = TRIGGER_VOLUMES
            .iter()
            .find(|volume| volume.tag == "FTUE_TF_Trigger")
            .expect("volume");
        let yaw = QUATERNION_HALF_ANGLES * tf.turn[0].atan2(tf.turn[1]);
        let at = |along: f32, across: f32, up: f32| Position {
            x: tf.at.x + along * yaw.cos() - across * yaw.sin(),
            y: tf.at.y + along * yaw.sin() + across * yaw.cos(),
            z: tf.at.z + up,
        };
        assert!(tf.contains(at(14.9, 1.4, 8.9)), "inside a corner");
        assert!(!tf.contains(at(15.1, 0.0, 0.0)), "past its length");
        assert!(!tf.contains(at(0.0, 1.6, 0.0)), "past its width");
        assert!(!tf.contains(at(0.0, 0.0, 9.1)), "above it");
        let fight = TRIGGER_VOLUMES
            .iter()
            .find(|volume| volume.tag == "FTUE_First_Fight_Trigger")
            .expect("volume");
        let up = |metres: f32| Position {
            z: fight.at.z + metres,
            ..fight.at
        };
        assert!(fight.contains(up(6.9)));
        assert!(!fight.contains(up(7.1)), "a sphere, not a circle");
        // A tag the level has no volume for waits around its location.
        let around = TriggerArea::of(Some("TriggerVol_12_Spawn_A"), Some(AREA));
        assert_eq!(around, Some(TriggerArea::Around(AREA)));
        assert_eq!(TriggerArea::of(Some("TriggerVol_12_Spawn_A"), None), None);
    }

    #[test]
    fn kill_types_name_categories_and_id_starts() {
        let categories = categories_of("Corrupted_Sailor_1H_Club_FTUE, Hostile,Corrupted");
        assert!(kills_count(
            "corrupted",
            "Corrupted_Sailor_1H_Club_FTUE",
            &categories,
            &[]
        ));
        assert!(kills_count(
            "Legion_Legionnaire",
            "Legion_Legionnaire_FTUE",
            &[],
            &[]
        ));
        assert!(!kills_count(
            "Legion_Legion",
            "Legion_Legionnaire_FTUE",
            &[],
            &[]
        ));
        assert!(!kills_count(
            "Damned_Commander_FTUE",
            "Corruption_Entity",
            &categories,
            &[]
        ));
    }

    #[test]
    fn a_kill_counts_by_its_spawners_category_and_its_group() {
        // Client row `16_Side_20_d_03_gather_heart` drops from `Barbary_Lion_16BS12`, the
        // category 9 of the level's point spawners give their `Barbary_Lion` mobs.
        let spawner = [lowercase_crc32("barbary_lion_16bs12")];
        assert!(kills_count(
            "Barbary_Lion_16BS12",
            "Barbary_Lion",
            &[],
            &spawner
        ));
        assert!(!kills_count(
            "Barbary_Lion_16BS12",
            "Barbary_Lion",
            &[],
            &[]
        ));
        assert!(!kills_count(
            "Barbary_Lion_16BS12",
            "Barbary_Lion",
            &[],
            &[lowercase_crc32("barbary_lion_16bs05")]
        ));
        // `VitalsCategoryData`: `Bloated_Corpse_13WF08a` is in the group `Periville`.
        let types = KillTypes::default()
            .with_groups([("Bloated_Corpse_13WF08a".to_owned(), "Periville".to_owned())]);
        let corpse = types.spawn_categories(Some(lowercase_crc32("bloated_corpse_13wf08a")));
        assert!(kills_count("Periville", "Bloated_Corpse", &[], &corpse));
        assert!(types.spawn_categories(None).is_empty());
    }

    #[test]
    fn kill_types_are_found_by_vitals_crc() {
        let types = KillTypes::build([(
            "Corruption_Entity".to_owned(),
            "Corruption_Entity,Hostile,Corrupted".to_owned(),
        )]);
        let (id, categories) = types
            .of(lowercase_crc32("corruption_entity"))
            .expect("type");
        assert_eq!(id, "Corruption_Entity");
        assert_eq!(categories, ["Corruption_Entity", "Hostile", "Corrupted"]);
        assert_eq!(types.of(0), None);
    }

    /// The retail events of the live tutorial run (live-tutorial note, step table), in order.
    fn retail_tutorial_events() -> Vec<RetailStep> {
        use RetailStep::{Cinematic, Cutscene, Equip, Gain, Kill, Loot, Move, Start, Wait};
        let at = |x, y, z| Move(Position { x, y, z });
        vec![
            Start("ftue_intro2"),
            Wait(2_000),
            Cinematic("ftue_intro2"),
            Cutscene("FTUE_01_SoleSurvivor_ConfrontingCrewmember_IGC"),
            Kill("Corrupted_Sailor_1H_Club_FTUE"),
            Kill("Corrupted_Sailor_1H_Club_FTUE"),
            Kill("Corrupted_Sailor_1H_Club_FTUE"),
            Wait(2_000),
            Cinematic("ftue_intro3"),
            Cutscene("FTUE_01_SoleSurvivor_WaterIntro_IGC"),
            at(755.87, 342.49, 20.9),
            // The centres of the trigger volumes that the task's `OverrideLocation` (its map
            // marker) misses, where a walking player crosses them: `FTUE_TR_Trigger`, a 3 m wide
            // box, lies 1.8 m from the marker (725.16, 360.02).
            at(725.41, 358.08, 20.0),
            Loot(&[
                "DestroyerArchetype_2hGreataxe",
                "DestroyerArchetype_2hWarhammer",
            ]),
            Equip("DestroyerArchetype_2hWarhammer"),
            Equip("DestroyerArchetype_2hGreataxe"),
            at(717.65, 371.04, 23.06),
            at(714.25, 379.70, 24.05),
            at(692.46, 381.35, 26.47),
            Kill("Legion_Legionnaire_FTUE"),
            at(661.06, 394.73, 26.07),
            at(663.24, 407.64, 26.47),
            Gain("PotionHealthT1"),
            Equip("PotionHealthT1"),
            at(637.04, 420.13, 23.50),
            at(629.98, 423.27, 22.13),
            Kill("Legion_Legionnaire_FTUE"),
            at(599.54, 401.25, 21.76),
            at(591.2, 392.56, 22.36),
            at(591.2, 392.56, 22.36),
            Kill("Corruption_Entity_Crawler_FTUE"),
            Kill("Corruption_Entity_Crawler_FTUE"),
            Kill("Corruption_Entity_Crawler_FTUE"),
            Kill("Corruption_Entity"),
            at(569.30, 373.32, 26.30),
            at(564.84, 371.00, 26.86),
            at(542.47, 367.10, 31.98),
            at(522.70, 368.83, 40.20),
            at(516.0, 371.0, 43.0),
            Wait(15_000),
            at(487.82, 350.29, 36.58),
            at(484.18, 321.15, 32.52),
            at(479.59, 294.95, 36.52),
            at(465.93, 269.1, 36.56),
            at(477.72, 232.44, 22.36),
            Cutscene("FTUE_01_SoleSurvivor2_TheFountain_IGC"),
            Cutscene("FTUE_01_SoleSurvivor2_OpeningTheBox_IGC"),
            at(489.23, 220.87, 22.0),
            Cinematic("ftue_rez"),
        ]
    }

    /// One retail event of [`retail_tutorial_events`].
    enum RetailStep {
        Start(&'static str),
        Cinematic(&'static str),
        Cutscene(&'static str),
        Kill(&'static str),
        Move(Position),
        Loot(&'static [&'static str]),
        Gain(&'static str),
        Equip(&'static str),
        Wait(u64),
    }

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_task_rows_the_server_does_not_run_are_the_known_ones() {
        const BUILD: &str = "25810808";
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let mut unsupported: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for row in task_rows(&bundle.tables) {
            if let TaskKind::NotRun(_) = kind_of(&row) {
                unsupported
                    .entry(row.kind.to_lowercase())
                    .or_default()
                    .push(row.task_id.clone());
            }
        }
        let counts: Vec<(String, usize, Vec<String>)> = unsupported
            .into_iter()
            .map(|(kind, rows)| {
                let count = rows.len();
                (kind, count, rows.into_iter().take(3).collect())
            })
            .collect();
        let kinds: Vec<&str> = counts.iter().map(|(kind, _, _)| kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "patroltask",
                "taskcontrolpoint",
                "taskperformemote",
                "taskslayerscript"
            ],
            "{counts:#?}"
        );
    }

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_open_world_quest_starts_as_retail() {
        const BUILD: &str = "25810808";
        /// The open-world start: the client spawns inside the start trigger.
        const SPAWN: Position = Position {
            x: 7280.49,
            y: 3141.49,
            z: 40.12,
        };
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let tree = Arc::new(
            ObjectiveTree::build(
                &task_rows(&bundle.tables),
                crate::player_content::OPEN_WORLD_QUEST_ROOT,
            )
            .expect("open-world quest"),
        );
        let (mut run, mut effects) = ObjectiveRun::start(tree, NOW);
        effects.extend(run.event(Event::Moved(SPAWN), NOW));
        // Retail's first open-world state (xp-potion-exit note, section 3.3): the slice swaps and
        // the grace unlocks done, the start trigger and its unlock done.
        for wanted in [
            Effect::Achievement("SwapActive_0425_HerbalistPath_mb".to_owned()),
            Effect::LockAchievement("SwapActive_01_CaveCorruption".to_owned()),
            Effect::Achievement("SwapActive_01_UrdaMB_01".to_owned()),
            Effect::Achievement("SwapActive_01_GraceCombatLoop_mb".to_owned()),
            Effect::Achievement("SwapActive_01_GraceCombatLoop_VO_MB".to_owned()),
            Effect::Achievement("Start_IGC_Aelstrom_MB".to_owned()),
        ] {
            assert!(effects.contains(&wanted), "{wanted:?} in {effects:?}");
        }
        assert!(!run.finished());
    }

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_tutorial_quest_runs_to_its_end_on_the_retail_events() {
        const BUILD: &str = "25810808";
        /// The achievement deltas of the live run, in order (live-tutorial note, section 1).
        const RETAIL_ACHIEVEMENTS: [&str; 27] = [
            "FTUE_Intro2_Cinematic_Unlock",
            "FTUE_PCCM_Cinematic_Unlock",
            "FTUE_Intro2_Cinematic_Watched",
            "FTUE_PCCM_Cinematic_Watched",
            "FTUE_Intro3_Cinematic_Unlock",
            "FTUE_S1_Cinematic_Unlock",
            "FTUE_Intro3_Cinematic_Watched",
            "FTUE_S1_Cinematic_Watched",
            "MSQ_Tutorial_Swimming",
            "MSQ_Tutorial_SwimDash",
            "MSQ_Tutorial_Loot",
            "MSQ_Tutorial_Inventory",
            "FTUE_AncientRuins_Door",
            "Tutorial_TargetLock1",
            "Tutorial_TargetLock2",
            "MSQ_Tutorial_Climb",
            "Tutorial_TargetLockSwitch1",
            "MSQ_Tutorial_WeaponMastery",
            "FTUE_Vista_Cinematic_Unlock",
            "FTUE_Vista_Cinematic_Watched",
            "Tutorial_Zoom",
            "FTUE_TF_Cinematic_Unlock",
            "FTUE_TF_Cinematic_Watched",
            "FTUE_OTB_Cinematic_Watched",
            "FTUE_Rez_Cinematic_Unlock",
            "FTUE_Rez_Cinematic_Watched",
            "SoleSurvivor02_Check",
        ];
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let tree = Arc::new(
            ObjectiveTree::build(
                &task_rows(&bundle.tables),
                crate::player_content::TUTORIAL_QUEST_ROOT,
            )
            .expect("tutorial quest"),
        );
        let kill_types = KillTypes::from_tables(&bundle.tables);
        let (mut run, mut effects) = ObjectiveRun::start(tree, NOW);
        let mut now = NOW;
        for step in retail_tutorial_events() {
            let more = match step {
                RetailStep::Start(video) => {
                    run.event(Event::CinematicStarted(lowercase_crc32(video)), now)
                }
                RetailStep::Cinematic(video) => {
                    run.event(Event::CinematicEnded(lowercase_crc32(video)), now)
                }
                RetailStep::Cutscene(cutscene) => {
                    run.event(Event::CutsceneEnded(lowercase_crc32(cutscene)), now)
                }
                RetailStep::Kill(vitals) => {
                    let (vitals_id, categories) =
                        kill_types.of(lowercase_crc32(vitals)).expect("kill type");
                    run.event(
                        Event::Killed {
                            vitals_id,
                            categories,
                            spawn_categories: &[],
                        },
                        now,
                    )
                }
                RetailStep::Move(position) => run.event(Event::Moved(position), now),
                RetailStep::Loot(items) => run.event(Event::ContainerLooted { items }, now),
                RetailStep::Gain(item) => run.event(Event::ItemGained { item, quantity: 1 }, now),
                RetailStep::Equip(item) => run.event(Event::Equipped(item), now),
                RetailStep::Wait(ms) => {
                    now += ms;
                    run.tick(now)
                }
            };
            effects.extend(more);
        }
        assert!(run.finished(), "the quest ends after the last video");
        // The wire numbers of the retail tracker (objectives-state note, section 4).
        let progress = run.progress();
        let index_of = |task: &str| {
            progress
                .iter()
                .find(|entry| entry.task == lowercase_crc32(task))
                .map(|entry| entry.index)
        };
        assert_eq!(index_of("01_SoleSurvivor2_FTUE_Intro2_Unlock"), Some(0));
        assert_eq!(
            index_of("01_SoleSurvivor2_Container_BK2_FTUE_Intro2"),
            Some(7)
        );
        assert_eq!(index_of("01_SoleSurvivor2_Container_Boat"), Some(24));
        assert_eq!(index_of("01_SoleSurvivor2_Container"), Some(133));
        let mut set = achievements(&effects);
        // The tree sets `FTUE_OTB_Cinematic_Watched` twice; the second changes nothing.
        set.dedup();
        assert_eq!(set, RETAIL_ACHIEVEMENTS);
        assert!(effects.contains(&Effect::RemoveItem("1hSwordT2_FTUE".to_owned(), 1)));
    }

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_mount_quest_runs_to_its_end() {
        // `9401_BefriendHorse` ("My Kingdom for a Horse", Jochi Khan in Everfall): the cave
        // trigger, the torch, the mural and its whisper, the azoth oats, then the talk with the
        // horse (`javelindata_c94_objectivetasks`).
        const BUILD: &str = "25810808";
        const ROOT: &str = "9401_Befriend_Container";
        const STEP_MS: usize = 30_000;
        /// The `9401_Goto_Cave` trigger's place.
        const CAVE: Position = Position {
            x: 9103.09,
            y: 5147.67,
            z: 144.10,
        };
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let book = TaskBook::new(task_rows(&bundle.tables));
        let tree = Arc::new(book.tree(ROOT).expect("the mount quest"));
        let (mut run, mut effects) = ObjectiveRun::start(tree, NOW);
        let steps = [
            Event::Moved(CAVE),
            Event::GameEvent("94_BeastTorch_Interact"),
            Event::GameEvent("94_BeastMural_Interact"),
            Event::GameEvent("94_AzothOats_Interact"),
            Event::TopicFinished(lowercase_crc32("9401_HorseTalk")),
        ];
        for (event, now) in steps.into_iter().zip((NOW..).step_by(STEP_MS)) {
            effects.extend(run.tick(now));
            effects.extend(run.event(event, now));
        }
        assert!(run.finished(), "{:?}", run.progress());
        for wanted in [
            Effect::Achievement("Vector_Mounts_PushQuest".to_owned()),
            Effect::Achievement("9401_Befriend_Started".to_owned()),
            Effect::GameEvent("94_BeastMural_Whisper".to_owned()),
            Effect::Achievement("9401_HorseWalkAway".to_owned()),
            Effect::LockAchievement("9401_HorseWalkAway".to_owned()),
        ] {
            assert!(effects.contains(&wanted), "{wanted:?} in {effects:?}");
        }
        // Its success event gives the reward box and Riding 500; salvaging the box fires the
        // event whose achievement owns the horse.
        const SUCCESS_EVENT: &str = "9401_BefriendHorse_Success";
        const BOX: &str = "MountItem_MountReward_Box_1";
        const BOX_EVENT: &str = "Reward_MountReward_Box_1";
        const HORSE: &str = "Mount_Horse_1";
        const HORSE_GIFT: &str = "AwardAchievement_Mount_Horse_1";
        let rewards = nw_content::rewards::Rewards::from_tables(&bundle.tables);
        assert_eq!(
            rewards
                .rolled(SUCCESS_EVENT, &mut |_| 0)
                .and_then(|reward| reward.item),
            Some((BOX.to_owned(), 1))
        );
        let grants = nw_content::rewards::EventGrants::from_tables(&bundle.tables);
        assert_eq!(
            grants
                .get(SUCCESS_EVENT)
                .and_then(|grant| grant.progression.clone()),
            Some(("Riding".to_owned(), 500.0))
        );
        let salvage = nw_content::salvage::Salvage::from_tables(&bundle.tables);
        assert_eq!(
            salvage.of(BOX).and_then(|rule| rule.event.as_deref()),
            Some(BOX_EVENT)
        );
        assert_eq!(
            grants
                .get(BOX_EVENT)
                .and_then(|grant| grant.achievement.as_deref()),
            Some(HORSE_GIFT)
        );
        let mounts = nw_content::mounts::MountRules::from_tables(&bundle.tables);
        assert_eq!(
            mounts
                .mount(lowercase_crc32(HORSE))
                .and_then(|mount| mount.required_achievement.as_deref()),
            Some(HORSE_GIFT)
        );
    }

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_wreck_quests_run_to_their_end_on_the_retail_steps() {
        const BUILD: &str = "25810808";
        /// The corrupted mariners of the second wreck (17 of the 21 spawns within 60 m of the
        /// crewmates' pin in the level's world file).
        const WRECK_MARINER: u32 = 2_756_893_354;
        /// Longer than every timer of the four trees (22.5 s).
        const STEP_MS: usize = 30_000;
        let at = |x, y| Event::Moved(Position { x, y, z: AREA.z });
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let book = TaskBook::new(task_rows(&bundle.tables));
        let kill_types = KillTypes::from_tables(&bundle.tables);
        let (mariner, categories) = kill_types.of(WRECK_MARINER).expect("the mariner");
        let mut fired = Vec::new();
        let mut set = Vec::new();
        let mut run_quest = |root: &str, steps: &[(&[&str], bool, Option<Event<'_>>)]| {
            let tree = Arc::new(book.tree(root).expect("tree"));
            let returned = tree.returned_items();
            let (mut run, mut effects) = ObjectiveRun::start(tree, NOW);
            // Each step comes after the timers that the step before started.
            for ((places, lore, event), now) in steps.iter().zip((NOW..).step_by(STEP_MS)) {
                let achieved = |id: &str| *lore && id == MANIFEST_LORE;
                effects.extend(run.tick(now));
                effects.extend(run.sync(
                    &Facts {
                        held: &[],
                        places: Some(places),
                        achieved: Some(&achieved),
                        level: None,
                        territories: None,
                        of_family: None,
                        houses: None,
                    },
                    now,
                ));
                if let Some(event) = event {
                    effects.extend(run.event(*event, now));
                }
            }
            for effect in effects {
                match effect {
                    Effect::GameEvent(event) => fired.push(event),
                    Effect::Achievement(id) => set.push(id),
                    _ => {}
                }
            }
            assert!(run.finished(), "{root}: {:?}", run.progress());
            returned
        };
        let kill = Event::Killed {
            vitals_id: mariner,
            categories,
            spawn_categories: &[],
        };
        let watchtower: &[&str] = &["watchtower"];
        let wreck_2: &[&str] = &["drowned_shipwreck_2_30125"];
        let wreck_4: &[&str] = &["drowned_shipwreck_4_30130"];
        let cutscene = |name: &str| Event::CutsceneEnded(lowercase_crc32(name));
        let nodule = Event::GameEvent(NODULE_EVENT);
        let gained = |item| Event::ItemGained { item, quantity: 1 };

        // Crewmates No More: out of the camp's watchtower, to the second wreck, five kills.
        let mut crewmates = vec![(watchtower, false, None), (&[][..], false, None)];
        crewmates.extend([(wreck_2, false, Some(kill)); 5]);
        assert!(run_quest("12_crewmates_container", &crewmates).is_empty());
        // Amidst the Wreckage: the whisper, Grace's call, four nodules, her thanks.
        let survivors = [
            (&[][..], false, Some(at(7228.6, 3210.7))),
            (&[][..], false, Some(at(7119.9, 3265.1))),
            (
                &[][..],
                false,
                Some(cutscene("12_Survivors_MSQ_GraceIntroHelp_MB_IGC")),
            ),
            (&[][..], false, Some(nodule)),
            (&[][..], false, Some(nodule)),
            (&[][..], false, Some(nodule)),
            (&[][..], false, Some(nodule)),
            (
                &[][..],
                false,
                Some(cutscene("12_Survivors_MSQ_GraceOutroThanks_MB_IGC")),
            ),
            (&[][..], false, None),
            (&[][..], false, None),
        ];
        assert!(run_quest("12_survivors_container", &survivors).is_empty());
        // Lessons of a Pirate's Life: the manifest and three treasures, given back.
        let pillage = [
            (&[][..], true, Some(gained("0601_0104_Treasure_A"))),
            (&[][..], true, Some(gained("0601_0104_Treasure_B"))),
            (&[][..], true, Some(gained("0601_0104_Treasure_C"))),
        ];
        assert_eq!(
            run_quest("12_pillageship_container", &pillage).len(),
            3,
            "the treasures go back"
        );
        // Memories of Grace: the fourth wreck's flag and wheel, the whisper, the watchtower.
        let recover = [
            (wreck_4, false, Some(gained("01_MSQ_GracesFlag"))),
            (wreck_4, false, Some(gained("0601_0104_ShipWheel"))),
            (&[][..], false, None),
            (watchtower, false, None),
        ];
        assert_eq!(run_quest("12_recoveritems_container", &recover).len(), 2);
        assert_eq!(
            fired,
            [
                "01_IsabellasFirstWhisper_Isabella_01_event",
                "12A_MB_whisper_03_interact"
            ]
        );
        for wanted in [
            "Start_IGC_GraceCallsForHelpMB",
            "12_Survivors_MSQ_GraceOutroThanks_IGC_Unlock",
            "SwapActive_01_GraceMB_B",
            "SwapActive_01_GraceMB_C",
            "MSQ_Tutorial_Autorun",
        ] {
            assert!(set.iter().any(|id| id == wanted), "{wanted} in {set:?}");
        }
    }

    /// The retail task rows of `12_skinningknife` and `12_boarfeast` (build 25810808), as their
    /// trees use them.
    pub(crate) fn beach_rows() -> Vec<TaskRow> {
        let unlock =
            |id: &str, achievement: &str| with_item(id, "TaskUnlockAchievement", achievement);
        let have = |id: &str, item: &str| TaskRow {
            target_qty: Some(1),
            ..with_item(id, "TaskHaveItems", item)
        };
        vec![
            container(
                "12_skinningknife_container",
                "ConsecutiveTaskContainer",
                &[
                    "12_skinningknife_01_Walkaway_01_Aelstrom_p01_Off",
                    "12_skinningknife_02_TutorialUnlock",
                    "12_skinningknife_03_timerAndGather_container_partial",
                ],
            ),
            unlock(
                "12_skinningknife_01_Walkaway_01_Aelstrom_p01_Off",
                "SwapActive_01_AelstromMB_01",
            ),
            unlock(
                "12_skinningknife_02_TutorialUnlock",
                "tutorial_gathering_bushflint",
            ),
            container(
                "12_skinningknife_03_timerAndGather_container_partial",
                "PartialTaskContainer",
                &[
                    "12_skinningknife_03_HaveKnife_container",
                    "12_skinningknife_03_gatherThenCraft_container",
                    "12_skinningknife_03_SwapTimerNeverComplete_container",
                ],
            ),
            container(
                "12_skinningknife_03_HaveKnife_container",
                "ConsecutiveTaskContainer",
                &[
                    "12_skinningknife_03_HaveKnife",
                    "12_skinningknife_03_SliceSwap_01_Aelstrom_p03_On",
                ],
            ),
            have("12_skinningknife_03_HaveKnife", "1hSkinningKnifeT1"),
            unlock(
                "12_skinningknife_03_SliceSwap_01_Aelstrom_p03_On",
                "SwapActive_01_AelstromMB_03",
            ),
            container(
                "12_skinningknife_03_gatherThenCraft_container",
                "ConsecutiveTaskContainer",
                &[
                    "12_skinningknife_03_gather_container",
                    "12_skinningknife_03_TaskHaveItems_1_1hSkinningKnifeT1",
                ],
            ),
            container(
                "12_skinningknife_03_gather_container",
                "SimpleTaskContainer",
                &[
                    "12_skinningknife_03_TaskHaveItems_1_FlintT1",
                    "12_skinningknife_03_TaskHaveItems_1_WoodT1",
                ],
            ),
            have("12_skinningknife_03_TaskHaveItems_1_FlintT1", "FlintT1"),
            have("12_skinningknife_03_TaskHaveItems_1_WoodT1", "WoodT1"),
            have(
                "12_skinningknife_03_TaskHaveItems_1_1hSkinningKnifeT1",
                "1hSkinningKnifeT1",
            ),
            container(
                "12_skinningknife_03_SwapTimerNeverComplete_container",
                "ConsecutiveTaskContainer",
                &[
                    "12_skinningknife_03_swapDelay",
                    "12_skinningknife_03_NeverTrue_task",
                ],
            ),
            TaskRow {
                duration: Some(TIMER_S),
                ..row("12_skinningknife_03_swapDelay", "TaskTimer")
            },
            TaskRow {
                ui_screen_tag: Some("neverbetrue".to_owned()),
                ..row("12_skinningknife_03_NeverTrue_task", "TaskGameEvent")
            },
            container(
                "12_boarfeast_container",
                "ConsecutiveTaskContainer",
                &[
                    "12_boarfeast_01_kill_1_Boar",
                    "12_boarfeast_02_equip_skinning",
                    "12_boarfeast_03_TaskGatherItems_1_RedMeatT1",
                    "12_boarfeast_05_01_TaskGameEvent_1_Craft_CookingT1",
                    "12_boarfeast_05_02_TaskHaveItems_1_LightRation",
                ],
            ),
            TaskRow {
                kill_enemy_type: Some("Boar".to_owned()),
                target_qty: Some(1),
                ..row("12_boarfeast_01_kill_1_Boar", "TaskKillContribution")
            },
            with_item(
                "12_boarfeast_02_equip_skinning",
                "TaskEquipItem",
                "1hSkinningKnifeT1",
            ),
            TaskRow {
                target_qty: Some(1),
                item_drop_vc: Some("Boar".to_owned()),
                item_drop_probability: Some(100),
                ..with_item(
                    "12_boarfeast_03_TaskGatherItems_1_RedMeatT1",
                    "TaskGatherItems",
                    "PorkT1",
                )
            },
            TaskRow {
                crafting_recipe_id: Some("Procedural_HealthRecoveryT1".to_owned()),
                ..with_item(
                    "12_boarfeast_05_01_TaskGameEvent_1_Craft_CookingT1",
                    "TaskCraftRecipe",
                    "FoodHealthRecoveryT1",
                )
            },
            have(
                "12_boarfeast_05_02_TaskHaveItems_1_LightRation",
                "FoodHealthRecoveryT1",
            ),
        ]
    }

    #[test]
    fn the_knife_ends_the_skinning_knife_quest_and_the_timer_branch_stops() {
        let tree = Arc::new(
            ObjectiveTree::build(&beach_rows(), "12_skinningknife_container").expect("tree"),
        );
        let (mut run, effects) = ObjectiveRun::start(tree, NOW);
        assert_eq!(
            achievements(&effects),
            [
                "SwapActive_01_AelstromMB_01",
                "tutorial_gathering_bushflint"
            ]
        );
        // The slice-swap timer runs out; its never-true task keeps the branch open.
        run.tick(NOW + TIMER_MS);
        assert!(run.is_done("12_skinningknife_03_swapDelay"));
        assert!(!run.finished());
        run.sync_held(&[("FlintT1", 1, false)], NOW + TIMER_MS);
        assert!(run.is_done("12_skinningknife_03_TaskHaveItems_1_FlintT1"));
        assert!(!run.is_done("12_skinningknife_03_gather_container"));
        run.sync_held(
            &[("FlintT1", 1, false), ("WoodT1", 1, false)],
            NOW + TIMER_MS,
        );
        assert!(run.is_done("12_skinningknife_03_gather_container"));
        let effects = run.sync_held(&[("1hSkinningKnifeT1", 1, false)], NOW + TIMER_MS);
        assert!(run.finished(), "the knife ends it");
        assert!(effects.contains(&Effect::Achievement(
            "SwapActive_01_AelstromMB_03".to_owned()
        )));
        let stopped = run
            .progress()
            .into_iter()
            .find(|entry| {
                entry.task
                    == lowercase_crc32("12_skinningknife_03_SwapTimerNeverComplete_container")
            })
            .expect("listed");
        assert_eq!(stopped.status, TaskStatus::Stopped, "retail state 4");
        assert_eq!(stopped.progress, 1, "its timer done");
    }

    #[test]
    fn the_boar_feast_runs_on_a_kill_an_equip_a_skinning_and_a_craft() {
        let tree =
            Arc::new(ObjectiveTree::build(&beach_rows(), "12_boarfeast_container").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        let boar = categories_of("Boar,Hostile");
        assert!(
            run.quest_drops("Boar", &boar, &[]).is_empty(),
            "no drop before its task"
        );
        run.event(
            Event::Killed {
                vitals_id: "Boar",
                categories: &boar,
                spawn_categories: &[],
            },
            NOW,
        );
        run.event(Event::Equipped("1hSkinningKnifeT1"), NOW);
        assert_eq!(
            run.quest_drops("Boar", &boar, &[]),
            [("PorkT1".to_owned(), 100)]
        );
        assert!(
            run.quest_drops("Wolf", &[], &[]).is_empty(),
            "another enemy"
        );
        run.event(
            Event::ItemGained {
                item: "PorkT1",
                quantity: 2,
            },
            NOW,
        );
        assert!(run.is_done("12_boarfeast_03_TaskGatherItems_1_RedMeatT1"));
        run.event(Event::Crafted("procedural_healthrecoveryt1"), NOW);
        assert!(!run.finished(), "the ration must be in the bag");
        run.sync_held(&[("FoodHealthRecoveryT1", 1, false)], NOW);
        assert!(run.finished());
    }

    #[test]
    fn a_have_items_task_shows_how_many_the_player_holds() {
        let rows = vec![
            container("Root", "ConsecutiveTaskContainer", &["Stones"]),
            TaskRow {
                target_qty: Some(3),
                ..with_item("Stones", "TaskHaveItems", "FlintT1")
            },
        ];
        let tree = Arc::new(ObjectiveTree::build(&rows, "Root").expect("tree"));
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        run.sync_held(&[("FlintT1", 1, false), ("FlintT1", 1, true)], NOW);
        let stones = |run: &ObjectiveRun| {
            run.progress()
                .into_iter()
                .find(|entry| entry.task == lowercase_crc32("Stones"))
                .map(|entry| (entry.progress, entry.status))
        };
        assert_eq!(stones(&run), Some((2, TaskStatus::Running)));
        run.sync_held(&[("FlintT1", 3, false)], NOW);
        assert_eq!(stones(&run), Some((3, TaskStatus::Done)));
    }

    /// The places, objects and events of the wreck quests (build 25810808 rows of
    /// `12_crewmates` to `12_recoveritems`) in one tree: a go-to, a lore note, a fired whisper,
    /// two nodules, a treasure to bring back, a leave area and an exit.
    pub(crate) fn wreck_rows() -> Vec<TaskRow> {
        let poi = |id: &str, kind: &str| TaskRow {
            poi_tag: Some(WRECK_POI.to_owned()),
            ..row(id, kind)
        };
        vec![
            container(
                WRECK_ROOT,
                "ConsecutiveTaskContainer",
                &[
                    "GoToWreck",
                    "ShipsManifest",
                    "Whisper",
                    "Nodules",
                    "Treasure",
                    "LeaveTrigger",
                    "ExitWreck",
                ],
            ),
            poi("GoToWreck", "TaskGoToPOITag"),
            with_item("ShipsManifest", "TaskAchievement", MANIFEST_LORE),
            TaskRow {
                kill_enemy_type: Some(WHISPER_EVENT.to_owned()),
                ..row("Whisper", "TaskTriggerGameEvent")
            },
            TaskRow {
                kill_enemy_type: Some(NODULE_EVENT.to_owned()),
                target_qty: Some(2),
                ..row("Nodules", "TaskGameEvent")
            },
            TaskRow {
                target_qty: Some(1),
                item_drop_vc: Some(TREASURE_GATHERABLE.to_owned()),
                item_drop_probability: Some(100),
                ..with_item("Treasure", "TaskGatherAndReturnItems", TREASURE)
            },
            TaskRow {
                override_location: Some("755.87 342.49 20.90".to_owned()),
                trigger_area_type: Some("Leave".to_owned()),
                ..row("LeaveTrigger", "TaskTriggerArea")
            },
            poi("ExitWreck", "TaskExitPOITag"),
        ]
    }
    /// The root of [`wreck_rows`].
    pub(crate) const WRECK_ROOT: &str = "12_pillageship_container";
    /// The point of interest of [`wreck_rows`] (retail tag of territory 30129).
    pub(crate) const WRECK_POI: &str = "drowned_shipwreck_3_30129";
    /// The lore note of the ship's manifest.
    pub(crate) const MANIFEST_LORE: &str = "Lore_unlock_00_Aeternum_Title_31_01";
    /// The whisper event of Amidst the Wreckage.
    pub(crate) const WHISPER_EVENT: &str = "01_IsabellasFirstWhisper_Isabella_01_event";
    /// The game event of Grace's nodules.
    pub(crate) const NODULE_EVENT: &str = "MSQ_CorruptedNodule_1_interact";
    /// A treasure of Lessons of a Pirate's Life and its quest object.
    pub(crate) const TREASURE: &str = "0601_0104_Treasure_A";
    pub(crate) const TREASURE_GATHERABLE: &str = "MSQ_Treasure_A";

    #[test]
    fn the_wreck_steps_run_on_places_lore_events_and_gathers() {
        let tree = Arc::new(ObjectiveTree::build(&wreck_rows(), WRECK_ROOT).expect("tree"));
        assert_eq!(tree.returned_items(), [(TREASURE.to_owned(), 1)]);
        let (mut run, _) = ObjectiveRun::start(tree, NOW);
        let nothing: [(&str, u32, bool); 0] = [];
        let sync = |run: &mut ObjectiveRun, places: &[&str], lore: bool| {
            let achieved = |id: &str| lore && id == MANIFEST_LORE;
            run.sync(
                &Facts {
                    held: &nothing,
                    places: Some(places),
                    achieved: Some(&achieved),
                    level: None,
                    territories: None,
                    of_family: None,
                    houses: None,
                },
                NOW,
            )
        };
        run.sync_held(&nothing, NOW);
        assert!(run.is_active("GoToWreck"), "unknown places judge nothing");
        sync(&mut run, &["watchtower"], false);
        assert!(run.is_active("GoToWreck"), "another point of interest");
        sync(&mut run, &[WRECK_POI], false);
        assert!(run.is_done("GoToWreck"));
        // The manifest read before the step counts once the step waits.
        let effects = sync(&mut run, &[WRECK_POI], true);
        assert!(run.is_done("ShipsManifest"));
        assert!(
            effects.contains(&Effect::GameEvent(WHISPER_EVENT.to_owned())),
            "the whisper fires and is done: {effects:?}"
        );
        assert!(run.is_done("Whisper"));
        assert!(run.counts_game_event(NODULE_EVENT));
        assert!(run.gather_drops(TREASURE_GATHERABLE).is_empty(), "not yet");
        run.event(Event::GameEvent(NODULE_EVENT), NOW);
        run.event(Event::GameEvent("other_interact"), NOW);
        assert!(run.is_active("Nodules"));
        run.event(Event::GameEvent("msq_corruptednodule_1_interact"), NOW);
        assert!(run.is_done("Nodules"), "two nodules");
        assert!(!run.counts_game_event(NODULE_EVENT));
        assert_eq!(
            run.gather_drops(TREASURE_GATHERABLE),
            [(TREASURE.to_owned(), 100)]
        );
        assert!(run.gather_drops("MSQ_Treasure_B").is_empty());
        run.event(
            Event::ItemGained {
                item: TREASURE,
                quantity: 1,
            },
            NOW,
        );
        assert!(run.is_done("Treasure"));
        run.event(Event::Moved(AREA), NOW);
        assert!(run.is_active("LeaveTrigger"), "in the volume");
        run.event(
            Event::Moved(Position {
                x: AREA.x + TRIGGER_RADIUS_M + 1.0,
                ..AREA
            }),
            NOW,
        );
        assert!(run.is_done("LeaveTrigger"), "out of it");
        sync(&mut run, &[WRECK_POI], true);
        assert!(!run.finished(), "still at the wreck");
        sync(&mut run, &[], true);
        assert!(run.finished(), "left the wreck");
    }

    #[test]
    fn a_place_or_event_task_without_its_cell_is_empty() {
        for kind in ["TaskGoToPOITag", "TaskExitPOITag", "TaskTriggerGameEvent"] {
            assert_eq!(
                kind_of(&row("Empty", kind)),
                TaskKind::Empty,
                "{kind}"
            );
        }
        assert_eq!(
            kind_of(&row("Pin", "TaskGameEvent")),
            TaskKind::Never,
            "a map pin beside the real step of a partial container never ends"
        );
        assert_eq!(
            counted_game_events(&wreck_rows()),
            HashSet::from([NODULE_EVENT.to_lowercase()])
        );
    }

    #[test]
    fn a_location_is_three_numbers() {
        assert_eq!(location_of("755.87 342.49 20.90"), Some(AREA));
        assert_eq!(location_of("1 2"), None);
        assert_eq!(location_of("1 2 3 4"), None);
    }
}
