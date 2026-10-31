// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crafting component's state, `MB::CraftingComponentReplicatedState` (205, own component
//! 63): child 0 `m_cooldowns`, the recipe cooldowns (`nw_content::recipe_cooldowns` has what the
//! values mean), and child 1 `m_craftGSBonuses` (retail: keys 0 and 3, or 0, 1, 2 and 6, each 5;
//! what they are is Unknown, so this server does not send it).
//!
//! **Observed** (retail ledgers `refining_all_stations_single_batch_territorylvlup`,
//! `FFA_PurchaseLimits` and `well-of-fortune`): an `m_cooldowns` entry is a u32 key, a u8 count
//! and a u64 end (unix ns), in the list form of [`crate::items::FullList`]. The state goes as
//! a recorded body ([`MemberState::Recorded`]): the bundle walk passes the class by its layout.

use crate::items::FullList;
use crate::payload::{BundleMember, MemberState};
use crate::slots::{SLOT_0, children_mask, write_children};
use crate::wire::{MessageError, Writer};

/// `MB::CraftingComponentReplicatedState`.
pub const CRAFTING_STATE_TYPE_INDEX: u64 = 205;
/// The own component of the state (all 85 retail states).
pub const CRAFTING_STATE_COMPONENT: u64 = 63;
/// The children of its slot 0.
const CRAFTING_CHILDREN: u8 = 2;
/// `m_cooldowns`.
const COOLDOWNS_CHILD: u8 = 0;

/// One `m_cooldowns` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraftingCooldown {
    /// CRC-32 of the lowercase `RecipeID`.
    pub key: u32,
    /// The crafts made.
    pub count: u8,
    /// The end, unix ns.
    pub ends_ns: u64,
}

/// The state with its `m_cooldowns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftingState {
    /// The recipe cooldowns.
    pub cooldowns: FullList<CraftingCooldown>,
}

impl CraftingState {
    /// The state's body bytes.
    ///
    /// # Errors
    ///
    /// Never fails today; the list writer's form keeps the signature of the other states.
    pub fn body(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        let mut present = [false; CRAFTING_CHILDREN as usize];
        present[usize::from(COOLDOWNS_CHILD)] = true;
        writer.u8(SLOT_0);
        write_children(
            &mut writer,
            children_mask(&present),
            CRAFTING_CHILDREN,
            |writer, _| {
                self.cooldowns.write(writer, |writer, cooldown| {
                    writer
                        .u32(cooldown.key)
                        .u8(cooldown.count)
                        .u64(cooldown.ends_ns);
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
            component: CRAFTING_STATE_COMPONENT,
            state: MemberState::Recorded {
                class: CRAFTING_STATE_TYPE_INDEX,
                body: self.body()?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_is_the_retail_full_form_of_the_refining_ledger() {
        // Retail body at 1781363699027 (refining ledger), child 0 only: version 2,307
        // (`83 24` wide), 3 entries (`IngotT53`, `TimberT51`, `LeatherT51`).
        let state = CraftingState {
            cooldowns: FullList {
                version: Some(2_307),
                elements: vec![
                    CraftingCooldown {
                        key: 0xd5d8_d8d8,
                        count: 1,
                        ends_ns: 0x18b8_d351_ab64_9974,
                    },
                    CraftingCooldown {
                        key: 0x1b4c_2cb9,
                        count: 9,
                        ends_ns: 0x18b8_d351_b519_cf84,
                    },
                    CraftingCooldown {
                        key: 0xee97_0363,
                        count: 10,
                        ends_ns: 0x18b8_d351_b144_f000,
                    },
                ],
            },
        };
        let retail: [u8; 46] = [
            1, 1, 0, 1, 131, 36, 3, 213, 216, 216, 216, 1, 24, 184, 211, 81, 171, 100, 153, 116,
            27, 76, 44, 185, 9, 24, 184, 211, 81, 181, 25, 207, 132, 238, 151, 3, 99, 10, 24, 184,
            211, 81, 177, 68, 240, 0,
        ];
        assert_eq!(state.body().expect("body"), retail);
        let member = state.member().expect("member");
        assert_eq!(member.component, CRAFTING_STATE_COMPONENT);
        assert_eq!(member.state.type_index(), CRAFTING_STATE_TYPE_INDEX);
    }
}
