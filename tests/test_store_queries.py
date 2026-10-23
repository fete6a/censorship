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
        for migration in sorted((ROOT / 'migrations').glob('*.sql')):
            self.db.executescript(migration.read_text())

    def tearDown(self):
        self.db.close()

    def add_case(self, user, due=160, status='pending'):
        self.db.execute('INSERT INTO pending_bans '
                        '(chat_id,user_id,message_id,created_at,due_at,status) '
                        'VALUES(-100,?,?,100,?,?)', (user, user + 1000, due, status))

    def test_due_batch_does_not_repeat_one_poison_case(self):
        for user in range(1, 11):
            self.add_case(user, due=150 + user)
        self.add_case(20, due=200)
        self.add_case(21, due=140, status='processing')
        rows = self.db.execute(query_for('due_pending'), (160,)).fetchall()
        self.assertEqual([row[1] for row in rows], list(range(1, 9)))

    def test_actual_claim_is_atomic_and_cannot_run_early(self):
        self.add_case(42)
        claim = query_for('claim_due')
        self.assertIsNone(self.db.execute(claim, (-100, 42, 1042, 159)).fetchone())
        self.assertIsNotNone(self.db.execute(claim, (-100, 42, 1042, 160)).fetchone())
        self.assertIsNone(self.db.execute(claim, (-100, 42, 1042, 160)).fetchone())

    def test_actual_cancellation_prevents_stale_timer(self):
        self.add_case(42)
        cancel = query_for('cancel_pending')
        self.assertIsNotNone(self.db.execute(cancel, (9, 'admin_command', 155, -100, 42)).fetchone())
        self.assertIsNone(self.db.execute(query_for('claim_due'), (-100, 42, 1042, 160)).fetchone())


if __name__ == '__main__':
    unittest.main()
