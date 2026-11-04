// SPDX-License-Identifier: AGPL-3.0-or-later
// GM commands of the admin pages: checks what the forms hold and sends it.

const GM_PREFIX = '/api/gm/';
const ADMIN_HEADERS = Object.freeze({ 'X-Blighted-Admin': '1', 'Content-Type': 'application/json' });
/** Most GM mobs one spawn makes (the sim's limit). */
export const MAX_SPAWN = 50;
/** Lowest and highest creature level. */
export const LOWEST_LEVEL = 1;
export const HIGHEST_LEVEL = 65;

function numbers(texts) {
  const trimmed = texts.map((text) => String(text).trim());
  const values = trimmed.map(Number);
  const bad = trimmed.some((text) => text === '') || values.some((value) => !Number.isFinite(value));
  return bad ? null : values;
}

/** The suffix of every weapon mastery id (`GreatSwordAbilityTable`). */
const MASTERY_SUFFIX = 'AbilityTable';
/** The largest experience or rank amount the server takes (`u32`). */
const MAX_AMOUNT = 4294967295;

/** The whole number of `text` from `lowest` to `highest`, or null. */
function wholeNumber(text, lowest, highest) {
  const trimmed = String(text).trim();
  const value = Number(trimmed);
  return trimmed !== '' && Number.isSafeInteger(value) && value >= lowest && value <= highest ? value : null;
}

/** The name a page shows for a weapon mastery id: `GreatSword` for `GreatSwordAbilityTable`. */
export function masteryName(id) {
  return id.endsWith(MASTERY_SUFFIX) ? id.slice(0, -MASTERY_SUFFIX.length) : id;
}

/** The give-experience request for a player and an amount text, or an error text. */
export function experienceRequest(entity, amountText) {
  const amount = wholeNumber(amountText, 1, MAX_AMOUNT);
  return amount === null ? { error: 'The experience must be a whole number of 1 or more.' } : { request: { entity, amount } };
}

/** The set-level request for a player and a level text (1 to `maxLevel`), or an error text. */
export function levelRequest(entity, levelText, maxLevel) {
  const level = wholeNumber(levelText, 1, maxLevel);
  return level === null ? { error: `The level must be a whole number from 1 to ${maxLevel}.` } : { request: { entity, level } };
}

/** The give-mastery request for a player, a mastery id and an amount text, or an error text. */
export function masteryRequest(entity, mastery, amountText) {
  if (mastery === '') {
    return { error: 'Pick one weapon to give mastery experience to.' };
  }
  const amount = wholeNumber(amountText, 1, MAX_AMOUNT);
  return amount === null
    ? { error: 'The mastery experience must be a whole number of 1 or more.' }
    : { request: { entity, mastery, amount } };
}

/**
 * The set-mastery-rank request for a player, a mastery id ('' for every weapon) and a rank text
 * (1 to `maxRank`), or an error text.
 */
export function masteryRankRequest(entity, mastery, rankText, maxRank) {
  const rank = wholeNumber(rankText, 1, maxRank);
  return rank === null
    ? { error: `The rank must be a whole number from 1 to ${maxRank}.` }
    : { request: { entity, mastery: mastery === '' ? null : mastery, rank } };
}

/** The set-coins request for a player and a coins text, or an error text. */
export function coinsRequest(entity, coinsText) {
  const coins = wholeNumber(coinsText, 0, Number.MAX_SAFE_INTEGER);
  return coins === null ? { error: 'The coins must be a whole number of 0 or more.' } : { request: { entity, coins } };
}

/** The teleport request for a player and the x, y and z field texts, or an error text. */
export function teleportRequest(entity, xText, yText, zText) {
  const values = numbers([xText, yText, zText]);
  if (!values) {
    return { error: 'x, y and z must be numbers (metres).' };
  }
  const [x, y, z] = values;
  return { request: { entity, x, y, z } };
}

/** The give-item request for a player, an item id and a quantity text, or an error text. */
export function itemRequest(entity, itemText, quantityText) {
  const item = String(itemText).trim();
  const quantity = Number(String(quantityText).trim() || '1');
  if (item === '') {
    return { error: 'Name an item id.' };
  }
  if (!Number.isInteger(quantity) || quantity < 1) {
    return { error: 'The quantity must be a whole number of 1 or more.' };
  }
  return { request: { entity, item, quantity } };
}

/** The quest start request for a player and the objective field text, or an error text. */
export function questRequest(entity, objectiveText) {
  const objective = String(objectiveText).trim();
  return objective === '' ? { error: 'Name an objective id.' } : { request: { entity, objective } };
}

/** The event start request for an event id and the point field texts, or an error text. */
export function eventStartRequest(eventText, xText, yText, zText) {
  const event = String(eventText).trim();
  const values = numbers([xText, yText, zText]);
  if (event === '') {
    return { error: 'Pick an event.' };
  }
  if (!values) {
    return { error: 'Right-click the map, or type x, y and z, for the event point.' };
  }
  const [x, y, z] = values;
  return { request: { event, x, y, z } };
}

/** The stop request of the picked event, or an error text. */
export function eventStopRequest(eventText) {
  const event = String(eventText).trim();
  return event === '' ? { error: 'Pick the event to stop.' } : { request: { event } };
}

/** The spawn request of the spawn form's field texts, or an error text. */
export function spawnRequest({ vitals, level, count, x, y, z }) {
  const id = String(vitals).trim();
  const values = numbers([level, count, x, y, z]);
  if (id === '' || !values) {
    return { error: 'Name a vitals id; level, count, x, y and z must be numbers.' };
  }
  const [levelValue, countValue, xValue, yValue, zValue] = values;
  if (!Number.isInteger(levelValue) || levelValue < LOWEST_LEVEL || levelValue > HIGHEST_LEVEL) {
    return { error: `The level must be ${LOWEST_LEVEL} to ${HIGHEST_LEVEL}.` };
  }
  if (!Number.isInteger(countValue) || countValue < 1 || countValue > MAX_SPAWN) {
    return { error: `The count must be 1 to ${MAX_SPAWN}.` };
  }
  return { request: { vitals: id, level: levelValue, count: countValue, x: xValue, y: yValue, z: zValue } };
}

/** Sends a GM command to `/api/gm/<path>`; gives the answer text and whether it worked. */
export async function sendGm(path, body) {
  const response = await fetch(`${GM_PREFIX}${path}`, {
    method: 'POST',
    headers: ADMIN_HEADERS,
    body: JSON.stringify(body),
  });
  return { ok: response.ok, text: await response.text() };
}

/** Sends a GM command and tells `say` what happens: sending, done (with the answer) or the failure. */
export async function sendGmAndSay(path, body, say) {
  say('Sending…');
  try {
    const answer = await sendGm(path, body);
    say(answer.ok ? `Done: ${answer.text}` : answer.text);
  } catch (failure) {
    say(`Not sent: ${failure.message ?? failure}`);
  }
}
