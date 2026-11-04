// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client records the studio editors show (admin studio spec section 7): every record of the
//! table kinds of the retail bundle, read once, with its English name.

use std::collections::HashMap;

use nw_content::ContentBundle;
use nw_content::bundle::english;
use nw_design::{Kind, client_records};
use serde::Serialize;
use serde_json::{Map, Value};

/// The kinds the editors read.
pub const CATALOG_KINDS: [Kind; 10] = [
    Kind::Item,
    Kind::Weapon,
    Kind::Armor,
    Kind::Consumable,
    Kind::Perk,
    Kind::LootTable,
    Kind::Mob,
    Kind::Npc,
    Kind::Objective,
    Kind::ObjectiveTask,
];
/// The field that holds a record's name key.
const NAME_FIELD: &str = "name";

/// One client record, its fields kept as compact JSON text to hold many records in little memory.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// The record id.
    pub id: String,
    /// The English name, when the record names a text key that the bundle has.
    pub name: Option<String>,
    fields: Box<str>,
}

impl CatalogEntry {
    /// Every field of the record except the id.
    pub fn fields(&self) -> Map<String, Value> {
        serde_json::from_str(&self.fields).unwrap_or_default()
    }
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogHit<'a> {
    /// The record id.
    pub id: &'a str,
    /// The English name.
    pub name: Option<&'a str>,
}

/// The records of [`CATALOG_KINDS`].
#[derive(Debug, Default)]
pub struct Catalog {
    kinds: HashMap<Kind, Vec<CatalogEntry>>,
}

impl Catalog {
    /// The catalogue of `bundle`.
    pub fn of_bundle(bundle: &ContentBundle) -> Self {
        let kinds = CATALOG_KINDS
            .iter()
            .map(|kind| {
                let entries = client_records(&bundle.tables, *kind)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|record| CatalogEntry {
                        name: name_of(&record.fields, &bundle.text),
                        fields: Value::Object(record.fields).to_string().into(),
                        id: record.id,
                    })
                    .collect();
                (*kind, entries)
            })
            .collect();
        Self { kinds }
    }

    /// At most `limit` records of `kind` whose id or name holds `query` (any case), in table
    /// order; every record for an empty query.
    pub fn search(&self, kind: Kind, query: &str, limit: usize) -> Vec<CatalogHit<'_>> {
        let query = query.trim().to_lowercase();
        self.kinds
            .get(&kind)
            .into_iter()
            .flatten()
            .filter(|entry| {
                query.is_empty()
                    || entry.id.to_lowercase().contains(&query)
                    || entry
                        .name
                        .as_ref()
                        .is_some_and(|name| name.to_lowercase().contains(&query))
            })
            .take(limit)
            .map(|entry| CatalogHit {
                id: &entry.id,
                name: entry.name.as_deref(),
            })
            .collect()
    }

    /// Every record of `kind`, in table order.
    pub fn entries(&self, kind: Kind) -> &[CatalogEntry] {
        self.kinds.get(&kind).map_or(&[], Vec::as_slice)
    }

    /// The record of `kind` with `id` (any case, as the server finds records).
    pub fn record(&self, kind: Kind, id: &str) -> Option<&CatalogEntry> {
        self.kinds
            .get(&kind)?
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id))
    }
}

/// The English text of the record's name key: the key as written, else in lower case.
fn name_of(
    fields: &Map<String, Value>,
    text: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    english(text, fields.get(NAME_FIELD)?.as_str()?).map(str::to_owned)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use nw_content::Tables;
    use nw_content::generated::MasterItemDefinitions;
    use nw_content::table::Table;

    use super::*;

    pub(crate) const SWORD: &str = "1hSwordT2";
    pub(crate) const SWORD_NAME: &str = "Iron Sword";
    const AXE: &str = "1hAxeT2";
    pub(crate) const TIER: i64 = 2;

    fn item(id: &str, name_key: &str) -> MasterItemDefinitions {
        serde_json::from_value(serde_json::json!({
            "item_id": id,
            "name": name_key,
            "tier": TIER,
        }))
        .expect("row")
    }

    /// A bundle with a named sword and an axe whose name is not in the text.
    pub(crate) fn bundle() -> ContentBundle {
        let mut tables = Tables::default();
        tables.master_item_definitions.push(Table {
            source_path: "items.datasheet".to_owned(),
            unique_id: "items".to_owned(),
            rows: vec![item(SWORD, "@Sword_Name"), item(AXE, "@Axe_Name")],
        });
        ContentBundle {
            tables,
            text: BTreeMap::from([("sword_name".to_owned(), SWORD_NAME.to_owned())]),
            ..ContentBundle::default()
        }
    }

    #[test]
    fn records_are_found_by_id_or_name_in_any_case() {
        let catalog = Catalog::of_bundle(&bundle());
        let hits = catalog.search(Kind::Item, "iron", 10);
        assert_eq!(
            hits,
            [CatalogHit {
                id: SWORD,
                name: Some(SWORD_NAME)
            }]
        );
        assert_eq!(catalog.search(Kind::Item, "AXE", 10)[0].name, None);
        assert_eq!(catalog.search(Kind::Item, "", 1).len(), 1);
        assert!(catalog.search(Kind::Mob, "", 10).is_empty());
        let sword = catalog.record(Kind::Item, "1HSWORDT2").expect("sword");
        assert_eq!(sword.fields()["tier"], TIER);
    }
}
