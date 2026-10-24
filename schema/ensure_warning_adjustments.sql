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
