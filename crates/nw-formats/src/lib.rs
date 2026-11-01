// SPDX-License-Identifier: MIT OR Apache-2.0
//! Readers for New World client file formats: pak archives, the asset catalog, datasheets,
//! localization files, AZCS wrappers, binary ObjectStreams, Mannequin and CAGE files, region
//! heightmaps, the animation chunk files (clips, skeletons, character definitions) and the physics
//! assets (`.rnr`).

pub mod asset_catalog;
pub mod azcs;
pub mod caf;
pub mod cgf;
pub mod character;
pub mod chunk_file;
pub mod datasheet;
pub mod dds;
pub mod heightmap;
pub mod input_profile;
pub mod loc;
#[cfg(feature = "local-data")]
pub mod local_data;
pub mod mannequin;
pub mod objectstream;
pub mod pak;
pub mod png_image;
pub mod rigid;
pub mod rnr;
pub mod skeleton;
