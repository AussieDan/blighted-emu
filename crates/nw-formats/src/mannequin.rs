// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Mannequin and CAGE files that number a creature's animations (creature-animation note,
//! 2026-09-18): the controller definitions (`*_controllerdefs.xml`: the fragments and scopes),
//! the tag definition (`*tags.xml`: the bits of the tag state) and the CAGE grid (`*.grid`: the
//! actions).
//!
//! A creature's replicated animation names a fragment by its index + 1 in `<FragmentDefs>`, an
//! action by its index + 1 among the grid's actions, and its tags as a 12-byte tag state.
//!
//! An animation database (`*.adb`, [`AnimDatabase`]) holds the fragments themselves: per fragment
//! name, one entry per tag set, each with procedural layers of timed clips (`CAGE-Damage`,
//! `Homing`, transitions, ...). An action list (`*.actionlist`, [`ActionList`]) names the fragment
//! of each action.

use std::collections::HashMap;

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

/// Bytes of a tag state (the client's bit assignment, `FUN_146d88770`, fills 12 bytes).
pub const TAG_STATE_LEN: usize = 12;
/// Bits in one byte of the tag state.
const BITS_PER_BYTE: u8 = 8;

const CONTROLLER_DEF: &str = "ControllerDef";
const TAGS_ELEMENT: &str = "Tags";
const FRAGMENT_DEFS: &str = "FragmentDefs";
const SCOPE_DEFS: &str = "ScopeDefs";
const FILENAME_ATTRIBUTE: &str = "filename";
const TAG_DEFINITION: &str = "TagDefinition";
const GROUP_ELEMENT: &str = "Group";
const TAG_ELEMENT: &str = "Tag";
const NAME_ATTRIBUTE: &str = "name";
const ACTION_ELEMENT: &str = "Action";
const ACTION_NAME_ATTRIBUTE: &str = "ActionName";
const ALIAS_ATTRIBUTE: &str = "Alias";
const ACTION_LISTS_ELEMENT: &str = "ActionLists";
const ACTION_LIST_ELEMENT: &str = "ActionList";
const PATH_ATTRIBUTE: &str = "Path";
const FRAGMENT_LIST: &str = "FragmentList";
const FRAGMENT_ELEMENT: &str = "Fragment";
const FRAGMENT_TAGS_ATTRIBUTE: &str = "Tags";
const FRAGMENT_FRAG_TAGS_ATTRIBUTE: &str = "FragTags";
const PROC_LAYER: &str = "ProcLayer";
const ANIM_LAYER: &str = "AnimLayer";
const ANIMATION_ELEMENT: &str = "Animation";
const START_TIME_ATTRIBUTE: &str = "StartTime";
const SPEED_ATTRIBUTE: &str = "speed";
const FLAGS_ATTRIBUTE: &str = "flags";
const WEIGHT_ATTRIBUTE: &str = "weight";
const LOOP_FLAG: &str = "Loop";
/// The speed and weight of an animation clip that does not write them.
const DEFAULT_SPEED: f64 = 1.0;
const DEFAULT_WEIGHT: f64 = 1.0;
const BLEND_ELEMENT: &str = "Blend";
const EXIT_TIME_ATTRIBUTE: &str = "ExitTime";
const DURATION_ATTRIBUTE: &str = "Duration";
const PROCEDURAL_ELEMENT: &str = "Procedural";
const TYPE_ATTRIBUTE: &str = "type";
const PROCEDURAL_PARAMS: &str = "ProceduralParams";
const VALUE_ATTRIBUTE: &str = "value";
const ACTION_LIST_NAME_ATTRIBUTE: &str = "Name";
const FRAGMENT_NAME_ATTRIBUTE: &str = "FragmentName";
const CONDITIONS_ELEMENT: &str = "Conditions";
const AND_ELEMENT: &str = "And";
const OR_ELEMENT: &str = "Or";
const NOT_ELEMENT: &str = "Not";
const TRUE_ELEMENT: &str = "True";
const TAG_TEST_ELEMENT: &str = "IfMannequinTag";
const TAG_NAME_ATTRIBUTE: &str = "TagName";
const MARKER_TEST_ELEMENT: &str = "IfMannequinMarker";
const MARKER_NAME_ATTRIBUTE: &str = "MarkerName";
const IF_THEN_ELSE_ELEMENT: &str = "IfThenElse";
const IF_CONDITION_ELEMENT: &str = "IfCondition";
const THEN_ACTIVITY_ELEMENT: &str = "ThenActivity";
const CAST_SPELL_ELEMENT: &str = "CastSpell";
const CAST_RAYCAST_ELEMENT: &str = "CastSpellRaycast";
const CAST_TARGETING_ELEMENT: &str = "CastSpellTargeting";
const ELSE_ACTIVITY_ELEMENT: &str = "ElseActivity";
const WEAPON_REQUIREMENTS_ATTRIBUTE: &str = "WeaponRequirements";
/// The separator of a grid list's weapon classes (`Musket+Blunderbuss`).
const WEAPON_CLASS_SEPARATOR: char = '+';
const INPUT_TEST_ELEMENT: &str = "IfInput";
const INPUT_NAME_ATTRIBUTE: &str = "InputName";
const EXTERNAL_TEST_ELEMENT: &str = "IfExternalCondition";
const ACTION_STATUS_TEST_ELEMENT: &str = "IfActionStatus";
const STATUS_ATTRIBUTE: &str = "Status";
/// `IfActionStatus Status` of a running action.
const INSTALLED_STATUS: &str = "Installed";
const FRAGMENT_PLAYING_TEST_ELEMENT: &str = "IfFragmentPlaying";
const FRAGMENT_DONE_TEST_ELEMENT: &str = "IfFragmentDone";
const USE_HOST_FRAGMENT_ATTRIBUTE: &str = "UseHostActionFragment";
const EXIT_OFFSET_ATTRIBUTE: &str = "ExitOffsetSeconds";
const ABILITY_ACTIVE_TEST_ELEMENT: &str = "IfAbilityActive";
const ITEM_IN_SLOT_TEST_ELEMENT: &str = "IfItemInSlot";
const SLOT_ALIAS_ATTRIBUTE: &str = "SlotToCheckAlias";
const TAG_IN_SLOT_TEST_ELEMENT: &str = "IfMannequinTagInItemSlot";
const CONFIG_VAR_TEST_ELEMENT: &str = "IfStackConfigVar";
const STACK_VAR_ATTRIBUTE: &str = "StackVar";
const CONDITION_NAME_ATTRIBUTE: &str = "ConditionName";
const ABILITY_MOVE_TEST_ELEMENT: &str = "IfActiveAbilityMoveName";
const ABILITY_NAME_ATTRIBUTE: &str = "AbilityName";
const MANA_COST_TEST_ELEMENT: &str = "IfManaCost";
const COMPARISON_ATTRIBUTE: &str = "ComparisonType";
const LESS_THAN: &str = "LessThan";
const LESS_THAN_OR_EQUAL: &str = "LessThanOrEqual";
const COST_ID_ATTRIBUTE: &str = "CostID";
const COOLDOWN_TEST_ELEMENT: &str = "IfCooldownTimer";
const ABILITY_ID_ATTRIBUTE: &str = "AbilityID";
const PERFORM_ABILITY_ELEMENT: &str = "PerformActiveAbility";
const SET_COOLDOWN_ELEMENT: &str = "SetCooldownTimer";
const APPLY_ON_ENTER_ELEMENT: &str = "EffectsToApplyOnEnter";
const EFFECT_ID_ELEMENT: &str = "EffectId";
const EFFECT_ID_ATTRIBUTE: &str = "EffectId";
/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;
/// How deep named conditions may name others (a named condition that names itself stops there).
const MAX_EXTERNAL_DEPTH: usize = 8;
const DEFAULT_ELEMENT: &str = "Default";
const SPELL_NAME_ATTRIBUTE: &str = "SpellName";
const STATUS_EFFECT_TEST_ELEMENT: &str = "IfHasStatusEffect";
const STATUS_EFFECT_ATTRIBUTE: &str = "StatusEffect";
const ACTIVATION_CONDITION_ELEMENT: &str = "ActivationCondition";
const DEACTIVATION_CONDITION_ELEMENT: &str = "DeactivationCondition";
const SET_TAG_ELEMENT: &str = "SetMannequinTag";
const ACTIVITIES_ELEMENT: &str = "Activities";
const DRIVEN_MOTION_ELEMENT: &str = "AnimationDrivenMotion";
const CONDITIONAL_ELEMENT: &str = "Conditional";
const MOTION_MULTIPLIER_ATTRIBUTE: &str = "MotionMultiplier";
const DECEL_ATTRIBUTE: &str = "Decel";
const INHERIT_VELOCITY_ATTRIBUTE: &str = "InheritVelocity";
const INHERIT_MAX_SPEED_ATTRIBUTE: &str = "InheritVelocityMaxSpeed";
const TRUE_VALUE: &str = "true";
/// The separator of a vector attribute's parts (`Decel="9,9,9"`).
const VECTOR_SEPARATOR: char = ',';
/// The separator of a fragment's tags (`1H_Melee+Sword`).
const TAG_SEPARATOR: char = '+';

/// A Mannequin or CAGE file that cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum MannequinError {
    /// The XML is not well formed.
    #[error("mannequin XML: {0}")]
    Xml(#[from] quick_xml::Error),
    /// An attribute cannot be read.
    #[error("mannequin XML attribute: {0}")]
    Attribute(#[from] quick_xml::events::attributes::AttrError),
    /// The root or a needed element is missing.
    #[error("mannequin XML has no <{0}>")]
    Missing(&'static str),
    /// A needed attribute is missing.
    #[error("mannequin XML <{element}> has no {attribute}")]
    MissingAttribute {
        /// The element.
        element: &'static str,
        /// The attribute.
        attribute: &'static str,
    },
    /// A tag definition needs more bits than a tag state holds.
    #[error("tag definition needs more than {TAG_STATE_LEN} bytes")]
    TooManyTags,
    /// A grid names an action list (pak path) that is not given.
    #[error("grid action list {0} is missing")]
    MissingActionList(String),
}

/// One element of a small XML file: its name, attributes and child elements in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// The element name.
    pub name: String,
    /// The attributes, name and value, in file order.
    pub attributes: Vec<(String, String)>,
    /// The child elements, in file order.
    pub children: Vec<Node>,
}

impl Node {
    /// The value of attribute `key`; `None` when the element has none.
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The first child element `name`.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError::Missing`] when it has none.
    pub fn child(&self, name: &'static str) -> Result<&Self, MannequinError> {
        self.children
            .iter()
            .find(|child| child.name == name)
            .ok_or(MannequinError::Missing(name))
    }

    /// Every element below it with name `name`, in document order.
    pub fn descendants<'a>(&'a self, name: &'a str, found: &mut Vec<&'a Self>) {
        for child in &self.children {
            if child.name == name {
                found.push(child);
            }
            child.descendants(name, found);
        }
    }
}

fn node_of(start: &BytesStart<'_>) -> Result<Node, MannequinError> {
    let attributes = start
        .attributes()
        .map(|attribute| {
            let attribute = attribute?;
            let key = attribute.key.as_ref().to_owned();
            let value = attribute.normalized_value(XmlVersion::Implicit1_0)?;
            Ok((key, value.into_owned()))
        })
        .collect::<Result<Vec<_>, MannequinError>>()?;
    Ok(Node {
        name: start.name().as_ref().to_owned(),
        attributes,
        children: Vec::new(),
    })
}

/// The root element of `text`, with its element tree (text and comments are left out).
///
/// # Errors
///
/// Returns [`MannequinError`] when the XML is not well formed or has no root element.
pub fn parse_tree(text: &str) -> Result<Node, MannequinError> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event()? {
            Event::Start(start) => stack.push(node_of(&start)?),
            Event::Empty(start) => {
                let node = node_of(&start)?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            }
            Event::End(_) => {
                let Some(node) = stack.pop() else {
                    break;
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    stack
        .pop()
        .and_then(|document| document.children.into_iter().next())
        .ok_or(MannequinError::Missing("root"))
}

/// A creature's Mannequin controller definitions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerDef {
    /// The tag definition file (`<Tags filename>`), as written (any case).
    pub tags_file: String,
    /// The fragment names, in `<FragmentDefs>` order: fragment `n` is at `n - 1`.
    pub fragments: Vec<String>,
    /// The scope names, in `<ScopeDefs>` order: a scope's bit in `scopeData` is its index.
    pub scopes: Vec<String>,
}

impl ControllerDef {
    /// Reads `*_controllerdefs.xml`.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError`] when the XML or a needed element is missing.
    pub fn parse(text: &str) -> Result<Self, MannequinError> {
        let root = parse_tree(text)?;
        if root.name != CONTROLLER_DEF {
            return Err(MannequinError::Missing(CONTROLLER_DEF));
        }
        let tags_file = root
            .child(TAGS_ELEMENT)?
            .attribute(FILENAME_ATTRIBUTE)
            .ok_or(MannequinError::MissingAttribute {
                element: TAGS_ELEMENT,
                attribute: FILENAME_ATTRIBUTE,
            })?
            .to_owned();
        let names = |element| -> Result<Vec<String>, MannequinError> {
            Ok(root
                .child(element)?
                .children
                .iter()
                .map(|child| child.name.clone())
                .collect())
        };
        Ok(Self {
            tags_file,
            fragments: names(FRAGMENT_DEFS)?,
            scopes: names(SCOPE_DEFS)?,
        })
    }
}

