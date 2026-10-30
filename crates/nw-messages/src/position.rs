// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MB::PositionInTheWorldReplicatedState` on the bundle path (replication note section 2).
//! Reader `FUN_1417b4110`: a slot byte, a child bitmap, then the present children `position`
//! (`FUN_145cea190`), `rotation` (`FUN_14087b250`) and `scale` (`FUN_14087b0b0`).

use crate::wire::{MessageError, Reader, Writer};

/// Type index of the class.
pub const POSITION_IN_THE_WORLD_TYPE_INDEX: u64 = 13;
/// Height of a raw `z` of 0, in metres.
pub const POSITION_Z_MIN: f32 = -100.0;
/// Height of a raw `z` of `u16::MAX`, in metres.
pub const POSITION_Z_MAX: f32 = 2000.0;
/// Slot byte bit: slot 0 (the child bitmap) follows.
const SLOT_0: u8 = 0x01;
const CHILD_POSITION: u8 = 0x01;
const CHILD_ROTATION: u8 = 0x02;
const CHILD_SCALE: u8 = 0x04;
const KNOWN_CHILDREN: u8 = CHILD_POSITION | CHILD_ROTATION | CHILD_SCALE;
/// Rotation mask bit: `w` is negated.
const ROTATION_W_NEGATIVE: u8 = 0x40;
/// Rotation mask: the bit of component `c` (0, 1, 2) that means 0; the bit three higher means 1.
const ROTATION_ONE_SHIFT: u8 = 3;
const ROTATION_COMPONENTS: usize = 3;
/// Rotation mask bit 7 has no read in the binary and was clear on every captured member.
const ROTATION_UNKNOWN: u8 = 0x80;
/// Scale tag: four values of 1, nothing read.
const SCALE_ONE: u8 = 0;
/// Scale tag: one binary16 for every axis.
const SCALE_UNIFORM: u8 = 1;

/// The `position` child: big-endian `f32` x and y, and a big-endian `u16` height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPosition {
    /// East, in metres.
    pub x: f32,
    /// North, in metres.
    pub y: f32,
    /// Height, from [`POSITION_Z_MIN`] (0) to [`POSITION_Z_MAX`] (`u16::MAX`).
    pub z_raw: u16,
}

impl WorldPosition {
    /// A position from metres; the height is clamped to the encoded range and rounded. `None`
    /// when a value is not finite: a NaN height would become -100 m without notice.
    pub fn from_metres(x: f32, y: f32, z: f32) -> Option<Self> {
        if !(x.is_finite() && y.is_finite() && z.is_finite()) {
            return None;
        }
        Some(Self {
            x,
            y,
            z_raw: height_to_raw(z, POSITION_Z_MIN, POSITION_Z_MAX),
        })
    }

    /// The height in metres.
    pub fn z(&self) -> f32 {
        height_from_raw(self.z_raw, POSITION_Z_MIN, POSITION_Z_MAX)
    }
}

/// The raw `u16` of a finite height `z` in `[min, max]`: clamped, then rounded.
pub(crate) fn height_to_raw(z: f32, min: f32, max: f32) -> u16 {
    let share = ((z - min) / (max - min)).clamp(0.0, 1.0);
    // `share` is in [0, 1], so the product is in [0, u16::MAX].
    (share * f32::from(u16::MAX)).round() as u16
}

/// The height of a raw `u16` in `[min, max]`: `raw / 65535 * (max - min) + min`.
pub(crate) fn height_from_raw(raw: u16, min: f32, max: f32) -> f32 {
    f32::from(raw) / f32::from(u16::MAX) * (max - min) + min
}

/// One rotation component on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationComponent {
    /// The component is 0; no bytes.
    Zero,
    /// The component is 1; no bytes.
    One,
    /// A big-endian `u16` mapped to `[-1, 1]`.
    Value(u16),
}

/// The `rotation` child: x, y and z of a unit quaternion; `w` is not on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rotation {
    /// x, y and z.
    pub components: [RotationComponent; ROTATION_COMPONENTS],
    /// Whether `w` is negative.
    pub w_negative: bool,
}

/// The steps of a rotation component's `u16` over `[-1, 1]`.
const ROTATION_STEPS: f32 = 65_535.0;

