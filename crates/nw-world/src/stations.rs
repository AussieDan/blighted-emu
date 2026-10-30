// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crafting stations of a level (campfire note, 2026-09-18). A station is a slice that a
//! level's slices spawn ([`crate::slice_data`]) and whose `.dynamicslice` has a crafting station
//! component. The slice's components give what the station's spawn group holds: each replicated
//! component's index (`m_replicationIndex`) is its member's component number, and some carry the
//! values the members send (Observed: the retail campfire groups of MB and WW, and three town
//! stations of `MSQ13-lv56`, match).
//!
//! Components are found by their class UUID (the element type in the slice); the UUIDs are those
//! of the components the retail campfire slice `int_watchtowercampt1` holds at the indexes its
//! retail group names.

use nw_formats::objectstream::{Element, lowercase_crc32};
use serde::{Deserialize, Serialize};

use crate::records::Position;

/// `Javelin::BuildableController`: `m_initialState`, `m_structureName`.
const BUILDABLE_CONTROLLER: [u8; UUID_LEN] = uuid(0x359f_8468_0483_46ee_b1d4_beb7_5c10_48fd);
/// The ownership component: `m_initialPermissionRule`, `m_initialStructureName`.
const OWNERSHIP: [u8; UUID_LEN] = uuid(0x2a33_3c23_a549_bc11_1a9a_ae90_5170_f2f6);
/// The crafting station component: `m_craftingStationReference.m_craftingStationEntry`.
const CRAFTING_STATION: [u8; UUID_LEN] = uuid(0x5200_15fd_2674_49e1_b862_e737_1ae6_0043);
/// The interact name component: `m_name`, the name the station's interaction answer carries.
const INTERACT_NAME: [u8; UUID_LEN] = uuid(0x76e2_6125_7e1c_32f4_c1d0_b841_d643_12ad);
/// `Javelin::PlacementObstructionComponent`.
const PLACEMENT_OBSTRUCTION: [u8; UUID_LEN] = uuid(0xfecc_8785_5d77_7138_0bdf_573d_e220_d90e);
/// `Javelin::TerritoryInterfaceComponent`.
const TERRITORY_INTERFACE: [u8; UUID_LEN] = uuid(0xe1d7_8b60_b892_4d25_9e5e_5ac8_c398_fcab);
/// `Javelin::BuildableGridComponent`: `m_gridHeightsData`.
const BUILDABLE_GRID: [u8; UUID_LEN] = uuid(0xf25b_009b_5108_84df_4f5b_1019_4fc1_7c6a);
/// The delayed event component (the station's ruin timer).
pub(crate) const DELAYED_EVENT: [u8; UUID_LEN] = uuid(0xd353_f7e8_4266_f749_19a1_aa2c_3841_fbca);
/// The detector filter of a buildable's completion obstruction (the campfire slice has one, a
/// town station's none; retail `hasCompletionObstruction` 1 and 0).
const COMPLETION_OBSTRUCTION_FILTER: &str = "CompletionObstructionCast";
/// The four sides of a grid height entry, in data order.
const GRID_SIDES: [&str; 4] = ["m_frontSide", "m_backSide", "m_rightSide", "m_leftSide"];
const UUID_LEN: usize = 16;

const fn uuid(value: u128) -> [u8; UUID_LEN] {
    value.to_be_bytes()
}

/// One side of a station's grid: its `m_side`, its grid point count, and whether a point is
/// active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationGridSide {
    /// `m_side`.
    pub side: u8,
    /// The count of `m_activeGridPoints`.
    pub points: u8,
    /// A point's `m_active` is set.
    pub active: bool,
}

/// What a station slice's components give.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationSlice {
    /// `m_craftingStationEntry`: the station type recipes name (`Camp1`).
    pub station_type: String,
    /// The interact name's `m_name` (`@ui_watcherscampfire`).
    pub name: String,
    /// The buildable controller's component and `m_initialState`.
    pub buildable: Option<(u8, u8)>,
    /// The buildable controller's `m_structureName`: the blueprint whose costs it holds.
    pub structure: String,
    /// The ownership component and `m_initialPermissionRule`.
    pub ownership: Option<(u8, u8)>,
    /// The ownership component's `m_initialStructureName`.
    pub structure_name: String,
    /// The placement obstruction component, and whether the slice has a completion
    /// obstruction detector.
    pub obstruction: Option<(u8, bool)>,
    /// The territory interface component.
    pub territory: Option<u8>,
    /// The buildable grid component and its sides.
    pub grid: Option<(u8, Vec<StationGridSide>)>,
    /// The delayed event component.
    pub delayed_event: Option<u8>,
}

