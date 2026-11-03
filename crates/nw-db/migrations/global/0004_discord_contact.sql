-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Discord contact details of each Discord login, written again at every login, so server staff
-- can help a player get an account back. Rows from before this migration get them at the next
-- login.

ALTER TABLE discord_login
    ADD COLUMN discord_username TEXT,
    ADD COLUMN email TEXT,
    ADD COLUMN email_verified BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN last_login_at TIMESTAMPTZ;

-- Staff look accounts up by email.
CREATE INDEX discord_login_email_idx ON discord_login (lower(email));
