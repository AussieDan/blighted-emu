// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quick course paths (mount races, rallies, tours and time trials; quick course note
//! 2026-10-30, `local-data/findings`): the course slices under [`QUICK_COURSE_FOLDERS`] hold one
//! entity per course with a `QuickCoursePathComponent`, whose `m_quickCoursePathCommon` names
//! the course (`m_quickCourseId`) and lists its sections in order (`m_sections`), each with its
//! primary node (`m_primaryNode`: `m_worldPos` in world metres and `m_nodeTypeId`, a
//! `QuickCourseNodeTypeData` row: `Start`, `Checkpoint45`, `Finish`).
//!
//! **Observed** (client `FUN_140d02880`, `FUN_140d2bef0`, `FUN_140d2c310` and `FUN_140d2bab0`
//! reflect those names; retail `ef_tour_0-Hermit-s-Rally`: `EF_Tour_0`'s Start at (8921.0,
//! 5130.3, 151.1), six `Checkpoint45` and its Finish at (9492.9, 4260.6, 110.4), and the
//! player's section count rose as it rode past them in that order).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nw_formats::objectstream::{Element, lowercase_crc32};
use serde::{Deserialize, Serialize};

/// The prefix of the file name.
pub const QUICK_COURSES_PREFIX: &str = "quick-courses-";
/// The extension of the file name.
pub const QUICK_COURSES_EXTENSION: &str = ".json";
/// The pak folders of the course slices (`slices/quickcourses/ef_races`,
/// `slices/mountraces/dw_races`).
pub const QUICK_COURSE_FOLDERS: [&str; 2] = ["slices/quickcourses/", "slices/mountraces/"];

/// One node a course's rider passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickCourseNode {
    /// `m_worldPos`.
    pub position: [f32; 3],
    /// `m_nodeTypeId`.
    pub node_type: String,
}

/// One course's path: its nodes in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickCoursePath {
    /// `m_quickCourseId`.
    pub course: String,
    /// The primary node of each section, in order.
    pub nodes: Vec<QuickCourseNode>,
}

impl QuickCoursePath {
    /// The paths of the course slice `root`: each element with an `m_quickCoursePathCommon`.
    pub fn all_of(root: &Element) -> Vec<Self> {
        let common = lowercase_crc32("m_quickCoursePathCommon");
        let mut paths = Vec::new();
        collect(root, common, &mut paths);
        paths
    }

    /// The path of an `m_quickCoursePathCommon` element; `None` without a course id or nodes.
    fn of(common: &Element) -> Option<Self> {
        let course = common
            .child(lowercase_crc32("m_quickCourseId"))?
            .as_string()
            .ok()?
            .to_owned();
        let sections = common.child(lowercase_crc32("m_sections"))?;
        let primary = lowercase_crc32("m_primaryNode");
        let position = lowercase_crc32("m_worldPos");
        let node_type = lowercase_crc32("m_nodeTypeId");
        let nodes: Vec<QuickCourseNode> = sections
            .children
            .iter()
            .filter_map(|section| {
                let node = section.child(primary)?;
                Some(QuickCourseNode {
                    position: node.child(position)?.as_vec3().ok()?,
                    node_type: node.child(node_type)?.as_string().ok()?.to_owned(),
                })
            })
            .collect();
        (!course.is_empty() && !nodes.is_empty()).then_some(Self { course, nodes })
    }
}

fn collect(element: &Element, common: u32, paths: &mut Vec<QuickCoursePath>) {
    if let Some(found) = element.child(common) {
        paths.extend(QuickCoursePath::of(found));
        return;
    }
    for child in &element.children {
        collect(child, common, paths);
    }
}

/// The course paths, by lowercase course id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickCoursePaths {
    /// The paths by lowercase course id.
    pub by_course: BTreeMap<String, QuickCoursePath>,
}

/// Why the file cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum QuickCoursesError {
    /// The file cannot be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file is not course paths.
    #[error("{path}: {source}")]
    Json {
        /// The file.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
}

/// The course path file of `client_build` in `dir`.
pub fn quick_courses_path(dir: &Path, client_build: &str) -> PathBuf {
    dir.join(format!(
        "{QUICK_COURSES_PREFIX}{client_build}{QUICK_COURSES_EXTENSION}"
    ))
}

impl QuickCoursePaths {
    /// The table of `paths`; the first path of a course wins (`EF_Tour_0` is in two slices).
    pub fn of(paths: impl IntoIterator<Item = QuickCoursePath>) -> Self {
        let mut by_course = BTreeMap::new();
        for path in paths {
            by_course.entry(path.course.to_lowercase()).or_insert(path);
        }
        Self { by_course }
    }

    /// Reads `path`; a missing file has no courses (content from before this file).
    ///
    /// # Errors
    ///
    /// Returns [`QuickCoursesError`] when the file cannot be read or is not course paths.
    pub fn load(path: &Path) -> Result<Self, QuickCoursesError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| QuickCoursesError::Json {
                path: path.to_path_buf(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(QuickCoursesError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Writes the file to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`QuickCoursesError`] when a value cannot be written or the file cannot be.
    pub fn write(&self, path: &Path) -> Result<(), QuickCoursesError> {
        let text =
            serde_json::to_string_pretty(self).map_err(|source| QuickCoursesError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        std::fs::write(path, text).map_err(|source| QuickCoursesError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The path of course `course` (any case).
    pub fn get(&self, course: &str) -> Option<&QuickCoursePath> {
        self.by_course.get(&course.to_lowercase())
    }

    /// Courses in the table.
    pub fn len(&self) -> usize {
        self.by_course.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_course.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, value: Vec<u8>) -> Element {
        Element::with_value(lowercase_crc32(name), value)
    }

    fn node(position: [f32; 3], node_type: &str) -> Element {
        let bytes: Vec<u8> = position
            .iter()
            .flat_map(|axis| axis.to_be_bytes())
            .collect();
        Element::with_children(
            0,
            vec![Element::with_children(
                lowercase_crc32("m_primaryNode"),
                vec![
                    named("m_worldPos", bytes),
                    named("m_nodeTypeId", node_type.as_bytes().to_vec()),
                ],
            )],
        )
    }

    #[test]
    fn a_course_slice_gives_its_course_and_its_nodes_in_order() {
        let common = Element::with_children(
            lowercase_crc32("m_quickCoursePathCommon"),
            vec![
                named("m_quickCourseId", b"EF_Tour_0".to_vec()),
                Element::with_children(
                    lowercase_crc32("m_sections"),
                    vec![
                        node([8921.0, 5130.3, 151.1], "Start"),
                        node([8957.0, 5004.0, 150.0], "Checkpoint45"),
                        node([9492.9, 4260.6, 110.4], "Finish"),
                    ],
                ),
            ],
        );
        let root = Element::with_children(0, vec![Element::with_children(0, vec![common])]);
        let paths = QuickCoursePath::all_of(&root);
        let [path] = paths.as_slice() else {
            panic!("one course: {paths:?}");
        };
        assert_eq!(path.course, "EF_Tour_0");
        let types: Vec<&str> = path
            .nodes
            .iter()
            .map(|node| node.node_type.as_str())
            .collect();
        assert_eq!(types, ["Start", "Checkpoint45", "Finish"]);
        assert_eq!(path.nodes[0].position, [8921.0, 5130.3, 151.1]);
        let table = QuickCoursePaths::of(paths.clone().into_iter().chain(paths));
        assert_eq!(table.len(), 1);
        assert!(table.get("ef_tour_0").is_some());
    }
}
