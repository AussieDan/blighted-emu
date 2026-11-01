// SPDX-License-Identifier: AGPL-3.0-or-later
//! The look a character was created with (player-appearance note, section "Saved look").
//!
//! `CreateCharacter` stores the client's `CharacterCreationParams` text: base64 of zlib data. The
//! data is JSON written byte by byte (bytes above `0x7f` are not UTF-8), and its `visualsBlob`
//! string holds a binary ObjectStream. The root of that stream has one string child per look
//! choice, keyed by the lowercase CRC-32 of a short name (`gender`, `race`, `skintone`, ...), with
//! values such as `Male`, `face05` or `hairstyle65m`. The wire look ([`PlayerLook`]) takes the
//! 0-based position of each choice in its list of the client's character-creation database
//! (`sharedassets/genericassets/character/charactercreationdatabase/` in
//! `SharedDataStrm-part7.pak`). The lists follow the name numbers, with a `none` row first in
//! some lists and two extra rows (`Chroma`, `ChromaCutout`) in the male race list.

use std::io::{Read, Write};

use data_encoding::BASE64;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use nw_formats::objectstream::{
    Element, lowercase_crc32, parse_object_stream, write_object_stream,
};
use nw_messages::{LOOK_CHOICES, PlayerLook};

/// The key of the creation settings in a stored character's appearance.
const CREATION_PARAMS_KEY: &str = "CharacterCreationParams";
/// The key of the ObjectStream text inside the creation settings.
const VISUALS_BLOB_KEY: &str = "visualsBlob";
/// Most bytes of inflated creation settings: a real one is about 3.4 KB.
const MAX_PARAMS_BYTES: u64 = 1 << 16;
/// The saved gender names, at their wire value.
const GENDERS: [&str; 2] = ["Male", "Female"];
/// The name of the empty first row of a list that has one.
const NONE_ROW: &str = "none";
/// Rows of the male race list: 29 faces and the two Chroma rows.
const MALE_RACES: u8 = 31;
/// Rows of the female race list: 30 faces.
const FEMALE_RACES: u8 = 30;
/// List positions of the male `Chroma` and `ChromaCutout` rows, which come between faces.
const MALE_CHROMA_ROWS: [u8; 2] = [3, 11];

/// How the names of one list map to positions.
#[derive(Debug, Clone, Copy)]
enum Numbering {
    /// `<prefix>N` is row `N - 1`.
    FromOne,
    /// Row 0 is `none`; `<prefix>N` is row `N`.
    NoneFirst,
}

/// One list of look choices.
#[derive(Debug, Clone, Copy)]
struct ChoiceList {
    /// The ObjectStream child name of the choice.
    name: &'static str,
    /// The text before the number in a row name.
    prefix: &'static str,
    numbering: Numbering,
    /// Rows in the list.
    rows: u8,
}

impl ChoiceList {
    const fn new(name: &'static str, prefix: &'static str, numbering: Numbering, rows: u8) -> Self {
        Self {
            name,
            prefix,
            numbering,
            rows,
        }
    }
}

/// The lists of the look choices after gender and race, in [`PlayerLook`] child order.
const CHOICE_LISTS: [ChoiceList; LOOK_CHOICES - 2] = [
    ChoiceList::new("skintone", "skin", Numbering::FromOne, 10),
    ChoiceList::new("hairstyle", "hairstyle", Numbering::NoneFirst, 72),
    ChoiceList::new("facialhair", "facialhair", Numbering::NoneFirst, 69),
    ChoiceList::new("haircolor", "haircolor", Numbering::FromOne, 30),
    ChoiceList::new("facialhaircolor", "haircolor", Numbering::FromOne, 30),
    ChoiceList::new("eyecolor", "eyecolor", Numbering::FromOne, 20),
    ChoiceList::new("facemark", "facemark", Numbering::NoneFirst, 7),
    ChoiceList::new("scar", "scar", Numbering::NoneFirst, 11),
    ChoiceList::new("tattoo", "tattoo", Numbering::NoneFirst, 62),
    ChoiceList::new("tattoocolor", "tattoocolor", Numbering::FromOne, 15),
];
/// The ObjectStream child names of gender and race.
const GENDER_NAME: &str = "gender";
const RACE_NAME: &str = "race";
/// The ObjectStream child name of the archetype pick: a backstory id.
const BACKSTORY_NAME: &str = "backstory";
/// The race row name prefix.
const RACE_PREFIX: &str = "face";
/// The head and facial hair names end with this letter in both genders' lists.
const HAIR_SUFFIX: char = 'm';

