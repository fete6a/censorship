-- Canonical D1 application schema, captured after successful 0006_pending_retry.sql.
-- Use ONLY on a newly created, empty D1 database.
-- Do not run against an existing database: upgrades and legacy migrations are not supported.
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
