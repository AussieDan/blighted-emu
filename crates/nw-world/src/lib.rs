// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed New World world data (M1b spec): levels, ground height, spawn and point lists,
//! capitals, the world file, and the level collision file with its search index.

pub mod camps;
pub mod capitals;
pub mod collision;
pub mod collision_index;
pub mod dungeon_scripts;
pub mod height;
pub mod interact_tags;
pub mod level_script;
pub mod map_tiles;
pub mod metadata;
pub mod quick_courses;
pub mod records;
pub mod roads;
pub mod script;
pub mod slice_data;
pub mod spawner_slices;
pub mod stations;
pub mod travel_slices;
pub mod water;
pub mod world_file;

pub use records::WorldFile;
pub mod slice_triggers;