/// The look saved with a character, from its stored appearance JSON. `None` when the settings are
/// missing (bots and tests store `{}`) or a choice is not in its list.
pub fn saved_look(appearance: &serde_json::Value) -> Option<PlayerLook> {
    let root = saved_visuals(appearance)?;
    let text = |name: &str| root.child(lowercase_crc32(name))?.as_string().ok();
    let gender_name = text(GENDER_NAME)?;
    let gender = GENDERS.iter().position(|known| *known == gender_name)?;
    let mut choices = [0; LOOK_CHOICES];
    choices[0] = u8::try_from(gender).ok()?;
    choices[1] = race_position(gender == 0, text(RACE_NAME)?)?;
    for (choice, list) in choices[2..].iter_mut().zip(CHOICE_LISTS) {
        *choice = list_position(list, text(list.name)?)?;
    }
    Some(PlayerLook::of_choices(choices))
}

/// The backstory id that the character's archetype pick saved (for example `Archetype_Mystic`),
/// from its stored appearance JSON (starter-kit note, section 6). `None` when the settings are
/// missing or have no backstory.
pub fn saved_backstory(appearance: &serde_json::Value) -> Option<String> {
    let root = saved_visuals(appearance)?;
    let backstory = root
        .child(lowercase_crc32(BACKSTORY_NAME))?
        .as_string()
        .ok()?;
    (!backstory.is_empty()).then(|| backstory.to_owned())
}

/// The JSON key of the character level in the creation settings.
const CURRENT_LEVEL_KEY: &[u8] = b"\"currentLevel\":";

/// The character's `PublishedData` for the character select screen (character-select note): the
/// stored creation settings with `currentLevel` set to `level`, in the same form (base64 of zlib
/// data of the JSON). The screen draws the character from its `visualsBlob`; without it the
/// character is a blank body. `None` when the settings are missing or have no level.
pub fn published_data(appearance: &serde_json::Value, level: u32) -> Option<String> {
    let mut text = inflated(appearance.get(CREATION_PARAMS_KEY)?.as_str()?)?;
    let start = text
        .windows(CURRENT_LEVEL_KEY.len())
        .position(|window| window == CURRENT_LEVEL_KEY)?
        + CURRENT_LEVEL_KEY.len();
    let digits = text[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || **byte == b'-')
        .count();
    if digits == 0 {
        return None;
    }
    text.splice(start..start + digits, level.to_string().into_bytes());
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&text).ok()?;
    Some(BASE64.encode(&encoder.finish().ok()?))
}

/// The JSON bytes of creation settings `params` (base64 of zlib data); its text is not UTF-8.
fn inflated(params: &str) -> Option<Vec<u8>> {
    let compressed = BASE64.decode(params.trim().as_bytes()).ok()?;
    let mut inflated = Vec::new();
    ZlibDecoder::new(compressed.as_slice())
        .take(MAX_PARAMS_BYTES)
        .read_to_end(&mut inflated)
        .ok()?;
    Some(inflated)
}

/// The root element of the `visualsBlob` in a stored appearance.
fn saved_visuals(appearance: &serde_json::Value) -> Option<Element> {
    visuals(appearance.get(CREATION_PARAMS_KEY)?.as_str()?)
}

/// The race row of `name` (`faceNN`) in the male or female race list.
fn race_position(male: bool, name: &str) -> Option<u8> {
    let face = name
        .strip_prefix(RACE_PREFIX)?
        .parse::<u8>()
        .ok()?
        .checked_sub(1)?;
    if !male {
        return (face < FEMALE_RACES).then_some(face);
    }
    let row = MALE_CHROMA_ROWS.iter().fold(
        face,
        |row, chroma| if row >= *chroma { row + 1 } else { row },
    );
    (row < MALE_RACES).then_some(row)
}