impl Rotation {
    /// The wire form of the unit quaternion `quaternion` (x, y, z, w): a component of exactly 0
    /// or 1 is its own form, another is `(c + 1) / 2` of [`ROTATION_STEPS`], truncated
    /// (Observed: the two retail camps of `Hivecamp` and `Hive_camping` sent z 0.874472 as
    /// 61421 and -0.512169 as 15985, the 2236 rotations their clients asked for).
    pub fn of_quaternion(quaternion: [f32; 4]) -> Self {
        let [x, y, z, w] = quaternion;
        let component = |value: f32| {
            if value == 0.0 {
                RotationComponent::Zero
            } else if value == 1.0 {
                RotationComponent::One
            } else {
                // The share is in [0, 1], so the product is in [0, u16::MAX].
                let share = ((value + 1.0) / 2.0).clamp(0.0, 1.0);
                RotationComponent::Value((share * ROTATION_STEPS) as u16)
            }
        };
        Self {
            components: [component(x), component(y), component(z)],
            w_negative: w < 0.0,
        }
    }
}

/// The `scale` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Tag 0: scale 1, nothing read.
    One,
    /// Tag 1: one binary16 for every axis.
    Uniform(u16),
    /// Any other tag: three binary16 values.
    PerAxis {
        /// The tag byte.
        tag: u8,
        /// x, y and z as binary16.
        values: [u16; 3],
    },
}

/// The replicated position, rotation and scale of one entity.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PositionInTheWorldReplicatedState {
    /// The position, when it changed.
    pub position: Option<WorldPosition>,
    /// The rotation, when it changed.
    pub rotation: Option<Rotation>,
    /// The scale, when it changed.
    pub scale: Option<Scale>,
}

impl PositionInTheWorldReplicatedState {
    /// Reads the state from the bundle path.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the body ends early or a bit that the note does not describe
    /// is set.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let slot = reader.u8()?;
        if slot & !SLOT_0 != 0 {
            return Err(MessageError::UnknownBits {
                field: "position slot",
                bits: slot & !SLOT_0,
            });
        }
        if slot & SLOT_0 == 0 {
            return Ok(Self::default());
        }
        let children = reader.u8()?;
        if children == 0 {
            // `write` sends a state without children as slot byte 0, so this form cannot be
            // written again.
            return Err(MessageError::UnknownBits {
                field: "position children (none set)",
                bits: children,
            });
        }
        if children & !KNOWN_CHILDREN != 0 {
            return Err(MessageError::UnknownBits {
                field: "position children",
                bits: children & !KNOWN_CHILDREN,
            });
        }
        let position = if children & CHILD_POSITION != 0 {
            Some(WorldPosition {
                x: reader.f32()?,
                y: reader.f32()?,
                z_raw: reader.u16()?,
            })
        } else {
            None
        };
        let rotation = if children & CHILD_ROTATION != 0 {
            Some(read_rotation(reader)?)
        } else {
            None
        };
        let scale = if children & CHILD_SCALE != 0 {
            Some(match reader.u8()? {
                SCALE_ONE => Scale::One,
                SCALE_UNIFORM => Scale::Uniform(reader.u16()?),
                tag => Scale::PerAxis {
                    tag,
                    values: [reader.u16()?, reader.u16()?, reader.u16()?],
                },
            })
        } else {
            None
        };
        Ok(Self {
            position,
            rotation,
            scale,
        })
    }

    /// Writes the state for the bundle path. A state with no child writes only the slot byte 0.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::BadTag`] for a per-axis scale with tag 0 or 1: the client reads
    /// those tags as the other scale forms.
    pub fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        if let Some(Scale::PerAxis { tag, .. }) = self.scale
            && matches!(tag, SCALE_ONE | SCALE_UNIFORM)
        {
            return Err(MessageError::BadTag {
                field: "scale",
                tag,
            });
        }
        let children = [
            (self.position.is_some(), CHILD_POSITION),
            (self.rotation.is_some(), CHILD_ROTATION),
            (self.scale.is_some(), CHILD_SCALE),
        ]
        .iter()
        .filter(|(present, _)| *present)
        .fold(0, |bits, (_, bit)| bits | bit);
        if children == 0 {
            writer.u8(0);
            return Ok(());
        }
        writer.u8(SLOT_0).u8(children);
        if let Some(position) = self.position {
            writer.f32(position.x).f32(position.y).u16(position.z_raw);
        }
        if let Some(rotation) = self.rotation {
            write_rotation(&rotation, writer);
        }
        match self.scale {
            None => {}
            Some(Scale::One) => {
                writer.u8(SCALE_ONE);
            }
            Some(Scale::Uniform(value)) => {
                writer.u8(SCALE_UNIFORM).u16(value);
            }
            Some(Scale::PerAxis { tag, values }) => {
                writer.u8(tag);
                for value in values {
                    writer.u16(value);
                }
            }
        }
        Ok(())
    }
}

