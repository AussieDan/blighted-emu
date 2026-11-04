// SPDX-License-Identifier: AGPL-3.0-or-later
// The live panel: the state stream of blighted-admin (server status, figures, map, players,
// events) and the bot batch buttons.

import { KINDS, mobEntity, playerEntity, selectionRows } from './entity.js';
import { eventStartRequest, eventStopRequest, sendGmAndSay, spawnRequest } from './gm.js';
import { mountPlayerControls } from './gm-player.js';
import { WorldMap } from './map.js';
import { SERIES, SIGNAL } from './palette.js';
import { STATUS_GROUPS, isMarked, nextStatuses, statusText } from './status.js';
import { sortRows } from './tables.js';
import { TILE_KINDS, TRAIL_POINTS, factElements, formatTile, trailPush } from './tiles.js';

const STREAM_URL = '/api/live/stream';
const BOTS_URL = '/api/live/bots';
const STOP_ALL_URL = '/api/live/bots/stop-all';
const STATE_EVENT = 'state';
const ADMIN_HEADERS = { 'X-Blighted-Admin': '1', 'Content-Type': 'application/json' };
const BATCH_SIZES = [1, 10, 100, 1000];
const BATCH_REFRESH_MS = 1000;
/** Most rows of the player table. */
const PLAYER_ROWS = 500;
const PLAYER_COLUMNS = 8;
const EVENTS_SHOWN = 200;
const SOCKET_LIVE = 'live';
const SOCKET_CLOSED = 'closed';
const SOCKET_CONNECTING = 'connecting';
/** Mob colours in the order of MOB_STATES (entity.js). */
const MOB_COLOURS = [SIGNAL.STONE, SIGNAL.YELLOW, SIGNAL.ORANGE, SIGNAL.RED, SIGNAL.BLUE, SIGNAL.INK];
const PLAYER_COLOUR = SIGNAL.WHITE;
const BOT_COLOUR = SIGNAL.GREEN;
const DEAD_COLOUR = SIGNAL.PURPLE;
const FROZEN_COLOUR = SIGNAL.ICE;
/** The player colour at 60 % (an 8-digit hex colour). */
const TRAIL_ALPHA_HEX = '99';
const TRAIL_COLOUR = `${PLAYER_COLOUR}${TRAIL_ALPHA_HEX}`;
const SIM_COLOURS = SERIES;
const BAD_FACT = 'bad';
/** What each program does, for the CPU line. */
const PROGRAM_ROLES = {
  sim: 'Game world',
  'rep-gateway': 'Network',
  'world-director': 'Map assignment',
  'edge-http': 'Login',
  'nw-testclient': 'Bots',
};
const MARKED_STATUS = 'marked';
const CHANGED_STATUS = 'changed';
/** Browser storage keys of the page layout choices; the values are '1' or '0'. */
const PLAYERS_OPEN_KEY = 'blighted-admin-players-open';
const BIG_MAP_KEY = 'blighted-admin-big-map';
const BIG_MAP_CLASS = 'big-map';
const PLAYER_KEY = 'player-';
const MOB_KEY = 'mob-';

const status = document.getElementById('status');
const socketBox = document.getElementById('socket');
const serversBox = document.getElementById('servers');
const pagesBox = document.getElementById('pages');
const tilesBox = document.getElementById('tiles');
const moreTilesBox = document.getElementById('tiles-more');
const cpuBox = document.getElementById('cpu');
const playersCount = document.getElementById('players-count');
const playersPanel = document.getElementById('players-panel');
/** The inspector section of the selected entity; a new selection opens it. */
const selectedPanel = document.getElementById('selected-panel');
const bigMapButton = document.getElementById('big-map');
const batchesList = document.getElementById('batches');
const eventsList = document.getElementById('events');
const selectionBox = document.getElementById('selection');
const playersBody = document.getElementById('players');
const trailsBox = document.getElementById('trails');
const sleepingBox = document.getElementById('sleeping');

