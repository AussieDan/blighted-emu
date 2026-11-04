// SPDX-License-Identifier: AGPL-3.0-or-later
// The world map on a 2D canvas: map tiles or relief tiles, sim cells, spawn points, paths and
// points, with pan, zoom, follow and pick. Shared by every admin page.

import { POINT_RING, SIGNAL } from './palette.js';
import {
  boxOf,
  spawnCellSquares,
  parentTile,
  screenToWorld,
  tileLevelFor,
  visibleTiles,
  worldToScreen,
  zoomAt,
} from './view.js';

/** Pixels of a map tile and of a relief tile. */
export const MAP_TILE_PX = 1024;
export const RELIEF_TILE_PX = 256;
/** Spawn points are fetched only closer than this (metres per pixel). */
export const SPAWN_ZOOM_M_PER_PX = 4;
/** Base layers. */
export const LAYERS = Object.freeze({ TILES: 'tiles', RELIEF: 'relief', BOTH: 'both' });

const SPAWN_COLOURS = { vitals: SIGNAL.RED, gatherables: SIGNAL.GREEN, landmarks: SIGNAL.YELLOW };
const SPAWN_CELLS_URL = '/api/world/spawn-cells';

const LABEL_M_PER_PX = 0.6;
const WHEEL_STEP = 1.2;
const BUTTON_STEP = 2;
/** A fitted box fills this share of the canvas less, so the corner tools cover no land. */
const FIT_MARGIN = 1.25;
const PICK_PX = 12;
/** A press becomes a drag only past this distance from where it started; less is a click. */
const DRAG_START_PX = 6;
const POINT_RADIUS_PX = 4;
const SPAWN_RADIUS_PX = 3;
const PATH_WIDTH_PX = 2;
const OVERLAY_ALPHA = 0.5;
const CELL_FILL_ALPHA = 0.15;
const CELL_LINE_WIDTH_PX = 1;
/** The centre of a cell, in cells from its corner. */
const CELL_CENTRE = 0.5;
const SPAWN_FETCH_DELAY_MS = 150;
const POINT_RING_PX = 1.5;
/** Weight and size of the labels; the family is the page's sign face. */
const LABEL_WEIGHT_SIZE = '600 12px';
const LABEL_INK = SIGNAL.INK;
const LABEL_HALO = SIGNAL.WHITE;
const LABEL_HALO_PX = 3;
const LABEL_GAP_PX = 2;
const LABEL_DROP_PX = 4;
/** CSS properties of style.css: the ground outside the tiles (per theme) and the sign face. */
const SEA_PROPERTY = '--map-sea';
const SIGN_FONT_PROPERTY = '--font-sign';
const THEME_ATTRIBUTE = 'data-theme';

export class WorldMap {
  /**
   * @param canvas the canvas to draw on
   * @param info `/api/world/info`
   * @param onCursor called with the world point under the mouse, or null
   */
  constructor(canvas, info, onCursor = () => {}) {
    this.canvas = canvas;
    this.context = canvas.getContext('2d');
    this.info = info;
    this.onCursor = onCursor;
    this.layer = info.map_tiles ? LAYERS.TILES : LAYERS.RELIEF;
    this.spawnKinds = new Set();
    this.spawns = [];
    // The zoomed out overview: its squares and the kinds they were fetched for.
    this.overview = [];
    this.overviewKinds = '';
    this.overviewRequest = 0;
    this.paths = [];
    this.points = [];
    this.cells = null;
    this.followKey = null;
    this.selectHandler = () => {};
    this.images = new Map();
    this.spawnTimer = null;
    this.spawnRequest = 0;
    this.view = { centerX: 0, centerY: 0, metresPerPixel: 1, width: 1, height: 1 };
    this.labelFont = '';
    this.drawQueued = false;
    // The fitted box, kept while the viewer has not moved the view: a resize fits it again (the
    // first fit waits for the canvas to have its laid-out size).
    this.fitBox = this.worldBox();
    this.resize();
    this.listen();
    // The sea colour belongs to the theme (theme.js sets it on the root): draw again on a change.
    new MutationObserver(() => this.redraw()).observe(document.documentElement, {
      attributes: true,
      attributeFilter: [THEME_ATTRIBUTE],
    });
  }

