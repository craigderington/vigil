CREATE TABLE monitors (
  id                     INTEGER PRIMARY KEY,
  name                   TEXT NOT NULL,
  type                   TEXT NOT NULL DEFAULT 'http',     -- P1: always 'http'
  url                    TEXT,                             -- nullable per blueprint, app enforces presence for type='http'
  method                 TEXT NOT NULL DEFAULT 'GET',
  headers                TEXT,                             -- JSON object or null
  body                   TEXT,
  auth_type              TEXT,                             -- none|basic|bearer|header
  auth_ref               TEXT,                             -- grammar in §6.2 (env:VAR | inline:<value>), never a keychain secret
  expected_status_codes  TEXT NOT NULL DEFAULT '200-299',
  interval_seconds       INTEGER NOT NULL DEFAULT 300,
  timeout_seconds        INTEGER NOT NULL DEFAULT 30,
  follow_redirects       INTEGER NOT NULL DEFAULT 1,
  verify_ssl             INTEGER NOT NULL DEFAULT 1,
  confirmation_threshold INTEGER NOT NULL DEFAULT 3,
  recovery_threshold     INTEGER NOT NULL DEFAULT 1,
  retry_interval_seconds INTEGER NOT NULL DEFAULT 30,
  status                 TEXT NOT NULL DEFAULT 'pending',  -- pending|up|down|paused|unknown
  is_paused              INTEGER NOT NULL DEFAULT 0,
  last_checked_at        INTEGER,
  next_run_at            INTEGER,
  consecutive_failures   INTEGER NOT NULL DEFAULT 0,
  consecutive_successes  INTEGER NOT NULL DEFAULT 0,
  tags                   TEXT,                             -- forward-compat: no tag filtering UI in P1
  sort_order             INTEGER NOT NULL DEFAULT 0,       -- forward-compat: reorder endpoint deferred, no P1 write path
  created_at             INTEGER NOT NULL,
  updated_at             INTEGER NOT NULL
);

CREATE TABLE checks (
  id               INTEGER PRIMARY KEY,
  monitor_id       INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  checked_at       INTEGER NOT NULL,
  status           TEXT NOT NULL,                          -- up|down  (raw probe outcome, not the uptime source)
  response_time_ms INTEGER,
  status_code      INTEGER,
  error_message    TEXT,
  resolved_ip      TEXT
);
CREATE INDEX idx_checks_monitor_time ON checks(monitor_id, checked_at DESC);

CREATE TABLE incidents (
  id               INTEGER PRIMARY KEY,
  monitor_id       INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  started_at       INTEGER NOT NULL,
  resolved_at      INTEGER,
  duration_seconds INTEGER,
  cause            TEXT,                                   -- timeout|status|connection|dns
  status_code      INTEGER,
  error_message    TEXT
);
CREATE INDEX idx_incidents_monitor ON incidents(monitor_id, started_at DESC);

CREATE TABLE notification_channels (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  type       TEXT NOT NULL,                                -- P1: 'email'
  config     TEXT NOT NULL,                                -- JSON: {host,port,security,from,to[]} — SOLE home of SMTP config, password NOT stored
  is_active  INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL
);

CREATE TABLE monitor_notifications (
  monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  channel_id INTEGER NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  triggers   TEXT NOT NULL DEFAULT '["down","recovered"]',
  PRIMARY KEY (monitor_id, channel_id)
);

CREATE TABLE notification_log (
  id          INTEGER PRIMARY KEY,
  monitor_id  INTEGER,
  channel_id  INTEGER,
  incident_id INTEGER,
  trigger     TEXT,
  sent_at     INTEGER,
  success     INTEGER,
  error       TEXT
);
CREATE INDEX idx_notif_log_monitor_trigger ON notification_log(monitor_id, trigger, sent_at DESC);

-- anchors, notify.cooldown_minutes, retention.raw_days, appearance.accent
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);