const info = await (await fetch('/api/world/info')).json();
const map = new WorldMap(document.getElementById('map'), info, (point) => {
  mapCursor = point;
});
let source = null;
let socket = SOCKET_CONNECTING;
let view = null;
/** The server statuses by key (status.js), or null before the first state. */
let statuses = null;
/** The drawn status elements by key: kept, so the bar only moves when its state changes. */
const statusElements = new Map();
let trails = new Map();
let selectedKey = null;
/** The batch list as last drawn: it is drawn again only when it changes, so a click lands. */
let shownBatches = '';
let sortColumn = 'entity';
let sortDescending = false;

function playerColour(player) {
  if (player.dead) {
    return DEAD_COLOUR;
  }
  if (player.frozen) {
    return FROZEN_COLOUR;
  }
  return player.bot ? BOT_COLOUR : PLAYER_COLOUR;
}

/** Every entity of the view by map key. */
function entities() {
  const all = new Map();
  for (const player of view?.players ?? []) {
    all.set(`${PLAYER_KEY}${player.entity}`, playerEntity(player));
  }
  for (const mob of view?.mobs ?? []) {
    all.set(`${MOB_KEY}${mob.entity}`, mobEntity(mob));
  }
  return all;
}

/** The six main figures: [label, value, bad] each. */
function mainFigures() {
  const tiles = view?.tiles;
  const p99 = tiles?.tick_p99_ms;
  const slow = p99 !== null && p99 !== undefined && p99 > view.tick_budget_ms;
  // Started bots that are not in a shown snapshot yet: still logging in, or turned away.
  const botsInWorld = (view?.players ?? []).filter((player) => player.bot).length;
  const loggingIn = tiles ? Math.max(tiles.bots - botsInWorld, 0) : null;
  return [
    ['In the world', formatTile(TILE_KINDS.COUNT, tiles?.players), false],
    ['Bots started', formatTile(TILE_KINDS.COUNT, tiles?.bots), false],
    ['Bots not in the world yet', formatTile(TILE_KINDS.COUNT, loggingIn), false],
    ['Worst tick p99', formatTile(TILE_KINDS.MS, p99), slow],
    ['Members sent', formatTile(TILE_KINDS.RATE, tiles?.members_s), false],
    ['Bandwidth out', formatTile(TILE_KINDS.BYTES, tiles?.bytes_out_s), false],
    ['Mobs awake', formatTile(TILE_KINDS.COUNT, tiles?.mobs_awake), false],
  ];
}

/** The other figures, in the small line under the main ones: [label, value] each. */
function moreFigures() {
  const tiles = view?.tiles;
  const rows = [
    ['Sims', formatTile(TILE_KINDS.COUNT, tiles?.sims)],
    ['Tick avg', formatTile(TILE_KINDS.MS, tiles?.tick_avg_ms)],
    ['Packets in', formatTile(TILE_KINDS.RATE, tiles?.datagrams_in_s)],
    ['Packets out', formatTile(TILE_KINDS.RATE, tiles?.datagrams_out_s)],
    ['Bandwidth in', formatTile(TILE_KINDS.BYTES, tiles?.bytes_in_s)],
    ['Mobs asleep', formatTile(TILE_KINDS.COUNT, tiles?.mobs_asleep)],
  ];
  return rows;
}

/** CPU per program, with a plain name for each program (100 % is one full core). */
function cpuFigures() {
  return (view?.tiles?.cpu ?? []).map(([program, percent]) => [
    PROGRAM_ROLES[program] ? `${PROGRAM_ROLES[program]} (${program})` : program,
    formatTile(TILE_KINDS.CPU, percent),
  ]);
}

function showFigures() {
  const main = mainFigures();
  const facts = factElements(main.map(([name, value]) => [name, value]));
  facts.forEach((fact, index) => fact.classList.toggle(BAD_FACT, main[index][2]));
  tilesBox.replaceChildren(...facts);
  moreTilesBox.replaceChildren(...factElements(moreFigures()));
  cpuBox.replaceChildren(...factElements(cpuFigures()));
}

function showSocket() {
  socketBox.dataset.state = socket;
  socketBox.textContent = socket === SOCKET_LIVE ? 'stream live' : `stream ${socket}`;
}

