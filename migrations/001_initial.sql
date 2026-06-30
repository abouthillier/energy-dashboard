CREATE TABLE IF NOT EXISTS source_files (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  path TEXT NOT NULL,
  filename TEXT NOT NULL,
  sha256 TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  provider TEXT,
  status TEXT NOT NULL,
  extracted_text TEXT,
  error TEXT,
  imported_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS import_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at TEXT NOT NULL DEFAULT (datetime('now')),
  finished_at TEXT,
  source TEXT NOT NULL,
  status TEXT NOT NULL,
  files_seen INTEGER NOT NULL DEFAULT 0,
  files_imported INTEGER NOT NULL DEFAULT 0,
  files_skipped INTEGER NOT NULL DEFAULT 0,
  errors INTEGER NOT NULL DEFAULT 0,
  message TEXT
);

CREATE TABLE IF NOT EXISTS utility_bills (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  bill_month TEXT NOT NULL,
  provider TEXT NOT NULL,
  account_label TEXT,
  billing_period_start TEXT,
  billing_period_end TEXT,
  bill_date TEXT,
  due_date TEXT,
  service_days INTEGER,
  electric_kwh REAL,
  gas_therms REAL,
  total_cost REAL NOT NULL,
  electric_cost REAL,
  gas_cost REAL,
  supply_cost REAL,
  delivery_cost REAL,
  public_benefits_cost REAL,
  credits_cost REAL,
  source_file_id INTEGER,
  parser_confidence REAL NOT NULL DEFAULT 0.0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  FOREIGN KEY (source_file_id) REFERENCES source_files(id),
  UNIQUE(provider, billing_period_start, billing_period_end)
);

CREATE TABLE IF NOT EXISTS weather_monthly (
  month TEXT PRIMARY KEY,
  avg_temp_f REAL,
  heating_degree_days REAL,
  cooling_degree_days REAL,
  source TEXT NOT NULL,
  latitude REAL,
  longitude REAL,
  fetched_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_utility_bills_month ON utility_bills(bill_month);
CREATE INDEX IF NOT EXISTS idx_utility_bills_provider ON utility_bills(provider);
CREATE INDEX IF NOT EXISTS idx_source_files_status ON source_files(status);
