// SPDX-License-Identifier: AGPL-3.0-or-later
// The live 3D admin page: the world as a low-poly wire model (the terrain from the height grid,
// the rocks, buildings and props from the level collision file, the sea at its level) with every
// player and mob the server sends for the render radius. It follows a picked player, or looks at
// a place picked by its coordinates. The pure helpers are exported so node:test covers them
// without three.js.

import { visibleTiles } from './view.js';

const VIEW_URL = '/api/live/view';
const STREAM_URL = '/api/live/stream';
const GRID_URL = '/api/world/height-grid';
const INFO_URL = '/api/world/info';
const TILE_URL = '/api/world/tiles';
const SHAPES_URL = '/api/world/shapes';
const SPAWNS_URL = '/api/world/spawns';

/** Half side of the terrain square around the followed player, in metres. */
const TERRAIN_HALF_M = 384;
/** Spacing of two height samples, in metres: one wire square of the ground. */
export const TERRAIN_STEP_M = 6;
/** Pixels per metre of the ground picture. */
const GROUND_PX_PER_M = 2;
/** The terrain loads again when the player is this far from its centre, in metres. */
const TERRAIN_RELOAD_M = TERRAIN_HALF_M / 2;
/** Depth used for a terrain sample with no height (off the terrain), below the sea, in metres. */
const NO_GROUND_DEPTH_M = 5;
/** Colour of the ground picture where a map tile is missing. */
const GROUND_FALLBACK = '#8a8061';
/** Opacity of the sea and of the ground wire. */
const SEA_OPACITY = 0.45;
const GROUND_WIRE_OPACITY = 0.35;
/** A solid's outline shows an edge where its faces meet at this angle or more, in degrees. */
const EDGE_ANGLE_DEG = 25;
/** Fog: where it starts and where it hides everything, in metres from the camera. */
const FOG_NEAR_M = 250;
const FOG_FAR_M = 1100;
/** The colours of the scene by theme (style.css: the game panels' night and day). */
const PALETTES = Object.freeze({
  dark: {
    sky: 0x0d0f10, groundFill: 0x16191b, groundWire: 0xc9a24a, shapeFill: 0x23272a, shapeEdge: 0xe4dbc6,
    plantFill: 0x1b281d, plantEdge: 0x86b06e, sea: 0x1d4a6e,
  },
  light: {
    sky: 0xd9cfb3, groundFill: 0xc9bb92, groundWire: 0x6b5a2e, shapeFill: 0xb6a477, shapeEdge: 0x231c11,
    plantFill: 0x93a46f, plantEdge: 0x2b3a20, sea: 0x5b86a6,
  },
});
/**
 * Plant heights by the size word of a tree or bush gatherable id (`TreeHardwoodLarge`,
 * `BushSmall`), in metres. The world file holds no plant size: these are our choice, so a larger
 * gatherable reads larger in the low-poly view.
 */
const TREE_HEIGHTS_M = Object.freeze([['huge', 24], ['large', 18], ['medium', 12], ['small', 8], ['tiny', 5]]);
const TREE_DEFAULT_M = 10;
const BUSH_HEIGHTS_M = Object.freeze([['huge', 3.5], ['large', 2.5], ['medium', 1.8], ['small', 1.2]]);
const BUSH_DEFAULT_M = 1.8;
/** Shape of a low-poly tree, as shares of its height: the trunk, and the cone of leaves on it. */
const TRUNK_TOP = 0.4;
const TRUNK_RADIUS = 0.04;
const CROWN_BASE = 0.3;
const CROWN_RADIUS = 0.3;
/** A bush is one cone from the ground, its base this share of its height across. */
const BUSH_RADIUS = 0.6;
/** Sides of a trunk and of a cone. */
const TRUNK_SIDES = 4;
const CONE_SIDES = 6;
/** The square around the camera is read in this many pieces per side: each piece is one spawn
 * request, under the server's point limit even in a forest. */
const PLANT_PIECES = 2;
const THEME_ATTRIBUTE = 'data-theme';
const LIGHT_THEME = 'light';
/** The sea plane is this many terrain sides wide. */
const SEA_SIDES = 8;

