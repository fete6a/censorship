-- Canonical D1 application schema, including group-scoped warning adjustments.
-- Use ONLY on a newly created, empty D1 database.
-- Do not run against an existing database. Apply schema/ensure_warning_adjustments.sql to existing D1 databases instead.
-- The production D1 migration ledger is intentionally retained only in production.

CREATE TABLE archived_messages (
  chat_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  user_id INTEGER,
  sent_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  payload TEXT NOT NULL,
  last_status TEXT NOT NULL DEFAULT 'pending',
  last_category TEXT NOT NULL DEFAULT 'none',
  last_probability REAL NOT NULL DEFAULT 0,
  last_reviewed_at INTEGER,
  PRIMARY KEY (chat_id, message_id)
);

CREATE TABLE group_settings (chat_id INTEGER PRIMARY KEY CHECK (chat_id < 0), dry_run INTEGER CHECK (dry_run IN (0,1)), logs_enabled INTEGER NOT NULL DEFAULT 1 CHECK (logs_enabled IN (0,1)), remove_threshold REAL CHECK(remove_threshold IS NULL OR remove_threshold BETWEEN 0 AND 1));

CREATE TABLE managed_chats (
  chat_id INTEGER PRIMARY KEY,
  title TEXT NOT NULL DEFAULT '',
  chat_type TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','delete_only','paused','disabled')),
  updated_at INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);

CREATE TABLE pending_bans (
  chat_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  due_at INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'processing', 'banned', 'canceled', 'failed')),
  canceled_by INTEGER,
  cancellation_reason TEXT,
  resolved_at INTEGER, next_attempt_at INTEGER NOT NULL DEFAULT 0, attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  PRIMARY KEY (chat_id, user_id)
);

CREATE TABLE processed_updates (
  update_id INTEGER PRIMARY KEY,
  processed_at INTEGER NOT NULL
);

CREATE TABLE scan_jobs (
  chat_id INTEGER PRIMARY KEY,
  cursor_message_id INTEGER NOT NULL DEFAULT 0,
  max_message_id INTEGER NOT NULL,
  scanned INTEGER NOT NULL DEFAULT 0,
  flagged INTEGER NOT NULL DEFAULT 0,
  enforce INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'running',
  started_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

-- Additive, idempotent extension for existing D1 databases.
-- Apply once (or safely repeat) before deploying Worker builds that use /warnings.
-- Original violations are preserved for audit and duplicate-punishment protection.
CREATE TABLE warning_adjustments (
  chat_id INTEGER NOT NULL CHECK (chat_id < 0),
  user_id INTEGER NOT NULL CHECK (user_id > 0),
  base_count INTEGER NOT NULL CHECK (base_count BETWEEN 0 AND 3),
  adjusted_at INTEGER NOT NULL,
  violation_rowid INTEGER NOT NULL DEFAULT 0 CHECK (violation_rowid >= 0),
  actor_id INTEGER NOT NULL CHECK (actor_id > 0),
  PRIMARY KEY (chat_id, user_id)
);

CREATE TABLE violations (
  chat_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  category TEXT NOT NULL,
  probability REAL NOT NULL,
  severity REAL NOT NULL DEFAULT 0,
  action TEXT NOT NULL,
  PRIMARY KEY (chat_id, message_id)
);

CREATE INDEX idx_archive_by_time
  ON archived_messages(chat_id, sent_at);

CREATE INDEX idx_managed_chats_status ON managed_chats(status);

CREATE INDEX idx_pending_bans_due ON pending_bans(status, due_at);

CREATE INDEX idx_pending_bans_ready
  ON pending_bans(status, next_attempt_at, due_at, chat_id, user_id);

CREATE INDEX idx_violations_recent
  ON violations(chat_id, user_id, created_at);


-- Durable per-Telegram-update receipt: retries of the same /warnings command
-- must not move the manual baseline past genuine strikes received afterwards.
CREATE TABLE IF NOT EXISTS warning_edit_receipts (
  update_id INTEGER PRIMARY KEY CHECK (update_id >= 0),
  chat_id INTEGER NOT NULL CHECK (chat_id < 0),
  user_id INTEGER NOT NULL CHECK (user_id > 0),
  base_count INTEGER NOT NULL CHECK (base_count BETWEEN 0 AND 3),
  actor_id INTEGER NOT NULL CHECK (actor_id > 0),
  created_at INTEGER NOT NULL
);

-- One SQLite statement applies the adjustment and cancels a pending case.
-- A duplicate update_id has no effects; a processing case is not falsely
-- marked canceled because the Telegram ban request may already be in flight.
CREATE TRIGGER IF NOT EXISTS apply_warning_edit_receipt
AFTER INSERT ON warning_edit_receipts
BEGIN
  INSERT INTO warning_adjustments(chat_id,user_id,base_count,adjusted_at,violation_rowid,actor_id)
    SELECT NEW.chat_id,NEW.user_id,NEW.base_count,NEW.created_at,
           COALESCE(MAX(rowid),0),NEW.actor_id
    FROM violations WHERE chat_id=NEW.chat_id AND user_id=NEW.user_id
    ON CONFLICT(chat_id,user_id) DO UPDATE SET
      base_count=excluded.base_count,adjusted_at=excluded.adjusted_at,
      violation_rowid=excluded.violation_rowid,actor_id=excluded.actor_id;
  UPDATE pending_bans SET status='canceled',canceled_by=NEW.actor_id,
    cancellation_reason='warning_count_adjusted',resolved_at=NEW.created_at
    WHERE chat_id=NEW.chat_id AND user_id=NEW.user_id AND status='pending';
END;
