// SPDX-License-Identifier: AGPL-3.0-or-later
//! The AI data file (`ai-data-<build>.bin`, written by `content-tool ai-data`): every Server.pak
//! behavior tree as one shared node arena, the AI part of every creature slice, and the CAGE task
//! resolution of every creature grid (behavior-tree study, section 5.1).
//!
//! The file is a content container (magic `NWAI`) with a postcard body, as the content bundle.

use std::path::{Path, PathBuf};

use nw_content::container::{
    ContainerError, ContainerSpec, decode_value, encode_value, read_file, write_file,
};
use nw_content::player_attacks::Flight;
use serde::{Deserialize, Serialize};

use crate::node::{Node, NodeId, SelectPositionParams};
use crate::value::{KeyId, NameId, Value};

/// The container header of the file.
pub const AI_DATA_SPEC: ContainerSpec = ContainerSpec {
    magic: *b"NWAI",
    format_version: 1,
    schema_hash: SCHEMA_VERSION,
};
/// Raised when the body layout changes.
const SCHEMA_VERSION: u32 = 4;
const FILE_PREFIX: &str = "ai-data-";
const FILE_SUFFIX: &str = ".bin";

/// The path of the file of `client_build` in `dir`.
pub fn ai_data_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!("{FILE_PREFIX}{client_build}{FILE_SUFFIX}"))
}

/// One `.behaviortree` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeAsset {
    /// Its lower-case pak path.
    pub path: NameId,
    /// Its root node.
    pub root: Option<NodeId>,
    /// Nodes with every sub-tree copy counted (the client's inlined form).
    pub expanded_nodes: u32,
}

/// A tree a slice names, and the Server.pak tree when the pak has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeRef {
    /// The lower-case path in the slice.
    pub path: NameId,
    /// Index in [`AiData::trees`]; `None`: Server.pak has no such tree (study section 4.1).
    pub tree: Option<u32>,
}

/// A vision profile: cone angle and range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vision {
    /// `m_visionAngle` (degrees; -1: none).
    pub angle_deg: f32,
    /// `m_visionDistance` (metres; -1: none).
    pub distance_m: f32,
}

/// One `m_navConfigs` entry of the slice navigation profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavConfig {
    /// `m_navConfigType`.
    pub kind: NameId,
    /// `m_maxSpeed` (m/s).
    pub max_speed: f32,
    /// `m_minSpeed` (m/s).
    pub min_speed: f32,
    /// `m_turnRateDegs`.
    pub turn_rate_deg: f32,
    /// `m_linearAcceleration`.
    pub linear_acceleration: f32,
    /// `m_linearDeceleration`.
    pub linear_deceleration: f32,
    /// `m_arrivalThreshold` (metres).
    pub arrival_threshold: f32,
    /// `m_targetRepathDistance` (metres).
    pub target_repath_distance: f32,
    /// `m_selectPositionParams`: how the destination is picked around the goal (the boar's
    /// `RunToTarget`: a point 1.5 to 2 m from the target, 300 to 60 degrees).
    pub select_position: Option<SelectPositionParams>,
    /// `m_teleport`.
    pub teleport: bool,
    /// `m_turnActionName`: the CAGE task it turns with.
    pub turn_action: Option<NameId>,
    /// `m_startActionName`: the CAGE task it starts with.
    pub start_action: Option<NameId>,
    /// `m_navStartRule`.
    pub start_rule: i64,
    /// The fields of `m_navActionParams` and `m_strafeActionParams` (steering, avoidance,
    /// separation, strafe distances), by the lower-case CRC32 of the field name.
    pub action_params: Vec<(u32, Value)>,
}