/** The dead mob state in `MobState` order (`asleep, idle, chase, attack, return, dead`). */
export const MOB_STATE_DEAD = 5;
/** Opacity of a dead entity. */
const DEAD_OPACITY = 0.35;
/** Marker sizes, in metres. */
const MARKER_RADIUS_M = 0.5;
const PLAYER_HEIGHT_M = 2;
const MOB_HEIGHT_M = 1.6;
/** Height of the label above the top of the marker, in metres. */
const LABEL_GAP_M = 1;
/** Label texture size, in pixels, and its size in the scene, in metres. */
const LABEL_PX = [256, 64];
const LABEL_M = [6, 1.5];
/** Marker colours. */
const PLAYER_COLOUR = 0x4a90e2;
const BOT_COLOUR = 0x9aa5b1;
const MOB_COLOUR = 0xd43f3f;
const FOLLOWED_COLOUR = 0xffd23f;
/** Ring on the ground under the followed player, in metres. */
const RING_INNER_M = 1.2;
const RING_OUTER_M = 1.6;

/** Camera: nearest and farthest distance, and the start offset behind and above the player. */
const MIN_ZOOM_M = 5;
const MAX_ZOOM_M = 1500;
const START_BACK_M = 70;
const START_UP_M = 55;
/** The camera offset over a place picked by Go to: high enough to clear its rocks and cliffs. */
const PLACE_BACK_M = 320;
const PLACE_UP_M = 260;
const FAR_PLANE_M = 20000;
const FIELD_OF_VIEW_DEG = 55;
/** Pixel ratio cap. */
const MAX_PIXEL_RATIO = 2;

/** Gap between two states when none is measured yet, and its bounds, in milliseconds. */
const DEFAULT_STATE_GAP_MS = 250;
const MIN_STATE_GAP_MS = 50;
const MAX_STATE_GAP_MS = 1000;
/** The player list refresh, in milliseconds. */
const PICKER_REFRESH_MS = 5000;

/**
 * The scene point of a world point. The world is x east, y north, z up; the scene is y up and
 * right-handed, so north goes to -z (a plain swap of y and z would mirror the map).
 * @returns {{ x: number, y: number, z: number }}
 */
export function toScene(x, y, z) {
  return { x, y: z, z: -y };
}

/**
 * Where an entity is drawn `elapsedMs` after a state that moved it from `from` to `to`, when the
 * states come `gapMs` apart: a straight line, then it stays at `to`.
 */
export function lerpPosition(from, to, elapsedMs, gapMs) {
  const t = gapMs > 0 ? Math.max(0, Math.min(1, elapsedMs / gapMs)) : 1;
  return {
    x: from.x + (to.x - from.x) * t,
    y: from.y + (to.y - from.y) * t,
    z: from.z + (to.z - from.z) * t,
  };
}

/** The measured gap between two states, kept inside the bounds. */
export function stateGap(previousMs, nowMs) {
  if (previousMs == null) return DEFAULT_STATE_GAP_MS;
  return Math.max(MIN_STATE_GAP_MS, Math.min(MAX_STATE_GAP_MS, nowMs - previousMs));
}

/** True when an entity of `kind` ('player' or 'mob') is dead. */
export function isDead(entity, kind) {
  return kind === 'mob' ? entity.state === MOB_STATE_DEAD || entity.health === 0 : entity.dead;
}

/** The text of an entity's label. */
export function labelText(entity, kind) {
  const name = kind === 'mob' ? `Mob ${entity.entity}` : entity.name;
  return `${name}  ${entity.health ?? 0}/${entity.max_health ?? 0}`;
}

/**
 * Fight lines from each live attacker to its target when the target is present and alive.
 * @returns {Array<{ fromEntity: number, toEntity: number }>}
 */
export function fightLines(view) {
  const players = view.players ?? [];
  const mobs = view.mobs ?? [];
  const alive = new Set();
  for (const player of players) {
    if (!isDead(player, 'player')) alive.add(player.entity);
  }
  for (const mob of mobs) {
    if (!isDead(mob, 'mob')) alive.add(mob.entity);
  }
  const lines = [];
  for (const attacker of [...players, ...mobs]) {
    if (attacker.target_entity == null) continue;
    if (!alive.has(attacker.entity) || !alive.has(attacker.target_entity)) continue;
    lines.push({ fromEntity: attacker.entity, toEntity: attacker.target_entity });
  }
  return lines;
}

