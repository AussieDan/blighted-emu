-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The role ids of each Discord login in the Discord server that logins need
-- (`[discord] guild_id` of config/edge.toml), written again at every login. A launcher session
-- is checked against `[discord] allowed_role_ids` with them at each Play, when Discord cannot be
-- asked. Empty without a server check; rows from before this migration get them at the next
-- login.
ALTER TABLE discord_login ADD COLUMN guild_role_ids TEXT[] NOT NULL DEFAULT '{}';