/// The AI of one creature slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Archetype {
    /// The slice (lower-case pak path).
    pub slice: NameId,
    /// `BehaviorTreeComponent.m_behaviorTreeAssets`.
    pub trees: Vec<TreeRef>,
    /// `m_configParams`: per-mob blackboard defaults.
    pub config: Vec<(KeyId, Value)>,
    /// `m_autoEvaluateBlackboardVariables`.
    pub auto_evaluate: Vec<KeyId>,
    /// `m_autoEvaluationFrequency` (seconds).
    pub auto_evaluation_s: Option<f32>,
    /// `m_ambientVisionProfile`.
    pub ambient_vision: Option<Vision>,
    /// `m_combatVisionProfile`.
    pub combat_vision: Option<Vision>,
    /// `m_groupAggroDistance` (metres).
    pub group_aggro_distance: Option<f32>,
    /// `m_groupAggroShareCap`.
    pub group_aggro_cap: Option<u32>,
    /// `m_targetingProfile` (`.curveconfiglist`).
    pub targeting_profile: Option<NameId>,
    /// `m_aggroDelayOverrideSec` when `m_overrideAggroDelay` is set.
    pub aggro_delay_override_s: Option<f32>,
    /// `m_navConfigs`.
    pub nav_configs: Vec<NavConfig>,
    /// `m_maxSlope` (degrees).
    pub max_slope_deg: Option<f32>,
    /// The CAGE grid, by index in [`AiData::grids`].
    pub grid: Option<u32>,
    /// `m_animationDatabase` (`.adb`).
    pub animation_database: Option<NameId>,
    /// `m_damageTable`.
    pub damage_table: Option<NameId>,
    /// The unique id of that damage table's datasheet (its `#DamageData` header, for example
    /// `BoarDamageTable`): the name the combat rows know the table by.
    pub damage_table_id: Option<NameId>,
    /// The item of the slice paperdoll's main hand (`MAIN_HAND_OPTION_1`), whose weapon row
    /// gives a blocked hit's `BlockStaminaDamage`.
    pub main_hand_item: Option<NameId>,
    /// The Mannequin tags the creature always has (slice default tags and main-hand weapon
    /// tags, as the creature animation table).
    pub default_tags: Vec<NameId>,
}

impl Archetype {
    /// The index of navigation config `kind` in [`Archetype::nav_configs`].
    pub fn nav_config(&self, kind: NameId) -> Option<usize> {
        self.nav_configs
            .iter()
            .position(|config| config.kind == kind)
    }

    /// The config default of `key`.
    pub fn config_value(&self, key: KeyId) -> Option<Value> {
        self.config
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, value)| *value)
    }
}

/// A CAGE action list condition (the action list's `<Conditions>` and `Conditional` names).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CageCondition {
    /// `<And>`.
    All(Vec<CageCondition>),
    /// `<Or>`.
    Any(Vec<CageCondition>),
    /// `<Not>`.
    Not(Box<CageCondition>),
    /// `<True/>`.
    True,
    /// `<IfMannequinTag>`.
    Tag(NameId),
    /// `<IfMannequinMarker>`.
    Marker(NameId),
    /// Any other test, by element name: false for the runtime (it holds no input, ability or
    /// status state).
    Other(NameId),
}

impl CageCondition {
    /// Whether it holds for a mob that has the tags `has_tag` says it has.
    pub fn holds(&self, has_tag: &impl Fn(NameId) -> bool) -> bool {
        match self {
            Self::All(parts) => parts.iter().all(|part| part.holds(has_tag)),
            Self::Any(parts) => parts.iter().any(|part| part.holds(has_tag)),
            Self::Not(part) => !part.holds(has_tag),
            Self::True => true,
            Self::Tag(tag) => has_tag(*tag),
            Self::Marker(_) | Self::Other(_) => false,
        }
    }
}

/// When a CAGE action tells the tree that it is done: `HTNPlanDoneActivity` or
/// `BehaviorTreeTask` `CompletionFragmentOffset` (study section 3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    /// `Default CompletionFragmentOffset`: seconds before the fragment's end.
    pub default_offset_s: f32,
    /// `Conditional` entries: the named condition and its offset, in file order.
    pub conditional: Vec<(CageCondition, f32)>,
}

/// The `Default` of a `SetMannequinTag` activity.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TagActivity {
    /// `TagName`.
    pub tag: NameId,
    /// `OnEnter`.
    pub on_enter: TagEffect,
    /// `OnExit`.
    pub on_exit: TagEffect,
    /// `DelayTimer` (seconds).
    pub delay_s: f32,
}

/// `OnEnter` / `OnExit` of a `SetMannequinTag` activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagEffect {
    /// `Set`.
    Set,
    /// `Clear`.
    Clear,
    /// `NoEffect`.
    NoEffect,
}

/// `SetDesiredFacingBlendParam` `FacingMode` (Observed values in the client action lists:
/// `AngleToTarget` 93 times, `AngleToDestination` 2 times).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FacingMode {
    /// `AngleToTarget`: toward the target (the turn in place, `baseai_turninplace.actionlist`).
    AngleToTarget,
    /// `AngleToDestination`: toward where it moves.
    AngleToDestination,
}

