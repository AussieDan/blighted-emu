// SPDX-License-Identifier: AGPL-3.0-or-later
//! Field readers and writers of message bodies (bootstrap bodies note, "Shared readers").
//! Integers and floats are big-endian. A raw id is 16 bytes, not swapped. A prefix-coded integer
//! uses the type-index prefix code of the envelope (not LEB128).

use nw_reflect::{TypeIndexError, decode_type_index_prefix, encode_type_index_prefix};
use thiserror::Error;

pub use crate::entity_ids::{RAW_ID_LEN, entity_of_raw_id, raw_id_of_entity};

/// Bits in one byte.
const BYTE_BITS: u32 = 8;
/// Value bits of a wide prefix-coded integer with `n` extra bytes, for `n` from 0 to 8.
const WIDE_VALUE_BITS: [u32; 9] = [7, 14, 21, 28, 35, 42, 49, 56, 64];
/// The form with 8 extra bytes (first byte 0xFF), which holds every value.
const WIDE_LAST_FORM: usize = 8;

/// A body that does not match its layout.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MessageError {
    /// The body ends before a field.
    #[error("body ends early: {needed} bytes needed, {available} left")]
    Truncated {
        /// Bytes the field needs.
        needed: usize,
        /// Bytes left in the body.
        available: usize,
    },
    /// Bytes are left after the last field.
    #[error("{left} bytes left after the last field")]
    TrailingBytes {
        /// Bytes left.
        left: usize,
    },
    /// A flag byte is not 0 or 1.
    #[error("flag byte {value} is not 0 or 1")]
    NotBoolean {
        /// The byte.
        value: u8,
    },
    /// A count is above its limit.
    #[error("count {count} is above the limit {limit}")]
    CountTooLarge {
        /// The count.
        count: u64,
        /// The limit.
        limit: u64,
    },
    /// A bundle member's body does not match its class's layout.
    #[error("member of class {class:?}: {source}")]
    InMember {
        /// The member's class.
        class: nw_reflect::TypeId,
        /// What went wrong.
        source: Box<MessageError>,
    },
    /// A member body uses a slot or a child whose size the notes do not give (a slot other than
    /// 0, a child beyond the known ones, an empty last child byte); the walk cannot pass it, and
    /// a bundle reader treats it like a class without a reader.
    #[error("{field}: byte {byte:#04x} names a slot or child that is not sized")]
    Unsized {
        /// The member class.
        field: &'static str,
        /// The slot or child byte.
        byte: u8,
    },
    /// Bits that the RE notes do not describe are set.
    #[error("{field}: unknown bits {bits:#04x}")]
    UnknownBits {
        /// The field.
        field: &'static str,
        /// The unknown bits that are set.
        bits: u8,
    },
    /// A tag value that the field's form does not allow.
    #[error("{field}: tag {tag} does not fit this form")]
    BadTag {
        /// The field.
        field: &'static str,
        /// The tag.
        tag: u8,
    },
    /// A fixed field is present without its mask bit, or the other way round.
    #[error("mask bit {bit} does not match its field")]
    MaskMismatch {
        /// The mask bit.
        bit: u8,
    },
    /// The rows after mask bit 8 are not the ones that the mask, the prefix integer and the gate
    /// byte select, so a decoder would read the body differently.
    #[error("the rows do not match the mask, the prefix and the gate")]
    RowsMismatch,
    /// A list does not have the length its count or element size needs.
    #[error("{field}: the length does not match")]
    LengthMismatch {
        /// The field.
        field: &'static str,
    },
    /// A replicated state class has no reader here, and its body has no length.
    #[error("no reader for replicated state class {class:?}")]
    UnknownMemberClass {
        /// The class type id.
        class: nw_reflect::TypeId,
    },
    /// The envelope around a body cannot be built or read.
    #[error("envelope: {0}")]
    Envelope(#[from] nw_reflect::EnvelopeError),
    /// A prefix-coded integer is cut off or too large.
    #[error("prefix-coded integer: {0}")]
    Prefix(#[from] TypeIndexError),
}

/// A message body with a known layout.
pub trait BodyCodec: Sized {
    /// File slot of the message class in `typeindex.json`.
    const TYPE_INDEX: u64;

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a value does not fit its field.
    fn encode(&self) -> Result<Vec<u8>, MessageError>;

    /// The message from its body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when the bytes do not match the layout.
    fn decode(body: &[u8]) -> Result<Self, MessageError>;
}

/// Reads fields from the front of a body.
#[derive(Debug)]
pub struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader over `body`.
    pub const fn new(body: &'a [u8]) -> Self {
        Self { rest: body }
    }

    /// The next `len` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8], MessageError> {
        if self.rest.len() < len {
            return Err(MessageError::Truncated {
                needed: len,
                available: self.rest.len(),
            });
        }
        let (field, rest) = self.rest.split_at(len);
        self.rest = rest;
        Ok(field)
    }

    /// The next `N` bytes as an array.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], MessageError> {
        let Some((field, rest)) = self.rest.split_first_chunk::<N>() else {
            return Err(MessageError::Truncated {
                needed: N,
                available: self.rest.len(),
            });
        };
        self.rest = rest;
        Ok(*field)
    }

    /// One byte.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] at the end of the body.
    pub fn u8(&mut self) -> Result<u8, MessageError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    /// A flag byte that must be 0 or 1.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::NotBoolean`] for another value.
    pub fn boolean(&mut self) -> Result<bool, MessageError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(MessageError::NotBoolean { value }),
        }
    }

    /// A big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn u16(&mut self) -> Result<u16, MessageError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// A big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn u32(&mut self) -> Result<u32, MessageError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// A big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn u64(&mut self) -> Result<u64, MessageError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// A big-endian `f32`.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn f32(&mut self) -> Result<f32, MessageError> {
        Ok(f32::from_be_bytes(self.array()?))
    }

    /// A 16-byte raw id.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when fewer bytes are left.
    pub fn raw_id(&mut self) -> Result<[u8; RAW_ID_LEN], MessageError> {
        self.array()
    }

    /// A wide prefix-coded integer (`FUN_14087b770`, movement message note section 1): the
    /// count of leading 1 bits of the first byte is the count of extra bytes (0 to 8). The extra
    /// bytes are little-endian, above the low bits of the first byte that are left.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Truncated`] when the integer is cut off.
    pub fn wide_prefixed(&mut self) -> Result<u64, MessageError> {
        let first = self.u8()?;
        let extra = first.leading_ones();
        let low_bits = WIDE_VALUE_BITS[extra as usize] - extra * BYTE_BITS;
        let mut high = 0_u64;
        for (index, byte) in self.bytes(extra as usize)?.iter().enumerate() {
            // `index` is below 8.
            high |= u64::from(*byte) << (BYTE_BITS * index as u32);
        }
        let low = u64::from(first) & ((1_u64 << low_bits) - 1);
        // `low_bits` is at most 7.
        Ok((high << low_bits) | low)
    }

    /// A prefix-coded integer.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Prefix`] when the prefix is cut off.
    pub fn prefixed(&mut self) -> Result<u64, MessageError> {
        let prefix = decode_type_index_prefix(self.rest)?;
        self.bytes(prefix.len)?;
        Ok(prefix.index)
    }

    /// Whether no bytes are left.
    pub const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// Bytes left.
    pub const fn remaining(&self) -> usize {
        self.rest.len()
    }

    /// The bytes left, without reading them.
    pub const fn rest(&self) -> &'a [u8] {
        self.rest
    }

    /// Ends the body.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::TrailingBytes`] when bytes are left.
    pub fn finish(self) -> Result<(), MessageError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(MessageError::TrailingBytes {
                left: self.rest.len(),
            })
        }
    }
}

