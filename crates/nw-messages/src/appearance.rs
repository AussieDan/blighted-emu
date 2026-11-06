// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MB::PlayerAppearanceComponentReplicatedState` (type 1195, player component 12) on the bundle
//! path (player-appearance note). The class has slot 0 only. Its constructor registers 14
//! children: twelve one-byte look choices (gender, race, skin tone, hairstyle, facial hair, hair
//! colour, facial hair colour, eye colour, face mark, scar, tattoo, tattoo colour), then
//! `iconData` (`PlayerIconData`: 7 bytes and a big-endian `u16`) and `appearanceChangeFlag` (one
//! byte). Each look byte is a position in a list of the client's character-creation data. Every
//! retail first body of the local player sets all 14 children (25 bytes).

use crate::slots::{read_slot_0, write_known};
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `MB::PlayerAppearanceComponentReplicatedState`.
pub const PLAYER_APPEARANCE_TYPE_INDEX: u64 = 1195;
/// Children registered on slot 0.
const APPEARANCE_CHILDREN: u8 = 14;
/// The look children: 0 to 11, one byte each.
pub const LOOK_CHOICES: usize = 12;
/// The child bits of the look children.
const LOOK_BITS: u16 = (1 << LOOK_CHOICES) - 1;
/// Child 12: `iconData`.
const CHILD_ICON: u8 = 12;
/// Child 13: `appearanceChangeFlag`.
const CHILD_CHANGE_FLAG: u8 = 13;
/// `appearanceChangeFlag` of a look that did not change: 0 on 6,718 of 6,726 full retail bodies.
pub const LOOK_UNCHANGED: u8 = 0;
/// `m_backgroundColorIndex` of the icon: 0 on 5,291 of 6,749 retail bodies; its table is Unknown.
pub const DEFAULT_ICON_BACKGROUND: u16 = 0;
const FIELD: &str = "player appearance";
/// `playerGender` of a female character.
const FEMALE: u8 = 1;

/// The twelve look choices of a character, in child order. Each is a position in its
/// character-creation list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayerLook {
    /// `playerGender`: 0 male, 1 female.
    pub gender: u8,
    /// `playerRace`: the face.
    pub race: u8,
    /// `playerSkinTone`.
    pub skin_tone: u8,
    /// `playerHairstyle`.
    pub hairstyle: u8,
    /// `playerFacialHair`.
    pub facial_hair: u8,
    /// `playerHairColor`.
    pub hair_color: u8,
    /// `playerFacialHairColor`.
    pub facial_hair_color: u8,
    /// `playerEyeColor`.
    pub eye_color: u8,
    /// `playerFaceMark`.
    pub face_mark: u8,
    /// `playerScar`.
    pub scar: u8,
    /// `playerTattoo`.
    pub tattoo: u8,
    /// `playerTattooColor`.
    pub tattoo_color: u8,
}

impl PlayerLook {
    /// Whether the character is female.
    pub const fn is_female(&self) -> bool {
        self.gender == FEMALE
    }

    /// The choices in child order.
    pub const fn choices(&self) -> [u8; LOOK_CHOICES] {
        [
            self.gender,
            self.race,
            self.skin_tone,
            self.hairstyle,
            self.facial_hair,
            self.hair_color,
            self.facial_hair_color,
            self.eye_color,
            self.face_mark,
            self.scar,
            self.tattoo,
            self.tattoo_color,
        ]
    }

    /// The look of `choices`, in child order.
    pub const fn of_choices(choices: [u8; LOOK_CHOICES]) -> Self {
        let [
            gender,
            race,
            skin_tone,
            hairstyle,
            facial_hair,
            hair_color,
            facial_hair_color,
            eye_color,
            face_mark,
            scar,
            tattoo,
            tattoo_color,
        ] = choices;
        Self {
            gender,
            race,
            skin_tone,
            hairstyle,
            facial_hair,
            hair_color,
            facial_hair_color,
            eye_color,
            face_mark,
            scar,
            tattoo,
            tattoo_color,
        }
    }
}

/// `PlayerIconData`, the look of the character icon: seven bytes in this order (hair colour
/// before facial hair, unlike the look children) and a big-endian `u16` background colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayerIconData {
    /// `m_genderIndex`.
    pub gender: u8,
    /// `m_raceIndex`.
    pub race: u8,
    /// `m_skintoneIndex`.
    pub skin_tone: u8,
    /// `m_hairIndex`.
    pub hair: u8,
    /// `m_hairColorIndex`.
    pub hair_color: u8,
    /// `m_facialHairIndex`.
    pub facial_hair: u8,
    /// `m_facialHairColorIndex`.
    pub facial_hair_color: u8,
    /// `m_backgroundColorIndex`.
    pub background_color: u16,
}

impl PlayerIconData {
    /// The icon of `look` on the default background. On the retail bodies the seven icon bytes
    /// equal the look's (female races excepted, for a reason that is Unknown).
    pub const fn of(look: &PlayerLook) -> Self {
        Self {
            gender: look.gender,
            race: look.race,
            skin_tone: look.skin_tone,
            hair: look.hairstyle,
            hair_color: look.hair_color,
            facial_hair: look.facial_hair,
            facial_hair_color: look.facial_hair_color,
            background_color: DEFAULT_ICON_BACKGROUND,
        }
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let [
            gender,
            race,
            skin_tone,
            hair,
            hair_color,
            facial_hair,
            facial_hair_color,
        ] = reader.array()?;
        Ok(Self {
            gender,
            race,
            skin_tone,
            hair,
            hair_color,
            facial_hair,
            facial_hair_color,
            background_color: reader.u16()?,
        })
    }

