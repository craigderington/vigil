ALTER TABLE monitors ADD COLUMN heartbeat_token TEXT;
ALTER TABLE monitors ADD COLUMN heartbeat_grace_seconds INTEGER NOT NULL DEFAULT 60;
ALTER TABLE monitors ADD COLUMN last_ping_at INTEGER;
CREATE UNIQUE INDEX idx_monitors_heartbeat_token ON monitors(heartbeat_token) WHERE heartbeat_token IS NOT NULL;
