CREATE TABLE IF NOT EXISTS group_settings (
  chat_id INTEGER PRIMARY KEY CHECK (chat_id < 0),
  dry_run INTEGER CHECK (dry_run IN (0,1)),
  logs_enabled INTEGER NOT NULL DEFAULT 1 CHECK (logs_enabled IN (0,1))
);