  /** Sets the base layer (one of LAYERS). */
  setLayer(layer) {
    this.layer = layer;
    this.redraw();
  }

  /**
   * Sets the spawn kinds to show ('vitals', 'gatherables', 'landmarks'). Zoomed in, each point
   * shows; zoomed out, a square per cell shows where the points are.
   */
  setSpawnKinds(kinds) {
    this.spawnKinds = new Set(kinds);
    this.fetchSpawnsSoon();
  }

  /** Paths: [{ key, color, points: [{ x, y }] }]. */
  setPaths(paths) {
    this.paths = paths;
    this.redraw();
  }

  /** Points: [{ key, x, y, color, label }]. */
  setPoints(points) {
    this.points = points;
    this.redraw();
  }

  /**
   * Sim cells: { size, columns, rows, owners, colors, names } (owners row by row from the
   * south-west corner, each an index into `colors` and `names` or null), or null for none. Each
   * sim's ground is labelled with its name at the centre of its cells.
   */
  setCells(cells) {
    this.cells = cells;
    this.redraw();
  }

  /** Keeps the point or the path end of `key` in the centre; null stops. */
  follow(key) {
    this.followKey = key;
    if (key !== null) {
      this.fitBox = null;
    }
    this.redraw();
  }

  /** Called with the picked point or path, or null. */
  onSelect(handler) {
    this.selectHandler = handler;
  }

  /** The box of the whole world. */
  worldBox() {
    return { minX: 0, minY: 0, maxX: this.info.world_m, maxY: this.info.world_m };
  }

  /** Shows a world box, or the whole world, with room for the tools on the map's corners. */
  fit(box = this.worldBox()) {
    this.fitBox = box;
    const width = Math.max(box.maxX - box.minX, 1);
    const height = Math.max(box.maxY - box.minY, 1);
    this.view = {
      ...this.view,
      centerX: (box.minX + box.maxX) / 2,
      centerY: (box.minY + box.maxY) / 2,
      metresPerPixel: Math.max(width / this.view.width, height / this.view.height) * FIT_MARGIN,
    };
    this.changed();
  }

  /** Zooms in (`steps` > 0) or out around the centre. */
  zoom(steps) {
    this.fitBox = null;
    this.view = zoomAt(
      this.view,
      this.view.width / 2,
      this.view.height / 2,
      BUTTON_STEP ** -steps,
      this.info.world_m,
    );
    this.changed();
  }

  /** Matches the canvas to its displayed size. */
  resize() {
    const ratio = window.devicePixelRatio || 1;
    const width = Math.max(this.canvas.clientWidth, 1);
    const height = Math.max(this.canvas.clientHeight, 1);
    this.canvas.width = Math.round(width * ratio);
    this.canvas.height = Math.round(height * ratio);
    this.context.setTransform(ratio, 0, 0, ratio, 0, 0);
    this.view = { ...this.view, width, height };
    if (this.fitBox && this.canvas.clientWidth > 0 && this.canvas.clientHeight > 0) {
      this.fit(this.fitBox);
      return;
    }
    this.changed();
  }

  /** The current view. */
  currentView() {
    return { ...this.view };
  }