/// The row of `name` in `list`.
fn list_position(list: ChoiceList, name: &str) -> Option<u8> {
    let row = match list.numbering {
        Numbering::NoneFirst if name.eq_ignore_ascii_case(NONE_ROW) => 0,
        Numbering::NoneFirst => number_after(name, list.prefix)?,
        Numbering::FromOne => number_after(name, list.prefix)?.checked_sub(1)?,
    };
    (row < list.rows).then_some(row)
}

/// The number in `name` after `prefix`, without the hair suffix.
fn number_after(name: &str, prefix: &str) -> Option<u8> {
    let rest = name.strip_prefix(prefix)?;
    rest.strip_suffix(HAIR_SUFFIX).unwrap_or(rest).parse().ok()
}

/// The root element of the `visualsBlob` of creation settings `params`.
fn visuals(params: &str) -> Option<Element> {
    let text: String = inflated(params)?.into_iter().map(char::from).collect();
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let blob: Vec<u8> = json
        .get(VISUALS_BLOB_KEY)?
        .as_str()?
        .chars()
        .map(|unit| u8::try_from(unit).ok())
        .collect::<Option<_>>()?;
    parse_object_stream(&blob).ok()
}

/// The saved names of the live test character, in [`PlayerLook`] child order: the look of the
/// characters the test client creates with an archetype pick ([`bot_creation_params`]).
const MALE_NAMES: [&str; LOOK_CHOICES] = [
    "Male",
    "face05",
    "skin01",
    "hairstyle65m",
    "facialhair44m",
    "haircolor5",
    "haircolor5",
    "eyecolor12",
    "none",
    "scar6",
    "tattoo26",
    "tattoocolor15",
];
/// Bytes below this one go in the JSON string as `\u` escapes, as the client writes them.
const ESCAPE_BELOW: u8 = 0x20;

/// The children of the look `names`, in [`PlayerLook`] child order.
fn names(names: &[&str]) -> Vec<(&'static str, Vec<u8>)> {
    [GENDER_NAME, RACE_NAME]
        .into_iter()
        .chain(CHOICE_LISTS.iter().map(|list| list.name))
        .zip(names)
        .map(|(child, name)| (child, name.as_bytes().to_vec()))
        .collect()
}

