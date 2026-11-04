// SPDX-License-Identifier: AGPL-3.0-or-later
//! The world layer of the map (admin tools spec section 3.2): the world file's size, ocean
//! level and spawn points with their datasheet ids, the map tile index, and a grid index for
//! box queries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use nw_common::config::ConfigError;
use nw_content::bundle::bundle_path;
use nw_content::client_build::load_client_build;
use nw_content::ids::{gatherable_ids, vitals_ids};
use nw_content::progression::Progression;
use nw_content::{ContainerError, ContentBundle, Tables};
use nw_formats::objectstream::lowercase_crc32;
use nw_world::WorldFile;
use nw_world::map_tiles::{TILES_FILE, TilesIndex};
use nw_world::script::{DesignData, design_data_path};
use nw_world::world_file::world_file_path;
use serde::Serialize;

use crate::config::AdminConfig;

/// Edge of one cell of the spawn index, in metres.
pub const SPAWN_CELL_M: f32 = 256.0;
/// Levels of the relief tile grid when no map tiles exist (as the client's map).
pub const RELIEF_LEVELS: u8 = 8;
/// Folder of the map tiles in `content_dir`.
pub const MAP_DIR: &str = "map";
const LIST_SEPARATOR: char = ',';

/// Datasheet ids by the crc32 that world file records store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnNames {
    /// Vitals ids.
    pub vitals: HashMap<u32, String>,
    /// Gatherable ids.
    pub gatherables: HashMap<u32, String>,
}

impl SpawnNames {
    /// The ids of the content tables.
    pub fn from_tables(tables: &Tables) -> Self {
        let by_crc = |ids: &mut dyn Iterator<Item = &str>| {
            ids.map(|id| (lowercase_crc32(id), id.to_owned()))
                .collect::<HashMap<_, _>>()
        };
        Self {
            vitals: by_crc(&mut vitals_ids(tables)),
            gatherables: by_crc(&mut gatherable_ids(tables)),
        }
    }
}

/// A kind of map point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnKind {
    /// Mob spawns.
    Vitals,
    /// Gatherable points.
    Gatherables,
    /// Landmarks.
    Landmarks,
}

impl SpawnKind {
    /// Kinds from a comma list (`vitals,gatherables,landmarks`).
    ///
    /// # Errors
    ///
    /// Returns [`SpawnQueryError::UnknownKind`] for a name that is not a kind.
    pub fn parse_list(text: &str) -> Result<Vec<Self>, SpawnQueryError> {
        text.split(LIST_SEPARATOR)
            .filter(|name| !name.is_empty())
            .map(|name| match name {
                "vitals" => Ok(Self::Vitals),
                "gatherables" => Ok(Self::Gatherables),
                "landmarks" => Ok(Self::Landmarks),
                other => Err(SpawnQueryError::UnknownKind(other.to_owned())),
            })
            .collect()
    }
}

/// A box of the world and the kinds wanted in it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnQuery {
    /// West edge, metres.
    pub min_x: f32,
    /// South edge, metres.
    pub min_y: f32,
    /// East edge, metres.
    pub max_x: f32,
    /// North edge, metres.
    pub max_y: f32,
    /// Kinds wanted.
    pub kinds: Vec<SpawnKind>,
}

/// A query that cannot be answered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpawnQueryError {
    /// A minimum is above its maximum.
    #[error("the box minimum is above its maximum")]
    UpsideDown,
    /// A box edge is not a finite number.
    #[error("a box edge is not finite")]
    NotFinite,
    /// A kind name is not known.
    #[error("unknown kind {0}")]
    UnknownKind(String),
}

/// One map point.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpawnPoint {
    /// Kind.
    pub kind: SpawnKind,
    /// Datasheet id (the crc32 in hexadecimal when no table names it), or the landmark text.
    pub id: String,
    /// Spawn level of a mob.
    pub level: Option<u32>,
    /// Design key of a mob spawn: the `id` of a `spawn_point` design file (content/README.md).
    pub key: Option<String>,
    /// East, metres.
    pub x: f32,
    /// North, metres.
    pub y: f32,
    /// Height, metres.
    pub z: f32,
}

