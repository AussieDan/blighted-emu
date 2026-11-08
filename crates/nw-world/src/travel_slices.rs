// SPDX-License-Identifier: AGPL-3.0-or-later
//! The travel places of a level that slices hold (run 15 travel2 study,
//! `local-data/scratch-work/run15/travel2/housing` and `teleport`):
//!
//! - A house plot is a spawned slice (`slices/housing/plots/<town>/city?_house_address_NNN`)
//!   whose `HousingPlotComponent` names its house type (`m_houseTypeString`, a `HouseTypeData`
//!   id). The 208 plots of `newworld_vitaeeterna` matched all 8 retail owned houses of the
//!   captures (0.00 to 0.01 m from the replicated house position, the same house type CRC).
//! - A teleporter is a capital whose slice has an `InteractTeleportComponent`: the destination is
//!   an entity of the same slice (`m_entityToTeleportTo`), placed by its `m_localTM` up its
//!   `m_parentId` chain, then by the capital's transform. All 77 retail kind-82 interactions on
//!   capitals landed there (the camera point 1.67 m above it and behind it by its boom).

use nw_formats::objectstream::{Element, lowercase_crc32};
use serde::{Deserialize, Serialize};

use crate::records::{Capital, Position};

/// `Javelin::HousingPlotComponent`.
const HOUSING_PLOT: [u8; UUID_LEN] = uuid(0xd578_bbb4_7e11_4c5a_8352_a95b_f5a6_cfd5);
/// `Javelin::InteractTeleportComponent`.
const INTERACT_TELEPORT: [u8; UUID_LEN] = uuid(0xa149_ec6d_cf0f_4051_a244_3e07_d7c4_b605);
/// `AZ::Entity`.
pub(crate) const ENTITY: [u8; UUID_LEN] = uuid(0x7565_1658_8663_478d_9090_2432_dfca_fa44);
/// Floats of a `m_localTM`: three basis columns, then the translation.
const TRANSFORM_FLOATS: usize = 12;
/// Bytes of one float.
const FLOAT_LEN: usize = 4;
/// The first float of the translation in a `m_localTM`.
pub(crate) const TRANSLATION_AT: usize = 9;
/// The first float of the +Y basis column (the facing).
const FORWARD_AT: usize = 3;
/// Most parents followed up a destination's chain (a slice is far shallower).
const MAX_CHAIN: usize = 64;
pub(crate) const UUID_LEN: usize = 16;
pub(crate) const AXES: usize = 3;

pub(crate) const fn uuid(value: u128) -> [u8; UUID_LEN] {
    value.to_be_bytes()
}

/// A house plot of the level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HousingPlot {
    /// Its GDE id (its spawned slice's).
    pub id: u64,
    /// Its spawned slice's path.
    pub slice: String,
    /// Its world position.
    pub position: Position,
    /// Its `HouseTypeData` id (`HouseTypeTier2`).
    pub house_type: String,
}

/// A teleporter of the level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Teleporter {
    /// Its capital's GUID text.
    pub capital: String,
    /// Where it puts the player.
    pub destination: Position,
    /// The facing there, radians about +z (0 faces +y).
    pub yaw_rad: f32,
    /// `m_channelDuration`: 1 in every retail teleporter answered after 1 s, 0 in every one
    /// answered after the fast travel channel (10 s).
    pub channel: u32,
}

/// The names read, as their name CRCs.
struct Names {
    house_type: u32,
    destination: u32,
    channel: u32,
    id: u32,
    local: u32,
    parent: u32,
}

impl Names {
    fn new() -> Self {
        Self {
            house_type: lowercase_crc32("m_houseTypeString"),
            destination: lowercase_crc32("m_entityToTeleportTo"),
            channel: lowercase_crc32("m_channelDuration"),
            id: lowercase_crc32("Id"),
            local: lowercase_crc32("m_localTM"),
            parent: lowercase_crc32("m_parentId"),
        }
    }
}

/// Every element under `root` (itself first), depth first.
pub(crate) fn descendants(root: &Element) -> Vec<&Element> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(element) = stack.pop() {
        found.push(element);
        stack.extend(element.children.iter().rev());
    }
    found
}

fn find(root: &Element, name: u32) -> Option<&Element> {
    descendants(root)
        .into_iter()
        .find(|element| element.name_crc == Some(name))
}

/// The house type of a plot's dynamic slice; `None` for a slice without a plot component.
pub fn plot_house_type(root: &Element) -> Option<String> {
    let names = Names::new();
    let plot = descendants(root)
        .into_iter()
        .find(|element| element.type_id == HOUSING_PLOT)?;
    let house_type = find(plot, names.house_type)?.as_string().ok()?;
    (!house_type.is_empty()).then(|| house_type.to_owned())
}

/// A 3x4 transform: three basis columns, then the translation.
pub(crate) type Transform = [f32; TRANSFORM_FLOATS];

pub(crate) const IDENTITY: Transform = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

