// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MB::VitalsComponentReplicatedState` (type 15) on the bundle path (vitals note, Rounds 3 and
//! 4). The class has two slots; a body with both reads slot 0, then slot 1.
//!
//! The constructor `FUN_14671e040` appends 14 children to slot 0: `HealthAmount`,
//! `StaminaAmount` (big-endian `f32`), `vitalsData` (`FUN_1469515b0`: a byte, two big-endian
//! `u64`, a byte), `healthChangeFlags` (a byte), `HealthMax` (`f32`), `HealthTickRate` (`u16`),
//! `StaminaMax` (`f32`), `StaminaTickRate` (`u16`), `vitalsId`, `vitalsCategoryId`, `vitalsLevel`
//! (`u32`), `invulnerability`, `displayImmuneWhenInvulnerable` (a byte each) and `maxHealth`
//! (`u16`). That table ends 99.997 % of 2,396,697 captured bodies. Slot 1 has `ManaAmount`
//! (`f32`), the hot and cold affliction lists ([`crate::afflictions`]), `ManaMax` (`f32`) and
//! `ManaTickRate` (`u16`); it ends every captured slot-1 body.

use crate::afflictions::{AfflictionList, ColdLong, HotLong};
use crate::slots::{
    children_mask, read_children, read_slot_byte, write_children, write_known_children,
};
use crate::time_synch::NS_PER_MS;
use crate::wire::{MessageError, Reader, Writer};

/// Type index of the class.
pub const VITALS_TYPE_INDEX: u64 = 15;
const SLOTS: u8 = 2;
const SLOT_0_BIT: u8 = 0b01;
const SLOT_1_BIT: u8 = 0b10;
const SLOT_0_CHILDREN: u8 = 14;
const SLOT_1_CHILDREN: u8 = 5;
const FIELD: &str = "vitals";
/// The `healthChangeFlags` of a creature's first damage: 0 in every retail card, 3 with the
/// first hit of every tutorial creature (sailors, legionnaire, signifier, risen, Thorpe); the
/// meaning of the bits is Unknown.
pub const FIRST_DAMAGE_FLAGS: u8 = 3;
/// `vitalsId` of a player: the lowercase CRC-32 of the vitals row `Player`, on every local-player
/// body of the captures (player-appearance note, section 3). The client finds the mana regen rate through it.
pub const PLAYER_VITALS_ID: u32 = 0x9819_7a65;
/// Max stamina of the vitals row `Player` (`StaminaBaseMax`); 100 on every captured local player.
pub const PLAYER_STAMINA_MAX: f32 = 100.0;
/// Max mana of the vitals row `Player` (`ManaBaseMax`); 100 on the captured local players.
pub const PLAYER_MANA_MAX: f32 = 100.0;
/// `StaminaTickRate` on all 1,333 captured local-player bodies that carry it; its unit is Unknown.
pub const PLAYER_STAMINA_TICK_RATE: u16 = 0x5100;
/// `HealthTickRate` and `ManaTickRate` on every captured local-player body.
pub const PLAYER_TICK_RATE: u16 = 0;
/// The `vitalsData` head byte of a dead creature (mob-cards note, section 5).
const DEAD_HEAD: u8 = 1;
/// The `vitalsData` head byte of a player at death's door (status effects note 2026-10-31:
/// `NH-MSQ-5.1` 523.141 s, with `invulnerability` 1, before the dead head).
const DEATHS_DOOR_HEAD: u8 = 2;
/// The `invulnerability` of a player at death's door.
const DEATHS_DOOR_INVULNERABLE: u8 = 1;
/// The `vitalsData` head byte of a live creature's first state (ship-mobs-lockon-ai note,
/// section 3.3: every retail tutorial creature card).
const ALIVE_HEAD: u8 = 0;
/// The `vitalsData` tail byte on every retail creature body seen (mob-cards note, section 2).
const CREATURE_DATA_TAIL: u8 = 0xff;

/// The `vitalsData` child (reader `FUN_1469515b0`): a byte, two big-endian `u64`, a byte. The
/// meaning is Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VitalsData {
    /// The first byte.
    pub head: u8,
    /// The first `u64`.
    pub a: u64,
    /// The second `u64`.
    pub b: u64,
    /// The last byte.
    pub tail: u8,
}

