ALTER TABLE monitors ADD COLUMN ssl_check_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN ssl_alert_days TEXT NOT NULL DEFAULT '[30,14,7,3,1]';
ALTER TABLE monitors ADD COLUMN domain_check_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE monitors ADD COLUMN domain_alert_days TEXT NOT NULL DEFAULT '[45,30,14,7]';

CREATE TABLE ssl_certs (
  monitor_id     INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  issuer TEXT, subject TEXT, valid_from INTEGER, valid_until INTEGER,
  days_remaining INTEGER, is_valid INTEGER, chain_ok INTEGER,
  hostname_match INTEGER, self_signed INTEGER,
  error TEXT,
  alerted_days INTEGER,
  invalid_alerted INTEGER NOT NULL DEFAULT 0,
  last_checked INTEGER
);

CREATE TABLE domain_info (
  monitor_id  INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
  registrar TEXT, expiry_date INTEGER, days_remaining INTEGER,
  name_servers TEXT, status_codes TEXT,
  queryable INTEGER,
  source TEXT,
  alerted_days INTEGER,
  last_checked INTEGER
);
