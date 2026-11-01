// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ai-data`: builds the AI data file (`ai-data-<build>.bin`, read by `nw_ai::data::AiData`)
//! from Server.pak and the game paks (behavior-tree study 2026-09-18, sections 2.4, 3 and 5.1).
//!
//! - Every Server.pak `.behaviortree` goes into one shared node arena (`nw_ai::decode`).
//! - Every creature slice of the creature animation table gives an archetype: its
//!   `BehaviorTreeComponent.m_behaviorTreeAssets`, `m_configParams`, auto-evaluated keys,
//!   vision and aggro values, targeting profile, navigation configs (`m_navConfigs`), body slope,
//!   CAGE grid, animation database, damage table (and its datasheet's unique id) and main-hand
//!   item (field names Observed: each field CRC is the
//!   lower-case CRC32 of the name, study `fieldnames-all.tsv`; `m_actionGridNew` is the
//!   `0x0d5832e9` field of `creature_animations`).
//! - Every grid action list `Action` whose `IfHTNCAGEAction` or `IfBehaviorTreeTask` names a task
//!   becomes a CAGE action: fragment, completion offsets, `SetMannequinTag` activities, motion
//!   multiplier, and the fragment's entries in the creature's animation database with their
//!   length and procedural clips (`CAGE-Damage`, `Homing`, ...).
//! - A fragment's length comes from its base animation layer: each clip plays its `.caf` length
//!   (`MotionParams` chunk: frames x ticks per frame x seconds per tick) from its blend
//!   `StartTime`, divided by its `speed`; a blend space plays the length its example clips share.
//!   A length that cannot be read is Unknown and listed in the report; nothing is filled in.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use nw_ai::data::{
    AiData, Archetype, CageAction, CageCondition, Completion, Creature, FacingMode, FragmentLength,
    FragmentVariant, Grid, NavConfig, ProcWindow, ProjectileSlice, RangedAttack, TagActivity,
    TagEffect, TreeRef, Vision,
};
use nw_ai::decode::{DecodeError, ServerPakError, TreeDecoder, decode_entries, server_pak_entries};
use nw_ai::node::NodeId;
use nw_ai::value::{NameId, Param, Value};
use nw_content::creature_animations::CreatureRow;
use nw_formats::azcs::{decode_azcs, is_azcs};
use nw_formats::datasheet::Datasheet;
use nw_formats::mannequin::{
    ActionList, AnimDatabase, CageGrid, Condition, GridList, MannequinError, Node as XmlNode,
    parse_tree,
};
use nw_formats::objectstream::{Element, lowercase_crc32, parse_object_stream};
use nw_formats::pak::{PakArchive, PakError, find_paks};
use nw_oodle::KrakenDecompressor;

use crate::creature_animations::{main_hand_item, read_entries, text_of};
use crate::error::ToolError;
use crate::player_attacks::{RANGED_ATTACK_DATABASE, RANGED_CLIP, ranged_attacks, slice_flight};
use crate::source::GameSource;
use crate::stand_in_trees::{StandInError, stand_in_trees};

/// Where the user keeps the retrieved Server.pak (server-pak note).
pub const DEFAULT_SERVER_PAK: &str = "local-data/server-pak/Server.pak";
/// The `CAGE-RangedAttack` parameter that names the projectile slice.
const SLICE_OVERRIDE_PARAM: &str = "SliceOverride";

/// Slice fields (Observed names).
const TREE_ASSETS: &str = "m_behaviorTreeAssets";
const ASSET_REF: &str = "m_assetRef";
const CONFIG_PARAMS: &str = "m_configParams";
const CONFIG_NAME: &str = "m_name";
const CONFIG_VALUE: &str = "m_value";
const AUTO_EVALUATE: &str = "m_autoEvaluateBlackboardVariables";
const AUTO_EVALUATION_FREQUENCY: &str = "m_autoEvaluationFrequency";
const AMBIENT_VISION: &str = "m_ambientVisionProfile";
const COMBAT_VISION: &str = "m_combatVisionProfile";
const VISION_ANGLE: &str = "m_visionAngle";
const VISION_DISTANCE: &str = "m_visionDistance";
const GROUP_AGGRO_DISTANCE: &str = "m_groupAggroDistance";
const GROUP_AGGRO_CAP: &str = "m_groupAggroShareCap";
const TARGETING_PROFILE: &str = "m_targetingProfile";
const OVERRIDE_AGGRO_DELAY: &str = "m_overrideAggroDelay";
const AGGRO_DELAY: &str = "m_aggroDelayOverrideSec";
const NAV_CONFIGS: &str = "m_navConfigs";
const NAV_CONFIG_TYPE: &str = "m_navConfigType";
const NAV_MAX_SPEED: &str = "m_maxSpeed";
const NAV_MIN_SPEED: &str = "m_minSpeed";
const NAV_TURN_RATE: &str = "m_turnRateDegs";
const NAV_ACCELERATION: &str = "m_linearAcceleration";
const NAV_DECELERATION: &str = "m_linearDeceleration";
const NAV_ARRIVAL: &str = "m_arrivalThreshold";
const NAV_REPATH: &str = "m_targetRepathDistance";
const NAV_SELECT_POSITION: &str = "m_selectPositionParams";
const NAV_ACTION_PARAMS: &str = "m_navActionParams";
const NAV_STRAFE_PARAMS: &str = "m_strafeActionParams";
const NAV_TELEPORT: &str = "m_teleport";
const NAV_TURN_ACTION: &str = "m_turnActionName";
const NAV_START_ACTION: &str = "m_startActionName";
const NAV_START_RULE: &str = "m_navStartRule";
const MAX_SLOPE: &str = "m_maxSlope";
const ACTION_GRID: &str = "m_actionGridNew";
const ANIMATION_DATABASE: &str = "m_animationDatabase";
const DAMAGE_TABLE: &str = "m_damageTable";
const ASSET_PATH: &str = "AssetPath";
const BASE_CLASS: &str = "BaseClass1";
const DAMAGE_TABLE_ASSET: &str = "Asset";
/// The file endings of a damage table in the slice and in the paks.
const DAMAGE_TABLE_SUFFIX: &str = ".xml";
const DATASHEET_SUFFIX: &str = ".datasheet";

/// Grid and action list XML (Observed in `predator/cage/boar/boar.grid`, `boar_combat.actionlist`).
const ACTION_ELEMENT: &str = "Action";
const NAME_ATTRIBUTE: &str = "Name";
const FRAGMENT_NAME_ATTRIBUTE: &str = "FragmentName";
const ACTIVATION_ELEMENT: &str = "ActivationCondition";
const TASK_TESTS: [&str; 2] = ["IfHTNCAGEAction", "IfBehaviorTreeTask"];
const TASK_ATTRIBUTE: &str = "ActionName";
const COMPLETION_ACTIVITIES: [&str; 2] = ["HTNPlanDoneActivity", "BehaviorTreeTask"];
const DEFAULT_ELEMENT: &str = "Default";
const CONDITIONAL_ELEMENT: &str = "Conditional";
const COMPLETION_OFFSET: &str = "CompletionFragmentOffset";
const SET_TAG_ACTIVITY: &str = "SetMannequinTag";
const TAG_NAME_ATTRIBUTE: &str = "TagName";
const ON_ENTER_ATTRIBUTE: &str = "OnEnter";
const ON_EXIT_ATTRIBUTE: &str = "OnExit";
const DELAY_ATTRIBUTE: &str = "DelayTimer";
const TAG_SET: &str = "Set";
const TAG_CLEAR: &str = "Clear";
const TAG_NO_EFFECT: &str = "NoEffect";
const MOTION_ACTIVITY: &str = "AnimationDrivenMotion";
const MOTION_MULTIPLIER: &str = "MotionMultiplier";
/// The facing activity (Observed in `baseai_turninplace.actionlist`).
const FACING_ACTIVITY: &str = "SetDesiredFacingBlendParam";
const FACING_MODE: &str = "FacingMode";

/// Animation database XML (Observed in `npc_boar_anims.adb`).
const FRAGMENT_LIST: &str = "FragmentList";
const FRAGMENT_ELEMENT: &str = "Fragment";
const ANIM_LAYER: &str = "AnimLayer";
const BLEND_ELEMENT: &str = "Blend";
const ANIMATION_ELEMENT: &str = "Animation";
const EXIT_TIME: &str = "ExitTime";
const START_TIME: &str = "StartTime";
const ANIMATION_NAME: &str = "name";
const ANIMATION_SPEED: &str = "speed";
const ANIMATION_FLAGS: &str = "flags";
const LOOP_FLAG: &str = "Loop";
/// The playback speed of a clip without `speed`.
const DEFAULT_SPEED: f64 = 1.0;

/// Animation files by extension.
const CAF_EXTENSION: &str = ".caf";
const BLEND_SPACE_EXTENSION: &str = ".bspace";
const COMBINED_BLEND_SPACE_EXTENSION: &str = ".comb";
const CLIP_EXTENSIONS: [&str; 3] = [
    CAF_EXTENSION,
    BLEND_SPACE_EXTENSION,
    COMBINED_BLEND_SPACE_EXTENSION,
];
/// Blend space XML (Observed in `boar_combat_turn_blend_l.bspace` and `_comb.comb`).
const EXAMPLE_ELEMENT: &str = "Example";
const BLEND_SPACE_ELEMENT: &str = "BlendSpace";
const CLIP_NAME_ATTRIBUTE: &str = "AName";
/// Two clip lengths closer than this are the same (one 30 fps frame is 33 ms).
const SAME_LENGTH_S: f64 = 1e-4;

