// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trees that neither Server.pak nor the client holds, built from held trees.
//!
//! `damned_firstmate_2h_anchor_basebehavior` (First Mate Janszoon,
//! `Corrupted_Sailor_StarterBeach_Named_00`) is missing. In the retail new-character run he
//! attacks with `Attack_Far_2H_Axe_01`, `Attack_Close_2H_Axe_04` and once with
//! `Officer_AnchorToss` (capture 1246273 ms, the player 8.7 m away, right after
//! `Attack_Close_2H_Axe_04`). No held tree holds all three. His tree is built from two (Inferred,
//! the base chosen by the user 2026-09-26, the toss added on the user's request 2026-10-30):
//!
//! - the damned great axeman's `damned_2h_melee_basebehavior`, which holds the two axe attacks;
//! - with, first in its combat selector, the toss of the undead officer's `FarRange` branch
//!   (target at 6 to 10 m and visible: lock the target and throw the anchor, a 6 to 9 s
//!   cooldown), the only toss of the held trees whose range holds the retail 8.7 m. The branch's
//!   other choices (the officer's ocean shield, anchor dash and run attack) are left out: the
//!   retail Janszoon showed none of them. The officer throws only at difficulty tier 2 or more;
//!   Janszoon's slice sets tier 1 and he threw in retail, so that one condition is left out too;
//! - with, first in the close range selector, the officer's close `SpellCDCheck` branch: the
//!   anchor slam behind its `SpellCD` cooldown (10 to 14 s at his tier). A second retail fight
//!   (the Linux main quest playthrough, part 1, 819480 ms) shows `Officer_AnchorSlam` right after
//!   `Attack_Combo_GreatAxeman_3`. The branch's ocean shield is left out (no retail fight shows
//!   it). Its place in the selector is Inferred.

use nw_ai::classes::{Class, class_of};
use nw_formats::objectstream::{Element, lowercase_crc32, parse_object_stream};

/// Janszoon's missing tree.
pub const JANSZOON_TREE: &str =
    "sharedassets/ai/behaviortrees/damned/damned_firstmate_2h_anchor_basebehavior.behaviortree";
/// The damned great axeman's base behavior.
const GREAT_AXEMAN_TREE: &str =
    "sharedassets/ai/behaviortrees/damned/damned_2h_melee_basebehavior.behaviortree";
/// The undead officer's combat behavior, which holds the anchor toss.
const OFFICER_COMBAT_TREE: &str =
    "sharedassets/ai/behaviortrees/undead/undead_officer_combatbehavior.behaviortree";
/// The great axeman's combat sub-tree (its `Tree` name, lower case).
const GREAT_AXEMAN_COMBAT: &str = "damnedgreataxeman_combatbehavior";
/// The selector of the combat sub-tree that picks the range branch.
const COMBAT_SELECTOR: &str = "Combat";
/// The officer's branch for a target at 6 to 10 m.
const TOSS_BRANCH: &str = "FarRange";
/// The selector of [`TOSS_BRANCH`].
const TOSS_SELECTOR: &str = "FarSelector";
/// The officer's tier check inside [`TOSS_SELECTOR`], above the toss.
const TIER_CHECK: &str = "MediumDifficultyCheck";
/// The key of the tier condition.
const TIER_KEY: &str = "Difficulty_Tier";
/// The officer's close range branch that slams behind `SpellCD`.
const SLAM_BRANCH: &str = "SpellCDCheck";
/// The selector of [`SLAM_BRANCH`] that picks the shield or the slam.
const SLAM_SELECTOR: &str = "SpellSelector";
/// The slam's CAGE task.
const SLAM_TASK: &str = "Officer_AnchorSlam_CAGE_Task";
/// The great axeman's close range branch of its combat selector.
const CLOSE_RANGE: &str = "CloseMeleeRange";
/// The selector of [`CLOSE_RANGE`].
const CLOSE_SELECTOR: &str = "CloseAttackSel";

const NAME: &str = "Name";
const ROOT: &str = "Root";
const CHILDREN: &str = "Children";
const CHILD: &str = "Child";
const ENTRY_CONDITIONS: &str = "EntryConditions";
const CONDITIONS: &str = "Conditions";
const BASE_CLASS: &str = "BaseClass1";

/// Why a stand-in tree cannot be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StandInError {
    /// A tree it is built from is not in Server.pak.
    #[error("stand-in tree {tree}: {from} is not in Server.pak")]
    MissingSource {
        /// The tree to build.
        tree: &'static str,
        /// The missing source tree.
        from: &'static str,
    },
    /// A source tree does not parse.
    #[error("stand-in tree {tree}: {from} does not parse: {reason}")]
    Parse {
        /// The tree to build.
        tree: &'static str,
        /// The source tree.
        from: &'static str,
        /// Why.
        reason: String,
    },
    /// A node it edits is not where the source trees had it.
    #[error("stand-in tree {tree}: {what} not found")]
    Shape {
        /// The tree to build.
        tree: &'static str,
        /// The node or field.
        what: &'static str,
    },
}