/// Where one tag's value goes in the tag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagBits {
    /// The byte of the tag state.
    pub byte: u8,
    /// The first bit of its group (or of the tag) in that byte.
    pub shift: u8,
    /// The tag's value in its group: its index + 1 (1 for a tag without a group).
    pub value: u8,
}

/// A Mannequin tag definition: the bits of each tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagDefinition {
    tags: HashMap<String, TagBits>,
}

impl TagDefinition {
    /// Reads `*tags.xml` and gives each tag its bits as the client does (`FUN_146d88770`, after
    /// the load in `FUN_146e4a9a0`): byte by byte, first every group not yet placed that still
    /// fits in the byte, in file order (a group of `n` tags takes the bits of the number `n`,
    /// its tags the values 1 to `n`), then single tags in the bits left, in file order.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError`] when the XML or `<Tags>` is missing, or the tags need more
    /// than [`TAG_STATE_LEN`] bytes.
    pub fn parse(text: &str) -> Result<Self, MannequinError> {
        let root = parse_tree(text)?;
        if root.name != TAG_DEFINITION {
            return Err(MannequinError::Missing(TAG_DEFINITION));
        }
        let entries = &root.child(TAGS_ELEMENT)?.children;
        let name_of = |node: &Node| node.attribute(NAME_ATTRIBUTE).map(str::to_owned);
        let groups: Vec<Vec<String>> = entries
            .iter()
            .filter(|node| node.name == GROUP_ELEMENT)
            .map(|group| group.children.iter().filter_map(name_of).collect())
            .collect();
        let singles: Vec<String> = entries
            .iter()
            .filter(|node| node.name == TAG_ELEMENT)
            .filter_map(name_of)
            .collect();
        let mut tags = HashMap::new();
        let mut groups_left: Vec<&Vec<String>> = groups.iter().collect();
        let mut singles_left = singles.iter();
        let mut byte: u8 = 0;
        while !groups_left.is_empty() || singles_left.len() > 0 {
            if usize::from(byte) >= TAG_STATE_LEN {
                return Err(MannequinError::TooManyTags);
            }
            let mut used: u8 = 0;
            groups_left.retain(|group| {
                // A group of `n` tags holds the values 1 to `n`: the bits of `n`.
                let width =
                    u8::try_from(usize::BITS - group.len().leading_zeros()).unwrap_or(u8::MAX);
                if used + width > BITS_PER_BYTE {
                    return true;
                }
                for (index, name) in group.iter().enumerate() {
                    let value = u8::try_from(index + 1).unwrap_or(u8::MAX);
                    tags.insert(
                        name.clone(),
                        TagBits {
                            byte,
                            shift: used,
                            value,
                        },
                    );
                }
                used += width;
                false
            });
            while used < BITS_PER_BYTE {
                let Some(name) = singles_left.next() else {
                    break;
                };
                tags.insert(
                    name.clone(),
                    TagBits {
                        byte,
                        shift: used,
                        value: 1,
                    },
                );
                used += 1;
            }
            byte += 1;
        }
        Ok(Self { tags })
    }

    /// Every tag with its bits.
    pub fn iter(&self) -> impl Iterator<Item = (&String, TagBits)> {
        self.tags.iter().map(|(name, bits)| (name, *bits))
    }

    /// Whether the definition has tag `name`.
    pub fn has(&self, name: &str) -> bool {
        self.tags.contains_key(name)
    }

    /// The tag state that sets `names`; a name the definition does not have sets nothing.
    pub fn state<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> [u8; TAG_STATE_LEN] {
        let mut state = [0; TAG_STATE_LEN];
        for bits in names.into_iter().filter_map(|name| self.tags.get(name)) {
            state[usize::from(bits.byte)] |= bits.value << bits.shift;
        }
        state
    }
}

impl FromIterator<(String, TagBits)> for TagDefinition {
    /// The definition of tags whose bits were given before (a stored table).
    fn from_iter<T: IntoIterator<Item = (String, TagBits)>>(tags: T) -> Self {
        Self {
            tags: tags.into_iter().collect(),
        }
    }
}

/// A creature's CAGE grid: its actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CageGrid {
    /// The action names, in the client's order: action `n` is at `n - 1`.
    pub actions: Vec<String>,
}

/// One action of a grid action list's `<AliasMap>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridAction {
    /// `ActionName`: the name of an action of the list.
    pub name: String,
    /// `Alias`; `None` when the entry has none (retail: the swarmer's `Equip`).
    pub alias: Option<String>,
}

/// One `<ActionList>` of a grid's `<ActionLists>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridList {
    /// `Path`, as the grid writes it.
    pub path: String,
    /// Its `<AliasMap>` entries, in file order.
    pub aliased: Vec<GridAction>,
    /// `WeaponRequirements`, split at `+`: the list is on while the weapon in hand has one of
    /// these item classes (player grid: `LifeStaff`, `Musket+Blunderbuss`); empty when the
    /// list has none.
    pub weapon_classes: Vec<String>,
}

impl GridList {
    /// Its path as the paks name it, [`pak_path`].
    pub fn pak_path(&self) -> String {
        pak_path(&self.path)
    }

    /// Whether its `<AliasMap>` names action `name` (the client compares the names without
    /// case: `_stricmp` in `FUN_142a4f6c0`).
    fn names(&self, name: &str) -> bool {
        self.aliased
            .iter()
            .any(|action| action.name.eq_ignore_ascii_case(name))
    }
}

/// `path` as the paks name it: lower case with forward slashes (some grids write backslashes,
/// for example `SharedAssets\SpringboardEntitites\Characters\BaseAI\CAGE\BaseAI_Stun.actionlist`).
pub fn pak_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

impl CageGrid {
    /// The action lists of `*.grid`, in file order: the `<ActionList>`s of its `<ActionLists>`
    /// (the `<ReactionTables>` lists name no actions).
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError`] when the XML is not well formed.
    pub fn lists(text: &str) -> Result<Vec<GridList>, MannequinError> {
        let root = parse_tree(text)?;
        Ok(root
            .children
            .iter()
            .filter(|child| child.name == ACTION_LISTS_ELEMENT)
            .flat_map(|lists| &lists.children)
            .filter(|list| list.name == ACTION_LIST_ELEMENT)
            .filter_map(|list| {
                let mut aliased = Vec::new();
                list.descendants(ACTION_ELEMENT, &mut aliased);
                Some(GridList {
                    path: list.attribute(PATH_ATTRIBUTE)?.to_owned(),
                    weapon_classes: list
                        .attribute(WEAPON_REQUIREMENTS_ATTRIBUTE)
                        .map(|classes| {
                            classes
                                .split(WEAPON_CLASS_SEPARATOR)
                                .map(str::trim)
                                .filter(|class| !class.is_empty())
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    aliased: aliased
                        .into_iter()
                        .filter_map(|action| {
                            Some(GridAction {
                                name: action.attribute(ACTION_NAME_ATTRIBUTE)?.to_owned(),
                                alias: action.attribute(ALIAS_ATTRIBUTE).map(str::to_owned),
                            })
                        })
                        .collect(),
                })
            })
            .collect())
    }

    /// Numbers the actions of the grid `lists`: list by list in grid order, the actions of the
    /// list file (`list_of`, by [`GridList::pak_path`]) in the file's order, each one that the
    /// grid's `<AliasMap>` names, with or without an `Alias`. The `<AliasMap>`'s own order does
    /// not count.
    ///
    /// Observed (retail capture `newchar-retail-2026-09-18`): with this numbering every
    /// full-body scope entry of the 13 creatures in it (boars, tutorial sailors, miner,
    /// legionnaire, signifier, swarmer, risen, Thorpe, beach first mate) names an action that
    /// plays the entry's fragment; numbered in the `<AliasMap>`'s order, Thorpe's attacks are 1
    /// to 2 lower than retail (`Thorpe_Combat.actionlist` has `Invasion_Special` second, its
    /// grid names it last) and the swarmer counts `Equip` (no `Alias`) where retail does.
    /// Inferred (client `FUN_142a4f6c0`): the client keeps an `<AliasMap>` as a tree keyed by
    /// name and finds each entry in the list's action array, so the array (file order) holds
    /// the numbers.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError::MissingActionList`] when `list_of` has no list of the grid:
    /// the numbers after it are then Unknown.
    pub fn number<'a>(
        lists: &[GridList],
        list_of: impl Fn(&str) -> Option<&'a ActionList>,
    ) -> Result<Self, MannequinError> {
        let mut actions = Vec::new();
        for list in lists {
            let path = list.pak_path();
            let file = list_of(&path).ok_or(MannequinError::MissingActionList(path))?;
            actions.extend(
                file.actions
                    .iter()
                    .filter(|action| list.names(&action.name))
                    .map(|action| action.name.clone()),
            );
        }
        Ok(Self { actions })
    }
}

/// One clip of a procedural layer: its type, when it starts and ends (seconds after the fragment
/// starts) and its parameters. A clip lasts until the next clip of its layer starts (an empty
/// clip of type `""` ends the one before it); the last clip of a layer lasts to the fragment's
/// end, `end_s` `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcClip {
    /// The clip type (`CAGE-Damage`, `Homing`, `CAGE-TransitionInstant`, ...).
    pub kind: String,
    /// Its start: the sum of the layer's blend exit times up to it.
    pub start_s: f64,
    /// Its end: the next clip's start; `None` for the last clip of its layer.
    pub end_s: Option<f64>,
    /// Its `<ProceduralParams>`, element name and `value`, in file order.
    pub params: Vec<(String, String)>,
    /// The index of its layer among the fragment's procedural layers.
    pub layer: usize,
}

/// One clip of an animation layer: the animation it plays from when, at which point of the
/// animation and how fast. A clip plays until the next clip of its layer starts (an empty
/// `<Animation name="">` ends the one before it), else to the animation's end.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimClip {
    /// The animation name (`grtswrd_prim_c1`).
    pub name: String,
    /// When it starts: the sum of the layer's blend exit times up to it, seconds after the
    /// fragment starts.
    pub start_s: f64,
    /// The point of the animation it starts at (the blend's `StartTime`), seconds (Observed fit:
    /// `grtswrd_prim_c1` at 0.1448 s and 1.15 times as fast puts the greatsword's
    /// 0.433-0.533 s damage window on the clip's horizontal swing, frames 19 to 23).
    pub anim_start_s: f64,
    /// Its blend-in time (the blend's `Duration`), seconds.
    pub blend_s: f64,
    /// How fast it plays (`speed`, 1 when not written).
    pub speed: f64,
    /// Whether it loops (`flags` has `Loop`).
    pub looped: bool,
    /// Its weight (`weight`, 1 when not written); a clip of weight 0 shows nothing.
    pub weight: f64,
    /// When the next clip of its layer starts; `None` for the last.
    pub end_s: Option<f64>,
}