/// Creation settings as the client writes them, with the ObjectStream children `children`: the
/// ObjectStream bytes as one JSON string, byte by byte, in zlib and base64. `None` when the
/// stream does not write.
pub fn creation_params(children: &[(&str, Vec<u8>)]) -> Option<String> {
    let root = Element {
        children: children
            .iter()
            .map(|(name, value)| Element::with_value(lowercase_crc32(name), value.clone()))
            .collect(),
        ..Element::default()
    };
    let blob = write_object_stream(&root).ok()?;
    let mut json = br#"{"currency":0,"visualsBlob":""#.to_vec();
    for byte in blob {
        if byte < ESCAPE_BELOW || byte == b'"' || byte == b'\\' {
            json.extend(format!("\\u{byte:04x}").bytes());
        } else {
            json.push(byte);
        }
    }
    json.extend(br#"","position":{"x":0.0,"y":0.0,"z":0.0},"currentLevel":0}"#);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json).ok()?;
    Some(BASE64.encode(&encoder.finish().ok()?))
}

/// The creation settings of a test character: the live test character's look and the
/// archetype pick `backstory` (`Archetype_Mystic`), so that it starts with that archetype's
/// starter kit as a player does.
pub fn bot_creation_params(backstory: &str) -> Option<String> {
    let mut children = names(&MALE_NAMES);
    children.push((BACKSTORY_NAME, backstory.as_bytes().to_vec()));
    creation_params(&children)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Their list positions (player-appearance note, section 2).
    const MALE_LOOK: [u8; LOOK_CHOICES] = [0, 5, 0, 65, 44, 4, 4, 11, 0, 6, 26, 14];
    /// A value with bytes that are not UTF-8 and need JSON escapes, like the saved item ids.
    const RAW_VALUE: [u8; 4] = [0x88, 0x00, b'"', 0xff];
    /// A child that is not a look choice.
    const OTHER_CHILD: &str = "equipment";
    fn params(children: &[(&str, Vec<u8>)]) -> String {
        creation_params(children).expect("params")
    }

    fn appearance(params: String) -> serde_json::Value {
        serde_json::json!({ "Name": "Test", CREATION_PARAMS_KEY: params })
    }

    /// The stored create request of the live test character.
    pub(crate) fn saved_appearance() -> serde_json::Value {
        appearance(params(&names(&MALE_NAMES)))
    }

    /// The JSON text of `published` data.
    pub(crate) fn published_text(published: &str) -> Vec<u8> {
        inflated(published).expect("inflate")
    }

    #[test]
    fn published_data_is_the_saved_settings_at_the_level() {
        const LEVEL: u32 = 1;
        let saved = saved_appearance();
        let published = published_data(&saved, LEVEL).expect("published");
        let text = published_text(&published);
        let level = br#""currentLevel":1}"#;
        assert!(text.ends_with(level), "the level is written in");
        let again = appearance(published);
        assert_eq!(saved_look(&again), saved_look(&saved), "the look stays");
        assert_eq!(published_data(&serde_json::json!({}), LEVEL), None);
    }

    #[test]
    fn the_saved_names_become_list_positions() {
        let mut children = names(&MALE_NAMES);
        children.push((OTHER_CHILD, RAW_VALUE.to_vec()));
        let look = saved_look(&appearance(params(&children))).expect("look");
        assert_eq!(look.choices(), MALE_LOOK);
    }

    #[test]
    fn a_female_face_has_no_chroma_rows() {
        let mut names_of = MALE_NAMES;
        names_of[0] = "Female";
        names_of[1] = "face12";
        let look = saved_look(&appearance(params(&names(&names_of)))).expect("look");
        assert_eq!((look.gender, look.race), (1, 11));
    }

    #[test]
    fn male_faces_skip_the_chroma_rows() {
        assert_eq!(race_position(true, "face03"), Some(2));
        assert_eq!(race_position(true, "face04"), Some(4));
        assert_eq!(race_position(true, "face10"), Some(10));
        assert_eq!(race_position(true, "face11"), Some(12));
        assert_eq!(race_position(true, "face29"), Some(MALE_RACES - 1));
        assert_eq!(race_position(true, "face30"), None);
        assert_eq!(race_position(false, "face30"), Some(FEMALE_RACES - 1));
        assert_eq!(race_position(false, "face00"), None);
    }

    #[test]
    fn a_name_outside_its_list_gives_no_look() {
        let mut names_of = MALE_NAMES;
        names_of[3] = "hairstyle72m";
        assert_eq!(saved_look(&appearance(params(&names(&names_of)))), None);
        names_of[3] = "None";
        assert_eq!(
            saved_look(&appearance(params(&names(&names_of)))).map(|look| look.hairstyle),
            Some(0),
            "the first hair row is None"
        );
    }

    #[test]
    fn a_test_character_saves_its_archetype_and_look() {
        const PICK: &str = "Archetype_Mystic";
        let saved = appearance(bot_creation_params(PICK).expect("params"));
        assert_eq!(saved_backstory(&saved).as_deref(), Some(PICK));
        assert_eq!(saved_look(&saved), Some(PlayerLook::of_choices(MALE_LOOK)));
    }

    #[test]
    fn the_archetype_pick_is_the_saved_backstory() {
        const PICK: &str = "Archetype_Mystic";
        let mut children = names(&MALE_NAMES);
        children.push((BACKSTORY_NAME, PICK.as_bytes().to_vec()));
        let saved = appearance(params(&children));
        assert_eq!(saved_backstory(&saved).as_deref(), Some(PICK));
        assert_eq!(
            saved_backstory(&appearance(params(&names(&MALE_NAMES)))),
            None
        );
        let empty = vec![(BACKSTORY_NAME, Vec::new())];
        assert_eq!(saved_backstory(&appearance(params(&empty))), None);
        assert_eq!(saved_backstory(&serde_json::json!({})), None);
    }

    #[test]
    fn missing_or_broken_settings_give_no_look() {
        assert_eq!(saved_look(&serde_json::json!({})), None);
        assert_eq!(
            saved_look(&serde_json::json!({ CREATION_PARAMS_KEY: "{}" })),
            None
        );
        let without_race: Vec<_> = names(&MALE_NAMES)
            .into_iter()
            .filter(|(name, _)| *name != RACE_NAME)
            .collect();
        assert_eq!(saved_look(&appearance(params(&without_race))), None);
    }
}
