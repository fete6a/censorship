-- Multi-group onboarding: only admin bots with delete rights can actively moderate.
-- Use 'active' for supergroups with delete/restrict rights, 'delete_only' for
-- groups without restrict rights, and 'paused' for insufficient privileges.
CREATE TABLE IF NOT EXISTS managed_chats (
  chat_id INTEGER PRIMARY KEY,
  title TEXT NOT NULL DEFAULT '',
  chat_type TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','delete_only','paused','disabled')),
  updated_at INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_managed_chats_status ON managed_chats(status);