function statusElement(key) {
  let item = statusElements.get(key);
  if (!item) {
    item = document.createElement('div');
    item.className = 'server';
    const state = document.createElement('span');
    state.className = 'state';
    item.append(document.createElement('b'), state, document.createElement('small'));
    item.addEventListener('animationend', () => item.classList.remove(CHANGED_STATUS));
    statusElements.set(key, item);
  }
  return item;
}

function groupElement(title, items) {
  const group = document.createElement('div');
  group.className = 'server-group';
  const head = document.createElement('h2');
  head.textContent = title;
  const row = document.createElement('div');
  row.className = 'server-row';
  row.append(...items);
  group.append(head, row);
  return group;
}

function mutedLine(text) {
  const line = document.createElement('p');
  line.className = 'muted';
  line.textContent = text;
  return line;
}

function showServers(nowMs) {
  if (statuses === null || statuses.size === 0) {
    serversBox.replaceChildren(
      mutedLine(statuses === null ? 'Waiting for the first state.' : 'No sim heartbeat and no metrics page yet.'),
    );
    pagesBox.replaceChildren();
    return;
  }
  const groups = { [STATUS_GROUPS.SIMS]: [], [STATUS_GROUPS.PAGES]: [] };
  for (const [key, server] of statuses) {
    const item = statusElement(key);
    const { reason, detail } = statusText(server, nowMs);
    const [name, stateBox, detailBox] = item.children;
    if (item.dataset.state !== server.state && item.dataset.state !== undefined) {
      item.classList.remove(CHANGED_STATUS);
      // Reading the layout restarts the bar animation.
      void item.offsetWidth;
      item.classList.add(CHANGED_STATUS);
    }
    item.dataset.state = server.state;
    name.textContent = server.name;
    stateBox.textContent = reason;
    detailBox.textContent = detail;
    item.classList.toggle(MARKED_STATUS, isMarked(server, nowMs));
    groups[server.group].push(item);
  }
  for (const key of statusElements.keys()) {
    if (!statuses.has(key)) {
      statusElements.delete(key);
    }
  }
  const sims = groups[STATUS_GROUPS.SIMS];
  const pages = groups[STATUS_GROUPS.PAGES];
  serversBox.replaceChildren(sims.length > 0 ? groupElement('Sims', sims) : mutedLine('No sim heartbeat yet.'));
  pagesBox.replaceChildren(...(pages.length > 0 ? [groupElement('Metrics pages', pages)] : []));
}

function showMap() {
  const table = view?.table;
  map.setCells(
    table
      ? {
        size: table.cell_size_m,
        columns: table.columns,
        rows: table.rows,
        owners: table.owners,
        colors: SIM_COLOURS,
        names: table.sims.map((sim) => sim.id),
      }
      : null,
  );
  const points = [];
  for (const mob of view?.mobs ?? []) {
    points.push({ key: `${MOB_KEY}${mob.entity}`, x: mob.x, y: mob.y, color: MOB_COLOURS[mob.state] ?? MOB_COLOURS[0] });
  }
  for (const player of view?.players ?? []) {
    const key = `${PLAYER_KEY}${player.entity}`;
    // Only the selected player carries its name: names on every dot would hide the map.
    const label = key === selectedKey ? player.name : undefined;
    points.push({ key, x: player.x, y: player.y, color: playerColour(player), label });
  }
  map.setPoints(points);
  map.setPaths(
    trailsBox.checked
      ? [...trails].map(([entity, trail]) => ({ key: `${PLAYER_KEY}${entity}`, color: TRAIL_COLOUR, points: trail }))
      : [],
  );
}

function showSelection() {
  if (selectedKey === null) {
    return;
  }
  const entity = entities().get(selectedKey);
  if (!entity) {
    const gone = document.createElement('p');
    gone.className = 'hint';
    gone.textContent = 'It is not in view any more.';
    selectionBox.replaceChildren(gone);
    return;
  }
  const list = document.createElement('dl');
  list.className = 'props';
  list.append(
    ...selectionRows(entity).flatMap(([label, value]) => {
      const term = document.createElement('dt');
      term.textContent = label;
      const detail = document.createElement('dd');
      detail.textContent = value;
      return [term, detail];
    }),
  );
  selectionBox.replaceChildren(list);
}

