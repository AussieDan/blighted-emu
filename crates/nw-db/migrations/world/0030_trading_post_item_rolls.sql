-- SPDX-License-Identifier: AGPL-3.0-or-later
-- What a traded item keeps of its own: a sell order keeps the stack its creator posted, and each
-- item delivery the stack it gives, with their rolls and look as the carried items keep them
-- (migration 0017). The buyer gets the stack the seller posted, not a new one of the item's data
-- values.
--
-- A NULL column keeps the rule of rows stored before this migration: the item's data value
-- (full durability, fixed perks, BindOnPickup).

ALTER TABLE trading_post_contract
    -- Its current durability; NULL: full. Its gear score and its perks' CRCs are the search
    -- columns `gear_score` and `perks`.
    ADD COLUMN durability INTEGER CHECK (durability >= 0),
    -- Its perk ids (PerkData PerkID) by perk slot, no gaps; NULL: the item's fixed perks.
    ADD COLUMN perk_ids TEXT[],
    -- Bound to the character; NULL: the item's BindOnPickup.
    ADD COLUMN bound BOOLEAN,
    -- The four DyeColorData indexes of its dye channels (R, G, B, A), 0 for none.
    ADD COLUMN dye SMALLINT[] NOT NULL DEFAULT '{0,0,0,0}',
    -- The skin applied over it: the CRC-32 of the lowercase appearance id, 0 for none.
    ADD COLUMN appearance BIGINT NOT NULL DEFAULT 0
        CHECK (appearance >= 0 AND appearance <= 4294967295),
    -- The four dye indexes of the applied skin.
    ADD COLUMN skin_dye SMALLINT[] NOT NULL DEFAULT '{0,0,0,0}';

-- The columns of migration 0017, for the item a delivery gives.
ALTER TABLE trading_post_delivery
    ADD COLUMN gear_score INTEGER CHECK (gear_score >= 0),
    ADD COLUMN durability INTEGER CHECK (durability >= 0),
    ADD COLUMN perks TEXT[],
    ADD COLUMN bound BOOLEAN,
    ADD COLUMN dye SMALLINT[] NOT NULL DEFAULT '{0,0,0,0}',
    ADD COLUMN appearance BIGINT NOT NULL DEFAULT 0
        CHECK (appearance >= 0 AND appearance <= 4294967295),
    ADD COLUMN skin_dye SMALLINT[] NOT NULL DEFAULT '{0,0,0,0}';