/// The stand-in trees (path and `Tree` root) built from the Server.pak tree `entries` (path
/// and ObjectStream bytes).
///
/// # Errors
///
/// Returns [`StandInError`] when a source tree is missing or not in its known shape.
pub fn stand_in_trees(
    entries: &[(String, Vec<u8>)],
) -> Result<Vec<(&'static str, Element)>, StandInError> {
    Ok(vec![(JANSZOON_TREE, janszoon_tree(entries)?)])
}

fn janszoon_tree(entries: &[(String, Vec<u8>)]) -> Result<Element, StandInError> {
    let shape = |what| StandInError::Shape {
        tree: JANSZOON_TREE,
        what,
    };
    let mut root = source(entries, GREAT_AXEMAN_TREE)?;
    let officer = source(entries, OFFICER_COMBAT_TREE)?;
    let mut toss = branch(&officer, TOSS_BRANCH, None).ok_or_else(|| shape(TOSS_BRANCH))?;
    if keep_one_choice(&mut toss, TOSS_SELECTOR, &|node| {
        node_name(node) == Some(TIER_CHECK)
    }) != 1
    {
        return Err(shape(TOSS_SELECTOR));
    }
    let mut slam =
        branch(&officer, SLAM_BRANCH, Some(SLAM_TASK)).ok_or_else(|| shape(SLAM_BRANCH))?;
    if keep_one_choice(&mut slam, SLAM_SELECTOR, &|node| {
        holds_text(node, SLAM_TASK)
    }) != 1
    {
        return Err(shape(SLAM_SELECTOR));
    }
    let mut tier_conditions = 0;
    for_each_mut(&mut toss, &mut |element| {
        if node_name(element) != Some(TIER_CHECK) {
            return;
        }
        let Some(list) =
            field_mut(element, ENTRY_CONDITIONS).and_then(|group| field_mut(group, CONDITIONS))
        else {
            return;
        };
        let before = list.children.len();
        list.children
            .retain(|condition| !holds_text(condition, TIER_KEY));
        tier_conditions += before - list.children.len();
    });
    if tier_conditions != 1 {
        return Err(shape(TIER_KEY));
    }
    let mut grafts = 0;
    let mut slams = 0;
    for_each_mut(&mut root, &mut |element| {
        let is_combat_tree = class_of(&element.type_id) == Some(Class::Tree)
            && field(element, NAME)
                .and_then(text)
                .is_some_and(|name| name.to_lowercase().contains(GREAT_AXEMAN_COMBAT));
        if !is_combat_tree {
            return;
        }
        let Some(list) = field_mut(element, ROOT)
            .and_then(|pointer| pointer.children.first_mut())
            .filter(|node| node_name(node) == Some(COMBAT_SELECTOR))
            .and_then(|selector| field_mut(selector, CHILDREN))
        else {
            return;
        };
        list.children.insert(0, toss.clone());
        grafts += 1;
        let close_choices = list
            .children
            .iter_mut()
            .filter_map(|pointer| pointer.children.first_mut())
            .filter(|node| node_name(node) == Some(CLOSE_RANGE))
            .filter_map(|node| field_mut(node, CHILD))
            .filter_map(|pointer| pointer.children.first_mut())
            .filter(|node| node_name(node) == Some(CLOSE_SELECTOR))
            .filter_map(|selector| field_mut(selector, CHILDREN));
        for choices in close_choices {
            choices.children.insert(0, slam.clone());
            slams += 1;
        }
    });
    if grafts == 0 {
        return Err(shape(GREAT_AXEMAN_COMBAT));
    }
    if slams != grafts {
        return Err(shape(CLOSE_SELECTOR));
    }
    Ok(root)
}

/// A copy of the first node pointer under `tree` whose node is named `name` and, with `task`,
/// holds that CAGE task.
fn branch(tree: &Element, name: &str, task: Option<&str>) -> Option<Element> {
    find(tree, &|element| {
        class_of(&element.type_id) == Some(Class::NodePtr)
            && element.children.first().is_some_and(|node| {
                node_name(node) == Some(name) && task.is_none_or(|task| holds_text(node, task))
            })
    })
    .cloned()
}

/// Keeps, in every selector named `selector` under `branch`, only the choices whose node `keep`
/// accepts; the number of choices kept.
fn keep_one_choice(branch: &mut Element, selector: &str, keep: &dyn Fn(&Element) -> bool) -> usize {
    let mut kept = 0;
    for_each_mut(branch, &mut |element| {
        if node_name(element) != Some(selector) {
            return;
        }
        if let Some(list) = field_mut(element, CHILDREN) {
            list.children
                .retain(|pointer| pointer.children.first().is_some_and(keep));
            kept += list.children.len();
        }
    });
    kept
}

