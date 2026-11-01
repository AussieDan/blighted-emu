// SPDX-License-Identifier: AGPL-3.0-or-later
//! `CooldownTimersComponentReplicatedState` (type 2932, own player component 45): the running
//! ability and consumable cooldowns (xp-potion-exit note, section 2.3; ability-use-wire note).
//!
//! Slot 0 children 0 to 2 (`cdmap1` to `cdmap3`) are the cooldowns of the abilities used with
//! ability key 1, 2 and 3: a map by the CRC-32 of the lowercase ability id to the end and the
//! start of its cooldown (µs since 2000-01-01, the server clock). Retail sends each map whole at
//! the login (every mapped ability, `(1, 1)`), then one changed entry at each cast; it never
//! removes an entry (the client reads the end). Child 3 (`ccdmap`) is a list in the full form;
//! each entry names the consumable, the end of its cooldown and the cooldown group. Retail sends
//! the whole list after a potion and an empty list when the tutorial ends. Children 4 to 6
//! (`gencds`, the next daily and weekly reset) are not written here; a body that sends them is
//! [`MessageError::Unsized`].

use crate::abilities::MAX_MAPPED_SLOTS;
use crate::items::{FULL_FORM, FullList, VERSION_PRESENT};
use crate::slots::{SLOT_0, read_slot_0, write_known_children};
use crate::wire::{MessageError, Reader, Writer};

/// Type index of `CooldownTimersComponentReplicatedState`.
pub const COOLDOWNS_TYPE_INDEX: u64 = 2932;
/// The own player component of the cooldowns.
pub const COOLDOWNS_COMPONENT: u64 = 45;

/// Children of slot 0 that this server knows: 0 to 3.
const CHILDREN: u8 = 4;
const CONSUMABLES_CHILD: u8 = 3;
/// Entries per delta group of a map.
const DELTA_GROUP: usize = 8;

/// One ability cooldown of a key's map (20 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbilityCooldown {
    /// CRC-32 of the lowercase ability id.
    pub ability: u32,
    /// When it ends: µs since 2000-01-01.
    pub end_us: u64,
    /// When it started (the cast): µs since 2000-01-01.
    pub start_us: u64,
}

impl AbilityCooldown {
    fn read_value(ability: u32, reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        Ok(Self {
            ability,
            end_us: reader.u64()?,
            start_us: reader.u64()?,
        })
    }

    fn write_value(&self, writer: &mut Writer) {
        writer.u64(self.end_us).u64(self.start_us);
    }
}

/// One changed entry of a key's map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CooldownChange {
    /// The entry.
    pub cooldown: AbilityCooldown,
    /// Its version (retail: a count of the map's changes, Inferred).
    pub version: u64,
}

/// A key's cooldown map, whole or as changed entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CooldownMap {
    /// The whole map.
    Full(FullList<AbilityCooldown>),
    /// Changed entries.
    Changes(Vec<CooldownChange>),
}

impl CooldownMap {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        if Reader::new(reader.rest()).wide_prefixed()? == FULL_FORM {
            return FullList::read(reader, |reader| {
                let ability = reader.u32()?;
                AbilityCooldown::read_value(ability, reader)
            })
            .map(Self::Full);
        }
        let count = reader.wide_prefixed()?;
        // Each entry takes at least 22 bytes.
        if count > reader.remaining() as u64 {
            return Err(MessageError::LengthMismatch { field: FIELD });
        }
        // At most the bytes left.
        let mut left = count as usize;
        let mut changes = Vec::with_capacity(left);
        while left > 0 {
            let mask = reader.u8()?;
            let group = left.min(DELTA_GROUP);
            for entry in 0..group {
                let ability = reader.u32()?;
                if reader.u8()? != VERSION_PRESENT || mask & 1 << entry == 0 {
                    return Err(MessageError::Unsized {
                        field: FIELD,
                        byte: mask,
                    });
                }
                let version = reader.wide_prefixed()?;
                changes.push(CooldownChange {
                    cooldown: AbilityCooldown::read_value(ability, reader)?,
                    version,
                });
            }
            left -= group;
        }
        Ok(Self::Changes(changes))
    }

    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        match self {
            Self::Full(list) => list.write(writer, |writer, cooldown| {
                writer.u32(cooldown.ability);
                cooldown.write_value(writer);
                Ok(())
            }),
            Self::Changes(changes) => {
                // A `usize` count fits 64 bits.
                writer.wide_prefixed(changes.len() as u64);
                for group in changes.chunks(DELTA_GROUP) {
                    // Every entry of a group carries a value; a group has at most 8 entries.
                    writer.u8(u8::MAX >> (DELTA_GROUP - group.len()));
                    for change in group {
                        writer
                            .u32(change.cooldown.ability)
                            .u8(VERSION_PRESENT)
                            .wide_prefixed(change.version);
                        change.cooldown.write_value(writer);
                    }
                }
                Ok(())
            }
        }
    }
}
const FIELD: &str = "cooldowns";

/// One running consumable cooldown (20 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumableCooldown {
    /// CRC-32 of the lowercase consumable id.
    pub consumable: u32,
    /// When it ends: µs since 2000-01-01.
    pub end_us: u64,
    /// CRC-32 of the lowercase cooldown id (the group of consumables that share it).
    pub group: u32,
}

