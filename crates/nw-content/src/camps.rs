// SPDX-License-Identifier: AGPL-3.0-or-later
//! The camps a player places (camp study 2026-10-30, retail `Hivecamp` and `Hive_camping`):
//! the camp blueprints (`BlueprintItemDefinitions` rows with `IsCamp`: `CampT1` to `CampT6`,
//! each `CampSkin_*` row and their `_PVP` variants) and the camp skins (`CampSkinData`).
//!
//! The 2236 placement names its blueprint by the CRC-32 of its lowercase id (Observed: retail
//! `CampSkin_MetalFest02_A` in `Hive_camping` and `CampSkin_MetalFest02_A_PVP` in `Hivecamp`).
//! The camp's tier is the player's `Camping` rank: its group's crafting station type is
//! `camp<tier>` and its tier buildable `CampT<tier>` (Observed: `crc("camp5")` and
//! `crc("campt5")` on both retail tier 5 camps, of skin blueprints).

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;

/// The id of the default camp blueprint of `tier` (`CampT1` to `CampT6`).
pub fn default_camp(tier: u32) -> String {
    format!("CampT{tier}")
}

/// The crafting station type of a camp of `tier` (`camp1` to `camp6`, the station type recipes
/// name).
pub fn camp_station_type(tier: u32) -> String {
    format!("Camp{tier}")
}

/// One camp blueprint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CampBlueprint {
    /// `BlueprintID`.
    pub id: String,
    /// `HP` (300 for every camp without PvP, 2000 to 22000 for the `_PVP` rows).
    pub hp: u32,
    /// `IsPvPCamp`.
    pub pvp: bool,
    /// `PvPCampVariantID`: the blueprint a PvP-flagged player places instead.
    pub pvp_variant: Option<String>,
}

/// One camp skin (`CampSkinData`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CampSkin {
    /// `CampSkinID`: the id of its blueprint without PvP.
    pub id: String,
    /// `IsEnabled`.
    pub enabled: bool,
    /// `RequiredAchievementId`.
    pub required_achievement: Option<String>,
}

/// The camp blueprints and skins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CampRules {
    /// The blueprints by the CRC-32 of their lowercase id.
    blueprints: HashMap<u32, CampBlueprint>,
    /// The skins by the CRC-32 of their lowercase id.
    skins: HashMap<u32, CampSkin>,
}

