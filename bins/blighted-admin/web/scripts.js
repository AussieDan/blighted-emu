// SPDX-License-Identifier: AGPL-3.0-or-later
// The script editor (admin studio spec section 7.5): blocks on a canvas joined by links, the
// values of the selected block, Save through the design file route, and the run log.

import { errorRows } from '/web/apply.js';
import { CHANGED_TEXT, HTTP_CHANGED, editHeaders } from '/web/design.js';
import { addLink, graphOf, newNodeId, removeNode, scriptFile, valueOf, valueText } from '/web/script-edit.js';

const BLOCKS_URL = '/api/studio/blocks';
const FILES_URL = '/api/studio/files';
const FILE_URL = '/api/studio/file';
const INFO_URL = '/api/world/info';
const LOG_URL = '/api/live/script-log';
const ADMIN_HEADERS = Object.freeze({ 'X-Blighted-Admin': '1' });
const HTTP_REFUSED = 422;
const SVG = 'http://www.w3.org/2000/svg';
const SCRIPT_KIND = 'script';
const NODE_WIDTH = 170;
const TITLE_HEIGHT = 26;
const PORT_GAP = 22;
const PORT_RADIUS = 6;
/** Where a new block lands, and how far each next one moves down. */
const NEW_X = 40;
const NEW_Y = 40;
const NEW_STEP = 30;
const LOG_POLL_MS = 2000;
const CATEGORIES = Object.freeze(['trigger', 'condition', 'action', 'flow']);

const elements = {
  status: document.getElementById('status'),
  list: document.getElementById('script-list'),
  id: document.getElementById('script-id'),
  problems: document.getElementById('problems'),
  palette: document.getElementById('palette'),
  canvas: document.getElementById('canvas'),
  panel: document.getElementById('node-panel'),
  title: document.getElementById('node-title'),
  help: document.getElementById('node-help'),
  values: document.getElementById('node-values'),
  log: document.getElementById('run-log'),
};
let blocks = new Map();
let level = '';
let files = [];
let graph = { nodes: [], links: [] };
let selected = null;
/** The output a click on a node links from: { node, output }. */
let linking = null;
let placed = 0;

function say(text, isError = false) {
  elements.status.classList.toggle('error', isError);
  elements.status.textContent = text;
}

function showProblems(errors) {
  elements.problems.replaceChildren(...errorRows(errors).map((row) => {
    const item = document.createElement('li');
    item.textContent = `${row.file}${row.field ? ` · ${row.field}` : ''}: ${row.problem}`;
    return item;
  }));
}