/// CAF chunk file (Observed: `CrCh` magic; chunk count and table offset at bytes 8 and 12; a
/// 16-byte table entry of type `u16`, version `u16`, id, size and offset `u32`; the
/// `MotionParams` chunk `0x3002` holds flags, compression, ticks per frame, seconds per tick,
/// start and end frame; boar_gore: 46 frames at 1/30 s, read by the audit script
/// `cafdur.py`).
const CAF_MAGIC: &[u8; 4] = b"CrCh";
const CAF_CHUNK_COUNT_AT: usize = 8;
const CAF_CHUNK_TABLE_AT: usize = 12;
const CAF_CHUNK_ENTRY_LEN: usize = 16;
const CAF_CHUNK_TYPE_LEN: usize = 2;
const CAF_CHUNK_OFFSET_AT: usize = 12;
const CAF_MOTION_PARAMS: u16 = 0x3002;
const MOTION_TICKS_PER_FRAME_AT: usize = 8;
const MOTION_SECONDS_PER_TICK_AT: usize = 12;
const MOTION_START_AT: usize = 16;
const MOTION_END_AT: usize = 20;

/// A failure of the AI data build.
#[derive(Debug, thiserror::Error)]
pub enum AiDataError {
    /// Server.pak cannot be read.
    #[error(transparent)]
    ServerPak(#[from] ServerPakError),
    /// A pak of the install cannot be listed.
    #[error(transparent)]
    Pak(#[from] PakError),
    /// A slice value does not decode.
    #[error("{path}: {source}")]
    Decode {
        /// The slice.
        path: String,
        /// Why.
        source: DecodeError,
    },
    /// A stand-in tree cannot be built.
    #[error(transparent)]
    StandIn(#[from] StandInError),
    /// The creature animation table has a bad asset id.
    #[error("creature animation table: bad asset {0}")]
    Asset(String),
}

/// What the build did, and every value it could not find.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiDataReport {
    /// Server.pak trees.
    pub trees: usize,
    /// Shared nodes.
    pub nodes: usize,
    /// Nodes with every sub-tree copy counted.
    pub expanded_nodes: u64,
    /// Creature slice assets.
    pub creatures: usize,
    /// Archetypes (distinct slices).
    pub archetypes: usize,
    /// Grid and animation database pairs.
    pub grids: usize,
    /// Distinct CAGE actions.
    pub cage_actions: usize,
    /// Archetypes with a Server.pak tree, with a grid, and with an animation database.
    pub with_tree: usize,
    /// Archetypes with a grid.
    pub with_grid: usize,
    /// Archetypes with an animation database.
    pub with_animation_database: usize,
    /// Fragment entries over all CAGE actions, and those with a known length.
    pub variants: usize,
    /// Fragment entries with a length in seconds.
    pub timed_variants: usize,
    /// Trees a slice names that Server.pak does not have, with the slices that name them.
    pub missing_trees: BTreeMap<String, BTreeSet<String>>,
    /// CAGE tasks a slice's tree runs that its grid has no action for, by slice.
    pub missing_tasks: BTreeMap<String, BTreeSet<String>>,
    /// Clips whose length is Unknown, with the reason.
    pub unknown_clips: BTreeMap<String, String>,
    /// Fragment entries whose length is Unknown, by animation database.
    pub unknown_fragments: BTreeMap<String, usize>,
    /// Attacks of the ranged attack database.
    pub ranged_attacks: usize,
    /// Projectile slices the ranged clips name, with a flight.
    pub projectile_slices: usize,
    /// Files that could not be used, with the reason.
    pub skipped: BTreeMap<String, String>,
}

/// The AI of one slice before the arena exists.
#[derive(Debug, Clone, Default)]
struct SliceAi {
    trees: Vec<String>,
    config: Vec<(String, Value)>,
    auto_evaluate: Vec<String>,
    auto_evaluation_s: Option<f32>,
    ambient_vision: Option<Vision>,
    combat_vision: Option<Vision>,
    group_aggro_distance: Option<f32>,
    group_aggro_cap: Option<u32>,
    targeting_profile: Option<String>,
    aggro_delay_override_s: Option<f32>,
    nav_configs: Vec<NavConfig>,
    max_slope_deg: Option<f32>,
    grid: Option<String>,
    animation_database: Option<String>,
    damage_table: Option<String>,
    main_hand: Option<String>,
}

fn crc(name: &str) -> u32 {
    lowercase_crc32(name)
}

/// The first element named `name` below `element`, depth first.
fn first<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    let wanted = crc(name);
    let mut stack: Vec<&Element> = element.children.iter().rev().collect();
    while let Some(next) = stack.pop() {
        if next.name_crc == Some(wanted) {
            return Some(next);
        }
        stack.extend(next.children.iter().rev());
    }
    None
}

/// The field `name` of a class element: a direct child, or one in its `BaseClass1` chain
/// (Observed: a config param's `m_name` sits in `BaseClass1`, its `m_value` beside it).
fn field<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    child(element, name).or_else(|| field(child(element, BASE_CLASS)?, name))
}

/// The direct child named `name`.
fn child<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    element.child(crc(name))
}

fn decode_err(path: &str) -> impl Fn(DecodeError) -> AiDataError + '_ {
    move |source| AiDataError::Decode {
        path: path.to_owned(),
        source,
    }
}

fn float_value(value: Value) -> Option<f32> {
    match value {
        Value::Float(value) => Some(value),
        // Small whole numbers only; a slice stores counts and degrees this way.
        Value::Int(value) => Some(value as f32),
        Value::Bool(_) | Value::Text(_) | Value::Vec3(_) => None,
    }
}

fn text_value(decoder: &TreeDecoder, value: Value) -> Option<String> {
    match value {
        Value::Text(text) => Some(decoder.name(text).to_owned()),
        _ => None,
    }
}

impl SliceAi {
    fn read(root: &Element, decoder: &mut TreeDecoder, path: &str) -> Result<Self, AiDataError> {
        let err = decode_err(path);
        let mut slice = Self::default();
        let value = |decoder: &mut TreeDecoder, element: &Element, field: &'static str| {
            decoder.value_of(field, element).map_err(&err)
        };
        if let Some(assets) = first(root, TREE_ASSETS) {
            for entry in &assets.children {
                if let Some(reference) = child(entry, ASSET_REF)
                    && let Some(asset) = decoder.asset_of(ASSET_REF, reference).map_err(&err)?
                    && let Some(hint) = asset.hint
                {
                    slice.trees.push(decoder.name(hint).to_owned());
                }
            }
        }
        if let Some(params) = first(root, CONFIG_PARAMS) {
            let mut stack = vec![params];
            while let Some(next) = stack.pop() {
                match (field(next, CONFIG_NAME), child(next, CONFIG_VALUE)) {
                    (Some(name), Some(stored)) => {
                        let name = value(decoder, name, CONFIG_NAME)?;
                        let name = text_value(decoder, name).ok_or_else(|| {
                            err(DecodeError::MissingField {
                                class: nw_ai::classes::Class::String,
                                field: CONFIG_NAME,
                            })
                        })?;
                        slice
                            .config
                            .push((name, value(decoder, stored, CONFIG_VALUE)?));
                    }
                    _ => stack.extend(next.children.iter().rev()),
                }
            }
        }
        if let Some(keys) = first(root, AUTO_EVALUATE) {
            for key in &keys.children {
                let key = value(decoder, key, AUTO_EVALUATE)?;
                slice.auto_evaluate.extend(text_value(decoder, key));
            }
        }
        let float = |decoder: &mut TreeDecoder,
                     within: &Element,
                     field: &'static str|
         -> Result<Option<f32>, AiDataError> {
            match first(within, field) {
                Some(element) => Ok(float_value(value(decoder, element, field)?)),
                None => Ok(None),
            }
        };
        slice.auto_evaluation_s = float(decoder, root, AUTO_EVALUATION_FREQUENCY)?;
        for (field, vision) in [
            (AMBIENT_VISION, &mut slice.ambient_vision),
            (COMBAT_VISION, &mut slice.combat_vision),
        ] {
            if let Some(profile) = first(root, field) {
                *vision = match (
                    float(decoder, profile, VISION_ANGLE)?,
                    float(decoder, profile, VISION_DISTANCE)?,
                ) {
                    (Some(angle_deg), Some(distance_m)) => Some(Vision {
                        angle_deg,
                        distance_m,
                    }),
                    _ => None,
                };
            }
        }
        slice.group_aggro_distance = float(decoder, root, GROUP_AGGRO_DISTANCE)?;
        slice.group_aggro_cap = float(decoder, root, GROUP_AGGRO_CAP)?.map(|cap| cap as u32);
        slice.max_slope_deg = float(decoder, root, MAX_SLOPE)?;
        let overridden = match first(root, OVERRIDE_AGGRO_DELAY) {
            Some(element) => value(decoder, element, OVERRIDE_AGGRO_DELAY)?.as_bool(),
            None => None,
        };
        if overridden == Some(true) {
            slice.aggro_delay_override_s = float(decoder, root, AGGRO_DELAY)?;
        }
        let asset_path = |decoder: &mut TreeDecoder,
                          field: &'static str|
         -> Result<Option<String>, AiDataError> {
            let Some(element) = first(root, field) else {
                return Ok(None);
            };
            Ok(decoder
                .asset_of(field, element)
                .map_err(&err)?
                .and_then(|asset| asset.hint)
                .map(|hint| decoder.name(hint).to_owned()))
        };
        slice.targeting_profile = asset_path(decoder, TARGETING_PROFILE)?;
        slice.grid = asset_path(decoder, ACTION_GRID)?;
        let text_at = |decoder: &mut TreeDecoder,
                       element: Option<&Element>|
         -> Result<Option<String>, AiDataError> {
            // `AssetPath` sits in the field's `BaseClass1` (Observed in the boar slice).
            match element.and_then(|element| first(element, ASSET_PATH)) {
                Some(path) => {
                    let stored = value(decoder, path, ASSET_PATH)?;
                    Ok(text_value(decoder, stored).map(|path| path.to_lowercase()))
                }
                None => Ok(None),
            }
        };
        slice.animation_database = text_at(decoder, first(root, ANIMATION_DATABASE))?;
        slice.damage_table = text_at(
            decoder,
            first(root, DAMAGE_TABLE).and_then(|table| child(table, DAMAGE_TABLE_ASSET)),
        )?;
        if let Some(configs) = first(root, NAV_CONFIGS) {
            for entry in &configs.children {
                slice.nav_configs.push(nav_config(entry, decoder, path)?);
            }
        }
        Ok(slice)
    }
}

