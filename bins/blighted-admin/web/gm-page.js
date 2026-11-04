// SPDX-License-Identifier: AGPL-3.0-or-later
// The GM page: the players online and the GM controls of the picked one, bans, ranks (head
// admin) and the audit log.

import { playerEntity, selectionRows } from '/web/entity.js';
import { sendGmAndSay } from '/web/gm.js';
import { mountPlayerControls } from '/web/gm-player.js';

const ME_URL = '/api/auth/me';
const LOGOUT_URL = '/api/auth/logout';
const AUDIT_URL = '/api/gm/audit?limit=200';
const RANKS_URL = '/api/gm/ranks';
const VIEW_URL = '/api/live/view';
/** How often the players online list is read again. */
const ONLINE_REFRESH_MS = 2000;
const ADMIN_HEADERS = Object.freeze({ 'X-Blighted-Admin': '1' });
const HEAD_ADMIN = 'head_admin';
const RANK_NAMES = Object.freeze({ csr: 'CSR', game_master: 'Game master', head_admin: 'Head admin' });

/** The columns of the audit table: time, who, rank, command, target, result. */
const AUDIT_COLUMNS = 6;
const status = document.getElementById('status');

function cell(text) {
  const td = document.createElement('td');
  td.textContent = text;
  return td;
}

async function loadAudit() {
  const response = await fetch(AUDIT_URL);
  const rows = response.ok ? await response.json() : [];
  document.getElementById('audit-count').textContent = `${rows.length} rows`;
  if (rows.length === 0) {
    const empty = document.createElement('tr');
    empty.className = 'empty';
    const text = cell(response.ok ? 'No GM command and no login is in the audit log yet.' : 'The audit log cannot be read.');
    text.colSpan = AUDIT_COLUMNS;
    empty.append(text);
    document.getElementById('audit').replaceChildren(empty);
    return;
  }
  document.getElementById('audit').replaceChildren(...rows.map((row) => {
    const tr = document.createElement('tr');
    const when = row.at_unix_ms ? new Date(row.at_unix_ms).toLocaleString() : '';
    const entry = row.entry;
    tr.append(
      cell(when),
      cell(entry.account ?? 'local'),
      cell(RANK_NAMES[entry.rank] ?? ''),
      cell(entry.command),
      cell(entry.target),
      cell(entry.result),
    );
    return tr;
  }));
}

async function loadRanks() {
  const response = await fetch(RANKS_URL);
  if (!response.ok) {
    return;
  }
  const rows = await response.json();
  document.getElementById('ranks').replaceChildren(...rows.map((row) => {
    const item = document.createElement('li');
    item.textContent = `${row.username ?? row.account}: ${RANK_NAMES[row.rank] ?? row.rank}`;
    return item;
  }));
}

function say(text) {
  status.textContent = text;
}

async function run(path, body) {
  await sendGmAndSay(path, body, say);
  await Promise.all([loadAudit(), loadRanks()]);
}

const showBots = document.getElementById('show-bots');
const picked = document.getElementById('picked');
/** The players online by entity number, as last read. */
let online = new Map();
let pickedEntity = null;

const controls = mountPlayerControls(document.getElementById('picked-controls'), {
  entity: () => online.get(pickedEntity),
  say,
  sent: () => loadAudit(),
});

function showPicked() {
  const player = online.get(pickedEntity);
  picked.hidden = !player;
  if (!player) {
    return;
  }
  document.getElementById('picked-name').textContent = player.name;
  document.getElementById('picked-facts').textContent = selectionRows(player)
    .filter(([label]) => label !== 'Name')
    .map(([label, value]) => `${label}: ${value}`)
    .join(' · ');
}

function showOnline() {
  const shown = [...online.values()]
    .filter((player) => showBots.checked || !player.bot)
    .sort((a, b) => a.name.localeCompare(b.name));
  const bots = online.size - [...online.values()].filter((player) => !player.bot).length;
  document.getElementById('online-count').textContent = `${online.size - bots} players, ${bots} bots`;
  const list = document.getElementById('online');
  if (shown.length === 0) {
    const empty = document.createElement('li');
    empty.className = 'empty';
    empty.textContent = 'No one is in the game now.';
    list.replaceChildren(empty);
    return;
  }
  list.replaceChildren(...shown.map((player) => {
    const item = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'online-player';
    button.setAttribute('aria-pressed', String(player.entity === pickedEntity));
    button.textContent = `${player.name} · ${player.sim} · ${player.state}`;
    button.addEventListener('click', () => {
      const first = pickedEntity !== player.entity;
      pickedEntity = player.entity;
      showOnline();
      showPicked();
      if (first) {
        controls.fill(player);
      }
    });
    item.append(button);
    return item;
  }));
}

async function loadOnline() {
  const response = await fetch(VIEW_URL);
  if (!response.ok) {
    say(`Players online: ${response.status} ${await response.text()}`);
    return;
  }
  const view = await response.json();
  online = new Map((view.players ?? []).map((player) => [player.entity, playerEntity(player)]));
  showOnline();
  showPicked();
}

showBots.addEventListener('change', showOnline);

const banName = () => document.getElementById('ban-name').value;
document.getElementById('ban').addEventListener('submit', (event) => {
  event.preventDefault();
  run('ban', { username: banName(), banned: true });
});
document.getElementById('unban').addEventListener('click', () => run('ban', { username: banName(), banned: false }));

document.getElementById('rank').addEventListener('submit', (event) => {
  event.preventDefault();
  const rank = document.getElementById('rank-value').value || null;
  run('ranks', { username: document.getElementById('rank-name').value, rank });
});

document.getElementById('logout').addEventListener('click', async () => {
  await fetch(LOGOUT_URL, { method: 'POST', headers: ADMIN_HEADERS });
  window.location.assign('/login');
});

const meResponse = await fetch(ME_URL);
if (meResponse.ok) {
  const me = await meResponse.json();
  document.getElementById('me').textContent = `${me.actor.username} · ${RANK_NAMES[me.actor.rank]} · ${me.mode} mode`;
  document.getElementById('logout').hidden = me.mode !== 'server';
  document.getElementById('ranks-block').hidden = me.actor.rank !== HEAD_ADMIN;
}
await Promise.all([loadAudit(), loadRanks(), loadOnline()]);
setInterval(() => loadOnline().catch((error) => say(String(error.message ?? error))), ONLINE_REFRESH_MS);
