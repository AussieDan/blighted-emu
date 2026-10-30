// SPDX-License-Identifier: AGPL-3.0-or-later
//! The camp slices (`camp-slices-<build>.json`, written by `content-tool camp-slices`, read by
//! the open world sims): per camp blueprint (`BlueprintItemDefinitions` row with `IsCamp`), the
//! `.dynamicslice` under `slices/structures/camps/` whose buildable controller names it
//! (`m_structureName`), its asset id (the asset catalog's), the components of its spawn group
//! and its respawn point.
//!
//! Observed against the retail camp groups of `Hivecamp` 1781367544743 (a PvP camp,
//! `campskin_metalfest02_a_pvp`, card asset `a0ba3a56…` sub 2) and `Hive_camping` 1782809402671
//! (the same skin without PvP, `2b4253bb…`): each member's component number is the
//! `m_replicationIndex` of the slice component of its class (the buildable controller 1, the
//! position 2, the territory interface 3, the delayed events 4, 13 and 14, the placement
//! obstruction 5, the interact 6, the vitals 7, the damage receiver 8 and the ownership 11 of the
//! PvP slice; the ownership is 7 in the other). The class UUIDs below are those of the slice
//! components at the indexes the retail groups name.
//!
//! The respawn point is the slice's entity named `SpawnPoint` (Observed: `Hivecamp` 1938 at
//! 1781367625.189 put the player 1.70 m from the camp, the `SpawnPoint` translation (0, -1.7,
//! 0.2) turned by the camp's rotation; the camp's 3652 element lists that point).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nw_formats::objectstream::{Element, lowercase_crc32};
use serde::{Deserialize, Serialize};

use crate::slice_data::GUID_LEN;
use crate::stations::{DELAYED_EVENT, StationSlice, replication_index};
use crate::travel_slices::{AXES, SliceEntities, TRANSLATION_AT, UUID_LEN, descendants, uuid};

/// The file name prefix; the client build and [`CAMP_SLICES_EXTENSION`] follow.
pub const CAMP_SLICES_PREFIX: &str = "camp-slices-";
/// The file extension.
pub const CAMP_SLICES_EXTENSION: &str = ".json";
/// The pak folder of the camp slices.
pub const CAMP_SLICE_FOLDER: &str = "slices/structures/camps/";

/// `PositionInTheWorldComponent` (retail member 2, class 13).
const POSITION: [u8; UUID_LEN] = uuid(0x6c2f_c842_f451_4b45_9d7d_a6ee_954d_f025);
/// The interact component (retail member 6, class 2930; the slice has three, the group sends
/// the first).
const INTERACT: [u8; UUID_LEN] = uuid(0xb690_45a3_a129_4334_bdfb_069c_18cc_84a2);
/// The vitals component of a PvP camp (retail member 7, class 15).
const VITALS: [u8; UUID_LEN] = uuid(0x0c84_8755_1f41_4408_b2be_0a36_2767_4f50);
/// The damage receiver of a PvP camp (retail member 8, class 1528).
const DAMAGE_RECEIVER: [u8; UUID_LEN] = uuid(0x1bd9_d601_9de8_4a8b_b115_bb53_4eb6_020c);
/// The name CRC of the vitals component's row text (`SiegeWeaponSmall` in the PvP camps: its
/// lowercase CRC-32 is the retail `vitalsId` 514784115; the field's name is Unknown).
const VITALS_ROW: u32 = 0x9304_da38;
/// The name of the entity whose transform is the camp's respawn point.
const SPAWN_POINT: &str = "SpawnPoint";

/// The vitals component of a camp slice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampVitals {
    /// Its component number.
    pub component: u8,
    /// The vitals row it names.
    pub row: String,
}

/// What a camp slice gives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CampSlice {
    /// The slice's pak path.
    pub slice: String,
    /// Its asset id: the GUID and the sub id the spawn card names.
    pub asset: [u8; GUID_LEN],
    /// The asset id's sub id.
    pub asset_sub_id: u32,
    /// Its station components (buildable controller, ownership, obstruction, territory).
    pub station: StationSlice,
    /// The position component.
    pub position: u8,
    /// The first interact component.
    pub interact: Option<u8>,
    /// The vitals component (PvP camps).
    pub vitals: Option<CampVitals>,
    /// The damage receiver (PvP camps).
    pub damage_receiver: Option<u8>,
    /// The delayed event components, lowest first.
    pub delayed_events: Vec<u8>,
    /// The respawn point in the camp's frame, metres.
    pub spawn_point: [f32; AXES],
}

