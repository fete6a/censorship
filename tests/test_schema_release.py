"""Fail-closed production D1 preflight and non-destructive legacy migrations."""
import json
from pathlib import Path
import re
import sqlite3
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import check_d1_schema as guard

OLD_SCHEMA = (ROOT / "tests/fixtures/schema_before_warning_adjustments.sql").read_text(encoding="utf-8")
ENSURE = (ROOT / "schema/ensure_warning_adjustments.sql").read_text(encoding="utf-8")


def store_query(name):
    source = (ROOT / "src/store.rs").read_text(encoding="utf-8")
    body = source.split(f"pub async fn {name}(", 1)[1]
    literal = re.search(r'prepare\(\s*"((?:[^"\\]|\\.)*)"', body, re.S)[1]
    return re.sub(r'\\\n\s*', '', literal)


class D1ReleaseSafetyTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.db.executescript(OLD_SCHEMA)
        self.db.execute("CREATE TABLE d1_migrations(id INTEGER PRIMARY KEY, name TEXT)")
        self.db.execute("INSERT INTO d1_migrations(id,name) VALUES(6,'0006_pending_retry.sql')")
        self.db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(-100,22,42,1700000000,'spam',0.99,'warned')"
        )
        self.db.execute(
            "INSERT INTO archived_messages(chat_id,message_id,user_id,sent_at,updated_at,payload) "
            "VALUES(-100,42,22,1700000000,1700000000,'{}')"
        )
        self.db.execute("INSERT INTO group_settings(chat_id,dry_run) VALUES(-100,0)")
        self.db.execute(
            "INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) "
            "VALUES(-100,22,42,1700000000,1700000060)"
        )

    def tearDown(self):
        self.db.close()

    def state(self, pre=False):
        return guard.validate(guard.inspect_sqlite(self.db), allow_missing_warning=pre)

    def historical_snapshot(self):
        return tuple(self.db.execute(f"SELECT * FROM {name}").fetchall()
                     for name in ("violations", "archived_messages", "group_settings",
                                  "pending_bans", "d1_migrations"))

    def test_pre_pr11_snapshot_requires_migration_and_normal_check_blocks(self):
        self.assertNotIn("warning_adjustments", OLD_SCHEMA)
        self.assertEqual(self.state(pre=True), ("migration_required", []))
        status, errors = self.state()
        self.assertEqual(status, "incompatible")
        self.assertIn("missing required table: warning_adjustments", errors)
        self.assertEqual(self.db.total_changes, 5, "read-only preflight must not mutate history")

    def test_migrate_old_schema_preserves_all_data_and_repeated_migration(self):
        before = self.historical_snapshot()
        count = store_query("recent_strikes")
        with self.assertRaises(sqlite3.OperationalError):
            self.db.execute(count, (-100, 22, 1699000000)).fetchone()
        self.db.executescript(ENSURE)
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.db.execute(count, (-100, 22, 1699000000)).fetchone()[0], 1)
        upsert = store_query("set_warning_count")
        self.db.execute(upsert, (-100, 22, 2, 1700000000, 11))
        adjusted = self.db.execute("SELECT * FROM warning_adjustments").fetchall()
        self.db.executescript(ENSURE)
        self.db.executescript(ENSURE)
        self.assertEqual(self.db.execute("SELECT * FROM warning_adjustments").fetchall(), adjusted)
        self.assertEqual(self.historical_snapshot(), before)
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.db.execute(count, (-100, 22, 1699000000)).fetchone()[0], 2)

    def test_incompatible_existing_warning_table_is_not_silently_repaired(self):
        self.db.execute(
            "CREATE TABLE warning_adjustments("
            "chat_id INTEGER,user_id INTEGER,base_count INTEGER,adjusted_at INTEGER,"
            "violation_rowid INTEGER,actor_id INTEGER,PRIMARY KEY(chat_id,user_id))"
        )
        before = self.historical_snapshot()
        self.db.executescript(ENSURE)  # IF NOT EXISTS silently does nothing.
        status, errors = self.state(pre=True)
        self.assertEqual(status, "incompatible")
        self.assertTrue(any("warning_adjustments" in reason for reason in errors))
        self.assertEqual(self.historical_snapshot(), before)

    def test_partial_failure_after_table_creation_is_retryable_without_data_loss(self):
        before = self.historical_snapshot()
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.executescript(
                ENSURE + "INSERT INTO warning_adjustments "
                "(chat_id,user_id,base_count,adjusted_at,actor_id) "
                "VALUES(-100,22,99,1700000000,11);"
            )
        self.assertEqual(self.state(), ("ready", []))
        self.db.executescript(ENSURE)
        self.assertEqual(self.historical_snapshot(), before)
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_adjustments").fetchone(), (0,))

    def test_old_core_table_version_mismatch_blocks_migration_and_release(self):
        self.db.execute("DROP TABLE scan_jobs")
        self.db.execute("CREATE TABLE scan_jobs(chat_id INTEGER PRIMARY KEY)")
        status, reasons = self.state(pre=True)
        self.assertEqual(status, "incompatible")
        self.assertTrue(any("scan_jobs" in r for r in reasons))
        self.db.executescript(ENSURE)
        self.assertEqual(self.state()[0], "incompatible")

    def test_missing_core_table_and_empty_database_are_not_approved(self):
        self.db.execute("DROP TABLE processed_updates")
        self.assertEqual(self.state(pre=True)[0], "incompatible")
        empty = sqlite3.connect(":memory:")
        try:
            status, errors = guard.validate(guard.inspect_sqlite(empty), allow_missing_warning=True)
            self.assertEqual(status, "incompatible")
            self.assertIn("missing required table: violations", errors)
        finally:
            empty.close()

    def test_metadata_rejects_failed_or_ambiguous_remote_query_results(self):
        self.db.executescript(ENSURE)
        rows = guard.inspect_sqlite(self.db)
        self.assertEqual(guard.parse_d1_json(json.dumps([{"success": True, "results": rows}])), rows)
        self.assertEqual(guard.parse_d1_json(json.dumps({"success": True, "results": rows})), rows)
        for payload in (
            [], [{"success": False, "results": rows}],
            [{"success": True, "results": []}],
            [{"success": True, "results": rows}, {"success": True, "results": rows}],
            [{"success": True, "results": [{"object_type": "table"}]}],
        ):
            with self.subTest(payload=str(payload)[:60]):
                with self.assertRaises(ValueError):
                    guard.parse_d1_json(json.dumps(payload))
        with self.assertRaises(json.JSONDecodeError):
            guard.parse_d1_json("not JSON")

    def test_runtime_pre_delete_schema_guard_and_safe_cleanup(self):
        source = (ROOT / "src/moderation.rs").read_text(encoding="utf-8")
        live = source.split("async fn review_with_policy_cache(", 1)[1].split("fn command_parts", 1)[0]
        guard_at = live.index("store::recent_strikes(env, chat_id, user_id).await?;")
        delete_at = live.index("tg::delete(env, chat_id, msg_id).await?;")
        self.assertLess(guard_at, delete_at, "schema check must precede destructive Telegram API")
        store = (ROOT / "src/store.rs").read_text(encoding="utf-8")
        self.assertIn("no such table: warning_adjustments", store)
        self.assertIn("warning_adjustments_missing", store)
        self.assertIn("DELETE FROM pending_bans WHERE", store.split("pub async fn cleanup(", 1)[1])

    def test_inspection_sql_is_read_only(self):
        self.assertTrue(guard.INSPECTION_SQL.lstrip().upper().startswith("SELECT "))
        self.assertIn("FROM sqlite_master", guard.INSPECTION_SQL)
        self.assertNotIn("INSERT ", guard.INSPECTION_SQL.upper())
        self.assertNotIn("DELETE ", guard.INSPECTION_SQL.upper())


if __name__ == "__main__":
    unittest.main()
