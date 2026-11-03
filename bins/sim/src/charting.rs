// SPDX-License-Identifier: AGPL-3.0-or-later
//! Charting the areas of a territory (retail newchar 2026-09-18, Windsward, and three more new
//! characters on Monarch's Bluffs: each got 550 experience as it first came into the start
//! beach area, the only gain between the tutorial's 120 and level 2's 670).
//!
//! **Observed.** The client names the area it comes into (2297: `9011`, the start beach of
//! Windsward), never the territory. At the first entry retail set the area's
//! `ChartedAchievement` (`starterbeach_9011_charted`, whose `GameEventID` is `Chart_Location`)
//! and listed two game events in the player's game event state, both for territory 6:
//! `Chart_Location` with amount 75 (Windsward's `POIDiscoveryXPModifier`) and standing 50, and
//! `CategoricalProgressionRankUp` with amount 125 (territory standing rank 1's `XPReward` 100
//! times Windsward's `TerritoryStandingXPModifier` 1.25). Each event's experience is its amount
//! times its `UniversalExpAmount` (2 and 1): (75 x 2 + 125 x 1) x the character multiplier 2 =
//! 550. The standing 50 x the territory's multiplier 3 = 150 is rank 0's cap, so it reached
//! rank 1. A second chart of the same territory later gave the same `Chart_Location` entry.
//!
//! **Observed.** The level's records list the territory first, then the area and the point of
//! interest they lie in (`[6, 9011]`, `[6, 40142, 9011]`, `[12, 9009]`): that is how the server
//! knows the territory of an area.
//!
//! **Observed.** A fast travel shrine is charted the same way: retail `Playthrough_Part_01`
//! 3210.693 s, the client's 2297 for Brysedd Overlook Shrine (30161, `PoiTag`
//! `spiritshrine_02_30151,fasttravel`, `IsArea` empty) and 68 ms later its `ChartedAchievement`
//! `spiritshrine_02_30151_charted` with a `Chart_Location` of 75 (Monarch's Bluffs); 152 of 173
//! first shrine entries over 1,082 ledgers. The world map lists an uncharted shrine as an
//! "Undiscovered Fast Travel Point" (`fasttravelicon.luac`). A discoverable shrine with a
//! `DiscoveredAchievement` is discovered at its first entry and charted at a later one.
//!
//! **Unknown.** What charts another point of interest: retail discovered the shipwreck 40142 at
//! the same entry (`drowned_shipwreck_2_40142_discovered`) but did not chart it, though it has a
//! `ChartedAchievement`. Only areas (`IsArea`) and fast travel points are charted here.

use std::collections::{HashMap, HashSet};

use nw_content::Tables;
use nw_world::WorldFile;

/// The game event of a chart (`Chart_Location`).
pub const CHART_EVENT: &str = "Chart_Location";
/// The game event of a categorical progression's rank up.
pub const RANK_UP_EVENT: &str = "CategoricalProgressionRankUp";
/// The standing experience modifier of a progression that is no territory's standing.
pub const NEUTRAL_STANDING_MODIFIER: f64 = 1.0;
/// The `PoiTag` of a fast travel point (all 137 rows that carry it are shrines with a
/// `ChartedAchievement`, build 25810808).
const FAST_TRAVEL_POI_TAG: &str = "fasttravel";
/// Separates the tags of a `PoiTag` cell.
const POI_TAG_SEPARATOR: char = ',';

/// Whether `poi_tag` (a `PoiTag` cell) names a fast travel point.
fn is_fast_travel_point(poi_tag: Option<&str>) -> bool {
    poi_tag.is_some_and(|tags| {
        tags.split(POI_TAG_SEPARATOR)
            .any(|tag| tag.trim().eq_ignore_ascii_case(FAST_TRAVEL_POI_TAG))
    })
}

/// What a territory's charts and standing give.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerritoryExperience {
    /// `POIDiscoveryXPModifier`: the amount of a chart in it.
    pub chart_amount: u32,
    /// `TerritoryStandingXPModifier`: the factor of its standing ranks' `XPReward`.
    pub standing_modifier: f64,
}

/// The chartable areas and fast travel points, the territory of each and what each territory
/// gives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartRules {
    /// The `ChartedAchievement` of each chartable area and fast travel point, by `TerritoryID`.
    charted: HashMap<u32, String>,
    /// The territory each area and point of interest of the level lies in.
    area_territories: HashMap<u32, u32>,
    /// What each territory gives, by `TerritoryID`.
    territories: HashMap<u32, TerritoryExperience>,
}