impl ProcClip {
    /// The value of parameter `key`; `None` when the clip has none.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// One entry of a fragment: the tags it plays for and its procedural clips.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fragment {
    /// The fragment name (`Attack_Primary`).
    pub name: String,
    /// The tags of the entry (`Tags`, split at `+`); empty for the default entry.
    pub tags: Vec<String>,
    /// The fragment tags of the entry (`FragTags`); empty for most.
    pub frag_tags: Vec<String>,
    /// The clips of every procedural layer, layer by layer; empty clips left out.
    pub clips: Vec<ProcClip>,
    /// The clips of every animation layer, one list per layer, in file order.
    pub anim_layers: Vec<Vec<AnimClip>>,
}

impl Fragment {
    /// The body's animation: the first clip of the animation layers with a weight above 0, else
    /// the first clip (Inferred: the dagger's attacks and the rapier's third have only clips of
    /// weight 0, and they swing the weapon in the game, so the clip is played all the same).
    pub fn main_animation(&self) -> Option<&AnimClip> {
        let clips = || self.anim_layers.iter().flatten();
        clips()
            .find(|clip| clip.weight > 0.0)
            .or_else(|| clips().next())
    }

    /// The clips of type `kind`, in layer order.
    pub fn clips_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a ProcClip> {
        self.clips.iter().filter(move |clip| clip.kind == kind)
    }
}

fn split_tags(text: Option<&str>) -> Vec<String> {
    text.unwrap_or_default()
        .split(TAG_SEPARATOR)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A blend's `ExitTime`.
fn exit_time(blend: &Node) -> Result<f64, MannequinError> {
    blend
        .attribute(EXIT_TIME_ATTRIBUTE)
        .and_then(|text| text.trim().parse::<f64>().ok())
        .ok_or(MannequinError::MissingAttribute {
            element: BLEND_ELEMENT,
            attribute: EXIT_TIME_ATTRIBUTE,
        })
}

/// A number attribute `key` of `node`, `default` when it is not written or not a number.
fn number_or(node: &Node, key: &str, default: f64) -> f64 {
    node.attribute(key)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

/// The clips of one `<AnimLayer>`: start times are the running sum of the blend exit times.
fn anim_clips(layer: &Node) -> Result<Vec<AnimClip>, MannequinError> {
    let mut time_s = 0.0;
    let mut blend: Option<&Node> = None;
    let mut clips: Vec<AnimClip> = Vec::new();
    for node in &layer.children {
        match node.name.as_str() {
            BLEND_ELEMENT => {
                time_s += exit_time(node)?;
                blend = Some(node);
            }
            ANIMATION_ELEMENT => {
                if let Some(last) = clips.last_mut().filter(|last| last.end_s.is_none()) {
                    last.end_s = Some(time_s);
                }
                let name = node.attribute(NAME_ATTRIBUTE).unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                clips.push(AnimClip {
                    name: name.to_owned(),
                    start_s: time_s,
                    anim_start_s: blend
                        .map_or(0.0, |blend| number_or(blend, START_TIME_ATTRIBUTE, 0.0)),
                    blend_s: blend.map_or(0.0, |blend| number_or(blend, DURATION_ATTRIBUTE, 0.0)),
                    speed: number_or(node, SPEED_ATTRIBUTE, DEFAULT_SPEED),
                    looped: node
                        .attribute(FLAGS_ATTRIBUTE)
                        .is_some_and(|flags| flags.split('+').any(|flag| flag.trim() == LOOP_FLAG)),
                    weight: number_or(node, WEIGHT_ATTRIBUTE, DEFAULT_WEIGHT),
                    end_s: None,
                });
            }
            _ => {}
        }
    }
    Ok(clips)
}

/// The clips of one `<ProcLayer>`: start times are the running sum of the blend exit times.
fn layer_clips(layer: &Node, index: usize) -> Result<Vec<ProcClip>, MannequinError> {
    let mut time_s = 0.0;
    let mut clips: Vec<ProcClip> = Vec::new();
    for node in &layer.children {
        match node.name.as_str() {
            BLEND_ELEMENT => time_s += exit_time(node)?,
            PROCEDURAL_ELEMENT => {
                if let Some(last) = clips.last_mut().filter(|last| last.end_s.is_none()) {
                    last.end_s = Some(time_s);
                }
                let kind = node.attribute(TYPE_ATTRIBUTE).unwrap_or_default();
                if kind.is_empty() {
                    continue;
                }
                let params = node
                    .children
                    .iter()
                    .filter(|child| child.name == PROCEDURAL_PARAMS)
                    .flat_map(|params| &params.children)
                    .map(|param| {
                        (
                            param.name.clone(),
                            param
                                .attribute(VALUE_ATTRIBUTE)
                                .unwrap_or_default()
                                .to_owned(),
                        )
                    })
                    .collect();
                clips.push(ProcClip {
                    kind: kind.to_owned(),
                    start_s: time_s,
                    end_s: None,
                    params,
                    layer: index,
                });
            }
            _ => {}
        }
    }
    Ok(clips)
}

/// A Mannequin animation database (`*.adb`): its fragments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimDatabase {
    /// Every fragment entry, in file order.
    pub fragments: Vec<Fragment>,
}

impl AnimDatabase {
    /// Reads `*.adb`: every `<Fragment>` of every fragment name in its `<FragmentList>`.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError`] when the XML, its `<FragmentList>` or a blend's exit time is
    /// missing.
    pub fn parse(text: &str) -> Result<Self, MannequinError> {
        let root = parse_tree(text)?;
        let mut fragments = Vec::new();
        for named in &root.child(FRAGMENT_LIST)?.children {
            for entry in named
                .children
                .iter()
                .filter(|entry| entry.name == FRAGMENT_ELEMENT)
            {
                let mut clips = Vec::new();
                for (index, layer) in entry
                    .children
                    .iter()
                    .filter(|layer| layer.name == PROC_LAYER)
                    .enumerate()
                {
                    clips.extend(layer_clips(layer, index)?);
                }
                let anim_layers = entry
                    .children
                    .iter()
                    .filter(|layer| layer.name == ANIM_LAYER)
                    .map(anim_clips)
                    .collect::<Result<_, _>>()?;
                fragments.push(Fragment {
                    name: named.name.clone(),
                    tags: split_tags(entry.attribute(FRAGMENT_TAGS_ATTRIBUTE)),
                    frag_tags: split_tags(entry.attribute(FRAGMENT_FRAG_TAGS_ATTRIBUTE)),
                    clips,
                    anim_layers,
                });
            }
        }
        Ok(Self { fragments })
    }

    /// The entry of fragment `name` that Mannequin plays for the tag state `tags`: the first of
    /// its [`AnimDatabase::options`]; `None` when no entry fits.
    pub fn best(&self, name: &str, tags: &[&str]) -> Option<&Fragment> {
        self.options(name, tags).into_iter().next()
    }

    /// The entries of fragment `name` that Mannequin picks among for the tag state `tags` (its
    /// options): of the entries without fragment tags whose every tag is in `tags` (any case),
    /// those with the most tags, in file order.
    pub fn options(&self, name: &str, tags: &[&str]) -> Vec<&Fragment> {
        let has = |tag: &String| tags.iter().any(|wanted| wanted.eq_ignore_ascii_case(tag));
        let fitting: Vec<&Fragment> = self
            .fragments
            .iter()
            .filter(|fragment| {
                fragment.name == name
                    && fragment.frag_tags.is_empty()
                    && fragment.tags.iter().all(has)
            })
            .collect();
        let most = fitting.iter().map(|fragment| fragment.tags.len()).max();
        fitting
            .into_iter()
            .filter(|fragment| Some(fragment.tags.len()) == most)
            .collect()
    }
}

/// A CAGE action list condition, as far as a character's tags, the markers of its fragment and
/// its inputs decide it: the logic elements, `True`, a Mannequin tag, a fragment marker, an
/// input and a named condition of the list; any other test (a status effect, an ability, a
/// lock, ...) is [`Condition::Other`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Condition {
    /// `<And>`: every part holds.
    All(Vec<Condition>),
    /// `<Or>`: a part holds.
    Any(Vec<Condition>),
    /// `<Not>`: its part does not hold.
    Not(Box<Condition>),
    /// `<True/>`.
    True,
    /// `<IfMannequinTag TagName>`: the character has the tag.
    Tag(String),
    /// `<IfMannequinMarker MarkerName>`: a clip of the playing fragment sets the marker.
    Marker(String),
    /// `<IfInput InputName>`: the input is on (a key press, a held key or a virtual input).
    Input(String),
    /// `<IfExternalCondition ConditionName>` of a condition the list does not name (a named one
    /// is put in its place, [`ActionList::parse`]).
    External(String),
    /// `<IfActionStatus ActionName Status>`: the action runs (`Installed`), else waits to
    /// (`Pending`).
    ActionStatus {
        /// The action.
        action: String,
        /// `Status="Installed"`.
        installed: bool,
    },
    /// `<IfFragmentPlaying FragmentName>`: the fragment plays; `None`: the action's own
    /// (`UseHostActionFragment`).
    FragmentPlaying(Option<String>),
    /// `<IfFragmentDone FragmentName ExitOffsetSeconds>`: the fragment is done, this long
    /// before its end; `None`: the action's own.
    FragmentDone {
        /// The fragment.
        fragment: Option<String>,
        /// `ExitOffsetSeconds`, ms.
        exit_offset_ms: u32,
    },
    /// `<IfActiveAbilityMoveName AbilityName>`: the key of this mapped ability is pressed.
    AbilityMove(String),
    /// `<IfCooldownTimer AbilityID>`: the ability's cooldown runs.
    Cooldown(String),
    /// `<IfManaCost CostID ComparisonType>`: the mana compares with the cost so.
    ManaCost {
        /// `CostID`.
        cost: String,
        /// `ComparisonType`.
        compare: ManaCompare,
    },
    /// `<IfHasStatusEffect StatusEffect>`: the character has the effect.
    StatusEffect(String),
    /// `<IfAbilityActive AbilityName>`: the passive ability (an upgrade perk) is on.
    AbilityActive(String),
    /// `<IfItemInSlot SlotToCheckAlias>`: an item is in the slot.
    ItemInSlot(String),
    /// `<IfMannequinTagInItemSlot TagName SlotToCheckAlias>`: the item in the slot has the
    /// Mannequin tag (`Flail` in `ACTIVE_MAIN_HAND_WEAPON`: a flail is out).
    TagInSlot {
        /// `TagName`.
        tag: String,
        /// `SlotToCheckAlias`.
        slot: String,
    },
    /// `<IfStackConfigVar StackVar>`: the client setting is on.
    ConfigVar(String),
    /// Any other test, by element name.
    Other(String),
}

/// How an `IfManaCost` compares the mana with the cost (`ComparisonType`; the retail lists use
/// these three: 83 `GreaterThanOrEqual`, 8 `LessThan`, 5 `LessThanOrEqual`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ManaCompare {
    /// `GreaterThanOrEqual`: the mana covers the cost.
    AtLeast,
    /// `LessThan`.
    Below,
    /// `LessThanOrEqual`.
    AtMost,
}

impl ManaCompare {
    /// Whether `mana` compares with `cost` so.
    pub fn holds(self, mana: f32, cost: f32) -> bool {
        match self {
            Self::AtLeast => mana >= cost,
            Self::Below => mana < cost,
            Self::AtMost => mana <= cost,
        }
    }
}

