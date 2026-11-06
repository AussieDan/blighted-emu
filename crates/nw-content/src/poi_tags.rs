// SPDX-License-Identifier: AGPL-3.0-or-later
//! The point-of-interest tags of the territories (`TerritoryDefinitions`): a quest task names a
//! point of interest by its `POITag` (`drowned_shipwreck_2_30125`, `watchtower`), and the client
//! names the territory areas the player enters and leaves by their `TerritoryID` (2297 and 6416;
//! retail 2026-09-18: 40148, a watchtower of the start beach).

use std::collections::HashMap;

use crate::Tables;

/// Separates the tags of one `POITag` cell (`spiritshrine_20312,fasttravel`).
const TAG_SEPARATOR: char = ',';

/// The tags of every territory by its id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoiTags {
    by_territory: HashMap<u32, Vec<String>>,
}

impl PoiTags {
    /// The tags of `TerritoryDefinitions` in `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .territory_definition
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let territory = u32::try_from(row.territory_id).ok()?;
                    Some((territory, row.poi_tag.clone()?))
                }),
        )
    }

    /// The tags of `territories` (territory id, `POITag` cell); the first row of an id wins.
    pub fn build(territories: impl IntoIterator<Item = (u32, String)>) -> Self {
        let mut by_territory = HashMap::new();
        for (territory, cell) in territories {
            let tags: Vec<String> = cell
                .split(TAG_SEPARATOR)
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect();
            if !tags.is_empty() {
                by_territory.entry(territory).or_insert(tags);
            }
        }
        Self { by_territory }
    }

    /// The tags of territory `territory`; none for a territory without one.
    pub fn of(&self, territory: u32) -> &[String] {
        self.by_territory.get(&territory).map_or(&[], Vec::as_slice)
    }

    /// The territories that carry tag `tag` (any case), in id order.
    pub fn territories_with(&self, tag: &str) -> Vec<u32> {
        let mut territories: Vec<u32> = self
            .by_territory
            .iter()
            .filter(|(_, tags)| tags.iter().any(|known| known.eq_ignore_ascii_case(tag)))
            .map(|(territory, _)| *territory)
            .collect();
        territories.sort_unstable();
        territories
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_territory_has_the_tags_of_its_cell() {
        // Build 25810808 rows: the start beach's watchtower and a shrine with two tags.
        const WATCHTOWER: u32 = 30_117;
        const SHRINE: u32 = 20_312;
        let tags = PoiTags::build([
            (WATCHTOWER, "watchtower".to_owned()),
            (SHRINE, "spiritshrine_20312, fasttravel".to_owned()),
            (WATCHTOWER, "other".to_owned()),
            (1, String::new()),
        ]);
        assert_eq!(tags.of(WATCHTOWER), ["watchtower"]);
        assert_eq!(tags.of(SHRINE), ["spiritshrine_20312", "fasttravel"]);
        assert!(tags.of(1).is_empty());
        assert!(tags.of(2).is_empty());
        assert_eq!(tags.territories_with("FastTravel"), [SHRINE]);
        assert!(
            tags.territories_with("other").is_empty(),
            "the first row of an id wins"
        );
    }
}
