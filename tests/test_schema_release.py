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

    def migrate(self):
        for statement in guard.scan_migrations(guard.inspect_sqlite(self.db)):
            self.db.execute(statement)
        self.db.executescript(ENSURE)

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
        self.migrate()
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.db.execute(count, (-100, 22, 1699000000)).fetchone()[0], 1)
        upsert = store_query("set_warning_count")
        self.db.execute(upsert, (5000, -100, 22, 2, 11, 1700000000))
        adjusted = self.db.execute("SELECT * FROM warning_adjustments").fetchall()
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_edit_receipts").fetchone(), (1,))
        self.assertEqual(self.db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-100"
        ).fetchone(), ("canceled",), "the receipt trigger should cancel a pending ban")
        after_command = self.historical_snapshot()
        self.migrate()
        self.migrate()
        self.assertEqual(self.db.execute("SELECT * FROM warning_adjustments").fetchall(), adjusted)
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_edit_receipts").fetchone(), (1,))
        self.assertEqual(self.historical_snapshot(), after_command)
        self.assertEqual(after_command[0], before[0], "violation history must survive migration and command")
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.db.execute(count, (-100, 22, 1699000000)).fetchone()[0], 2)

    def test_incompatible_existing_warning_table_is_not_silently_repaired(self):
        self.db.execute(
            "CREATE TABLE warning_adjustments("
            "chat_id INTEGER,user_id INTEGER,base_count INTEGER,adjusted_at INTEGER,"
            "violation_rowid INTEGER,actor_id INTEGER,PRIMARY KEY(chat_id,user_id))"
        )
        before = self.historical_snapshot()
        self.migrate()  # IF NOT EXISTS silently does nothing.
        status, errors = self.state(pre=True)
        self.assertEqual(status, "incompatible")
        self.assertTrue(any("warning_adjustments" in reason for reason in errors))
        self.assertEqual(self.historical_snapshot(), before)

    def test_partial_failure_after_table_creation_is_retryable_without_data_loss(self):
        before = self.historical_snapshot()
        for statement in guard.scan_migrations(guard.inspect_sqlite(self.db)):
            self.db.execute(statement)
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.executescript(
                ENSURE + "INSERT INTO warning_adjustments "
                "(chat_id,user_id,base_count,adjusted_at,actor_id) "
                "VALUES(-100,22,99,1700000000,11);"
            )
        self.assertEqual(self.state(), ("ready", []))
        self.migrate()
        self.assertEqual(self.historical_snapshot(), before)
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_adjustments").fetchone(), (0,))

    def test_old_core_table_version_mismatch_blocks_migration_and_release(self):
        self.db.execute("DROP TABLE scan_jobs")
        self.db.execute("CREATE TABLE scan_jobs(chat_id INTEGER PRIMARY KEY)")
        status, reasons = self.state(pre=True)
        self.assertEqual(status, "incompatible")
        self.assertTrue(any("scan_jobs" in r for r in reasons))
        self.migrate()
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
        self.migrate()
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
        self.assertIn("'trigger'", guard.INSPECTION_SQL)
        self.assertIn("m.tbl_name", guard.INSPECTION_SQL)
        self.assertNotIn("INSERT ", guard.INSPECTION_SQL.upper())
        self.assertNotIn("DELETE ", guard.INSPECTION_SQL.upper())

    def test_d1_literal_pragma_batch_matches_local_metadata_and_rejects_partial_results(self):
        def batch():
            self.db.row_factory = sqlite3.Row
            try:
                return [{"success": True, "results": [dict(row) for row in self.db.execute(query)]}
                        for query in guard.inspection_queries()]
            finally:
                self.db.row_factory = None
        old = guard.parse_inspection_json(json.dumps(batch()))
        self.assertEqual(guard.validate(old, allow_missing_warning=True), ("migration_required", []))
        self.migrate()
        results = batch()
        rows = guard.parse_inspection_json(json.dumps(results))
        self.assertEqual(guard.validate(rows), ("ready", []))
        expected = guard.objects(guard.inspect_sqlite(self.db))
        for key in guard.objects(guard.expected_metadata()):
            self.assertEqual(guard.objects(rows)[key], expected[key])
        self.assertTrue(all(query.startswith(('SELECT ', 'PRAGMA table_info("'))
                            for query in guard.inspection_queries()))
        with self.assertRaises(ValueError):
            guard.parse_inspection_json(json.dumps(results[:-1]))
        results[1]["success"] = False
        with self.assertRaises(ValueError):
            guard.parse_inspection_json(json.dumps(results))


    def test_post_pr11_database_needs_only_additive_receipt_migration(self):
        # PR #11 already created adjustments, but PR #13 has not yet run.
        self.db.executescript(ENSURE.split("-- Durable per-Telegram-update receipt", 1)[0])
        before = self.historical_snapshot()
        self.assertEqual(self.state(pre=True), ("migration_required", []))
        self.assertEqual(self.state()[0], "incompatible")
        self.migrate()
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.historical_snapshot(), before)
        self.migrate()
        self.assertEqual(self.historical_snapshot(), before)

    def test_adjustment_table_only_and_both_new_objects_missing_is_retryable(self):
        self.migrate()
        self.db.execute("DROP TABLE warning_edit_receipts")  # SQLite also removes its trigger.
        previous = self.historical_snapshot()
        self.assertEqual(self.state(pre=True), ("migration_required", []))
        self.assertEqual(self.state()[0], "incompatible")
        self.migrate()
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.historical_snapshot(), previous)

    def test_receipts_present_without_trigger_must_fail_closed(self):
        self.migrate()
        self.db.execute("DROP TRIGGER apply_warning_edit_receipt")
        # An accepted receipt can exist without applying any side effect.
        self.db.execute(
            "INSERT INTO warning_edit_receipts(update_id,chat_id,user_id,base_count,actor_id,created_at) "
            "VALUES(700,-100,22,0,11,1700000001)"
        )
        for pre in (False, True):
            with self.subTest(pre_migration=pre):
                state, errors = self.state(pre=pre)
                self.assertEqual(state, "incompatible")
                self.assertTrue(any("trigger" in error for error in errors))
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_adjustments").fetchone(), (0,))
        self.assertEqual(self.db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-100"
        ).fetchone(), ("pending",))
        # Rerunning additive SQL would recreate the trigger, not backfill the
        # previously accepted but unapplied receipt; do NOT report code 3.
        self.migrate()
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_adjustments").fetchone(), (0,))

    def test_wrong_named_trigger_definition_is_not_deployment_ready(self):
        self.migrate()
        self.db.execute("DROP TRIGGER apply_warning_edit_receipt")
        self.db.executescript(
            "CREATE TRIGGER apply_warning_edit_receipt "
            "AFTER INSERT ON warning_edit_receipts BEGIN SELECT 1; END"
        )
        for pre in (False, True):
            state, errors = self.state(pre=pre)
            self.assertEqual(state, "incompatible")
            self.assertTrue(any("trigger definition mismatch" in error for error in errors))
        self.db.execute(
            "INSERT INTO warning_edit_receipts(update_id,chat_id,user_id,base_count,actor_id,created_at) "
            "VALUES(710,-100,22,0,11,1700000001)"
        )
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_adjustments").fetchone(), (0,))

    def test_receipt_table_without_adjustments_is_incompatible(self):
        self.migrate()
        self.db.execute("DROP TABLE warning_adjustments")
        state, reasons = self.state(pre=True)
        self.assertEqual(state, "incompatible")
        self.assertTrue(any("receipt table exists without warning_adjustments" in r for r in reasons))

    def test_extra_trigger_on_sensitive_table_is_incompatible(self):
        self.migrate()
        self.db.executescript(
            "CREATE TRIGGER unexpected_receipt_effect AFTER INSERT ON warning_edit_receipts "
            "BEGIN UPDATE pending_bans SET status='canceled' WHERE status='pending'; END"
        )
        state, reasons = self.state()
        self.assertEqual(state, "incompatible")
        self.assertTrue(any("unexpected trigger" in reason for reason in reasons))

    def test_repeat_receipt_cannot_erase_a_new_genuine_violation(self):
        self.migrate()
        upsert = store_query("set_warning_count")
        count = store_query("recent_strikes")
        self.db.execute(upsert, (901, -100, 22, 0, 11, 1700000001))
        baseline = self.db.execute(
            "SELECT base_count,violation_rowid FROM warning_adjustments "
            "WHERE chat_id=-100 AND user_id=22"
        ).fetchone()
        self.assertEqual(baseline, (0, 1))
        self.db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(-100,22,43,1700000002,'spam',0.99,'warned')"
        )
        self.db.execute(upsert, (901, -100, 22, 0, 11, 1700000003))
        self.assertEqual(self.db.execute(
            "SELECT base_count,violation_rowid FROM warning_adjustments "
            "WHERE chat_id=-100 AND user_id=22"
        ).fetchone(), baseline)
        self.assertEqual(self.db.execute(count, (-100, 22, 1699000000)).fetchone(), (1,))
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM warning_edit_receipts").fetchone(), (1,))
        self.assertIsNone(self.db.execute(
            "SELECT 1 FROM warning_adjustments WHERE chat_id=-200"
        ).fetchone())

    def test_trigger_behaviour_is_verified_on_dummy_rows_only(self):
        self.migrate()
        guard.verify_warning_trigger_behavior()
        rows = guard.inspect_sqlite(self.db)
        self.assertEqual(
            len([row for row in rows if row["object_type"] == "trigger"
                 and row["object_name"] == "apply_warning_edit_receipt"]), 1
        )

    def test_trigger_comparison_preserves_string_literal_semantics(self):
        self.migrate()
        rows = guard.inspect_sqlite(self.db)
        for row in rows:
            if row["object_type"] == "trigger":
                row["definition"] = row["definition"].replace("'pending'", "'PENDING'")
        state, errors = guard.validate(rows, allow_missing_warning=True)
        self.assertEqual(state, "incompatible")
        self.assertTrue(any("trigger definition mismatch" in error for error in errors))

    def test_scan_migrations_resume_each_partial_column_state_and_preserve_jobs(self):
        self.db.execute("INSERT INTO scan_jobs(chat_id,max_message_id,started_at,updated_at) VALUES(-100,42,1,1)")
        self.db.executescript(ENSURE)
        statements = guard.scan_migrations(guard.inspect_sqlite(self.db))
        self.assertEqual(len(statements), 3)
        for i, statement in enumerate(statements):
            self.assertEqual(self.state(pre=True), ("migration_required", []))
            self.assertEqual(self.state()[0], "incompatible")
            self.db.execute(statement)
            remaining = guard.scan_migrations(guard.inspect_sqlite(self.db))
            self.assertEqual(remaining, statements[i+1:])
        self.assertEqual(self.state(), ("ready", []))
        self.assertEqual(self.db.execute("SELECT chat_id,max_message_id,generation,lease_id,lease_until FROM scan_jobs").fetchone(), (-100,42,1,0,0))

    def test_wrong_scan_column_is_not_migratable(self):
        self.db.execute("ALTER TABLE scan_jobs ADD COLUMN generation TEXT")
        status, errors = self.state(pre=True)
        self.assertEqual(status, "incompatible")
        self.assertTrue(any("scan_jobs.generation" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