impl VitalsData {
    /// The block of a creature's first state at server time `server_ms` (ms since 2000-01-01)
    /// with health `health`. Alive, retail sends `00 <server ns> 0 ff`: the time is the server
    /// clock at the card (sailor 1: `0bbca3350847b1bf` = 13:04:38.206 at its card at 16198 ms).
    /// At 0 health it is the dead block.
    pub const fn creature_first(health: u32, server_ms: u64) -> Self {
        if health == 0 {
            Self::DEAD
        } else {
            Self::alive_since(server_ms)
        }
    }

    /// The live block of an entity that spawned at server time `server_ms` (ms since
    /// 2000-01-01): `00 <server ns> 0 ff` (a creature's first state, and a placed camp's:
    /// `Hivecamp` 1781367544743 sent `0b956388ac0fc109`, its placement time).
    pub const fn alive_since(server_ms: u64) -> Self {
        Self {
            head: ALIVE_HEAD,
            a: server_ms.saturating_mul(NS_PER_MS),
            b: 0,
            tail: CREATURE_DATA_TAIL,
        }
    }

    /// The block of a player who died: the dead head and the server time of its last spawn
    /// `spawned_ms` in ns (open-world note: retail sent it 55 to 72 ms after the 4299; the client
    /// then opened its death screen).
    pub const fn player_dead(spawned_ms: u64) -> Self {
        Self {
            head: DEAD_HEAD,
            a: spawned_ms.saturating_mul(NS_PER_MS),
            b: 0,
            tail: CREATURE_DATA_TAIL,
        }
    }

    /// The block of a player who went down to death's door: its head and the server time of its
    /// last spawn (retail kept `a` as it was).
    pub const fn player_at_deaths_door(spawned_ms: u64) -> Self {
        Self {
            head: DEATHS_DOOR_HEAD,
            ..Self::player_dead(spawned_ms)
        }
    }

    /// The dead block of a player who died at death's door after `door_ms` there (retail
    /// `NH-MSQ-5.1`: `b` 2430 after a hit 2.43 s in; about 120,033 after the full time).
    pub const fn player_dead_after_door(spawned_ms: u64, door_ms: u64) -> Self {
        Self {
            b: door_ms,
            ..Self::player_dead(spawned_ms)
        }
    }

    /// It is the block of a dead body.
    pub const fn is_dead(&self) -> bool {
        self.head == DEAD_HEAD
    }

    /// It is the block of a player at death's door.
    pub const fn is_at_deaths_door(&self) -> bool {
        self.head == DEATHS_DOOR_HEAD
    }

    /// The block of a player who lives again at server time `server_ms` (retail 483 to 488 ms
    /// after the respawn request 1938).
    pub const fn player_alive(server_ms: u64) -> Self {
        Self {
            head: ALIVE_HEAD,
            a: server_ms.saturating_mul(NS_PER_MS),
            b: 0,
            tail: CREATURE_DATA_TAIL,
        }
    }

    /// The block of a dead creature: its head plays the death on the client.
    const DEAD: Self = Self {
        head: DEAD_HEAD,
        a: 0,
        b: 0,
        tail: CREATURE_DATA_TAIL,
    };
}

/// The vitals state: slot 0 and slot 1, each when sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VitalsState {
    /// Slot 0: health, stamina and the ids.
    pub slot_0: Option<VitalsSlot0>,
    /// Slot 1: mana and the afflictions.
    pub slot_1: Option<VitalsSlot1>,
}

impl VitalsState {
    /// The first vitals of a player with `health` of `health_max`, full stamina and mana: the
    /// children of a retail first local-player body (slot 0 children 0, 1, 4 to 8 and 13; slot 1
    /// children 0, 3 and 4), without `vitalsData`, `healthChangeFlags` and the affliction lists,
    /// whose meaning or form is Unknown.
    pub fn player(health: u32, health_max: u32) -> Self {
        Self {
            slot_0: Some(VitalsSlot0 {
                health_amount: Some(health as f32),
                stamina_amount: Some(PLAYER_STAMINA_MAX),
                health_max: Some(health_max as f32),
                health_tick_rate: Some(PLAYER_TICK_RATE),
                stamina_max: Some(PLAYER_STAMINA_MAX),
                stamina_tick_rate: Some(PLAYER_STAMINA_TICK_RATE),
                vitals_id: Some(PLAYER_VITALS_ID),
                max_health: Some(u16::try_from(health_max).unwrap_or(u16::MAX)),
                ..VitalsSlot0::default()
            }),
            slot_1: Some(VitalsSlot1 {
                mana_amount: Some(PLAYER_MANA_MAX),
                mana_max: Some(PLAYER_MANA_MAX),
                mana_tick_rate: Some(PLAYER_TICK_RATE),
                ..VitalsSlot1::default()
            }),
        }
    }