/// The parsed `Tree` root of the Server.pak tree `path`.
fn source(entries: &[(String, Vec<u8>)], path: &'static str) -> Result<Element, StandInError> {
    let (_, bytes) =
        entries
            .iter()
            .find(|(known, _)| known == path)
            .ok_or(StandInError::MissingSource {
                tree: JANSZOON_TREE,
                from: path,
            })?;
    parse_object_stream(bytes).map_err(|error| StandInError::Parse {
        tree: JANSZOON_TREE,
        from: path,
        reason: error.to_string(),
    })
}

/// The field `name` of `element`, its base classes included.
fn field<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    let crc = lowercase_crc32(name);
    let base = lowercase_crc32(BASE_CLASS);
    element
        .children
        .iter()
        .find_map(|child| match child.name_crc {
            Some(known) if known == crc => Some(child),
            Some(known) if known == base => field(child, name),
            _ => None,
        })
}

/// The field `name` of `element`, its base classes included, to change.
fn field_mut<'a>(element: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    let crc = lowercase_crc32(name);
    let base = lowercase_crc32(BASE_CLASS);
    let index = element.children.iter().position(|child| {
        child.name_crc == Some(crc)
            || (child.name_crc == Some(base) && field(child, name).is_some())
    })?;
    let child = &mut element.children[index];
    if child.name_crc == Some(crc) {
        Some(child)
    } else {
        field_mut(child, name)
    }
}

/// The text of a string element.
fn text(element: &Element) -> Option<&str> {
    (class_of(&element.type_id) == Some(Class::String))
        .then(|| std::str::from_utf8(&element.value).ok())
        .flatten()
}

/// The `Name` of a node class element; `None` for the base class parts inside one.
fn node_name(node: &Element) -> Option<&str> {
    if node.name_crc == Some(lowercase_crc32(BASE_CLASS)) {
        return None;
    }
    field(node, NAME).and_then(text)
}

/// Whether a string element under `element` holds `wanted`.
fn holds_text(element: &Element, wanted: &str) -> bool {
    text(element) == Some(wanted)
        || element
            .children
            .iter()
            .any(|child| holds_text(child, wanted))
}

/// The first element under `element` (itself included, depth first) that `test` accepts.
fn find<'a>(element: &'a Element, test: &dyn Fn(&Element) -> bool) -> Option<&'a Element> {
    if test(element) {
        return Some(element);
    }
    element.children.iter().find_map(|child| find(child, test))
}

/// Calls `visit` on `element` and every element under it, parents first.
fn for_each_mut(element: &mut Element, visit: &mut dyn FnMut(&mut Element)) {
    visit(element);
    for child in &mut element.children {
        for_each_mut(child, visit);
    }
}

#[cfg(all(test, feature = "local-data"))]
mod tests {
    use std::path::Path;

    use nw_ai::decode::server_pak_entries;
    use nw_oodle::KrakenDecompressor;

    use super::*;
    use crate::ai_data::DEFAULT_SERVER_PAK;

    fn entries() -> Vec<(String, Vec<u8>)> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        server_pak_entries(&repo.join(DEFAULT_SERVER_PAK), &KrakenDecompressor).expect("Server.pak")
    }

    #[test]
    fn janszoon_s_tree_is_the_great_axeman_s_with_the_officer_s_toss_and_slam() {
        const TOSS_TASK: &str = "Officer_AnchorToss_CAGE_Task";
        let entries = entries();
        let trees = stand_in_trees(&entries).expect("stand-in trees");
        let [(path, root)] = trees.as_slice() else {
            panic!("one stand-in tree");
        };
        assert_eq!(*path, JANSZOON_TREE);
        let combat = find(root, &|element| {
            node_name(element) == Some(COMBAT_SELECTOR)
                && field(element, CHILDREN).is_some_and(|list| {
                    list.children
                        .first()
                        .and_then(|pointer| pointer.children.first())
                        .and_then(node_name)
                        == Some(TOSS_BRANCH)
                })
        })
        .expect("the toss first in the combat selector");
        let branch = &field(combat, CHILDREN).expect("children").children[0];
        assert!(holds_text(branch, TOSS_TASK));
        assert!(!holds_text(branch, TIER_KEY));
        let selector = find(branch, &|element| node_name(element) == Some(TOSS_SELECTOR))
            .expect("toss selector");
        assert_eq!(
            field(selector, CHILDREN).expect("children").children.len(),
            1
        );
        let close = find(root, &|element| {
            node_name(element) == Some(CLOSE_SELECTOR)
                && field(element, CHILDREN).is_some_and(|list| {
                    list.children
                        .first()
                        .and_then(|pointer| pointer.children.first())
                        .and_then(node_name)
                        == Some(SLAM_BRANCH)
                })
        })
        .expect("the slam first in the close range selector");
        let slam = &field(close, CHILDREN).expect("children").children[0];
        assert!(holds_text(slam, SLAM_TASK));
        assert!(!holds_text(slam, "Officer_OceanShield_CAGE_Task"));
        assert!(!holds_text(slam, "Officer_OceanShield_Easy_CAGE_Task"));
        let mut decoder = nw_ai::decode::TreeDecoder::default();
        decoder
            .add_tree(path, root)
            .expect("the built tree decodes");
    }
}
