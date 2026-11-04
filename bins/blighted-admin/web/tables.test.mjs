// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { directionText, largest, formatBytes, formatRate, ratioText, sortRows } from './tables.js';

test('sortRows sorts numbers and puts missing last', () => {
  const rows = [{ n: 10 }, { n: null }, { n: 2 }, { n: 33 }];
  assert.deepEqual(sortRows(rows, 'n', false).map((row) => row.n), [2, 10, 33, null]);
  assert.deepEqual(sortRows(rows, 'n', true).map((row) => row.n), [33, 10, 2, null]);
  assert.deepEqual(rows.map((row) => row.n), [10, null, 2, 33], 'the input is not changed');
});

test('sortRows sorts text without case', () => {
  const rows = [{ name: 'beta' }, { name: 'Alpha' }, { name: undefined }];
  assert.deepEqual(sortRows(rows, 'name', false).map((row) => row.name), ['Alpha', 'beta', undefined]);
});

test('formatRate rounds below 10 to one decimal', () => {
  assert.equal(formatRate(3.456), '3.5');
  assert.equal(formatRate(123.4), '123');
  assert.equal(formatRate(null), '—');
});

test('ratioText is a dash for a missing side', () => {
  assert.equal(ratioText(null), '—');
  assert.equal(ratioText(0.5), '×0.50');
  assert.equal(ratioText(12.345), '×12.3');
});

test('formatBytes picks its unit', () => {
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(2048), '2.0 KiB');
  assert.equal(formatBytes(16 * 1024 ** 3), '16.0 GiB');
  assert.equal(formatBytes(3 * 1024 * 1024), '3.0 MiB');
});

test('directionText names the two directions', () => {
  assert.equal(directionText(0), 'server → client');
  assert.equal(directionText(1), 'client → server');
});

test('largest takes a very long list and has a floor', () => {
  const values = Array.from({ length: 400000 }, (_, index) => index % 1000);
  assert.equal(largest(values, 1), 999);
  assert.equal(largest([], 1), 1);
});