    /// The first vitals of a creature: `health` of `health_max`, its vitals and category ids and
    /// its level (the children of a retail first creature body that this server knows; the
    /// meaning of the others is Unknown).
    pub fn creature(
        health: u32,
        health_max: u32,
        vitals_id: u32,
        category_id: u32,
        level: u32,
    ) -> Self {
        Self {
            slot_0: Some(VitalsSlot0 {
                health_amount: Some(health as f32),
                health_max: Some(health_max as f32),
                vitals_id: Some(vitals_id),
                vitals_category_id: Some(category_id),
                vitals_level: Some(level),
                max_health: Some(u16::try_from(health_max).unwrap_or(u16::MAX)),
                ..VitalsSlot0::default()
            }),
            slot_1: None,
        }
    }

    /// A recorded retail first state of a creature (with its stamina, data and tick children)
    /// with this creature's `health`, `health_max`, ids and `level` written in, as
    /// [`VitalsState::creature`] sets them.
    pub fn creature_as_recorded(
        &self,
        health: u32,
        health_max: u32,
        vitals_id: u32,
        category_id: u32,
        level: u32,
    ) -> Self {
        let ours = Self::creature(health, health_max, vitals_id, category_id, level).slot_0;
        let (Some(recorded), Some(ours)) = (self.slot_0, ours) else {
            return Self::creature(health, health_max, vitals_id, category_id, level);
        };
        Self {
            slot_0: Some(VitalsSlot0 {
                health_amount: ours.health_amount,
                health_max: ours.health_max,
                vitals_id: ours.vitals_id,
                vitals_category_id: ours.vitals_category_id,
                vitals_level: ours.vitals_level,
                max_health: ours.max_health,
                ..recorded
            }),
            slot_1: self.slot_1.clone(),
        }
    }

    /// This state with the max health `health_max` in slot 0 (`healthMax` and `maxHealth`).
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "health stays far below 2^24")]
    pub fn with_health_max(mut self, health_max: u32) -> Self {
        let slot = self.slot_0.get_or_insert_with(VitalsSlot0::default);
        slot.health_max = Some(health_max as f32);
        slot.max_health = Some(u16::try_from(health_max).unwrap_or(u16::MAX));
        self
    }

    /// A change of a creature's health to `health`. A creature at 0 is dead: its `vitalsData`
    /// head is 1, which plays the death on the client.
    pub fn creature_health(health: u32) -> Self {
        Self::creature_change(health, None)
    }

    /// The change of a creature's first damage: its health `health` and `healthChangeFlags`
    /// [`FIRST_DAMAGE_FLAGS`] (retail: every tutorial creature's first hit, `01 09 <health> 03`).
    pub fn creature_first_damage(health: u32) -> Self {
        Self::creature_change(health, Some(FIRST_DAMAGE_FLAGS))
    }

    /// A player's death or new life: its health `health`, its `vitalsData` `data` and
    /// `healthChangeFlags` [`FIRST_DAMAGE_FLAGS`] (retail `01 0d <health> <data> 03`).
    #[allow(clippy::cast_precision_loss, reason = "health stays far below 2^24")]
    pub fn player_life(health: u32, data: VitalsData) -> Self {
        Self {
            slot_0: Some(VitalsSlot0 {
                health_amount: Some(health as f32),
                health_change_flags: Some(FIRST_DAMAGE_FLAGS),
                vitals_data: Some(data),
                // Death's door holds the player invulnerable (retail 1 with its head; retail sent
                // no change of it with the dead and the alive blocks after it).
                invulnerability: data.is_at_deaths_door().then_some(DEATHS_DOOR_INVULNERABLE),
                ..VitalsSlot0::default()
            }),
            slot_1: None,
        }
    }

    #[allow(clippy::cast_precision_loss, reason = "health stays far below 2^24")]
    fn creature_change(health: u32, health_change_flags: Option<u8>) -> Self {
        Self {
            slot_0: Some(VitalsSlot0 {
                health_amount: Some(health as f32),
                health_change_flags,
                vitals_data: (health == 0).then_some(VitalsData::DEAD),
                ..VitalsSlot0::default()
            }),
            slot_1: None,
        }
    }

    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early, uses a slot above 1, or has a form
    /// `write` cannot make.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slots = read_slot_byte(reader, FIELD, SLOTS)?;
        let slot_0 = (slots & SLOT_0_BIT != 0)
            .then(|| VitalsSlot0::read(reader))
            .transpose()?;
        let slot_1 = (slots & SLOT_1_BIT != 0)
            .then(|| VitalsSlot1::read(reader))
            .transpose()?;
        Ok(Self { slot_0, slot_1 })
    }

    /// Writes the state for the bundle path. A slot without a child is not sent.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Prefix`] when an affliction count does not fit the prefix code.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        let slot_0 = self.slot_0.filter(|slot| slot.children() != 0);
        let slot_1 = self.slot_1.as_ref().filter(|slot| slot.children() != 0);
        let mut slots = 0;
        if slot_0.is_some() {
            slots |= SLOT_0_BIT;
        }
        if slot_1.is_some() {
            slots |= SLOT_1_BIT;
        }
        writer.u8(slots);
        if let Some(slot) = slot_0 {
            slot.write(writer);
        }
        if let Some(slot) = slot_1 {
            slot.write(writer)?;
        }
        Ok(())
    }
}

