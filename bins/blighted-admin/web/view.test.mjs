// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  boxAround,
  boxOf,
  parentTile,
  screenToWorld,
  spawnCellSquares,
  tileAt,
  tileLevelFor,
  visibleTiles,
  worldToScreen,
  zoomAt,
} from './view.js';

const WORLD_M = 14336;
const MAP_TILE_PX = 1024;

function tilesIndex(northUp) {
  const levels = [];
  for (let level = 1; level <= 8; level += 1) {
    const tile_m = 256 * 2 ** (level - 1);
    const count = Math.ceil(WORLD_M / tile_m);
    levels.push({ level, tile_m, columns: count, rows: count });
  }
  return { level: 'test', world_m: WORLD_M, levels, y_index_north_up: northUp };
}

const view = { centerX: 7000, centerY: 5000, metresPerPixel: 2, width: 800, height: 600 };

test('worldToScreen and screenToWorld are inverse', () => {
  const screen = worldToScreen(view, 7100, 4900);
  const world = screenToWorld(view, screen.px, screen.py);
  assert.ok(Math.abs(world.x - 7100) < 1e-9 && Math.abs(world.y - 4900) < 1e-9);
});

test('north is up on screen', () => {
  const north = worldToScreen(view, 7000, 5100);
  const south = worldToScreen(view, 7000, 4900);
  assert.ok(north.py < south.py);
  assert.deepEqual(worldToScreen(view, 7000, 5000), { px: 400, py: 300 });
});

test('zoomAt keeps the cursor point', () => {
  const before = screenToWorld(view, 100, 50);
  const zoomed = zoomAt(view, 100, 50, 0.5, WORLD_M);
  assert.equal(zoomed.metresPerPixel, 1);
  const after = screenToWorld(zoomed, 100, 50);
  assert.ok(Math.abs(after.x - before.x) < 1e-9 && Math.abs(after.y - before.y) < 1e-9);
});

test('zoomAt stays inside its limits', () => {
  assert.ok(zoomAt(view, 0, 0, 1e-9, WORLD_M).metresPerPixel > 0);
  assert.ok(zoomAt(view, 0, 0, 1e9, WORLD_M).metresPerPixel <= WORLD_M);
});

test('tileLevelFor picks level 1 near 0.25 m per pixel and level 8 for the whole world', () => {
  const tiles = tilesIndex(false);
  assert.equal(tileLevelFor({ ...view, metresPerPixel: 0.25 }, tiles, MAP_TILE_PX), 1);
  assert.equal(tileLevelFor({ ...view, metresPerPixel: 0.1 }, tiles, MAP_TILE_PX), 1);
  assert.equal(tileLevelFor({ ...view, metresPerPixel: WORLD_M / 400 }, tiles, MAP_TILE_PX), 8);
});

test('visibleTiles covers the view and flips y when asked', () => {
  const close = { centerX: 300, centerY: 300, metresPerPixel: 0.25, width: 512, height: 512 };
  const southUp = visibleTiles(close, tilesIndex(false), 1);
  assert.deepEqual(
    southUp.map((tile) => [tile.row, tile.column]).sort(),
    [[0, 0], [0, 1], [1, 0], [1, 1]],
  );
  const first = southUp.find((tile) => tile.row === 0 && tile.column === 0);
  assert.deepEqual([first.west, first.south, first.size], [0, 0, 256]);
  const northUp = visibleTiles(close, tilesIndex(true), 1);
  assert.deepEqual(
    northUp.map((tile) => tile.row).sort((a, b) => a - b),
    [54, 54, 55, 55],
  );
  const bottom = northUp.find((tile) => tile.row === 55 && tile.column === 0);
  assert.equal(bottom.south, WORLD_M - 56 * 256);
});

test('visibleTiles outside the world is empty', () => {
  const away = { centerX: -5000, centerY: -5000, metresPerPixel: 1, width: 100, height: 100 };
  assert.deepEqual(visibleTiles(away, tilesIndex(false), 1), []);
});

test('boxOf gives the world box of the view', () => {
  assert.deepEqual(boxOf(view), { minX: 6200, minY: 4400, maxX: 7800, maxY: 5600 });
});

test('tileAt and parentTile place a tile and its coarser tile', () => {
  const tiles = tilesIndex(false);
  assert.deepEqual(tileAt(tiles, 1, 3, 5), { level: 1, row: 3, column: 5, west: 1280, south: 768, size: 256 });
  assert.deepEqual(parentTile(tiles, tileAt(tiles, 1, 3, 5)), tileAt(tiles, 2, 1, 2));
  assert.equal(parentTile(tiles, tileAt(tiles, 8, 0, 0)), null);
  const north = tilesIndex(true);
  assert.equal(tileAt(north, 1, 0, 0).south, WORLD_M - 256);
});

test('boxAround takes a very long point list', () => {
  const points = Array.from({ length: 400000 }, (_, index) => ({ x: index, y: -index }));
  assert.deepEqual(boxAround(points, 10), { minX: -10, minY: -399999 - 10, maxX: 399999 + 10, maxY: 10 });
  assert.equal(boxAround([], 10), null);
});

const CELL_M = 256;

test('the first kind fills its cell and later kinds are smaller squares in the centre', () => {
  const squares = spawnCellSquares({
    cell_m: CELL_M,
    cells: [
      { kind: 'vitals', x: 2, y: 3, count: 4 },
      { kind: 'landmarks', x: 2, y: 3, count: 1 },
    ],
  });
  const [vitals, landmarks] = squares;
  assert.deepEqual([vitals.x, vitals.y, vitals.side], [2 * CELL_M, 3 * CELL_M, CELL_M]);
  assert.ok(landmarks.side < vitals.side);
  assert.equal(landmarks.x + landmarks.side / 2, 2 * CELL_M + CELL_M / 2);
  assert.equal(landmarks.y + landmarks.side / 2, 3 * CELL_M + CELL_M / 2);
});

test('a fuller cell is more opaque than an emptier cell of the same kind', () => {
  const [full, sparse] = spawnCellSquares({
    cell_m: CELL_M,
    cells: [
      { kind: 'gatherables', x: 0, y: 0, count: 10 },
      { kind: 'gatherables', x: 1, y: 0, count: 1 },
    ],
  });
  assert.ok(full.alpha > sparse.alpha);
});

test('each kind is scaled against its own fullest cell', () => {
  const [vitals, gatherables] = spawnCellSquares({
    cell_m: CELL_M,
    cells: [
      { kind: 'vitals', x: 0, y: 0, count: 2 },
      { kind: 'gatherables', x: 1, y: 0, count: 200 },
    ],
  });
  assert.equal(vitals.alpha, gatherables.alpha);
});
