-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The analytics clean-up of edge-http deletes the records older than the kept days
-- (`[analytics] retention_days` of config/edge.toml).
CREATE INDEX analytics_event_received_idx ON analytics_event (received_at);