/// `transform` applied to point `point`.
fn apply(transform: &Transform, point: [f32; AXES]) -> [f32; AXES] {
    std::array::from_fn(|axis| {
        transform[axis] * point[0]
            + transform[AXES + axis] * point[1]
            + transform[2 * AXES + axis] * point[2]
            + transform[TRANSLATION_AT + axis]
    })
}

/// `outer` after `inner`.
pub(crate) fn compose(outer: &Transform, inner: &Transform) -> Transform {
    let linear = |column: [f32; AXES]| {
        let mut moved = apply(outer, column);
        for (axis, value) in moved.iter_mut().enumerate() {
            *value -= outer[TRANSLATION_AT + axis];
        }
        moved
    };
    let mut composed = [0.0; TRANSFORM_FLOATS];
    for column in 0..AXES {
        let at = column * AXES;
        composed[at..at + AXES].copy_from_slice(&linear([inner[at], inner[at + 1], inner[at + 2]]));
    }
    let translation = apply(
        outer,
        [
            inner[TRANSLATION_AT],
            inner[TRANSLATION_AT + 1],
            inner[TRANSLATION_AT + 2],
        ],
    );
    composed[TRANSLATION_AT..].copy_from_slice(&translation);
    composed
}

/// The `u64` of an entity id element (its inner `Id`).
fn entity_id(element: Option<&Element>, names: &Names) -> Option<u64> {
    element?.child(names.id)?.as_u64().ok()
}

/// An entity's `m_localTM` and its `m_parentId`.
fn local_transform(entity: &Element, names: &Names) -> Option<(Transform, Option<u64>)> {
    descendants(entity).into_iter().find_map(|element| {
        let local = element.child(names.local)?;
        let bytes: &[u8; TRANSFORM_FLOATS * FLOAT_LEN] = local.value.as_slice().try_into().ok()?;
        let transform = std::array::from_fn(|index| {
            let at = index * FLOAT_LEN;
            f32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        });
        Some((transform, entity_id(element.child(names.parent), names)))
    })
}

/// The entities of a slice by their id.
pub(crate) struct SliceEntities<'a> {
    entities: Vec<(u64, &'a Element)>,
    names: Names,
}

impl<'a> SliceEntities<'a> {
    /// The `AZ::Entity` elements among `all`.
    pub(crate) fn of(all: &[&'a Element]) -> Self {
        let names = Names::new();
        let entities = all
            .iter()
            .copied()
            .filter(|element| element.type_id == ENTITY)
            .filter_map(|entity| Some((entity_id(entity.child(names.id), &names)?, entity)))
            .collect();
        Self { entities, names }
    }

    /// The entity of id `id`.
    pub(crate) fn get(&self, id: u64) -> Option<&'a Element> {
        self.entities
            .iter()
            .find(|(entity_id, _)| *entity_id == id)
            .map(|(_, entity)| *entity)
    }

    /// Every entity with its id.
    pub(crate) fn all(&self) -> impl Iterator<Item = (u64, &'a Element)> + '_ {
        self.entities.iter().copied()
    }

    /// The transform of entity `id` in the slice's frame: its `m_localTM` up its `m_parentId`
    /// chain.
    pub(crate) fn placed(&self, id: u64) -> Transform {
        let mut placed = IDENTITY;
        let mut next = Some(id);
        for _ in 0..MAX_CHAIN {
            let Some((transform, parent)) = next
                .and_then(|id| self.get(id))
                .and_then(|entity| local_transform(entity, &self.names))
            else {
                break;
            };
            placed = compose(&transform, &placed);
            next = parent.filter(|parent| Some(*parent) != next);
        }
        placed
    }
}

/// The point `local` of a slice placed at `capital` (its scale, rotation and position).
pub(crate) fn capital_point(capital: &Capital, local: [f32; AXES]) -> [f32; AXES] {
    let scale = capital.scale.unwrap_or(1.0);
    let moved = rotate(capital.rotation, local.map(|value| value * scale));
    std::array::from_fn(|axis| capital.position[axis] + moved[axis])
}

/// What a teleporter's dynamic slice gives: its channel and its destination in the slice's
/// frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeleporterSlice {
    /// `m_channelDuration`.
    pub channel: u32,
    /// The destination entity's transform in the slice's frame.
    pub destination: [f32; TRANSFORM_FLOATS],
}

impl TeleporterSlice {
    /// The teleporter of the dynamic slice whose root element is `root`; `None` for a slice
    /// without an `InteractTeleportComponent` or whose destination entity is missing.
    pub fn of(root: &Element) -> Option<Self> {
        let names = Names::new();
        let all = descendants(root);
        let component = all
            .iter()
            .copied()
            .find(|element| element.type_id == INTERACT_TELEPORT)?;
        let channel = find(component, names.channel)
            .and_then(|channel| channel.as_u32().ok())
            .unwrap_or_default();
        let target = entity_id(find(component, names.destination), &names)?;
        let entities = SliceEntities::of(&all);
        entities.get(target)?;
        Some(Self {
            channel,
            destination: entities.placed(target),
        })
    }

