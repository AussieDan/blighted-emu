-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Coin the social service owes a character: a company deposit that left the player and that the
-- treasury did not take, or a withdrawal that the treasury could not take back, while the player
-- was offline or its sim did not take the coin. The social service gives the coin to the player
-- when it is next online and deletes the row once its sim took it. Only the social service writes
-- these rows.
CREATE TABLE company_coin_return (
    return_id UUID PRIMARY KEY,
    character_id UUID NOT NULL REFERENCES player_character (character_id) ON DELETE CASCADE,
    -- The company the coin came from or went to; NULL once that company is deleted.
    company_id UUID REFERENCES company (company_id) ON DELETE SET NULL,
    -- Hundredths of a coin.
    amount BIGINT NOT NULL CHECK (amount > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX company_coin_return_character_idx ON company_coin_return (character_id);
