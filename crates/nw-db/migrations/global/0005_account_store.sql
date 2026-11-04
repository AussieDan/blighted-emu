-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The store of each account: the store entitlements it holds (a store purchase adds one) and its
-- wallet of Marks of Fortune, the store currency. The entitlement is the game data's
-- UniqueTagID; instance_id is the id the store service gives the client for it.

CREATE TABLE account_entitlement (
    account_id UUID NOT NULL REFERENCES account (account_id) ON DELETE CASCADE,
    entitlement TEXT NOT NULL CHECK (entitlement <> ''),
    instance_id UUID NOT NULL,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, entitlement)
);

-- An account with no row has 0 Marks of Fortune.
CREATE TABLE account_wallet (
    account_id UUID PRIMARY KEY REFERENCES account (account_id) ON DELETE CASCADE,
    marks_of_fortune BIGINT NOT NULL DEFAULT 0 CHECK (marks_of_fortune >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
