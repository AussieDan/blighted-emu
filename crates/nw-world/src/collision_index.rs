// SPDX-License-Identifier: AGPL-3.0-or-later
//! The search structure over a [`LevelCollision`]: a grid of the bodies' ground-plane bounds,
//! and a bounding-volume tree over each mesh's triangles. It answers the floor under a body
//! ([`CollisionIndex::floor_at`]) and the first solid surface on a line
//! ([`CollisionIndex::raycast`]).
//!
//! A query takes the line into each candidate body's own space (the inverse of its place), so a
//! shape is stored once and placed many times; a line's parameter is the same in both spaces.

use std::cell::Cell;
use std::sync::Arc;

use nw_formats::rigid::{Vec3, add, cross, dot, scale, sub};

use crate::collision::{
    Affine, Body, Geometry, LevelCollision, invert, transform_point, transform_vector,
};

/// The highest surface above a body's feet that can be its floor, m: the highest obstacle the
/// player's character controller detects as a step (`slices/player.dynamicslice`,
/// `m_stepUpMaxDetectionHeight` 1.3). A higher surface is over its head, not under its feet.
pub const FLOOR_SEARCH_ABOVE_M: f32 = 1.3;
/// The steepest surface a player stands on, degrees: the player capsule's `m_maxSlope` 70
/// (`slices/player.dynamicslice`, the shape component with filter `Player`).
pub const MAX_WALKABLE_SLOPE_DEG: f32 = 70.0;
/// The side of a grid cell, m (our choice: a few bodies per cell in the open world's towns).
const CELL_M: f32 = 32.0;
/// Triangles in a tree leaf (our choice).
const LEAF_TRIANGLES: usize = 4;
/// Corners of a box.
const BOX_CORNERS: u32 = 8;
/// Axes of space.
const AXES: usize = 3;
/// The index of the up axis.
const UP: usize = 2;

/// An axis-aligned box.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bounds {
    min: Vec3,
    max: Vec3,
}

impl Bounds {
    const EMPTY: Self = Self {
        min: [f32::INFINITY; AXES],
        max: [f32::NEG_INFINITY; AXES],
    };

    fn grow(&mut self, point: Vec3) {
        for ((min, max), value) in self.min.iter_mut().zip(&mut self.max).zip(point) {
            *min = min.min(value);
            *max = max.max(value);
        }
    }

    fn join(mut self, other: &Self) -> Self {
        self.grow(other.min);
        self.grow(other.max);
        self
    }

    /// The bounds of this box after `place`.
    fn placed(&self, place: &Affine) -> Self {
        let mut out = Self::EMPTY;
        for corner in 0..BOX_CORNERS {
            let pick = |axis: usize| {
                if corner & (1 << axis) == 0 {
                    self.min[axis]
                } else {
                    self.max[axis]
                }
            };
            out.grow(transform_point(place, [pick(0), pick(1), pick(2)]));
        }
        out
    }

    /// The part `[enter, leave]` of the line `origin + t * direction`, `t` in `[0, length]`,
    /// inside the box, or `None`.
    fn clip(&self, origin: Vec3, direction: Vec3, length: f32) -> Option<(f32, f32)> {
        let (mut enter, mut leave) = (0.0_f32, length);
        for axis in 0..AXES {
            if direction[axis] == 0.0 {
                if origin[axis] < self.min[axis] || origin[axis] > self.max[axis] {
                    return None;
                }
                continue;
            }
            let inverse = direction[axis].recip();
            let a = (self.min[axis] - origin[axis]) * inverse;
            let b = (self.max[axis] - origin[axis]) * inverse;
            enter = enter.max(a.min(b));
            leave = leave.min(a.max(b));
            if enter > leave {
                return None;
            }
        }
        Some((enter, leave))
    }
}

/// A node of a mesh's tree: its bounds and either two children (`count` 0, children at
/// `first` and `first + 1`) or `count` triangles from `first` of the tree's order.
#[derive(Debug, Clone, Copy)]
struct Node {
    bounds: Bounds,
    first: u32,
    count: u32,
}

/// The bounding-volume tree of one mesh.
#[derive(Debug, Clone)]
struct MeshTree {
    nodes: Vec<Node>,
    /// Triangle indices in tree order.
    order: Vec<u32>,
}

/// A surface a line meets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// Where, in the world.
    pub point: Vec3,
    /// The surface's outward unit normal in the world.
    pub normal: Vec3,
    /// How far along the line, as a share of its length (0 at its start, 1 at its end).
    pub share: f32,
}

/// What a line meets in one shape: the parameter and the outward normal in the shape's space.
#[derive(Debug, Clone, Copy)]
struct LocalHit {
    t: f32,
    normal: Vec3,
}

/// Which surfaces a query takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Faces {
    /// Only surfaces the line enters from outside (a floor seen from above).
    Entering,
    /// Every surface (a wall in either direction).
    Both,
}

/// A body with its bounds.
#[derive(Debug, Clone, Copy)]
struct PlacedBody {
    body: Body,
    bounds: Bounds,
}

