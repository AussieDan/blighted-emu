-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The status effects of the used consumables a character holds (retail target-lock-test: a
-- FoodRecoveryT2 held with 303.5 s left 1.5 s after the login), with the repeating vitals
-- change the consumable gives with each. Times are unix ns. An effect pauses while the player is
-- away unless its rule has DisablePauseOnPersist (runs_offline): a load moves the end of a
-- pausing effect by the time since saved_ns. They belong to the progression of migration 0006
-- and are saved with it. Only sim writes this table.

CREATE TABLE character_status_effect (
    character_id UUID NOT NULL REFERENCES player_character (character_id) ON DELETE CASCADE,
    -- The StatusID of the effect.
    status_id TEXT NOT NULL CHECK (status_id <> ''),
    -- The ConsumableID of the consumable that added it.
    consumable_id TEXT NOT NULL CHECK (consumable_id <> ''),
    ends_ns BIGINT NOT NULL CHECK (ends_ns >= 0),
    stacks SMALLINT NOT NULL CHECK (stacks >= 1 AND stacks <= 255),
    runs_offline BOOLEAN NOT NULL,
    -- The next repeat of the consumable's vitals change and the end of the repeats, or both NULL.
    repeat_next_ns BIGINT CHECK (repeat_next_ns >= 0),
    repeat_ends_ns BIGINT CHECK (repeat_ends_ns >= 0),
    -- When the row was written.
    saved_ns BIGINT NOT NULL CHECK (saved_ns >= 0),
    PRIMARY KEY (character_id, status_id),
    CHECK ((repeat_next_ns IS NULL) = (repeat_ends_ns IS NULL))
);
