// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's input actions (`libs/config/defaultprofile.xml` and the files it includes): the
//! numbers of the action bits of the client's input records.
//!
//! **Observed** (2026-10-31, abilities design note): the client gives each action name the
//! place of its name in the list of every action name of the profile, sorted by the CRC-32 of the
//! lowercase name (`FUN_14280a040` asks an object for the number by name and splits it into a
//! 32-bit word and a bit). The rule gives the numbers of the ability keys (`ability1` 402,
//! `ability2` 115, `ability3` 139) and of every other input number known from retail records:
//! `dodge_depressed` 67, `autorun` 88, `aim_draw` 126 (block), `attack_primary_hold` 191,
//! `toggleFishingBaitWindow` 265, `fishing_activate` 305, `walk` 313, `prone_toggle` 331.

use std::collections::BTreeSet;

use crate::mannequin::{MannequinError, Node, parse_tree};
use crate::objectstream::lowercase_crc32;

/// The element of an action.
const ACTION_ELEMENT: &str = "action";
/// The element that includes another profile file.
const INCLUDE_ELEMENT: &str = "include";
const NAME_ATTRIBUTE: &str = "name";
const FILENAME_ATTRIBUTE: &str = "filename";
/// The suffix an include names its file with, and the suffix of the file in the paks.
const INCLUDE_SUFFIX: &str = ".inputprofilexml";
const FILE_SUFFIX: &str = ".xml";
/// Includes of includes, at most (the retail profile has one level).
const MAX_INCLUDE_DEPTH: usize = 4;

/// The pak path of the file an include names (`libs/config/includes/debug.inputprofilexml` to
/// `libs/config/includes/debug.xml`, lowercase).
pub fn include_path(filename: &str) -> String {
    let path = filename.to_lowercase();
    match path.strip_suffix(INCLUDE_SUFFIX) {
        Some(stem) => format!("{stem}{FILE_SUFFIX}"),
        None => path,
    }
}

/// The action names of the profile `text`, in input-number order (module notes); `file` gives
/// the text of an included file by its pak path ([`include_path`]), `None` when it is missing.
///
/// # Errors
///
/// Returns [`MannequinError`] when the profile or an included file is not well-formed XML.
pub fn input_actions(
    text: &str,
    file: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, MannequinError> {
    let mut names = BTreeSet::new();
    collect(&parse_tree(text)?, file, &mut names, 0)?;
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort_by_key(|name| lowercase_crc32(name));
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Ok(names)
}

fn collect(
    node: &Node,
    file: &dyn Fn(&str) -> Option<String>,
    names: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), MannequinError> {
    match node.name.as_str() {
        ACTION_ELEMENT => {
            if let Some(name) = node.attribute(NAME_ATTRIBUTE) {
                names.insert(name.to_owned());
            }
        }
        INCLUDE_ELEMENT if depth < MAX_INCLUDE_DEPTH => {
            if let Some(text) = node
                .attribute(FILENAME_ATTRIBUTE)
                .and_then(|filename| file(&include_path(filename)))
            {
                collect(&parse_tree(&text)?, file, names, depth + 1)?;
            }
        }
        _ => {}
    }
    for child in &node.children {
        collect(child, file, names, depth)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"<profile version="74">
        <include filename="libs/config/includes/virtual.inputprofilexml"/>
        <actionmap name="player">
            <action name="ability1" onPress="1" keyboard="q"/>
            <action name="ability2" onPress="1" keyboard="r"/>
            <action name="ability1" onPress="1" keyboard="1"/>
        </actionmap>
    </profile>"#;
    const VIRTUAL: &str = r#"<profile><actionmap name="virtual">
        <action name="ability3"/>
    </actionmap></profile>"#;

    #[test]
    fn the_actions_of_a_profile_and_its_includes_are_numbered_by_their_name_crc() {
        let file =
            |path: &str| (path == "libs/config/includes/virtual.xml").then(|| VIRTUAL.to_owned());
        let actions = input_actions(PROFILE, &file).expect("profile");
        let mut by_crc = vec!["ability1", "ability2", "ability3"];
        by_crc.sort_by_key(|name| lowercase_crc32(name));
        assert_eq!(actions, by_crc, "each name once, the include in");
        let missing = input_actions(PROFILE, &|_| None).expect("profile");
        assert_eq!(missing.len(), 2, "a missing include adds nothing");
    }
}