/**
 * The terrain square around (cx, cy): its south-west corner, side and sample count. The corner
 * snaps to the sample step so a reload lines up with the last one.
 */
export function terrainSquare(cx, cy) {
  const x0 = Math.floor((cx - TERRAIN_HALF_M) / TERRAIN_STEP_M) * TERRAIN_STEP_M;
  const y0 = Math.floor((cy - TERRAIN_HALF_M) / TERRAIN_STEP_M) * TERRAIN_STEP_M;
  return { x0, y0, side: 2 * TERRAIN_HALF_M };
}

/**
 * The finest map tiles over a square, each with where it goes on the ground picture (pixels from
 * the north-west corner; the picture is north up).
 */
export function groundTiles(tilesIndex, square, pxPerM) {
  const level = tilesIndex.levels[0].level;
  const view = {
    centerX: square.x0 + square.side / 2,
    centerY: square.y0 + square.side / 2,
    width: square.side,
    height: square.side,
    metresPerPixel: 1,
  };
  const north = square.y0 + square.side;
  return visibleTiles(view, tilesIndex, level).map((tile) => ({
    tile,
    px: (tile.west - square.x0) * pxPerM,
    py: (north - (tile.south + tile.size)) * pxPerM,
    sizePx: tile.size * pxPerM,
  }));
}

/**
 * The scene buffers of `/api/world/shapes` (flat world x, y, z corners and corner indices): the
 * corners turned into scene space (see toScene) and the indices as they are.
 */
export function sceneTriangles(answer) {
  const positions = new Float32Array(answer.points.length);
  for (let corner = 0; corner < answer.points.length; corner += 3) {
    const at = toScene(answer.points[corner], answer.points[corner + 1], answer.points[corner + 2]);
    positions[corner] = at.x;
    positions[corner + 1] = at.y;
    positions[corner + 2] = at.z;
  }
  return { positions, index: Uint32Array.from(answer.triangles) };
}

/**
 * The low-poly kind and height of a gatherable id: a tree, a bush, or null for anything else.
 * @returns {{ tree: boolean, height: number } | null}
 */
export function plantOf(id) {
  const name = id.toLowerCase();
  const sized = (heights, fallback) => heights.find(([word]) => name.includes(word))?.[1] ?? fallback;
  if (name.includes('tree')) return { tree: true, height: sized(TREE_HEIGHTS_M, TREE_DEFAULT_M) };
  if (name.includes('bush')) return { tree: false, height: sized(BUSH_HEIGHTS_M, BUSH_DEFAULT_M) };
  return null;
}

/**
 * The scene buffers of the trees and bushes among gatherable points ({ id, x, y, z }): a tree is
 * a square trunk under a six-sided cone of leaves, a bush one cone on the ground; every cone and
 * trunk is closed. Other gatherables are left out.
 */
export function plantTriangles(points) {
  const positions = [];
  const index = [];
  const corner = (x, y, z) => {
    const at = toScene(x, y, z);
    positions.push(at.x, at.y, at.z);
    return positions.length / 3 - 1;
  };
  const ring = (x, y, z, radius, sides) => Array.from({ length: sides }, (_, side) => {
    const angle = (2 * Math.PI * side) / sides;
    return corner(x + radius * Math.cos(angle), y + radius * Math.sin(angle), z);
  });
  const cone = (x, y, base, top, radius) => {
    const rim = ring(x, y, base, radius, CONE_SIDES);
    const tip = corner(x, y, top);
    const middle = corner(x, y, base);
    rim.forEach((point, side) => {
      const next = rim[(side + 1) % rim.length];
      index.push(point, next, tip, next, point, middle);
    });
  };
  const prism = (x, y, base, top, radius) => {
    const low = ring(x, y, base, radius, TRUNK_SIDES);
    const high = ring(x, y, top, radius, TRUNK_SIDES);
    low.forEach((point, side) => {
      const next = (side + 1) % low.length;
      index.push(point, low[next], high[next], point, high[next], high[side]);
    });
  };
  for (const point of points) {
    const plant = plantOf(point.id);
    if (!plant) continue;
    const { height } = plant;
    if (plant.tree) {
      prism(point.x, point.y, point.z, point.z + height * TRUNK_TOP, height * TRUNK_RADIUS);
      cone(point.x, point.y, point.z + height * CROWN_BASE, point.z + height, height * CROWN_RADIUS);
    } else {
      cone(point.x, point.y, point.z, point.z + height, height * BUSH_RADIUS);
    }
  }
  return { positions: Float32Array.from(positions), index: Uint32Array.from(index) };
}

