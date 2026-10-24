-- Bounded fair retries for pending Telegram bans; existing rows are eligible immediately.
ALTER TABLE pending_bans ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pending_bans ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0);
CREATE INDEX IF NOT EXISTS idx_pending_bans_ready
  ON pending_bans(status, next_attempt_at, due_at, chat_id, user_id);