/// The grid of the bodies' ground-plane bounds.
#[derive(Debug, Clone, Default)]
struct Grid {
    origin: [f32; 2],
    columns: usize,
    rows: usize,
    /// For each cell, where its bodies start in `items` (one more entry than cells).
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "cells of a level of a few thousand metres"
    )]
    fn cell_of(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let column = ((x - self.origin[0]) / CELL_M).floor();
        let row = ((y - self.origin[1]) / CELL_M).floor();
        (column >= 0.0
            && row >= 0.0
            && (column as usize) < self.columns
            && (row as usize) < self.rows)
            .then_some((column as usize, row as usize))
    }

    fn cell_range(&self, low: f32, high: f32, origin: f32, cells: usize) -> (usize, usize) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "cells of a level of a few thousand metres"
        )]
        let index =
            |value: f32| (((value - origin) / CELL_M).floor().max(0.0) as usize).min(cells - 1);
        (index(low), index(high))
    }

    /// The first cell (column, row) of `bounds`: the grid holds a body in every cell from there
    /// to the cell of its far corner.
    fn first_cell(&self, bounds: &Bounds) -> (usize, usize) {
        let column = self.cell_range(bounds.min[0], bounds.min[0], self.origin[0], self.columns);
        let row = self.cell_range(bounds.min[1], bounds.min[1], self.origin[1], self.rows);
        (column.0, row.0)
    }

    fn bodies_in(&self, column: usize, row: usize) -> &[u32] {
        let cell = row * self.columns + column;
        let (start, end) = (self.starts[cell] as usize, self.starts[cell + 1] as usize);
        &self.items[start..end]
    }
}

/// A level's shapes with their bounds and trees, shared by the indexes over its bodies.
#[derive(Debug, Default)]
struct Shapes {
    geometries: Vec<Geometry>,
    local_bounds: Vec<Bounds>,
    trees: Vec<Option<MeshTree>>,
}

/// The searchable collision of a level.
#[derive(Debug, Clone, Default)]
pub struct CollisionIndex {
    shapes: Arc<Shapes>,
    bodies: Vec<PlacedBody>,
    grid: Grid,
}

impl CollisionIndex {
    /// The index of `collision`. A body whose geometry index is out of range or whose place
    /// cannot be inverted is left out.
    pub fn new(collision: LevelCollision) -> Self {
        let LevelCollision {
            geometries, bodies, ..
        } = collision;
        let shapes = Shapes {
            local_bounds: geometries.iter().map(geometry_bounds).collect(),
            trees: geometries.iter().map(mesh_tree).collect(),
            geometries,
        };
        Self::over(Arc::new(shapes), bodies)
    }

    /// An index over other `bodies` of this index's level, sharing its shapes (the solids of a
    /// deferred slice the server shows).
    pub fn with_bodies(&self, bodies: Vec<Body>) -> Self {
        Self::over(Arc::clone(&self.shapes), bodies)
    }

    /// The index of `bodies` over `shapes`.
    fn over(shapes: Arc<Shapes>, bodies: Vec<Body>) -> Self {
        let bodies: Vec<PlacedBody> = bodies
            .into_iter()
            .filter(|body| invert(&body.place).is_some())
            .filter_map(|body| {
                let bounds = shapes
                    .local_bounds
                    .get(body.geometry as usize)?
                    .placed(&body.place);
                Some(PlacedBody { body, bounds })
            })
            .collect();
        let grid = build_grid(&bodies);
        Self {
            shapes,
            bodies,
            grid,
        }
    }

    /// The number of placed bodies.
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// The number of shapes.
    pub fn geometry_count(&self) -> usize {
        self.shapes.geometries.len()
    }

    /// The bytes the index holds (shapes, trees, bodies and grid), for the size report.
    pub fn memory_bytes(&self) -> usize {
        let geometry: usize = self
            .shapes
            .geometries
            .iter()
            .map(|geometry| match geometry {
                Geometry::Mesh { points, triangles } => {
                    size_of_val(points.as_slice()) + size_of_val(triangles.as_slice())
                }
                Geometry::Convex { points, planes } => {
                    size_of_val(points.as_slice()) + size_of_val(planes.as_slice())
                }
                _ => 0,
            })
            .sum::<usize>()
            + size_of_val(self.shapes.geometries.as_slice())
            + size_of_val(self.shapes.local_bounds.as_slice());
        let trees: usize = self
            .shapes
            .trees
            .iter()
            .flatten()
            .map(|tree| size_of_val(tree.nodes.as_slice()) + size_of_val(tree.order.as_slice()))
            .sum();
        geometry
            + trees
            + size_of_val(self.bodies.as_slice())
            + size_of_val(self.grid.starts.as_slice())
            + size_of_val(self.grid.items.as_slice())
    }

    /// The height of the floor under a body whose feet are at (`x`, `y`, `z`): the highest
    /// surface at most [`FLOOR_SEARCH_ABOVE_M`] above the feet that faces up at most
    /// [`MAX_WALKABLE_SLOPE_DEG`] from level, or `None` where no solid is under it.
    pub fn floor_at(&self, x: f32, y: f32, z: f32) -> Option<f32> {
        let (column, row) = self.grid.cell_of(x, y)?;
        let top = z + FLOOR_SEARCH_ABOVE_M;
        let min_up = MAX_WALKABLE_SLOPE_DEG.to_radians().cos();
        let mut best: Option<f32> = None;
        for &index in self.grid.bodies_in(column, row) {
            let placed = &self.bodies[index as usize];
            let bounds = &placed.bounds;
            if x < bounds.min[0]
                || x > bounds.max[0]
                || y < bounds.min[1]
                || y > bounds.max[1]
                || bounds.min[UP] > top
                || best.is_some_and(|floor| bounds.max[UP] <= floor)
            {
                continue;
            }
            let length = top - bounds.min[UP];
            let reach = best.map_or(length, |floor| top - floor);
            let origin = [x, y, top];
            let down = [0.0, 0.0, -1.0];
            let hits =
                self.hits_in_body(placed, origin, down, reach, Faces::Entering, Some(min_up));
            for (t, normal) in hits {
                if normal[UP] >= min_up && best.is_none_or(|floor| top - t > floor) {
                    best = Some(top - t);
                }
            }
        }
        best
    }