fn nav_config(
    entry: &Element,
    decoder: &mut TreeDecoder,
    path: &str,
) -> Result<NavConfig, AiDataError> {
    let err = decode_err(path);
    let missing = |field: &'static str| {
        err(DecodeError::MissingField {
            class: nw_ai::classes::Class::Float,
            field,
        })
    };
    let value = |decoder: &mut TreeDecoder, field: &'static str| -> Result<Value, AiDataError> {
        let element = child(entry, field).ok_or_else(|| missing(field))?;
        decoder.value_of(field, element).map_err(&err)
    };
    let number = |decoder: &mut TreeDecoder, field: &'static str| -> Result<f32, AiDataError> {
        float_value(value(decoder, field)?).ok_or_else(|| missing(field))
    };
    let stored = value(decoder, NAV_CONFIG_TYPE)?;
    let kind_text = text_value(decoder, stored).ok_or_else(|| missing(NAV_CONFIG_TYPE))?;
    let kind = decoder.intern(&kind_text);
    let max_speed = number(decoder, NAV_MAX_SPEED)?;
    let min_speed = number(decoder, NAV_MIN_SPEED)?;
    let turn_rate_deg = number(decoder, NAV_TURN_RATE)?;
    let linear_acceleration = number(decoder, NAV_ACCELERATION)?;
    let linear_deceleration = number(decoder, NAV_DECELERATION)?;
    let arrival_threshold = number(decoder, NAV_ARRIVAL)?;
    let target_repath_distance = number(decoder, NAV_REPATH)?;
    let select_position = match child(entry, NAV_SELECT_POSITION) {
        Some(params) => Some(decoder.select_position_of(params).map_err(&err)?),
        None => None,
    };
    let teleport = match child(entry, NAV_TELEPORT) {
        Some(element) => decoder
            .value_of(NAV_TELEPORT, element)
            .map_err(&err)?
            .as_bool()
            .unwrap_or(false),
        None => false,
    };
    let task =
        |decoder: &mut TreeDecoder, field: &'static str| -> Result<Option<NameId>, AiDataError> {
            let Some(element) = child(entry, field) else {
                return Ok(None);
            };
            let stored = decoder.value_of(field, element).map_err(&err)?;
            let text = text_value(decoder, stored);
            Ok(text
                .filter(|text| !text.is_empty())
                .map(|text| decoder.intern(&text)))
        };
    let turn_action = task(decoder, NAV_TURN_ACTION)?;
    let start_action = task(decoder, NAV_START_ACTION)?;
    let start_rule = match child(entry, NAV_START_RULE) {
        Some(element) => match decoder.value_of(NAV_START_RULE, element).map_err(&err)? {
            Value::Int(rule) => rule,
            _ => return Err(missing(NAV_START_RULE)),
        },
        None => return Err(missing(NAV_START_RULE)),
    };
    let mut action_params = Vec::new();
    for group in [NAV_ACTION_PARAMS, NAV_STRAFE_PARAMS] {
        if let Some(params) = child(entry, group) {
            for field in &params.children {
                if let Some(name_crc) = field.name_crc {
                    action_params.push((name_crc, decoder.value_of(group, field).map_err(&err)?));
                }
            }
        }
    }
    Ok(NavConfig {
        kind,
        max_speed,
        min_speed,
        turn_rate_deg,
        linear_acceleration,
        linear_deceleration,
        arrival_threshold,
        target_repath_distance,
        select_position,
        teleport,
        turn_action,
        start_action,
        start_rule,
        action_params,
    })
}

/// One action of an action list, before interning.
#[derive(Debug, Clone, Default, PartialEq)]
struct ListAction {
    name: String,
    fragment: String,
    tasks: Vec<String>,
    completion: Option<(f32, Vec<(String, f32)>)>,
    tags: Vec<(String, TagEffect, TagEffect, f32)>,
    motion_multiplier: Option<f32>,
    facing: Option<FacingMode>,
}

fn parse_number(text: Option<&str>) -> Option<f32> {
    text?.trim().parse().ok()
}

fn tag_effect(text: Option<&str>) -> Option<TagEffect> {
    match text? {
        TAG_SET => Some(TagEffect::Set),
        TAG_CLEAR => Some(TagEffect::Clear),
        TAG_NO_EFFECT => Some(TagEffect::NoEffect),
        _ => None,
    }
}

fn children_named<'a>(node: &'a XmlNode, name: &'a str) -> impl Iterator<Item = &'a XmlNode> {
    node.children.iter().filter(move |child| child.name == name)
}

/// An action list: its actions and its named conditions.
#[derive(Debug, Clone, Default, PartialEq)]
struct ParsedList {
    actions: Vec<ListAction>,
    conditions: Vec<(String, Condition)>,
}

/// The tasks and the action names of a grid with their CAGE actions, each sorted by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GridIndex {
    tasks: Vec<(NameId, u32)>,
    actions: Vec<(NameId, u32)>,
}

/// The actions of an action list and its named conditions.
fn list_actions(text: &str) -> Result<ParsedList, MannequinError> {
    let root = parse_tree(text)?;
    let mut actions = Vec::new();
    root.descendants(ACTION_ELEMENT, &mut actions);
    let mut out = Vec::new();
    for action in actions {
        let (Some(name), Some(fragment)) = (
            action.attribute(NAME_ATTRIBUTE),
            action.attribute(FRAGMENT_NAME_ATTRIBUTE),
        ) else {
            continue;
        };
        let mut tasks = Vec::new();
        for activation in children_named(action, ACTIVATION_ELEMENT) {
            for test in TASK_TESTS {
                let mut found = Vec::new();
                activation.descendants(test, &mut found);
                tasks.extend(
                    found
                        .iter()
                        .filter_map(|node| node.attribute(TASK_ATTRIBUTE))
                        .map(str::to_owned),
                );
            }
        }
        let mut completion = None;
        let mut tags = Vec::new();
        let mut motion_multiplier = None;
        let mut facing = None;
        for activities in action
            .children
            .iter()
            .filter(|node| node.name == "Activities")
        {
            for activity in &activities.children {
                let default = children_named(activity, DEFAULT_ELEMENT).next();
                if COMPLETION_ACTIVITIES.contains(&activity.name.as_str()) && completion.is_none() {
                    if let Some(offset) =
                        default.and_then(|node| parse_number(node.attribute(COMPLETION_OFFSET)))
                    {
                        let conditional = children_named(activity, CONDITIONAL_ELEMENT)
                            .filter_map(|node| {
                                Some((
                                    node.attribute(NAME_ATTRIBUTE)?.to_owned(),
                                    parse_number(node.attribute(COMPLETION_OFFSET))?,
                                ))
                            })
                            .collect();
                        completion = Some((offset, conditional));
                    }
                } else if activity.name == SET_TAG_ACTIVITY {
                    if let Some(node) = default
                        && let (Some(tag), Some(enter), Some(exit)) = (
                            node.attribute(TAG_NAME_ATTRIBUTE),
                            tag_effect(node.attribute(ON_ENTER_ATTRIBUTE)),
                            tag_effect(node.attribute(ON_EXIT_ATTRIBUTE)),
                        )
                    {
                        tags.push((
                            tag.to_owned(),
                            enter,
                            exit,
                            parse_number(node.attribute(DELAY_ATTRIBUTE)).unwrap_or(0.0),
                        ));
                    }
                } else if activity.name == MOTION_ACTIVITY {
                    motion_multiplier =
                        default.and_then(|node| parse_number(node.attribute(MOTION_MULTIPLIER)));
                } else if activity.name == FACING_ACTIVITY {
                    facing = default
                        .and_then(|node| node.attribute(FACING_MODE))
                        .and_then(FacingMode::of);
                }
            }
        }
        out.push(ListAction {
            name: name.to_owned(),
            fragment: fragment.to_owned(),
            tasks,
            completion,
            tags,
            motion_multiplier,
            facing,
        });
    }
    let conditions = ActionList::parse(text)?.conditions;
    Ok(ParsedList {
        actions: out,
        conditions,
    })
}

/// The action lists of a grid, in file order: the pak path of each and the alias of each of
/// its actions (`<AliasMap><Action ActionName Alias>`).
fn grid_lists(text: &str) -> Result<GridLists, MannequinError> {
    let lists = CageGrid::lists(text)?;
    Ok(GridLists {
        paths: lists.iter().map(GridList::pak_path).collect(),
        aliases: lists
            .iter()
            .flat_map(|list| &list.aliased)
            .filter_map(|action| Some((action.name.clone(), action.alias.clone()?)))
            .collect(),
    })
}

/// What a grid names: its action lists and the alias of each action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GridLists {
    paths: Vec<String>,
    aliases: Vec<(String, String)>,
}

/// One clip of a fragment's base animation layer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayerClip {
    exit_s: f64,
    start_s: f64,
    /// The animation's name, lower case.
    pub(crate) animation: String,
    speed: f64,
    looping: bool,
}

/// The base animation layer of one fragment entry; the entry's name, tags and procedural clips
/// are those of the `nw_formats::mannequin::Fragment` at the same index.
pub(crate) type AdbEntry = Vec<LayerClip>;

/// The base layers of the entries of an animation database, in file order (the order of
/// `AnimDatabase::parse`).
pub(crate) fn adb_entries(text: &str) -> Result<Vec<AdbEntry>, MannequinError> {
    let root = parse_tree(text)?;
    let mut entries = Vec::new();
    for named in &root.child(FRAGMENT_LIST)?.children {
        for entry in children_named(named, FRAGMENT_ELEMENT) {
            let mut clips = Vec::new();
            if let Some(layer) = children_named(entry, ANIM_LAYER).next() {
                let mut blend: Option<(f64, f64)> = None;
                for node in &layer.children {
                    if node.name == BLEND_ELEMENT {
                        blend = Some((
                            parse_number(node.attribute(EXIT_TIME)).map_or(0.0, f64::from),
                            parse_number(node.attribute(START_TIME)).map_or(0.0, f64::from),
                        ));
                    } else if node.name == ANIMATION_ELEMENT {
                        let (exit_s, start_s) = blend.take().unwrap_or((0.0, 0.0));
                        clips.push(LayerClip {
                            exit_s,
                            start_s,
                            animation: node
                                .attribute(ANIMATION_NAME)
                                .unwrap_or_default()
                                .to_lowercase(),
                            speed: parse_number(node.attribute(ANIMATION_SPEED))
                                .map_or(DEFAULT_SPEED, f64::from),
                            looping: node
                                .attribute(ANIMATION_FLAGS)
                                .is_some_and(|flags| flags.contains(LOOP_FLAG)),
                        });
                    }
                }
            }
            entries.push(clips);
        }
    }
    Ok(entries)
}

