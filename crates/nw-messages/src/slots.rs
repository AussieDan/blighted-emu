// SPDX-License-Identifier: AGPL-3.0-or-later
//! The slot and child bytes of a bundle member body (mob-spawn note section 1): the reader
//! `FUN_1417b4110` reads one byte per group of up to 8 slots, and `FUN_1417b43c0` one byte per
//! group of up to 7 children of a slot, where bit 7 means that another child byte follows.
//!
//! A slot's child bytes are not all up front: each child byte is followed by the data of the
//! children it sets, then the next child byte (player-appearance note, section 1).
//!
//! These members use slot 0 only, with the children the notes size. A slot other than 0, a
//! child beyond those, or a last child byte that sets no child is [`MessageError::Unsized`]: the
//! captures have such bodies (slot 1 of the vitals: mana and the afflictions), and a bundle
//! walk stops there. A slot without children is refused. So every body that reads encodes
//! back to the same bytes. A child byte before the last may set none: that is how only a later
//! child is sent.

use std::convert::Infallible;

use crate::wire::{MessageError, Reader, Writer};

/// Slot byte bit: slot 0 follows.
pub(crate) const SLOT_0: u8 = 0x01;
/// Child byte bit: another child byte follows.
pub(crate) const CONTINUE: u8 = 0x80;
/// Children in one child byte.
const CHILDREN_PER_BYTE: u8 = 7;
/// The child bits of one child byte.
const CHILD_BITS: u8 = 0x7f;

/// Reads the slot byte of a class with `slots` slots: bit `n` is slot `n`, and the slots that are
/// set follow in order (`FUN_1417b4110` tests only the bits of the slots that exist).
pub(crate) fn read_slot_byte(
    reader: &mut Reader<'_>,
    field: &'static str,
    slots: u8,
) -> Result<u8, MessageError> {
    let byte = reader.u8()?;
    if u32::from(byte) >> slots != 0 {
        return Err(MessageError::Unsized { field, byte });
    }
    Ok(byte)
}

