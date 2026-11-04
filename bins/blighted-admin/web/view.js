// SPDX-License-Identifier: AGPL-3.0-or-later
// The map view: world metres (x east, y north) to screen pixels (y down), zoom, and the tiles
// in view. A view is { centerX, centerY, metresPerPixel, width, height }.

/** Closest zoom: metres per screen pixel. */
export const MIN_METRES_PER_PIXEL = 0.05;

/** Screen pixel of a world point. */
export function worldToScreen(view, x, y) {
  return {
    px: (x - view.centerX) / view.metresPerPixel + view.width / 2,
    py: (view.centerY - y) / view.metresPerPixel + view.height / 2,
  };
}

/** World point of a screen pixel. */
export function screenToWorld(view, px, py) {
  return {
    x: view.centerX + (px - view.width / 2) * view.metresPerPixel,
    y: view.centerY - (py - view.height / 2) * view.metresPerPixel,
  };
}

/** The view zoomed by `factor` (below 1 zooms in) around a screen pixel, which keeps its point. */
export function zoomAt(view, px, py, factor, worldM) {
  const metresPerPixel = Math.min(
    Math.max(view.metresPerPixel * factor, MIN_METRES_PER_PIXEL),
    worldM,
  );
  const point = screenToWorld(view, px, py);
  return {
    ...view,
    metresPerPixel,
    centerX: point.x - (px - view.width / 2) * metresPerPixel,
    centerY: point.y + (py - view.height / 2) * metresPerPixel,
  };
}

/** World box of the view. */
export function boxOf(view) {
  const halfWidth = (view.width / 2) * view.metresPerPixel;
  const halfHeight = (view.height / 2) * view.metresPerPixel;
  return {
    minX: view.centerX - halfWidth,
    minY: view.centerY - halfHeight,
    maxX: view.centerX + halfWidth,
    maxY: view.centerY + halfHeight,
  };
}

/**
 * The coarsest level whose tile pixels are not larger than the screen's, so the map stays sharp;
 * level 1 when the view is closer than level 1.
 */
export function tileLevelFor(view, tilesIndex, tilePx) {
  let chosen = tilesIndex.levels[0].level;
  for (const level of tilesIndex.levels) {
    if (level.tile_m / tilePx <= view.metresPerPixel) {
      chosen = Math.max(chosen, level.level);
    }
  }
  return chosen;
}

/** Grid of `level`, or undefined. */
function gridOf(tilesIndex, level) {
  return tilesIndex.levels.find((entry) => entry.level === level);
}

/**
 * One tile: { level, row, column, west, south, size } in metres. Rows count from the south edge,
 * or from the north edge when the index says `y_index_north_up`.
 */
export function tileAt(tilesIndex, level, row, column) {
  const size = gridOf(tilesIndex, level).tile_m;
  const south = tilesIndex.y_index_north_up
    ? tilesIndex.world_m - (row + 1) * size
    : row * size;
  return { level, row, column, west: column * size, south, size };
}

/** The tile of the next coarser level that holds `tile`; null at the coarsest level. */
export function parentTile(tilesIndex, tile) {
  if (!gridOf(tilesIndex, tile.level + 1)) {
    return null;
  }
  return tileAt(tilesIndex, tile.level + 1, Math.floor(tile.row / 2), Math.floor(tile.column / 2));
}

/** The tiles of `level` in view (see `tileAt`). */
export function visibleTiles(view, tilesIndex, level) {
  const grid = gridOf(tilesIndex, level);
  if (!grid) {
    return [];
  }
  const size = grid.tile_m;
  const worldM = tilesIndex.world_m;
  const box = boxOf(view);
  const firstColumn = Math.max(0, Math.floor(box.minX / size));
  const lastColumn = Math.min(grid.columns - 1, Math.floor(box.maxX / size));
  const northUp = tilesIndex.y_index_north_up;
  const rowOf = (y) => (northUp ? Math.floor((worldM - y) / size) : Math.floor(y / size));
  const firstRow = Math.max(0, Math.min(rowOf(box.minY), rowOf(box.maxY)));
  const lastRow = Math.min(grid.rows - 1, Math.max(rowOf(box.minY), rowOf(box.maxY)));
  const tiles = [];
  for (let row = firstRow; row <= lastRow; row += 1) {
    for (let column = firstColumn; column <= lastColumn; column += 1) {
      tiles.push(tileAt(tilesIndex, level, row, column));
    }
  }
  return tiles;
}

/** The world box around points ({ x, y }) with a margin in metres; null for no point. */
export function boxAround(points, margin) {
  if (points.length === 0) {
    return null;
  }
  // A loop, not Math.min(...): a spread of a few 100,000 values overflows the call stack.
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const point of points) {
    minX = Math.min(minX, point.x);
    minY = Math.min(minY, point.y);
    maxX = Math.max(maxX, point.x);
    maxY = Math.max(maxY, point.y);
  }
  return { minX: minX - margin, minY: minY - margin, maxX: maxX + margin, maxY: maxY + margin };
}

/** The order of the kinds in an overview cell: the first fills the cell, each next one is smaller. */
const SPAWN_KIND_ORDER = Object.freeze(['vitals', 'gatherables', 'landmarks']);
/** Opacity of an overview square: the fewest points of a kind, then the most. */
const OVERVIEW_ALPHA_MIN = 0.25;
const OVERVIEW_ALPHA_MAX = 0.85;
/**
 * The squares of a zoomed out map from `/api/world/spawn-cells`: one per cell and kind, in world
 * metres from the south-west corner. The first kind of SPAWN_KIND_ORDER fills its cell; each next
 * kind is a smaller square in the centre, so every kind stays seen. A square is more opaque the
 * more points of its kind the cell holds, against the fullest cell of that kind.
 */
export function spawnCellSquares(answer) {
  const most = new Map();
  for (const cell of answer.cells) {
    most.set(cell.kind, Math.max(most.get(cell.kind) ?? 0, cell.count));
  }
  const shares = SPAWN_KIND_ORDER.length;
  return answer.cells.map((cell) => {
    const rank = Math.max(SPAWN_KIND_ORDER.indexOf(cell.kind), 0);
    const side = (answer.cell_m * (shares - rank)) / shares;
    const inset = (answer.cell_m - side) / 2;
    const fullness = cell.count / most.get(cell.kind);
    return {
      kind: cell.kind,
      x: cell.x * answer.cell_m + inset,
      y: cell.y * answer.cell_m + inset,
      side,
      alpha: OVERVIEW_ALPHA_MIN + (OVERVIEW_ALPHA_MAX - OVERVIEW_ALPHA_MIN) * fullness,
    };
  });
}