/// What the tests of an action list read from a character. A test this world does not know is
/// false.
pub trait CageWorld {
    /// It has Mannequin tag `tag` (any case).
    fn tag(&self, tag: &str) -> bool;
    /// Its playing fragment sets `marker`.
    fn marker(&self, marker: &str) -> bool;
    /// Its input `input` is on.
    fn input(&self, input: &str) -> bool;
    /// Action `action` runs (`installed`), else waits to.
    fn action_status(&self, _action: &str, _installed: bool) -> bool {
        false
    }
    /// Fragment `fragment` plays (`None`: the testing action's own).
    fn fragment_playing(&self, _fragment: Option<&str>) -> bool {
        false
    }
    /// Fragment `fragment` is done, `exit_offset_ms` before its end (`None`: the testing
    /// action's own).
    fn fragment_done(&self, _fragment: Option<&str>, _exit_offset_ms: u32) -> bool {
        false
    }
    /// The key of mapped ability `ability` is pressed.
    fn ability_move(&self, _ability: &str) -> bool {
        false
    }
    /// The cooldown of `ability` runs.
    fn cooldown(&self, _ability: &str) -> bool {
        false
    }
    /// The mana compares with cost `cost` as `compare` says.
    fn mana_cost(&self, _cost: &str, _compare: ManaCompare) -> bool {
        false
    }
    /// It has status effect `effect`.
    fn status_effect(&self, _effect: &str) -> bool {
        false
    }
    /// Passive ability `ability` is on.
    fn ability_active(&self, _ability: &str) -> bool {
        false
    }
    /// An item is in slot `slot`.
    fn item_in_slot(&self, _slot: &str) -> bool {
        false
    }
    /// The item in slot `slot` has Mannequin tag `tag`.
    fn tag_in_slot(&self, _tag: &str, _slot: &str) -> bool {
        false
    }
    /// Client setting `variable` is on.
    fn config_var(&self, _variable: &str) -> bool {
        false
    }
    /// Another test, by element name.
    fn other(&self, _test: &str) -> bool {
        false
    }
}

/// What the tests of an action list read from a character: its Mannequin tags (any case), the
/// markers its playing fragment sets and its inputs that are on; no other state.
#[derive(Debug, Clone, Copy, Default)]
pub struct CageState<'a> {
    /// Its tags.
    pub tags: &'a [&'a str],
    /// The markers of its fragment.
    pub markers: &'a [&'a str],
    /// Its inputs that are on.
    pub inputs: &'a [&'a str],
}

impl CageWorld for CageState<'_> {
    fn tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|own| own.eq_ignore_ascii_case(tag))
    }

    fn marker(&self, marker: &str) -> bool {
        self.markers.contains(&marker)
    }

    fn input(&self, input: &str) -> bool {
        self.inputs.contains(&input)
    }
}

impl Condition {
    fn of(node: &Node) -> Self {
        let parts = || node.children.iter().map(Self::of).collect();
        let attribute = |key| node.attribute(key).unwrap_or_default().to_owned();
        // The action's own fragment, or the one it names.
        let fragment = || {
            (node.attribute(USE_HOST_FRAGMENT_ATTRIBUTE) != Some(TRUE_VALUE))
                .then(|| node.attribute(FRAGMENT_NAME_ATTRIBUTE))
                .flatten()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        };
        match node.name.as_str() {
            AND_ELEMENT => Self::All(parts()),
            OR_ELEMENT => Self::Any(parts()),
            NOT_ELEMENT => Self::Not(Box::new(Self::All(parts()))),
            TRUE_ELEMENT => Self::True,
            TAG_TEST_ELEMENT => Self::Tag(
                node.attribute(TAG_NAME_ATTRIBUTE)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            MARKER_TEST_ELEMENT => Self::Marker(
                node.attribute(MARKER_NAME_ATTRIBUTE)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            INPUT_TEST_ELEMENT => Self::Input(
                node.attribute(INPUT_NAME_ATTRIBUTE)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            EXTERNAL_TEST_ELEMENT => Self::External(attribute(CONDITION_NAME_ATTRIBUTE)),
            ACTION_STATUS_TEST_ELEMENT => Self::ActionStatus {
                action: attribute(ACTION_NAME_ATTRIBUTE),
                installed: node.attribute(STATUS_ATTRIBUTE) == Some(INSTALLED_STATUS),
            },
            FRAGMENT_PLAYING_TEST_ELEMENT => Self::FragmentPlaying(fragment()),
            FRAGMENT_DONE_TEST_ELEMENT => Self::FragmentDone {
                fragment: fragment(),
                exit_offset_ms: node
                    .attribute(EXIT_OFFSET_ATTRIBUTE)
                    .and_then(|offset| offset.parse::<f64>().ok())
                    // A fraction of a fragment's length.
                    .map_or(0, |seconds| (seconds * MS_PER_SECOND).round() as u32),
            },
            ABILITY_MOVE_TEST_ELEMENT => Self::AbilityMove(attribute(ABILITY_NAME_ATTRIBUTE)),
            COOLDOWN_TEST_ELEMENT => Self::Cooldown(attribute(ABILITY_ID_ATTRIBUTE)),
            MANA_COST_TEST_ELEMENT => Self::ManaCost {
                cost: attribute(COST_ID_ATTRIBUTE),
                compare: match node.attribute(COMPARISON_ATTRIBUTE) {
                    Some(LESS_THAN) => ManaCompare::Below,
                    Some(LESS_THAN_OR_EQUAL) => ManaCompare::AtMost,
                    _ => ManaCompare::AtLeast,
                },
            },
            STATUS_EFFECT_TEST_ELEMENT => Self::StatusEffect(attribute(STATUS_EFFECT_ATTRIBUTE)),
            ABILITY_ACTIVE_TEST_ELEMENT => Self::AbilityActive(attribute(ABILITY_NAME_ATTRIBUTE)),
            ITEM_IN_SLOT_TEST_ELEMENT => Self::ItemInSlot(attribute(SLOT_ALIAS_ATTRIBUTE)),
            TAG_IN_SLOT_TEST_ELEMENT => Self::TagInSlot {
                tag: attribute(TAG_NAME_ATTRIBUTE),
                slot: attribute(SLOT_ALIAS_ATTRIBUTE),
            },
            CONFIG_VAR_TEST_ELEMENT => Self::ConfigVar(attribute(STACK_VAR_ATTRIBUTE)),
            other => Self::Other(other.to_owned()),
        }
    }

    /// Whether it holds for a character with the tags `tags` (any case) whose fragment sets the
    /// markers `markers`, and no other state: no input, status effect, active ability or lock,
    /// so [`Condition::Other`] is false.
    pub fn holds(&self, tags: &[&str], markers: &[&str]) -> bool {
        self.holds_in(&CageState {
            tags,
            markers,
            inputs: &[],
        })
    }

    /// Whether it holds for a character in `world`; an unnamed [`Condition::External`] is
    /// false.
    pub fn holds_in(&self, world: &dyn CageWorld) -> bool {
        match self {
            Self::All(parts) => parts.iter().all(|part| part.holds_in(world)),
            Self::Any(parts) => parts.iter().any(|part| part.holds_in(world)),
            Self::Not(part) => !part.holds_in(world),
            Self::True => true,
            Self::Tag(tag) => world.tag(tag),
            Self::Marker(marker) => world.marker(marker),
            Self::Input(input) => world.input(input),
            Self::External(_) => false,
            Self::ActionStatus { action, installed } => world.action_status(action, *installed),
            Self::FragmentPlaying(fragment) => world.fragment_playing(fragment.as_deref()),
            Self::FragmentDone {
                fragment,
                exit_offset_ms,
            } => world.fragment_done(fragment.as_deref(), *exit_offset_ms),
            Self::AbilityMove(ability) => world.ability_move(ability),
            Self::Cooldown(ability) => world.cooldown(ability),
            Self::ManaCost { cost, compare } => world.mana_cost(cost, *compare),
            Self::StatusEffect(effect) => world.status_effect(effect),
            Self::AbilityActive(ability) => world.ability_active(ability),
            Self::ItemInSlot(slot) => world.item_in_slot(slot),
            Self::TagInSlot { tag, slot } => world.tag_in_slot(tag, slot),
            Self::ConfigVar(variable) => world.config_var(variable),
            Self::Other(test) => world.other(test),
        }
    }

    /// The condition with each [`Condition::External`] that `named` names put in its place,
    /// itself resolved, down to [`MAX_EXTERNAL_DEPTH`] names deep.
    fn resolved(self, named: &[(String, Condition)], depth: usize) -> Self {
        let parts = |parts: Vec<Self>| {
            parts
                .into_iter()
                .map(|part| part.resolved(named, depth))
                .collect()
        };
        match self {
            Self::All(list) => Self::All(parts(list)),
            Self::Any(list) => Self::Any(parts(list)),
            Self::Not(part) => Self::Not(Box::new(part.resolved(named, depth))),
            Self::External(name) if depth < MAX_EXTERNAL_DEPTH => named
                .iter()
                .find(|(own, _)| *own == name)
                .map_or(Self::External(name), |(_, condition)| {
                    condition.clone().resolved(named, depth + 1)
                }),
            other => other,
        }
    }

    /// The element name of a test that reads more than tags and markers (an input, a named
    /// condition, an action, a fragment, an ability, a cooldown, mana, an effect, a slot or
    /// another test); `None` for the logic elements, `True`, a tag and a marker.
    pub fn test_element(&self) -> Option<&str> {
        Some(match self {
            Self::All(_)
            | Self::Any(_)
            | Self::Not(_)
            | Self::True
            | Self::Tag(_)
            | Self::Marker(_) => {
                return None;
            }
            Self::Input(_) => INPUT_TEST_ELEMENT,
            Self::External(_) => EXTERNAL_TEST_ELEMENT,
            Self::ActionStatus { .. } => ACTION_STATUS_TEST_ELEMENT,
            Self::FragmentPlaying(_) => FRAGMENT_PLAYING_TEST_ELEMENT,
            Self::FragmentDone { .. } => FRAGMENT_DONE_TEST_ELEMENT,
            Self::AbilityMove(_) => ABILITY_MOVE_TEST_ELEMENT,
            Self::Cooldown(_) => COOLDOWN_TEST_ELEMENT,
            Self::ManaCost { .. } => MANA_COST_TEST_ELEMENT,
            Self::StatusEffect(_) => STATUS_EFFECT_TEST_ELEMENT,
            Self::AbilityActive(_) => ABILITY_ACTIVE_TEST_ELEMENT,
            Self::ItemInSlot(_) => ITEM_IN_SLOT_TEST_ELEMENT,
            Self::TagInSlot { .. } => TAG_IN_SLOT_TEST_ELEMENT,
            Self::ConfigVar(_) => CONFIG_VAR_TEST_ELEMENT,
            Self::Other(test) => test,
        })
    }

    /// The markers it tests, in order.
    pub fn markers(&self) -> Vec<&str> {
        match self {
            Self::All(parts) | Self::Any(parts) => parts.iter().flat_map(Self::markers).collect(),
            Self::Not(part) => part.markers(),
            Self::Marker(marker) => vec![marker.as_str()],
            _ => Vec::new(),
        }
    }
}

/// One row of an `AnimationDrivenMotion` activity: what it sets, each `None` where the row
/// leaves it out.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MotionRow {
    /// `MotionMultiplier`: the factor on the animation's root motion.
    pub multiplier: Option<f32>,
    /// `Decel` on the ground plane (the character's x and y), m/s².
    pub decel_mps2: Option<[f32; 2]>,
    /// `InheritVelocity`: the motion keeps the character's velocity at the start.
    pub inherit_velocity: Option<bool>,
    /// `InheritVelocityMaxSpeed`, m/s.
    pub inherit_max_mps: Option<f32>,
}

impl MotionRow {
    fn of(node: &Node) -> Self {
        let number = |key| node.attribute(key)?.trim().parse::<f32>().ok();
        Self {
            multiplier: number(MOTION_MULTIPLIER_ATTRIBUTE),
            decel_mps2: node.attribute(DECEL_ATTRIBUTE).and_then(|text| {
                let mut parts = text
                    .split(VECTOR_SEPARATOR)
                    .map(|part| part.trim().parse().ok());
                Some([parts.next()??, parts.next()??])
            }),
            inherit_velocity: node
                .attribute(INHERIT_VELOCITY_ATTRIBUTE)
                .map(|value| value.eq_ignore_ascii_case(TRUE_VALUE)),
            inherit_max_mps: number(INHERIT_MAX_SPEED_ATTRIBUTE),
        }
    }

    /// The row with each value it leaves out taken from `base`.
    #[must_use]
    pub fn or(self, base: Self) -> Self {
        Self {
            multiplier: self.multiplier.or(base.multiplier),
            decel_mps2: self.decel_mps2.or(base.decel_mps2),
            inherit_velocity: self.inherit_velocity.or(base.inherit_velocity),
            inherit_max_mps: self.inherit_max_mps.or(base.inherit_max_mps),
        }
    }
}

/// An action's `AnimationDrivenMotion` activity: the animation's root motion moves the
/// character, not its movement input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrivenMotion {
    /// The `Default` row.
    pub default: MotionRow,
    /// The `Conditional` rows: the named condition and the row, in file order.
    pub conditionals: Vec<(String, MotionRow)>,
}

impl DrivenMotion {
    fn of(node: &Node) -> Self {
        let mut motion = Self::default();
        for row in &node.children {
            if row.name == DEFAULT_ELEMENT {
                motion.default = MotionRow::of(row);
            } else if row.name == CONDITIONAL_ELEMENT
                && let Some(name) = row.attribute(ACTION_LIST_NAME_ATTRIBUTE)
            {
                motion
                    .conditionals
                    .push((name.to_owned(), MotionRow::of(row)));
            }
        }
        motion
    }

