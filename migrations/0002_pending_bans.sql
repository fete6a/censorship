-- Two-stage sanctions: Telegram restrictChatMember, then optional ban after one minute.
-- Status is monotonic: pending -> processing -> banned/failed, or pending -> canceled.
CREATE TABLE IF NOT EXISTS pending_bans (
  chat_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  due_at INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'processing', 'banned', 'canceled', 'failed')),
  canceled_by INTEGER,
  cancellation_reason TEXT,
  resolved_at INTEGER,
  PRIMARY KEY (chat_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_pending_bans_due ON pending_bans(status, due_at);
