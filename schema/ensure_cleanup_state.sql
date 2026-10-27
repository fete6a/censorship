-- Additive, idempotent singleton metadata for fenced daily cleanup.
-- Apply to existing D1 only after the read-only --pre-migration gate.
-- No data rows are changed here; the Worker atomically seeds the singleton.
CREATE TABLE IF NOT EXISTS cleanup_state (singleton INTEGER PRIMARY KEY CHECK (singleton=1), last_success_day INTEGER NOT NULL DEFAULT -1 CHECK (last_success_day>=-1), lease_id INTEGER NOT NULL DEFAULT 0 CHECK (lease_id>=0), lease_until INTEGER NOT NULL DEFAULT 0);
