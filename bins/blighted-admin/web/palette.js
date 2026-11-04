// SPDX-License-Identifier: AGPL-3.0-or-later
// The data colours of the map: strong signal colours, each easy to tell from the others, shared
// by every page. style.css holds the values (--signal-*); they are the same in both themes.

const style = getComputedStyle(document.documentElement);
const signal = (name) => style.getPropertyValue(`--signal-${name}`).trim();

export const SIGNAL = Object.freeze({
  NAVY: signal('navy'),
  RED: signal('red'),
  YELLOW: signal('yellow'),
  WHITE: signal('white'),
  INK: signal('ink'),
  GREEN: signal('green'),
  BLUE: signal('blue'),
  ORANGE: signal('orange'),
  PURPLE: signal('purple'),
  ICE: signal('ice'),
  STONE: signal('stone'),
});

/**
 * Colours of things in a row (sims, tracks): each next to its neighbours is easy to tell. Navy
 * comes first: no live map point uses it, so the ground of the first sim never hides a point.
 */
export const SERIES = Object.freeze([
  SIGNAL.NAVY,
  SIGNAL.ICE,
  SIGNAL.ORANGE,
  SIGNAL.BLUE,
  SIGNAL.STONE,
  SIGNAL.PURPLE,
  SIGNAL.RED,
  SIGNAL.GREEN,
  SIGNAL.YELLOW,
]);

/** The ring around every point, so a point shows on light and dark ground. */
export const POINT_RING = SIGNAL.INK;
