-- ONE-TIME additive migration. Run only the missing ALTER printed by preflight.
-- Existing rows retain their on/off mode; legacy DEFAULT 0 remains accepted.
-- The Worker uses warn for absent rows and explicitly initializes new settings rows to warn.
ALTER TABLE group_settings ADD COLUMN logs_warn_only INTEGER NOT NULL DEFAULT 0 CHECK (logs_warn_only IN (0,1));