    /// The first solid surface on the line from `from` to `to`, either side of it.
    pub fn raycast(&self, from: Vec3, to: Vec3) -> Option<RayHit> {
        self.first_hit(from, to, Faces::Both)
    }

    /// The first solid surface the line from `from` to `to` enters from outside: a line that
    /// starts inside a solid meets none of that solid's faces on its way out.
    pub fn entering_hit(&self, from: Vec3, to: Vec3) -> Option<RayHit> {
        self.first_hit(from, to, Faces::Entering)
    }

    /// The first surface of `faces` on the line from `from` to `to`.
    fn first_hit(&self, from: Vec3, to: Vec3, faces: Faces) -> Option<RayHit> {
        let direction = sub(to, from);
        let length = dot(direction, direction).sqrt();
        if length <= f32::EPSILON {
            return None;
        }
        let unit = scale(direction, length.recip());
        let low = [from[0].min(to[0]), from[1].min(to[1])];
        let high = [from[0].max(to[0]), from[1].max(to[1])];
        if self.grid.columns == 0 {
            return None;
        }
        let (first_column, last_column) =
            self.grid
                .cell_range(low[0], high[0], self.grid.origin[0], self.grid.columns);
        let (first_row, last_row) =
            self.grid
                .cell_range(low[1], high[1], self.grid.origin[1], self.grid.rows);
        let one_cell = first_column == last_column && first_row == last_row;
        let mut best: Option<(f32, Vec3)> = None;
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                for &index in self.grid.bodies_in(column, row) {
                    let placed = &self.bodies[index as usize];
                    let reach = best.map_or(length, |(t, _)| t);
                    if placed.bounds.clip(from, unit, reach).is_none() {
                        continue;
                    }
                    // A body of several cells is taken once, in the first of its cells that the
                    // line's cells reach: the cell where a set of the bodies seen would first
                    // take it.
                    if !one_cell {
                        let (body_column, body_row) = self.grid.first_cell(&placed.bounds);
                        if column != body_column.max(first_column) || row != body_row.max(first_row)
                        {
                            continue;
                        }
                    }
                    for (t, normal) in self.hits_in_body(placed, from, unit, reach, faces, None) {
                        if best.is_none_or(|(known, _)| t < known) {
                            best = Some((t, normal));
                        }
                    }
                }
            }
        }
        best.map(|(t, normal)| RayHit {
            point: add(from, scale(unit, t)),
            normal,
            share: t / length,
        })
    }

    /// The surfaces of one body on the line `origin + t * direction` (`direction` of unit
    /// length), `t` in `[0, reach]`: each `t` with the world's outward unit normal. The search of
    /// a mesh stops past the nearest hit that faces up at least `stop_up` (any hit without one):
    /// the callers take the nearest such hit only.
    fn hits_in_body(
        &self,
        placed: &PlacedBody,
        origin: Vec3,
        direction: Vec3,
        reach: f32,
        faces: Faces,
        stop_up: Option<f32>,
    ) -> Vec<(f32, Vec3)> {
        let Some(inverse) = invert(&placed.body.place) else {
            return Vec::new();
        };
        let local_origin = transform_point(&inverse, origin);
        let local_direction = transform_vector(&inverse, direction);
        let index = placed.body.geometry as usize;
        let (Some(geometry), Some(tree)) = (
            self.shapes.geometries.get(index),
            self.shapes.trees.get(index),
        ) else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        let limit = Cell::new(reach);
        let mut take = |hit: LocalHit| {
            if (0.0..=limit.get()).contains(&hit.t) {
                let normal = world_normal(&inverse, hit.normal);
                hits.push((hit.t, normal));
                if stop_up.is_none_or(|min_up| normal[UP] >= min_up) {
                    limit.set(hit.t);
                }
            }
        };
        match geometry {
            Geometry::Mesh { points, triangles } => {
                if let Some(tree) = tree {
                    mesh_hits(
                        tree,
                        points,
                        triangles,
                        local_origin,
                        local_direction,
                        &limit,
                        faces,
                        &mut take,
                    );
                }
            }
            Geometry::Box { half_extents } => {
                let bounds = Bounds {
                    min: scale(*half_extents, -1.0),
                    max: *half_extents,
                };
                box_hits(&bounds, local_origin, local_direction, faces, &mut take);
            }
            Geometry::Sphere { radius } => {
                sphere_hits(
                    [0.0; AXES],
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut take,
                );
            }
            Geometry::Capsule { from, to, radius } => {
                sphere_hits(
                    *from,
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut |hit| {
                        if dot(
                            sub(add(local_origin, scale(local_direction, hit.t)), *from),
                            sub(*to, *from),
                        ) <= 0.0
                        {
                            take(hit);
                        }
                    },
                );
                sphere_hits(
                    *to,
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut |hit| {
                        if dot(
                            sub(add(local_origin, scale(local_direction, hit.t)), *to),
                            sub(*from, *to),
                        ) <= 0.0
                        {
                            take(hit);
                        }
                    },
                );
                tube_hits(
                    *from,
                    *to,
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut take,
                );
            }
            Geometry::Cylinder { from, to, radius } => {
                tube_hits(
                    *from,
                    *to,
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut take,
                );
                cap_hits(
                    *from,
                    *to,
                    *radius,
                    local_origin,
                    local_direction,
                    faces,
                    &mut take,
                );
            }
            Geometry::Convex { planes, .. } => {
                convex_hits(planes, local_origin, local_direction, faces, &mut take);
            }
        }
        hits
    }
}

