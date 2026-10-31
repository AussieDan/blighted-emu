// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player's quick course state, `MB::PlayerQuickCourseComponentReplicatedState` (6044, own
//! component 38): slot 0 child 0 `activeRaceSection`, a list keyed by the CRC-32 of the
//! lowercase `QuickCourseId` (u32) with the index of the next section to reach (u8).
//!
//! **Observed** (retail `ef_tour_0-Hermit-s-Rally`, quick course note 2026-10-30): the course
//! starts with `{ crc("ef_tour_0"): 0 }`, each node hit raises the value by one, and at the end
//! the list is empty, 2 ms after the `OnQuickCourseEnded` (6065). The state goes as a recorded
//! body ([`MemberState::Recorded`]): the bundle walk passes the class by its layout.

use crate::items::FullList;
use crate::payload::{BundleMember, MemberState};
use crate::slots::{SLOT_0, children_mask, write_children};
use crate::wire::{MessageError, Writer};

/// `MB::PlayerQuickCourseComponentReplicatedState`.
pub const QUICK_COURSE_STATE_TYPE_INDEX: u64 = 6044;
/// The own component of the state.
pub const QUICK_COURSE_STATE_COMPONENT: u64 = 38;
/// The children of its slot 0.
const CHILDREN: u8 = 1;

/// The state: the active courses, each with its next section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickCourseState {
    /// `activeRaceSection`: (course CRC-32, next section).
    pub active: FullList<(u32, u8)>,
}

impl QuickCourseState {
    /// The state's body bytes.
    ///
    /// # Errors
    ///
    /// Never fails today; the list writer's form keeps the signature of the other states.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        writer.u8(SLOT_0);
        write_children(
            &mut writer,
            children_mask(&[true]),
            CHILDREN,
            |writer, _| {
                self.active.write(writer, |writer, (course, next)| {
                    writer.u32(*course).u8(*next);
                    Ok(())
                })
            },
        )?;
        Ok(writer.finish())
    }

    /// The own member of the state.
    ///
    /// # Errors
    ///
    /// As [`Self::body`].
    pub fn member(&self) -> Result<BundleMember, MessageError> {
        Ok(BundleMember {
            component: QUICK_COURSE_STATE_COMPONENT,
            state: MemberState::Recorded {
                class: QUICK_COURSE_STATE_TYPE_INDEX,
                body: self.body()?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_started_and_an_ended_course_are_the_retail_forms() {
        let started = QuickCourseState {
            active: FullList {
                version: Some(1),
                elements: vec![(0xa85b_d2cb, 0)],
            },
        };
        assert_eq!(
            started.body().expect("body"),
            [
                0x01, 0x01, 0x00, 0x01, 0x01, 0x01, 0xa8, 0x5b, 0xd2, 0xcb, 0x00
            ]
        );
        let ended = QuickCourseState {
            active: FullList {
                version: Some(2),
                elements: Vec::new(),
            },
        };
        assert_eq!(
            ended.body().expect("body"),
            [0x01, 0x01, 0x00, 0x01, 0x02, 0x00]
        );
        let member = ended.member().expect("member");
        assert_eq!(member.component, QUICK_COURSE_STATE_COMPONENT);
    }
}