    /// The row for a character with the tags `tags` whose fragment sets the markers `markers`:
    /// the first conditional whose condition (by `condition`) holds, its left-out values the
    /// default's, else the default (Inferred: the rows of the build's attacks read in file
    /// order).
    pub fn row<'a>(
        &self,
        condition: impl Fn(&str) -> Option<&'a Condition>,
        tags: &[&str],
        markers: &[&str],
    ) -> MotionRow {
        self.conditionals
            .iter()
            .find(|(name, _)| condition(name).is_some_and(|test| test.holds(tags, markers)))
            .map_or(self.default, |(_, row)| row.or(self.default))
    }
}

/// One action of an action list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    /// `Name`.
    pub name: String,
    /// `FragmentName`.
    pub fragment: String,
    /// The spells its activities cast, in file order.
    pub casts: Vec<SpellCast>,
    /// Its activation on a status effect: an `<ActivationCondition>` whose `And` holds one test
    /// `<IfHasStatusEffect>` (not under a `Not`), the effect and the rest of the `And`.
    pub on_status_effect: Option<(String, Condition)>,
    /// The Mannequin tags its `SetMannequinTag` activities set while it is active, in file order.
    pub sets_tags: Vec<String>,
    /// Its `AnimationDrivenMotion` activity, when one is among its activities (not under a
    /// branch).
    pub driven_motion: Option<DrivenMotion>,
    /// Its `<ActivationCondition>`: it starts while this holds; `None` when it has none (it
    /// does not start by itself).
    pub activation: Option<Condition>,
    /// Its `<DeactivationCondition>`: it stops when this holds; `None` when it has none.
    pub deactivation: Option<Condition>,
    /// Its active ability parts; `None` for an action that tests and performs no ability.
    pub ability: Option<AbilityUse>,
}

/// How a cast activity aims its spell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CastKind {
    /// `CastSpell`: from the caster, as its spell row says.
    Plain,
    /// `CastSpellRaycast`: where the caster's aim ray meets something.
    Raycast,
    /// `CastSpellTargeting`: at the point the player picks with its targeting slice.
    Targeting,
}

/// One spell an action casts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpellCast {
    /// The conditions of the branches it is in; `True` for an unconditional cast.
    pub condition: Condition,
    /// The `SpellData` id.
    pub spell: String,
    /// How it aims.
    pub kind: CastKind,
}

/// The active ability parts of an action (the `playeractions_ability_*` lists).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AbilityUse {
    /// `IfActiveAbilityMoveName AbilityName` of its activation (not under a `Not`): the mapped
    /// ability whose key starts it.
    pub move_name: Option<String>,
    /// `IfManaCost CostID` of its activation (not under a `Not`): the cost the mana must cover.
    pub mana_cost: Option<String>,
    /// `IfCooldownTimer AbilityID` of its activation under a `Not`: the cooldowns that must be
    /// over.
    pub needs_ready: Vec<String>,
    /// `PerformActiveAbility AbilityName` activities, in file order, once each.
    pub performs: Vec<String>,
    /// `SetCooldownTimer AbilityID` activities, in file order, once each.
    pub starts_cooldowns: Vec<String>,
    /// The `EffectId`s of its `EffectsToApplyOnEnter` rows (of its `ModifyStatusEffects`), in
    /// file order, once each.
    pub applies_on_enter: Vec<String>,
}

/// The child `element` of `action` (`<ActivationCondition>`, ...) as one condition: all its parts.
fn condition_of(action: &Node, element: &str) -> Option<Condition> {
    action
        .children
        .iter()
        .find(|child| child.name == element)
        .map(|test| Condition::All(test.children.iter().map(Condition::of).collect()))
}

/// The tests named `element` of `node` and below, split by whether a `Not` holds them.
fn tests<'a>(node: &'a Node, element: &str, negated: bool, found: &mut Vec<(&'a Node, bool)>) {
    for child in &node.children {
        if child.name == element {
            found.push((child, negated));
        }
        tests(child, element, negated ^ (child.name == NOT_ELEMENT), found);
    }
}

/// The values of attribute `key` of the elements `element` below `node`, once each, in order.
fn attribute_values(node: &Node, element: &str, key: &str) -> Vec<String> {
    let mut nodes = Vec::new();
    node.descendants(element, &mut nodes);
    let mut values: Vec<String> = Vec::new();
    for value in nodes
        .into_iter()
        .flat_map(|found| std::iter::once(found).chain(&found.children))
        .filter_map(|found| found.attribute(key))
    {
        if !values.iter().any(|own| own == value) {
            values.push(value.to_owned());
        }
    }
    values
}

fn ability_use(action: &Node) -> Option<AbilityUse> {
    let activation = action
        .children
        .iter()
        .find(|child| child.name == ACTIVATION_CONDITION_ELEMENT);
    let found = |element: &str| {
        let mut found = Vec::new();
        if let Some(activation) = activation {
            tests(activation, element, false, &mut found);
        }
        found
    };
    let first_positive = |element: &str, key: &str| {
        found(element)
            .into_iter()
            .find(|(_, negated)| !negated)
            .and_then(|(test, _)| test.attribute(key).map(str::to_owned))
    };
    let mut effect_rows = Vec::new();
    action.descendants(APPLY_ON_ENTER_ELEMENT, &mut effect_rows);
    let mut applies_on_enter: Vec<String> = Vec::new();
    for effect in effect_rows
        .into_iter()
        .flat_map(|row| &row.children)
        .filter(|child| child.name == EFFECT_ID_ELEMENT)
        .filter_map(|child| child.attribute(EFFECT_ID_ATTRIBUTE))
    {
        if !applies_on_enter.iter().any(|own| own == effect) {
            applies_on_enter.push(effect.to_owned());
        }
    }
    let ability = AbilityUse {
        move_name: first_positive(ABILITY_MOVE_TEST_ELEMENT, ABILITY_NAME_ATTRIBUTE),
        mana_cost: first_positive(MANA_COST_TEST_ELEMENT, COST_ID_ATTRIBUTE),
        needs_ready: found(COOLDOWN_TEST_ELEMENT)
            .into_iter()
            .filter(|(_, negated)| *negated)
            .filter_map(|(test, _)| test.attribute(ABILITY_ID_ATTRIBUTE).map(str::to_owned))
            .collect(),
        performs: attribute_values(action, PERFORM_ABILITY_ELEMENT, ABILITY_NAME_ATTRIBUTE),
        starts_cooldowns: attribute_values(action, SET_COOLDOWN_ELEMENT, ABILITY_ID_ATTRIBUTE),
        applies_on_enter,
    };
    (ability.move_name.is_some()
        || !ability.performs.is_empty()
        || !ability.starts_cooldowns.is_empty())
    .then_some(ability)
}

fn driven_motion(action: &Node) -> Option<DrivenMotion> {
    action
        .children
        .iter()
        .filter(|child| child.name == ACTIVITIES_ELEMENT)
        .flat_map(|activities| &activities.children)
        .find(|activity| activity.name == DRIVEN_MOTION_ELEMENT)
        .map(DrivenMotion::of)
}

/// The parts of the `And` elements from `node` down, `And` inside `And` flattened.
fn and_parts<'a>(node: &'a Node, parts: &mut Vec<&'a Node>) {
    for child in &node.children {
        if child.name == AND_ELEMENT {
            and_parts(child, parts);
        } else {
            parts.push(child);
        }
    }
}

fn on_status_effect(action: &Node) -> Option<(String, Condition)> {
    let test = action
        .children
        .iter()
        .find(|child| child.name == ACTIVATION_CONDITION_ELEMENT)?;
    let mut parts = Vec::new();
    and_parts(test, &mut parts);
    let (effects, rest): (Vec<&Node>, Vec<&Node>) = parts
        .into_iter()
        .partition(|part| part.name == STATUS_EFFECT_TEST_ELEMENT);
    let [effect] = effects.as_slice() else {
        return None;
    };
    Some((
        effect.attribute(STATUS_EFFECT_ATTRIBUTE)?.to_owned(),
        Condition::All(rest.into_iter().map(Condition::of).collect()),
    ))
}

fn sets_tags(action: &Node) -> Vec<String> {
    let mut found = Vec::new();
    action.descendants(SET_TAG_ELEMENT, &mut found);
    found
        .into_iter()
        .flat_map(|set| &set.children)
        .filter(|child| child.name == DEFAULT_ELEMENT)
        .filter_map(|default| default.attribute(TAG_NAME_ATTRIBUTE).map(str::to_owned))
        .collect()
}

/// The spells its activities cast, each with the conditions of the branches it is in (`True`
/// for an unconditional cast), in file order.
fn casts(action: &Node) -> Vec<SpellCast> {
    let mut found = Vec::new();
    for activities in action
        .children
        .iter()
        .filter(|child| child.name == ACTIVITIES_ELEMENT)
    {
        branch_casts(activities, &[], &mut found);
    }
    found
}

/// The spell an activity casts and how: `<CastSpell><Default SpellName>`, or a raycast or
/// targeting cast that names it.
fn spell_of(activity: &Node) -> Option<(&str, CastKind)> {
    match activity.name.as_str() {
        CAST_SPELL_ELEMENT => Some((
            activity
                .children
                .iter()
                .find(|child| child.name == DEFAULT_ELEMENT)?
                .attribute(SPELL_NAME_ATTRIBUTE)?,
            CastKind::Plain,
        )),
        CAST_RAYCAST_ELEMENT => {
            Some((activity.attribute(SPELL_NAME_ATTRIBUTE)?, CastKind::Raycast))
        }
        CAST_TARGETING_ELEMENT => Some((
            activity.attribute(SPELL_NAME_ATTRIBUTE)?,
            CastKind::Targeting,
        )),
        _ => None,
    }
}

fn branch_casts(node: &Node, conditions: &[Condition], found: &mut Vec<SpellCast>) {
    for child in &node.children {
        if let Some((spell, kind)) = spell_of(child) {
            let condition = if conditions.is_empty() {
                Condition::True
            } else {
                Condition::All(conditions.to_vec())
            };
            found.push(SpellCast {
                condition,
                spell: spell.to_owned(),
                kind,
            });
        } else if child.name == IF_THEN_ELSE_ELEMENT {
            let Some(test) = child
                .children
                .iter()
                .find(|part| part.name == IF_CONDITION_ELEMENT)
                .map(|test| Condition::All(test.children.iter().map(Condition::of).collect()))
            else {
                continue;
            };
            for (branch, holds) in [
                (THEN_ACTIVITY_ELEMENT, true),
                (ELSE_ACTIVITY_ELEMENT, false),
            ] {
                if let Some(activity) = child.children.iter().find(|part| part.name == branch) {
                    let mut inner = conditions.to_vec();
                    inner.push(if holds {
                        test.clone()
                    } else {
                        Condition::Not(Box::new(test.clone()))
                    });
                    branch_casts(activity, &inner, found);
                }
            }
        } else {
            branch_casts(child, conditions, found);
        }
    }
}

/// A CAGE action list (`*.actionlist`): its actions and its named conditions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionList {
    /// Every `<Action>` with a `Name`, in file order (an action without a `FragmentName`, such
    /// as one that only sets a tag, has an empty fragment).
    pub actions: Vec<Action>,
    /// The named conditions of its `<Conditions>`, in file order.
    pub conditions: Vec<(String, Condition)>,
}