/// A crafting station of the level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CraftingStation {
    /// Its GDE id (its spawned slice's).
    pub id: u64,
    /// Its spawned slice's path.
    pub slice: String,
    /// Its world position.
    pub position: Position,
    /// What its slice's components give.
    pub components: StationSlice,
}

/// The names read, as their name CRCs.
struct Names {
    initial_state: u32,
    structure_name: u32,
    permission_rule: u32,
    initial_structure_name: u32,
    station_reference: u32,
    station_entry: u32,
    name: u32,
    filter: u32,
    grid_heights: u32,
    side: u32,
    points: u32,
    active: u32,
}

impl Names {
    fn new() -> Self {
        Self {
            initial_state: lowercase_crc32("m_initialState"),
            structure_name: lowercase_crc32("m_structureName"),
            permission_rule: lowercase_crc32("m_initialPermissionRule"),
            initial_structure_name: lowercase_crc32("m_initialStructureName"),
            station_reference: lowercase_crc32("m_craftingStationReference"),
            station_entry: lowercase_crc32("m_craftingStationEntry"),
            name: lowercase_crc32("m_name"),
            filter: lowercase_crc32("m_strFilter"),
            grid_heights: lowercase_crc32("m_gridHeightsData"),
            side: lowercase_crc32("m_side"),
            points: lowercase_crc32("m_activeGridPoints"),
            active: lowercase_crc32("m_active"),
        }
    }
}

/// Every element under `root` (itself first), depth first.
fn descendants(root: &Element) -> Vec<&Element> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(element) = stack.pop() {
        found.push(element);
        stack.extend(element.children.iter().rev());
    }
    found
}

/// The first element named `name` under `root`.
fn find(root: &Element, name: u32) -> Option<&Element> {
    descendants(root)
        .into_iter()
        .find(|element| element.name_crc == Some(name))
}

fn text(element: Option<&Element>) -> String {
    element
        .and_then(|element| element.as_string().ok())
        .unwrap_or_default()
        .to_owned()
}

/// The replicated component number (`m_replicationIndex`) of component `element`.
pub(crate) fn replication_index(element: &Element) -> Option<u8> {
    small(find(element, lowercase_crc32("m_replicationIndex")))
}

/// A `u32` field as a byte (indexes and enums of a slice are small).
fn small(element: Option<&Element>) -> Option<u8> {
    element
        .and_then(|element| element.as_u32().ok())
        .and_then(|value| u8::try_from(value).ok())
}