impl ConsumableCooldown {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let consumable = reader.u32()?;
        let end_us = reader.u64()?;
        let group = reader.u32()?;
        let again = reader.u32()?;
        if again != consumable {
            return Err(MessageError::Unsized {
                field: FIELD,
                byte: 0,
            });
        }
        Ok(Self {
            consumable,
            end_us,
            group,
        })
    }

    /// Retail writes the consumable twice: before the end time and after the group.
    fn write(&self, writer: &mut Writer) {
        writer
            .u32(self.consumable)
            .u64(self.end_us)
            .u32(self.group)
            .u32(self.consumable);
    }
}

/// The cooldowns state; `None` sends no change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CooldownsState {
    /// The ability cooldown map of each ability key (`cdmap1` to `cdmap3`).
    pub keys: [Option<CooldownMap>; MAX_MAPPED_SLOTS],
    /// The consumable cooldowns.
    pub consumables: Option<FullList<ConsumableCooldown>>,
}

impl CooldownsState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, sends a child above 3, or names two
    /// consumables in one entry.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut state = Self::default();
        read_slot_0(reader, FIELD, CHILDREN, |reader, child| {
            if child == CONSUMABLES_CHILD {
                state.consumables = Some(FullList::read(reader, |reader| {
                    ConsumableCooldown::read(reader)
                })?);
                return Ok(());
            }
            match state.keys.get_mut(usize::from(child)) {
                Some(key) => {
                    *key = Some(CooldownMap::read(reader)?);
                    Ok(())
                }
                None => Err(MessageError::Unsized {
                    field: FIELD,
                    byte: child,
                }),
            }
        })?;
        Ok(state)
    }

    /// Writes the state.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the list cannot be written.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let mut children: u16 = 0;
        for (child, key) in self.keys.iter().enumerate() {
            if key.is_some() {
                children |= 1 << child;
            }
        }
        if self.consumables.is_some() {
            children |= 1 << CONSUMABLES_CHILD;
        }
        if children == 0 {
            writer.u8(0);
            return Ok(());
        }
        writer.u8(SLOT_0);
        let mut result = Ok(());
        write_known_children(writer, children, CHILDREN, |writer, child| {
            if result.is_err() {
                return;
            }
            result = match (
                self.keys.get(usize::from(child)).and_then(Option::as_ref),
                &self.consumables,
            ) {
                (Some(map), _) => map.write(writer),
                (None, Some(list)) if child == CONSUMABLES_CHILD => {
                    list.write(writer, |writer, cooldown| {
                        cooldown.write(writer);
                        Ok(())
                    })
                }
                _ => Ok(()),
            };
        });
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Retail after the potion (xp-potion-exit note, section 2.3).
    const RETAIL: [u8; 25] = [
        0x01, 0x08, 0x00, 0x01, 0xb6, 0xd7, 0x01, 0x57, 0x31, 0x5d, 0x70, 0x00, 0x03, 0x01, 0x30,
        0x0c, 0x2a, 0x3e, 0x84, 0x3b, 0xe0, 0xa5, 0x4a, 0x57, 0x31,
    ];

    #[test]
    fn the_retail_potion_cooldown_reads_and_writes_the_same_bytes() {
        let mut body = RETAIL.to_vec();
        body.extend([0x5d, 0x70]);
        let mut reader = Reader::new(&body);
        let state = CooldownsState::read(&mut reader).expect("read");
        reader.finish().expect("whole");
        assert_eq!(
            state.consumables.as_ref().map(|list| list.elements.clone()),
            Some(vec![ConsumableCooldown {
                consumable: 0x5731_5d70,
                end_us: 0x0003_0130_0c2a_3e84,
                group: 0x3be0_a54a,
            }])
        );
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        assert_eq!(writer.finish(), body);
        let mut writer = Writer::new();
        CooldownsState::default().write(&mut writer).expect("write");
        assert_eq!(writer.finish(), [0]);
    }

    /// Retail Sacred Ground on ability key 1 (`general-other-01` 79.792 s, ability-use-wire
    /// note): one changed entry of `cdmap1`.
    const RETAIL_CAST: [u8; 27] = [
        0x01, 0x01, 0x01, 0x01, 0x50, 0x9e, 0xcd, 0x28, 0x01, 0xb5, 0x18, 0x00, 0x03, 0x01, 0xaf,
        0xd8, 0x9e, 0x79, 0x03, 0x00, 0x03, 0x01, 0xaf, 0xd7, 0x90, 0x7b, 0xd3,
    ];

    #[test]
    fn the_retail_ability_cooldown_reads_and_writes_the_same_bytes() {
        let mut reader = Reader::new(&RETAIL_CAST);
        let state = CooldownsState::read(&mut reader).expect("read");
        reader.finish().expect("whole");
        let Some(CooldownMap::Changes(changes)) = &state.keys[0] else {
            panic!("a change of cdmap1: {state:?}");
        };
        let change = changes[0];
        assert_eq!(
            change.cooldown.ability, 0x509e_cd28,
            "Ability_Lifestaff_SacredGround"
        );
        assert_eq!(change.cooldown.end_us, 846_279_695_300_867);
        assert_eq!(change.cooldown.start_us, 846_279_677_606_867);
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        assert_eq!(writer.finish(), RETAIL_CAST);
    }

    #[test]
    fn a_whole_map_and_the_consumables_round_trip() {
        let state = CooldownsState {
            keys: [
                None,
                Some(CooldownMap::Full(FullList::first(vec![AbilityCooldown {
                    ability: 7,
                    end_us: 1,
                    start_us: 1,
                }]))),
                None,
            ],
            consumables: Some(FullList::first(Vec::new())),
        };
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        assert_eq!(CooldownsState::read(&mut reader).expect("read"), state);
        reader.finish().expect("whole");
    }
}