/// How many points of one kind one cell of the spawn index holds: the overview of a zoomed out
/// map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpawnCell {
    /// Kind.
    pub kind: SpawnKind,
    /// Column of the cell, west to east; its west edge is `x` times [`SPAWN_CELL_M`].
    pub x: i64,
    /// Row of the cell, south to north; its south edge is `y` times [`SPAWN_CELL_M`].
    pub y: i64,
    /// Points of the kind in the cell.
    pub count: usize,
}

/// The world facts the map page needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorldInfo {
    /// The level name, as design files name it with `map`.
    pub level: String,
    /// Edge of the square world, metres.
    pub world_m: u32,
    /// Ocean height, metres.
    pub ocean_level: f32,
    /// Whether `content-tool map-tiles` wrote the map tiles.
    pub map_tiles: bool,
    /// The tile grid (of the map tiles, else of the relief).
    pub tiles: TilesIndex,
}

/// The world layer cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum WorldLayerError {
    /// The client build file cannot be read.
    #[error("client build file: {0}")]
    ClientBuild(#[source] ConfigError),
    /// The world file or the content bundle cannot be read.
    #[error(transparent)]
    Container(#[from] ContainerError),
    /// `tiles.json` cannot be read.
    #[error("{path}: {reason}")]
    Tiles {
        /// File.
        path: PathBuf,
        /// Why.
        reason: String,
    },
}

#[derive(Debug, Clone, Copy)]
enum PointRef {
    Vitals(usize),
    Gatherable(usize),
    Landmark(usize),
}

impl PointRef {
    fn kind(self) -> SpawnKind {
        match self {
            Self::Vitals(_) => SpawnKind::Vitals,
            Self::Gatherable(_) => SpawnKind::Gatherables,
            Self::Landmark(_) => SpawnKind::Landmarks,
        }
    }
}

/// The world layer the routes read; an Apply that writes new merged files puts a new one in.
#[derive(Debug, Clone)]
pub struct WorldSlot(Arc<RwLock<Arc<WorldLayer>>>);

impl WorldSlot {
    /// A slot holding `layer`.
    pub fn new(layer: WorldLayer) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(layer))))
    }

    /// The layer now in the slot.
    pub fn get(&self) -> Arc<WorldLayer> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Puts `layer` in the slot; readers that hold the old one keep it until they finish.
    pub fn set(&self, layer: WorldLayer) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(layer);
    }
}

/// The world file, its spawn names, the tile grid, and a cell index of the points.
#[derive(Debug)]
pub struct WorldLayer {
    world: WorldFile,
    names: SpawnNames,
    tiles: TilesIndex,
    map_tiles: bool,
    cells: HashMap<(i64, i64), Vec<PointRef>>,
    max_spawns: usize,
    /// The merged design data file (scripts and events), when the layer came from files.
    design_data: Option<PathBuf>,
    /// The level table and the weapon masteries, for the GM page.
    progression: Progression,
}

impl WorldLayer {
    /// A layer over `world`; `tiles` is the map tile index when the map tiles exist.
    pub fn new(
        world: WorldFile,
        names: SpawnNames,
        tiles: Option<TilesIndex>,
        max_spawns: usize,
    ) -> Self {
        let map_tiles = tiles.is_some();
        let tiles = tiles.unwrap_or_else(|| {
            TilesIndex::new(&world.level.name, world_m(&world), RELIEF_LEVELS, false)
        });
        let mut cells: HashMap<(i64, i64), Vec<PointRef>> = HashMap::new();
        let points = world
            .vitals
            .iter()
            .enumerate()
            .map(|(index, spawn)| (PointRef::Vitals(index), spawn.position))
            .chain(
                world
                    .gatherables
                    .iter()
                    .enumerate()
                    .map(|(index, point)| (PointRef::Gatherable(index), point.position)),
            )
            .chain(
                world
                    .landmarks
                    .iter()
                    .enumerate()
                    .map(|(index, landmark)| (PointRef::Landmark(index), landmark.position)),
            );
        for (point, [x, y, _]) in points {
            cells
                .entry((cell_of(x), cell_of(y)))
                .or_default()
                .push(point);
        }
        Self {
            world,
            names,
            tiles,
            map_tiles,
            cells,
            max_spawns,
            design_data: None,
            progression: Progression::default(),
        }
    }