impl FacingMode {
    /// The mode of a `FacingMode` attribute.
    pub fn of(text: &str) -> Option<Self> {
        match text {
            "AngleToTarget" => Some(Self::AngleToTarget),
            "AngleToDestination" => Some(Self::AngleToDestination),
            _ => None,
        }
    }
}

/// How long a fragment entry plays.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FragmentLength {
    /// It ends after this many seconds.
    Seconds(f32),
    /// Its last clip loops: it has no end of its own.
    Loops,
    /// Unknown: a clip whose length could not be read (the reason is in the build report).
    Unknown,
}

/// A procedural clip of a fragment entry (`CAGE-Damage`, `Homing`): when it runs (seconds after
/// the fragment starts) and its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcWindow {
    /// The clip type.
    pub kind: NameId,
    /// Its start.
    pub start_s: f32,
    /// Its end; `None`: to the fragment's end.
    pub end_s: Option<f32>,
    /// `<ProceduralParams>` element names and values.
    pub params: Vec<(NameId, NameId)>,
}

/// One entry of a fragment in the creature's animation database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FragmentVariant {
    /// `Tags`: Mannequin chooses the entry whose tags the mob has, the most tags first.
    pub tags: Vec<NameId>,
    /// `FragTags`.
    pub frag_tags: Vec<NameId>,
    /// How long it plays.
    pub length: FragmentLength,
    /// Its procedural clips (`CAGE-Damage`, `CAGE-RangedAttack`, `Homing`, ...).
    pub windows: Vec<ProcWindow>,
}

/// One CAGE action: the grid action list `Action` whose `IfHTNCAGEAction` or
/// `IfBehaviorTreeTask` names a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CageAction {
    /// The action list (lower-case pak path).
    pub list: NameId,
    /// `Name`.
    pub action: NameId,
    /// `FragmentName`.
    pub fragment: NameId,
    /// Its completion; `None`: the action has no `HTNPlanDoneActivity` or `BehaviorTreeTask`.
    pub completion: Option<Completion>,
    /// Its `SetMannequinTag` activities.
    pub tags: Vec<TagActivity>,
    /// `AnimationDrivenMotion Default MotionMultiplier`.
    pub motion_multiplier: Option<f32>,
    /// `SetDesiredFacingBlendParam Default FacingMode`: where the action turns the body.
    pub facing: Option<FacingMode>,
    /// The fragment's entries in the creature's animation database.
    pub variants: Vec<FragmentVariant>,
    /// The fragment's entries in its main-hand weapon's animation database, which the weapon's
    /// `WeaponContext` scope plays beside the body (Observed: the `CAGE-Damage` clips of the
    /// damned axe attacks are only in `npc_damned_2h_axeanims.adb`, the weapon rows'
    /// `AnimDbPath`).
    pub weapon_variants: Vec<FragmentVariant>,
}

/// The entry of `variants` Mannequin plays for a mob with the tags `has_tag` says it has: of the
/// entries without fragment tags whose every tag the mob has, the one with the most tags, the
/// first on a tie (as `nw_formats::mannequin::AnimDatabase::best`).
fn best_variant<'a>(
    variants: &'a [FragmentVariant],
    has_tag: &impl Fn(NameId) -> bool,
) -> Option<&'a FragmentVariant> {
    variants
        .iter()
        .filter(|variant| variant.frag_tags.is_empty() && variant.tags.iter().all(|t| has_tag(*t)))
        .fold(None, |best: Option<&FragmentVariant>, variant| match best {
            Some(best) if best.tags.len() >= variant.tags.len() => Some(best),
            _ => Some(variant),
        })
}

impl CageAction {
    /// The entry of the creature's database Mannequin plays for a mob with the tags `has_tag`
    /// says it has ([`best_variant`]).
    pub fn variant(&self, has_tag: &impl Fn(NameId) -> bool) -> Option<&FragmentVariant> {
        best_variant(&self.variants, has_tag)
    }

    /// The entry of the weapon's database its `WeaponContext` scope plays ([`best_variant`]).
    pub fn weapon_variant(&self, has_tag: &impl Fn(NameId) -> bool) -> Option<&FragmentVariant> {
        best_variant(&self.weapon_variants, has_tag)
    }