function select(key) {
  selectedKey = key;
  if (key !== null) {
    selectedPanel.open = true;
  }
  map.follow(key);
  showMap();
  showSelection();
  showTeleport();
}

const gmStatus = document.getElementById('gm-status');
const gmPlayer = document.getElementById('gm-player');
const gmMob = document.getElementById('gm-mob');
const spawnFields = Object.fromEntries(
  ['vitals', 'level', 'count', 'x', 'y', 'z'].map((name) => [name, document.getElementById(`gm-spawn-${name}`)]),
);
/** The world point under the pointer, for a right-click. */
let mapCursor = null;

/** The selected entity, or undefined. */
function selectedEntity() {
  return selectedKey === null ? undefined : entities().get(selectedKey);
}

/** Shows the GM controls of the selected player, bot or mob. */
function showTeleport() {
  const entity = selectedEntity();
  gmPlayer.hidden = !entity || entity.kind === KINDS.MOB;
  gmMob.hidden = !entity || entity.kind !== KINDS.MOB;
  gmStatus.textContent = '';
  if (entity && entity.kind !== KINDS.MOB) {
    playerControls.fill(entity);
    spawnFields.z.value = entity.z.toFixed(1);
  }
}

function sayGm(text) {
  gmStatus.textContent = text;
}

/** Sends a GM command and shows its answer. */
function gm(path, body) {
  return sendGmAndSay(path, body, sayGm);
}

const playerControls = mountPlayerControls(document.getElementById('gm-player-controls'), {
  entity: () => {
    const entity = selectedEntity();
    return entity && entity.kind !== KINDS.MOB ? entity : undefined;
  },
  say: sayGm,
});

/** Runs `send` with the selected entity, or says there is none. */
function onSelected(send) {
  const entity = selectedEntity();
  if (entity) {
    send(entity);
  } else {
    gmStatus.textContent = 'Select a player or a mob first.';
  }
}

document.getElementById('gm-despawn').addEventListener('click', () => {
  onSelected((entity) => gm('despawn', { entities: [entity.entity] }));
});
document.getElementById('gm-despawn-all').addEventListener('click', () => gm('despawn', { entities: [] }));

document.getElementById('gm-spawn').addEventListener('submit', (event) => {
  event.preventDefault();
  const { request, error } = spawnRequest(
    Object.fromEntries(Object.entries(spawnFields).map(([name, field]) => [name, field.value])),
  );
  if (error) {
    gmStatus.textContent = error;
  } else {
    gm('spawn', request);
  }
});

const EVENTS_URL = '/api/gm/events';
const eventSelect = document.getElementById('gm-event');
const eventsRunning = document.getElementById('gm-events-running');

/** Lists the level's events and the running ones. */
async function loadEvents() {
  const response = await fetch(EVENTS_URL);
  if (!response.ok) {
    eventsRunning.replaceChildren(emptyItem(`Events: ${response.status} ${await response.text()}`));
    return;
  }
  const events = await response.json();
  eventSelect.replaceChildren(...events.defined.map((id) => {
    const option = document.createElement('option');
    option.value = id;
    option.textContent = id;
    return option;
  }));
  eventsRunning.replaceChildren(...(events.running.length === 0
    ? [emptyItem('No event runs.')]
    : events.running.map(([sim, event]) => {
      const item = document.createElement('li');
      item.textContent = `${event.event} on ${sim}: ${event.mobs_alive} mobs alive, ${event.waves_left} waves to come`;
      return item;
    })));
}

document.getElementById('gm-events').addEventListener('submit', (event) => {
  event.preventDefault();
  const { request, error } = eventStartRequest(eventSelect.value, spawnFields.x.value, spawnFields.y.value, spawnFields.z.value);
  if (error) {
    gmStatus.textContent = error;
  } else {
    gm('start-event', request).then(() => loadEvents());
  }
});
document.getElementById('gm-event-stop').addEventListener('click', () => {
  const { request, error } = eventStopRequest(eventSelect.value);
  if (error) {
    gmStatus.textContent = error;
  } else {
    gm('stop-event', request).then(() => loadEvents());
  }
});
/** Lists the events; a failed read shows in the list. */
function refreshEvents() {
  loadEvents().catch((error) => eventsRunning.replaceChildren(emptyItem(`Events: ${error.message ?? error}`)));
}
document.getElementById('gm-events-refresh').addEventListener('click', refreshEvents);
refreshEvents();

