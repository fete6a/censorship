-- Existing 1.x violations table is intentionally left compatible.
CREATE TABLE IF NOT EXISTS violations (
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
CREATE INDEX IF NOT EXISTS idx_violations_recent
  ON violations(chat_id, user_id, created_at);

-- Archive only what is needed to replay Bot API messages; no media bytes.
CREATE TABLE IF NOT EXISTS archived_messages (
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
CREATE INDEX IF NOT EXISTS idx_archive_by_time
  ON archived_messages(chat_id, sent_at);

CREATE TABLE IF NOT EXISTS scan_jobs (
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

-- Mark updates only after successful processing, so Queue retries remain possible.
CREATE TABLE IF NOT EXISTS processed_updates (
  update_id INTEGER PRIMARY KEY,
  processed_at INTEGER NOT NULL
);