/// Slot 0 of the vitals state; every child is optional.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VitalsSlot0 {
    /// `HealthAmount`: the current health.
    pub health_amount: Option<f32>,
    /// `StaminaAmount`.
    pub stamina_amount: Option<f32>,
    /// `vitalsData`.
    pub vitals_data: Option<VitalsData>,
    /// `healthChangeFlags`.
    pub health_change_flags: Option<u8>,
    /// `HealthMax`.
    pub health_max: Option<f32>,
    /// `HealthTickRate`.
    pub health_tick_rate: Option<u16>,
    /// `StaminaMax`.
    pub stamina_max: Option<f32>,
    /// `StaminaTickRate`.
    pub stamina_tick_rate: Option<u16>,
    /// `vitalsId`.
    pub vitals_id: Option<u32>,
    /// `vitalsCategoryId`.
    pub vitals_category_id: Option<u32>,
    /// `vitalsLevel`.
    pub vitals_level: Option<u32>,
    /// `invulnerability`.
    pub invulnerability: Option<u8>,
    /// `displayImmuneWhenInvulnerable`.
    pub display_immune: Option<u8>,
    /// `maxHealth`.
    pub max_health: Option<u16>,
}

impl VitalsSlot0 {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut slot = Self::default();
        read_children(reader, FIELD, SLOT_0_CHILDREN, |reader, child| {
            match child {
                0 => slot.health_amount = Some(reader.f32()?),
                1 => slot.stamina_amount = Some(reader.f32()?),
                2 => slot.vitals_data = Some(read_data(reader)?),
                3 => slot.health_change_flags = Some(reader.u8()?),
                4 => slot.health_max = Some(reader.f32()?),
                5 => slot.health_tick_rate = Some(reader.u16()?),
                6 => slot.stamina_max = Some(reader.f32()?),
                7 => slot.stamina_tick_rate = Some(reader.u16()?),
                8 => slot.vitals_id = Some(reader.u32()?),
                9 => slot.vitals_category_id = Some(reader.u32()?),
                10 => slot.vitals_level = Some(reader.u32()?),
                11 => slot.invulnerability = Some(reader.u8()?),
                12 => slot.display_immune = Some(reader.u8()?),
                _ => slot.max_health = Some(reader.u16()?),
            }
            Ok(())
        })?;
        Ok(slot)
    }

    /// The child bitmap of the children that are sent.
    pub fn children(&self) -> u16 {
        children_mask(&[
            self.health_amount.is_some(),
            self.stamina_amount.is_some(),
            self.vitals_data.is_some(),
            self.health_change_flags.is_some(),
            self.health_max.is_some(),
            self.health_tick_rate.is_some(),
            self.stamina_max.is_some(),
            self.stamina_tick_rate.is_some(),
            self.vitals_id.is_some(),
            self.vitals_category_id.is_some(),
            self.vitals_level.is_some(),
            self.invulnerability.is_some(),
            self.display_immune.is_some(),
            self.max_health.is_some(),
        ])
    }

    /// Writes the child bytes and the children that are sent.
    fn write(&self, writer: &mut Writer) {
        write_known_children(writer, self.children(), SLOT_0_CHILDREN, |writer, child| {
            match child {
                0 => self.health_amount.map(|value| writer.f32(value)),
                1 => self.stamina_amount.map(|value| writer.f32(value)),
                2 => self
                    .vitals_data
                    .map(|data| writer.u8(data.head).u64(data.a).u64(data.b).u8(data.tail)),
                3 => self.health_change_flags.map(|flags| writer.u8(flags)),
                4 => self.health_max.map(|value| writer.f32(value)),
                5 => self.health_tick_rate.map(|rate| writer.u16(rate)),
                6 => self.stamina_max.map(|value| writer.f32(value)),
                7 => self.stamina_tick_rate.map(|rate| writer.u16(rate)),
                8 => self.vitals_id.map(|id| writer.u32(id)),
                9 => self.vitals_category_id.map(|id| writer.u32(id)),
                10 => self.vitals_level.map(|level| writer.u32(level)),
                11 => self.invulnerability.map(|byte| writer.u8(byte)),
                12 => self.display_immune.map(|byte| writer.u8(byte)),
                _ => self.max_health.map(|max| writer.u16(max)),
            };
        });
    }
}

