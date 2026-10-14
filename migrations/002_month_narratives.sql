CREATE TABLE IF NOT EXISTS month_narratives (
  bill_month TEXT PRIMARY KEY,
  facts_hash TEXT NOT NULL,
  text TEXT NOT NULL,
  source TEXT NOT NULL,
  model TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
