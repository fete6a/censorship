ALTER TABLE group_settings ADD COLUMN remove_threshold REAL
  CHECK(remove_threshold IS NULL OR remove_threshold BETWEEN 0 AND 1);