/// Slot 1 of the vitals state; every child is optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VitalsSlot1 {
    /// `ManaAmount`.
    pub mana_amount: Option<f32>,
    /// `replicatedAfflictionsHotData`.
    pub afflictions_hot: Option<AfflictionList<HotLong>>,
    /// `replicatedAfflictionsColdData`.
    pub afflictions_cold: Option<AfflictionList<ColdLong>>,
    /// `ManaMax`.
    pub mana_max: Option<f32>,
    /// `ManaTickRate`.
    pub mana_tick_rate: Option<u16>,
}

impl VitalsSlot1 {
    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let mut slot = Self::default();
        read_children(reader, FIELD, SLOT_1_CHILDREN, |reader, child| {
            match child {
                0 => slot.mana_amount = Some(reader.f32()?),
                1 => slot.afflictions_hot = Some(AfflictionList::read(reader)?),
                2 => slot.afflictions_cold = Some(AfflictionList::read(reader)?),
                3 => slot.mana_max = Some(reader.f32()?),
                _ => slot.mana_tick_rate = Some(reader.u16()?),
            }
            Ok(())
        })?;
        Ok(slot)
    }

    /// The child bitmap of the children that are sent.
    pub fn children(&self) -> u16 {
        children_mask(&[
            self.mana_amount.is_some(),
            self.afflictions_hot.is_some(),
            self.afflictions_cold.is_some(),
            self.mana_max.is_some(),
            self.mana_tick_rate.is_some(),
        ])
    }

    /// Writes the child bytes and the children that are sent.
    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        write_children(writer, self.children(), SLOT_1_CHILDREN, |writer, child| {
            match child {
                0 => {
                    self.mana_amount.map(|mana| writer.f32(mana));
                }
                1 => {
                    if let Some(list) = &self.afflictions_hot {
                        list.write(writer)?;
                    }
                }
                2 => {
                    if let Some(list) = &self.afflictions_cold {
                        list.write(writer)?;
                    }
                }
                3 => {
                    self.mana_max.map(|mana_max| writer.f32(mana_max));
                }
                _ => {
                    self.mana_tick_rate.map(|rate| writer.u16(rate));
                }
            }
            Ok(())
        })
    }
}

