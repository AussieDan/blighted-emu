// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changes of the faction component's state, `MB::FactionComponentReplicatedState` (3152, own
//! component 49): slot 1 children 0 `pvpFlagPending`, 1 `notifyPending`, 2
//! `pvpFlagPendingEndTime` (u64 unix ns), 3 `lastFactionChangeTimepoint` (u64 unix ns) and 4
//! `factionChangeCount` (u16); slot 2 children 0 `faction`, 1 `pvpFlag` and 2 `hasSanctuary`
//! (member layout table; one byte each for the flags).
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
/// `pvpFlagPending`, `notifyPending`, `pvpFlagPendingEndTime` (u64 unix ns),
/// `lastFactionChangeTimepoint` (u64 unix ns) and `factionChangeCount` (u16) (slot 1).
const PVP_PENDING_CHILD: u8 = 0;
const NOTIFY_PENDING_CHILD: u8 = 1;
const PVP_PENDING_END_CHILD: u8 = 2;
const CHANGED_AT_CHILD: u8 = 3;
const CHANGES_CHILD: u8 = 4;
/// `faction`, `pvpFlag` and `hasSanctuary` (slot 2).
const FACTION_CHILD: u8 = 0;
const PVP_FLAG_CHILD: u8 = 1;
const SANCTUARY_CHILD: u8 = 2;

/// A change of some children of the faction state; `None` children stay as they are.
///
/// **Observed** (`Hive_flag_pvp_on_and_off`, every 3152 of the own component): a PvP toggle on
/// sets `pvpFlagPending` and `notifyPending` (`02 03 01 01`), a toggle off clears the pending
/// flag (`02 03 00 01`); leaving a sanctuary with the flag pending sets
/// `pvpFlagPendingEndTime` and clears `hasSanctuary` (`06 04 <time> 04 00`); at the end time
/// the three pending children clear and `pvpFlag` is set (`06 07 00 00 <0> 02 01`); entering a
/// sanctuary with the flag on clears `pvpFlag` and makes it pending again (`06 01 01 06 00 01`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactionStateChange {
    /// `pvpFlagPending`: the flag waits (in a sanctuary, or for its countdown).
    pub pvp_flag_pending: Option<bool>,
    /// `notifyPending`.
    pub notify_pending: Option<bool>,
    /// `pvpFlagPendingEndTime`: unix ns when the flag goes on; 0 for none.
    pub pvp_flag_pending_end_ns: Option<u64>,
    /// `lastFactionChangeTimepoint`: unix ns.
    pub last_faction_change_ns: Option<u64>,
    /// `factionChangeCount`.
    pub faction_change_count: Option<u16>,
    /// `faction`.
    pub faction: Option<u8>,
    /// `pvpFlag`: the player is flagged for PvP.
    pub pvp_flag: Option<bool>,
    /// `hasSanctuary`: the player is in a sanctuary.
    pub has_sanctuary: Option<bool>,
}

impl FactionStateChange {
    /// The change's body bytes.
    ///
    /// # Errors
    ///
    /// Never fails today; the child writer's form keeps the signature of the other states.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let owner = [
            self.pvp_flag_pending.is_some(),
            self.notify_pending.is_some(),
            self.pvp_flag_pending_end_ns.is_some(),
            self.last_faction_change_ns.is_some(),
            self.faction_change_count.is_some(),
        ];
        let public = [
            self.faction.is_some(),
            self.pvp_flag.is_some(),
            self.has_sanctuary.is_some(),
        ];
        let mut slots = 0;
        if owner.contains(&true) {
            slots |= OWNER_SLOT;
        }
        if public.contains(&true) {
            slots |= PUBLIC_SLOT;
        }
        let mut writer = Writer::new();
        writer.u8(slots);
        let flag = |value: Option<bool>| u8::from(value.unwrap_or_default());
        if owner.contains(&true) {
            write_children(
                &mut writer,
                children_mask(&owner),
                OWNER_CHILDREN,
                |writer, child| {
                    match child {
                        PVP_PENDING_CHILD => writer.u8(flag(self.pvp_flag_pending)),
                        NOTIFY_PENDING_CHILD => writer.u8(flag(self.notify_pending)),
                        PVP_PENDING_END_CHILD => {
                            writer.u64(self.pvp_flag_pending_end_ns.unwrap_or_default())
                        }
                        CHANGED_AT_CHILD => {
                            writer.u64(self.last_faction_change_ns.unwrap_or_default())
                        }
                        CHANGES_CHILD => writer.u16(self.faction_change_count.unwrap_or_default()),
                        // The mask holds no other child.
                        _ => writer,
                    };
                    Ok::<(), MessageError>(())
                },
            )?;
        }
        if public.contains(&true) {
            write_children(
                &mut writer,
                children_mask(&public),
                PUBLIC_CHILDREN,
                |writer, child| {
                    match child {
                        FACTION_CHILD => writer.u8(self.faction.unwrap_or_default()),
                        PVP_FLAG_CHILD => writer.u8(flag(self.pvp_flag)),
                        SANCTUARY_CHILD => writer.u8(flag(self.has_sanctuary)),
                        // The mask holds no other child.
                        _ => writer,
                    };
                    Ok::<(), MessageError>(())
                },
            )?;
        }
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