async function json(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function svg(tag, attributes, text) {
  const made = document.createElementNS(SVG, tag);
  for (const [name, value] of Object.entries(attributes)) {
    made.setAttribute(name, String(value));
  }
  if (text !== undefined) {
    made.textContent = text;
  }
  return made;
}

/** The screen place of output `index` of a node, and of its input. */
function outputAt(node, index) {
  return { x: node.x + NODE_WIDTH, y: node.y + TITLE_HEIGHT + PORT_GAP * (index + 0.5) };
}

function inputAt(node) {
  return { x: node.x, y: node.y + TITLE_HEIGHT / 2 };
}

function nodeHeight(node) {
  const outputs = blocks.get(node.block)?.outputs.length ?? 1;
  return TITLE_HEIGHT + PORT_GAP * outputs;
}

function draw() {
  const byId = new Map(graph.nodes.map((node) => [node.id, node]));
  const links = graph.links.map((link) => {
    const from = byId.get(link.from);
    const to = byId.get(link.to);
    if (!from || !to) {
      return null;
    }
    const outputs = blocks.get(from.block)?.outputs ?? [];
    const start = outputAt(from, Math.max(outputs.indexOf(link.output), 0));
    const end = inputAt(to);
    const bend = Math.max(Math.abs(end.x - start.x) / 2, 40);
    const path = svg('path', {
      d: `M ${start.x} ${start.y} C ${start.x + bend} ${start.y}, ${end.x - bend} ${end.y}, ${end.x} ${end.y}`,
      class: 'script-link',
    });
    path.addEventListener('click', () => {
      graph = { ...graph, links: graph.links.filter((other) => other !== link) };
      say(`Link ${link.from}.${link.output} → ${link.to} removed.`);
      draw();
    });
    return path;
  }).filter(Boolean);
  const nodes = graph.nodes.map((node) => {
    const spec = blocks.get(node.block);
    const group = svg('g', { class: `script-node-box ${spec?.category ?? ''}`, transform: `translate(${node.x} ${node.y})` });
    group.append(svg('rect', {
      width: NODE_WIDTH,
      height: nodeHeight(node),
      class: node.id === selected ? 'selected' : '',
    }));
    group.append(svg('text', { x: 8, y: 17 }, node.block));
    group.append(svg('circle', { cx: 0, cy: TITLE_HEIGHT / 2, r: PORT_RADIUS, class: 'script-port' }));
    (spec?.outputs ?? []).forEach((output, index) => {
      const at = outputAt({ x: 0, y: 0 }, index);
      group.append(svg('text', { x: NODE_WIDTH - 14, y: at.y + 4, 'text-anchor': 'end' }, output));
      const port = svg('circle', {
        cx: at.x,
        cy: at.y,
        r: PORT_RADIUS,
        class: linking?.node === node.id && linking?.output === output ? 'script-port linking' : 'script-port',
      });
      port.addEventListener('pointerdown', (event) => {
        event.stopPropagation();
        linking = { node: node.id, output };
        say(`Click the block that ${node.id}.${output} goes to.`);
        draw();
      });
      group.append(port);
    });
    group.addEventListener('pointerdown', (event) => startDrag(event, node));
    return group;
  });
  elements.canvas.replaceChildren(...links, ...nodes);
  drawPanel();
}

function startDrag(event, node) {
  if (linking) {
    graph = addLink(graph, linking.node, linking.output, node.id);
    say(`Linked ${linking.node}.${linking.output} → ${node.id}.`);
    linking = null;
    draw();
    return;
  }
  selected = node.id;
  const start = { x: event.clientX, y: event.clientY, nodeX: node.x, nodeY: node.y };
  const move = (moved) => {
    node.x = Math.max(0, start.nodeX + moved.clientX - start.x);
    node.y = Math.max(0, start.nodeY + moved.clientY - start.y);
    draw();
  };
  const end = () => {
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', end);
  };
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', end);
  draw();
}

function drawPanel() {
  const node = graph.nodes.find((candidate) => candidate.id === selected);
  elements.panel.hidden = !node;
  if (!node) {
    return;
  }
  const spec = blocks.get(node.block);
  elements.title.textContent = `${node.id} · ${node.block}`;
  elements.help.textContent = spec?.help ?? 'Unknown block.';
  const focused = document.activeElement?.name;
  elements.values.replaceChildren(...(spec?.values ?? []).map((wanted) => {
    const label = document.createElement('label');
    label.textContent = `${wanted.name}${wanted.required ? '' : ' (optional)'}`;
    const input = document.createElement('input');
    input.type = 'text';
    input.name = wanted.name;
    input.className = 'wide';
    input.value = valueText(node.values[wanted.name]);
    input.placeholder = wanted.kind === 'position' ? 'x, y, z' : wanted.kind;
    input.addEventListener('change', () => {
      const result = valueOf(wanted.kind, input.value);
      if (result.error) {
        say(`${wanted.name}: ${result.error}`, true);
        return;
      }
      if (result.value === null) {
        delete node.values[wanted.name];
      } else {
        node.values[wanted.name] = result.value;
      }
      say(`${node.id}.${wanted.name} set.`);
    });
    label.append(input);
    return label;
  }));
  if (focused) {
    elements.values.elements[focused]?.focus();
  }
}

function addBlock(name) {
  const id = newNodeId(graph, name);
  const offset = (placed % 10) * NEW_STEP;
  placed += 1;
  graph = { ...graph, nodes: [...graph.nodes, { id, block: name, x: NEW_X + offset, y: NEW_Y + offset, values: {} }] };
  selected = id;
  draw();
}

function drawPalette() {
  elements.palette.replaceChildren(...CATEGORIES.flatMap((category) => {
    const heading = document.createElement('h3');
    heading.textContent = category;
    const buttons = [...blocks.values()].filter((block) => block.category === category).map((block) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = block.name;
      button.title = block.help;
      button.addEventListener('click', () => addBlock(block.name));
      return button;
    });
    return [heading, ...buttons];
  }));
}

