// SPDX-License-Identifier: AGPL-3.0-or-later
//! The points of a level that a player uses where they stand: the gatherables and the lore of
//! its metadata (`nw_world::records::GatherablePoint`, `LorePoint`), by kind and position. A
//! client's gather or lore read names its kind; the server takes it only when a point of that
//! kind lies within reach of the player, so a client cannot gather or read from anywhere.

use std::collections::HashMap;

use nw_sim_core::Position;

/// The edge of one grid cell of the index, metres (a cell and its 8 neighbours hold every point
/// within [`CELL_M`] of a position).
const CELL_M: f32 = 64.0;

/// The points of one kind of use, indexed by grid cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlacedPoints {
    /// Each point's kind (CRC-32 of its lowercase id) and position.
    points: Vec<(u32, Position)>,
    /// The points of each cell, by index.
    cells: HashMap<(i32, i32), Vec<usize>>,
}

/// The grid cell of `position`.
#[allow(clippy::cast_possible_truncation, reason = "level metres fit an i32 cell")]
fn cell_of(position: Position) -> (i32, i32) {
    (
        (position.x / CELL_M).floor() as i32,
        (position.y / CELL_M).floor() as i32,
    )
}

impl PlacedPoints {
    /// The index of `points` (kind, position).
    pub fn of(points: impl IntoIterator<Item = (u32, Position)>) -> Self {
        let mut index = Self::default();
        for (kind, position) in points {
            index
                .cells
                .entry(cell_of(position))
                .or_default()
                .push(index.points.len());
            index.points.push((kind, position));
        }
        index
    }

    /// The points of kind `kind` within `reach_m` (at most [`CELL_M`]) of `at`, across the
    /// ground, nearest first: index and position of each.
    pub fn within(&self, kind: u32, at: Position, reach_m: f32) -> Vec<(usize, Position)> {
        let reach_m = reach_m.min(CELL_M);
        let (cell_x, cell_y) = cell_of(at);
        let mut near: Vec<(usize, Position, f32)> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cell_x + dx, cell_y + dy)))
            .filter_map(|cell| self.cells.get(&cell))
            .flatten()
            .filter_map(|index| {
                let (point_kind, position) = self.points[*index];
                let distance = position.horizontal_distance(at);
                (point_kind == kind && distance <= reach_m).then_some((*index, position, distance))
            })
            .collect();
        near.sort_by(|a, b| a.2.total_cmp(&b.2));
        near.into_iter()
            .map(|(index, position, _)| (index, position))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLINT: u32 = 0x1111;
    const TREE: u32 = 0x2222;
    const REACH_M: f32 = 10.0;

    fn at(x: f32, y: f32) -> Position {
        Position { x, y, z: 0.0 }
    }

    #[test]
    fn the_points_of_the_kind_within_reach_are_found_across_cells() {
        let points = PlacedPoints::of([
            (FLINT, at(63.0, 0.0)),
            (FLINT, at(70.0, 0.0)),
            (TREE, at(65.0, 0.0)),
            (FLINT, at(500.0, 0.0)),
        ]);
        assert_eq!(
            points.within(FLINT, at(65.0, 0.0), REACH_M),
            [(0, at(63.0, 0.0)), (1, at(70.0, 0.0))],
            "nearest first, across cells"
        );
        assert_eq!(points.within(TREE, at(65.0, 3.0), REACH_M), [(2, at(65.0, 0.0))]);
        assert!(points.within(FLINT, at(200.0, 0.0), REACH_M).is_empty(), "out of reach");
        assert!(points.within(0x3333, at(63.0, 0.0), REACH_M).is_empty(), "no such kind");
    }
}
