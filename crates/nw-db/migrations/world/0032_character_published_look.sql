-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The armor look the character select screen shows (retail PublishedData): per armor slot (head,
-- chest, hands, legs, feet) the CRC-32 of the gendered appearance id the character wears there,
-- 0 for none, as a signed 32-bit bit pattern. Saved by the sim with the items; NULL until the
-- character's first item save, when the screen shows the creation look.

ALTER TABLE player_character
    ADD COLUMN published_look INTEGER[] CHECK (cardinality(published_look) = 5);