    /// Seconds from the start until the tree is told the action is done: the fragment length
    /// minus the completion offset of the first conditional entry that holds, else the default
    /// (Inferred: retail boar `Attack_Gore` entries lasted 1,465 and 1,498 ms; 46 frames at 30
    /// fps / 0.85 speed - 0.4 s offset = 1,404 ms, plus one or two 33 ms server ticks). `None`
    /// when the action has no completion, the fragment loops or its length is Unknown.
    pub fn done_after_s(&self, has_tag: &impl Fn(NameId) -> bool) -> Option<f32> {
        let completion = self.completion.as_ref()?;
        let FragmentLength::Seconds(length) = self.variant(has_tag)?.length else {
            return None;
        };
        let offset = completion
            .conditional
            .iter()
            .find(|(condition, _)| condition.holds(has_tag))
            .map_or(completion.default_offset_s, |(_, offset)| *offset);
        Some((length - offset).max(0.0))
    }
}

/// A CAGE grid with the animation database its creatures play it with: its tasks, sorted by
/// task name id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    /// The grid (lower-case pak path).
    pub path: NameId,
    /// The animation database (lower-case pak path) its fragments come from.
    pub animation_database: Option<NameId>,
    /// Task name and index in [`AiData::cage_actions`], sorted by task.
    pub tasks: Vec<(NameId, u32)>,
    /// Every action of its action lists, with or without a task (the engine starts the
    /// task-less ones itself: `Reaction`, `Death`, ...), by action name, sorted by name.
    pub actions: Vec<(NameId, u32)>,
    /// The grid alias of each action name (`<AliasMap><Action ActionName Alias>`: `Reaction`,
    /// `Death`, `BehaviorTreeTask`, `Idle_No_Task`, ...), sorted by action name.
    pub aliases: Vec<(NameId, NameId)>,
}

impl Grid {
    /// The action of `task`.
    pub fn action(&self, task: NameId) -> Option<u32> {
        self.tasks
            .binary_search_by_key(&task, |(known, _)| *known)
            .ok()
            .map(|index| self.tasks[index].1)
    }

    /// The grid alias of action `name`.
    pub fn alias(&self, name: NameId) -> Option<NameId> {
        self.aliases
            .binary_search_by_key(&name, |(known, _)| *known)
            .ok()
            .map(|index| self.aliases[index].1)
    }

    /// The action named `name` (the first list that has it).
    pub fn named(&self, name: NameId) -> Option<u32> {
        self.actions
            .binary_search_by_key(&name, |(known, _)| *known)
            .ok()
            .map(|index| self.actions[index].1)
    }
}

/// One attack of the ranged attack database (`rangedattackdatabase.radb`): a `CAGE-RangedAttack`
/// clip's `RangedAttackName` in the ranged profile of the mob's main-hand weapon
/// (`RangedAttackProfile`; Observed: `Corrupted_First_Mate_2hAnchor` is `AI_Undead`, whose
/// `Officer_Anchor` Janszoon's toss names).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RangedAttack {
    /// The profile.
    pub profile: NameId,
    /// The attack name.
    pub attack: NameId,
    /// `m_maxRange`.
    pub range_m: f32,
    /// `m_projectileRadius`.
    pub radius_m: f32,
    /// `m_isHitScan`.
    pub hit_scan: bool,
}

/// How the projectile of a slice a mob's `CAGE-RangedAttack` clip names (`SliceOverride`) flies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProjectileSlice {
    /// The slice (lower-case pak path).
    pub slice: NameId,
    /// Its projectile component.
    pub flight: Flight,
}

/// A creature slice asset and its archetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creature {
    /// The slice asset GUID (the spawn card's asset id).
    pub asset: [u8; 16],
    /// Index in [`AiData::archetypes`].
    pub archetype: u32,
}

/// The whole AI data file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AiData {
    /// Every string the data names.
    pub names: Vec<String>,
    /// The blackboard keys: key `k` is named `names[keys[k]]`.
    pub keys: Vec<NameId>,
    /// The shared node arena (identical sub-trees are stored once).
    pub nodes: Vec<Node>,
    /// The child lists of the composites.
    pub child_ids: Vec<NodeId>,
    /// Every Server.pak tree, sorted by path.
    pub trees: Vec<TreeAsset>,
    /// Every creature slice of the mob prefab table.
    pub archetypes: Vec<Archetype>,
    /// Every CAGE grid the archetypes use.
    pub grids: Vec<Grid>,
    /// Every resolved CAGE action.
    pub cage_actions: Vec<CageAction>,
    /// Creature slice assets, sorted by asset.
    pub creatures: Vec<Creature>,
    /// Every attack of the ranged attack database.
    pub ranged_attacks: Vec<RangedAttack>,
    /// The projectiles of the slices the creatures' ranged clips name.
    pub projectile_slices: Vec<ProjectileSlice>,
}