/** The height of grid row `row` counted from the north edge, with no-ground samples filled. */
export function northRowHeight(heights, size, row, column, fill) {
  const height = heights?.[size - 1 - row]?.[column];
  return height == null ? fill : height;
}

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function labelCanvas(text, ratio) {
  const canvas = document.createElement('canvas');
  [canvas.width, canvas.height] = LABEL_PX;
  const context = canvas.getContext('2d');
  context.fillStyle = 'rgba(0, 0, 0, 0.65)';
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.font = 'bold 26px system-ui, sans-serif';
  context.textAlign = 'center';
  context.fillStyle = '#fff';
  context.fillText(text, canvas.width / 2, 30);
  context.fillStyle = '#333';
  context.fillRect(10, 42, canvas.width - 20, 12);
  context.fillStyle = ratio > 0.5 ? '#4caf50' : ratio > 0.2 ? '#ffb300' : '#e53935';
  context.fillRect(10, 42, (canvas.width - 20) * ratio, 12);
  return canvas;
}

/** The browser entry: the scene, the terrain, the stream and the controls. */
export async function run(THREE, { OrbitControls }) {
  const canvas = document.getElementById('scene');
  const statusElement = document.getElementById('status');
  const playerSelect = document.getElementById('player');
  const mapColours = document.getElementById('map-colours');
  const say = (text, isError = false) => {
    statusElement.classList.toggle('error', isError);
    statusElement.textContent = text;
  };

  const info = await fetchJson(INFO_URL);

  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio ?? 1, MAX_PIXEL_RATIO));
  const scene = new THREE.Scene();
  const palette = () => PALETTES[document.documentElement.getAttribute(THEME_ATTRIBUTE) === LIGHT_THEME ? 'light' : 'dark'];
  scene.background = new THREE.Color();
  scene.fog = new THREE.Fog(0, FOG_NEAR_M, FOG_FAR_M);
  // One material of each kind for the whole page; a reload swaps only the geometry.
  const solid = (extra) => new THREE.MeshLambertMaterial({
    flatShading: true, polygonOffset: true, polygonOffsetFactor: 1, polygonOffsetUnits: 1, ...extra,
  });
  const materials = {
    groundFill: solid({}),
    groundWire: new THREE.LineBasicMaterial({ transparent: true, opacity: GROUND_WIRE_OPACITY }),
    shapeFill: solid({}),
    shapeEdge: new THREE.LineBasicMaterial({}),
    plantFill: solid({}),
    plantEdge: new THREE.LineBasicMaterial({}),
    sea: new THREE.MeshLambertMaterial({ transparent: true, opacity: SEA_OPACITY }),
  };
  function paint() {
    const colours = palette();
    scene.background.setHex(colours.sky);
    scene.fog.color.setHex(colours.sky);
    for (const [name, material] of Object.entries(materials)) {
      material.color.setHex(material.map ? 0xffffff : colours[name]);
    }
  }
  paint();
  new MutationObserver(paint).observe(document.documentElement, { attributes: true, attributeFilter: [THEME_ATTRIBUTE] });
  const camera = new THREE.PerspectiveCamera(FIELD_OF_VIEW_DEG, 1, 0.5, FAR_PLANE_M);
  const controls = new OrbitControls(camera, renderer.domElement);
  controls.minDistance = MIN_ZOOM_M;
  controls.maxDistance = MAX_ZOOM_M;
  controls.maxPolarAngle = Math.PI / 2.05;
  controls.enableDamping = true;
  scene.add(new THREE.HemisphereLight(0xffffff, 0x3a4a5a, 1.6));
  const sun = new THREE.DirectionalLight(0xfff2cc, 1.2);
  sun.position.set(300, 600, 200);
  scene.add(sun);

  const resize = () => {
    const rect = canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    renderer.setSize(rect.width, rect.height, false);
    camera.aspect = rect.width / rect.height;
    camera.updateProjectionMatrix();
  };
  new ResizeObserver(resize).observe(canvas);

  const sea = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), materials.sea);
  sea.geometry.rotateX(-Math.PI / 2);
  sea.visible = false;
  scene.add(sea);

  let terrain = null;
  let shapes = null;
  let plants = null;
  let terrainCentre = null;
  let terrainLoading = false;
  /** Whether the level collision file was found; the page says it once when it is not. */
  let shapesMissingSaid = false;

  /** Takes `group` out of the scene and frees its geometry (the materials stay). */
  function drop(group) {
    if (!group) return;
    scene.remove(group);
    group.traverse((part) => part.geometry?.dispose());
  }

  /** The map tiles painted on a ground picture of `square`, as a texture. */
  function groundPicture(square) {
    const picture = document.createElement('canvas');
    picture.width = picture.height = square.side * GROUND_PX_PER_M;
    const context = picture.getContext('2d');
    context.fillStyle = GROUND_FALLBACK;
    context.fillRect(0, 0, picture.width, picture.height);
    const texture = new THREE.CanvasTexture(picture);
    texture.colorSpace = THREE.SRGBColorSpace;
    texture.anisotropy = renderer.capabilities.getMaxAnisotropy();
    if (info.map_tiles) {
      for (const { tile, px, py, sizePx } of groundTiles(info.tiles, square, GROUND_PX_PER_M)) {
        const image = new Image();
        image.onload = () => {
          context.drawImage(image, px, py, sizePx, sizePx);
          texture.needsUpdate = true;
        };
        image.src = `${TILE_URL}/${tile.level}/${tile.row}/${tile.column}`;
      }
    }
    return texture;
  }

  /** Puts the map tiles on the ground, or takes them off, as the Map colours box says. */
  function paintGround() {
    materials.groundFill.map?.dispose();
    materials.groundFill.map = mapColours.checked && terrain ? groundPicture(terrain.userData.square) : null;
    materials.groundFill.needsUpdate = true;
    paint();
  }

  async function loadShapes(square) {
    const half = square.side / 2;
    const response = await fetch(`${SHAPES_URL}?x=${square.x0 + half}&y=${square.y0 + half}&half=${half}`);
    if (!response.ok) {
      if (!shapesMissingSaid) {
        shapesMissingSaid = true;
        say(`No rocks or buildings: ${await response.text()}`, true);
      }
      return null;
    }
    return outlined(sceneTriangles(await response.json()), materials.shapeFill, materials.shapeEdge);
  }

  /** The filled and outlined solid of scene buffers, with `fill` and `edge` materials. */
  function outlined({ positions, index }, fill, edge) {
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geometry.setIndex(new THREE.BufferAttribute(index, 1));
    const group = new THREE.Group();
    group.add(new THREE.Mesh(geometry, fill), new THREE.LineSegments(new THREE.EdgesGeometry(geometry, EDGE_ANGLE_DEG), edge));
    return group;
  }

  /** The trees and bushes of the square: its gatherables, read in pieces. */
  async function loadPlants(square) {
    const piece = square.side / PLANT_PIECES;
    const requests = [];
    for (let row = 0; row < PLANT_PIECES; row += 1) {
      for (let column = 0; column < PLANT_PIECES; column += 1) {
        const minX = square.x0 + column * piece;
        const minY = square.y0 + row * piece;
        const box = new URLSearchParams({
          min_x: minX, min_y: minY, max_x: minX + piece, max_y: minY + piece, kinds: 'gatherables',
        });
        requests.push(fetchJson(`${SPAWNS_URL}?${box}`));
      }
    }
    return outlined(plantTriangles((await Promise.all(requests)).flat()), materials.plantFill, materials.plantEdge);
  }

  /** Loads the ground, the solids and the sea around (cx, cy); returns the ground height there. */
  async function loadTerrain(cx, cy) {
    terrainLoading = true;
    try {
      const square = terrainSquare(cx, cy);
      const half = square.side / 2;
      const [grid, solids, trees] = await Promise.all([
        fetchJson(`${GRID_URL}?x=${square.x0 + half}&y=${square.y0 + half}&half=${half}&step=${TERRAIN_STEP_M}`),
        loadShapes(square),
        loadPlants(square),
      ]);
      const { size, heights } = grid;
      const fill = info.ocean_level - NO_GROUND_DEPTH_M;
      const geometry = new THREE.PlaneGeometry(square.side, square.side, size - 1, size - 1);
      geometry.rotateX(-Math.PI / 2);
      const positions = geometry.attributes.position;
      for (let row = 0; row < size; row += 1) {
        for (let column = 0; column < size; column += 1) {
          positions.setY(row * size + column, northRowHeight(heights, size, row, column, fill));
        }
      }
      geometry.computeVertexNormals();
      const ground = new THREE.Group();
      ground.add(
        new THREE.Mesh(geometry, materials.groundFill),
        new THREE.LineSegments(new THREE.WireframeGeometry(geometry), materials.groundWire),
      );
      const centre = toScene(square.x0 + half, square.y0 + half, 0);
      ground.position.set(centre.x, 0, centre.z);
      ground.userData.square = square;
      drop(terrain);
      terrain = ground;
      scene.add(terrain);
      paintGround();
      drop(shapes);
      shapes = solids;
      if (shapes) scene.add(shapes);
      drop(plants);
      plants = trees;
      scene.add(plants);

      const seaSide = square.side * SEA_SIDES;
      sea.scale.set(seaSide, 1, seaSide);
      sea.position.set(centre.x, info.ocean_level, centre.z);
      sea.visible = true;
      terrainCentre = { x: cx, y: cy };
      const middle = Math.floor(size / 2);
      return northRowHeight(heights, size, middle, middle, info.ocean_level);
    } finally {
      terrainLoading = false;
    }
  }

  mapColours.addEventListener('change', paintGround);

  const markerGroup = new THREE.Group();
  scene.add(markerGroup);
  const markers = new Map();
  const ring = new THREE.Mesh(
    new THREE.RingGeometry(RING_INNER_M, RING_OUTER_M, 32),
    new THREE.MeshBasicMaterial({ color: FOLLOWED_COLOUR, side: THREE.DoubleSide }),
  );
  ring.geometry.rotateX(-Math.PI / 2);
  ring.visible = false;
  scene.add(ring);
  const lines = new THREE.LineSegments(
    new THREE.BufferGeometry(),
    new THREE.LineBasicMaterial({ color: 0xffffff }),
  );
  scene.add(lines);
  let linePairs = [];

  function colourOf(entity, kind) {
    if (kind === 'mob') return MOB_COLOUR;
    return entity.bot ? BOT_COLOUR : PLAYER_COLOUR;
  }

  function newMarker(entity, kind) {
    const height = kind === 'mob' ? MOB_HEIGHT_M : PLAYER_HEIGHT_M;
    const body = new THREE.Mesh(
      new THREE.CapsuleGeometry(MARKER_RADIUS_M, height - 2 * MARKER_RADIUS_M, 4, 12),
      new THREE.MeshLambertMaterial({ color: colourOf(entity, kind), transparent: true }),
    );
    body.userData = { entity: entity.entity, kind };
    const label = new THREE.Sprite(new THREE.SpriteMaterial({ transparent: true, depthTest: false }));
    label.scale.set(LABEL_M[0], LABEL_M[1], 1);
    label.renderOrder = 1;
    markerGroup.add(body, label);
    const at = toScene(entity.x, entity.y, entity.z);
    return { body, label, kind, height, from: at, to: at, shown: at, labelKey: '' };
  }

  function dropMarker(entity) {
    const marker = markers.get(entity);
    markerGroup.remove(marker.body, marker.label);
    marker.body.geometry.dispose();
    marker.body.material.dispose();
    marker.label.material.map?.dispose();
    marker.label.material.dispose();
    markers.delete(entity);
  }

  function setLabel(marker, entity) {
    const text = labelText(entity, marker.kind);
    if (text === marker.labelKey) return;
    const ratio = entity.max_health > 0 ? Math.min(1, entity.health / entity.max_health) : 0;
    marker.label.material.map?.dispose();
    marker.label.material.map = new THREE.CanvasTexture(labelCanvas(text, ratio));
    marker.label.material.needsUpdate = true;
    marker.labelKey = text;
  }

  let focus = null;
  let lastStateMs = null;
  let stateAtMs = performance.now();
  let gapMs = DEFAULT_STATE_GAP_MS;
  let source = null;

  function onState(view) {
    const now = performance.now();
    gapMs = stateGap(lastStateMs, now);
    lastStateMs = now;
    stateAtMs = now;
    const seen = new Set();
    const place = (entity, kind) => {
      seen.add(entity.entity);
      const marker = markers.get(entity.entity) ?? newMarker(entity, kind);
      markers.set(entity.entity, marker);
      marker.from = marker.shown;
      marker.to = toScene(entity.x, entity.y, entity.z);
      marker.body.material.opacity = isDead(entity, kind) ? DEAD_OPACITY : 1;
      const followed = kind === 'player' && entity.entity === focus;
      marker.body.material.color.setHex(followed ? FOLLOWED_COLOUR : colourOf(entity, kind));
      setLabel(marker, entity);
    };
    for (const player of view.players ?? []) place(player, 'player');
    for (const mob of view.mobs ?? []) place(mob, 'mob');
    for (const entity of [...markers.keys()]) {
      if (!seen.has(entity)) dropMarker(entity);
    }
    linePairs = fightLines(view);

    const self = (view.players ?? []).find((player) => player.entity === focus);
    if (self) {
      const moved = terrainCentre && Math.hypot(self.x - terrainCentre.x, self.y - terrainCentre.y);
      if (!terrainLoading && (!terrainCentre || moved > TERRAIN_RELOAD_M)) {
        loadTerrain(self.x, self.y).catch((error) => say(error.message, true));
      }
      const where = `${self.x.toFixed(1)}, ${self.y.toFixed(1)}, ${self.z.toFixed(1)}`;
      say(`${self.name} at ${where} · HP ${self.health}/${self.max_health} · `
        + `${(view.players ?? []).length} players · ${(view.mobs ?? []).length} mobs near`);
    } else if (focus != null) {
      say('The player is not in the world.', true);
    }
  }

  function placeCamera() {
    const marker = markers.get(focus);
    if (!marker) return;
    const at = marker.shown;
    controls.target.set(at.x, at.y + marker.height / 2, at.z);
    camera.position.set(at.x, at.y + START_UP_M, at.z + START_BACK_M);
    controls.update();
  }

  function openStream() {
    if (source) source.close();
    source = new EventSource(`${STREAM_URL}?focus=${focus}`);
    let first = true;
    source.addEventListener('state', (event) => {
      try {
        onState(JSON.parse(event.data));
        if (first) {
          first = false;
          placeCamera();
        }
      } catch (error) {
        say(error.message, true);
      }
    });
    source.addEventListener('error', () => say('Live stream lost; it reconnects by itself.', true));
  }

  /** Stops following and looks at the world point (x, y) from above and to the south. */
  async function goTo(x, y) {
    focus = null;
    if (source) source.close();
    source = null;
    for (const id of [...markers.keys()]) dropMarker(id);
    linePairs = [];
    say(`Loading the world around ${x.toFixed(0)}, ${y.toFixed(0)}…`);
    const ground = await loadTerrain(x, y);
    const at = toScene(x, y, ground);
    controls.target.set(at.x, at.y, at.z);
    camera.position.set(at.x, at.y + PLACE_UP_M, at.z + PLACE_BACK_M);
    controls.update();
    say(`Looking at ${x.toFixed(0)}, ${y.toFixed(0)}. Pick a player above to follow one.`);
  }

  function follow(entity) {
    focus = entity;
    playerSelect.value = String(entity);
    for (const id of [...markers.keys()]) dropMarker(id);
    terrainCentre = null;
    lastStateMs = null;
    openStream();
  }

  async function refreshPicker() {
    const view = await fetchJson(VIEW_URL);
    const players = [...(view.players ?? [])].sort((a, b) => Number(a.bot) - Number(b.bot)
      || a.name.localeCompare(b.name));
    playerSelect.replaceChildren(...players.map((player) => {
      const option = document.createElement('option');
      option.value = String(player.entity);
      option.textContent = `${player.bot ? 'Bot · ' : ''}${player.name} · ${player.entity}`;
      return option;
    }));
    if (focus != null && players.some((player) => player.entity === focus)) {
      playerSelect.value = String(focus);
    } else if (focus == null && players.length > 0) {
      follow((players.find((player) => !player.bot) ?? players[0]).entity);
    } else if (players.length === 0 && focus == null && !terrainCentre && !terrainLoading) {
      const middle = info.world_m / 2;
      await goTo(middle, middle);
    }
  }

  document.getElementById('goto').addEventListener('submit', (event) => {
    event.preventDefault();
    const x = Number(document.getElementById('goto-x').value);
    const y = Number(document.getElementById('goto-y').value);
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      say('Give x and y in metres.', true);
      return;
    }
    goTo(x, y).catch((error) => say(error.message, true));
  });
  playerSelect.addEventListener('change', () => {
    const entity = Number(playerSelect.value);
    if (Number.isFinite(entity)) follow(entity);
  });
  window.addEventListener('keydown', (event) => {
    if (event.key.toLowerCase() === 'r' && event.target === document.body) placeCamera();
  });

  const raycaster = new THREE.Raycaster();
  let pressAt = null;
  canvas.addEventListener('pointerdown', (event) => {
    pressAt = { x: event.clientX, y: event.clientY };
  });
  canvas.addEventListener('pointerup', (event) => {
    if (!pressAt || Math.hypot(event.clientX - pressAt.x, event.clientY - pressAt.y) > 6) return;
    const rect = canvas.getBoundingClientRect();
    const pointer = new THREE.Vector2(
      ((event.clientX - rect.left) / rect.width) * 2 - 1,
      -((event.clientY - rect.top) / rect.height) * 2 + 1,
    );
    raycaster.setFromCamera(pointer, camera);
    const bodies = [...markers.values()].map((marker) => marker.body);
    const hit = raycaster.intersectObjects(bodies)[0];
    if (hit?.object.userData.kind === 'player' && hit.object.userData.entity !== focus) {
      follow(hit.object.userData.entity);
    }
  });

  function frame() {
    const now = performance.now();
    const followed = markers.get(focus);
    const before = followed ? { ...followed.shown } : null;
    for (const marker of markers.values()) {
      const at = lerpPosition(marker.from, marker.to, now - stateAtMs, gapMs);
      marker.shown = at;
      marker.body.position.set(at.x, at.y + marker.height / 2, at.z);
      marker.label.position.set(at.x, at.y + marker.height + LABEL_GAP_M, at.z);
    }
    if (followed && before) {
      const dx = followed.shown.x - before.x;
      const dy = followed.shown.y - before.y;
      const dz = followed.shown.z - before.z;
      controls.target.x += dx;
      controls.target.y += dy;
      controls.target.z += dz;
      camera.position.x += dx;
      camera.position.y += dy;
      camera.position.z += dz;
      ring.position.set(followed.shown.x, followed.shown.y + 0.05, followed.shown.z);
      ring.visible = true;
    } else {
      ring.visible = false;
    }
    const points = [];
    for (const { fromEntity, toEntity } of linePairs) {
      const from = markers.get(fromEntity);
      const to = markers.get(toEntity);
      if (!from || !to) continue;
      points.push(from.body.position.clone(), to.body.position.clone());
    }
    lines.geometry.dispose();
    lines.geometry = new THREE.BufferGeometry().setFromPoints(points);
    controls.update();
    renderer.render(scene, camera);
    requestAnimationFrame(frame);
  }

  resize();
  await refreshPicker();
  setInterval(() => refreshPicker().catch((error) => say(error.message, true)), PICKER_REFRESH_MS);
  frame();
}

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  const [THREE, controls] = await Promise.all([
    import('three'),
    import('three/addons/controls/OrbitControls.js'),
  ]);
  run(THREE, controls).catch((error) => {
    const statusElement = document.getElementById('status');
    statusElement.classList.add('error');
    statusElement.textContent = error.message;
  });
}