/// One change of the faction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactionChange {
    /// The faction now.
    pub faction: u8,
    /// For a change from one faction to another: the change's unix ns and the changes so far.
    pub switched: Option<(u64, u16)>,
}

impl FactionChange {
    fn state(&self) -> FactionStateChange {
        FactionStateChange {
            faction: Some(self.faction),
            last_faction_change_ns: self.switched.map(|(changed_ns, _)| changed_ns),
            faction_change_count: self.switched.map(|(_, changes)| changes),
            ..FactionStateChange::default()
        }
    }

    /// The change's body bytes.
    ///
    /// # Errors
    ///
    /// As [`FactionStateChange::body`].
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        self.state().body()
    }

    /// The own member of the change.
    ///
    /// # Errors
    ///
    /// As [`FactionStateChange::body`].
    pub fn member(&self) -> Result<BundleMember, MessageError> {
        self.state().member()
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
        assert_eq!(
            member,
            FactionStateChange {
                faction: Some(3),
                ..FactionStateChange::default()
            }
            .member()
            .expect("same member")
        );
        assert_eq!(member.component, FACTION_COMPONENT);
        assert_eq!(member.state.type_index(), FACTION_TYPE_INDEX);
    }

    /// The PvP flag bodies of `Hive_flag_pvp_on_and_off` (own component 49), in ledger order.
    #[test]
    fn the_pvp_flag_changes_are_the_retail_bodies() {
        const END_NS: u64 = 0x18bd_da4c_f21c_6278;
        let change = FactionStateChange::default;
        let toggle_on = FactionStateChange {
            pvp_flag_pending: Some(true),
            notify_pending: Some(true),
            ..change()
        };
        assert_eq!(toggle_on.body().expect("on"), [0x02, 0x03, 0x01, 0x01]);
        let entered = FactionStateChange {
            has_sanctuary: Some(true),
            ..change()
        };
        assert_eq!(entered.body().expect("entered"), [0x04, 0x04, 0x01]);
        let left = FactionStateChange {
            pvp_flag_pending_end_ns: Some(END_NS),
            has_sanctuary: Some(false),
            ..change()
        };
        let mut expected = vec![0x06, 0x04];
        expected.extend(END_NS.to_be_bytes());
        expected.extend([0x04, 0x00]);
        assert_eq!(left.body().expect("left"), expected);
        let flagged = FactionStateChange {
            pvp_flag_pending: Some(false),
            notify_pending: Some(false),
            pvp_flag_pending_end_ns: Some(0),
            pvp_flag: Some(true),
            ..change()
        };
        assert_eq!(
            flagged.body().expect("flagged"),
            [0x06, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x01]
        );
        let back_in = FactionStateChange {
            pvp_flag_pending: Some(true),
            pvp_flag: Some(false),
            has_sanctuary: Some(true),
            ..change()
        };
        assert_eq!(
            back_in.body().expect("back in"),
            [0x06, 0x01, 0x01, 0x06, 0x00, 0x01]
        );
        let toggle_off = FactionStateChange {
            pvp_flag_pending: Some(false),
            notify_pending: Some(true),
            ..change()
        };
        assert_eq!(toggle_off.body().expect("off"), [0x02, 0x03, 0x00, 0x01]);
    }
}