/// The world's outward unit normal of a shape-space normal (the inverse's transpose).
fn world_normal(inverse: &Affine, normal: Vec3) -> Vec3 {
    let world = [
        inverse[0][0] * normal[0] + inverse[1][0] * normal[1] + inverse[2][0] * normal[2],
        inverse[0][1] * normal[0] + inverse[1][1] * normal[1] + inverse[2][1] * normal[2],
        inverse[0][2] * normal[0] + inverse[1][2] * normal[1] + inverse[2][2] * normal[2],
    ];
    let length = dot(world, world).sqrt();
    if length <= f32::EPSILON {
        world
    } else {
        scale(world, length.recip())
    }
}

/// Whether a surface with outward `normal` met by a line along `direction` counts.
fn counts(faces: Faces, normal: Vec3, direction: Vec3) -> bool {
    faces == Faces::Both || dot(normal, direction) < 0.0
}

/// The triangles of a mesh on the line, out to `reach`; `take` may shorten it.
#[allow(
    clippy::too_many_arguments,
    reason = "the line, the mesh and its tree, read in one walk"
)]
fn mesh_hits(
    tree: &MeshTree,
    points: &[Vec3],
    triangles: &[[u16; 3]],
    origin: Vec3,
    direction: Vec3,
    reach: &Cell<f32>,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let mut stack = vec![0_usize];
    while let Some(index) = stack.pop() {
        let Some(node) = tree.nodes.get(index) else {
            continue;
        };
        if node.bounds.clip(origin, direction, reach.get()).is_none() {
            continue;
        }
        if node.count == 0 {
            stack.push(node.first as usize);
            stack.push(node.first as usize + 1);
            continue;
        }
        let first = node.first as usize;
        for &triangle in &tree.order[first..first + node.count as usize] {
            let corners = triangles[triangle as usize].map(|corner| points[corner as usize]);
            if let Some(hit) = triangle_hit(corners, origin, direction)
                && counts(faces, hit.normal, direction)
            {
                take(hit);
            }
        }
    }
}

