// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store table ([`nw_protocol::store_api`]) from the game data: every enabled entitlement of
//! `javelindata_entitlements` with `RewardType` `Mount` and `SourceType` `Store` (52 in build
//! 25810808; its `Reward(s)` is the `MountId`) that has a store product, and the entitlements they
//! require (`RequiredEntitlements`: `Ent_Xpack_RiseOfTheAngryEarth_Owner` for 50 of them), which
//! every account holds on this server (Inferred: every retail player who rode a mount owned the
//! expansion).
//!
//! A mount's store product is the `javelindata_storeproducts` row with the mount's `DisplayName`
//! (Observed: 31 of 31 retail mount offers, `unlock_store_content` capture: `Mount_Wolf_1` for
//! `Ent_Mount_MTX_1_wolf`). Two rows with that name: the one whose `UniqueTagID` holds the mount's
//! variant, the last part of its `MountId` (`Mount_Lion_Reaper` for `Mount_MTX_8_lion_Reaper`).
//! A mount without a product is not sold: the client leaves out an offer whose alias names no
//! product (`entitlementsdatahandler.luac`: "No StoreProductData in JavelinData_StoreProducts.csv
//! found"). Build 25810808 sells 47 of the 52.

use std::collections::{BTreeMap, HashMap};

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
/// Between the parts of a `MountId`; the last part is the mount's variant.
const MOUNT_ID_SEPARATOR: char = '_';

/// The store products by lowercase `DisplayName`: the `UniqueTagID` of each.
#[derive(Debug, Default)]
pub struct StoreProducts {
    by_name: HashMap<String, Vec<String>>,
}

impl StoreProducts {
    /// The products of `products`: (`UniqueTagID`, `DisplayName`).
    pub fn build<'a>(products: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        for (product, name) in products {
            by_name
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(product.to_owned());
        }
        Self { by_name }
    }

    /// The product of mount `mount` named `name`; `None` without one, or with two that its
    /// variant does not tell apart.
    fn of(&self, mount: &str, name: &str) -> Option<&str> {
        let named = self.by_name.get(&name.to_ascii_lowercase())?;
        if let [product] = named.as_slice() {
            return Some(product);
        }
        let variant = mount
            .rsplit(MOUNT_ID_SEPARATOR)
            .next()?
            .to_ascii_lowercase();
        let mut matching = named
            .iter()
            .filter(|product| product.to_ascii_lowercase().contains(&variant));
        match (matching.next(), matching.next()) {
            (Some(product), None) => Some(product),
            _ => None,
        }
    }
}

/// The store table of `tables`, with the English names of `text`.
pub fn store_table(tables: &Tables, text: &BTreeMap<String, String>) -> StoreTable {
    let rows: Vec<&EntitlementData> = tables
        .entitlement_data
        .iter()
        .flat_map(|table| &table.rows)
        .collect();
    let mount_names: HashMap<String, &str> = tables
        .mount_data
        .iter()
        .flat_map(|table| &table.rows)
        .map(|row| (row.mount_id.to_ascii_lowercase(), row.display_name.as_str()))
        .collect();
    let products = StoreProducts::build(
        tables
            .store_product_data
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| (row.unique_tag_id.as_str(), row.display_name.as_str())),
    );
    store_table_of(&rows, text, &mount_names, &products)
}

/// The store table of the entitlement rows `rows`, with the English names of `text`, the
/// `DisplayName` of each mount by lowercase `MountId` (`mount_names`) and the store products.
pub fn store_table_of(
    rows: &[&EntitlementData],
    text: &BTreeMap<String, String>,
    mount_names: &HashMap<String, &str>,
    products: &StoreProducts,
) -> StoreTable {
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
                let name = mount_names.get(&mount.to_ascii_lowercase())?;
                Some(StoreMount {
                    unlock: entitlement(row),
                    mount: mount.to_owned(),
                    product: products.of(mount, name)?.to_owned(),
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
        let rows = [
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
        ];
        let rows: Vec<&EntitlementData> = rows.iter().collect();
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
        // Build 25810808: each mount's `DisplayName` and the products that carry it.
        let mount_names = HashMap::from([
            (
                "mount_mtx_7_bear_ronin".to_owned(),
                "@ui_mounts_MTX_7_bear_name",
            ),
            ("mount_mtx_1_wolf".to_owned(), "@ui_mounts_MTX_1_wolf_name"),
        ]);
        let products = StoreProducts::build([
            ("Mount_Bear_MTX7_Ronin", "@ui_mounts_MTX_7_bear_name"),
            ("Mount_Wolf_1", "@ui_mounts_MTX_1_wolf_name"),
        ]);
        let table = store_table_of(&rows, &text, &mount_names, &products);
        let sold: Vec<(&str, &str, &str, &str)> = table
            .mounts
            .iter()
            .map(|m| {
                (
                    m.unlock.entitlement.as_str(),
                    m.unlock.name.as_str(),
                    m.mount.as_str(),
                    m.product.as_str(),
                )
            })
            .collect();
        assert_eq!(
            sold,
            [
                (
                    "Ent_Mount_MTX_7_bear_Ronin",
                    "Ronin Bear Mount",
                    "Mount_MTX_7_bear_Ronin",
                    "Mount_Bear_MTX7_Ronin"
                ),
                (
                    "Ent_Mount_MTX_1_wolf",
                    "Ent_Mount_MTX_1_wolf",
                    "Mount_MTX_1_wolf",
                    "Mount_Wolf_1"
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
        let unsold = store_table_of(&rows, &text, &mount_names, &StoreProducts::default());
        assert!(
            unsold.mounts.is_empty(),
            "a mount without a product is not sold"
        );
    }

    #[test]
    fn a_shared_product_name_is_told_apart_by_the_mount_s_variant() {
        // Build 25810808: two lion products share the Reaper lion's name, and two products the
        // Blood grunt's.
        const LION_NAME: &str = "@ui_mounts_MTX_8_lion_name";
        const GRUNT_NAME: &str = "@ui_mounts_MTX_2_grunt_name";
        let products = StoreProducts::build([
            ("Mount_Lion_Reaper", LION_NAME),
            ("StoreItem_Mount_Lion_GreekGods", LION_NAME),
            ("StoreItem_Mount_Grunt_2_Blood", GRUNT_NAME),
            ("StoreItem_Mount_Horse_Coquette", GRUNT_NAME),
        ]);
        assert_eq!(
            products.of("Mount_MTX_8_lion_Reaper", LION_NAME),
            Some("Mount_Lion_Reaper")
        );
        assert_eq!(
            products.of("Mount_MTX_2_Grunt_Blood", GRUNT_NAME),
            Some("StoreItem_Mount_Grunt_2_Blood")
        );
        assert_eq!(products.of("Mount_MTX_8_lion_Plain", LION_NAME), None);
        assert_eq!(products.of("Mount_MTX_1_wolf", "@nothing"), None);
    }
}
