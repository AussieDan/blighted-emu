// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  MOB_STATE_DEAD,
  TERRAIN_STEP_M,
  fightLines,
  groundTiles,
  labelText,
  lerpPosition,
  northRowHeight,
  plantOf,
  plantTriangles,
  sceneTriangles,
  stateGap,
  terrainSquare,
  toScene,
} = await import('./live3d.js');

const TILES = {
  world_m: 1024,
  y_index_north_up: false,
  levels: [{ level: 1, tile_m: 256, columns: 4, rows: 4 }],
};

test('toScene puts north away from a camera looking north, without a mirror', () => {
  const origin = toScene(0, 0, 0);
  const east = toScene(1, 0, 0);
  const north = toScene(0, 1, 0);
  const up = toScene(0, 0, 1);
  assert.deepEqual(up, { x: 0, y: 1, z: -0 });
  assert.equal(north.z < origin.z, true);
  // Right-handed check: east x north must point up, as in the world.
  const cross = {
    x: east.y * north.z - east.z * north.y,
    y: east.z * north.x - east.x * north.z,
    z: east.x * north.y - east.y * north.x,
  };
  assert.equal(cross.y > 0, true);
});

test('lerpPosition starts at from, ends at to, and holds after the gap', () => {
  const from = { x: 0, y: 0, z: 0 };
  const to = { x: 10, y: 20, z: 30 };
  assert.deepEqual(lerpPosition(from, to, 0, 250), from);
  assert.deepEqual(lerpPosition(from, to, 125, 250), { x: 5, y: 10, z: 15 });
  assert.deepEqual(lerpPosition(from, to, 900, 250), to);
  assert.deepEqual(lerpPosition(from, to, 10, 0), to);
});

test('stateGap is the default first, then the measured gap inside its bounds', () => {
  assert.equal(stateGap(null, 1000), 250);
  assert.equal(stateGap(1000, 1240), 240);
  assert.equal(stateGap(1000, 1001), 50);
  assert.equal(stateGap(1000, 9000), 1000);
});

test('labelText names a player and numbers a mob, with health', () => {
  assert.equal(labelText({ name: 'Voks', health: 50, max_health: 100 }, 'player'), 'Voks  50/100');
  assert.equal(labelText({ entity: 7, health: 0, max_health: 30 }, 'mob'), 'Mob 7  0/30');
});

test('fightLines skips a missing target', () => {
  const view = { players: [{ entity: 1, dead: false, target_entity: 99 }], mobs: [] };
  assert.deepEqual(fightLines(view), []);
});

test('fightLines skips a dead attacker and a dead target', () => {
  const view = {
    players: [
      { entity: 1, dead: true, target_entity: 2 },
      { entity: 2, dead: false, target_entity: null },
    ],
    mobs: [
      { entity: 10, state: 2, health: 5, target_entity: 2 },
      { entity: 11, state: MOB_STATE_DEAD, health: 0, target_entity: 2 },
      { entity: 12, state: 3, health: 5, target_entity: 11 },
    ],
  };
  assert.deepEqual(fightLines(view), [{ fromEntity: 10, toEntity: 2 }]);
});

test('fightLines pairs a live attacker with its live target both ways', () => {
  const view = {
    players: [{ entity: 1, dead: false, target_entity: 10 }],
    mobs: [{ entity: 10, state: 3, health: 5, target_entity: 1 }],
  };
  const lines = fightLines(view).map((line) => `${line.fromEntity}->${line.toEntity}`);
  assert.deepEqual(lines.sort(), ['1->10', '10->1']);
});

test('terrainSquare snaps its corner to the sample step and centres on the player', () => {
  const square = terrainSquare(1000.7, 2000.2);
  assert.equal(square.x0 % TERRAIN_STEP_M, 0);
  assert.equal(square.y0 % TERRAIN_STEP_M, 0);
  assert.equal(square.side, 768);
  assert.ok(Math.abs(square.x0 + square.side / 2 - 1000.7) < TERRAIN_STEP_M);
  assert.ok(Math.abs(square.y0 + square.side / 2 - 2000.2) < TERRAIN_STEP_M);
});

test('sceneTriangles turns world corners into scene space and keeps the indices', () => {
  const { positions, index } = sceneTriangles({ points: [10, 20, 3, 11, 20, 3, 10, 21, 4], triangles: [0, 1, 2] });
  assert.deepEqual([...positions], [10, 3, -20, 11, 3, -20, 10, 4, -21]);
  assert.deepEqual([...index], [0, 1, 2]);
});

test('groundTiles places the north tile at the top of the picture', () => {
  const square = { x0: 0, y0: 0, side: 512 };
  const placed = groundTiles(TILES, square, 2);
  const byRow = (row) => placed.filter((entry) => entry.tile.row === row && entry.tile.column === 0);
  const [south] = byRow(0);
  const [north] = byRow(1);
  assert.equal(south.px, 0);
  assert.equal(south.py, 512);
  assert.equal(north.py, 0);
  assert.equal(north.sizePx, 512);
});

test('northRowHeight reads rows from the north and fills missing samples', () => {
  const heights = [
    [1, 2],
    [3, null],
  ];
  assert.equal(northRowHeight(heights, 2, 0, 0, -1), 3);
  assert.equal(northRowHeight(heights, 2, 0, 1, -1), -1);
  assert.equal(northRowHeight(heights, 2, 1, 1, -1), 2);
});

test('plantOf sizes trees and bushes by the size word of their id and skips the rest', () => {
  assert.deepEqual(plantOf('TreeHardwoodHuge'), { tree: true, height: 24 });
  assert.deepEqual(plantOf('TreeIronwoodTiny'), { tree: true, height: 5 });
  assert.deepEqual(plantOf('BananaTreeSmall'), { tree: true, height: 8 });
  assert.equal(plantOf('BushLarge').tree, false);
  assert.ok(plantOf('BushLarge').height > plantOf('BushSmall').height);
  assert.equal(plantOf('Damned_Axeman'), null);
  assert.equal(plantOf('IronVein'), null);
});

test('plantTriangles builds a closed trunk and crown for a tree and one cone for a bush', () => {
  const tree = plantTriangles([{ id: 'TreeHardwoodLarge', x: 100, y: 200, z: 30 }]);
  // A square trunk (4 sides, 2 triangles each) and a six-sided cone (side and base per side).
  assert.equal(tree.index.length / 3, 4 * 2 + 6 * 2);
  const heights = [];
  for (let corner = 1; corner < tree.positions.length; corner += 3) heights.push(tree.positions[corner]);
  assert.equal(Math.min(...heights), 30, 'the trunk stands on the gatherable point');
  assert.equal(Math.max(...heights), 30 + 18, 'a large tree is 18 m high');
  const bush = plantTriangles([{ id: 'BushSmall', x: 0, y: 0, z: 5 }, { id: 'Skeleton', x: 0, y: 0, z: 5 }]);
  assert.equal(bush.index.length / 3, 6 * 2, 'one cone; the skeleton is left out');
});
