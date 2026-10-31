-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The faction of a character (faction note 2026-10-30): the FactionComponentReplicatedState
-- faction (0 none, 1 Syndicate, 2 Marauders, 3 Covenant; retail RequestSetFaction 3161 sets it),
-- factionChangeCount and lastFactionChangeTimepoint (unix ns; 0 before any change). They belong
-- to the progression of migration 0006 and are saved with it. Only sim writes these columns.

ALTER TABLE player_character
    ADD COLUMN faction SMALLINT NOT NULL DEFAULT 0 CHECK (faction >= 0 AND faction <= 255),
    ADD COLUMN faction_changes INTEGER NOT NULL DEFAULT 0
        CHECK (faction_changes >= 0 AND faction_changes <= 65535),
    ADD COLUMN faction_changed_ns BIGINT NOT NULL DEFAULT 0 CHECK (faction_changed_ns >= 0);
