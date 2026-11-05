-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The privacy settings an account set through the client's privacy settings service
-- (`CreateOrUpdatePrivacySettings`). A setting with no row has its default. The alias and value
-- are the service's texts, for example `who-can-send-me-a-friend-request` and `NoOne`.
CREATE TABLE account_privacy_setting (
    account_id UUID NOT NULL REFERENCES account (account_id) ON DELETE CASCADE,
    alias TEXT NOT NULL CHECK (alias <> ''),
    value TEXT NOT NULL CHECK (value <> ''),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, alias)
);

-- The social services read the settings changed since their last read.
CREATE INDEX account_privacy_setting_updated_idx ON account_privacy_setting (updated_at);