impl CampRules {
    /// The camp rows of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .blueprint_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter(|row| row.is_camp == Some(true))
                .map(|row| CampBlueprint {
                    id: row.blueprint_id.clone(),
                    hp: row
                        .hp
                        .and_then(|hp| u32::try_from(hp).ok())
                        .unwrap_or_default(),
                    pvp: row.is_pv_p_camp == Some(true),
                    pvp_variant: row
                        .pv_p_camp_variant_id
                        .clone()
                        .filter(|variant| !variant.is_empty()),
                }),
            tables
                .camp_skin_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| CampSkin {
                    id: row.camp_skin_id.clone(),
                    enabled: row.is_enabled,
                    required_achievement: row
                        .required_achievement_id
                        .clone()
                        .filter(|achievement| !achievement.is_empty()),
                }),
        )
    }

    /// The rules of `blueprints` and `skins`.
    pub fn build(
        blueprints: impl IntoIterator<Item = CampBlueprint>,
        skins: impl IntoIterator<Item = CampSkin>,
    ) -> Self {
        Self {
            blueprints: blueprints
                .into_iter()
                .map(|blueprint| (lowercase_crc32(&blueprint.id), blueprint))
                .collect(),
            skins: skins
                .into_iter()
                .map(|skin| (lowercase_crc32(&skin.id), skin))
                .collect(),
        }
    }

    /// The blueprint whose lowercase id has the CRC-32 `crc`.
    pub fn blueprint_of_crc(&self, crc: u32) -> Option<&CampBlueprint> {
        self.blueprints.get(&crc)
    }

    /// The blueprint `id` (any case).
    pub fn blueprint(&self, id: &str) -> Option<&CampBlueprint> {
        self.blueprint_of_crc(lowercase_crc32(id))
    }

    /// The skin of blueprint `blueprint`: the skin of its id, or of the blueprint whose PvP
    /// variant it is; `None` for a default camp.
    pub fn skin_of(&self, blueprint: &CampBlueprint) -> Option<&CampSkin> {
        self.skins.get(&lowercase_crc32(&blueprint.id)).or_else(|| {
            self.blueprints
                .values()
                .filter(|base| {
                    base.pvp_variant
                        .as_deref()
                        .is_some_and(|variant| variant.eq_ignore_ascii_case(&blueprint.id))
                })
                .find_map(|base| self.skins.get(&lowercase_crc32(&base.id)))
        })
    }

    /// The skin whose lowercase id has the CRC-32 `crc` (the 742 and 1292 skin).
    pub fn skin_of_crc(&self, crc: u32) -> Option<&CampSkin> {
        self.skins.get(&crc)
    }

    /// The blueprint a player places with blueprint `blueprint`: its PvP variant when `pvp`, else
    /// itself.
    pub fn placed<'a>(&'a self, blueprint: &'a CampBlueprint, pvp: bool) -> &'a CampBlueprint {
        blueprint
            .pvp_variant
            .as_deref()
            .filter(|_| pvp && !blueprint.pvp)
            .and_then(|variant| self.blueprint(variant))
            .unwrap_or(blueprint)
    }

    /// Camp blueprints in the rules.
    pub fn len(&self) -> usize {
        self.blueprints.len()
    }

    /// Whether the rules have no camp blueprint.
    pub fn is_empty(&self) -> bool {
        self.blueprints.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blueprint(id: &str, pvp: bool, variant: Option<&str>) -> CampBlueprint {
        CampBlueprint {
            id: id.to_owned(),
            hp: if pvp { 2_000 } else { 300 },
            pvp,
            pvp_variant: variant.map(str::to_owned),
        }
    }

    fn rules() -> CampRules {
        CampRules::build(
            [
                blueprint("CampT1", false, Some("CampT1_PVP")),
                blueprint("CampT1_PVP", true, None),
                blueprint(
                    "CampSkin_MetalFest02_A",
                    false,
                    Some("CampSkin_MetalFest02_A_PVP"),
                ),
                blueprint("CampSkin_MetalFest02_A_PVP", true, None),
            ],
            [CampSkin {
                id: "CampSkin_MetalFest02_A".to_owned(),
                enabled: true,
                required_achievement: None,
            }],
        )
    }

    #[test]
    fn a_placement_crc_names_its_blueprint_and_its_skin() {
        let rules = rules();
        // `Hivecamp` 2236 at 1781367544.661: skin 0x89a7af2d.
        let pvp = rules.blueprint_of_crc(0x89a7_af2d).expect("PvP skin camp");
        assert_eq!(pvp.id, "CampSkin_MetalFest02_A_PVP");
        assert_eq!(
            rules.skin_of(pvp).map(|skin| skin.id.as_str()),
            Some("CampSkin_MetalFest02_A")
        );
        let default = rules.blueprint("campt1").expect("default camp");
        assert_eq!(rules.skin_of(default), None);
        assert_eq!(rules.placed(default, true).id, "CampT1_PVP");
        assert_eq!(rules.placed(default, false).id, "CampT1");
        assert_eq!(rules.placed(pvp, false).id, pvp.id, "a PvP row stays");
    }

    #[test]
    fn the_tier_names_the_default_camp_and_its_station() {
        assert_eq!(default_camp(5), "CampT5");
        assert_eq!(lowercase_crc32(&camp_station_type(5)), 2_729_021_248);
        assert_eq!(lowercase_crc32(&default_camp(5)), 464_371_189);
    }
}