/// The length of one clip in seconds, or why it is Unknown.
pub(crate) type ClipLength = Result<f64, String>;

/// The length of a `.caf` (its `MotionParams` chunk).
fn caf_seconds(bytes: &[u8]) -> Option<f64> {
    if bytes.get(..CAF_MAGIC.len())? != CAF_MAGIC {
        return None;
    }
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let count = u32_at(CAF_CHUNK_COUNT_AT)? as usize;
    let table = u32_at(CAF_CHUNK_TABLE_AT)? as usize;
    for index in 0..count {
        let entry = table + index * CAF_CHUNK_ENTRY_LEN;
        let kind = u16::from_le_bytes(
            bytes
                .get(entry..entry + CAF_CHUNK_TYPE_LEN)?
                .try_into()
                .ok()?,
        );
        if kind != CAF_MOTION_PARAMS {
            continue;
        }
        let at = u32_at(entry + CAF_CHUNK_OFFSET_AT)? as usize;
        let ticks_per_frame = f64::from(u32_at(at + MOTION_TICKS_PER_FRAME_AT)? as i32);
        let seconds_per_tick = f64::from(f32::from_bits(u32_at(at + MOTION_SECONDS_PER_TICK_AT)?));
        let start = f64::from(u32_at(at + MOTION_START_AT)? as i32);
        let end = f64::from(u32_at(at + MOTION_END_AT)? as i32);
        return Some((end - start) * ticks_per_frame * seconds_per_tick);
    }
    None
}

/// The clip names a blend space or combined blend space plays (lower-case base names).
fn blend_members(text: &str) -> Result<Vec<String>, MannequinError> {
    let root = parse_tree(text)?;
    let mut members = Vec::new();
    for element in [EXAMPLE_ELEMENT, BLEND_SPACE_ELEMENT] {
        let mut found = Vec::new();
        root.descendants(element, &mut found);
        members.extend(
            found
                .iter()
                .filter_map(|node| node.attribute(CLIP_NAME_ATTRIBUTE))
                .map(base_name),
        );
    }
    Ok(members)
}

/// The lower-case file name of a path without its folder and extension.
fn base_name(path: &str) -> String {
    let file = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    CLIP_EXTENSIONS
        .iter()
        .find_map(|extension| file.strip_suffix(extension))
        .map_or(file.clone(), str::to_owned)
}

/// Clip lengths by base name, read from the game paks.
pub(crate) struct ClipLengths {
    lengths: HashMap<String, ClipLength>,
}

impl ClipLengths {
    /// The lengths `entries`, by clip name.
    #[cfg(test)]
    pub(crate) fn of(entries: &[(&str, ClipLength)]) -> Self {
        Self {
            lengths: entries
                .iter()
                .map(|(name, length)| ((*name).to_owned(), length.clone()))
                .collect(),
        }
    }

    /// Lists the animation files of the install and reads the clips `names` play, and the clips
    /// their blend spaces play.
    pub(crate) fn read(
        source: &GameSource,
        game_dir: &Path,
        names: &BTreeSet<String>,
    ) -> Result<Self, ToolError> {
        let mut files: HashMap<String, Vec<String>> = HashMap::new();
        for pak in find_paks(game_dir).map_err(AiDataError::from)? {
            let mut archive = PakArchive::open(&pak).map_err(AiDataError::from)?;
            for entry in archive.entries().map_err(AiDataError::from)? {
                let path = entry.path.to_lowercase();
                if CLIP_EXTENSIONS
                    .iter()
                    .any(|extension| path.ends_with(extension))
                {
                    files.entry(base_name(&path)).or_default().push(path);
                }
            }
        }
        let mut raw: HashMap<String, Result<Vec<u8>, String>> = HashMap::new();
        let mut pending: BTreeSet<String> = names.clone();
        while !pending.is_empty() {
            let mut wanted = HashSet::new();
            for name in &pending {
                match files.get(name).map(Vec::as_slice) {
                    Some([path]) => {
                        wanted.insert(path.clone());
                    }
                    Some(paths) => {
                        raw.insert(
                            name.clone(),
                            Err(format!("{} files have this name", paths.len())),
                        );
                    }
                    None => {
                        raw.insert(
                            name.clone(),
                            Err("no animation file has this name".to_owned()),
                        );
                    }
                }
            }
            let read = read_entries(source, &wanted)?;
            let mut next = BTreeSet::new();
            for (path, bytes) in read {
                let name = base_name(&path);
                if !path.ends_with(CAF_EXTENSION) {
                    match text_of(Some(&bytes)).map(blend_members) {
                        Some(Ok(members)) => next.extend(
                            members
                                .into_iter()
                                .filter(|member| !raw.contains_key(member)),
                        ),
                        Some(Err(error)) => {
                            raw.insert(name, Err(error.to_string()));
                            continue;
                        }
                        None => {
                            raw.insert(name, Err("blend space is not UTF-8".to_owned()));
                            continue;
                        }
                    }
                }
                raw.insert(name, Ok(bytes));
            }
            pending = next
                .into_iter()
                .filter(|name| !raw.contains_key(name))
                .collect();
        }
        let mut lengths = HashMap::new();
        let names: Vec<String> = raw.keys().cloned().collect();
        for name in names {
            let length = Self::resolve(&name, &raw, &mut lengths, &mut Vec::new());
            lengths.insert(name, length);
        }
        Ok(Self { lengths })
    }

    /// The length of `name`: a `.caf` from its chunk, a blend space from the clips it plays when
    /// they all have one length.
    fn resolve(
        name: &str,
        raw: &HashMap<String, Result<Vec<u8>, String>>,
        lengths: &mut HashMap<String, ClipLength>,
        stack: &mut Vec<String>,
    ) -> ClipLength {
        if let Some(known) = lengths.get(name) {
            return known.clone();
        }
        if stack.iter().any(|open| open == name) {
            return Err("blend space plays itself".to_owned());
        }
        let length = match raw.get(name) {
            None => Err("not read".to_owned()),
            Some(Err(reason)) => Err(reason.clone()),
            Some(Ok(bytes)) => match caf_seconds(bytes) {
                Some(seconds) => Ok(seconds),
                None => match text_of(Some(bytes)).map(blend_members) {
                    Some(Ok(members)) if !members.is_empty() => {
                        stack.push(name.to_owned());
                        let parts: Vec<ClipLength> = members
                            .iter()
                            .map(|member| Self::resolve(member, raw, lengths, stack))
                            .collect();
                        stack.pop();
                        match parts.iter().find_map(|part| part.as_ref().err()) {
                            Some(reason) => Err(format!("a blended clip: {reason}")),
                            None => {
                                let values: Vec<f64> = parts.into_iter().flatten().collect();
                                let first = values[0];
                                if values
                                    .iter()
                                    .all(|value| (value - first).abs() < SAME_LENGTH_S)
                                {
                                    Ok(first)
                                } else {
                                    Err("blends clips of different lengths".to_owned())
                                }
                            }
                        }
                    }
                    _ => Err("neither a CAF nor a blend space".to_owned()),
                },
            },
        };
        lengths.insert(name.to_owned(), length.clone());
        length
    }

    fn get(&self, name: &str) -> ClipLength {
        self.lengths
            .get(name)
            .cloned()
            .unwrap_or_else(|| Err("not read".to_owned()))
    }
}

/// How long a fragment entry plays, from its base layer: each clip starts `ExitTime` seconds
/// after the one before (a negative exit time: at its end) and plays `(length - StartTime) /
/// speed`; the entry ends with its last clip, or where an empty clip starts; a looping last clip
/// never ends. The time base of `ExitTime` and `StartTime` (seconds) is Inferred; it fits the
/// retail boar `Attack_Gore` (one clip; 1,465 and 1,498 ms observed against 1,404 ms + one or two
/// ticks) and `Combat_Turn` (a blend space of 1.8 s clips, 1,632 and 1,683 ms against 1,600 ms).
pub(crate) fn fragment_length(
    clips: &[LayerClip],
    lengths: &ClipLengths,
) -> Result<FragmentLength, String> {
    let Some(last) = clips.len().checked_sub(1) else {
        return Err("no animation layer".to_owned());
    };
    let mut start = 0.0;
    let mut previous = 0.0;
    for (index, clip) in clips.iter().enumerate() {
        if index > 0 {
            start += if clip.exit_s < 0.0 {
                previous
            } else {
                clip.exit_s
            };
        }
        if clip.animation.is_empty() {
            if index == last {
                return Ok(FragmentLength::Seconds(start as f32));
            }
            previous = 0.0;
            continue;
        }
        if clip.looping && index == last {
            return Ok(FragmentLength::Loops);
        }
        let length = lengths
            .get(&clip.animation)
            .map_err(|reason| format!("{}: {reason}", clip.animation))?;
        previous = (length - clip.start_s).max(0.0) / clip.speed;
    }
    Ok(FragmentLength::Seconds((start + previous) as f32))
}

fn cage_condition(decoder: &mut TreeDecoder, condition: &Condition) -> CageCondition {
    match condition {
        Condition::All(parts) => CageCondition::All(
            parts
                .iter()
                .map(|part| cage_condition(decoder, part))
                .collect(),
        ),
        Condition::Any(parts) => CageCondition::Any(
            parts
                .iter()
                .map(|part| cage_condition(decoder, part))
                .collect(),
        ),
        Condition::Not(part) => CageCondition::Not(Box::new(cage_condition(decoder, part))),
        Condition::True => CageCondition::True,
        Condition::Tag(tag) => CageCondition::Tag(decoder.intern(tag)),
        Condition::Marker(marker) => CageCondition::Marker(decoder.intern(marker)),
        // A mob's tree reads tags and markers only: every other test stays a test by its
        // element name, as before the parser knew them.
        other => CageCondition::Other(decoder.intern(other.test_element().unwrap_or_default())),
    }
}

