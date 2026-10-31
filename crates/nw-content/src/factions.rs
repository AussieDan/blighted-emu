// SPDX-License-Identifier: AGPL-3.0-or-later
//! The factions (`FactionData`): `None` (0), `Faction1` the Syndicate (1), `Faction2` the
//! Marauders (2), `Faction3` the Covenant (3); each faction row's `RequiredAchievementID` names
//! its recruitment quest (`FactionIntro_Syndicate_Recruitment`).
//!
//! **Observed** (retail `Hive_join_yellow_faction_quest`, `Marauders_Advancement` and
//! `Change-Faction`): the client's `RequestSetFaction` (3161) carries the faction byte 3 for the
//! Covenant, 2 for the Marauders and 1 for the Syndicate.
//!
//! A `RequiredFaction` cell of an objective or a mission names a faction (`Covenant`, `Marauder`,
//! `Syndicate`) or, after `!`, a faction the player must not be of. **Inferred**: `None` (105
//! objectives, among them quests every new character takes) asks for nothing.

/// No faction.
pub const NO_FACTION: u8 = 0;
/// The Syndicate (`Faction1`).
pub const SYNDICATE: u8 = 1;
/// The Marauders (`Faction2`).
pub const MARAUDERS: u8 = 2;
/// The Covenant (`Faction3`).
pub const COVENANT: u8 = 3;
/// The mark of a `RequiredFaction` the player must not be of.
const NOT_MARK: char = '!';
/// The `RequiredFaction` names of the factions.
const NAMES: [(&str, u8); 3] = [
    ("Syndicate", SYNDICATE),
    ("Marauder", MARAUDERS),
    ("Covenant", COVENANT),
];

/// The faction a `RequiredFaction` name (any case) names.
pub fn faction_named(name: &str) -> Option<u8> {
    NAMES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name.trim()))
        .map(|(_, faction)| *faction)
}

/// Whether a player of faction `faction` may take content whose `RequiredFaction` is `cell`:
/// an empty cell, `None` or a name the data does not have asks for nothing.
pub fn faction_requirement_met(cell: Option<&str>, faction: u8) -> bool {
    let Some(cell) = cell.map(str::trim).filter(|cell| !cell.is_empty()) else {
        return true;
    };
    match cell.strip_prefix(NOT_MARK) {
        Some(name) => faction_named(name).is_none_or(|named| named != faction),
        None => faction_named(cell).is_none_or(|named| named == faction),
    }
}

/// Whether `faction` is one a player may join.
pub const fn joinable(faction: u8) -> bool {
    matches!(faction, SYNDICATE | MARAUDERS | COVENANT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_required_faction_names_one_or_excludes_one() {
        assert!(faction_requirement_met(Some("Covenant"), COVENANT));
        assert!(!faction_requirement_met(Some("Covenant"), MARAUDERS));
        assert!(!faction_requirement_met(Some("Marauder"), NO_FACTION));
        // The recruitment quests: `!Covenant` for anyone not of the Covenant.
        assert!(faction_requirement_met(Some("!Covenant"), NO_FACTION));
        assert!(faction_requirement_met(Some("!Covenant"), SYNDICATE));
        assert!(!faction_requirement_met(Some("!covenant"), COVENANT));
        assert!(faction_requirement_met(Some("None"), COVENANT));
        assert!(faction_requirement_met(None, NO_FACTION));
        assert!(faction_requirement_met(Some(""), SYNDICATE));
    }

    #[test]
    fn only_the_three_factions_are_joinable() {
        assert!(joinable(SYNDICATE) && joinable(MARAUDERS) && joinable(COVENANT));
        assert!(!joinable(NO_FACTION));
        assert!(!joinable(4));
        assert_eq!(faction_named("marauder"), Some(MARAUDERS));
    }
}
