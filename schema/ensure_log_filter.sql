-- ONE-TIME additive migration. Run only the missing ALTER printed by preflight.
-- Existing groups retain their on/off setting and the default all-log behavior.
ALTER TABLE group_settings ADD COLUMN logs_warn_only INTEGER NOT NULL DEFAULT 0 CHECK (logs_warn_only IN (0,1));
