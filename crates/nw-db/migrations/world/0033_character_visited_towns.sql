-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The towns and outposts a character has been in: the territory ids (TerritoryDefinitions
-- TerritoryID) of the settlement and outpost rows it came into. Each one gives the character the
-- town's home point (kind 7 of the home point list), the point the world map counts as a known
-- fast travel point. Observed (retail captures, 2026-11-06): a town's home point is added after
-- the client says it came into that town's territory (2297), and a new character has none. The
-- rows carry no achievement, so the server keeps the list. It is part of the travel state of
-- migration 0010 and is saved with it. Only sim writes this column.

ALTER TABLE player_character
    ADD COLUMN visited_towns INTEGER[] NOT NULL DEFAULT '{}';