/// Reads one slot of `count` children: each child byte, then the data of the children it sets,
/// which `read_child` reads in child order (player-appearance note, section 1). Returns the child
/// bitmap (bit `n` is child `n`); at least one child is set.
pub(crate) fn read_children(
    reader: &mut Reader<'_>,
    field: &'static str,
    count: u8,
    read_child: impl FnMut(&mut Reader<'_>, u8) -> Result<(), MessageError>,
) -> Result<u16, MessageError> {
    let children = read_wide_children(reader, field, count, read_child)?;
    u16::try_from(children).map_err(|_| MessageError::Unsized {
        field,
        // A child above bit 15: the class has more children than this reader keeps.
        byte: CONTINUE,
    })
}

/// [`read_children`] for a slot with more than 16 children (up to 32): the bitmap as a `u32`.
pub(crate) fn read_wide_children(
    reader: &mut Reader<'_>,
    field: &'static str,
    count: u8,
    read_child: impl FnMut(&mut Reader<'_>, u8) -> Result<(), MessageError>,
) -> Result<u32, MessageError> {
    let children = read_children_64(reader, field, count, read_child)?;
    u32::try_from(children).map_err(|_| MessageError::Unsized {
        field,
        // A child above bit 31: the class has more children than this reader keeps.
        byte: CONTINUE,
    })
}

/// [`read_children`] for a slot with up to 64 children: the bitmap as a `u64`.
pub(crate) fn read_children_64(
    reader: &mut Reader<'_>,
    field: &'static str,
    count: u8,
    mut read_child: impl FnMut(&mut Reader<'_>, u8) -> Result<(), MessageError>,
) -> Result<u64, MessageError> {
    let mut children: u64 = 0;
    let mut first: u8 = 0;
    loop {
        let byte = reader.u8()?;
        let bits = byte & CHILD_BITS;
        let last = byte & CONTINUE == 0;
        let in_group = count.saturating_sub(first).min(CHILDREN_PER_BYTE);
        let known: u8 = if in_group >= CHILDREN_PER_BYTE {
            CHILD_BITS
        } else {
            (1_u8 << in_group) - 1
        };
        let more_allowed = count > first + CHILDREN_PER_BYTE;
        if first == 0 && bits == 0 && last {
            return Err(MessageError::UnknownBits { field, bits: byte });
        }
        if bits & !known != 0 || (bits == 0 && last) || (!last && !more_allowed) {
            return Err(MessageError::Unsized { field, byte });
        }
        for child in (0..CHILDREN_PER_BYTE).filter(|bit| bits & 1 << bit != 0) {
            read_child(reader, first + child)?;
        }
        children |= u64::from(bits) << first;
        if last {
            return Ok(children);
        }
        first += CHILDREN_PER_BYTE;
    }
}

/// Writes one slot of `count` children with the bitmap `children` (not 0): each child byte, then
/// the data of the children it sets, which `write_child` writes in child order.
pub(crate) fn write_children<E>(
    writer: &mut Writer,
    children: u16,
    count: u8,
    write_child: impl FnMut(&mut Writer, u8) -> Result<(), E>,
) -> Result<(), E> {
    write_wide_children(writer, u32::from(children), count, write_child)
}

/// [`write_children`] for a slot with more than 16 children (up to 32).
pub(crate) fn write_wide_children<E>(
    writer: &mut Writer,
    children: u32,
    count: u8,
    write_child: impl FnMut(&mut Writer, u8) -> Result<(), E>,
) -> Result<(), E> {
    write_children_64(writer, u64::from(children), count, write_child)
}

/// [`write_children`] for a slot with up to 64 children.
pub(crate) fn write_children_64<E>(
    writer: &mut Writer,
    children: u64,
    count: u8,
    mut write_child: impl FnMut(&mut Writer, u8) -> Result<(), E>,
) -> Result<(), E> {
    let mut first: u8 = 0;
    while first < count {
        // A child byte holds 7 children: the shift leaves at most 7 bits.
        let bits = (children >> first) as u8 & CHILD_BITS;
        let rest = children
            .checked_shr(u32::from(first + CHILDREN_PER_BYTE))
            .unwrap_or(0);
        writer.u8(if rest == 0 { bits } else { bits | CONTINUE });
        for child in (0..CHILDREN_PER_BYTE).filter(|bit| bits & 1 << bit != 0) {
            write_child(writer, first + child)?;
        }
        if rest == 0 {
            break;
        }
        first += CHILDREN_PER_BYTE;
    }
    Ok(())
}

/// The child bitmap of the present children; `present[n]` is child `n`.
pub(crate) fn children_mask(present: &[bool]) -> u16 {
    present
        .iter()
        .enumerate()
        .filter(|(_, present)| **present)
        .fold(0, |bits, (child, _)| bits | 1 << child)
}

/// Reads the slot byte of a class with slot 0 only and, when slot 0 is set, its children (see
/// [`read_children`]). `None` when slot 0 is not set.
pub(crate) fn read_slot_0(
    reader: &mut Reader<'_>,
    field: &'static str,
    count: u8,
    read_child: impl FnMut(&mut Reader<'_>, u8) -> Result<(), MessageError>,
) -> Result<Option<u16>, MessageError> {
    if read_slot_byte(reader, field, 1)? == 0 {
        return Ok(None);
    }
    read_children(reader, field, count, read_child).map(Some)
}

/// [`write_children`] with a child writer that cannot fail.
pub(crate) fn write_known_children(
    writer: &mut Writer,
    children: u16,
    count: u8,
    write_child: impl FnMut(&mut Writer, u8),
) {
    write_known_wide_children(writer, u32::from(children), count, write_child);
}

/// [`write_wide_children`] with a child writer that cannot fail.
pub(crate) fn write_known_wide_children(
    writer: &mut Writer,
    children: u32,
    count: u8,
    mut write_child: impl FnMut(&mut Writer, u8),
) {
    write_wide_children::<Infallible>(writer, children, count, |writer, child| {
        write_child(writer, child);
        Ok(())
    })
    .unwrap_or_else(|never| match never {});
}

/// Writes the slot byte and, when `children` sets a child, slot 0 (see [`write_children`]), with
/// a child writer that cannot fail.
pub(crate) fn write_known(
    writer: &mut Writer,
    children: u16,
    count: u8,
    write_child: impl FnMut(&mut Writer, u8),
) {
    if children == 0 {
        writer.u8(0);
        return;
    }
    writer.u8(SLOT_0);
    write_known_children(writer, children, count, write_child);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a slot whose children carry one byte each: the child index.
    fn round(children: u16, count: u8) -> Vec<u8> {
        let mut writer = Writer::new();
        write_known(&mut writer, children, count, |writer, child| {
            writer.u8(child);
        });
        writer.finish()
    }

    /// Reads a body written by [`round`]: the bitmap and the children in read order.
    fn read(bytes: &[u8], count: u8) -> Result<(Option<u16>, Vec<u8>), MessageError> {
        let mut reader = Reader::new(bytes);
        let mut seen = Vec::new();
        let children = read_slot_0(&mut reader, "test", count, |reader, child| {
            assert_eq!(
                reader.u8()?,
                child,
                "each child's data follows its own child byte"
            );
            seen.push(child);
            Ok(())
        })?;
        reader.finish()?;
        Ok((children, seen))
    }

    #[test]
    fn no_slot_is_one_zero_byte() {
        assert_eq!(round(0, 2), vec![0]);
        assert_eq!(read(&[0], 2).expect("read"), (None, Vec::new()));
    }

    #[test]
    fn children_up_to_seven_take_one_byte() {
        let bytes = round(0b11, 2);
        assert_eq!(bytes, vec![SLOT_0, 0b11, 0, 1]);
        assert_eq!(read(&bytes, 2).expect("read"), (Some(0b11), vec![0, 1]));
    }

    #[test]
    fn the_eighth_child_byte_comes_after_the_first_seven_children() {
        let bytes = round(1 << 7 | 1, 8);
        assert_eq!(bytes, vec![SLOT_0, 1 | CONTINUE, 0, 1, 7]);
        assert_eq!(
            read(&bytes, 8).expect("read"),
            (Some(1 << 7 | 1), vec![0, 7])
        );
    }

    #[test]
    fn only_the_eighth_child_is_an_empty_first_byte_then_one() {
        let bytes = round(1 << 7, 8);
        assert_eq!(bytes, vec![SLOT_0, CONTINUE, 1, 7]);
        assert_eq!(read(&bytes, 8).expect("read"), (Some(1 << 7), vec![7]));
    }

    #[test]
    fn unknown_slots_children_and_empty_forms_are_refused() {
        let cases: [(&[u8], u8); 6] = [
            (&[0b10], 2),
            (&[SLOT_0, 0], 2),
            (&[SLOT_0, 0b100, 2], 2),
            (&[SLOT_0, 1 | CONTINUE, 0, 1], 2),
            (&[SLOT_0, CONTINUE, 0], 8),
            (&[SLOT_0, CONTINUE, 0b10, 8], 8),
        ];
        for (bytes, count) in cases {
            assert!(read(bytes, count).is_err(), "{bytes:?} with {count}");
        }
    }
}