async function loadFiles() {
  const view = await json(FILES_URL);
  files = view.files.filter((file) => file.kind === SCRIPT_KIND && file.map === level);
  elements.list.replaceChildren(...files.map((file) => {
    const option = document.createElement('option');
    option.value = file.id;
    option.textContent = file.id;
    return option;
  }));
}

function openScript() {
  const file = files.find((candidate) => candidate.id === elements.list.value);
  if (!file) {
    return;
  }
  graph = graphOf(file.fields);
  elements.id.value = file.id;
  selected = null;
  linking = null;
  draw();
  say(`Opened ${file.path}.`);
}

async function save() {
  const written = scriptFile(elements.id.value.trim(), level, graph);
  if (written.error) {
    say(written.error, true);
    return;
  }
  const opened = files.find((file) => file.path === written.path);
  const response = await fetch(`${FILE_URL}?path=${encodeURIComponent(written.path)}`, {
    method: 'PUT',
    headers: editHeaders(ADMIN_HEADERS, opened?.version ?? null),
    body: written.text,
  });
  if (response.status === HTTP_CHANGED) {
    say(`Not saved: ${CHANGED_TEXT}`, true);
    return;
  }
  if (response.status === HTTP_REFUSED) {
    showProblems((await response.json()).errors);
    say(`Not saved: ${written.path} has problems.`, true);
    return;
  }
  if (!response.ok) {
    say(`Not saved: ${response.status} ${await response.text()}`, true);
    return;
  }
  showProblems([]);
  say(`Saved ${written.path}. Apply on the Studio page to run it.`);
  await loadFiles();
  elements.list.value = elements.id.value.trim();
}

async function remove() {
  const id = elements.id.value.trim();
  const file = files.find((candidate) => candidate.id === id);
  if (!file || !window.confirm(`Delete ${file.path}?`)) {
    return;
  }
  const response = await fetch(`${FILE_URL}?path=${encodeURIComponent(file.path)}`, {
    method: 'DELETE',
    headers: editHeaders(ADMIN_HEADERS, file.version),
  });
  if (response.status === HTTP_CHANGED) {
    say(`Not deleted: ${CHANGED_TEXT}`, true);
    return;
  }
  if (!response.ok) {
    say(`Not deleted: ${response.status} ${await response.text()}`, true);
    return;
  }
  graph = { nodes: [], links: [] };
  selected = null;
  draw();
  say(`Deleted ${file.path}.`);
  await loadFiles();
}

/** Shows the run log of the open script, and polls again after a failed read too. */
async function pollLog() {
  try {
    const id = elements.id.value.trim();
    const response = await fetch(LOG_URL);
    if (response.ok) {
      const lines = (await response.json()).filter((line) => line.script === id);
      elements.log.replaceChildren(...lines.map((line) => {
        const item = document.createElement('li');
        const time = document.createElement('time');
        time.textContent = new Date(line.time_ms).toLocaleTimeString();
        const player = line.player === null ? '' : ` · player ${line.player}`;
        item.append(time, `${line.node} · ${line.block}${player}${line.note ? ` · ${line.note}` : ''}`);
        return item;
      }));
    }
  } finally {
    setTimeout(() => pollLog().catch(fail), LOG_POLL_MS);
  }
}

function fail(error) {
  say(String(error.message ?? error), true);
}

document.getElementById('script-form').addEventListener('submit', (event) => {
  event.preventDefault();
  save().catch(fail);
});
document.getElementById('open').addEventListener('click', openScript);
document.getElementById('new').addEventListener('click', () => {
  graph = { nodes: [], links: [] };
  selected = null;
  linking = null;
  draw();
  say('New script: add a trigger from the blocks.');
});
document.getElementById('delete').addEventListener('click', () => remove().catch(fail));
document.getElementById('node-delete').addEventListener('click', () => {
  graph = removeNode(graph, selected);
  selected = null;
  draw();
});
elements.canvas.addEventListener('pointerdown', (event) => {
  if (event.target === elements.canvas) {
    selected = null;
    linking = null;
    draw();
  }
});

const [blockList, info] = await Promise.all([json(BLOCKS_URL), json(INFO_URL)]);
blocks = new Map(blockList.map((block) => [block.name, block]));
level = info.level;
drawPalette();
draw();
loadFiles().catch(fail);
pollLog().catch(fail);
say(`Scripts of ${level}. Click an output dot, then a block, to link them; click a link to remove it.`);