/// The tasks a tree can request, by name (literal tasks and tasks the archetype's config gives
/// a keyed `ActionName`).
fn tree_tasks(
    data_nodes: &[nw_ai::node::Node],
    child_ids: &[NodeId],
    root: NodeId,
    arch: &Archetype,
) -> BTreeSet<NameId> {
    let mut tasks = BTreeSet::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let kind = &data_nodes[id as usize].kind;
        if let Some(Param { key, value }) = kind.task() {
            match (key.and_then(|key| arch.config_value(key)), value) {
                (Some(Value::Text(task)), _) | (None, Some(Value::Text(task))) => {
                    tasks.insert(task);
                }
                _ => {}
            }
        }
        stack.extend(kind.child());
        if let Some(children) = kind.children() {
            let first = children.first as usize;
            stack.extend_from_slice(&child_ids[first..first + children.count as usize]);
        }
    }
    tasks
}

/// The pak path of the datasheet of a slice's `m_damageTable` (Observed: the slice names
/// `.../javelindata_damagetable_boar.xml`, the paks hold `.../javelindata_damagetable_boar.datasheet`).
fn datasheet_path(table: &str) -> String {
    table.strip_suffix(DAMAGE_TABLE_SUFFIX).map_or_else(
        || table.to_owned(),
        |stem| format!("{stem}{DATASHEET_SUFFIX}"),
    )
}

/// The unique id of the damage table datasheet of each slice that names one, by the slice's
/// table path; a datasheet that is missing or does not parse is listed in the report.
fn damage_table_ids(
    source: &GameSource,
    slices: &BTreeMap<String, SliceAi>,
    report: &mut AiDataReport,
) -> Result<HashMap<String, String>, ToolError> {
    let tables: HashSet<String> = slices
        .values()
        .filter_map(|slice| slice.damage_table.clone())
        // Some slices hold an empty path: they name no table.
        .filter(|table| !table.is_empty())
        .collect();
    let sheets: HashSet<String> = tables.iter().map(|table| datasheet_path(table)).collect();
    let bytes = read_entries(source, &sheets)?;
    let mut ids = HashMap::new();
    for table in tables {
        let sheet = datasheet_path(&table);
        match bytes.get(&sheet).map(|bytes| Datasheet::parse(bytes)) {
            Some(Ok(parsed)) => {
                ids.insert(table, parsed.unique_id);
            }
            Some(Err(error)) => {
                report.skipped.insert(sheet, error.to_string());
            }
            None => {
                report.skipped.insert(sheet, "not in the paks".to_owned());
            }
        }
    }
    Ok(ids)
}