impl CampSlice {
    /// The camp of the dynamic slice `slice` (pak path) whose root element is `root` and whose
    /// asset id is `asset`, `sub_id`; `None` for a slice without a crafting station, a buildable
    /// controller, a position or a `SpawnPoint` entity.
    pub fn of(slice: &str, root: &Element, asset: [u8; GUID_LEN], sub_id: u32) -> Option<Self> {
        let station = StationSlice::of(root)?;
        station.buildable?;
        let all = descendants(root);
        let indexes = |class: &[u8; UUID_LEN]| {
            let mut indexes: Vec<u8> = all
                .iter()
                .filter(|element| &element.type_id == class)
                .filter_map(|element| replication_index(element))
                .collect();
            indexes.sort_unstable();
            indexes
        };
        let first = |class: &[u8; UUID_LEN]| indexes(class).first().copied();
        let vitals = all
            .iter()
            .find(|element| element.type_id == VITALS)
            .and_then(|vitals| {
                let row = descendants(vitals)
                    .into_iter()
                    .find(|element| element.name_crc == Some(VITALS_ROW))?
                    .as_string()
                    .ok()?;
                Some(CampVitals {
                    component: replication_index(vitals)?,
                    row: row.to_owned(),
                })
            });
        Some(Self {
            slice: slice.to_owned(),
            asset,
            asset_sub_id: sub_id,
            position: first(&POSITION)?,
            interact: first(&INTERACT),
            vitals,
            damage_receiver: first(&DAMAGE_RECEIVER),
            delayed_events: indexes(&DELAYED_EVENT),
            spawn_point: spawn_point(&all)?,
            station,
        })
    }

    /// The camp blueprint the slice is (`m_structureName`).
    pub fn blueprint(&self) -> &str {
        &self.station.structure
    }
}

/// The translation of the slice entity named [`SPAWN_POINT`] in the slice's frame.
fn spawn_point(all: &[&Element]) -> Option<[f32; AXES]> {
    let name = lowercase_crc32("Name");
    let entities = SliceEntities::of(all);
    let (id, _) = entities.all().find(|(_, entity)| {
        entity
            .child(name)
            .and_then(|name| name.as_string().ok())
            .is_some_and(|name| name == SPAWN_POINT)
    })?;
    let placed = entities.placed(id);
    Some(std::array::from_fn(|axis| placed[TRANSLATION_AT + axis]))
}

/// The camp slices, by lowercase blueprint id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CampSlices {
    /// The slices by lowercase blueprint id.
    pub by_blueprint: BTreeMap<String, CampSlice>,
}

/// Why the file cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum CampSlicesError {
    /// The file cannot be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file is not camp slices.
    #[error("{path}: {source}")]
    Json {
        /// The file.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
}

/// The camp slice file of `client_build` in `dir`.
pub fn camp_slices_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!(
        "{CAMP_SLICES_PREFIX}{client_build}{CAMP_SLICES_EXTENSION}"
    ))
}

impl CampSlices {
    /// The table of `slices`.
    pub fn of(slices: impl IntoIterator<Item = CampSlice>) -> Self {
        Self {
            by_blueprint: slices
                .into_iter()
                .map(|slice| (slice.blueprint().to_lowercase(), slice))
                .collect(),
        }
    }

