-- P2 (Signal): new monitor-type columns, incident acknowledge flag,
-- and the daily rollup table backing 30d/90d stats + the 90-day uptime bar.
ALTER TABLE monitors ADD COLUMN host TEXT;
ALTER TABLE monitors ADD COLUMN port INTEGER;
ALTER TABLE monitors ADD COLUMN keyword TEXT;
ALTER TABLE monitors ADD COLUMN keyword_mode TEXT;
ALTER TABLE monitors ADD COLUMN keyword_case_sensitive INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN dns_record_type TEXT;
ALTER TABLE monitors ADD COLUMN dns_expected_value TEXT;

ALTER TABLE incidents ADD COLUMN acknowledged INTEGER NOT NULL DEFAULT 0;

CREATE TABLE check_aggregates_daily (
  monitor_id      INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
  day             TEXT NOT NULL,                 -- YYYY-MM-DD (UTC)
  up_count        INTEGER NOT NULL DEFAULT 0,
  down_count      INTEGER NOT NULL DEFAULT 0,
  degraded_count  INTEGER NOT NULL DEFAULT 0,    -- P2: always 0
  avg_response_ms REAL,
  min_response_ms INTEGER,
  max_response_ms INTEGER,
  uptime_pct      REAL,                          -- stored for P4 durable reports (completed days)
  incident_count  INTEGER NOT NULL DEFAULT 0,
  sample_count    INTEGER NOT NULL DEFAULT 0,    -- up_count+down_count, for count-weighted 30d/90d avg
  PRIMARY KEY (monitor_id, day)
);
CREATE INDEX idx_aggregates_day ON check_aggregates_daily(monitor_id, day);
