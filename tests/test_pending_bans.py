"""Behavior checks on the SQL state machine; not a Rust compilation test."""
from pathlib import Path
import json, sqlite3, unittest
ROOT = Path(__file__).resolve().parents[1]

class PendingBanStateTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.executescript((ROOT / 'schema/current.sql').read_text())
        self.db.execute("INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at,status) VALUES(-100,42,1001,100,160,'pending')")
    def tearDown(self): self.db.close()
    def test_cancellation_wins_before_deadline(self):
        row=self.db.execute("UPDATE pending_bans SET status='canceled',canceled_by=9, resolved_at=155 WHERE chat_id=-100 AND user_id=42 AND status='pending' RETURNING message_id,status").fetchone()
        self.assertEqual(row,(1001,'canceled'))
        claim=self.db.execute("UPDATE pending_bans SET status='processing' WHERE chat_id=-100 AND user_id=42 AND message_id=1001 AND status='pending' AND due_at<=160 RETURNING message_id").fetchone()
        self.assertIsNone(claim)
    def test_claim_not_early(self):
        early=self.db.execute("UPDATE pending_bans SET status='processing' WHERE chat_id=-100 AND user_id=42 AND status='pending' AND due_at<=159 RETURNING message_id").fetchone()
        self.assertIsNone(early)
    def test_only_one_claim(self):
        sql="UPDATE pending_bans SET status='processing' WHERE chat_id=-100 AND user_id=42 AND message_id=1001 AND status='pending' AND due_at<=160 RETURNING message_id"
        self.assertEqual(self.db.execute(sql).fetchone(),(1001,))
        self.assertIsNone(self.db.execute(sql).fetchone())
        self.assertIsNone(self.db.execute("UPDATE pending_bans SET status='canceled' WHERE status='pending' AND user_id=42 RETURNING message_id").fetchone())
    def test_stale_timer_cannot_claim_new_case(self):
        self.db.execute("UPDATE pending_bans SET status='canceled' WHERE user_id=42")
        self.db.execute("INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at,status) VALUES(-100,42,1002,180,240,'pending') ON CONFLICT(chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,created_at=excluded.created_at,due_at=excluded.due_at,status='pending' WHERE pending_bans.status IN ('canceled','banned','failed')")
        old=self.db.execute("UPDATE pending_bans SET status='processing' WHERE user_id=42 AND message_id=1001 AND due_at<=240 AND status='pending' RETURNING message_id").fetchone()
        self.assertIsNone(old)
        self.assertEqual(self.db.execute('SELECT message_id,status FROM pending_bans').fetchone(),(1002,'pending'))
    def test_no_replace_existing_pending(self):
        self.db.execute("INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at,status) VALUES(-100,42,2002,140,200,'pending') ON CONFLICT(chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,created_at=excluded.created_at,due_at=excluded.due_at,status='pending' WHERE pending_bans.status IN ('canceled','banned','failed')")
        self.assertEqual(self.db.execute('SELECT message_id,due_at FROM pending_bans').fetchone(),(1001,160))
    def test_configuration_delayed_queue_lto_and_chatmember(self):
        config=json.loads((ROOT/'wrangler.jsonc').read_text())
        producer=next(x for x in config['queues']['producers'] if x['binding']=='BAN_DELAY_QUEUE')
        self.assertEqual(producer['delivery_delay'],60)
        self.assertTrue(any(c['queue']==producer['queue'] for c in config['queues']['consumers']))
        cargo=(ROOT/'Cargo.toml').read_text();self.assertIn('lto = "fat"',cargo)
        self.assertIn('codegen-units = 1',cargo)
        code=(ROOT/'src/moderation.rs').read_text()
        self.assertIn('"/cancel"',code)
        self.assertIn('cancel_from_member_update',code)
        self.assertIn('store::claim_due(',code)
        self.assertIn('"administrator" | "creator" | "left" | "kicked"',code)
        self.assertIn('tg::mute_one_minute(',code)
    def test_late_processing_cancels(self):
        code=(ROOT/'src/moderation.rs').read_text()
        self.assertIn('saturating_sub(case.due_at)>600',code)
        self.assertIn('"timer_too_late"',code)
        self.assertIn('"admin_unmuted_in_telegram"',code)

if __name__=='__main__': unittest.main(verbosity=2)