  listen() {
    new ResizeObserver(() => this.resize()).observe(this.canvas);
    let drag = null;
    this.canvas.addEventListener('pointerdown', (event) => {
      drag = { startX: event.offsetX, startY: event.offsetY, x: event.offsetX, y: event.offsetY, moved: false };
      this.canvas.setPointerCapture(event.pointerId);
    });
    this.canvas.addEventListener('pointermove', (event) => {
      this.onCursor(screenToWorld(this.view, event.offsetX, event.offsetY));
      if (!drag) {
        return;
      }
      if (!drag.moved && Math.hypot(event.offsetX - drag.startX, event.offsetY - drag.startY) <= DRAG_START_PX) {
        return;
      }
      drag.moved = true;
      const dx = event.offsetX - drag.x;
      const dy = event.offsetY - drag.y;
      this.view = {
        ...this.view,
        centerX: this.view.centerX - dx * this.view.metresPerPixel,
        centerY: this.view.centerY + dy * this.view.metresPerPixel,
      };
      drag.x = event.offsetX;
      drag.y = event.offsetY;
      this.followKey = null;
      this.fitBox = null;
      this.changed();
    });
    this.canvas.addEventListener('pointerup', (event) => {
      if (drag && !drag.moved) {
        this.selectHandler(this.pick(event.offsetX, event.offsetY));
      }
      drag = null;
    });
    this.canvas.addEventListener('pointerleave', () => this.onCursor(null));
    this.canvas.addEventListener(
      'wheel',
      (event) => {
        event.preventDefault();
        this.fitBox = null;
        const factor = event.deltaY > 0 ? WHEEL_STEP : 1 / WHEEL_STEP;
        this.view = zoomAt(this.view, event.offsetX, event.offsetY, factor, this.info.world_m);
        this.changed();
        this.onCursor(screenToWorld(this.view, event.offsetX, event.offsetY));
      },
      { passive: false },
    );
  }

  changed() {
    this.fetchSpawnsSoon();
    this.redraw();
  }

  redraw() {
    if (this.drawQueued) {
      return;
    }
    this.drawQueued = true;
    requestAnimationFrame(() => {
      this.drawQueued = false;
      this.draw();
    });
  }

  fetchSpawnsSoon() {
    clearTimeout(this.spawnTimer);
    if (this.spawnKinds.size === 0 || this.zoomedOut()) {
      this.spawns = [];
      this.fetchOverview();
      this.redraw();
      return;
    }
    this.spawnTimer = setTimeout(() => this.fetchSpawns(), SPAWN_FETCH_DELAY_MS);
  }

  /** Whether the view is too far out for single spawn points. */
  zoomedOut() {
    return this.view.metresPerPixel > SPAWN_ZOOM_M_PER_PX;
  }

  /** Fetches the overview of the kinds now shown, once per set of kinds. */
  async fetchOverview() {
    const kinds = [...this.spawnKinds].sort().join(',');
    if (kinds === this.overviewKinds) {
      return;
    }
    this.overviewKinds = kinds;
    const request = ++this.overviewRequest;
    if (kinds === '') {
      this.overview = [];
      return;
    }
    try {
      const response = await fetch(`${SPAWN_CELLS_URL}?${new URLSearchParams({ kinds })}`);
      const squares = response.ok ? spawnCellSquares(await response.json()) : [];
      if (request === this.overviewRequest) {
        this.overview = squares;
        this.redraw();
      }
    } catch {
      // A failed fetch shows no overview; the next change of kinds fetches again.
      this.overviewKinds = '';
    }
  }

  async fetchSpawns() {
    const request = ++this.spawnRequest;
    const box = boxOf(this.view);
    const query = new URLSearchParams({
      min_x: box.minX,
      min_y: box.minY,
      max_x: box.maxX,
      max_y: box.maxY,
      kinds: [...this.spawnKinds].join(','),
    });
    try {
      const response = await fetch(`/api/world/spawns?${query}`);
      const spawns = response.ok ? await response.json() : [];
      if (request === this.spawnRequest) {
        this.spawns = spawns;
        this.redraw();
      }
    } catch {
      // A failed fetch keeps the last points; the next move fetches again.
    }
  }

  followed() {
    if (this.followKey === null) {
      return null;
    }
    const point = this.points.find((item) => item.key === this.followKey);
    if (point) {
      return point;
    }
    const path = this.paths.find((item) => item.key === this.followKey);
    return path && path.points.length > 0 ? path.points[path.points.length - 1] : null;
  }

