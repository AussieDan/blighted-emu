// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { TILE_KINDS, UNKNOWN, formatTile, hostFacts, trailPush } from './tiles.js';

test('formatTile shows Unknown for null', () => {
  for (const kind of Object.values(TILE_KINDS)) {
    assert.equal(formatTile(kind, null), UNKNOWN);
    assert.equal(formatTile(kind, undefined), UNKNOWN);
  }
  assert.equal(formatTile(TILE_KINDS.MS, 1.236), '1.24 ms');
  assert.equal(formatTile(TILE_KINDS.CPU, 112.34), '112.3 %');
  assert.equal(formatTile(TILE_KINDS.RATE, 3.45), '3.5/s');
  assert.equal(formatTile(TILE_KINDS.COUNT, 0), '0');
});

test('bytes per second pick their unit', () => {
  assert.equal(formatTile(TILE_KINDS.BYTES, 512.4), '512 B/s');
  assert.equal(formatTile(TILE_KINDS.BYTES, 2048), '2.0 KiB/s');
  assert.equal(formatTile(TILE_KINDS.BYTES, 3 * 1024 * 1024), '3.0 MiB/s');
});

test('trailPush keeps the last points and drops leavers', () => {
  const at = (entity, x) => ({ entity, x, y: -x });
  let trails = new Map();
  for (const x of [1, 2, 3, 4]) {
    trails = trailPush(trails, [at(7, x), at(8, x * 10)], 3);
  }
  assert.deepEqual(trails.get(7), [{ x: 2, y: -2 }, { x: 3, y: -3 }, { x: 4, y: -4 }]);
  trails = trailPush(trails, [at(8, 50)], 3);
  assert.equal(trails.has(7), false, 'a player that left loses its trail');
  assert.deepEqual(trails.get(8).map((point) => point.x), [30, 40, 50]);
});

test('hostFacts shows memory and processor use, and Unknown without them', () => {
  const gib = 1024 ** 3;
  const facts = hostFacts({ memory: { total_bytes: 16 * gib, used_bytes: 4 * gib }, cpu: { cores: 8, percent: 37.4 } });
  assert.deepEqual(facts.pairs, [
    ['Memory used', '4.0 GiB of 16.0 GiB'],
    ['Memory use', '25 %'],
    ['Processor cores', '8'],
    ['Processor use', '37 %'],
  ]);
  assert.equal(facts.memoryShare, 25);
  assert.equal(facts.cpuShare, 37.4);
  const none = hostFacts({ memory: null, cpu: null });
  assert.ok(none.pairs.every(([, value]) => value === UNKNOWN));
  assert.equal(none.memoryShare, null);
});
