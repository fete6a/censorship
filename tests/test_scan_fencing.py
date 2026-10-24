"""Exercise production SQL against SQLite, including abandoned Cron claims."""
import sqlite3
import unittest
from test_store_queries import ROOT, query_for


class ScanFencingTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.executescript((ROOT / 'schema/current.sql').read_text())
        self.time = 100
        self.db.create_function('unixepoch', 0, lambda: self.time)

    def tearDown(self):
        self.db.close()

    def start(self, chat=-100, enforce=1):
        # start_scan's first query captures the archive maximum; this is its
        # actual second, conditional INSERT rather than a copied approximation.
        import re
        body = (ROOT / 'src/store.rs').read_text().split('pub async fn start_scan(', 1)[1]
        literals = re.findall(r'prepare\(\s*"((?:[^"\\]|\\.)*)"', body, re.S)
        sql = re.sub(r'\\\n\s*', '', literals[1])
        return self.db.execute(sql, (chat, 42, enforce, self.time, self.time)).fetchone()

    def claim(self):
        self.db.row_factory = sqlite3.Row
        try:
            return self.db.execute(query_for('claim_active_scan')).fetchone()
        finally:
            self.db.row_factory = None

    def scope(self, job):
        return (job['chat_id'], job['generation'], job['lease_id'])

    def test_expired_claim_cannot_advance_finish_stop_or_release_successor(self):
        self.start()
        old = self.claim()
        self.assertIsNone(self.claim())
        self.time += 1200
        new = self.claim()
        self.assertGreater(new['lease_id'], old['lease_id'])
        old_scope = self.scope(old)
        self.assertEqual(self.db.execute(query_for('scan_is_current'), old_scope).fetchone(), (0,))
        self.assertIsNone(self.db.execute(query_for('advance_scan'), (42, 1, *old_scope, 0, 42, 42)).fetchone())
        self.assertIsNone(self.db.execute(query_for('finish_scan'), (*old_scope, 0)).fetchone())
        self.db.execute(query_for('stop_scan_if_current'), old_scope)
        self.db.execute(query_for('release_scan_claim'), old_scope)
        self.assertEqual(self.db.execute(query_for('scan_is_current'), self.scope(new)).fetchone(), (1,))
        self.assertIsNotNone(self.db.execute(query_for('advance_scan'), (42, 1, *self.scope(new), 0, 42, 42)).fetchone())
        self.assertIsNotNone(self.db.execute(query_for('finish_scan'), (*self.scope(new), 42)).fetchone())

    def test_stopped_mute_cannot_schedule_ban_but_fresh_scan_and_live_can(self):
        self.start()
        old = self.claim()
        self.db.execute("INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(-100,22,42,100,'spam',1,'deleted')")
        self.db.execute(query_for('stop_scan'), (self.time, -100))
        self.start(enforce=0)
        new = self.claim()
        sql = query_for('create_pending_after_warning_check')
        args = (-100, 22, 42, 100, 160)
        self.assertIsNone(self.db.execute(sql, (*args, old['generation'], old['lease_id'])).fetchone())
        self.assertIsNotNone(self.db.execute(sql, (*args, new['generation'], new['lease_id'])).fetchone())
        self.db.execute("DELETE FROM pending_bans")
        self.assertIsNotNone(self.db.execute(sql, (*args, None, None)).fetchone())

    def test_two_chats_claim_independently_and_duplicate_start_keeps_mode(self):
        self.start()
        self.assertIsNone(self.start(enforce=0))
        self.start(chat=-200, enforce=0)
        first, second = self.claim(), self.claim()
        self.assertEqual({first['chat_id'], second['chat_id']}, {-100, -200})
        self.assertIsNone(self.claim())
        self.assertEqual(self.db.execute('SELECT enforce FROM scan_jobs WHERE chat_id=-100').fetchone(), (1,))