  draw() {
    const target = this.followed();
    if (target) {
      this.view = { ...this.view, centerX: target.x, centerY: target.y };
    }
    const context = this.context;
    const style = getComputedStyle(this.canvas);
    this.labelFont = `${LABEL_WEIGHT_SIZE} ${style.getPropertyValue(SIGN_FONT_PROPERTY)}`;
    context.fillStyle = style.getPropertyValue(SEA_PROPERTY);
    context.fillRect(0, 0, this.view.width, this.view.height);
    if (this.layer !== LAYERS.RELIEF && this.info.map_tiles) {
      this.drawTiles('tiles', MAP_TILE_PX, 1);
    }
    if (this.layer !== LAYERS.TILES || !this.info.map_tiles) {
      const alpha = this.layer === LAYERS.BOTH && this.info.map_tiles ? OVERLAY_ALPHA : 1;
      this.drawTiles('relief', RELIEF_TILE_PX, alpha);
    }
    this.drawCells();
    if (this.zoomedOut()) {
      this.drawOverview();
    } else {
      this.drawSpawns();
    }
    this.drawPaths();
    this.drawPoints();
  }

  drawTiles(kind, tilePx, alpha) {
    const tiles = this.info.tiles;
    const level = tileLevelFor(this.view, tiles, tilePx);
    this.context.globalAlpha = alpha;
    for (const tile of visibleTiles(this.view, tiles, level)) {
      this.drawTile(kind, tile, tile);
    }
    this.context.globalAlpha = 1;
  }

  /** Draws `area` (a tile rectangle) from `tile`'s image, or from a coarser tile's. */
  drawTile(kind, tile, area) {
    const image = this.image(kind, tile);
    if (!image.complete || image.naturalWidth === 0) {
      const parent = parentTile(this.info.tiles, tile);
      if (parent) {
        this.drawTile(kind, parent, area);
      }
      return;
    }
    const scale = image.naturalWidth / tile.size;
    const sourceX = (area.west - tile.west) * scale;
    const sourceY = (tile.south + tile.size - (area.south + area.size)) * scale;
    const topLeft = worldToScreen(this.view, area.west, area.south + area.size);
    const side = area.size / this.view.metresPerPixel;
    this.context.drawImage(
      image,
      sourceX,
      sourceY,
      area.size * scale,
      area.size * scale,
      topLeft.px,
      topLeft.py,
      side + 0.5,
      side + 0.5,
    );
  }

  image(kind, tile) {
    const url = `/api/world/${kind}/${tile.level}/${tile.row}/${tile.column}`;
    let image = this.images.get(url);
    if (!image) {
      image = new Image();
      image.onload = () => this.redraw();
      image.src = url;
      this.images.set(url, image);
    }
    return image;
  }

  drawCells() {
    if (!this.cells) {
      return;
    }
    const { size, columns, owners, colors } = this.cells;
    const context = this.context;
    const side = size / this.view.metresPerPixel;
    context.lineWidth = CELL_LINE_WIDTH_PX;
    owners.forEach((owner, index) => {
      if (owner === null || owner === undefined) {
        return;
      }
      const column = index % columns;
      const row = Math.floor(index / columns);
      const topLeft = worldToScreen(this.view, column * size, (row + 1) * size);
      context.globalAlpha = CELL_FILL_ALPHA;
      context.fillStyle = colors[owner % colors.length];
      context.fillRect(topLeft.px, topLeft.py, side, side);
      context.globalAlpha = 1;
      context.strokeStyle = colors[owner % colors.length];
      context.strokeRect(topLeft.px, topLeft.py, side, side);
    });
    this.drawCellNames();
  }

