-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The rest of a placed camp (migration 0020 kept its place, tier and time): its rotation, as the
-- four f32 bit patterns of its quaternion (x, y, z, w; the 2236 placement's), and its blueprint,
-- the CRC-32 of the lowercase id (`CampT5`, a camp skin's blueprint). Without them a camp came
-- back unturned and of the owner's camp skin. Both NULL for a camp saved before this migration
-- and for no camp. Saved with the progression like migration 0010.

ALTER TABLE player_character
    ADD COLUMN camp_rotation_bits INTEGER[]
        CHECK (camp_rotation_bits IS NULL OR (cardinality(camp_rotation_bits) = 4
            AND array_position(camp_rotation_bits, NULL) IS NULL)),
    ADD COLUMN camp_blueprint BIGINT
        CHECK (camp_blueprint IS NULL OR (camp_blueprint >= 0 AND camp_blueprint <= 4294967295));