fn asset_bytes(hex: &str) -> Result<[u8; 16], AiDataError> {
    let bytes = hex::decode(hex).map_err(|_| AiDataError::Asset(hex.to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| AiDataError::Asset(hex.to_owned()))
}

fn slice_root(bytes: &[u8]) -> Result<Element, String> {
    let stream = if is_azcs(bytes) {
        decode_azcs(bytes).map_err(|error| error.to_string())?
    } else {
        bytes.to_vec()
    };
    parse_object_stream(&stream).map_err(|error| error.to_string())
}

/// Builds the AI data from `source` (the install at `game_dir`), the Server.pak at
/// `server_pak` and the creature animation table `creatures`.
///
/// # Errors
///
/// Returns [`ToolError`] when Server.pak or a pak cannot be read, or a tree does not decode.
pub fn build_ai_data(
    source: &GameSource,
    game_dir: &Path,
    server_pak: &Path,
    creatures: &[CreatureRow],
) -> Result<(AiData, AiDataReport), ToolError> {
    let mut report = AiDataReport::default();
    let mut decoder = TreeDecoder::default();
    {
        let entries =
            server_pak_entries(server_pak, &KrakenDecompressor).map_err(AiDataError::from)?;
        decode_entries(&mut decoder, &entries).map_err(AiDataError::from)?;
        for (path, root) in stand_in_trees(&entries).map_err(AiDataError::from)? {
            decoder
                .add_tree(path, &root)
                .map_err(|source| AiDataError::Decode {
                    path: path.to_owned(),
                    source,
                })?;
        }
    }
    let slice_paths: HashSet<String> = creatures
        .iter()
        .map(|row| row.slice.to_lowercase())
        .collect();
    let slice_bytes = read_entries(source, &slice_paths)?;
    let mut slices: BTreeMap<String, SliceAi> = BTreeMap::new();
    for path in &slice_paths {
        let Some(bytes) = slice_bytes.get(path) else {
            report
                .skipped
                .insert(path.clone(), "not in the paks".to_owned());
            continue;
        };
        match slice_root(bytes) {
            Ok(root) => {
                let mut slice = SliceAi::read(&root, &mut decoder, path)?;
                slice.main_hand = main_hand_item(&root);
                slices.insert(path.clone(), slice);
            }
            Err(reason) => {
                report.skipped.insert(path.clone(), reason);
            }
        }
    }
    drop(slice_bytes);
    let table_ids = damage_table_ids(source, &slices, &mut report)?;
    let grid_paths: HashSet<String> = slices
        .values()
        .filter_map(|slice| slice.grid.clone())
        .collect();
    let grid_bytes = read_entries(source, &grid_paths)?;
    let mut grid_lists_by_path: HashMap<String, GridLists> = HashMap::new();
    for path in &grid_paths {
        match text_of(grid_bytes.get(path)).map(grid_lists) {
            Some(Ok(lists)) => {
                grid_lists_by_path.insert(path.clone(), lists);
            }
            Some(Err(error)) => {
                report.skipped.insert(path.clone(), error.to_string());
            }
            None => {
                report
                    .skipped
                    .insert(path.clone(), "grid missing or not UTF-8".to_owned());
            }
        }
    }
    let list_paths: HashSet<String> = grid_lists_by_path
        .values()
        .flat_map(|lists| lists.paths.iter().cloned())
        .collect();
    let list_bytes = read_entries(source, &list_paths)?;
    let mut lists: HashMap<String, ParsedList> = HashMap::new();
    for path in &list_paths {
        match text_of(list_bytes.get(path)).map(list_actions) {
            Some(Ok(parsed)) => {
                lists.insert(path.clone(), parsed);
            }
            Some(Err(error)) => {
                report.skipped.insert(path.clone(), error.to_string());
            }
            None => {
                report
                    .skipped
                    .insert(path.clone(), "action list missing or not UTF-8".to_owned());
            }
        }
    }
    // The animation database of each slice's main-hand weapon (its creature row's).
    let weapon_adbs: HashMap<String, String> = creatures
        .iter()
        .filter_map(|row| {
            Some((
                row.slice.to_lowercase(),
                row.weapon_animation_database.clone()?,
            ))
        })
        .collect();
    let adb_paths: HashSet<String> = slices
        .values()
        .filter_map(|slice| slice.animation_database.clone())
        .chain(weapon_adbs.values().cloned())
        .collect();
    let adb_bytes = read_entries(source, &adb_paths)?;
    let mut adbs: HashMap<String, (Vec<AdbEntry>, AnimDatabase)> = HashMap::new();
    for path in &adb_paths {
        let parsed = text_of(adb_bytes.get(path))
            .ok_or_else(|| "animation database missing or not UTF-8".to_owned())
            .and_then(|text| {
                let entries = adb_entries(text).map_err(|error| error.to_string())?;
                let clips = AnimDatabase::parse(text).map_err(|error| error.to_string())?;
                if clips.fragments.len() == entries.len() {
                    Ok((entries, clips))
                } else {
                    Err("fragment entries and procedural clips do not line up".to_owned())
                }
            });
        match parsed {
            Ok(parsed) => {
                adbs.insert(path.clone(), parsed);
            }
            Err(reason) => {
                report.skipped.insert(path.clone(), reason);
            }
        }
    }
    drop(adb_bytes);
    let clip_names: BTreeSet<String> = adbs
        .values()
        .flat_map(|(entries, _)| entries.iter().flatten())
        .map(|clip| clip.animation.clone())
        .filter(|name| !name.is_empty())
        .collect();
    let lengths = ClipLengths::read(source, game_dir, &clip_names)?;
    for name in &clip_names {
        if let Err(reason) = lengths.get(name) {
            report.unknown_clips.insert(name.clone(), reason);
        }
    }

    let mut builder = CageBuilder {
        lists: &lists,
        lengths: &lengths,
        actions: Vec::new(),
        index: HashMap::new(),
        unknown_fragments: BTreeMap::new(),
    };
    let mut grids: Vec<Grid> = Vec::new();
    let mut grid_index: HashMap<(String, Option<String>, Option<String>), u32> = HashMap::new();
    let mut archetypes: Vec<Archetype> = Vec::new();
    let mut archetype_of_slice: HashMap<String, u32> = HashMap::new();
    for (path, slice) in &slices {
        let weapon_adb = weapon_adbs.get(path);
        let grid = match &slice.grid {
            Some(grid) if grid_lists_by_path.contains_key(grid) => {
                let key = (
                    grid.clone(),
                    slice.animation_database.clone(),
                    weapon_adb.cloned(),
                );
                match grid_index.get(&key) {
                    Some(index) => Some(*index),
                    None => {
                        let named = &grid_lists_by_path[grid];
                        let GridIndex { tasks, actions } = builder.grid(
                            &mut decoder,
                            &named.paths,
                            database(&adbs, slice.animation_database.as_deref()),
                            database(&adbs, weapon_adb.map(String::as_str)),
                        );
                        let mut first_alias: BTreeMap<NameId, NameId> = BTreeMap::new();
                        for (action, alias) in &named.aliases {
                            let action = decoder.intern(action);
                            let alias = decoder.intern(alias);
                            first_alias.entry(action).or_insert(alias);
                        }
                        let aliases: Vec<(NameId, NameId)> = first_alias.into_iter().collect();
                        let index = u32::try_from(grids.len()).unwrap_or(u32::MAX);
                        grids.push(Grid {
                            path: decoder.intern(grid),
                            animation_database: slice
                                .animation_database
                                .as_ref()
                                .map(|adb| decoder.intern(adb)),
                            tasks,
                            actions,
                            aliases,
                        });
                        grid_index.insert(key, index);
                        Some(index)
                    }
                }
            }
            _ => None,
        };
        let config = slice
            .config
            .iter()
            .map(|(name, value)| (decoder.key(name), *value))
            .collect();
        let auto_evaluate = slice
            .auto_evaluate
            .iter()
            .map(|name| decoder.key(name))
            .collect();
        let row_tags: BTreeSet<&String> = creatures
            .iter()
            .filter(|row| row.slice.to_lowercase() == *path)
            .flat_map(|row| &row.default_tags)
            .collect();
        let archetype = Archetype {
            slice: decoder.intern(path),
            trees: slice
                .trees
                .iter()
                .map(|tree| TreeRef {
                    path: decoder.intern(tree),
                    tree: None,
                })
                .collect(),
            config,
            auto_evaluate,
            auto_evaluation_s: slice.auto_evaluation_s,
            ambient_vision: slice.ambient_vision,
            combat_vision: slice.combat_vision,
            group_aggro_distance: slice.group_aggro_distance,
            group_aggro_cap: slice.group_aggro_cap,
            targeting_profile: slice
                .targeting_profile
                .as_ref()
                .map(|profile| decoder.intern(profile)),
            aggro_delay_override_s: slice.aggro_delay_override_s,
            nav_configs: slice.nav_configs.clone(),
            max_slope_deg: slice.max_slope_deg,
            grid,
            animation_database: slice
                .animation_database
                .as_ref()
                .map(|adb| decoder.intern(adb)),
            damage_table: slice
                .damage_table
                .as_ref()
                .map(|table| decoder.intern(table)),
            damage_table_id: slice
                .damage_table
                .as_ref()
                .and_then(|table| table_ids.get(table))
                .map(|id| decoder.intern(id)),
            main_hand_item: slice.main_hand.as_ref().map(|item| decoder.intern(item)),
            default_tags: row_tags
                .into_iter()
                .map(|tag| decoder.intern(tag))
                .collect(),
        };
        archetype_of_slice.insert(
            path.clone(),
            u32::try_from(archetypes.len()).unwrap_or(u32::MAX),
        );
        archetypes.push(archetype);
    }
    let (ranged_attacks, projectile_slices) =
        ranged_shots(source, &mut decoder, &builder.actions, &mut report)?;
    let trees = decoder.finish();
    let tree_of: HashMap<NameId, u32> = trees
        .trees
        .iter()
        .enumerate()
        .map(|(index, tree)| (tree.path, u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    for archetype in &mut archetypes {
        for reference in &mut archetype.trees {
            reference.tree = tree_of.get(&reference.path).copied();
            if reference.tree.is_none() {
                report
                    .missing_trees
                    .entry(trees.names[reference.path as usize].clone())
                    .or_default()
                    .insert(trees.names[archetype.slice as usize].clone());
            }
        }
        let root = archetype
            .trees
            .iter()
            .find_map(|reference| reference.tree)
            .and_then(|tree| trees.trees[tree as usize].root);
        if let Some(root) = root {
            let grid = archetype.grid.and_then(|grid| grids.get(grid as usize));
            for task in tree_tasks(&trees.nodes, &trees.child_ids, root, archetype) {
                if grid.and_then(|grid| grid.action(task)).is_none() {
                    report
                        .missing_tasks
                        .entry(trees.names[archetype.slice as usize].clone())
                        .or_default()
                        .insert(trees.names[task as usize].clone());
                }
            }
        }
    }
    let mut creature_list = Vec::new();
    for row in creatures {
        if let Some(archetype) = archetype_of_slice.get(&row.slice.to_lowercase()) {
            creature_list.push(Creature {
                asset: asset_bytes(&row.asset)?,
                archetype: *archetype,
            });
        }
    }
    creature_list.sort_by_key(|creature| creature.asset);
    creature_list.dedup_by(|left, right| left.asset == right.asset);
    report.trees = trees.trees.len();
    report.nodes = trees.nodes.len();
    report.expanded_nodes = trees
        .trees
        .iter()
        .map(|tree| u64::from(tree.expanded_nodes))
        .sum();
    report.creatures = creature_list.len();
    report.archetypes = archetypes.len();
    report.grids = grids.len();
    report.cage_actions = builder.actions.len();
    report.with_tree = archetypes
        .iter()
        .filter(|arch| arch.trees.iter().any(|tree| tree.tree.is_some()))
        .count();
    report.with_grid = archetypes.iter().filter(|arch| arch.grid.is_some()).count();
    report.with_animation_database = archetypes
        .iter()
        .filter(|arch| arch.animation_database.is_some())
        .count();
    let variants = builder.actions.iter().flat_map(|action| &action.variants);
    report.variants = variants.clone().count();
    report.timed_variants = variants
        .filter(|variant| matches!(variant.length, FragmentLength::Seconds(_)))
        .count();
    report.unknown_fragments = builder.unknown_fragments;
    let data = AiData {
        names: trees.names,
        keys: trees.keys,
        nodes: trees.nodes,
        child_ids: trees.child_ids,
        trees: trees.trees,
        archetypes,
        grids,
        cage_actions: builder.actions,
        creatures: creature_list,
        ranged_attacks,
        projectile_slices,
    };
    Ok((data, report))
}

/// Every attack of the ranged attack database, and the flight of each projectile slice that a
/// `CAGE-RangedAttack` clip of `actions` names (`SliceOverride`). A slice that is missing or has
/// no projectile component goes to the report's skipped files.
fn ranged_shots(
    source: &GameSource,
    decoder: &mut TreeDecoder,
    actions: &[CageAction],
    report: &mut AiDataReport,
) -> Result<(Vec<RangedAttack>, Vec<ProjectileSlice>), ToolError> {
    let ranged_clip = decoder.intern(RANGED_CLIP);
    let slice_param = decoder.intern(SLICE_OVERRIDE_PARAM);
    let slice_paths: BTreeSet<String> = actions
        .iter()
        .flat_map(|action| action.variants.iter().chain(&action.weapon_variants))
        .flat_map(|variant| &variant.windows)
        .filter(|window| window.kind == ranged_clip)
        .filter_map(|window| {
            window
                .params
                .iter()
                .find(|(name, _)| *name == slice_param)
                .map(|(_, slice)| decoder.name(*slice).to_lowercase())
        })
        .filter(|slice| !slice.is_empty())
        .collect();
    let wanted: HashSet<String> = slice_paths
        .iter()
        .cloned()
        .chain([RANGED_ATTACK_DATABASE.to_owned()])
        .collect();
    let bytes = read_entries(source, &wanted)?;
    let mut ranged = Vec::new();
    match bytes
        .get(RANGED_ATTACK_DATABASE)
        .map(|bytes| slice_root(bytes))
    {
        Some(Ok(root)) => {
            let mut attacks: Vec<_> = ranged_attacks(&root).into_iter().collect();
            attacks.sort_by(|left, right| left.0.cmp(&right.0));
            for ((profile, attack), (range_m, radius_m, hit_scan)) in attacks {
                ranged.push(RangedAttack {
                    profile: decoder.intern(&profile),
                    attack: decoder.intern(&attack),
                    range_m,
                    radius_m,
                    hit_scan,
                });
            }
        }
        Some(Err(reason)) => {
            report
                .skipped
                .insert(RANGED_ATTACK_DATABASE.to_owned(), reason);
        }
        None => {
            report.skipped.insert(
                RANGED_ATTACK_DATABASE.to_owned(),
                "not in the paks".to_owned(),
            );
        }
    }
    let mut projectiles = Vec::new();
    for path in &slice_paths {
        let flight = bytes
            .get(path)
            .ok_or_else(|| "not in the paks".to_owned())
            .and_then(|bytes| slice_root(bytes))
            .and_then(|root| {
                slice_flight(&root).ok_or_else(|| "no projectile component".to_owned())
            });
        match flight {
            Ok(flight) => projectiles.push(ProjectileSlice {
                slice: decoder.intern(path),
                flight,
            }),
            Err(reason) => {
                report.skipped.insert(path.clone(), reason);
            }
        }
    }
    report.ranged_attacks = ranged.len();
    report.projectile_slices = projectiles.len();
    Ok((ranged, projectiles))
}

/// The parsed animation database at `path` among `adbs`, with its path.
fn database<'a>(
    adbs: &'a HashMap<String, (Vec<AdbEntry>, AnimDatabase)>,
    path: Option<&'a str>,
) -> Option<(&'a str, &'a (Vec<AdbEntry>, AnimDatabase))> {
    let path = path?;
    Some((path, adbs.get(path)?))
}

/// The CAGE actions built so far, shared by the grids (equal actions are stored once).
struct CageBuilder<'a> {
    lists: &'a HashMap<String, ParsedList>,
    lengths: &'a ClipLengths,
    actions: Vec<CageAction>,
    index: HashMap<Vec<u8>, u32>,
    unknown_fragments: BTreeMap<String, usize>,
}

impl CageBuilder<'_> {
    /// The tasks and actions of one grid with the CAGE actions they resolve to, each sorted by
    /// name (the first action list that has a task or an action name wins, as the client reads
    /// the grid's lists in order).
    fn grid(
        &mut self,
        decoder: &mut TreeDecoder,
        list_paths: &[String],
        adb: Option<(&str, &(Vec<AdbEntry>, AnimDatabase))>,
        weapon_adb: Option<(&str, &(Vec<AdbEntry>, AnimDatabase))>,
    ) -> GridIndex {
        let mut tasks: BTreeMap<NameId, u32> = BTreeMap::new();
        let mut names: BTreeMap<NameId, u32> = BTreeMap::new();
        for list_path in list_paths {
            let Some(ParsedList {
                actions,
                conditions,
            }) = self.lists.get(list_path)
            else {
                continue;
            };
            let list = decoder.intern(list_path);
            for action in actions {
                let cage = self.action(decoder, list, action, conditions, adb, weapon_adb);
                let name = cage.action;
                let key = serde_json::to_vec(&cage).unwrap_or_default();
                let index = match self.index.get(&key) {
                    Some(index) => *index,
                    None => {
                        let index = u32::try_from(self.actions.len()).unwrap_or(u32::MAX);
                        self.actions.push(cage);
                        self.index.insert(key, index);
                        index
                    }
                };
                names.entry(name).or_insert(index);
                for task in &action.tasks {
                    tasks.entry(decoder.intern(task)).or_insert(index);
                }
            }
        }
        GridIndex {
            tasks: tasks.into_iter().collect(),
            actions: names.into_iter().collect(),
        }
    }

    fn action(
        &mut self,
        decoder: &mut TreeDecoder,
        list: NameId,
        action: &ListAction,
        conditions: &[(String, Condition)],
        adb: Option<(&str, &(Vec<AdbEntry>, AnimDatabase))>,
        weapon_adb: Option<(&str, &(Vec<AdbEntry>, AnimDatabase))>,
    ) -> CageAction {
        let completion = action
            .completion
            .as_ref()
            .map(|(default_offset_s, conditional)| Completion {
                default_offset_s: *default_offset_s,
                conditional: conditional
                    .iter()
                    .map(|(name, offset)| {
                        let condition = match conditions.iter().find(|(known, _)| known == name) {
                            Some((_, condition)) => cage_condition(decoder, condition),
                            None => CageCondition::Other(decoder.intern(name)),
                        };
                        (condition, *offset)
                    })
                    .collect(),
            });
        let variants = self.variants(decoder, &action.fragment, adb);
        let weapon_variants = self.variants(decoder, &action.fragment, weapon_adb);
        CageAction {
            list,
            action: decoder.intern(&action.name),
            fragment: decoder.intern(&action.fragment),
            completion,
            tags: action
                .tags
                .iter()
                .map(|(tag, on_enter, on_exit, delay_s)| TagActivity {
                    tag: decoder.intern(tag),
                    on_enter: *on_enter,
                    on_exit: *on_exit,
                    delay_s: *delay_s,
                })
                .collect(),
            motion_multiplier: action.motion_multiplier,
            facing: action.facing,
            variants,
            weapon_variants,
        }
    }

    /// The entries of `fragment` in the animation database `adb`, with their procedural clips.
    fn variants(
        &mut self,
        decoder: &mut TreeDecoder,
        fragment: &str,
        adb: Option<(&str, &(Vec<AdbEntry>, AnimDatabase))>,
    ) -> Vec<FragmentVariant> {
        let mut variants = Vec::new();
        if let Some((adb_path, (entries, clips))) = adb {
            for (entry, procedural) in entries.iter().zip(&clips.fragments) {
                if procedural.name != fragment {
                    continue;
                }
                let length = fragment_length(entry, self.lengths).unwrap_or_else(|_| {
                    *self
                        .unknown_fragments
                        .entry(adb_path.to_owned())
                        .or_default() += 1;
                    FragmentLength::Unknown
                });
                variants.push(FragmentVariant {
                    tags: procedural
                        .tags
                        .iter()
                        .map(|tag| decoder.intern(tag))
                        .collect(),
                    frag_tags: procedural
                        .frag_tags
                        .iter()
                        .map(|tag| decoder.intern(tag))
                        .collect(),
                    length,
                    windows: procedural
                        .clips
                        .iter()
                        .map(|clip| ProcWindow {
                            kind: decoder.intern(&clip.kind),
                            start_s: clip.start_s as f32,
                            end_s: clip.end_s.map(|end| end as f32),
                            params: clip
                                .params
                                .iter()
                                .map(|(name, value)| (decoder.intern(name), decoder.intern(value)))
                                .collect(),
                        })
                        .collect(),
                });
            }
        }
        variants
    }
}