    /// The teleporter this slice makes at `capital`.
    pub fn at(&self, capital: &Capital) -> Teleporter {
        let destination = capital_point(
            capital,
            [
                self.destination[TRANSLATION_AT],
                self.destination[TRANSLATION_AT + 1],
                self.destination[TRANSLATION_AT + 2],
            ],
        );
        let forward = rotate(
            capital.rotation,
            [
                self.destination[FORWARD_AT],
                self.destination[FORWARD_AT + 1],
                self.destination[FORWARD_AT + 2],
            ],
        );
        Teleporter {
            capital: capital.id.clone(),
            destination,
            yaw_rad: (-forward[0]).atan2(forward[1]),
            channel: self.channel,
        }
    }
}

/// `vector` rotated by the unit quaternion `rotation` (x, y, z, w).
pub fn rotate(rotation: [f32; 4], vector: [f32; AXES]) -> [f32; AXES] {
    let [x, y, z, w] = rotation;
    let [ux, uy, uz] = vector;
    let (tx, ty, tz) = (
        2.0 * (y * uz - z * uy),
        2.0 * (z * ux - x * uz),
        2.0 * (x * uy - y * ux),
    );
    [
        ux + w * tx + y * tz - z * ty,
        uy + w * ty + z * tx - x * tz,
        uz + w * tz + x * ty - y * tx,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-4;

    fn named(name: &str, value: Vec<u8>) -> Element {
        Element::with_value(lowercase_crc32(name), value)
    }

    fn typed(type_id: [u8; UUID_LEN], children: Vec<Element>) -> Element {
        Element {
            type_id,
            ..Element::with_children(0, children)
        }
    }

    fn id_element(name: &str, id: u64) -> Element {
        Element::with_children(
            lowercase_crc32(name),
            vec![named("Id", id.to_be_bytes().to_vec())],
        )
    }

    fn transform(local: Transform, parent: Option<u64>) -> Element {
        let bytes = local.iter().flat_map(|value| value.to_be_bytes()).collect();
        let mut children = vec![named("m_localTM", bytes)];
        children.extend(parent.map(|parent| id_element("m_parentId", parent)));
        Element::with_children(0, children)
    }

    fn entity(id: u64, local: Transform, parent: Option<u64>) -> Element {
        typed(ENTITY, vec![id_element("Id", id), transform(local, parent)])
    }

    /// A translation by `at`.
    fn moved(at: [f32; AXES]) -> Transform {
        let mut transform = IDENTITY;
        transform[TRANSLATION_AT..].copy_from_slice(&at);
        transform
    }

    #[test]
    fn a_plot_names_its_house_type() {
        let root = Element::with_children(
            0,
            vec![typed(
                HOUSING_PLOT,
                vec![named("m_houseTypeString", b"HouseTypeTier2".to_vec())],
            )],
        );
        assert_eq!(plot_house_type(&root).as_deref(), Some("HouseTypeTier2"));
        assert_eq!(
            plot_house_type(&Element::with_children(0, Vec::new())),
            None
        );
    }

    #[test]
    fn a_teleporter_goes_to_its_destination_up_the_parent_chain_and_the_capital() {
        const ROOT: u64 = 1;
        const SPAWN: u64 = 2;
        // The retail reekwater teleporter's spawn point offset (teleport study).
        let offset = [-1.947, 0.317, -27.503];
        let root = Element::with_children(
            0,
            vec![
                entity(ROOT, moved([10.0, 0.0, 0.0]), None),
                entity(SPAWN, moved(offset), Some(ROOT)),
                typed(
                    INTERACT_TELEPORT,
                    vec![
                        named("m_channelDuration", 1_u32.to_be_bytes().to_vec()),
                        Element::with_children(
                            lowercase_crc32("m_serverFacetPtr"),
                            vec![id_element("m_entityToTeleportTo", SPAWN)],
                        ),
                    ],
                ),
            ],
        );
        let slice = TeleporterSlice::of(&root).expect("a teleporter");
        assert_eq!(slice.channel, 1);
        // A quarter turn about +z: +x goes to +y.
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let capital = Capital {
            id: "95e1bfbb-0590-7cd2-cdf7-3f8c5b976652".to_owned(),
            position: [3_000.0, 6_000.0, 100.0],
            rotation: [0.0, 0.0, half, half],
            scale: None,
            slice_name: String::new(),
            variant_name: None,
            slice_asset_id: String::new(),
        };
        let teleporter = slice.at(&capital);
        let expected = [
            3_000.0 - offset[1],
            6_000.0 + 10.0 + offset[0],
            100.0 + offset[2],
        ];
        for (found, wanted) in teleporter.destination.iter().zip(expected) {
            assert!((found - wanted).abs() < EPSILON);
        }
        assert!((teleporter.yaw_rad - std::f32::consts::FRAC_PI_2).abs() < EPSILON);
        assert_eq!(teleporter.capital, capital.id);
        let without = Element::with_children(0, vec![entity(ROOT, IDENTITY, None)]);
        assert!(TeleporterSlice::of(&without).is_none());
    }
}
