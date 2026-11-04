// SPDX-License-Identifier: AGPL-3.0-or-later
// The GM controls of one online player, shared by the live page and the GM page: teleport, heal,
// god mode, kill, kick, items, quests, level, weapon mastery and coins.

import {
  coinsRequest, experienceRequest, itemRequest, levelRequest, masteryName, masteryRankRequest, masteryRequest,
  questRequest, sendGmAndSay, teleportRequest,
} from './gm.js';

const PROGRESSION_URL = '/api/gm/progression';
const POSITION_DECIMALS = 1;
const PLAYER_COMMANDS = Object.freeze({
  heal: (entity) => ['heal', { entity }],
  kill: (entity) => ['kill', { entity }],
  kick: (entity) => ['kick', { entity }],
  'god-on': (entity) => ['god', { entity, on: true }],
  'god-off': (entity) => ['god', { entity, on: false }],
});
const MARKUP = `
  <form class="gm" data-form="teleport">
    <strong>Teleport</strong>
    <input type="text" name="x" inputmode="decimal" aria-label="x (metres)" placeholder="x">
    <input type="text" name="y" inputmode="decimal" aria-label="y (metres)" placeholder="y">
    <input type="text" name="z" inputmode="decimal" aria-label="z (metres)" placeholder="z">
    <button type="submit">Move</button>
  </form>
  <form class="gm" data-form="to-player">
    <strong>To player</strong>
    <input type="text" name="to" inputmode="numeric" aria-label="entity of the player to go to" placeholder="entity">
    <button type="submit">Move</button>
  </form>
  <div class="gm">
    <button type="button" data-command="heal">Heal</button>
    <button type="button" data-command="god-on">God on</button>
    <button type="button" data-command="god-off">God off</button>
    <button type="button" class="danger" data-command="kill">Kill</button>
    <button type="button" class="danger" data-command="kick">Kick</button>
  </div>
  <form class="gm" data-form="item">
    <strong>Item</strong>
    <input type="text" name="item" aria-label="item id" placeholder="item id" class="wide">
    <input type="text" name="quantity" inputmode="numeric" aria-label="quantity" placeholder="1">
    <button type="submit">Give</button>
    <button type="button" data-action="take">Take</button>
  </form>
  <form class="gm" data-form="quest">
    <input type="text" name="objective" aria-label="objective id" placeholder="objective id" class="wide">
    <button type="submit">Start quest</button>
  </form>
  <form class="gm" data-form="level">
    <strong>Level</strong>
    <input type="text" name="level" inputmode="numeric" aria-label="level" placeholder="level">
    <button type="submit">Set level</button>
    <input type="text" name="xp" inputmode="numeric" aria-label="experience points" placeholder="xp">
    <button type="button" data-action="give-xp">Give XP</button>
  </form>
  <form class="gm" data-form="mastery">
    <strong>Weapon mastery</strong>
    <select name="mastery" aria-label="weapon mastery"><option value="">All weapons</option></select>
    <input type="text" name="rank" inputmode="numeric" aria-label="mastery rank" placeholder="rank">
    <button type="submit">Set rank</button>
    <input type="text" name="xp" inputmode="numeric" aria-label="mastery points" placeholder="xp">
    <button type="button" data-action="give-mastery">Give XP</button>
  </form>
  <form class="gm" data-form="coins">
    <strong>Coins</strong>
    <input type="text" name="coins" inputmode="numeric" aria-label="coins" placeholder="coins">
    <button type="submit">Set coins</button>
  </form>`;

/**
 * Builds the player controls into `box`. `entity()` gives the player they act on (an entity of
 * entity.js, or undefined); `say` shows a line to the viewer; `sent` runs after each command.
 * Returns `fill(entity)`, which puts the player's position in the teleport fields.
 */
export function mountPlayerControls(box, { entity, say, sent = () => {} }) {
  box.innerHTML = MARKUP;
  const form = (name) => box.querySelector(`[data-form="${name}"]`);
  const field = (formName, name) => form(formName).elements.namedItem(name);
  const tops = { maxLevel: 0, maxRank: 0 };

  async function send(path, body) {
    await sendGmAndSay(path, body, say);
    sent();
  }

  /** Runs the request that `build` makes from the player's entity number, or shows its error. */
  function sendBuilt(path, build) {
    const player = entity();
    if (!player) {
      say('Pick a player first.');
      return;
    }
    const { request, error } = build(player.entity);
    if (error) {
      say(error);
    } else {
      send(path, request);
    }
  }

  function onSubmit(name, handler) {
    form(name).addEventListener('submit', (event) => {
      event.preventDefault();
      handler();
    });
  }

  onSubmit('teleport', () => sendBuilt('teleport', (player) =>
    teleportRequest(player, ...['x', 'y', 'z'].map((name) => field('teleport', name).value))));
  onSubmit('to-player', () => {
    const toEntity = Number(field('to-player', 'to').value.trim());
    sendBuilt('teleport-to-player', (player) => (Number.isInteger(toEntity) && toEntity > 0
      ? { request: { entity: player, to_entity: toEntity } }
      : { error: 'Give the entity number of the player to go to.' }));
  });
  for (const button of box.querySelectorAll('[data-command]')) {
    button.addEventListener('click', () => {
      const player = entity();
      if (player) {
        send(...PLAYER_COMMANDS[button.dataset.command](player.entity));
      } else {
        say('Pick a player first.');
      }
    });
  }
  const itemFields = () => [field('item', 'item').value, field('item', 'quantity').value];
  onSubmit('item', () => sendBuilt('give-item', (player) => itemRequest(player, ...itemFields())));
  box.querySelector('[data-action="take"]').addEventListener('click', () => sendBuilt('take-item', (player) => {
    const { request, error } = itemRequest(player, ...itemFields());
    return error ? { error } : { request: { entity: request.entity, item: request.item } };
  }));
  onSubmit('quest', () => sendBuilt('start-quest', (player) => questRequest(player, field('quest', 'objective').value)));
  onSubmit('level', () => sendBuilt('set-level', (player) => levelRequest(player, field('level', 'level').value, tops.maxLevel)));
  box.querySelector('[data-action="give-xp"]').addEventListener('click', () =>
    sendBuilt('give-experience', (player) => experienceRequest(player, field('level', 'xp').value)));
  const mastery = field('mastery', 'mastery');
  onSubmit('mastery', () => sendBuilt('set-mastery-rank', (player) =>
    masteryRankRequest(player, mastery.value, field('mastery', 'rank').value, tops.maxRank)));
  box.querySelector('[data-action="give-mastery"]').addEventListener('click', () =>
    sendBuilt('give-mastery', (player) => masteryRequest(player, mastery.value, field('mastery', 'xp').value)));
  onSubmit('coins', () => sendBuilt('set-coins', (player) => coinsRequest(player, field('coins', 'coins').value)));

  fetch(PROGRESSION_URL)
    .then(async (response) => {
      if (!response.ok) {
        throw new Error(`Progression: ${response.status} ${await response.text()}`);
      }
      const progression = await response.json();
      tops.maxLevel = progression.max_level;
      tops.maxRank = progression.max_rank;
      mastery.append(...progression.masteries.map((id) => {
        const option = document.createElement('option');
        option.value = id;
        option.textContent = masteryName(id);
        return option;
      }));
    })
    .catch((error) => say(String(error.message ?? error)));

  return {
    fill(player) {
      ['x', 'y', 'z'].forEach((name) => {
        field('teleport', name).value = player[name].toFixed(POSITION_DECIMALS);
      });
    },
  };
}