fn read_rotation(reader: &mut Reader<'_>) -> Result<Rotation, MessageError> {
    let mask = reader.u8()?;
    if mask & ROTATION_UNKNOWN != 0 {
        return Err(MessageError::UnknownBits {
            field: "rotation mask",
            bits: ROTATION_UNKNOWN,
        });
    }
    let mut components = [RotationComponent::Zero; ROTATION_COMPONENTS];
    for (index, component) in (0_u8..).zip(components.iter_mut()) {
        let zero = mask & (1 << index) != 0;
        let one = mask & (1 << (index + ROTATION_ONE_SHIFT)) != 0;
        if zero && one {
            return Err(MessageError::UnknownBits {
                field: "rotation mask",
                bits: (1 << index) | (1 << (index + ROTATION_ONE_SHIFT)),
            });
        }
        *component = if zero {
            RotationComponent::Zero
        } else if one {
            RotationComponent::One
        } else {
            RotationComponent::Value(reader.u16()?)
        };
    }
    Ok(Rotation {
        components,
        w_negative: mask & ROTATION_W_NEGATIVE != 0,
    })
}

fn write_rotation(rotation: &Rotation, writer: &mut Writer) {
    let mut mask = if rotation.w_negative {
        ROTATION_W_NEGATIVE
    } else {
        0
    };
    for (index, component) in (0_u8..).zip(rotation.components) {
        match component {
            RotationComponent::Zero => mask |= 1 << index,
            RotationComponent::One => mask |= 1 << (index + ROTATION_ONE_SHIFT),
            RotationComponent::Value(_) => {}
        }
    }
    writer.u8(mask);
    for component in rotation.components {
        if let RotationComponent::Value(value) = component {
            writer.u16(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const X: f32 = 8000.5;
    const Y: f32 = 3000.25;
    const HEIGHT: f32 = 120.0;
    const TOLERANCE_M: f32 = 0.05;

    fn full() -> PositionInTheWorldReplicatedState {
        PositionInTheWorldReplicatedState {
            position: Some(at(X, Y, HEIGHT)),
            rotation: Some(Rotation {
                components: [
                    RotationComponent::Zero,
                    RotationComponent::One,
                    RotationComponent::Value(0x1234),
                ],
                w_negative: true,
            }),
            scale: Some(Scale::PerAxis {
                tag: 2,
                values: [1, 2, 3],
            }),
        }
    }

    #[test]
    fn a_quaternion_has_the_retail_camp_rotation() {
        // `Hivecamp` 2236 at 1781367544.661 asked for (0, 0, 0x3f5fdd61, 0xbef85bf1); its camp
        // group sent z 61421 with w negative.
        let rotation = Rotation::of_quaternion([
            0.0,
            0.0,
            f32::from_bits(0x3f5f_dd61),
            f32::from_bits(0xbef8_5bf1),
        ]);
        assert_eq!(
            rotation,
            Rotation {
                components: [
                    RotationComponent::Zero,
                    RotationComponent::Zero,
                    RotationComponent::Value(61_421),
                ],
                w_negative: true,
            }
        );
        let other = Rotation::of_quaternion([0.0, 1.0, f32::from_bits(0xbf03_1d7c), 0.5]);
        assert_eq!(
            other.components,
            [
                RotationComponent::Zero,
                RotationComponent::One,
                RotationComponent::Value(15_985)
            ]
        );
        assert!(!other.w_negative);
    }

    fn encode(state: &PositionInTheWorldReplicatedState) -> Vec<u8> {
        let mut writer = Writer::new();
        state.write(&mut writer).expect("write");
        writer.finish()
    }

    fn at(x: f32, y: f32, z: f32) -> WorldPosition {
        WorldPosition::from_metres(x, y, z).expect("finite")
    }

    fn decode(bytes: &[u8]) -> Result<PositionInTheWorldReplicatedState, MessageError> {
        let mut reader = Reader::new(bytes);
        let state = PositionInTheWorldReplicatedState::read(&mut reader)?;
        reader.finish()?;
        Ok(state)
    }

    #[test]
    fn every_child_round_trips() {
        let bytes = encode(&full());
        assert_eq!(&bytes[..2], &[SLOT_0, KNOWN_CHILDREN]);
        assert_eq!(decode(&bytes), Ok(full()));
    }

    #[test]
    fn a_position_alone_is_twelve_bytes() {
        let state = PositionInTheWorldReplicatedState {
            position: Some(at(X, Y, HEIGHT)),
            ..PositionInTheWorldReplicatedState::default()
        };
        let bytes = encode(&state);
        assert_eq!(bytes.len(), 12);
        assert_eq!(decode(&bytes), Ok(state));
        let empty = encode(&PositionInTheWorldReplicatedState::default());
        assert_eq!(empty, [0]);
    }

    #[test]
    fn the_height_maps_from_minus_100_to_2000() {
        assert_eq!(at(0.0, 0.0, POSITION_Z_MIN).z_raw, 0);
        assert_eq!(at(0.0, 0.0, POSITION_Z_MAX).z_raw, u16::MAX);
        assert_eq!(at(0.0, 0.0, 9999.0).z_raw, u16::MAX);
        assert_eq!(at(0.0, 0.0, -500.0).z_raw, 0);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(WorldPosition::from_metres(bad, 0.0, 0.0), None);
            assert_eq!(WorldPosition::from_metres(0.0, 0.0, bad), None);
        }
        let back = at(X, Y, HEIGHT).z();
        assert!((back - HEIGHT).abs() < TOLERANCE_M);
    }

    #[test]
    fn unknown_bits_are_errors() {
        assert!(matches!(
            decode(&[SLOT_0 | 0x02]),
            Err(MessageError::UnknownBits { .. })
        ));
        assert!(matches!(
            decode(&[SLOT_0, 0x80]),
            Err(MessageError::UnknownBits { .. })
        ));
        assert!(matches!(
            decode(&[SLOT_0, CHILD_ROTATION, ROTATION_UNKNOWN]),
            Err(MessageError::UnknownBits { .. })
        ));
        assert!(decode(&[SLOT_0, CHILD_POSITION, 0]).is_err());
    }

    #[test]
    fn a_component_marked_zero_and_one_is_refused() {
        let zero_and_one_for_x = 0x01 | 0x08;
        assert_eq!(
            decode(&[SLOT_0, CHILD_ROTATION, zero_and_one_for_x]),
            Err(MessageError::UnknownBits {
                field: "rotation mask",
                bits: zero_and_one_for_x
            })
        );
    }

    #[test]
    fn a_slot_without_children_is_refused_because_it_cannot_be_written_again() {
        assert!(matches!(
            decode(&[SLOT_0, 0]),
            Err(MessageError::UnknownBits { .. })
        ));
    }

    #[test]
    fn a_per_axis_scale_must_not_use_the_tags_of_the_other_forms() {
        for tag in [SCALE_ONE, SCALE_UNIFORM] {
            let state = PositionInTheWorldReplicatedState {
                scale: Some(Scale::PerAxis {
                    tag,
                    values: [1, 2, 3],
                }),
                ..PositionInTheWorldReplicatedState::default()
            };
            assert_eq!(
                state.write(&mut Writer::new()),
                Err(MessageError::BadTag {
                    field: "scale",
                    tag
                })
            );
        }
    }

    proptest! {
        #[test]
        fn random_children_never_panic(
            children in any::<u8>(),
            tail in proptest::collection::vec(any::<u8>(), 0..32),
        ) {
            let mut bytes = vec![SLOT_0, children];
            bytes.extend(tail);
            let _ = decode(&bytes);
        }
    }
}