impl ActionList {
    /// Reads `*.actionlist`.
    ///
    /// # Errors
    ///
    /// Returns [`MannequinError`] when the XML is not well formed.
    pub fn parse(text: &str) -> Result<Self, MannequinError> {
        let root = parse_tree(text)?;
        let mut actions = Vec::new();
        root.descendants(ACTION_ELEMENT, &mut actions);
        let conditions: Vec<(String, Condition)> = root
            .children
            .iter()
            .filter(|child| child.name == CONDITIONS_ELEMENT)
            .flat_map(|conditions| &conditions.children)
            .filter_map(|condition| {
                Some((
                    condition.attribute(ACTION_LIST_NAME_ATTRIBUTE)?.to_owned(),
                    Condition::of(condition),
                ))
            })
            .collect();
        Ok(Self {
            actions: actions
                .into_iter()
                .filter_map(|action| {
                    Some(Action {
                        name: action.attribute(ACTION_LIST_NAME_ATTRIBUTE)?.to_owned(),
                        fragment: action
                            .attribute(FRAGMENT_NAME_ATTRIBUTE)
                            .unwrap_or_default()
                            .to_owned(),
                        casts: casts(action)
                            .into_iter()
                            .map(|cast| SpellCast {
                                condition: cast.condition.resolved(&conditions, 0),
                                ..cast
                            })
                            .collect(),
                        on_status_effect: on_status_effect(action)
                            .map(|(effect, rest)| (effect, rest.resolved(&conditions, 0))),
                        sets_tags: sets_tags(action),
                        driven_motion: driven_motion(action),
                        activation: condition_of(action, ACTIVATION_CONDITION_ELEMENT)
                            .map(|condition| condition.resolved(&conditions, 0)),
                        deactivation: condition_of(action, DEACTIVATION_CONDITION_ELEMENT)
                            .map(|condition| condition.resolved(&conditions, 0)),
                        ability: ability_use(action),
                    })
                })
                .collect(),
            conditions,
        })
    }

    /// The action `name`.
    pub fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.name == name)
    }

    /// The fragment of action `name`; `None` for an action without one.
    pub fn fragment(&self, name: &str) -> Option<&str> {
        self.action(name)
            .map(|action| action.fragment.as_str())
            .filter(|fragment| !fragment.is_empty())
    }

    /// The named condition `name` with the named conditions it names put in their place.
    pub fn resolved_condition(&self, name: &str) -> Option<Condition> {
        self.condition(name)
            .map(|condition| condition.clone().resolved(&self.conditions, 1))
    }

    /// The named condition `name`.
    pub fn condition(&self, name: &str) -> Option<&Condition> {
        self.conditions
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, condition)| condition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boar's tag definition (`boar_tags.xml`, build 25810808).
    const BOAR_TAGS: &str = r#"<TagDefinition version="2">
 <Tags>
  <Group name="Variant"><Tag name="Pig"/><Tag name="Boar"/><Tag name="Strange"/><Tag name="Armadillo"/></Group>
  <Group name="WeaponType"><Tag name="CurledBall"/><Tag name="DummyWeapon"/></Group>
  <Tag name="Alerted"/><Tag name="DisableLookPose"/><Tag name="ChainAttack"/><Tag name="BreakoutWindow"/>
  <Tag name="Stunned"/><Tag name="Reaction"/><Tag name="Reset"/><Tag name="End_Loop"/>
  <Tag name="ReactionLimiter_Active"/>
 </Tags>
</TagDefinition>"#;

    /// The risen's tag definition: groups that do not fit move to the next byte, and single tags
    /// fill the bits left (`risen_tags.xml`).
    const RISEN_TAGS: &str = r#"<TagDefinition version="2"><Tags>
  <Group name="Arms"><Tag name="Left_Arm"/><Tag name="Right_Arm"/><Tag name="Both_Arms"/></Group>
  <Group name="Family"><Tag name="Withered"/><Tag name="CorruptionConstruct"/><Tag name="BlightFiend"/><Tag name="Undead"/><Tag name="Invasion"/><Tag name="Inquisitor"/><Tag name="AngryEarth"/><Tag name="Lost"/></Group>
  <Group name="Variant"><Tag name="Base"/><Tag name="Regurgitator"/><Tag name="Impaler"/><Tag name="Bomber"/><Tag name="SwampFiend"/><Tag name="Shivers"/><Tag name="Flamekeeper"/><Tag name="Feculent"/><Tag name="Blackpowder"/><Tag name="SulfurEntity"/><Tag name="FeralGhoul"/></Group>
  <Group name="DifficultyTier"><Tag name="DT1"/><Tag name="DT3"/><Tag name="DT5"/><Tag name="Unstable"/></Group>
  <Group name="RunVariant"><Tag name="Run_v2"/><Tag name="Run_v3"/><Tag name="Run_v4"/><Tag name="Run_v5"/></Group>
  <Tag name="LookPose"/><Tag name="End_Loop"/><Tag name="Alerted"/><Tag name="Reaction"/>
</Tags></TagDefinition>"#;

    fn state_hex(definition: &TagDefinition, names: &[&str]) -> Vec<u8> {
        definition.state(names.iter().copied()).to_vec()
    }

    #[test]
    fn the_boar_tags_are_the_retail_bytes() {
        let tags = TagDefinition::parse(BOAR_TAGS).expect("tags");
        // Retail calm boar `02 00..`, alerted `22 00..`, POI end `02 10`.
        assert_eq!(state_hex(&tags, &["Boar"])[..2], [0x02, 0x00]);
        assert_eq!(state_hex(&tags, &["Boar", "Alerted"])[..2], [0x22, 0x00]);
        assert_eq!(state_hex(&tags, &["Boar", "End_Loop"])[..2], [0x02, 0x10]);
        assert_eq!(
            state_hex(&tags, &["Boar", "NotATag"])[..1],
            [0x02],
            "unknown: nothing"
        );
    }

    #[test]
    fn a_group_that_does_not_fit_moves_to_the_next_byte() {
        let tags = TagDefinition::parse(RISEN_TAGS).expect("tags");
        // Retail risen calm `0b 31`, alerted `0b b1`.
        let calm = ["Both_Arms", "CorruptionConstruct", "Base", "DT5"];
        assert_eq!(state_hex(&tags, &calm)[..2], [0x0b, 0x31]);
        let alert: Vec<&str> = calm.iter().copied().chain(["Alerted"]).collect();
        assert_eq!(state_hex(&tags, &alert)[..2], [0x0b, 0xb1]);
    }

    #[test]
    fn the_controller_numbers_fragments_and_scopes_in_file_order() {
        let text = r#"<ControllerDef>
 <Tags filename="Animations/Mannequin/ADB/NPC_Monster/Boar_Family/Boar_Tags.xml"/>
 <FragmentDefs><Idle scopes="FullBody"><Override tags="A" scopes="FullBody"/></Idle><Move scopes="FullBody"/></FragmentDefs>
 <ScopeDefs><FullBody layer="0"/><Additive layer="3"/><LookPose layer="7"/></ScopeDefs>
</ControllerDef>"#;
        let def = ControllerDef::parse(text).expect("defs");
        assert_eq!(def.fragments, ["Idle", "Move"]);
        assert_eq!(def.scopes, ["FullBody", "Additive", "LookPose"]);
        assert_eq!(
            def.tags_file,
            "Animations/Mannequin/ADB/NPC_Monster/Boar_Family/Boar_Tags.xml"
        );
    }

    /// A cut of `ftue_thorpe.grid` (build 25810808): its `<AliasMap>` order is not the list
    /// files' order, one `<AliasMap>` entry has no `Alias` (as the swarmer's `Equip` in
    /// `broken.grid`) and its reaction tables name no actions.
    const THORPE_GRID: &str = r#"<Grid><ActionLists>
  <ActionList Path="SharedAssets\Thorpe\Thorpe_Combat.actionlist"><AliasMap>
    <Action ActionName="Combat_Tell" Alias="Attack"/>
    <Action ActionName="Invasion_Special" Alias="Attack"/>
  </AliasMap></ActionList>
  <ActionList Path="SharedAssets/Broken_Global.actionlist"><AliasMap>
    <Action ActionName="LookPose" Alias="LookPose"/>
    <Action ActionName="equip" IsIgnoredByGrid="true"/>
  </AliasMap></ActionList>
</ActionLists><ReactionTables><ActionList Path="SharedAssets/Harness.actionlist"/></ReactionTables></Grid>"#;
    /// `Thorpe_Combat.actionlist` holds `Phase_Transition_1_to_2`, which this grid's
    /// `<AliasMap>` for it does not name.
    const THORPE_COMBAT: &str = r#"<ActionList><Actions>
  <Action Name="Invasion_Special" FragmentName="Invasion_Special"/>
  <Action Name="Combat_Tell" FragmentName="Combat_Tell"/>
  <Action Name="Phase_Transition_1_to_2" FragmentName="Phase_Transition_1_to_2"/>
</Actions></ActionList>"#;
    const BROKEN_GLOBAL: &str = r#"<ActionList><Actions>
  <Action Name="Equip" FragmentName=""/>
  <Action Name="LookPose" FragmentName="LookPose"/>
</Actions></ActionList>"#;
    const THORPE_COMBAT_PATH: &str = "sharedassets/thorpe/thorpe_combat.actionlist";
    const BROKEN_GLOBAL_PATH: &str = "sharedassets/broken_global.actionlist";

    #[test]
    fn a_grid_numbers_the_named_actions_in_list_file_order() {
        let lists = CageGrid::lists(THORPE_GRID).expect("grid");
        assert_eq!(
            lists.iter().map(GridList::pak_path).collect::<Vec<_>>(),
            [THORPE_COMBAT_PATH, BROKEN_GLOBAL_PATH]
        );
        assert_eq!(lists[1].aliased[1].alias, None);
        let files = HashMap::from([
            (
                THORPE_COMBAT_PATH,
                ActionList::parse(THORPE_COMBAT).expect("list"),
            ),
            (
                BROKEN_GLOBAL_PATH,
                ActionList::parse(BROKEN_GLOBAL).expect("list"),
            ),
        ]);
        let grid = CageGrid::number(&lists, |path| files.get(path)).expect("numbered");
        assert_eq!(
            grid.actions,
            ["Invasion_Special", "Combat_Tell", "Equip", "LookPose"]
        );
    }

    #[test]
    fn a_player_grid_list_names_its_weapon_classes() {
        // `player.grid`.
        let grid = r#"<Grid><ActionLists>
  <ActionList WeaponRequirements="Musket+Blunderbuss" Path="SharedAssets/x/PlayerActions_Ability_Musket.actionlist"><AliasMap/></ActionList>
  <ActionList Path="SharedAssets/x/PlayerActions_Nav.actionlist"><AliasMap/></ActionList>
</ActionLists></Grid>"#;
        let lists = CageGrid::lists(grid).expect("grid");
        assert_eq!(lists[0].weapon_classes, ["Musket", "Blunderbuss"]);
        assert!(
            lists[1].weapon_classes.is_empty(),
            "a list for every weapon"
        );
    }

    #[test]
    fn a_grid_without_one_of_its_lists_has_no_numbers() {
        let lists = CageGrid::lists(THORPE_GRID).expect("grid");
        let only_combat = ActionList::parse(THORPE_COMBAT).expect("list");
        let missing = CageGrid::number(&lists, |path| {
            (path == THORPE_COMBAT_PATH).then_some(&only_combat)
        });
        assert!(
            matches!(missing, Err(MannequinError::MissingActionList(path)) if path == BROKEN_GLOBAL_PATH)
        );
    }

    /// A cut of `player_greatsword.adb` (build 25810808): the light attack's homing and the charge
    /// fragment's transitions, with an empty clip and an `Audio` clip between them.
    const GREATSWORD_ADB: &str = r#"<AnimDB FragDef="x" TagDef="y"><FragmentList>
 <Attack_Primary>
  <Fragment BlendOutDuration="0.2" Tags="2H_Melee+Greatsword+Offhand"><ProcLayer/></Fragment>
  <Fragment BlendOutDuration="0.2" Tags="2H_Melee+Greatsword">
   <AnimLayer><Blend ExitTime="0" StartTime="0" Duration="0.2"/><Animation name="grtswrd_prim_c1"/></AnimLayer>
   <ProcLayer>
    <Blend ExitTime="0.26666668" StartTime="0" Duration="0"/>
    <Procedural type="Homing" contextType="HomingContext"><ProceduralParams>
     <Radius value="4.5"/><MaxAngle value="60"/><MoveToDistance value="1.2"/>
    </ProceduralParams></Procedural>
    <Blend ExitTime="0.2" StartTime="0" Duration="0"/>
    <Procedural type="" contextType=""/>
   </ProcLayer>
  </Fragment>
  <Fragment BlendOutDuration="0.2" Tags="2H_Melee+Greatsword" FragTags="Upperbody"><ProcLayer/></Fragment>
 </Attack_Primary>
 <Attack_Start_Charge>
  <Fragment BlendOutDuration="0.2" Tags="2H_Melee+Greatsword">
   <ProcLayer>
    <Blend ExitTime="0" StartTime="0" Duration="0"/>
    <Procedural type="CAGE-TransitionInstant" contextType="TransitionContext"><ProceduralParams>
     <Condition value="Attack_Heavy_NotHeld"/><NewAction value="Attack_Primary_C1"/>
    </ProceduralParams></Procedural>
    <Blend ExitTime="0.32901338" StartTime="0" Duration="0"/>
    <Procedural type="CAGE-TransitionInstant" contextType="TransitionContext"><ProceduralParams>
     <Condition value="Attack_Primary_Released"/><NewAction value="Attack_Heavy"/>
    </ProceduralParams></Procedural>
    <Blend ExitTime="0.33765331" StartTime="0" Duration="0"/>
    <Procedural type="CAGE-TransitionInstant" contextType="TransitionContext"><ProceduralParams>
     <Condition value="Test_AlwaysTrue"/><NewAction value="Attack_Heavy_Charged"/>
    </ProceduralParams></Procedural>
   </ProcLayer>
   <ProcLayer>
    <Blend ExitTime="0" StartTime="0" Duration="0"/>
    <Procedural type="Audio" contextType="AudioContext"><ProceduralParams><StartTrigger value="play"/></ProceduralParams></Procedural>
   </ProcLayer>
  </Fragment>
 </Attack_Start_Charge>