/// Where the line meets the triangle `corners` (Möller-Trumbore), with the triangle's normal
/// by its winding.
fn triangle_hit(corners: [Vec3; 3], origin: Vec3, direction: Vec3) -> Option<LocalHit> {
    let edge1 = sub(corners[1], corners[0]);
    let edge2 = sub(corners[2], corners[0]);
    let p = cross(direction, edge2);
    let determinant = dot(edge1, p);
    if determinant.abs() <= f32::EPSILON * dot(edge1, edge1).max(dot(edge2, edge2)) {
        return None;
    }
    let inverse = determinant.recip();
    let to_origin = sub(origin, corners[0]);
    let u = dot(to_origin, p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(to_origin, edge1);
    let v = dot(direction, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(LocalHit {
        t: dot(edge2, q) * inverse,
        normal: cross(edge1, edge2),
    })
}

/// The faces of a box on the line: where it enters and leaves the box.
fn box_hits(
    bounds: &Bounds,
    origin: Vec3,
    direction: Vec3,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let (mut enter, mut leave) = (f32::NEG_INFINITY, f32::INFINITY);
    let (mut enter_normal, mut leave_normal) = ([0.0; AXES], [0.0; AXES]);
    for axis in 0..AXES {
        if direction[axis] == 0.0 {
            if origin[axis] < bounds.min[axis] || origin[axis] > bounds.max[axis] {
                return;
            }
            continue;
        }
        let inverse = direction[axis].recip();
        let to_min = (bounds.min[axis] - origin[axis]) * inverse;
        let to_max = (bounds.max[axis] - origin[axis]) * inverse;
        let (near, far, near_sign) = if to_min < to_max {
            (to_min, to_max, -1.0)
        } else {
            (to_max, to_min, 1.0)
        };
        if near > enter {
            enter = near;
            enter_normal = [0.0; AXES];
            enter_normal[axis] = near_sign;
        }
        if far < leave {
            leave = far;
            leave_normal = [0.0; AXES];
            leave_normal[axis] = -near_sign;
        }
    }
    if enter > leave {
        return;
    }
    take(LocalHit {
        t: enter,
        normal: enter_normal,
    });
    if faces == Faces::Both {
        take(LocalHit {
            t: leave,
            normal: leave_normal,
        });
    }
}

/// The surface of a sphere on the line.
fn sphere_hits(
    centre: Vec3,
    radius: f32,
    origin: Vec3,
    direction: Vec3,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let offset = sub(origin, centre);
    let a = dot(direction, direction);
    let b = dot(offset, direction);
    let c = dot(offset, offset) - radius * radius;
    let discriminant = b * b - a * c;
    if a <= 0.0 || discriminant < 0.0 {
        return;
    }
    let root = discriminant.sqrt();
    for (t, keep) in [
        ((-b - root) / a, true),
        ((-b + root) / a, faces == Faces::Both),
    ] {
        if keep {
            let normal = add(offset, scale(direction, t));
            take(LocalHit { t, normal });
        }
    }
}

/// The side of the tube around the segment from `from` to `to`, between its ends.
fn tube_hits(
    from: Vec3,
    to: Vec3,
    radius: f32,
    origin: Vec3,
    direction: Vec3,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let axis = sub(to, from);
    let offset = sub(origin, from);
    let axis_axis = dot(axis, axis);
    let axis_direction = dot(axis, direction);
    let axis_offset = dot(axis, offset);
    let k2 = axis_axis * dot(direction, direction) - axis_direction * axis_direction;
    let k1 = axis_axis * dot(offset, direction) - axis_offset * axis_direction;
    let k0 =
        axis_axis * dot(offset, offset) - axis_offset * axis_offset - radius * radius * axis_axis;
    let discriminant = k1 * k1 - k2 * k0;
    if k2 <= 0.0 || discriminant < 0.0 {
        return;
    }
    let root = discriminant.sqrt();
    for (t, keep) in [
        ((-k1 - root) / k2, true),
        ((-k1 + root) / k2, faces == Faces::Both),
    ] {
        let along = axis_offset + t * axis_direction;
        if keep && along > 0.0 && along < axis_axis {
            let point = add(offset, scale(direction, t));
            let normal = sub(point, scale(axis, along / axis_axis));
            take(LocalHit { t, normal });
        }
    }
}

/// The two flat caps of the cylinder from `from` to `to`.
fn cap_hits(
    from: Vec3,
    to: Vec3,
    radius: f32,
    origin: Vec3,
    direction: Vec3,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let axis = sub(to, from);
    for (centre, outward) in [(from, scale(axis, -1.0)), (to, axis)] {
        let towards = dot(outward, direction);
        if towards == 0.0 || (faces == Faces::Entering && towards > 0.0) {
            continue;
        }
        let t = dot(outward, sub(centre, origin)) / towards;
        let off_axis = sub(add(origin, scale(direction, t)), centre);
        if dot(off_axis, off_axis) <= radius * radius {
            take(LocalHit { t, normal: outward });
        }
    }
}

/// Where the line enters (and leaves) a convex solid of `planes`.
fn convex_hits(
    planes: &[[f32; 4]],
    origin: Vec3,
    direction: Vec3,
    faces: Faces,
    take: &mut impl FnMut(LocalHit),
) {
    let (mut enter, mut leave) = (f32::NEG_INFINITY, f32::INFINITY);
    let (mut enter_normal, mut leave_normal) = (None, None);
    for plane in planes {
        let normal = [plane[0], plane[1], plane[2]];
        let towards = dot(normal, direction);
        let outside = dot(normal, origin) - plane[3];
        if towards == 0.0 {
            if outside > 0.0 {
                return;
            }
            continue;
        }
        let t = -outside / towards;
        if towards < 0.0 {
            if t > enter {
                enter = t;
                enter_normal = Some(normal);
            }
        } else if t < leave {
            leave = t;
            leave_normal = Some(normal);
        }
    }
    if enter > leave {
        return;
    }
    if let Some(normal) = enter_normal {
        take(LocalHit { t: enter, normal });
    }
    if faces == Faces::Both
        && let Some(normal) = leave_normal
    {
        take(LocalHit { t: leave, normal });
    }
}

/// The bounds of a geometry in its own space.
fn geometry_bounds(geometry: &Geometry) -> Bounds {
    let mut bounds = Bounds::EMPTY;
    let spread = |bounds: &mut Bounds, centre: Vec3, radius: f32| {
        bounds.grow(sub(centre, [radius; AXES]));
        bounds.grow(add(centre, [radius; AXES]));
    };
    match geometry {
        Geometry::Mesh { points, .. } | Geometry::Convex { points, .. } => {
            points.iter().for_each(|&point| bounds.grow(point));
        }
        Geometry::Box { half_extents } => {
            bounds.grow(scale(*half_extents, -1.0));
            bounds.grow(*half_extents);
        }
        Geometry::Sphere { radius } => spread(&mut bounds, [0.0; AXES], *radius),
        Geometry::Capsule { from, to, radius } | Geometry::Cylinder { from, to, radius } => {
            spread(&mut bounds, *from, *radius);
            spread(&mut bounds, *to, *radius);
        }
    }
    bounds
}

/// The tree of a mesh geometry; `None` for the other kinds.
fn mesh_tree(geometry: &Geometry) -> Option<MeshTree> {
    let Geometry::Mesh { points, triangles } = geometry else {
        return None;
    };
    let triangle_bounds: Vec<Bounds> = triangles
        .iter()
        .map(|triangle| {
            let mut bounds = Bounds::EMPTY;
            for &corner in triangle {
                if let Some(&point) = points.get(corner as usize) {
                    bounds.grow(point);
                }
            }
            bounds
        })
        .collect();
    let valid = |triangle: &[u16; 3]| {
        triangle
            .iter()
            .all(|&corner| (corner as usize) < points.len())
    };
    let mut order: Vec<u32> = (0..triangles.len())
        .filter(|&index| valid(&triangles[index]))
        .filter_map(|index| u32::try_from(index).ok())
        .collect();
    let mut nodes = Vec::new();
    if !order.is_empty() {
        let count = order.len();
        nodes.push(Node {
            bounds: Bounds::EMPTY,
            first: 0,
            count: 0,
        });
        split(&mut nodes, 0, &mut order, 0, count, &triangle_bounds);
    }
    Some(MeshTree { nodes, order })
}

/// Fills node `node` with the triangles `order[first..first + count]`: a leaf when few, else
/// two children split at the median of the longest axis.
fn split(
    nodes: &mut Vec<Node>,
    node: usize,
    order: &mut [u32],
    first: usize,
    count: usize,
    triangle_bounds: &[Bounds],
) {
    let slice = &mut order[first..first + count];
    let bounds = slice.iter().fold(Bounds::EMPTY, |all, &index| {
        all.join(&triangle_bounds[index as usize])
    });
    if count <= LEAF_TRIANGLES {
        nodes[node] = Node {
            bounds,
            first: small_index(first),
            count: small_index(count),
        };
        return;
    }
    let extent = sub(bounds.max, bounds.min);
    let axis = (0..AXES)
        .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
        .unwrap_or(0);
    let centre = |index: u32| {
        let bounds = &triangle_bounds[index as usize];
        bounds.min[axis] + bounds.max[axis]
    };
    let half = count / 2;
    slice.select_nth_unstable_by(half, |&a, &b| centre(a).total_cmp(&centre(b)));
    let children = nodes.len();
    nodes[node] = Node {
        bounds,
        first: small_index(children),
        count: 0,
    };
    for _ in 0..2 {
        nodes.push(Node {
            bounds: Bounds::EMPTY,
            first: 0,
            count: 0,
        });
    }
    split(nodes, children, order, first, half, triangle_bounds);
    split(
        nodes,
        children + 1,
        order,
        first + half,
        count - half,
        triangle_bounds,
    );
}

/// `value` as a `u32` index: the triangles of a mesh (at most 65,536 points), the nodes of its
/// tree and the bodies of a level are far fewer than 4 billion.
#[allow(clippy::cast_possible_truncation, reason = "see the doc comment")]
const fn small_index(value: usize) -> u32 {
    value as u32
}

/// The grid over the ground-plane bounds of `bodies`.
fn build_grid(bodies: &[PlacedBody]) -> Grid {
    let mut all = Bounds::EMPTY;
    for placed in bodies {
        all = all.join(&placed.bounds);
    }
    if bodies.is_empty() {
        return Grid::default();
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "cells of a level of a few thousand metres"
    )]
    let cells = |low: f32, high: f32| (((high - low) / CELL_M).floor() as usize) + 1;
    let mut grid = Grid {
        origin: [all.min[0], all.min[1]],
        columns: cells(all.min[0], all.max[0]),
        rows: cells(all.min[1], all.max[1]),
        starts: Vec::new(),
        items: Vec::new(),
    };
    let cell_count = grid.columns * grid.rows;
    let ranges: Vec<((usize, usize), (usize, usize))> = bodies
        .iter()
        .map(|placed| {
            (
                grid.cell_range(
                    placed.bounds.min[0],
                    placed.bounds.max[0],
                    grid.origin[0],
                    grid.columns,
                ),
                grid.cell_range(
                    placed.bounds.min[1],
                    placed.bounds.max[1],
                    grid.origin[1],
                    grid.rows,
                ),
            )
        })
        .collect();
    let mut counts = vec![0_u32; cell_count + 1];
    for ((first_column, last_column), (first_row, last_row)) in &ranges {
        for row in *first_row..=*last_row {
            for column in *first_column..=*last_column {
                counts[row * grid.columns + column + 1] += 1;
            }
        }
    }
    for cell in 0..cell_count {
        counts[cell + 1] += counts[cell];
    }
    let mut next = counts.clone();
    let mut items = vec![0_u32; counts[cell_count] as usize];
    for (index, ((first_column, last_column), (first_row, last_row))) in ranges.iter().enumerate() {
        for row in *first_row..=*last_row {
            for column in *first_column..=*last_column {
                let cell = row * grid.columns + column;
                items[next[cell] as usize] = small_index(index);
                next[cell] += 1;
            }
        }
    }
    grid.starts = counts;
    grid.items = items;
    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::{IDENTITY_AFFINE, place_of};

    const TOLERANCE: f32 = 1e-4;

    /// A deck of two triangles 10 m square at height 0 (facing up) and its underside 0.35 m
    /// lower (facing down), like the tutorial galleon's deck slab.
    fn deck() -> Geometry {
        Geometry::Mesh {
            points: vec![
                [-5.0, -5.0, 0.0],
                [5.0, -5.0, 0.0],
                [5.0, 5.0, 0.0],
                [-5.0, 5.0, 0.0],
                [-5.0, -5.0, -0.35],
                [5.0, -5.0, -0.35],
                [5.0, 5.0, -0.35],
                [-5.0, 5.0, -0.35],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6]],
        }
    }

    fn moved(x: f32, y: f32, z: f32) -> Affine {
        let mut place = IDENTITY_AFFINE;
        place[0][3] = x;
        place[1][3] = y;
        place[2][3] = z;
        place
    }

    fn index(geometries: Vec<Geometry>, bodies: Vec<Body>) -> CollisionIndex {
        CollisionIndex::new(LevelCollision {
            level: "lv".to_owned(),
            geometries,
            bodies,
            ..LevelCollision::default()
        })
    }

    #[test]
    fn the_floor_is_the_top_of_the_deck_not_its_underside() {
        let index = index(
            vec![deck()],
            vec![Body {
                geometry: 0,
                place: moved(480.0, 850.0, 89.38),
            }],
        );
        let floor = |z: f32| index.floor_at(479.5, 849.7, z);
        assert!((floor(89.38).expect("on it") - 89.38).abs() < TOLERANCE);
        assert!(
            (floor(88.5).expect("just under the top") - 89.38).abs() < TOLERANCE,
            "within a step of the feet"
        );
        assert!((floor(120.0).expect("from above") - 89.38).abs() < TOLERANCE);
        assert_eq!(floor(80.0), None, "far under the deck: over the head");
        assert_eq!(index.floor_at(470.0, 849.7, 89.38), None, "off the deck");
    }

    #[test]
    fn stacked_decks_give_the_one_under_the_feet() {
        let index = index(
            vec![deck()],
            vec![
                Body {
                    geometry: 0,
                    place: moved(0.0, 0.0, 86.18),
                },
                Body {
                    geometry: 0,
                    place: moved(0.0, 0.0, 89.38),
                },
            ],
        );
        assert!((index.floor_at(1.0, 1.0, 86.2).expect("lower") - 86.18).abs() < TOLERANCE);
        assert!((index.floor_at(1.0, 1.0, 89.4).expect("upper") - 89.38).abs() < TOLERANCE);
        assert_eq!(index.body_count(), 2);
        assert_eq!(index.geometry_count(), 1);
        assert!(index.memory_bytes() > 0);
    }

    #[test]
    fn a_turned_scaled_box_and_a_steep_roof() {
        // A box turned a quarter about z and stretched twice: its top at 2 m, 4 m long in y.
        let turned = place_of(
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
            2.0,
            [0.0, 0.0, 0.0],
        );
        // A roof plane tilted 80 degrees: too steep to stand on.
        let tilt = 80.0_f32.to_radians();
        let steep = [
            [tilt.cos(), 0.0, -tilt.sin(), 20.0],
            [0.0, 1.0, 0.0, 0.0],
            [tilt.sin(), 0.0, tilt.cos(), 0.0],
        ];
        let index = index(
            vec![
                Geometry::Box {
                    half_extents: [0.5, 2.0, 1.0],
                },
                deck(),
            ],
            vec![
                Body {
                    geometry: 0,
                    place: turned,
                },
                Body {
                    geometry: 1,
                    place: steep,
                },
            ],
        );
        assert!((index.floor_at(3.5, 0.0, 2.0).expect("top") - 2.0).abs() < TOLERANCE);
        assert_eq!(index.floor_at(0.0, 3.5, 2.0), None, "the short side is 1 m");
        assert_eq!(index.floor_at(20.0, 0.0, 0.5), None, "too steep");
    }

    #[test]
    fn spheres_capsules_cylinders_and_hulls_have_tops() {
        let corners: Vec<Vec3> = (0..8)
            .map(|bits| {
                let axis = |bit: u32| if bits & (1 << bit) == 0 { -1.0 } else { 1.0 };
                [axis(0), axis(1), axis(2)]
            })
            .collect();
        let planes = crate::collision::hull_planes(&corners);
        let index = index(
            vec![
                Geometry::Sphere { radius: 1.0 },
                Geometry::Capsule {
                    from: [0.0, 0.0, -0.45],
                    to: [0.0, 0.0, 0.45],
                    radius: 0.45,
                },
                Geometry::Cylinder {
                    from: [0.0, 0.0, -2.0],
                    to: [0.0, 0.0, 2.0],
                    radius: 21.0,
                },
                Geometry::Convex {
                    points: corners,
                    planes,
                },
            ],
            vec![
                Body {
                    geometry: 0,
                    place: moved(0.0, 0.0, 0.0),
                },
                Body {
                    geometry: 1,
                    place: moved(100.0, 0.0, 0.9),
                },
                Body {
                    geometry: 2,
                    place: moved(200.0, 0.0, 0.0),
                },
                Body {
                    geometry: 3,
                    place: moved(300.0, 0.0, 0.0),
                },
            ],
        );
        assert!((index.floor_at(0.0, 0.0, 1.0).expect("sphere") - 1.0).abs() < TOLERANCE);
        assert!((index.floor_at(100.0, 0.0, 1.8).expect("capsule") - 1.8).abs() < TOLERANCE);
        assert!((index.floor_at(215.0, 0.0, 2.0).expect("cylinder") - 2.0).abs() < TOLERANCE);
        assert!((index.floor_at(300.5, 0.5, 1.0).expect("hull") - 1.0).abs() < TOLERANCE);
    }

    #[test]
    fn a_ray_stops_at_the_first_wall_from_either_side() {
        // A wall: a box 0.25 m thick across x at x = 10.
        let index = index(
            vec![Geometry::Box {
                half_extents: [0.125, 5.0, 2.0],
            }],
            vec![Body {
                geometry: 0,
                place: moved(10.0, 0.0, 2.0),
            }],
        );
        let hit = index
            .raycast([0.0, 0.0, 1.0], [20.0, 0.0, 1.0])
            .expect("wall");
        assert!((hit.point[0] - 9.875).abs() < TOLERANCE, "{hit:?}");
        assert!((hit.normal[0] + 1.0).abs() < TOLERANCE);
        assert!((hit.share - 9.875 / 20.0).abs() < TOLERANCE);
        let back = index
            .raycast([20.0, 0.0, 1.0], [0.0, 0.0, 1.0])
            .expect("from the other side");
        assert!((back.point[0] - 10.125).abs() < TOLERANCE);
        assert_eq!(
            index.raycast([0.0, 0.0, 1.0], [5.0, 0.0, 1.0]),
            None,
            "short"
        );
        assert_eq!(
            index.raycast([0.0, 0.0, 9.0], [20.0, 0.0, 9.0]),
            None,
            "over it"
        );
    }

    #[test]
    fn a_ray_over_many_cells_meets_walls_of_many_cells_at_the_nearest() {
        // Walls 0.25 m thick across x, 200 m long in y: each is in several cell rows, and the
        // line starts in a row past their first.
        let index = index(
            vec![Geometry::Box {
                half_extents: [0.125, 100.0, 2.0],
            }],
            vec![
                Body {
                    geometry: 0,
                    place: moved(150.0, 0.0, 2.0),
                },
                Body {
                    geometry: 0,
                    place: moved(70.0, 0.0, 2.0),
                },
            ],
        );
        let hit = index
            .raycast([0.0, 40.0, 1.0], [200.0, 75.0, 1.0])
            .expect("wall");
        assert!((hit.point[0] - 69.875).abs() < TOLERANCE, "{hit:?}");
        let back = index
            .raycast([200.0, -75.0, 1.0], [0.0, -40.0, 1.0])
            .expect("from the other side");
        assert!((back.point[0] - 150.125).abs() < TOLERANCE, "{back:?}");
    }

    #[test]
    fn an_entering_ray_meets_a_wall_from_outside_only() {
        // A box 2 m thick across x from x = 9 to x = 11.
        let index = index(
            vec![Geometry::Box {
                half_extents: [1.0, 5.0, 2.0],
            }],
            vec![Body {
                geometry: 0,
                place: moved(10.0, 0.0, 2.0),
            }],
        );
        let hit = index
            .entering_hit([0.0, 0.0, 1.0], [20.0, 0.0, 1.0])
            .expect("wall");
        assert!((hit.point[0] - 9.0).abs() < TOLERANCE, "{hit:?}");
        assert!((hit.normal[0] + 1.0).abs() < TOLERANCE);
        assert_eq!(
            index.entering_hit([10.0, 0.0, 1.0], [20.0, 0.0, 1.0]),
            None,
            "from inside, out"
        );
        assert!(
            index.raycast([10.0, 0.0, 1.0], [20.0, 0.0, 1.0]).is_some(),
            "the way out is a surface either side"
        );
    }

    #[test]
    fn other_bodies_share_the_shapes() {
        let level = index(vec![deck()], Vec::new());
        assert_eq!(level.floor_at(0.0, 0.0, 1.0), None);
        let shown = level.with_bodies(vec![Body {
            geometry: 0,
            place: moved(0.0, 0.0, 1.0),
        }]);
        assert!((shown.floor_at(0.0, 0.0, 1.0).expect("deck") - 1.0).abs() < TOLERANCE);
        assert_eq!(shown.geometry_count(), 1);
    }

    #[test]
    fn an_empty_level_has_no_floor_and_bad_bodies_are_left_out() {
        let empty = CollisionIndex::default();
        assert_eq!(empty.floor_at(0.0, 0.0, 0.0), None);
        assert_eq!(empty.raycast([0.0; 3], [1.0, 0.0, 0.0]), None);
        let flat = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4]];
        let index = index(
            vec![deck()],
            vec![
                Body {
                    geometry: 0,
                    place: flat,
                },
                Body {
                    geometry: 7,
                    place: IDENTITY_AFFINE,
                },
            ],
        );
        assert_eq!(index.body_count(), 0);
    }

    /// Three mob places of the open world's starting beach (the hunt starts of the tools
    /// repository's test client config), where retail beach mobs walk.
    const BEACH_STARTS: [[f32; 2]; 3] = [[7171.3, 3160.2], [9696.3, 2260.2], [7971.3, 4685.2]];
    /// The half side of the square of points around each place: a mob walks there.
    const BEACH_HALF_SIDE_M: f32 = 96.0;
    /// The spacing of the points, m.
    const BEACH_SPACING_M: f32 = 1.5;
    /// A mob's walk line step (`sim::mob_ai` `FLOOR_STEP_M`).
    const WALK_STEP_M: f32 = 0.5;
    /// A height over the beach's floors, m.
    const ABOVE_BEACH_M: f32 = 120.0;
    /// The knee height of a walk step's wall ray over its floor (`sim::mob_ai` `STEP_UP_M`).
    const KNEE_M: f32 = 0.5;
    /// The longest mean time of one walk step (a floor and a wall ray), µs, on the 28-core
    /// development PC (2026-10-31): 1.82 with the first-cell check on every body of a ray, 1.55
    /// with it only on rays over several cells and a mesh search that stops at the nearest hit.
    const MAX_WALK_STEP_US: f64 = 1.7;
    const US_PER_S: f64 = 1e6;

    /// Run with `NW_CONTENT_DIR` set:
    /// `cargo test --release -p nw-world walk_steps_on_the_beach -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the build 25810808 collision file in NW_CONTENT_DIR; a release timing"]
    fn walk_steps_on_the_beach_are_fast() {
        const BUILD: &str = "25810808";
        const LEVEL: &str = "newworld_vitaeeterna";
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = crate::collision::collision_file_path(std::path::Path::new(&dir), BUILD, LEVEL);
        let level = LevelCollision::load(&path, BUILD).expect("collision");
        let index = CollisionIndex::new(LevelCollision {
            deferred: Vec::new(),
            ..level
        });
        // A few hundred points a side.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a few hundred"
        )]
        let side = (2.0 * BEACH_HALF_SIDE_M / BEACH_SPACING_M) as u32;
        let mut points = Vec::new();
        for [x, y] in BEACH_STARTS {
            for row in 0..side {
                for column in 0..side {
                    points.push([
                        x - BEACH_HALF_SIDE_M + column as f32 * BEACH_SPACING_M,
                        y - BEACH_HALF_SIDE_M + row as f32 * BEACH_SPACING_M,
                    ]);
                }
            }
        }
        let started = std::time::Instant::now();
        let (mut floors, mut walls) = (0_u32, 0_u32);
        for [x, y] in &points {
            let floor = index.floor_at(*x, *y, ABOVE_BEACH_M);
            floors += u32::from(floor.is_some());
            let knee = floor.unwrap_or(0.0) + KNEE_M;
            walls += u32::from(
                index
                    .raycast([*x, *y, knee], [*x + WALK_STEP_M, *y, knee])
                    .is_some(),
            );
        }
        let step_us = started.elapsed().as_secs_f64() * US_PER_S / points.len() as f64;
        println!(
            "beach walk steps: {} points, {floors} on a structure, {walls} walls; {step_us:.3} us a step",
            points.len()
        );
        assert!(floors > 0, "the beach has structures");
        assert!(step_us < MAX_WALK_STEP_US, "{step_us:.3} us a step");
    }
}
