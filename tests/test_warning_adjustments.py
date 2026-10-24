"""Exact SQLite query and permission-scope contract for administrator warning adjustments."""
from pathlib import Path
import re
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = (ROOT / "schema/current.sql").read_text()
ENSURE = (ROOT / "schema/ensure_warning_adjustments.sql").read_text()


def store_query(name):
    source = (ROOT / "src/store.rs").read_text()
    body = source.split(f"pub async fn {name}(", 1)[1]
    literal = re.search(r'prepare\(\s*"((?:[^"\\]|\\.)*)"', body, re.S)[1]
    return re.sub(r'\\\n\s*', '', literal)


class WarningAdjustmentSQLTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.db.executescript(SCHEMA)
        self.next_update_id = 5000
        self.upsert = store_query("set_warning_count")
        self.pending_sql = store_query("create_pending_after_warning_check")
        self.count = store_query("recent_strikes")

    def tearDown(self):
        self.db.close()

    def violation(self, chat, user, message, when):
        self.db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(?,?,?,?,'spam',0.98,'warned')", (chat, user, message, when)
        )

    def get_count(self, chat, user, cutoff):
        return self.db.execute(self.count, (chat, user, cutoff)).fetchone()[0]

    def set_count(self, chat, user, count, when, actor=11):
        self.db.execute(self.upsert, (self.next_update_id, chat, user, count, actor, when))
        self.next_update_id += 1

    def test_existing_strikes_survive_reset_and_later_strike_counts_even_in_same_second(self):
        timestamp = 1_700_000_000
        self.violation(-100, 22, 401, timestamp - 100)
        self.violation(-100, 22, 402, timestamp - 10)
        self.assertEqual(self.get_count(-100, 22, timestamp - 7 * 86400), 2)
        self.set_count(-100, 22, 0, timestamp)
        self.assertEqual(self.get_count(-100, 22, timestamp - 7 * 86400), 0)
        self.violation(-100, 22, 403, timestamp)
        self.assertEqual(self.get_count(-100, 22, timestamp - 7 * 86400), 1)
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM violations").fetchone()[0], 3)
        self.assertEqual(self.db.execute(
            "SELECT COUNT(*) FROM violations WHERE chat_id=-100 AND message_id=401"
        ).fetchone()[0], 1, "duplicate-punishment guard is unchanged")

    def test_exact_group_and_user_isolation(self):
        now = 1_700_000_000
        for chat, user, message in [(-100, 22, 1), (-200, 22, 2), (-100, 33, 3)]:
            self.violation(chat, user, message, now)
        self.set_count(-100, 22, 3, now, 11)
        self.assertEqual(self.get_count(-100, 22, now - 100), 3)
        self.assertEqual(self.get_count(-200, 22, now - 100), 1)
        self.assertEqual(self.get_count(-100, 33, now - 100), 1)
        self.set_count(-200, 22, 0, now, 21)
        self.assertEqual(self.get_count(-100, 22, now - 100), 3)
        self.assertEqual(self.get_count(-200, 22, now - 100), 0)
        self.assertEqual(self.db.execute(
            "SELECT actor_id FROM warning_adjustments WHERE chat_id=-100 AND user_id=22"
        ).fetchone(), (11,))

    def test_adjustments_expire_and_new_real_violations_keep_rolling(self):
        now = 1_700_000_000
        self.violation(-100, 22, 1, now - 60)
        self.set_count(-100, 22, 2, now)
        self.violation(-100, 22, 2, now + 600)
        self.assertEqual(self.get_count(-100, 22, now - 7 * 86400), 3)
        self.assertEqual(self.get_count(-100, 22, now + 1), 1,
                         "expired baseline must not mask newer genuine strikes")
        self.set_count(-100, 22, 1, now + 601)
        self.assertEqual(self.get_count(-100, 22, now + 1), 1)
        self.violation(-100, 22, 3, now + 601)
        self.assertEqual(self.get_count(-100, 22, now + 1), 2)

    def test_same_update_replayed_after_new_strike_does_not_erase_it(self):
        when = 1_700_000_000
        self.violation(-100,22,401,when)
        self.db.execute(self.upsert,(900,-100,22,0,11,when))
        self.violation(-100,22,402,when+1)
        self.assertEqual(self.get_count(-100,22,when-7*86400),1)
        self.db.execute(self.upsert,(900,-100,22,0,11,when+2))
        self.assertEqual(self.get_count(-100,22,when-7*86400),1)
        self.assertEqual(self.db.execute(
            "SELECT COUNT(*) FROM warning_edit_receipts").fetchone(),(1,))

    def test_retention_cannot_reuse_a_rowid_below_a_live_warning_baseline(self):
        # Old violations can be >30 days old while a new adjustment is active.
        self.db.execute("INSERT INTO violations(rowid,chat_id,user_id,message_id,created_at,category,probability,action) VALUES(100,-100,22,1,1,'spam',1,'warned')")
        self.set_count(-100,22,0,3_000_000)
        self.db.execute("DELETE FROM violations WHERE created_at<100")
        add = store_query('add_strike')
        self.db.execute(add, (-100,22,2,3_000_000,'spam',1))
        self.assertEqual(self.db.execute('SELECT rowid,chat_id,user_id,message_id FROM violations').fetchone(), (101,-100,22,2))
        self.assertEqual(self.get_count(-100,22,2_900_000), 1)
        # A duplicate must not move its ID or count, even after later inserts.
        self.db.execute(add, (-200,33,3,3_000_000,'spam',1))
        self.db.execute(add, (-100,22,2,3_000_001,'spam',1))
        self.assertEqual(self.get_count(-100,22,2_900_000), 1)
        self.assertEqual(self.db.execute('SELECT COUNT(*) FROM violations').fetchone(), (2,))

    def test_reset_prevents_late_pending_creation_but_preserves_new_violation(self):
        when = 1_700_000_000
        for mid in [401,402,403]: self.violation(-100,22,mid,when)
        self.set_count(-100,22,0,when)
        self.assertIsNone(self.db.execute(self.pending_sql,(-100,22,403,when+1,when+61,None,None)).fetchone())
        self.assertIsNone(self.db.execute(
            "SELECT 1 FROM pending_bans WHERE chat_id=-100 AND user_id=22").fetchone())
        self.violation(-100,22,404,when+1)
        self.assertIsNotNone(self.db.execute(self.pending_sql,(-100,22,404,when+1,when+61,None,None)).fetchone())

    def test_reset_and_pending_cancel_are_atomic_group_scoped(self):
        when = 1_700_000_000
        self.violation(-100,22,500,when)
        self.violation(-200,22,600,when)
        for chat,mid in [(-100,500),(-200,600)]:
            self.db.execute("INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) VALUES(?,?,?, ?,?)",
                            (chat,22,mid,when,when+60))
        self.set_count(-100,22,0,when)
        self.assertEqual(self.db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-100").fetchone(),('canceled',))
        self.assertEqual(self.db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-200").fetchone(),('pending',))
        self.assertIsNone(self.db.execute(
            "SELECT 1 FROM warning_adjustments WHERE chat_id=-200").fetchone())

    def test_schema_constraints_and_idempotent_existing_database_setup(self):
        for chat, user, count, actor in [(1, 22, 2, 11), (-100, -22, 2, 11),
                                         (-100, 22, 4, 11), (-100, 22, -1, 11),
                                         (-100, 22, 2, -1)]:
            with self.assertRaises(sqlite3.IntegrityError):
                self.set_count(chat, user, count, 100, actor)
        self.db.executescript(ENSURE)
        self.db.executescript(ENSURE)
        old = sqlite3.connect(":memory:")
        try:
            old.executescript(
                "CREATE TABLE violations(chat_id INTEGER,user_id INTEGER,message_id INTEGER,"
                "created_at INTEGER,category TEXT,probability REAL,action TEXT);"
            )
            old.executescript(ENSURE)
            old.executescript(ENSURE)
            self.assertIn("warning_adjustments", {
                row[0] for row in old.execute("SELECT name FROM sqlite_master WHERE type='table'")
            })
        finally:
            old.close()


if __name__ == "__main__":
    unittest.main()
