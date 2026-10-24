"""Execute the actual store SQL against SQLite, including timer batch boundaries."""
from pathlib import Path
import re
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]


def query_for(function):
    source = (ROOT / 'src/store.rs').read_text()
    body = source.split(f'pub async fn {function}(', 1)[1]
    literal = re.search(r'prepare\(\s*"((?:[^"\\]|\\.)*)"', body, re.S)[1]
    return re.sub(r'\\\n\s*', '', literal)


class StoreQueryTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.executescript((ROOT / 'schema/current.sql').read_text())

    def tearDown(self):
        self.db.close()

    def add_case(self, user, due=160, status='pending'):
        self.db.execute('INSERT INTO pending_bans '
                        '(chat_id,user_id,message_id,created_at,due_at,status) '
                        'VALUES(-100,?,?,100,?,?)', (user, user + 1000, due, status))

    def claim_at(self, timestamp):
        self.db.create_function('unixepoch', 0, lambda: timestamp)
        return self.db.execute(query_for('claim_due'), (-100, 42, 1042)).fetchone()

    def test_due_batch_does_not_repeat_one_poison_case(self):
        for user in range(1, 11):
            self.add_case(user, due=150 + user)
        self.add_case(20, due=200)
        self.add_case(21, due=140, status='processing')
        rows = self.db.execute(query_for('due_pending'), (160,)).fetchall()
        self.assertEqual([row[1] for row in rows], list(range(1, 9)))

    def test_actual_claim_is_atomic_and_cannot_run_early(self):
        self.add_case(42)
        self.assertIsNone(self.claim_at(159))
        self.assertIsNotNone(self.claim_at(160))
        self.assertIsNone(self.claim_at(160))

    def test_actual_cancellation_prevents_stale_timer(self):
        self.add_case(42)
        cancel = query_for('cancel_pending')
        self.assertIsNotNone(self.db.execute(cancel, (9, 'admin_command', 155, -100, 42)).fetchone())
        self.assertIsNone(self.claim_at(160))

    def test_actual_claim_rejects_expired_case(self):
        self.add_case(42)
        self.assertIsNone(self.claim_at(761))

    def test_actual_claim_allows_cutoff_boundary(self):
        self.add_case(42)
        self.assertIsNotNone(self.claim_at(760))

    def test_group_setting_defaults_and_isolated_partial_updates(self):
        self.db.execute('INSERT INTO group_settings(chat_id) VALUES(-1002)')
        source=(ROOT/'src/store.rs').read_text()
        dry,logs=re.findall(r'"(INSERT INTO group_settings[^"\n]+)"', source.split('pub async fn set_group_option(',1)[1])
        self.db.execute(dry,(-1001,0))
        self.db.execute(logs,(-1001,0))
        self.assertEqual(self.db.execute('SELECT chat_id,dry_run,logs_enabled FROM group_settings ORDER BY chat_id DESC').fetchall(),[(-1001,0,0),(-1002,None,1)])
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute('INSERT INTO group_settings(chat_id) VALUES(123)')


if __name__ == '__main__':
    unittest.main()

class GroupThresholdTests(unittest.TestCase):
    setUp=StoreQueryTests.setUp
    tearDown=StoreQueryTests.tearDown
    def test_threshold_upsert_preserves_other_settings_and_other_groups(self):
        self.db.execute('INSERT INTO group_settings(chat_id,dry_run,logs_enabled) VALUES(-1001,0,0)')
        self.db.execute(query_for('set_group_threshold'),(-1001,0.75))
        self.db.execute(query_for('set_group_threshold'),(-1002,0.9))
        self.db.execute(query_for('set_group_threshold'),(-1001,0.8))
        self.assertEqual(self.db.execute('SELECT chat_id,dry_run,logs_enabled,remove_threshold FROM group_settings ORDER BY chat_id DESC').fetchall(),[(-1001,0,0,0.8),(-1002,None,1,0.9)])
        for value in (-0.1,1.01):
            with self.assertRaises(sqlite3.IntegrityError):
                self.db.execute(query_for('set_group_threshold'),(-1001,value))