    /// Reads the world file, the content bundle names and `tiles.json` of `config`.
    ///
    /// # Errors
    ///
    /// Returns [`WorldLayerError`] when a file cannot be read.
    pub fn load(config: &AdminConfig) -> Result<Self, WorldLayerError> {
        let build =
            load_client_build(&config.client_build_config).map_err(WorldLayerError::ClientBuild)?;
        let world = WorldFile::load(
            &world_file_path(&config.content_dir, &build, &config.level),
            &build,
        )?;
        let (names, progression) = {
            let bundle = ContentBundle::load(&bundle_path(&config.content_dir, &build), &build)?;
            (
                SpawnNames::from_tables(&bundle.tables),
                Progression::of_tables(&bundle.tables),
            )
        };
        let path = map_dir(config).join(TILES_FILE);
        let tiles =
            match std::fs::read(&path) {
                Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|error| {
                    WorldLayerError::Tiles {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(WorldLayerError::Tiles {
                        path,
                        reason: error.to_string(),
                    });
                }
            };
        Ok(Self {
            design_data: Some(design_data_path(&config.content_dir, &build)),
            progression,
            ..Self::new(world, names, tiles, config.max_spawns_per_request)
        })
    }

    /// The world facts of the map page.
    pub fn info(&self) -> WorldInfo {
        WorldInfo {
            level: self.world.level.name.clone(),
            world_m: world_m(&self.world),
            ocean_level: self.world.level.ocean_level,
            map_tiles: self.map_tiles,
            tiles: self.tiles.clone(),
        }
    }

    /// Ground height in metres at (`x`, `y`); `None` off the terrain.
    pub fn ground_height(&self, x: f32, y: f32) -> Option<f32> {
        self.world.height_at(x, y)
    }

    /// The ids of this level's events in the merged design data, read now (Apply writes it while
    /// the admin runs) off the async threads; none without the file.
    pub async fn event_ids(&self) -> Vec<String> {
        let Some(path) = self.design_data.clone() else {
            return Vec::new();
        };
        let level = self.world.level.name.clone();
        tokio::task::spawn_blocking(move || level_event_ids(&path, &level))
            .await
            .unwrap_or_default()
    }

    /// The level table and the weapon masteries.
    pub fn progression(&self) -> &Progression {
        &self.progression
    }

    /// The tile grid.
    pub fn tiles(&self) -> &TilesIndex {
        &self.tiles
    }

    /// The world file.
    pub fn world(&self) -> &WorldFile {
        &self.world
    }

    /// The points of the wanted kinds in the box, at most the configured count, west to east in
    /// cell rows from south to north.
    ///
    /// # Errors
    ///
    /// Returns [`SpawnQueryError`] for a box that is upside down or not finite.
    pub fn spawns(&self, query: &SpawnQuery) -> Result<Vec<SpawnPoint>, SpawnQueryError> {
        let edges = [query.min_x, query.min_y, query.max_x, query.max_y];
        if !edges.iter().all(|edge| edge.is_finite()) {
            return Err(SpawnQueryError::NotFinite);
        }
        if query.min_x > query.max_x || query.min_y > query.max_y {
            return Err(SpawnQueryError::UpsideDown);
        }
        let world = world_m(&self.world) as f32;
        let (min_x, max_x) = (query.min_x.max(0.0), query.max_x.min(world));
        let (min_y, max_y) = (query.min_y.max(0.0), query.max_y.min(world));
        let mut points = Vec::new();
        if min_x > max_x || min_y > max_y {
            return Ok(points);
        }
        for cell_y in cell_of(min_y)..=cell_of(max_y) {
            for cell_x in cell_of(min_x)..=cell_of(max_x) {
                for point in self.cells.get(&(cell_x, cell_y)).into_iter().flatten() {
                    let Some(found) = self.point(*point, &query.kinds) else {
                        continue;
                    };
                    let inside = (query.min_x..=query.max_x).contains(&found.x)
                        && (query.min_y..=query.max_y).contains(&found.y);
                    if inside {
                        points.push(found);
                        if points.len() >= self.max_spawns {
                            return Ok(points);
                        }
                    }
                }
            }
        }
        Ok(points)
    }

    /// The point count of each wanted kind in each cell of the spawn index, west to east in cell
    /// rows from south to north, kinds in the order of `kinds`.
    pub fn spawn_cells(&self, kinds: &[SpawnKind]) -> Vec<SpawnCell> {
        let mut cells: Vec<SpawnCell> = self
            .cells
            .iter()
            .flat_map(|(&(x, y), points)| {
                kinds.iter().filter_map(move |&kind| {
                    let count = points.iter().filter(|point| point.kind() == kind).count();
                    (count > 0).then_some(SpawnCell { kind, x, y, count })
                })
            })
            .collect();
        let rank = |kind: SpawnKind| kinds.iter().position(|wanted| *wanted == kind);
        cells.sort_by_key(|cell| (cell.y, cell.x, rank(cell.kind)));
        cells
    }

    fn point(&self, point: PointRef, kinds: &[SpawnKind]) -> Option<SpawnPoint> {
        let named = |names: &HashMap<u32, String>, crc: u32| {
            names
                .get(&crc)
                .cloned()
                .unwrap_or_else(|| format!("{crc:08x}"))
        };
        let (kind, id, level, key, [x, y, z]) = match point {
            PointRef::Vitals(index) => {
                let spawn = &self.world.vitals[index];
                (
                    SpawnKind::Vitals,
                    named(&self.names.vitals, spawn.vitals_id),
                    Some(spawn.level),
                    self.world.vitals_keys.get(index).cloned(),
                    spawn.position,
                )
            }
            PointRef::Gatherable(index) => {
                let gatherable = &self.world.gatherables[index];
                (
                    SpawnKind::Gatherables,
                    named(&self.names.gatherables, gatherable.gatherable_id),
                    None,
                    None,
                    gatherable.position,
                )
            }
            PointRef::Landmark(index) => {
                let landmark = &self.world.landmarks[index];
                (
                    SpawnKind::Landmarks,
                    landmark.landmark_data.clone(),
                    None,
                    None,
                    landmark.position,
                )
            }
        };
        kinds.contains(&kind).then_some(SpawnPoint {
            kind,
            id,
            level,
            key,
            x,
            y,
            z,
        })
    }
}

/// The map tile folder of `config`'s level.
pub fn map_dir(config: &AdminConfig) -> PathBuf {
    config.content_dir.join(MAP_DIR).join(&config.level)
}

/// Edge of the square world: the region size times the larger grid side.
fn world_m(world: &WorldFile) -> u32 {
    world
        .level
        .region_size
        .saturating_mul(world.level.grid_width.max(world.level.grid_height))
}

fn cell_of(metres: f32) -> i64 {
    (metres / SPAWN_CELL_M).floor() as i64
}

/// The event ids of `level` in the design data file `path`; none when it cannot be read.
fn level_event_ids(path: &Path, level: &str) -> Vec<String> {
    match DesignData::load(path) {
        Ok(mut data) => data
            .levels
            .remove(level)
            .map(|level| level.events.into_keys().collect())
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "design data");
            Vec::new()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use nw_formats::objectstream::lowercase_crc32;
    use nw_world::records::{
        GatherablePoint, HeightGrid, Landmark, LevelInfo, Region, VitalsSpawn,
    };

    use super::*;

    pub(crate) const REGION_M: u32 = 2_048;
    pub(crate) const OCEAN_M: f32 = 10.0;
    /// Raw height of the south-east quarter: 100 m.
    const LAND_RAW: u16 = 3_200;
    /// Raw height of the north-east quarter: 200 m.
    const HIGH_RAW: u16 = 6_400;
    const VITALS_ID: &str = "Boar_20";
    const GATHERABLE_ID: &str = "IronVein";
    const LEVEL: u32 = 20;
    const CAP: usize = 5_000;

    /// One region: the west half under the ocean (0 m), the east half land, 200 m in the north
    /// and 100 m in the south; one vitals spawn, one gatherable and one landmark.
    pub(crate) fn test_world() -> WorldFile {
        let side = REGION_M as usize;
        // Sample row 0 is the north edge.
        let samples = (0..side * side)
            .map(
                |index| match (index % side < side / 2, index / side < side / 2) {
                    (true, _) => 0,
                    (false, true) => HIGH_RAW,
                    (false, false) => LAND_RAW,
                },
            )
            .collect();
        let mut world = WorldFile {
            level: LevelInfo {
                name: "test".to_owned(),
                region_size: REGION_M,
                grid_width: 1,
                grid_height: 1,
                ocean_level: OCEAN_M,
                has_terrain: true,
            },
            regions: vec![Region {
                x: 0,
                y: 0,
                heights: Some(HeightGrid { samples }),
            }],
            vitals: vec![VitalsSpawn {
                vitals_id: lowercase_crc32(VITALS_ID),
                level: LEVEL,
                position: [100.0, 100.0, 5.0],
                ..VitalsSpawn::default()
            }],
            gatherables: vec![GatherablePoint {
                gatherable_id: lowercase_crc32(GATHERABLE_ID),
                position: [1_500.0, 200.0, 100.0],
                ..GatherablePoint::default()
            }],
            landmarks: vec![Landmark {
                landmark_data: "Shrine".to_owned(),
                position: [1_500.0, 1_500.0, 100.0],
                ..Landmark::default()
            }],
            ..WorldFile::default()
        };
        world.fill_point_keys();
        world
    }

    pub(crate) fn test_names() -> SpawnNames {
        SpawnNames {
            vitals: HashMap::from([(lowercase_crc32(VITALS_ID), VITALS_ID.to_owned())]),
            gatherables: HashMap::from([(
                lowercase_crc32(GATHERABLE_ID),
                GATHERABLE_ID.to_owned(),
            )]),
        }
    }

    pub(crate) fn test_layer(cap: usize) -> WorldLayer {
        WorldLayer::new(test_world(), test_names(), None, cap)
    }

    fn query(min: (f32, f32), max: (f32, f32), kinds: &[SpawnKind]) -> SpawnQuery {
        SpawnQuery {
            min_x: min.0,
            min_y: min.1,
            max_x: max.0,
            max_y: max.1,
            kinds: kinds.to_vec(),
        }
    }

    const ALL: [SpawnKind; 3] = [
        SpawnKind::Vitals,
        SpawnKind::Gatherables,
        SpawnKind::Landmarks,
    ];

    #[test]
    fn info_gives_the_world_size() {
        let info = test_layer(CAP).info();
        assert_eq!((info.world_m, info.ocean_level), (REGION_M, OCEAN_M));
        assert_eq!(info.level, "test");
        assert!(!info.map_tiles);
        assert_eq!(info.tiles.world_m, REGION_M);
    }

    #[test]
    fn spawns_in_a_box_have_their_names() {
        let points = test_layer(CAP)
            .spawns(&query((0.0, 0.0), (2_048.0, 2_048.0), &ALL))
            .expect("query");
        let found: Vec<(SpawnKind, &str, Option<u32>)> = points
            .iter()
            .map(|point| (point.kind, point.id.as_str(), point.level))
            .collect();
        assert_eq!(
            found,
            vec![
                (SpawnKind::Vitals, VITALS_ID, Some(LEVEL)),
                (SpawnKind::Gatherables, GATHERABLE_ID, None),
                (SpawnKind::Landmarks, "Shrine", None),
            ]
        );
    }

    #[test]
    fn a_mob_spawn_has_its_design_key() {
        let points = test_layer(CAP)
            .spawns(&query((0.0, 0.0), (2_048.0, 2_048.0), &ALL))
            .expect("query");
        let keys: Vec<Option<&str>> = points.iter().map(|point| point.key.as_deref()).collect();
        let expected = format!("{}_100.0_100.0_5.0", lowercase_crc32(VITALS_ID));
        assert_eq!(keys, [Some(expected.as_str()), None, None]);
    }

    #[test]
    fn spawns_of_other_kinds_and_places_are_left_out() {
        let layer = test_layer(CAP);
        let points = layer
            .spawns(&query(
                (0.0, 0.0),
                (2_048.0, 2_048.0),
                &[SpawnKind::Gatherables],
            ))
            .expect("query");
        assert_eq!(points.len(), 1);
        let points = layer
            .spawns(&query((0.0, 0.0), (500.0, 500.0), &ALL))
            .expect("query");
        assert_eq!(points.len(), 1, "only the vitals spawn");
    }

    #[test]
    fn spawn_cells_count_each_kind_per_cell() {
        let cells = test_layer(CAP).spawn_cells(&ALL);
        let vitals_cell = cell_of(100.0);
        let gatherable_cell = (cell_of(1_500.0), cell_of(200.0));
        assert!(cells.contains(&SpawnCell {
            kind: SpawnKind::Vitals,
            x: vitals_cell,
            y: vitals_cell,
            count: 1,
        }));
        assert!(cells.contains(&SpawnCell {
            kind: SpawnKind::Gatherables,
            x: gatherable_cell.0,
            y: gatherable_cell.1,
            count: 1,
        }));
        assert_eq!(cells.iter().map(|cell| cell.count).sum::<usize>(), 3);
    }

    #[test]
    fn spawn_cells_leave_out_unwanted_kinds() {
        let cells = test_layer(CAP).spawn_cells(&[SpawnKind::Landmarks]);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].kind, SpawnKind::Landmarks);
    }

    #[test]
    fn spawns_are_capped() {
        let points = test_layer(1)
            .spawns(&query((0.0, 0.0), (2_048.0, 2_048.0), &ALL))
            .expect("query");
        assert_eq!(points.len(), 1);
    }

    #[test]
    fn a_box_outside_the_world_is_empty() {
        let points = test_layer(CAP)
            .spawns(&query((-9_000.0, -9_000.0), (-8_000.0, -8_000.0), &ALL))
            .expect("query");
        assert!(points.is_empty());
        let points = test_layer(CAP)
            .spawns(&query((1.0e9, 1.0e9), (2.0e9, 2.0e9), &ALL))
            .expect("query");
        assert!(points.is_empty());
    }

    #[test]
    fn an_upside_down_box_is_a_bad_request() {
        assert_eq!(
            test_layer(CAP).spawns(&query((10.0, 10.0), (0.0, 20.0), &ALL)),
            Err(SpawnQueryError::UpsideDown)
        );
        assert_eq!(
            test_layer(CAP).spawns(&query((f32::NAN, 0.0), (1.0, 1.0), &ALL)),
            Err(SpawnQueryError::NotFinite)
        );
    }

    #[test]
    fn kinds_parse_from_a_list() {
        assert_eq!(
            SpawnKind::parse_list("vitals,landmarks"),
            Ok(vec![SpawnKind::Vitals, SpawnKind::Landmarks])
        );
        assert_eq!(
            SpawnKind::parse_list("vitals,boats"),
            Err(SpawnQueryError::UnknownKind("boats".to_owned()))
        );
    }
}
