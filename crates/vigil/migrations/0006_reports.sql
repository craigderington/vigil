CREATE TABLE reports (
  id            INTEGER PRIMARY KEY,
  period_start  INTEGER NOT NULL,               -- first day of month, 00:00:00 UTC (epoch secs)
  period_end    INTEGER NOT NULL,               -- first day of next month, 00:00:00 UTC (exclusive)
  label         TEXT NOT NULL,                  -- "March 2026"
  generated_at  INTEGER NOT NULL,
  summary_json  TEXT NOT NULL,                  -- cached ReportSummary
  html_path     TEXT,
  pdf_path      TEXT,
  emailed_at    INTEGER,
  UNIQUE(period_start)
);
