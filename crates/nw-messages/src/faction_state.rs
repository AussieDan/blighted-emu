// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changes of the faction component's state, `MB::FactionComponentReplicatedState` (3152, own
//! component 49): slot 2 child 0 `faction`, slot 1 children 3 `lastFactionChangeTimepoint`
//! (u64 unix ns) and 4 `factionChangeCount` (u16).
//!
//! **Observed**: the first join sets the faction alone (`Hive_join_yellow_faction_quest`, the
//! bundle after the 3161: `04 01 03`); a change from one faction to another sets the time and the
//! count with it (`Change-Faction`: `06 18 <time> 00 01 01 01`, the time 10 ms after the 3161).
//! A character with a faction logs in with its faction in the spawn body (`general-other-01`:
//! `06 41 00 00 0b 02 00 00`, faction 2). The changes go as recorded bodies
//! ([`MemberState::Recorded`]): the bundle walk passes the class by its layout.

use crate::payload::{BundleMember, MemberState};
use crate::slots::{children_mask, write_children};
use crate::wire::{MessageError, Writer};

/// `MB::FactionComponentReplicatedState`.
pub const FACTION_TYPE_INDEX: u64 = 3152;
/// The own component of the state.
pub const FACTION_COMPONENT: u64 = 49;
/// Slot byte bit of slot 1 (the owner's).
const OWNER_SLOT: u8 = 0x02;
/// Slot byte bit of slot 2 (the public one).
const PUBLIC_SLOT: u8 = 0x04;
/// Children of slot 1 and slot 2.
const OWNER_CHILDREN: u8 = 7;
const PUBLIC_CHILDREN: u8 = 5;
/// `lastFactionChangeTimepoint` and `factionChangeCount` (slot 1).
const CHANGED_AT_CHILD: usize = 3;
const CHANGES_CHILD: usize = 4;
/// `faction` (slot 2).
const FACTION_CHILD: usize = 0;

/// One change of the faction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactionChange {
    /// The faction now.
    pub faction: u8,
    /// For a change from one faction to another: the change's unix ns and the changes so far.
    pub switched: Option<(u64, u16)>,
}

impl FactionChange {
    /// The change's body bytes.
    ///
    /// # Errors
    ///
    /// Never fails today; the child writer's form keeps the signature of the other states.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        let mut owner = [false; OWNER_CHILDREN as usize];
        owner[CHANGED_AT_CHILD] = self.switched.is_some();
        owner[CHANGES_CHILD] = self.switched.is_some();
        let mut public = [false; PUBLIC_CHILDREN as usize];
        public[FACTION_CHILD] = true;
        let slots = if self.switched.is_some() {
            OWNER_SLOT | PUBLIC_SLOT
        } else {
            PUBLIC_SLOT
        };
        writer.u8(slots);
        if let Some((changed_ns, changes)) = self.switched {
            write_children(
                &mut writer,
                children_mask(&owner),
                OWNER_CHILDREN,
                |writer, child| {
                    match usize::from(child) {
                        CHANGED_AT_CHILD => writer.u64(changed_ns),
                        _ => writer.u16(changes),
                    };
                    Ok::<(), MessageError>(())
                },
            )?;
        }
        write_children(
            &mut writer,
            children_mask(&public),
            PUBLIC_CHILDREN,
            |writer, _| {
                writer.u8(self.faction);
                Ok::<(), MessageError>(())
            },
        )?;
        Ok(writer.finish())
    }

    /// The own member of the change.
    ///
    /// # Errors
    ///
    /// As [`Self::body`].
    pub fn member(&self) -> Result<BundleMember, MessageError> {
        Ok(BundleMember {
            component: FACTION_COMPONENT,
            state: MemberState::Recorded {
                class: FACTION_TYPE_INDEX,
                body: self.body()?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_and_a_switch_are_the_retail_bodies() {
        let join = FactionChange {
            faction: 3,
            switched: None,
        };
        assert_eq!(join.body().expect("join"), [0x04, 0x01, 0x03]);
        let switch = FactionChange {
            faction: 1,
            switched: Some((0x18e1_bb69_e04f_ba64, 1)),
        };
        assert_eq!(
            switch.body().expect("switch"),
            [6, 24, 24, 225, 187, 105, 224, 79, 186, 100, 0, 1, 1, 1]
        );
        let member = join.member().expect("member");
        assert_eq!(member.component, FACTION_COMPONENT);
        assert_eq!(member.state.type_index(), FACTION_TYPE_INDEX);
    }
}