fn read_data(reader: &mut Reader<'_>) -> Result<VitalsData, MessageError> {
    Ok(VitalsData {
        head: reader.u8()?,
        a: reader.u64()?,
        b: reader.u64()?,
        tail: reader.u8()?,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_player_death_has_the_retail_bytes() {
        // Retail own vitals after a death: `01 0d 00000000 01 <spawn ns> 0..0 ff 03`.
        const SPAWNED_MS: u64 = 845_730_262_408;
        let mut writer = Writer::new();
        VitalsState::player_life(0, VitalsData::player_dead(SPAWNED_MS))
            .write(&mut writer)
            .expect("write");
        let mut expected = vec![0x01, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x01];
        expected.extend((SPAWNED_MS * NS_PER_MS).to_be_bytes());
        expected.extend([0; 8]);
        expected.extend([0xff, FIRST_DAMAGE_FLAGS]);
        assert_eq!(writer.finish(), expected);
        let alive = VitalsData::player_alive(SPAWNED_MS);
        assert_eq!((alive.head, alive.a), (ALIVE_HEAD, SPAWNED_MS * NS_PER_MS));
    }

    #[test]
    fn a_first_damage_is_the_retail_body() {
        // Retail sailor 1, first hit (78114 of the tutorial capture): `01 09 43754dbc 03`.
        const RETAIL: [u8; 7] = [0x01, 0x09, 0x43, 0x75, 0x4d, 0xbc, 0x03];
        let mut state = VitalsState::creature_first_damage(245);
        if let Some(slot) = state.slot_0.as_mut() {
            slot.health_amount = Some(f32::from_bits(0x4375_4dbc));
        }
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        assert_eq!(writer.finish(), RETAIL);
    }
    use crate::afflictions::{Affliction, AfflictionId};

    const HEALTH: f32 = 700.5;
    const STAMINA: f32 = 100.0;
    const MANA: f32 = 55.5;
    const TICK_RATE: u16 = 5;

    fn round(state: &VitalsState) -> VitalsState {
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        read_all(&writer.finish())
    }

    fn read_all(bytes: &[u8]) -> VitalsState {
        let mut reader = Reader::new(bytes);
        let state = VitalsState::read(&mut reader).expect("read");
        reader.finish().expect("to the end");
        state
    }

    fn slot_0(bytes: &[u8]) -> VitalsSlot0 {
        read_all(bytes).slot_0.expect("slot 0")
    }

    /// A finite float: a NaN is not equal to itself.
    fn float() -> impl Strategy<Value = Option<f32>> {
        proptest::option::of(-1.0e9_f32..1.0e9_f32)
    }

    fn data() -> impl Strategy<Value = Option<VitalsData>> {
        proptest::option::of(
            (any::<u8>(), any::<u64>(), any::<u64>(), any::<u8>())
                .prop_map(|(head, a, b, tail)| VitalsData { head, a, b, tail }),
        )
    }

    fn slot_0_state() -> impl Strategy<Value = VitalsSlot0> {
        (
            (float(), float(), float(), float()),
            data(),
            proptest::option::of(any::<u8>()),
            (
                proptest::option::of(any::<u16>()),
                proptest::option::of(any::<u16>()),
            ),
            (
                proptest::option::of(any::<u32>()),
                proptest::option::of(any::<u32>()),
                proptest::option::of(any::<u32>()),
            ),
            (
                proptest::option::of(any::<u8>()),
                proptest::option::of(any::<u8>()),
            ),
            proptest::option::of(any::<u16>()),
        )
            .prop_map(
                |(
                    (health_amount, stamina_amount, health_max, stamina_max),
                    vitals_data,
                    health_change_flags,
                    (health_tick_rate, stamina_tick_rate),
                    (vitals_id, vitals_category_id, vitals_level),
                    (invulnerability, display_immune),
                    max_health,
                )| VitalsSlot0 {
                    health_amount,
                    stamina_amount,
                    vitals_data,
                    health_change_flags,
                    health_max,
                    health_tick_rate,
                    stamina_max,
                    stamina_tick_rate,
                    vitals_id,
                    vitals_category_id,
                    vitals_level,
                    invulnerability,
                    display_immune,
                    max_health,
                },
            )
    }

    fn slot_1_state() -> impl Strategy<Value = VitalsSlot1> {
        (float(), float(), proptest::option::of(any::<u16>())).prop_map(
            |(mana_amount, mana_max, mana_tick_rate)| VitalsSlot1 {
                mana_amount,
                mana_max,
                mana_tick_rate,
                ..VitalsSlot1::default()
            },
        )
    }

    proptest! {
        #[test]
        fn vitals_round_trip(
            slot_0 in proptest::option::of(slot_0_state()),
            slot_1 in proptest::option::of(slot_1_state()),
        ) {
            let nonempty_0 = slot_0.filter(|slot| slot.children() != 0);
            let nonempty_1 = slot_1.filter(|slot| slot.children() != 0);
            let state = VitalsState { slot_0: nonempty_0, slot_1: nonempty_1 };
            prop_assert_eq!(round(&state), state);
        }
    }

    #[test]
    fn a_player_start_is_the_retail_first_body_without_unknown_children() {
        const HEALTH_NOW: u32 = 800;
        const HEALTH_MAX: u32 = 856;
        let mut writer = Writer::new();
        VitalsState::player(HEALTH_NOW, HEALTH_MAX)
            .write(&mut writer)
            .expect("write");
        let mut expected = vec![0b11, 0xf3];
        expected.extend((HEALTH_NOW as f32).to_be_bytes());
        expected.extend(PLAYER_STAMINA_MAX.to_be_bytes());
        expected.extend((HEALTH_MAX as f32).to_be_bytes());
        expected.extend(PLAYER_TICK_RATE.to_be_bytes());
        expected.extend(PLAYER_STAMINA_MAX.to_be_bytes());
        // Children 7, 8 and 13: the second child byte comes after the first seven children.
        expected.push(0x43);
        expected.extend(PLAYER_STAMINA_TICK_RATE.to_be_bytes());
        expected.extend(PLAYER_VITALS_ID.to_be_bytes());
        expected.extend((HEALTH_MAX as u16).to_be_bytes());
        // Slot 1: children 0, 3 and 4.
        expected.push(0b1_1001);
        expected.extend(PLAYER_MANA_MAX.to_be_bytes());
        expected.extend(PLAYER_MANA_MAX.to_be_bytes());
        expected.extend(PLAYER_TICK_RATE.to_be_bytes());
        let bytes = writer.finish();
        assert_eq!(bytes, expected);
        let state = read_all(&bytes);
        let slot = state.slot_0.expect("slot 0");
        assert_eq!(slot.vitals_id, Some(PLAYER_VITALS_ID));
        assert_eq!(slot.max_health, Some(HEALTH_MAX as u16));
        assert_eq!(slot.children(), 0x21f3);
    }

    #[test]
    fn a_slot_above_1_is_refused() {
        let mut reader = Reader::new(&[0b100, 0x01, 0]);
        assert!(VitalsState::read(&mut reader).is_err());
    }

    #[test]
    fn health_and_stamina_are_the_first_two_floats() {
        let mut bytes = vec![0x01, 0b11];
        bytes.extend(HEALTH.to_be_bytes());
        bytes.extend(STAMINA.to_be_bytes());
        let state = slot_0(&bytes);
        assert_eq!(
            (state.health_amount, state.stamina_amount),
            (Some(HEALTH), Some(STAMINA))
        );
    }

    #[test]
    fn the_vitals_data_block_is_child_2_of_18_bytes() {
        let mut bytes = vec![0x01, 1 << 2, 9];
        bytes.extend(1_u64.to_be_bytes());
        bytes.extend(2_u64.to_be_bytes());
        bytes.push(3);
        let data = slot_0(&bytes).vitals_data.expect("vitals data");
        assert_eq!((data.head, data.a, data.b, data.tail), (9, 1, 2, 3));
    }

    #[test]
    fn a_live_creature_first_block_is_the_retail_form() {
        // Retail sailor 1 at 16198: `00 0bbca3350847b1bf 0000000000000000 ff`.
        const RETAIL_NS: u64 = 0x0bbc_a335_0847_b1bf;
        const HEALTH_ALIVE: u32 = 353;
        let server_ms = RETAIL_NS / NS_PER_MS;
        let data = VitalsData::creature_first(HEALTH_ALIVE, server_ms);
        assert_eq!((data.head, data.b, data.tail), (0, 0, 0xff));
        assert_eq!(data.a, server_ms * NS_PER_MS);
        assert!(RETAIL_NS - data.a < NS_PER_MS);
        assert_eq!(VitalsData::creature_first(0, server_ms).head, DEAD_HEAD);
    }

    #[test]
    fn death_s_door_has_its_head_and_invulnerability_then_the_dead_block_its_time() {
        const SPAWNED_MS: u64 = 842_881_859_891;
        const DOOR_MS: u64 = 2_430;
        let door = VitalsState::player_life(0, VitalsData::player_at_deaths_door(SPAWNED_MS));
        let slot = door.slot_0.expect("slot 0");
        let data = slot.vitals_data.expect("data");
        assert!(data.is_at_deaths_door() && !data.is_dead());
        assert_eq!(slot.invulnerability, Some(DEATHS_DOOR_INVULNERABLE));
        let dead = VitalsData::player_dead_after_door(SPAWNED_MS, DOOR_MS);
        assert!(dead.is_dead());
        assert_eq!((dead.a, dead.b), (data.a, DOOR_MS), "retail NH-MSQ-5.1");
        let plain = VitalsState::player_life(0, VitalsData::player_dead(SPAWNED_MS));
        assert_eq!(plain.slot_0.expect("slot 0").invulnerability, None);
    }

    #[test]
    fn a_tick_rate_is_2_bytes() {
        let mut bytes = vec![0x01, 1 << 5];
        bytes.extend(TICK_RATE.to_be_bytes());
        assert_eq!(slot_0(&bytes).health_tick_rate, Some(TICK_RATE));
    }

    #[test]
    fn only_max_health_is_child_13() {
        let state = VitalsState {
            slot_0: Some(VitalsSlot0 {
                max_health: Some(500),
                ..VitalsSlot0::default()
            }),
            slot_1: None,
        };
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        assert_eq!(writer.finish(), vec![0x01, 0x80, 0x40, 0x01, 0xf4]);
    }

    #[test]
    fn slot_1_alone_holds_the_mana() {
        let mut bytes = vec![0b10, 0x01];
        bytes.extend(MANA.to_be_bytes());
        let state = read_all(&bytes);
        assert_eq!(state.slot_0, None);
        assert_eq!(state.slot_1.expect("slot 1").mana_amount, Some(MANA));
    }

    #[test]
    fn both_slots_read_slot_0_then_slot_1() {
        let mut bytes = vec![0b11, 0x01];
        bytes.extend(HEALTH.to_be_bytes());
        bytes.push(0x01);
        bytes.extend(MANA.to_be_bytes());
        let state = read_all(&bytes);
        assert_eq!(state.slot_0.expect("slot 0").health_amount, Some(HEALTH));
        assert_eq!(state.slot_1.expect("slot 1").mana_amount, Some(MANA));
    }

    #[test]
    fn slot_1_holds_both_affliction_lists_then_the_mana_max_and_tick() {
        let state = VitalsState {
            slot_0: None,
            slot_1: Some(VitalsSlot1 {
                afflictions_hot: Some(AfflictionList::Full {
                    id: AfflictionId::None,
                    items: vec![(
                        1,
                        HotLong {
                            a: 2,
                            b: 3,
                            c: 4,
                            d: 5,
                        },
                    )],
                }),
                afflictions_cold: Some(AfflictionList::Items(vec![Affliction {
                    head: 6,
                    id: AfflictionId::Wide(7),
                    long: Some(ColdLong {
                        value: 8,
                        byte: 9,
                        flag: true,
                    }),
                }])),
                mana_max: Some(MANA),
                mana_tick_rate: Some(TICK_RATE),
                ..VitalsSlot1::default()
            }),
        };
        assert_eq!(round(&state), state);
    }

    #[test]
    fn a_recorded_creature_state_keeps_its_children_with_our_health() {
        // Sailor 1's first vitals body (mob-cards note, section 2.1).
        const RETAIL_SAILOR: &str = concat!(
            "01ff43b0800000000000000bb5e1fa5b636ba00000000000000000ff0043b0800000003f8000004f00",
            "00886982c9a452daaa000000010161"
        );
        const HEALTH: u32 = 300;
        const LEVEL: u32 = 2;
        let bytes: Vec<u8> = (0..RETAIL_SAILOR.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&RETAIL_SAILOR[at..at + 2], 16).expect("hex"))
            .collect();
        let recorded = VitalsState::read(&mut Reader::new(&bytes)).expect("read");
        let ours = recorded.creature_as_recorded(HEALTH, HEALTH, 1, 2, LEVEL);
        let (Some(was), Some(now)) = (recorded.slot_0, ours.slot_0) else {
            panic!("slot 0");
        };
        assert_eq!(now.health_amount, Some(HEALTH as f32));
        assert_eq!((now.vitals_id, now.vitals_level), (Some(1), Some(LEVEL)));
        assert_eq!(now.vitals_data, was.vitals_data, "the recorded data stays");
        assert_eq!(now.stamina_max, was.stamina_max);
    }
}
