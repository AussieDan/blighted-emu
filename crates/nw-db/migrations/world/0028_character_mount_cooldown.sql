-- SPDX-License-Identifier: AGPL-3.0-or-later
-- When a character's next mount summon may start (the MountComponent summonCooldownEndTime, Unix
-- ns; 0 for no cooldown). Before this migration a log out ended the dismount cooldown. Saved
-- with the mounts of migration 0012.

ALTER TABLE player_character
    ADD COLUMN mount_cooldown_end_ns BIGINT NOT NULL DEFAULT 0 CHECK (mount_cooldown_end_ns >= 0);