#[cfg(test)]
mod tests {
    use nw_ai::classes::{CLASSES, Class};

    use super::*;

    /// 46 frames at 1/30 s: `boar_gore.caf` (build 25810808).
    const GORE_FRAMES: i32 = 46;
    const SECONDS_PER_FRAME: f32 = 1.0 / 30.0;
    const GORE_SPEED: f64 = 0.85;
    const CAF_HEADER_LEN: usize = 16;
    const MOTION_PARAMS_LEN: usize = 24;

    /// A CAF with one chunk, the `MotionParams` of a clip of `frames` frames.
    fn caf(frames: i32) -> Vec<u8> {
        let mut bytes = CAF_MAGIC.to_vec();
        bytes.extend_from_slice(&0x746_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&(CAF_HEADER_LEN as u32).to_le_bytes());
        let motion_at = (CAF_HEADER_LEN + CAF_CHUNK_ENTRY_LEN) as u32;
        bytes.extend_from_slice(&CAF_MOTION_PARAMS.to_le_bytes());
        bytes.extend_from_slice(&0x925_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&(MOTION_PARAMS_LEN as u32).to_le_bytes());
        bytes.extend_from_slice(&motion_at.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&SECONDS_PER_FRAME.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&frames.to_le_bytes());
        bytes
    }

    fn lengths(entries: &[(&str, ClipLength)]) -> ClipLengths {
        ClipLengths::of(entries)
    }

    fn clip(exit_s: f64, animation: &str, speed: f64, looping: bool) -> LayerClip {
        LayerClip {
            exit_s,
            start_s: 0.0,
            animation: animation.to_owned(),
            speed,
            looping,
        }
    }

    #[test]
    fn a_caf_length_is_its_frames_times_the_frame_time() {
        let seconds = caf_seconds(&caf(GORE_FRAMES)).expect("length");
        assert!((seconds - f64::from(GORE_FRAMES) * f64::from(SECONDS_PER_FRAME)).abs() < 1e-6);
        let mut other = caf(GORE_FRAMES);
        other[0] = b'X';
        assert_eq!(caf_seconds(&other), None);
        assert_eq!(caf_seconds(&caf(GORE_FRAMES)[..CAF_HEADER_LEN]), None);
    }

    #[test]
    fn a_fragment_plays_its_clips_by_exit_time_speed_and_loop() {
        let gore_s = f64::from(GORE_FRAMES) * f64::from(SECONDS_PER_FRAME);
        let known = lengths(&[
            ("boar_gore", Ok(gore_s)),
            ("loop", Ok(1.0)),
            ("bad", Err("x".into())),
        ]);
        let one = [clip(0.0, "boar_gore", GORE_SPEED, false)];
        let FragmentLength::Seconds(seconds) = fragment_length(&one, &known).expect("length")
        else {
            panic!("seconds");
        };
        assert!((f64::from(seconds) - gore_s / GORE_SPEED).abs() < 1e-5);
        let cut = [
            clip(0.0, "boar_gore", 1.0, false),
            clip(2.0, "", 1.0, false),
        ];
        assert_eq!(
            fragment_length(&cut, &known),
            Ok(FragmentLength::Seconds(2.0))
        );
        let chained = [
            clip(0.0, "loop", 1.0, false),
            clip(-1.0, "loop", 1.0, false),
        ];
        assert_eq!(
            fragment_length(&chained, &known),
            Ok(FragmentLength::Seconds(2.0))
        );
        let looping = [clip(0.0, "loop", 1.0, true)];
        assert_eq!(fragment_length(&looping, &known), Ok(FragmentLength::Loops));
        assert!(fragment_length(&[clip(0.0, "bad", 1.0, false)], &known).is_err());
        assert!(fragment_length(&[], &known).is_err());
    }

    #[test]
    fn names_and_paths_are_the_paks_form() {
        assert_eq!(base_name("animations/X/Boar_Gore.caf"), "boar_gore");
        assert_eq!(base_name("a\\b\\turn_comb.comb"), "turn_comb");
        assert_eq!(base_name("boar_combat_idle_turn"), "boar_combat_idle_turn");
    }

    /// The boar's `Attack_Gore` action (`boar_combat.actionlist`, build 25810808).
    const GORE_LIST: &str = r#"<ActionList><Actions>
<Action Name="Attack_Gore" FragmentName="Attack_Gore" Predictive="true">
 <ActivationCondition><IfHTNCAGEAction ActionName="Attack_Gore_CAGE_Task"/></ActivationCondition>
 <Activities>
  <HTNPlanDoneActivity>
   <Default InvalidatePlanOnExit="false" CompletionFragmentOffset="0.4"/>
   <Conditional Name="ChainAttack" CompletionFragmentOffset="0.8"/>
  </HTNPlanDoneActivity>
  <AnimationDrivenMotion><Default MotionMultiplier="1.00"/></AnimationDrivenMotion>
  <SetDesiredFacingBlendParam><Default FacingMode="AngleToTarget"/></SetDesiredFacingBlendParam>
  <SetMannequinTag ResetWhenReinstalled="true"><Default TagName="Alerted" OnEnter="Set" OnExit="NoEffect" DelayTimer="0.10"/></SetMannequinTag>
 </Activities>
</Action>
<Action Name="Reaction" FragmentName="r_R2_F"><Activities/></Action>
</Actions>
<Conditions><IfMannequinTag Name="ChainAttack" TagName="ChainAttack"/></Conditions>
</ActionList>"#;