    pub(crate) fn write(&self, writer: &mut Writer) {
        writer
            .bytes(&[
                self.gender,
                self.race,
                self.skin_tone,
                self.hair,
                self.hair_color,
                self.facial_hair,
                self.facial_hair_color,
            ])
            .u16(self.background_color);
    }
}

/// The appearance state; each part when sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayerAppearanceState {
    /// The twelve look choices, sent together.
    pub look: Option<PlayerLook>,
    /// `iconData`.
    pub icon: Option<PlayerIconData>,
    /// `appearanceChangeFlag`.
    pub change_flag: Option<u8>,
}

impl PlayerAppearanceState {
    /// The full state of `look`, as a retail first body: the look, its icon, and no change.
    pub const fn of(look: PlayerLook) -> Self {
        Self {
            look: Some(look),
            icon: Some(PlayerIconData::of(&look)),
            change_flag: Some(LOOK_UNCHANGED),
        }
    }

    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early or sends only part of the look.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut choices = [0; LOOK_CHOICES];
        let mut state = Self::default();
        let children = read_slot_0(reader, FIELD, APPEARANCE_CHILDREN, |reader, child| {
            match child {
                CHILD_ICON => state.icon = Some(PlayerIconData::read(reader)?),
                CHILD_CHANGE_FLAG => state.change_flag = Some(reader.u8()?),
                _ => choices[usize::from(child)] = reader.u8()?,
            }
            Ok(())
        })?
        .unwrap_or_default();
        match children & LOOK_BITS {
            0 => {}
            LOOK_BITS => state.look = Some(PlayerLook::of_choices(choices)),
            part => {
                return Err(MessageError::Unsized {
                    field: FIELD,
                    byte: part.trailing_ones() as u8,
                });
            }
        }
        Ok(state)
    }

    /// Writes the state for the bundle path.
    pub fn write(&self, writer: &mut Writer) {
        let look_bits = if self.look.is_some() { LOOK_BITS } else { 0 };
        let children = look_bits
            | u16::from(self.icon.is_some()) << CHILD_ICON
            | u16::from(self.change_flag.is_some()) << CHILD_CHANGE_FLAG;
        let choices = self.look.map(|look| look.choices());
        write_known(writer, children, APPEARANCE_CHILDREN, |writer, child| {
            match child {
                CHILD_ICON => self.icon.map(|icon| icon.write(writer)),
                CHILD_CHANGE_FLAG => self.change_flag.map(|flag| {
                    writer.u8(flag);
                }),
                _ => choices.map(|choices| {
                    writer.u8(choices[usize::from(child)]);
                }),
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The look of the live test character: male, face05, skin01, hairstyle65m, facialhair44m,
    /// haircolor5 twice, eyecolor12, no face mark, scar6, tattoo26, tattoocolor15.
    const LOOK: [u8; LOOK_CHOICES] = [0, 5, 0, 65, 44, 4, 4, 11, 0, 6, 26, 14];
    /// Child byte of children 0 to 6 with more to come, then of children 7 to 13.
    const FIRST_GROUP: u8 = 0xff;
    const SECOND_GROUP: u8 = 0x7f;
    /// A retail full body: 25 bytes.
    const FULL_BODY_LEN: usize = 25;

    fn encoded(state: &PlayerAppearanceState) -> Vec<u8> {
        let mut writer = Writer::new();
        state.write(&mut writer);
        writer.finish()
    }

    #[test]
    fn a_full_look_is_the_25_byte_retail_form() {
        let look = PlayerLook::of_choices(LOOK);
        let bytes = encoded(&PlayerAppearanceState::of(look));
        let mut expected = vec![0x01, FIRST_GROUP];
        expected.extend_from_slice(&LOOK[..7]);
        expected.push(SECOND_GROUP);
        expected.extend_from_slice(&LOOK[7..]);
        // iconData: gender, race, skin, hair, hair colour, facial hair, facial hair colour, then
        // the background colour as a big-endian u16.
        expected.extend_from_slice(&[0, 5, 0, 65, 4, 44, 4, 0, 0]);
        expected.push(LOOK_UNCHANGED);
        assert_eq!(bytes, expected);
        assert_eq!(bytes.len(), FULL_BODY_LEN);
        let mut reader = Reader::new(&bytes);
        let read = PlayerAppearanceState::read(&mut reader).expect("read");
        reader.finish().expect("to the end");
        assert_eq!(read, PlayerAppearanceState::of(look));
        assert_eq!(read.look.map(|look| look.choices()), Some(LOOK));
    }

    #[test]
    fn gender_1_is_female() {
        let female = PlayerLook {
            gender: FEMALE,
            ..PlayerLook::default()
        };
        assert!(female.is_female());
        assert!(!PlayerLook::default().is_female(), "gender 0 is male");
    }

    #[test]
    fn no_look_is_one_zero_byte() {
        let bytes = encoded(&PlayerAppearanceState::default());
        assert_eq!(bytes, vec![0]);
        let read = PlayerAppearanceState::read(&mut Reader::new(&bytes)).expect("read");
        assert_eq!(read, PlayerAppearanceState::default());
    }

    #[test]
    fn the_icon_and_flag_alone_read_back() {
        let state = PlayerAppearanceState {
            look: None,
            icon: Some(PlayerIconData::of(&PlayerLook::of_choices(LOOK))),
            change_flag: Some(1),
        };
        let bytes = encoded(&state);
        let mut reader = Reader::new(&bytes);
        assert_eq!(
            PlayerAppearanceState::read(&mut reader).expect("read"),
            state
        );
        reader.finish().expect("to the end");
    }

    #[test]
    fn part_of_a_look_is_refused() {
        let mut reader = Reader::new(&[0x01, 0b1, 3]);
        assert!(matches!(
            PlayerAppearanceState::read(&mut reader),
            Err(MessageError::Unsized { .. })
        ));
    }
}