/// Writes fields of a body in order.
#[derive(Debug, Default)]
pub struct Writer {
    body: Vec<u8>,
}

impl Writer {
    /// An empty body.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds raw bytes.
    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.body.extend_from_slice(bytes);
        self
    }

    /// Adds one byte.
    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.bytes(&[value])
    }

    /// Adds a flag byte, 0 or 1.
    pub fn boolean(&mut self, value: bool) -> &mut Self {
        self.u8(u8::from(value))
    }

    /// Adds a big-endian `u16`.
    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// Adds a big-endian `u32`.
    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// Adds a big-endian `u64`.
    pub fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// Adds a big-endian `f32`.
    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// Adds the one-byte count of a list of `len` elements.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] when `len` is above `u8::MAX`.
    pub fn u8_count(&mut self, len: usize) -> Result<&mut Self, MessageError> {
        let count = u8::try_from(len).map_err(|_| MessageError::CountTooLarge {
            count: u64::try_from(len).unwrap_or(u64::MAX),
            limit: u64::from(u8::MAX),
        })?;
        Ok(self.u8(count))
    }

    /// Adds a prefix-coded integer in its shortest form.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::Prefix`] when `value` does not fit the prefix code.
    pub fn prefixed(&mut self, value: u64) -> Result<&mut Self, MessageError> {
        let prefix = encode_type_index_prefix(value)?;
        Ok(self.bytes(&prefix))
    }

    /// Adds a wide prefix-coded integer in its shortest form (see [`Reader::wide_prefixed`]).
    pub fn wide_prefixed(&mut self, value: u64) -> &mut Self {
        // Below the last form, a value fits the first form with room for it; else it needs
        // all 8 extra bytes.
        let extra = WIDE_VALUE_BITS[..WIDE_LAST_FORM]
            .iter()
            .position(|bits| value >> bits == 0)
            .unwrap_or(WIDE_LAST_FORM);
        // `extra` is at most 8.
        let extra_bits = extra as u32 * BYTE_BITS;
        let low_bits = WIDE_VALUE_BITS[extra] - extra_bits;
        let tag = u8::MAX.checked_shl(BYTE_BITS - extra as u32).unwrap_or(0);
        // The low bits fit below the tag bits.
        let first = tag | (value & ((1_u64 << low_bits) - 1)) as u8;
        let high = value.checked_shr(low_bits).unwrap_or(0).to_le_bytes();
        self.u8(first).bytes(&high[..extra])
    }

    /// The finished body.
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIXED_VALUES: [u64; 4] = [0, 127, 128, 12_702];

    /// The first value of each wide form: 1, 2, 3, 4, 5, 6, 7 and 8 extra bytes.
    const WIDE_FORM_STARTS: [u64; 8] = [
        1 << 7,
        1 << 14,
        1 << 21,
        1 << 28,
        1 << 35,
        1 << 42,
        1 << 49,
        1 << 56,
    ];

    #[test]
    fn wide_prefixed_integers_use_up_to_nine_bytes() {
        for (extra, start) in WIDE_FORM_STARTS.into_iter().enumerate() {
            for value in [start - 1, start] {
                let body = Writer::new().wide_prefixed(value).finish();
                let expected_len = if value == start { extra + 2 } else { extra + 1 };
                assert_eq!(body.len(), expected_len, "{value:#x}");
                let mut reader = Reader::new(&body);
                assert_eq!(reader.wide_prefixed(), Ok(value));
                assert!(reader.is_empty());
            }
        }
        let largest = Writer::new().wide_prefixed(u64::MAX).finish();
        assert_eq!(largest[0], u8::MAX);
        assert_eq!(Reader::new(&largest).wide_prefixed(), Ok(u64::MAX));
    }

    #[test]
    fn a_wide_form_below_five_bytes_matches_the_narrow_form() {
        for value in [0, 127, 128, 12_702, (1 << 35) - 1] {
            let wide = Writer::new().wide_prefixed(value).finish();
            let narrow = Writer::new().prefixed(value).expect("narrow").finish();
            assert_eq!(wide, narrow);
        }
    }

    #[test]
    fn a_short_wide_form_is_an_error() {
        let body = Writer::new().wide_prefixed(u64::MAX).finish();
        assert!(matches!(
            Reader::new(&body[..body.len() - 1]).wide_prefixed(),
            Err(MessageError::Truncated { .. })
        ));
    }
    const FLOAT: f32 = 6.0;

    #[test]
    fn fields_are_big_endian_and_round_trip() {
        let id = [7; RAW_ID_LEN];
        let mut writer = Writer::new();
        writer
            .u8(1)
            .boolean(true)
            .u16(0x0102)
            .u32(0x0304_0506)
            .u64(0x0708_090a_0b0c_0d0e)
            .f32(FLOAT)
            .bytes(&id);
        let body = writer.finish();
        assert_eq!(&body[2..4], &[1, 2]);
        assert_eq!(&body[4..8], &[3, 4, 5, 6]);
        let mut reader = Reader::new(&body);
        assert_eq!(reader.u8(), Ok(1));
        assert_eq!(reader.boolean(), Ok(true));
        assert_eq!(reader.u16(), Ok(0x0102));
        assert_eq!(reader.u32(), Ok(0x0304_0506));
        assert_eq!(reader.u64(), Ok(0x0708_090a_0b0c_0d0e));
        assert_eq!(reader.f32(), Ok(FLOAT));
        assert_eq!(reader.raw_id(), Ok(id));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn prefixed_integers_use_the_type_index_code() {
        for value in PREFIXED_VALUES {
            let mut writer = Writer::new();
            writer.prefixed(value).expect("fits");
            let body = writer.finish();
            assert_eq!(body, encode_type_index_prefix(value).expect("fits"));
            let mut reader = Reader::new(&body);
            assert_eq!(reader.prefixed(), Ok(value));
            assert_eq!(reader.finish(), Ok(()));
        }
    }

    #[test]
    fn a_one_byte_count_fits_up_to_its_limit() {
        let largest = usize::from(u8::MAX);
        assert_eq!(
            Writer::new().u8_count(largest).expect("fits").finish(),
            [u8::MAX]
        );
        assert_eq!(
            Writer::new().u8_count(largest + 1).map(|_| ()),
            Err(MessageError::CountTooLarge {
                count: u64::from(u8::MAX) + 1,
                limit: u64::from(u8::MAX),
            })
        );
    }

    #[test]
    fn a_value_beyond_the_prefix_code_is_an_error() {
        assert!(matches!(
            Writer::new().prefixed(u64::MAX),
            Err(MessageError::Prefix(_))
        ));
    }

    #[test]
    fn short_bodies_flags_and_leftovers_are_errors() {
        assert_eq!(
            Reader::new(&[1, 2]).u32(),
            Err(MessageError::Truncated {
                needed: 4,
                available: 2
            })
        );
        assert_eq!(
            Reader::new(&[2]).boolean(),
            Err(MessageError::NotBoolean { value: 2 })
        );
        assert_eq!(
            Reader::new(&[0]).finish(),
            Err(MessageError::TrailingBytes { left: 1 })
        );
    }
}
