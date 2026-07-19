CREATE TABLE maintenance_windows (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  scope       TEXT NOT NULL DEFAULT 'all',       -- all | tag | monitors
  target_ref  TEXT,                              -- JSON string: NULL (all) | "\"prod\"" (tag) | "[1,2,3]" (monitors)
  starts_at   INTEGER NOT NULL,                  -- epoch; one-off start, or the >= lower bound + duration anchor for cron
  ends_at     INTEGER NOT NULL,                  -- epoch; one-off end; for cron, only ends_at - starts_at (the duration) matters
  recurrence  TEXT,                              -- NULL (one-off) | a 5-field cron expression (UTC)
  suppress    TEXT NOT NULL DEFAULT 'alerts',    -- alerts | checks
  is_active   INTEGER NOT NULL DEFAULT 1,        -- disable without deleting
  created_at  INTEGER NOT NULL
);