document.getElementById('map').addEventListener('contextmenu', (event) => {
  if (!mapCursor) {
    return;
  }
  event.preventDefault();
  spawnFields.x.value = mapCursor.x.toFixed(1);
  spawnFields.y.value = mapCursor.y.toFixed(1);
  gmStatus.textContent = `Map point ${mapCursor.x.toFixed(1)}, ${mapCursor.y.toFixed(1)} set for a spawn.`;
});

/** A list item that says the list is empty. */
function emptyItem(text) {
  const item = document.createElement('li');
  item.className = 'empty';
  item.textContent = text;
  return item;
}

/** A layout choice kept in this browser, or `fallback` without one (or without storage). */
function savedChoice(key, fallback) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? fallback : value === '1';
  } catch {
    return fallback;
  }
}

function saveChoice(key, value) {
  try {
    localStorage.setItem(key, value ? '1' : '0');
  } catch {
    // Without storage the choice lasts until the page is left.
  }
}

function showBigMap(on) {
  document.body.classList.toggle(BIG_MAP_CLASS, on);
  bigMapButton.setAttribute('aria-pressed', String(on));
  bigMapButton.textContent = on ? 'Show panels' : 'Big map';
}

function showPlayers() {
  const rows = (view?.players ?? []).map(playerEntity);
  playersCount.textContent = rows.length > PLAYER_ROWS ? `first ${PLAYER_ROWS} of ${rows.length}` : `${rows.length}`;
  // A folded table is not drawn: thousands of rows each state would cost for nothing.
  if (!playersPanel.open) {
    return;
  }
  const sorted = sortRows(rows, sortColumn, sortDescending).slice(0, PLAYER_ROWS);
  if (sorted.length === 0) {
    const row = document.createElement('tr');
    row.className = 'empty';
    const cell = document.createElement('td');
    cell.colSpan = PLAYER_COLUMNS;
    cell.textContent = 'No player in the shown snapshots.';
    row.append(cell);
    playersBody.replaceChildren(row);
    return;
  }
  playersBody.replaceChildren(
    ...sorted.map((player) => {
      const row = document.createElement('tr');
      row.className = 'clickable';
      row.dataset.key = `${PLAYER_KEY}${player.entity}`;
      for (const [value, numeric] of [
        [player.name, false],
        [player.kind === KINDS.BOT ? 'bot' : 'player', false],
        [player.entity, false],
        [player.sim, false],
        [player.x.toFixed(1), true],
        [player.y.toFixed(1), true],
        [player.z.toFixed(1), true],
        [player.state, false],
      ]) {
        const cell = document.createElement('td');
        cell.textContent = String(value);
        if (numeric) {
          cell.className = 'num';
        }
        row.append(cell);
      }
      return row;
    }),
  );
}

function showEvents() {
  const events = [...(view?.events ?? [])].reverse().slice(0, EVENTS_SHOWN);
  if (events.length === 0) {
    eventsList.replaceChildren(emptyItem('No event yet.'));
    return;
  }
  eventsList.replaceChildren(
    ...events.map((event) => {
      const item = document.createElement('li');
      const time = document.createElement('time');
      time.dateTime = new Date(event.time_ms).toISOString();
      time.textContent = new Date(event.time_ms).toLocaleTimeString();
      item.append(time, event.text);
      return item;
    }),
  );
}

function showAll() {
  const nowMs = Date.now();
  statuses = nextStatuses(statuses, view, nowMs);
  showServers(nowMs);
  showFigures();
  showMap();
  showSelection();
  showPlayers();
  showEvents();
}

