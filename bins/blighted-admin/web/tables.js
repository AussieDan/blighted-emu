// SPDX-License-Identifier: AGPL-3.0-or-later
// Table helpers of the admin pages.

const MISSING = '—';
const ONE_DECIMAL_BELOW = 10;
const KIB = 1024;
const MIB = KIB * KIB;
const GIB = MIB * KIB;
const DIRECTIONS = ['server → client', 'client → server'];

function missing(value) {
  return value === null || value === undefined;
}

/** A sorted copy of `rows` by `column`: numbers as numbers, text without case, missing last. */
export function sortRows(rows, column, descending) {
  const sign = descending ? -1 : 1;
  return [...rows].sort((left, right) => {
    const a = left[column];
    const b = right[column];
    if (missing(a) || missing(b)) {
      return Number(missing(a)) - Number(missing(b));
    }
    if (typeof a === 'number' && typeof b === 'number') {
      return sign * (a - b);
    }
    return sign * String(a).localeCompare(String(b), undefined, { sensitivity: 'base' });
  });
}

/** A rate per second: one decimal below 10, whole numbers above. */
export function formatRate(perSecond) {
  if (missing(perSecond)) {
    return MISSING;
  }
  return perSecond < ONE_DECIMAL_BELOW ? perSecond.toFixed(1) : Math.round(perSecond).toString();
}

/** A compare ratio (compare / main), or a dash when one side is missing. */
export function ratioText(ratio) {
  if (missing(ratio)) {
    return MISSING;
  }
  return ratio < ONE_DECIMAL_BELOW ? `×${ratio.toFixed(2)}` : `×${ratio.toFixed(1)}`;
}

/** A byte count with its unit. */
export function formatBytes(bytes) {
  if (bytes >= GIB) {
    return `${(bytes / GIB).toFixed(1)} GiB`;
  }
  if (bytes >= MIB) {
    return `${(bytes / MIB).toFixed(1)} MiB`;
  }
  if (bytes >= KIB) {
    return `${(bytes / KIB).toFixed(1)} KiB`;
  }
  return `${bytes} B`;
}

/** A ledger direction (0 server to client, 1 client to server). */
export function directionText(direction) {
  return DIRECTIONS[direction] ?? MISSING;
}

/** The largest value, at least `floor` (a loop: a spread of a long list overflows the stack). */
export function largest(values, floor) {
  let top = floor;
  for (const value of values) {
    top = Math.max(top, value);
  }
  return top;
}
