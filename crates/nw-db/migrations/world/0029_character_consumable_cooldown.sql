-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The running consumable cooldowns of a character (the ccdmap of the item component: potions,
-- food and the like; consumables of one CooldownId share one). Each row is one cooldown: the
-- consumable that started it and its group (the CRC-32 of the lowercase ConsumableID and
-- CooldownId) and its end in microseconds since 2000-01-01, the server clock the client gets.
-- Inferred: a log out does not end a cooldown (no retail login sample has a running one; without
-- this a log out let a potion be drunk again at once). They belong to the progression of
-- migration 0006 and are saved with it. Only sim writes this table.

CREATE TABLE character_consumable_cooldown (
    character_id UUID NOT NULL REFERENCES player_character (character_id) ON DELETE CASCADE,
    cooldown_group BIGINT NOT NULL CHECK (cooldown_group >= 0 AND cooldown_group <= 4294967295),
    consumable BIGINT NOT NULL CHECK (consumable >= 0 AND consumable <= 4294967295),
    end_us BIGINT NOT NULL CHECK (end_us >= 0),
    PRIMARY KEY (character_id, cooldown_group)
);