</FragmentList></AnimDB>"#;

    fn ms(seconds: f64) -> u64 {
        (seconds * MS_PER_SECOND).round() as u64
    }

    #[test]
    fn a_fragment_entry_has_its_clips_at_the_sum_of_the_exit_times() {
        let adb = AnimDatabase::parse(GREATSWORD_ADB).expect("adb");
        assert_eq!(adb.fragments.len(), 4);
        let charge = adb
            .best("Attack_Start_Charge", &["2H_Melee", "GreatSword"])
            .expect("charge");
        let transitions: Vec<(u64, Option<u64>, &str)> = charge
            .clips_of("CAGE-TransitionInstant")
            .map(|clip| {
                (
                    ms(clip.start_s),
                    clip.end_s.map(ms),
                    clip.param("NewAction").unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            transitions,
            [
                (0, Some(329), "Attack_Primary_C1"),
                (329, Some(667), "Attack_Heavy"),
                (667, None, "Attack_Heavy_Charged"),
            ]
        );
        assert_eq!(charge.clips_of("Audio").count(), 1, "the other layer");
        let primary = adb
            .best("Attack_Primary", &["2H_Melee", "GreatSword"])
            .expect("primary");
        let homing = primary.clips_of("Homing").next().expect("homing");
        assert_eq!((ms(homing.start_s), homing.end_s.map(ms)), (267, Some(467)));
        assert_eq!(homing.param("Radius"), Some("4.5"));
        assert_eq!(homing.param("Height"), None);
    }

    #[test]
    fn a_fragment_entry_has_its_animation_clips() {
        let text = r#"<AnimDB><FragmentList><Attack_Primary>
 <Fragment Tags="1H_Melee+Sword"><AnimLayer>
  <Blend ExitTime="0" StartTime="0" Duration="0.2"/><Animation name="rswrd_1h_lt_atk_01" weight="0"/>
 </AnimLayer><AnimLayer>
  <Blend ExitTime="0" StartTime="0.14480001" Duration="0.23751484"/><Animation name="grtswrd_prim_c1" speed="1.15"/>
  <Blend ExitTime="0.5" StartTime="0" Duration="0.1"/><Animation name="grtswrd_idle" flags="Loop"/>
  <Blend ExitTime="0.3" StartTime="0" Duration="0"/><Animation name=""/>
 </AnimLayer><ProcLayer/><ProcLayer>
  <Blend ExitTime="0.1"/><Procedural type="Homing"><ProceduralParams/></Procedural>
 </ProcLayer></Fragment>
</Attack_Primary></FragmentList></AnimDB>"#;
        let adb = AnimDatabase::parse(text).expect("adb");
        let entry = &adb.fragments[0];
        assert_eq!(entry.anim_layers.len(), 2);
        let main = entry.main_animation().expect("main");
        assert_eq!(main.name, "grtswrd_prim_c1", "not the clip of weight 0");
        let unweighted = Fragment {
            anim_layers: vec![entry.anim_layers[0].clone()],
            ..entry.clone()
        };
        assert_eq!(
            unweighted.main_animation().map(|clip| clip.name.as_str()),
            Some("rswrd_1h_lt_atk_01"),
            "only clips of weight 0: the first"
        );
        assert_eq!(
            (main.anim_start_s, main.speed, main.looped),
            (0.144_800_01, 1.15, false)
        );
        assert_eq!(main.end_s, Some(0.5));
        let idle = &entry.anim_layers[1][1];
        assert!(idle.looped && (idle.start_s - 0.5).abs() < 1e-9);
        assert_eq!(idle.end_s, Some(0.8), "the empty clip ends it");
        assert_eq!(entry.clips[0].layer, 1, "the second procedural layer");
    }

    #[test]
    fn the_options_are_the_entries_with_the_most_tags_in_file_order() {
        // `npc_risen_anims.adb`: two plain `r_Death` entries and tagged ones.
        let text = r#"<AnimDB><FragmentList><r_Death>
 <Fragment Tags="BlightFiend"><ProcLayer/></Fragment>
 <Fragment Tags=""><AnimLayer><Blend ExitTime="0"/><Animation name="first"/></AnimLayer></Fragment>
 <Fragment Tags=""><AnimLayer><Blend ExitTime="0"/><Animation name="second"/></AnimLayer></Fragment>
</r_Death></FragmentList></AnimDB>"#;
        let adb = AnimDatabase::parse(text).expect("adb");
        let names = |tags: &[&str]| -> Vec<String> {
            adb.options("r_Death", tags)
                .iter()
                .filter_map(|entry| Some(entry.main_animation()?.name.clone()))
                .collect()
        };
        assert_eq!(names(&["Base", "DT5"]), ["first", "second"]);
        assert_eq!(
            adb.options("r_Death", &["BlightFiend"]).len(),
            1,
            "only the tagged one: it has more tags"
        );
        assert_eq!(
            adb.best("r_Death", &["Base"])
                .and_then(Fragment::main_animation)
                .map(|clip| clip.name.as_str()),
            Some("first")
        );
    }

    #[test]
    fn the_best_entry_has_every_tag_and_no_fragment_tags() {
        let adb = AnimDatabase::parse(GREATSWORD_ADB).expect("adb");
        let tags = |wanted: &[&str]| {
            adb.best("Attack_Primary", wanted)
                .map(|fragment| fragment.tags.join("+"))
        };
        assert_eq!(
            tags(&["2H_Melee", "Greatsword"]).as_deref(),
            Some("2H_Melee+Greatsword"),
            "not the off-hand one, nor the upper-body one"
        );
        assert_eq!(
            tags(&["2H_Melee", "Greatsword", "Offhand"]).as_deref(),
            Some("2H_Melee+Greatsword+Offhand"),
            "the most tags"
        );
        assert_eq!(tags(&["1H_Melee", "Sword"]), None);
        let options = |wanted: &[&str]| adb.options("Attack_Primary", wanted).len();
        assert_eq!(options(&["2H_Melee", "Greatsword"]), 1);
        assert_eq!(options(&["1H_Melee", "Sword"]), 0);
    }

    #[test]
    fn an_action_list_names_each_action_fragment() {
        let text = r#"<ActionList><Actions>
  <Action Name="Attack_Primary_C1" FragmentName="Attack_Primary" Predictive="true"><ActivationCondition/></Action>
  <Action Name="Dodge" FragmentName="Dodge_Roll"/>
</Actions><Conditions><And Name="Attack_Heavy_NotHeld"/></Conditions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        assert_eq!(list.fragment("Attack_Primary_C1"), Some("Attack_Primary"));
        assert_eq!(list.fragment("Dodge"), Some("Dodge_Roll"));
        assert_eq!(list.fragment("Attack_Heavy_NotHeld"), None);
        assert_eq!(
            list.condition("Attack_Heavy_NotHeld"),
            Some(&Condition::All(Vec::new()))
        );
    }

    #[test]
    fn an_attack_moves_by_its_animation_with_the_row_of_its_markers() {
        // `playeractions_attack.actionlist`: the light attack's motion rows.
        let text = r#"<ActionList><Actions>
 <Action Name="Attack_Primary_C1" FragmentName="Attack_Primary"><Activities>
  <IfThenElse><ThenActivity><AnimationDrivenMotion><Default MotionMultiplier="3.0"/></AnimationDrivenMotion></ThenActivity></IfThenElse>
  <AnimationDrivenMotion>
   <Default MotionMultiplier="1.00" Decel="9,9,9" InheritVelocity="false"/>
   <Conditional Name="Attack_Translation_Sword" MotionMultiplier="1.5" Decel="1.0,2.0,1.0"/>
   <Conditional Name="Attack_Translation_Sword_FirstAttack" InheritVelocity="true" InheritVelocityMaxSpeed="6"/>
  </AnimationDrivenMotion>
 </Activities></Action>
 <Action Name="Dodge" FragmentName="Dodge_Roll"/>
</Actions><Conditions>
 <And Name="Attack_Translation_Sword"><IfMannequinMarker MarkerName="Attack_Translation"/><IfMannequinTag TagName="Sword"/></And>
 <And Name="Attack_Translation_Sword_FirstAttack"><IfMannequinMarker MarkerName="Attack_Translation_FirstAttack"/><IfMannequinTag TagName="Sword"/></And>
</Conditions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let motion = list
            .action("Attack_Primary_C1")
            .and_then(|action| action.driven_motion.clone())
            .expect("the top activity, not the branch's");
        assert_eq!(list.action("Dodge").expect("dodge").driven_motion, None);
        let row = |tags: &[&str], markers: &[&str]| {
            motion.row(|name| list.condition(name), tags, markers)
        };
        let sword = ["1H_Melee", "Sword"];
        assert_eq!(
            row(&sword, &[]),
            MotionRow {
                multiplier: Some(1.0),
                decel_mps2: Some([9.0, 9.0]),
                inherit_velocity: Some(false),
                inherit_max_mps: None,
            }
        );
        let translation = row(&sword, &["Attack_Translation"]);
        assert_eq!(translation.multiplier, Some(1.5));
        assert_eq!(translation.decel_mps2, Some([1.0, 2.0]));
        let first = row(&sword, &["Attack_Translation_FirstAttack"]);
        assert_eq!(first.inherit_velocity, Some(true));
        assert_eq!(first.inherit_max_mps, Some(6.0));
        assert_eq!(first.multiplier, Some(1.0), "left out: the default's");
        assert_eq!(row(&["Axe"], &["Attack_Translation"]), row(&sword, &[]));
    }

    #[test]
    fn a_condition_holds_for_the_base_state_of_its_tags() {
        // `playeractions_ability_blunderbuss.actionlist` and `..._greataxe.actionlist`.
        let text = r#"<ActionList><Actions/><Conditions>
  <And Name="Blunderbuss_Basic_Hipfire"><And>
   <Not><IfHasStatusEffect StatusEffect="Blunderbuss_Nine"/></Not>
   <Not><IfMannequinTag TagName="AimDownSights"/></Not>
  </And></And>
  <And Name="Blunderbuss_Unload_Hipfire"><And>
   <IfHasStatusEffect StatusEffect="Blunderbuss_Nine"/>
   <Not><IfMannequinTag TagName="AimDownSights"/></Not>
  </And></And>
  <Or Name="Is_Blunderbuss"><IfMannequinTag TagName="blunderbuss"/><True/></Or>
</Conditions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let holds = |name: &str, tags: &[&str]| list.condition(name).expect(name).holds(tags, &[]);
        let tags = ["2H_Ranged", "Blunderbuss"];
        assert!(holds("Blunderbuss_Basic_Hipfire", &tags));
        assert!(
            !holds("Blunderbuss_Unload_Hipfire", &tags),
            "no status effect"
        );
        assert!(
            !holds("Blunderbuss_Basic_Hipfire", &["AimDownSights"]),
            "a tag the character has"
        );
        assert!(holds("Is_Blunderbuss", &[]));
    }

    #[test]
    fn an_action_sets_a_tag_while_its_status_effect_is_on() {
        // `playeractions_ability_greatsword.actionlist`.
        let text = r#"<ActionList><Actions>
 <Action Name="Greatsword_OffenseStance">
  <ActivationCondition><And>
   <IfHasStatusEffect StatusEffect="Greatsword_Stance_Offense"/><IfMannequinTag TagName="Greatsword"/>
  </And></ActivationCondition>
  <Activities><SetMannequinTag ResetWhenReinstalled="false"><Default TagName="GenericTagA" OnEnter="Set" OnExit="Unset"/></SetMannequinTag></Activities>
 </Action>
</Actions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let action = list.action("Greatsword_OffenseStance").expect("action");
        assert_eq!(action.sets_tags, ["GenericTagA"]);
        assert_eq!(
            list.fragment("Greatsword_OffenseStance"),
            None,
            "no fragment"
        );
        let (effect, rest) = action.on_status_effect.as_ref().expect("on an effect");
        assert_eq!(effect, "Greatsword_Stance_Offense");
        assert!(
            rest.holds(&["2H_Melee", "GreatSword"], &[]),
            "the rest: the weapon's tag"
        );
        assert!(!rest.holds(&["1H_Melee"], &[]));
    }

    #[test]
    fn an_ability_action_names_its_ability_cost_cooldown_spells_and_effects() {
        // `playeractions_ability_lifemagic.actionlist`, Smite, trimmed.
        let text = r#"<ActionList><Actions>
 <Action Name="Ability_Life_Staff_Smite" FragmentName="Ability_Life_Staff_Smite">
  <ActivationCondition><And>
   <IfActiveAbilityMoveName AbilityName="Ability_Life_Staff_Smite"/>
   <IfManaCost ComparisonType="GreaterThanOrEqual" CostID="LifeStaff_Smite" SlotToCheckAlias="ACTIVE_MAIN_HAND_WEAPON"/>
   <Not><IfCooldownTimer AbilityID="Ability_Life_Staff_Smite" SlotToCheckAlias="ACTIVE_MAIN_HAND_WEAPON"/></Not>
   <Not><IfActiveAbilityMoveName AbilityName="Not_This"/></Not>
  </And></ActivationCondition>
  <Activities>
   <PerformActiveAbility><Default AbilityName="Ability_Life_Staff_Smite"/></PerformActiveAbility>
   <ModifyStatusEffects><Default><EffectsToApplyOnEnter><EffectId EffectId="Smite_Slow"/></EffectsToApplyOnEnter></Default></ModifyStatusEffects>
   <IfThenElse><IfCondition><IfExternalCondition ConditionName="NoSmiteUpgrade_RayCast"/></IfCondition>
    <ThenActivity><CastSpellRaycast SpellName="LifeStaff_Smite"/></ThenActivity>
    <ElseActivity><Group><SetCooldownTimer><Default AbilityID="Ability_Life_Staff_Smite"/></SetCooldownTimer></Group></ElseActivity>
   </IfThenElse>
   <CastSpell><Default SpellName="Always_Cast"/></CastSpell>
  </Activities>
 </Action>
</Actions>
<Conditions>
 <And Name="NoSmiteUpgrade_RayCast"><IfInput InputName="attack_shoot"/><IfMannequinTag TagName="Heal"/><IfMannequinMarker MarkerName="Magic_IsReadying_Smite"/></And>
</Conditions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let action = list.action("Ability_Life_Staff_Smite").expect("action");
        let ability = action.ability.as_ref().expect("an ability action");
        assert_eq!(
            ability.move_name.as_deref(),
            Some("Ability_Life_Staff_Smite")
        );
        assert_eq!(ability.mana_cost.as_deref(), Some("LifeStaff_Smite"));
        assert_eq!(ability.needs_ready, ["Ability_Life_Staff_Smite"]);
        assert_eq!(ability.performs, ["Ability_Life_Staff_Smite"]);
        assert_eq!(ability.starts_cooldowns, ["Ability_Life_Staff_Smite"]);
        assert_eq!(ability.applies_on_enter, ["Smite_Slow"]);
        let spells: Vec<(&str, CastKind)> = action
            .casts
            .iter()
            .map(|cast| (cast.spell.as_str(), cast.kind))
            .collect();
        assert_eq!(
            spells,
            [
                ("LifeStaff_Smite", CastKind::Raycast),
                ("Always_Cast", CastKind::Plain)
            ]
        );
        let smite = &action.casts[0].condition;
        let state = |inputs: &'static [&'static str]| CageState {
            tags: &["Heal"],
            markers: &["Magic_IsReadying_Smite"],
            inputs,
        };
        assert!(
            smite.holds_in(&state(&["attack_shoot"])),
            "the named condition holds"
        );
        assert!(!smite.holds_in(&state(&[])), "not without the key");
        assert!(
            action.casts[1].condition.holds_in(&state(&[])),
            "an unconditional cast"
        );
        assert!(
            list.action("Ability_Life_Staff_Smite")
                .is_some_and(|action| action.ability.is_some())
        );
    }

    /// A player whose Smite key is pressed, with mana, and the cooldowns `cooling`.
    struct Pressed {
        cooling: &'static [&'static str],
    }

    impl CageWorld for Pressed {
        fn tag(&self, _: &str) -> bool {
            false
        }

        fn marker(&self, _: &str) -> bool {
            false
        }

        fn input(&self, _: &str) -> bool {
            false
        }

        fn ability_move(&self, ability: &str) -> bool {
            ability == "Ability_Life_Staff_Smite"
        }

        fn cooldown(&self, ability: &str) -> bool {
            self.cooling.contains(&ability)
        }

        fn mana_cost(&self, _: &str, compare: ManaCompare) -> bool {
            compare == ManaCompare::AtLeast
        }
    }

    #[test]
    fn an_ability_action_starts_on_its_key_when_its_cooldown_is_over() {
        let text = r#"<ActionList><Actions>
 <Action Name="Smite" FragmentName="Smite">
  <ActivationCondition><And>
   <IfActiveAbilityMoveName AbilityName="Ability_Life_Staff_Smite"/>
   <IfManaCost ComparisonType="GreaterThanOrEqual" CostID="LifeStaff_Smite" SlotToCheckAlias="ACTIVE_MAIN_HAND_WEAPON"/>
   <Not><IfCooldownTimer AbilityID="Ability_Life_Staff_Smite" SlotToCheckAlias="ACTIVE_MAIN_HAND_WEAPON"/></Not>
  </And></ActivationCondition>
  <DeactivationCondition><IfFragmentDone UseHostActionFragment="true" FragmentName="" ExitOffsetSeconds="0.25"/></DeactivationCondition>
 </Action>
</Actions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let action = list.action("Smite").expect("action");
        let activation = action.activation.as_ref().expect("an activation");
        assert!(activation.holds_in(&Pressed { cooling: &[] }));
        assert!(!activation.holds_in(&Pressed {
            cooling: &["Ability_Life_Staff_Smite"]
        }));
        assert_eq!(
            action.deactivation,
            Some(Condition::All(vec![Condition::FragmentDone {
                fragment: None,
                exit_offset_ms: 250
            }]))
        );
    }

    /// `playeractions_ability_lifemagic.actionlist`: Beacon needs more mana than its cost
    /// (`Beacon_Mana_Check` is `LessThanOrEqual`), a flail out (`IfMannequinTagInItemSlot`) and
    /// a client setting (`IfStackConfigVar`).
    #[test]
    fn mana_compares_slot_tags_and_settings_read_as_the_list_says() {
        let text = r#"<ActionList><Conditions>
 <And Name="Beacon_Mana_Check"><IfManaCost ComparisonType="LessThanOrEqual" CostID="LifeStaff_Beacon"/></And>
</Conditions><Actions>
 <Action Name="Beacon" FragmentName="Beacon">
  <ActivationCondition><And>
   <Not><IfExternalCondition ConditionName="Beacon_Mana_Check"/></Not>
   <IfMannequinTagInItemSlot TagName="Flail" SlotToCheckAlias="ACTIVE_MAIN_HAND_WEAPON"/>
   <IfStackConfigVar StackVar="javelin.enable-ammoless-weapons"/>
  </And></ActivationCondition>
 </Action>
</Actions></ActionList>"#;
        struct Flail {
            mana: f32,
        }
        impl CageWorld for Flail {
            fn tag(&self, _: &str) -> bool {
                false
            }
            fn marker(&self, _: &str) -> bool {
                false
            }
            fn input(&self, _: &str) -> bool {
                false
            }
            fn mana_cost(&self, _: &str, compare: ManaCompare) -> bool {
                compare.holds(self.mana, 30.0)
            }
            fn tag_in_slot(&self, tag: &str, slot: &str) -> bool {
                tag == "Flail" && slot == "ACTIVE_MAIN_HAND_WEAPON"
            }
            fn config_var(&self, variable: &str) -> bool {
                variable == "javelin.enable-ammoless-weapons"
            }
        }
        let list = ActionList::parse(text).expect("list");
        let activation = list
            .action("Beacon")
            .and_then(|action| action.activation.as_ref())
            .expect("an activation");
        assert!(
            activation.holds_in(&Flail { mana: 100.0 }),
            "more mana than the cost"
        );
        assert!(
            !activation.holds_in(&Flail { mana: 30.0 }),
            "mana at the cost"
        );
        assert_eq!(
            list.resolved_condition("Beacon_Mana_Check"),
            Some(Condition::All(vec![Condition::ManaCost {
                cost: "LifeStaff_Beacon".to_owned(),
                compare: ManaCompare::AtMost,
            }]))
        );
        assert!(ManaCompare::Below.holds(29.0, 30.0) && !ManaCompare::Below.holds(30.0, 30.0));
    }

    #[test]
    fn an_action_without_ability_parts_has_none() {
        let text = r#"<ActionList><Actions><Action Name="Idle" FragmentName="Idle"><Activities/></Action></Actions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        assert_eq!(
            list.action("Idle")
                .and_then(|action| action.ability.as_ref()),
            None
        );
    }

    #[test]
    fn an_action_casts_a_spell_while_its_marker_is_on() {
        // `playeractions_ability_firemagic.actionlist`: the fire staff's light attack.
        let text = r#"<ActionList><Actions>
 <Action Name="Fire_Staff_Cast_Primary" FragmentName="Magic_Staff_Cast_Primary_C1"><Activities>
  <IfThenElse Comment="x"><IfCondition><And><IfExternalCondition ConditionName="IsMelee"/></And></IfCondition>
   <ThenActivity><CastSpell><Default SpellName="Not_This"/></CastSpell></ThenActivity></IfThenElse>
  <IfThenElse Comment="Cast light attack Projectile"><IfCondition><And><IfMannequinMarker MarkerName="CastFireProjectile"/></And></IfCondition>
   <ThenActivity><CastSpell><Default SpellName="Elemental_Fire_Projectile"/></CastSpell></ThenActivity></IfThenElse>
 </Activities></Action>
</Actions></ActionList>"#;
        let list = ActionList::parse(text).expect("list");
        let action = list.action("Fire_Staff_Cast_Primary").expect("action");
        let cast = |marker: &str| {
            action
                .casts
                .iter()
                .find(|cast| cast.condition.holds(&["Fire"], &[marker]))
                .map(|cast| cast.spell.as_str())
        };
        assert_eq!(
            cast("CastFireProjectile"),
            Some("Elemental_Fire_Projectile")
        );
        assert_eq!(cast("Other"), None, "the melee check never holds");
        assert_eq!(action.casts[1].condition.markers(), ["CastFireProjectile"]);
    }
}