impl StationSlice {
    /// The station of the dynamic slice whose root element is `root`; `None` for a slice
    /// without a crafting station component.
    pub fn of(root: &Element) -> Option<Self> {
        let names = Names::new();
        let all = descendants(root);
        let component = |class: &[u8; UUID_LEN]| {
            all.iter()
                .copied()
                .find(|element| &element.type_id == class)
        };
        let index = replication_index;
        let station = component(&CRAFTING_STATION)?;
        let entry = find(station, names.station_reference)
            .and_then(|reference| reference.child(names.station_entry));
        let buildable = component(&BUILDABLE_CONTROLLER);
        let ownership = component(&OWNERSHIP);
        let completion = all.iter().any(|element| {
            element.name_crc == Some(names.filter)
                && element.as_string().ok() == Some(COMPLETION_OBSTRUCTION_FILTER)
        });
        let grid = component(&BUILDABLE_GRID).and_then(|grid| {
            let heights = find(grid, names.grid_heights)?.children.first()?;
            let sides = GRID_SIDES
                .iter()
                .filter_map(|side| heights.child(lowercase_crc32(side)))
                .map(|side| {
                    let points = side.child(names.points);
                    let point_elements = points.map(|points| points.children.as_slice());
                    StationGridSide {
                        side: small(side.child(names.side)).unwrap_or_default(),
                        points: point_elements.map_or(0, |elements| {
                            u8::try_from(elements.len()).unwrap_or(u8::MAX)
                        }),
                        active: point_elements.unwrap_or_default().iter().any(|point| {
                            point
                                .child(names.active)
                                .and_then(|active| active.as_bool().ok())
                                .unwrap_or(false)
                        }),
                    }
                })
                .collect();
            Some((index(grid)?, sides))
        });
        Some(Self {
            station_type: text(entry),
            name: text(component(&INTERACT_NAME).and_then(|name| find(name, names.name))),
            buildable: buildable.and_then(|buildable| {
                Some((
                    index(buildable)?,
                    small(find(buildable, names.initial_state))?,
                ))
            }),
            structure: text(buildable.and_then(|buildable| find(buildable, names.structure_name))),
            ownership: ownership.and_then(|ownership| {
                Some((
                    index(ownership)?,
                    small(find(ownership, names.permission_rule))?,
                ))
            }),
            structure_name: text(
                ownership.and_then(|ownership| find(ownership, names.initial_structure_name)),
            ),
            obstruction: component(&PLACEMENT_OBSTRUCTION)
                .and_then(index)
                .map(|component| (component, completion)),
            territory: component(&TERRITORY_INTERFACE).and_then(index),
            grid,
            delayed_event: component(&DELAYED_EVENT).and_then(index),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, children: Vec<Element>) -> Element {
        Element::with_children(lowercase_crc32(name), children)
    }

    fn value(name: &str, bytes: &[u8]) -> Element {
        Element::with_value(lowercase_crc32(name), bytes.to_vec())
    }

    fn component(class: [u8; UUID_LEN], index: u32, fields: Vec<Element>) -> Element {
        let mut children = vec![named(
            "BaseClass1",
            vec![value("m_replicationIndex", &index.to_be_bytes())],
        )];
        children.extend(fields);
        Element {
            type_id: class,
            children,
            ..Element::default()
        }
    }

    /// The campfire slice's station components (`int_watchtowercampt1.dynamicslice`).
    fn campfire() -> Element {
        let side = |name: &str, number: u32| {
            named(
                name,
                vec![
                    value("m_side", &number.to_be_bytes()),
                    named(
                        "m_activeGridPoints",
                        vec![named("element", vec![value("m_active", &[0])])],
                    ),
                ],
            )
        };
        let components = vec![
            component(
                BUILDABLE_CONTROLLER,
                1,
                vec![
                    value("m_initialState", &5_u32.to_be_bytes()),
                    value("m_structureName", b"CampT1"),
                ],
            ),
            component(
                OWNERSHIP,
                9,
                vec![
                    value("m_initialPermissionRule", &1_u32.to_be_bytes()),
                    value("m_initialStructureName", b""),
                ],
            ),
            component(
                CRAFTING_STATION,
                0,
                vec![named(
                    "m_craftingStationReference",
                    vec![value("m_craftingStationEntry", b"Camp1")],
                )],
            ),
            component(
                INTERACT_NAME,
                0,
                vec![value("m_name", b"@ui_watcherscampfire")],
            ),
            component(
                PLACEMENT_OBSTRUCTION,
                8,
                vec![value(
                    "m_strFilter",
                    COMPLETION_OBSTRUCTION_FILTER.as_bytes(),
                )],
            ),
            component(TERRITORY_INTERFACE, 3, Vec::new()),
            component(
                BUILDABLE_GRID,
                7,
                vec![named(
                    "m_gridHeightsData",
                    vec![named(
                        "element",
                        vec![
                            side("m_frontSide", 1),
                            side("m_backSide", 2),
                            side("m_rightSide", 3),
                            side("m_leftSide", 4),
                        ],
                    )],
                )],
            ),
            component(DELAYED_EVENT, 6, Vec::new()),
        ];
        named("Entities", vec![named("element", components)])
    }

    #[test]
    fn the_campfire_slice_gives_its_station() {
        let station = StationSlice::of(&campfire()).expect("station");
        let sides: Vec<StationGridSide> = (1..=4)
            .map(|side| StationGridSide {
                side,
                points: 1,
                active: false,
            })
            .collect();
        assert_eq!(
            station,
            StationSlice {
                station_type: "Camp1".to_owned(),
                name: "@ui_watcherscampfire".to_owned(),
                buildable: Some((1, 5)),
                structure: "CampT1".to_owned(),
                ownership: Some((9, 1)),
                structure_name: String::new(),
                obstruction: Some((8, true)),
                territory: Some(3),
                grid: Some((7, sides)),
                delayed_event: Some(6),
            }
        );
    }

    #[test]
    fn a_slice_without_a_crafting_station_is_none() {
        let mut slice = campfire();
        slice.children[0]
            .children
            .retain(|component| component.type_id != CRAFTING_STATION);
        assert_eq!(StationSlice::of(&slice), None);
    }
}