function connect() {
  source?.close();
  socket = SOCKET_CONNECTING;
  showSocket();
  source = new EventSource(STREAM_URL);
  source.addEventListener('open', () => {
    socket = SOCKET_LIVE;
    showSocket();
  });
  source.addEventListener('error', () => {
    socket = source.readyState === EventSource.CLOSED ? SOCKET_CLOSED : SOCKET_CONNECTING;
    showSocket();
  });
  source.addEventListener(STATE_EVENT, (event) => {
    view = JSON.parse(event.data);
    trails = trailPush(trails, view.players, TRAIL_POINTS);
    showAll();
  });
}

async function post(url, body) {
  const response = await fetch(url, { method: 'POST', headers: ADMIN_HEADERS, body: body === undefined ? undefined : JSON.stringify(body) });
  if (!response.ok) {
    status.textContent = `${response.status}: ${await response.text()}`;
  }
  await refreshBatches();
}

async function refreshBatches() {
  try {
    const response = await fetch(BOTS_URL);
    const batches = response.ok ? await response.json() : [];
    const drawn = JSON.stringify(batches);
    if (drawn === shownBatches) {
      return;
    }
    shownBatches = drawn;
    if (batches.length === 0) {
      batchesList.replaceChildren(emptyItem('No batch. A Bots button starts one.'));
      return;
    }
    batchesList.replaceChildren(
      ...batches.map((batch) => {
        const item = document.createElement('li');
        item.className = 'batch';
        const text = document.createElement('span');
        text.textContent = `batch ${batch.batch}: ${batch.size} bots, ${batch.state}`;
        const stop = document.createElement('button');
        stop.type = 'button';
        stop.textContent = 'Stop';
        // A waiting batch can be taken back; a stopping one is on its way out.
        stop.disabled = batch.state === 'stopping';
        stop.addEventListener('click', () => post(`${BOTS_URL}/${batch.batch}/stop`));
        item.append(text, stop);
        return item;
      }),
    );
  } catch {
    // The next refresh tries again.
  }
}

const buttons = document.getElementById('bot-buttons');
for (const size of BATCH_SIZES) {
  const button = document.createElement('button');
  button.type = 'button';
  button.textContent = `+${size}`;
  button.addEventListener('click', () => {
    status.textContent = '';
    post(BOTS_URL, { size });
  });
  buttons.append(button);
}
document.getElementById('stop-all').addEventListener('click', () => post(STOP_ALL_URL));
document.getElementById('reconnect').addEventListener('click', connect);
document.getElementById('fit').addEventListener('click', () => map.fit());
playersPanel.open = savedChoice(PLAYERS_OPEN_KEY, false);
playersPanel.addEventListener('toggle', () => {
  saveChoice(PLAYERS_OPEN_KEY, playersPanel.open);
  showPlayers();
});
showBigMap(savedChoice(BIG_MAP_KEY, false));
bigMapButton.addEventListener('click', () => {
  const on = !document.body.classList.contains(BIG_MAP_CLASS);
  saveChoice(BIG_MAP_KEY, on);
  showBigMap(on);
});
trailsBox.addEventListener('change', showMap);
sleepingBox.addEventListener('change', () => map.setSpawnKinds(sleepingBox.checked ? ['vitals'] : []));
// The table is drawn again with every state: a press selects, a click could miss its row.
playersBody.addEventListener('pointerdown', (event) => {
  const key = event.target.closest('tr')?.dataset.key;
  if (key) {
    select(key);
  }
});
map.onSelect((item) => {
  if (item?.key) {
    select(item.key);
  }
});
for (const header of document.querySelectorAll('th.sortable')) {
  header.addEventListener('click', () => {
    const column = header.dataset.column;
    sortDescending = column === sortColumn ? !sortDescending : false;
    sortColumn = column;
    for (const other of document.querySelectorAll('th.sortable')) {
      delete other.dataset.sorted;
    }
    header.dataset.sorted = sortDescending ? 'down' : 'up';
    showPlayers();
  });
}

showServers(Date.now());
showFigures();
showEvents();
showPlayers();
connect();
refreshBatches();
setInterval(refreshBatches, BATCH_REFRESH_MS);
