// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quick course rules (`javelindata_quickcourses`, `javelindata_quickcoursenodetypes`): per
//! course whether it is timed, its `StartingTimerSeconds`, the course whose path it rides
//! (`PathReferenceQuickCourseId`: `EF_Rally_3` rides `EF_Rally_1`'s) and its
//! `NodeTimeOverrideMultiplier`; per node type its `AddTimeSeconds` and `DetectionRadius`.
//!
//! **Observed** (retail `ef_tour_0-Hermit-s-Rally`, quick course note 2026-10-30): `EF_Tour_0`
//! (`StartingTimerSeconds` 30, a `Start` of 35 s, six `Checkpoint45`) was ridden in segments of
//! 15.8 to 39.5 s, each under its node's time. **Inferred**: a timed course gives its starting
//! time when it starts and each node hit adds its node's time times the multiplier; a course
//! without a starting time runs its clock from its first node. **Unknown**: what
//! `AccumulateTime` and the node types' `UseTimeOverride` change (not applied).

use std::collections::HashMap;

use crate::Tables;

/// One course's rules.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickCourse {
    /// `QuickCourseId`.
    pub id: String,
    /// `IsTimed`.
    pub timed: bool,
    /// `StartingTimerSeconds`.
    pub starting_seconds: Option<u32>,
    /// `PathReferenceQuickCourseId`: the course whose path this one rides.
    pub path: Option<String>,
    /// `NodeTimeOverrideMultiplier`; 1 without one.
    pub node_time_multiplier: f64,
}

/// One node type's rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuickCourseNodeType {
    /// `AddTimeSeconds`.
    pub add_seconds: u32,
    /// `DetectionRadius`, metres.
    pub radius_m: f32,
}

/// The courses and node types, by lowercase id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuickCourseRules {
    courses: HashMap<String, QuickCourse>,
    node_types: HashMap<String, QuickCourseNodeType>,
}

/// The multiplier of a course without `NodeTimeOverrideMultiplier`.
const PLAIN_MULTIPLIER: f64 = 1.0;
/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1_000.0;

impl QuickCourseRules {
    /// The rules of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let positive = |cell: Option<i64>| cell.and_then(|value| u32::try_from(value).ok());
        Self::build(
            tables
                .quick_course_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| QuickCourse {
                    id: row.quick_course_id.clone(),
                    timed: row.is_timed,
                    starting_seconds: positive(row.starting_timer_seconds),
                    path: row
                        .path_reference_quick_course_id
                        .clone()
                        .filter(|path| !path.is_empty()),
                    node_time_multiplier: row
                        .node_time_override_multiplier
                        .unwrap_or(PLAIN_MULTIPLIER),
                }),
            tables
                .quick_course_node_type_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    (
                        row.timed_race_node_type_id.clone(),
                        QuickCourseNodeType {
                            add_seconds: positive(Some(row.add_time_seconds)).unwrap_or_default(),
                            // Radii of a few metres.
                            radius_m: row.detection_radius as f32,
                        },
                    )
                }),
        )
    }

    /// The rules of `courses` and `node_types` (id, rules); the first row of an id wins.
    pub fn build(
        courses: impl IntoIterator<Item = QuickCourse>,
        node_types: impl IntoIterator<Item = (String, QuickCourseNodeType)>,
    ) -> Self {
        let mut rules = Self::default();
        for course in courses {
            rules
                .courses
                .entry(course.id.to_lowercase())
                .or_insert(course);
        }
        for (id, node_type) in node_types {
            rules
                .node_types
                .entry(id.to_lowercase())
                .or_insert(node_type);
        }
        rules
    }

    /// Course `id` (any case).
    pub fn course(&self, id: &str) -> Option<&QuickCourse> {
        self.courses.get(&id.to_lowercase())
    }

    /// Node type `id` (any case).
    pub fn node_type(&self, id: &str) -> Option<&QuickCourseNodeType> {
        self.node_types.get(&id.to_lowercase())
    }
}

impl QuickCourse {
    /// The id of the course whose path this one rides: its reference, else its own.
    pub fn path_id(&self) -> &str {
        self.path.as_deref().unwrap_or(&self.id)
    }

    /// The time a hit of a node of `node_type` adds, in ms (module notes).
    pub fn added_ms(&self, node_type: &QuickCourseNodeType) -> u64 {
        // Minutes at most.
        (f64::from(node_type.add_seconds) * self.node_time_multiplier * MS_PER_SECOND).round()
            as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> QuickCourseRules {
        QuickCourseRules::build(
            [
                QuickCourse {
                    id: "EF_Rally_1".to_owned(),
                    timed: true,
                    starting_seconds: None,
                    path: None,
                    node_time_multiplier: 1.0,
                },
                QuickCourse {
                    id: "EF_Rally_3".to_owned(),
                    timed: true,
                    starting_seconds: None,
                    path: Some("EF_Rally_1".to_owned()),
                    node_time_multiplier: 0.85,
                },
            ],
            [(
                "Checkpoint45".to_owned(),
                QuickCourseNodeType {
                    add_seconds: 45,
                    radius_m: 5.0,
                },
            )],
        )
    }

    #[test]
    fn a_course_rides_its_reference_path_with_its_multiplier() {
        let rules = rules();
        let hard = rules.course("ef_rally_3").expect("course");
        assert_eq!(hard.path_id(), "EF_Rally_1");
        let checkpoint = rules.node_type("checkpoint45").expect("node type");
        assert_eq!(hard.added_ms(checkpoint), 38_250);
        let plain = rules.course("EF_Rally_1").expect("course");
        assert_eq!(plain.path_id(), "EF_Rally_1");
        assert_eq!(plain.added_ms(checkpoint), 45_000);
    }
}