impl ChartRules {
    /// The rules of `tables` and the records of `level`.
    pub fn of(tables: &Tables, level: &WorldFile) -> Self {
        let rows = || {
            tables
                .territory_definition
                .iter()
                .flat_map(|table| &table.rows)
        };
        let charted = rows()
            .filter(|row| {
                row.is_chartable.unwrap_or_default()
                    && (row.is_area.unwrap_or_default()
                        || is_fast_travel_point(row.poi_tag.as_deref()))
            })
            .filter_map(|row| {
                let area = u32::try_from(row.territory_id).ok()?;
                let achievement = row.charted_achievement.clone()?;
                Some((area, achievement))
            });
        let territories = rows()
            .filter(|row| row.is_territory.unwrap_or_default())
            .filter_map(|row| {
                let territory = u32::try_from(row.territory_id).ok()?;
                let experience = TerritoryExperience {
                    chart_amount: row
                        .poi_discovery_xp_modifier
                        .and_then(|amount| u32::try_from(amount).ok())
                        .unwrap_or_default(),
                    standing_modifier: row
                        .territory_standing_xp_modifier
                        .unwrap_or(NEUTRAL_STANDING_MODIFIER),
                };
                Some((territory, experience))
            });
        let lists = level
            .npcs
            .iter()
            .map(|record| &record.territory_ids)
            .chain(level.vitals.iter().map(|record| &record.territory_ids))
            .chain(level.gatherables.iter().map(|record| &record.territory_ids))
            .chain(level.lore.iter().map(|record| &record.territory_ids))
            .chain(level.landmarks.iter().map(|record| &record.territory_ids))
            .chain(level.encounters.iter().map(|record| &record.territory_ids));
        Self::build(charted, territories, lists)
    }

    /// The rules of the chartable areas `charted` (area, `ChartedAchievement`), the territories
    /// `territories` and the level's territory id lists `lists`: an id of a list that is no
    /// territory lies in the list's territory.
    pub fn build<'a>(
        charted: impl IntoIterator<Item = (u32, String)>,
        territories: impl IntoIterator<Item = (u32, TerritoryExperience)>,
        lists: impl IntoIterator<Item = &'a Vec<u16>>,
    ) -> Self {
        let territories: HashMap<u32, TerritoryExperience> = territories.into_iter().collect();
        let known: HashSet<u32> = territories.keys().copied().collect();
        let mut area_territories = HashMap::new();
        for list in lists {
            let ids = list.iter().map(|id| u32::from(*id));
            let Some(territory) = ids.clone().find(|id| known.contains(id)) else {
                continue;
            };
            for area in ids.filter(|id| !known.contains(id)) {
                area_territories.entry(area).or_insert(territory);
            }
        }
        Self {
            charted: charted.into_iter().collect(),
            area_territories,
            territories,
        }
    }

    /// The `ChartedAchievement` of area `area`; `None` for an area that is not charted.
    pub fn charted_achievement(&self, area: u32) -> Option<&str> {
        self.charted.get(&area).map(String::as_str)
    }

    /// The territory that `id` names: itself for a territory, else the territory the area or
    /// point of interest lies in; `None` for an id of neither.
    pub fn territory_of(&self, id: u32) -> Option<u32> {
        if self.territories.contains_key(&id) {
            return Some(id);
        }
        self.area_territories.get(&id).copied()
    }

    /// What territory `territory` gives; `None` for an id that is no territory.
    pub fn experience(&self, territory: u32) -> Option<TerritoryExperience> {
        self.territories.get(&territory).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windsward and its start beach area (content bundle 25810808).
    const WINDSWARD: u32 = 6;
    const BEACH: u32 = 9011;
    const BEACH_CHARTED: &str = "starterbeach_9011_charted";
    /// A point of interest on the beach: discoverable, not an area.
    const SHIPWRECK: u16 = 40_142;
    const WINDSWARD_EXPERIENCE: TerritoryExperience = TerritoryExperience {
        chart_amount: 75,
        standing_modifier: 1.25,
    };

    #[test]
    fn a_shrine_s_poi_tag_names_a_fast_travel_point_and_a_camp_s_does_not() {
        // Brysedd Overlook Shrine (30161) and a camp (30143), build 25810808.
        assert!(is_fast_travel_point(Some(
            "spiritshrine_02_30151,fasttravel"
        )));
        assert!(is_fast_travel_point(Some("FastTravel")));
        assert!(!is_fast_travel_point(Some("camp_30143")));
        assert!(!is_fast_travel_point(None));
    }

    #[test]
    fn an_area_lies_in_the_territory_its_records_list_first() {
        let lists = [
            vec![6, SHIPWRECK, 9011],
            vec![6, 9011],
            vec![12, 9009],
            vec![9_999],
        ];
        let rules = ChartRules::build(
            [(BEACH, BEACH_CHARTED.to_owned())],
            [(WINDSWARD, WINDSWARD_EXPERIENCE)],
            &lists,
        );
        assert_eq!(rules.territory_of(BEACH), Some(WINDSWARD));
        assert_eq!(rules.territory_of(u32::from(SHIPWRECK)), Some(WINDSWARD));
        assert_eq!(rules.territory_of(WINDSWARD), Some(WINDSWARD));
        assert_eq!(
            rules.territory_of(9009),
            None,
            "territory 12 is not known here"
        );
        assert_eq!(rules.territory_of(9_999), None);
        assert_eq!(rules.charted_achievement(BEACH), Some(BEACH_CHARTED));
        assert_eq!(rules.charted_achievement(u32::from(SHIPWRECK)), None);
        assert_eq!(rules.experience(WINDSWARD), Some(WINDSWARD_EXPERIENCE));
        assert_eq!(rules.experience(BEACH), None);
    }
}
