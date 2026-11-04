// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store table ([`nw_protocol::store_api`]) from the game data: every enabled entitlement of
//! `javelindata_entitlements` with `RewardType` `Mount` and `SourceType` `Store` (52 in build
//! 25810808; its `Reward(s)` is the `MountId`), and the entitlements they require
//! (`RequiredEntitlements`: `Ent_Xpack_RiseOfTheAngryEarth_Owner` for 50 of them), which every
//! account holds on this server (Inferred: every retail player who rode a mount owned the
//! expansion).

use std::collections::BTreeMap;

use nw_protocol::store_api::{StoreEntitlement, StoreMount, StoreTable};

use crate::bundle::english;
use crate::generated::{EntitlementData, Tables};

/// `RewardType` of a mount entitlement.
const MOUNT_REWARD: &str = "Mount";
/// `SourceType` of a store entitlement.
const STORE_SOURCE: &str = "Store";
/// Between the entitlements of a `RequiredEntitlements` cell (Inferred: every cell of build
/// 25810808 holds one).
const REQUIRED_SEPARATOR: char = ',';

/// The store table of `tables`, with the English names of `text`.
pub fn store_table(tables: &Tables, text: &BTreeMap<String, String>) -> StoreTable {
    let rows: Vec<&EntitlementData> = tables
        .entitlement_data
        .iter()
        .flat_map(|table| &table.rows)
        .collect();
    let entitlement = |row: &EntitlementData| StoreEntitlement {
        entitlement: row.unique_tag_id.clone(),
        name: row
            .display_name
            .as_deref()
            .and_then(|key| english(text, key))
            .unwrap_or(&row.unique_tag_id)
            .to_owned(),
    };
    let sold: Vec<&EntitlementData> = rows
        .iter()
        .copied()
        .filter(|row| {
            row.is_enabled
                && row.reward_type.as_deref() == Some(MOUNT_REWARD)
                && row.source_type.as_deref() == Some(STORE_SOURCE)
        })
        .collect();
    let mut required: Vec<&str> = sold
        .iter()
        .filter_map(|row| row.required_entitlements.as_deref())
        .flat_map(|cell| cell.split(REQUIRED_SEPARATOR))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    required.sort_unstable();
    required.dedup();
    StoreTable {
        mounts: sold
            .iter()
            .filter_map(|row| {
                let mount = row.reward_s.as_deref().filter(|id| !id.is_empty())?;
                Some(StoreMount {
                    unlock: entitlement(row),
                    mount: mount.to_owned(),
                })
            })
            .collect(),
        held_by_all: required
            .into_iter()
            .map(|id| {
                rows.iter().find(|row| row.unique_tag_id == id).map_or_else(
                    || StoreEntitlement {
                        entitlement: id.to_owned(),
                        name: id.to_owned(),
                    },
                    |row| entitlement(row),
                )
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Table;

    fn row(
        id: &str,
        reward_type: &str,
        source: &str,
        reward: &str,
        required: &str,
    ) -> EntitlementData {
        EntitlementData {
            achievement_id: None,
            description: None,
            display_name: Some(format!("@{id}_name")),
            entitlement_index: 1,
            entitlement_info: None,
            icon: None,
            is_consumable: None,
            is_enabled: true,
            notes: None,
            objective_id: None,
            required_entitlements: Some(required.to_owned()),
            reward_s: Some(reward.to_owned()),
            reward_type: Some(reward_type.to_owned()),
            source_type: Some(source.to_owned()),
            unique_tag_id: id.to_owned(),
        }
    }

    #[test]
    fn the_store_sells_each_enabled_store_mount_and_everyone_holds_what_they_require() {
        const XPACK: &str = "Ent_Xpack_RiseOfTheAngryEarth_Owner";
        let mut disabled = row("Ent_Mount_Off", "Mount", "Store", "Mount_Off", "");
        disabled.is_enabled = false;
        let mut tables = Tables::default();
        tables.entitlement_data.push(Table {
            source_path: "entitlements".to_owned(),
            unique_id: "entitlements".to_owned(),
            rows: vec![
                row(
                    "Ent_Mount_MTX_7_bear_Ronin",
                    "Mount",
                    "Store",
                    "Mount_MTX_7_bear_Ronin",
                    XPACK,
                ),
                row(
                    "Ent_Mount_MTX_1_wolf",
                    "Mount",
                    "Store",
                    "Mount_MTX_1_wolf",
                    XPACK,
                ),
                row(
                    "Ent_Mount_Preorder",
                    "Mount",
                    "Preorder",
                    "Mount_Preorder",
                    "",
                ),
                row("Ent_Skin", "Skin", "Store", "Skin_1", ""),
                disabled,
                row(XPACK, "Expansion2023", "", "", ""),
            ],
        });
        let text = BTreeMap::from([
            (
                "Ent_Mount_MTX_7_bear_Ronin_name".to_owned(),
                "Ronin Bear Mount".to_owned(),
            ),
            (
                format!("{}_name", XPACK.to_lowercase()),
                "Rise of the Angry Earth".to_owned(),
            ),
        ]);
        let table = store_table(&tables, &text);
        let sold: Vec<(&str, &str, &str)> = table
            .mounts
            .iter()
            .map(|m| {
                (
                    m.unlock.entitlement.as_str(),
                    m.unlock.name.as_str(),
                    m.mount.as_str(),
                )
            })
            .collect();
        assert_eq!(
            sold,
            [
                (
                    "Ent_Mount_MTX_7_bear_Ronin",
                    "Ronin Bear Mount",
                    "Mount_MTX_7_bear_Ronin"
                ),
                (
                    "Ent_Mount_MTX_1_wolf",
                    "Ent_Mount_MTX_1_wolf",
                    "Mount_MTX_1_wolf"
                ),
            ]
        );
        assert_eq!(
            table.held_by_all,
            [StoreEntitlement {
                entitlement: XPACK.to_owned(),
                name: "Rise of the Angry Earth".to_owned(),
            }]
        );
    }
}