    #[test]
    fn an_action_list_gives_tasks_completion_motion_and_tags() {
        let parsed = list_actions(GORE_LIST).expect("list");
        let gore = &parsed.actions[0];
        assert_eq!(gore.tasks, ["Attack_Gore_CAGE_Task"]);
        assert_eq!(
            gore.completion,
            Some((0.4, vec![("ChainAttack".to_owned(), 0.8)]))
        );
        assert_eq!(gore.motion_multiplier, Some(1.0));
        assert_eq!(gore.facing, Some(FacingMode::AngleToTarget));
        assert_eq!(parsed.actions[1].facing, None);
        assert_eq!(
            gore.tags,
            [(
                "Alerted".to_owned(),
                TagEffect::Set,
                TagEffect::NoEffect,
                0.1
            )]
        );
        assert!(
            parsed.actions[1].tasks.is_empty(),
            "the engine starts reactions"
        );
        assert_eq!(parsed.conditions[0].0, "ChainAttack");
        let mut decoder = TreeDecoder::default();
        let condition = cage_condition(&mut decoder, &parsed.conditions[0].1);
        assert_eq!(condition, CageCondition::Tag(decoder.intern("ChainAttack")));
    }

    #[test]
    fn a_damage_table_of_a_slice_is_a_datasheet_in_the_paks() {
        assert_eq!(
            datasheet_path("sharedassets/datatables/javelindata_damagetable_boar.xml"),
            "sharedassets/datatables/javelindata_damagetable_boar.datasheet"
        );
        assert_eq!(datasheet_path("other.datasheet"), "other.datasheet");
    }

    #[test]
    fn a_grid_names_its_lists_and_aliases() {
        let grid = r#"<Grid><ActionLists>
<ActionList Path="SharedAssets\SpringboardEntitites\Characters\BaseAI\CAGE\BaseAI_Stun.actionlist"><AliasMap><Action ActionName="Stun" Alias="BehaviorTreeTask"/></AliasMap></ActionList>
<ActionList Path="SharedAssets/Boar_Reaction.actionlist"><AliasMap><Action ActionName="Reaction" Alias="Reaction"/></AliasMap></ActionList>
</ActionLists></Grid>"#;
        let lists = grid_lists(grid).expect("grid");
        assert_eq!(
            lists.paths,
            [
                "sharedassets/springboardentitites/characters/baseai/cage/baseai_stun.actionlist",
                "sharedassets/boar_reaction.actionlist"
            ]
        );
        assert_eq!(
            lists.aliases[1],
            ("Reaction".to_owned(), "Reaction".to_owned())
        );
    }

    #[test]
    fn an_animation_database_gives_each_entry_its_base_layer() {
        let adb = r#"<AnimDB><FragmentList><Attack_Gore>
<Fragment BlendOutDuration="0.2" Tags=""><AnimLayer><Blend ExitTime="0" StartTime="0" Duration="0.1"/><Animation name="boar_gore" speed="0.85000002"/></AnimLayer>
<ProcLayer><Blend ExitTime="0.69999999" StartTime="0" Duration="0"/><Procedural type="CAGE-Damage"><ProceduralParams><DamageTableRow value="Attack_Gore"/></ProceduralParams></Procedural><Blend ExitTime="0.19999999"/><Procedural type=""/></ProcLayer>
</Fragment>
<Fragment Tags="Armadillo"><AnimLayer><Blend ExitTime="0"/><Animation name="armadillo_idle" flags="Loop"/></AnimLayer></Fragment>
</Attack_Gore></FragmentList></AnimDB>"#;
        let entries = adb_entries(adb).expect("adb");
        let clips = AnimDatabase::parse(adb).expect("clips");
        assert_eq!(entries.len(), clips.fragments.len());
        assert_eq!(entries[0][0].animation, "boar_gore");
        assert!((entries[0][0].speed - GORE_SPEED).abs() < 1e-6);
        assert!(entries[1][0].looping);
        assert_eq!(clips.fragments[1].tags, ["Armadillo"]);
        let damage = &clips.fragments[0].clips[0];
        assert!((damage.start_s - 0.7).abs() < 1e-6);
        assert_eq!(damage.param("DamageTableRow"), Some("Attack_Gore"));
    }

    fn uuid_of(class: Class) -> [u8; 16] {
        CLASSES
            .iter()
            .find(|(known, _)| *known == class)
            .map(|(_, uuid)| *uuid)
            .expect("class")
    }

    fn typed(name: &str, class: Class, value: Vec<u8>, children: Vec<Element>) -> Element {
        Element {
            name_crc: Some(crc(name)),
            version: None,
            type_id: uuid_of(class),
            value,
            children,
        }
    }

    fn float(name: &str, value: f32) -> Element {
        typed(name, Class::Float, value.to_be_bytes().to_vec(), Vec::new())
    }

    fn text(name: &str, value: &str) -> Element {
        typed(name, Class::String, value.as_bytes().to_vec(), Vec::new())
    }

    fn asset(name: &str, hint: &str) -> Element {
        let mut value = vec![1; nw_ai::decode::ASSET_HINT_LEN_AT];
        value.extend_from_slice(&(hint.len() as u64).to_be_bytes());
        value.extend_from_slice(hint.as_bytes());
        typed(name, Class::Asset, value, Vec::new())
    }

    fn position(name: &str) -> Element {
        typed(
            name,
            Class::PositionType,
            Vec::new(),
            vec![typed(
                "PositionType",
                Class::Int,
                0_i32.to_be_bytes().to_vec(),
                Vec::new(),
            )],
        )
    }

    /// The boar's `RunToTarget` config (slice `boar.dynamicslice`).
    fn run_config() -> Element {
        let mut select = vec![position("Origin"), position("Anchor")];
        for name in [
            "Direction",
            "PositionSelectMode",
            "MaxAttempts",
            "MaxAttemptsParam",
        ] {
            select.push(typed(
                name,
                Class::Int,
                0_i32.to_be_bytes().to_vec(),
                Vec::new(),
            ));
        }
        for name in ["CheckPathToOrigin", "CheckPathToAnchor", "FavorMiddleAngle"] {
            select.push(typed(name, Class::Bool, vec![0], Vec::new()));
        }
        for name in [
            "StartAngle",
            "StopAngle",
            "RangeMin",
            "RangeMax",
            "DistanceFromWall",
            "QueryRadius",
            "StartAngleParam",
            "StopAngleParam",
            "RangeMinParam",
            "RangeMaxParam",
            "DistanceFromWallParam",
            "QueryRadiusParam",
            "GroupRange",
        ] {
            select.push(float(name, 1.5));
        }
        typed(
            "element",
            Class::Node,
            Vec::new(),
            vec![
                text(NAV_CONFIG_TYPE, "RunToTarget"),
                float(NAV_TURN_RATE, 120.0),
                float(NAV_ARRIVAL, 0.5),
                float(NAV_MIN_SPEED, 2.0),
                float(NAV_MAX_SPEED, 7.0),
                float(NAV_ACCELERATION, 30.0),
                float(NAV_DECELERATION, 30.0),
                float(NAV_REPATH, 0.5),
                typed(NAV_SELECT_POSITION, Class::Parameters, Vec::new(), select),
                typed(
                    NAV_ACTION_PARAMS,
                    Class::Node,
                    Vec::new(),
                    vec![float("m_agentRadius", 0.5)],
                ),
                typed(NAV_TELEPORT, Class::Bool, vec![0], Vec::new()),
                text(NAV_TURN_ACTION, "Combat_Turn_ToNav_CAGE_Task"),
                text(NAV_START_ACTION, ""),
                typed(
                    NAV_START_RULE,
                    Class::Int,
                    1_i32.to_be_bytes().to_vec(),
                    Vec::new(),
                ),
            ],
        )
    }

    #[test]
    fn a_slice_gives_its_tree_config_navigation_and_files() {
        let any = |value: Element| typed(CONFIG_VALUE, Class::Any, Vec::new(), vec![value]);
        let config = typed(
            CONFIG_PARAMS,
            Class::Node,
            Vec::new(),
            vec![typed(
                "element",
                Class::Node,
                Vec::new(),
                vec![
                    text(CONFIG_NAME, "CombatLeashDistance"),
                    any(float("m_data", 70.0)),
                ],
            )],
        );
        let trees = typed(
            TREE_ASSETS,
            Class::Node,
            Vec::new(),
            vec![typed(
                "element",
                Class::Node,
                Vec::new(),
                vec![asset(
                    ASSET_REF,
                    "SharedAssets/AI/BehaviorTrees/Predator/Boar/Boar_Base.behaviortree",
                )],
            )],
        );
        let database = typed(
            ANIMATION_DATABASE,
            Class::Node,
            Vec::new(),
            vec![typed(
                "BaseClass1",
                Class::Node,
                Vec::new(),
                vec![text(
                    ASSET_PATH,
                    "Animations/Mannequin/ADB/NPC_Boar_Anims.adb",
                )],
            )],
        );
        let navigation = typed(NAV_CONFIGS, Class::Node, Vec::new(), vec![run_config()]);
        let root = typed(
            "root",
            Class::Node,
            Vec::new(),
            vec![
                config,
                trees,
                database,
                navigation,
                float(MAX_SLOPE, 70.0),
                asset(ACTION_GRID, "SharedAssets/Boar.grid"),
            ],
        );
        let mut decoder = TreeDecoder::default();
        let slice = SliceAi::read(&root, &mut decoder, "boar").expect("slice");
        assert_eq!(
            slice.trees,
            ["sharedassets/ai/behaviortrees/predator/boar/boar_base.behaviortree"]
        );
        assert_eq!(
            slice.config,
            [("CombatLeashDistance".to_owned(), Value::Float(70.0))]
        );
        assert_eq!(
            slice.animation_database.as_deref(),
            Some("animations/mannequin/adb/npc_boar_anims.adb")
        );
        assert_eq!(slice.grid.as_deref(), Some("sharedassets/boar.grid"));
        assert_eq!(slice.max_slope_deg, Some(70.0));
        let run = &slice.nav_configs[0];
        assert_eq!(decoder.name(run.kind), "RunToTarget");
        assert_eq!(
            (run.max_speed, run.min_speed, run.turn_rate_deg),
            (7.0, 2.0, 120.0)
        );
        assert_eq!(
            run.turn_action
                .map(|task| decoder.name(task).to_owned())
                .as_deref(),
            Some("Combat_Turn_ToNav_CAGE_Task")
        );
        assert_eq!(run.start_action, None);
        assert_eq!(
            run.action_params,
            [(crc("m_agentRadius"), Value::Float(0.5))]
        );
        assert!(run.select_position.is_some());
    }
}
