-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The recipe cooldown counts of a character (nw_content::recipe_cooldowns): for each recipe
-- with CooldownSeconds and CooldownQuantity, the crafts made and when the count goes back to 0
-- (unix ns; retail keeps them in the crafting component's m_cooldowns). They belong to the
-- progression of migration 0006 and are saved with it. Only sim writes this table.

CREATE TABLE character_recipe_cooldown (
    character_id UUID NOT NULL REFERENCES player_character (character_id) ON DELETE CASCADE,
    -- The lowercase RecipeID.
    recipe_id TEXT NOT NULL CHECK (recipe_id = lower(recipe_id) AND recipe_id <> ''),
    count BIGINT NOT NULL CHECK (count >= 0 AND count <= 4294967295),
    ends_ns BIGINT NOT NULL CHECK (ends_ns >= 0),
    PRIMARY KEY (character_id, recipe_id)
);
