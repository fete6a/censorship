-- ONE-TIME additive migration. Inspect columns before running; ALTER TABLE
-- ADD COLUMN IF NOT EXISTS is not supported across all SQLite/D1 versions.
-- Never run on an existing table which already contains these columns.
-- Deploy only after the production D1 schema has been checked, before the
-- corresponding fenced Worker artifact; no existing data is removed.
ALTER TABLE scan_jobs ADD COLUMN generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0);
ALTER TABLE scan_jobs ADD COLUMN lease_id INTEGER NOT NULL DEFAULT 0 CHECK (lease_id >= 0);
ALTER TABLE scan_jobs ADD COLUMN lease_until INTEGER NOT NULL DEFAULT 0;