    /// Reads `path`; a missing file has no camps (content from before this file).
    ///
    /// # Errors
    ///
    /// Returns [`CampSlicesError`] when the file cannot be read or is not camp slices.
    pub fn load(path: &Path) -> Result<Self, CampSlicesError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| CampSlicesError::Json {
                path: path.to_path_buf(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(CampSlicesError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Writes the file to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`CampSlicesError`] when a value cannot be written or the file cannot be.
    pub fn write(&self, path: &Path) -> Result<(), CampSlicesError> {
        let text = serde_json::to_string_pretty(self).map_err(|source| CampSlicesError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, text).map_err(|source| CampSlicesError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The slice of camp blueprint `id` (any case).
    pub fn get(&self, id: &str) -> Option<&CampSlice> {
        self.by_blueprint.get(&id.to_lowercase())
    }

    /// Camp slices in the table.
    pub fn len(&self) -> usize {
        self.by_blueprint.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_blueprint.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::travel_slices::ENTITY;

    fn named(name: &str, children: Vec<Element>) -> Element {
        Element::with_children(lowercase_crc32(name), children)
    }

    fn value(name: &str, bytes: &[u8]) -> Element {
        Element::with_value(lowercase_crc32(name), bytes.to_vec())
    }

    fn component(class: [u8; UUID_LEN], index: u32, mut fields: Vec<Element>) -> Element {
        fields.insert(
            0,
            named(
                "BaseClass1",
                vec![value("m_replicationIndex", &index.to_be_bytes())],
            ),
        );
        Element {
            type_id: class,
            children: fields,
            ..Element::default()
        }
    }

    /// An entity of id `id` named `name` with `components`.
    fn entity(id: u64, name: &str, components: Vec<Element>) -> Element {
        Element {
            type_id: ENTITY,
            children: vec![
                named("Id", vec![value("id", &id.to_be_bytes())]),
                value("Name", name.as_bytes()),
                named("Components", components),
            ],
            ..Element::default()
        }
    }

    fn transform(translation: [f32; AXES]) -> Element {
        let floats: Vec<u8> = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
            .into_iter()
            .chain(translation)
            .flat_map(f32::to_be_bytes)
            .collect();
        named("Transform", vec![value("m_localTM", &floats)])
    }

    /// The PvP metalfest camp slice's components (`campskin_metalfest02_a_pvp.dynamicslice`, cut).
    fn pvp_camp() -> Element {
        let station = |index, class| component(class, index, Vec::new());
        let structure = entity(
            1,
            "CampSkin_MetalFest02_A_PVP",
            vec![
                component(
                    uuid(0x359f_8468_0483_46ee_b1d4_beb7_5c10_48fd),
                    1,
                    vec![
                        value("m_initialState", &3_u32.to_be_bytes()),
                        value("m_structureName", b"CampSkin_MetalFest02_A_PVP"),
                    ],
                ),
                station(2, POSITION),
                component(
                    uuid(0x5200_15fd_2674_49e1_b862_e737_1ae6_0043),
                    0,
                    vec![named(
                        "m_craftingStationReference",
                        vec![value("m_craftingStationEntry", b"Camp1")],
                    )],
                ),
                station(14, DELAYED_EVENT),
                station(4, DELAYED_EVENT),
                station(13, DELAYED_EVENT),
                station(9, INTERACT),
                station(6, INTERACT),
                component(
                    VITALS,
                    7,
                    vec![Element::with_value(
                        VITALS_ROW,
                        b"SiegeWeaponSmall".to_vec(),
                    )],
                ),
                station(8, DAMAGE_RECEIVER),
            ],
        );
        let spawn = entity(2, SPAWN_POINT, vec![transform([0.0, -1.7, 0.2])]);
        named("Entities", vec![structure, spawn])
    }

    #[test]
    fn the_pvp_camp_slice_gives_its_members_and_respawn_point() {
        const ASSET: [u8; GUID_LEN] = [7; GUID_LEN];
        const SLICE: &str = "slices/structures/camps/campvariants/campskin_metalfest02_a_pvp";
        let camp = CampSlice::of(SLICE, &pvp_camp(), ASSET, 2).expect("camp");
        assert_eq!(camp.blueprint(), "CampSkin_MetalFest02_A_PVP");
        assert_eq!(camp.station.buildable, Some((1, 3)));
        assert_eq!(camp.position, 2);
        assert_eq!(camp.interact, Some(6), "the first interact");
        assert_eq!(camp.delayed_events, [4, 13, 14]);
        assert_eq!(
            camp.vitals,
            Some(CampVitals {
                component: 7,
                row: "SiegeWeaponSmall".to_owned(),
            })
        );
        assert_eq!(camp.damage_receiver, Some(8));
        assert_eq!(camp.spawn_point, [0.0, -1.7, 0.2]);
        assert_eq!((camp.asset, camp.asset_sub_id), (ASSET, 2));
    }

    #[test]
    fn a_slice_without_a_spawn_point_is_no_camp() {
        let mut slice = pvp_camp();
        slice.children.truncate(1);
        assert_eq!(CampSlice::of("x", &slice, [0; GUID_LEN], 2), None);
    }

    #[test]
    fn the_table_finds_a_blueprint_in_any_case() {
        let camp = CampSlice::of("x", &pvp_camp(), [0; GUID_LEN], 2).expect("camp");
        let table = CampSlices::of([camp]);
        assert!(table.get("campskin_metalfest02_a_pvp").is_some());
        assert_eq!(table.len(), 1);
        let dir = tempfile::tempdir().expect("dir");
        let path = camp_slices_path(dir.path(), "1");
        table.write(&path).expect("write");
        assert_eq!(CampSlices::load(&path).expect("load"), table);
        assert!(
            CampSlices::load(&dir.path().join("none"))
                .expect("missing")
                .is_empty()
        );
    }
}
