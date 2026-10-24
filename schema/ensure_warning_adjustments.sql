-- Additive, idempotent extension for existing D1 databases.
-- Apply once (or safely repeat) before deploying Worker builds that use /warnings.
-- Original violations are preserved for audit and duplicate-punishment protection.
CREATE TABLE IF NOT EXISTS warning_adjustments (
  chat_id INTEGER NOT NULL CHECK (chat_id < 0),
  user_id INTEGER NOT NULL CHECK (user_id > 0),
  base_count INTEGER NOT NULL CHECK (base_count BETWEEN 0 AND 3),
  adjusted_at INTEGER NOT NULL,
  violation_rowid INTEGER NOT NULL DEFAULT 0 CHECK (violation_rowid >= 0),
  actor_id INTEGER NOT NULL CHECK (actor_id > 0),
  PRIMARY KEY (chat_id, user_id)
);