  /** The name of each sim at the centre of the cells it owns. */
  drawCellNames() {
    const { size, columns, owners, names } = this.cells;
    if (!names) {
      return;
    }
    const sums = new Map();
    owners.forEach((owner, index) => {
      if (owner === null || owner === undefined) {
        return;
      }
      const sum = sums.get(owner) ?? { x: 0, y: 0, count: 0 };
      sum.x += (index % columns) + CELL_CENTRE;
      sum.y += Math.floor(index / columns) + CELL_CENTRE;
      sum.count += 1;
      sums.set(owner, sum);
    });
    this.context.font = this.labelFont;
    for (const [owner, sum] of sums) {
      const name = names[owner];
      if (name) {
        const { px, py } = worldToScreen(this.view, (sum.x / sum.count) * size, (sum.y / sum.count) * size);
        this.drawLabel(name, px, py);
      }
    }
  }

  drawOverview() {
    const context = this.context;
    for (const square of this.overview) {
      const topLeft = worldToScreen(this.view, square.x, square.y + square.side);
      const side = square.side / this.view.metresPerPixel;
      context.globalAlpha = square.alpha;
      context.fillStyle = SPAWN_COLOURS[square.kind];
      context.fillRect(topLeft.px, topLeft.py, side, side);
    }
    context.globalAlpha = 1;
  }

  drawSpawns() {
    const context = this.context;
    const labels = this.view.metresPerPixel <= LABEL_M_PER_PX;
    context.font = this.labelFont;
    for (const spawn of this.spawns) {
      const { px, py } = worldToScreen(this.view, spawn.x, spawn.y);
      this.drawDot(px, py, SPAWN_RADIUS_PX, SPAWN_COLOURS[spawn.kind]);
      if (labels) {
        const level = spawn.level === null ? '' : ` ${spawn.level}`;
        this.drawLabel(`${spawn.id}${level}`, px + SPAWN_RADIUS_PX, py);
      }
    }
  }

  drawPaths() {
    const context = this.context;
    context.lineWidth = PATH_WIDTH_PX;
    for (const path of this.paths) {
      if (path.points.length === 0) {
        continue;
      }
      context.strokeStyle = path.color;
      context.beginPath();
      path.points.forEach((point, index) => {
        const { px, py } = worldToScreen(this.view, point.x, point.y);
        if (index === 0) {
          context.moveTo(px, py);
        } else {
          context.lineTo(px, py);
        }
      });
      context.stroke();
    }
  }

  drawPoints() {
    const context = this.context;
    context.font = this.labelFont;
    for (const point of this.points) {
      const { px, py } = worldToScreen(this.view, point.x, point.y);
      this.drawDot(px, py, POINT_RADIUS_PX, point.color);
      if (point.label) {
        this.drawLabel(point.label, px + POINT_RADIUS_PX, py);
      }
    }
  }

  /** A filled dot with a thin ring, so it shows on light and dark ground. */
  drawDot(px, py, radius, color) {
    const context = this.context;
    context.fillStyle = color;
    context.strokeStyle = POINT_RING;
    context.lineWidth = POINT_RING_PX;
    context.beginPath();
    context.arc(px, py, radius, 0, Math.PI * 2);
    context.fill();
    context.stroke();
  }

  /** Ink text on a white halo to the right of a dot. */
  drawLabel(text, px, py) {
    const context = this.context;
    const x = px + LABEL_GAP_PX;
    const y = py + LABEL_DROP_PX;
    context.lineWidth = LABEL_HALO_PX;
    context.strokeStyle = LABEL_HALO;
    context.strokeText(text, x, y);
    context.fillStyle = LABEL_INK;
    context.fillText(text, x, y);
  }

  /** The point, path or spawn nearest to a screen pixel within PICK_PX, or null. */
  pick(px, py) {
    let best = null;
    let bestDistance = PICK_PX;
    const consider = (item, x, y) => {
      const screen = worldToScreen(this.view, x, y);
      const distance = Math.hypot(screen.px - px, screen.py - py);
      if (distance <= bestDistance) {
        best = item;
        bestDistance = distance;
      }
    };
    for (const point of this.points) {
      consider(point, point.x, point.y);
    }
    for (const path of this.paths) {
      for (const point of path.points) {
        consider(path, point.x, point.y);
      }
    }
    for (const spawn of this.spawns) {
      consider(spawn, spawn.x, spawn.y);
    }
    return best;
  }
}