impl AiData {
    /// The string `id`.
    pub fn name(&self, id: NameId) -> &str {
        self.names.get(id as usize).map_or("", String::as_str)
    }

    /// The id of string `text`, when the data has it.
    pub fn name_id(&self, text: &str) -> Option<NameId> {
        self.names
            .iter()
            .position(|known| known == text)
            .and_then(|index| NameId::try_from(index).ok())
    }

    /// The key named `text`, when the data has it.
    pub fn key(&self, text: &str) -> Option<KeyId> {
        let name = self.name_id(text)?;
        self.keys
            .iter()
            .position(|known| *known == name)
            .and_then(|index| KeyId::try_from(index).ok())
    }

    /// The name of key `key`.
    pub fn key_name(&self, key: KeyId) -> &str {
        self.keys
            .get(usize::from(key))
            .map_or("", |name| self.name(*name))
    }

    /// Node `id`.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// The children of composite `id`.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.node(id).kind.children().map_or(&[], |children| {
            let first = children.first as usize;
            &self.child_ids[first..first + children.count as usize]
        })
    }

    /// The tree at `path` (lower case).
    pub fn tree(&self, path: &str) -> Option<&TreeAsset> {
        let id = self.name_id(path)?;
        self.trees.iter().find(|tree| tree.path == id)
    }

    /// The archetype of the creature slice asset `asset`.
    pub fn creature(&self, asset: &[u8; 16]) -> Option<&Archetype> {
        let index = self
            .creatures
            .binary_search_by(|creature| creature.asset.cmp(asset))
            .ok()?;
        self.archetypes
            .get(self.creatures[index].archetype as usize)
    }

    /// The archetype of the slice `slice` (lower-case pak path).
    pub fn archetype(&self, slice: &str) -> Option<&Archetype> {
        let id = self.name_id(slice)?;
        self.archetypes
            .iter()
            .find(|archetype| archetype.slice == id)
    }

    /// The CAGE action of `task` in the grid of `archetype`.
    pub fn cage_action(&self, archetype: &Archetype, task: NameId) -> Option<&CageAction> {
        let grid = self.grids.get(archetype.grid? as usize)?;
        self.cage_actions.get(grid.action(task)? as usize)
    }

    /// The attack `attack` of the ranged profile `profile`.
    pub fn ranged_attack(&self, profile: &str, attack: &str) -> Option<&RangedAttack> {
        self.ranged_attacks
            .iter()
            .find(|known| self.name(known.profile) == profile && self.name(known.attack) == attack)
    }

    /// How the projectile of the slice `slice` (lower-case pak path) flies.
    pub fn projectile_flight(&self, slice: &str) -> Option<Flight> {
        self.projectile_slices
            .iter()
            .find(|known| self.name(known.slice) == slice)
            .map(|known| known.flight)
    }

    /// Reads the file of `client_build` from `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError`] when the file is missing, from another build or bad.
    pub fn load(dir: &Path, client_build: &str) -> Result<Self, ContainerError> {
        let bytes = read_file(&ai_data_path(dir, client_build))?;
        decode_value(&AI_DATA_SPEC, &bytes, client_build)
    }

    /// Writes the file of `client_build` into `dir` and returns its path.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError`] when the data cannot be encoded or the file written.
    pub fn save(&self, dir: &Path, client_build: &str) -> Result<PathBuf, ContainerError> {
        let path = ai_data_path(dir, client_build);
        write_file(&path, &encode_value(&AI_DATA_SPEC, client_build, self)?)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GORE_FRAMES_S: f32 = 46.0 / 30.0;
    const GORE_SPEED: f32 = 0.85;
    const GORE_OFFSET_S: f32 = 0.4;
    const CHAINED_OFFSET_S: f32 = 0.8;
    const CHAIN_TAG: NameId = 7;
    const ARMADILLO_TAG: NameId = 8;

    fn gore() -> CageAction {
        CageAction {
            list: 0,
            action: 1,
            fragment: 1,
            completion: Some(Completion {
                default_offset_s: GORE_OFFSET_S,
                conditional: vec![(CageCondition::Tag(CHAIN_TAG), CHAINED_OFFSET_S)],
            }),
            tags: Vec::new(),
            motion_multiplier: Some(1.0),
            facing: None,
            variants: vec![
                FragmentVariant {
                    tags: vec![ARMADILLO_TAG],
                    frag_tags: Vec::new(),
                    length: FragmentLength::Loops,
                    windows: Vec::new(),
                },
                FragmentVariant {
                    tags: Vec::new(),
                    frag_tags: Vec::new(),
                    length: FragmentLength::Seconds(GORE_FRAMES_S / GORE_SPEED),
                    windows: Vec::new(),
                },
            ],
            weapon_variants: Vec::new(),
        }
    }

    #[test]
    fn the_gore_is_done_its_offset_before_the_fragment_end() {
        let plain = gore().done_after_s(&|_| false).expect("done");
        assert!((plain - (GORE_FRAMES_S / GORE_SPEED - GORE_OFFSET_S)).abs() < 1e-6);
        let chained = gore().done_after_s(&|tag| tag == CHAIN_TAG).expect("done");
        assert!((chained - (GORE_FRAMES_S / GORE_SPEED - CHAINED_OFFSET_S)).abs() < 1e-6);
        assert_eq!(
            gore().done_after_s(&|tag| tag == ARMADILLO_TAG),
            None,
            "loops"
        );
    }

    #[test]
    fn conditions_hold_by_tags_only() {
        let has = |tag: NameId| tag == CHAIN_TAG;
        assert!(
            CageCondition::Any(vec![CageCondition::Other(0), CageCondition::Tag(CHAIN_TAG)])
                .holds(&has)
        );
        assert!(
            !CageCondition::All(vec![CageCondition::True, CageCondition::Marker(1)]).holds(&has)
        );
        assert!(CageCondition::Not(Box::new(CageCondition::Tag(ARMADILLO_TAG))).holds(&has));
    }

    #[test]
    fn a_creature_finds_its_archetype_grid_and_actions() {
        let gore_task = 1;
        let reaction = 2;
        let alias = 3;
        let data = AiData {
            names: vec![
                "slice".into(),
                "Gore_Task".into(),
                "Reaction".into(),
                "Reaction".into(),
            ],
            archetypes: vec![Archetype {
                slice: 0,
                trees: Vec::new(),
                config: vec![(0, Value::Float(70.0))],
                auto_evaluate: Vec::new(),
                auto_evaluation_s: None,
                ambient_vision: None,
                combat_vision: None,
                group_aggro_distance: None,
                group_aggro_cap: None,
                targeting_profile: None,
                aggro_delay_override_s: None,
                nav_configs: Vec::new(),
                max_slope_deg: None,
                grid: Some(0),
                animation_database: None,
                damage_table: None,
                damage_table_id: None,
                main_hand_item: None,
                default_tags: Vec::new(),
            }],
            grids: vec![Grid {
                path: 0,
                animation_database: None,
                tasks: vec![(gore_task, 0)],
                actions: vec![(reaction, 0)],
                aliases: vec![(reaction, alias)],
            }],
            cage_actions: vec![gore()],
            creatures: vec![Creature {
                asset: [7; 16],
                archetype: 0,
            }],
            ..AiData::default()
        };
        let arch = data.creature(&[7; 16]).expect("creature");
        assert_eq!(data.archetype("slice"), Some(arch));
        assert!(data.creature(&[8; 16]).is_none());
        assert_eq!(
            data.cage_action(arch, gore_task),
            Some(&data.cage_actions[0])
        );
        assert_eq!(data.grids[0].named(reaction), Some(0));
        assert_eq!(data.grids[0].alias(reaction), Some(alias));
        assert_eq!(arch.config_value(0), Some(Value::Float(70.0)));
        assert_eq!(arch.nav_config(0), None);
    }

    #[test]
    fn the_file_round_trips() {
        let dir = tempfile::tempdir().expect("dir");
        let dir = dir.path();
        let data = AiData {
            names: vec!["a".into(), "b".into()],
            keys: vec![1],
            cage_actions: vec![gore()],
            grids: vec![Grid {
                path: 0,
                animation_database: None,
                tasks: vec![(1, 0)],
                actions: vec![(1, 0)],
                aliases: vec![(1, 0)],
            }],
            ..AiData::default()
        };
        data.save(dir, "1").expect("save");
        assert_eq!(AiData::load(dir, "1").expect("load"), data);
        assert_eq!(data.key("b"), Some(0));
        assert_eq!(data.key_name(0), "b");
        assert!(AiData::load(dir, "2").is_err(), "other build");
    }
}
