"""SQLite regression tests for fair pending-ban retries; fake Telegram only."""
import sqlite3
import unittest
from pathlib import Path

from test_store_queries import query_for

ROOT = Path(__file__).resolve().parents[1]


class FakeTelegram:
    def __init__(self):
        self.fail_lookup = set()
        self.fail_ban = set()
        self.calls = []
        self.banned = []

    def get_member(self, key):
        self.calls.append(("lookup", key))
        if key in self.fail_lookup:
            raise ConnectionError("simulated Telegram lookup failure")

    def ban(self, key):
        self.calls.append(("ban", key))
        if key in self.fail_ban:
            raise ConnectionError("simulated Telegram ban failure")
        self.banned.append(key)


class PendingRetryTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.db.executescript((ROOT / "schema/current.sql").read_text())
        self.time = 200
        self.db.create_function("unixepoch", 0, lambda: self.time)
        self.tg = FakeTelegram()

    def tearDown(self):
        self.db.close()

    def insert(self, chat, user, due=180):
        mid = abs(chat) * 100 + user
        self.db.execute(
            "INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) "
            "VALUES(?,?,?,100,?)", (chat,user,mid,due)
        )
        return mid

    def state(self, chat, user):
        return self.db.execute(
            "SELECT message_id,status,attempt_count,next_attempt_at "
            "FROM pending_bans WHERE chat_id=? AND user_id=?",(chat,user)
        ).fetchone()

    def run_tick(self):
        rows = self.db.execute(query_for("due_pending"),(self.time,)).fetchall()
        self.assertLessEqual(len(rows),8)
        for chat,user,mid,*_ in rows:
            reserved = self.db.execute(
                query_for("begin_pending_attempt"),(chat,user,mid)
            ).fetchone()
            if reserved is None:
                continue
            key=(chat,user)
            try:
                self.tg.get_member(key)
            except ConnectionError:
                continue
            claim=self.db.execute(query_for("claim_due"),(chat,user,mid)).fetchone()
            if claim is None:
                continue
            try:
                self.tg.ban(key)
            except ConnectionError:
                self.db.execute(query_for("retry_pending"),(chat,user,mid))
            else:
                self.db.execute(query_for("conclude_pending"),
                                ("banned",self.time,chat,user,mid))
        return [(r[0],r[1]) for r in rows]

    def test_first_eight_fail_later_four_execute_then_eight_recover(self):
        for u in range(1,9):
            self.insert(-200,u)
            self.tg.fail_lookup.add((-200,u))
        for u in range(1,5):
            self.insert(-100,u)
        self.assertEqual(self.run_tick(),[(-200,u) for u in range(1,9)])
        self.assertEqual(self.run_tick(),[(-100,u) for u in range(1,5)])
        self.assertEqual(self.tg.banned,[(-100,u) for u in range(1,5)])
        self.assertEqual(self.run_tick(),[])
        self.time=259
        self.assertEqual(self.run_tick(),[])
        self.time=260
        self.tg.fail_lookup.clear()
        self.assertEqual(self.run_tick(),[(-200,u) for u in range(1,9)])
        self.assertEqual(len(self.tg.banned),12)
        self.assertEqual(len(set(self.tg.banned)),12)
        self.assertEqual(self.run_tick(),[])
        for chat,count in [(-100,4),(-200,8)]:
            for user in range(1,count+1):
                self.assertEqual(self.state(chat,user)[1],"banned")

    def test_ban_failure_backoff_recovery_no_duplicate_ban(self):
        mid=self.insert(-200,7)
        self.tg.fail_ban.add((-200,7))
        self.run_tick()
        self.assertEqual(self.state(-200,7),(mid,"pending",1,260))
        self.time=259
        self.assertEqual(self.run_tick(),[])
        self.time=260
        self.run_tick()
        self.assertEqual(self.state(-200,7),(mid,"pending",2,380))
        self.time=380
        self.tg.fail_ban.clear()
        self.run_tick()
        self.assertEqual(self.state(-200,7)[1],"banned")
        self.assertEqual(self.tg.banned,[(-200,7)])
        self.assertEqual(len([c for c in self.tg.calls if c[0]=="ban"]),3)
        self.time=620
        self.assertEqual(self.run_tick(),[])

    def test_concurrent_attempt_cancellation_stale_timer_and_group_isolation(self):
        mid=self.insert(-200,7)
        other=self.insert(-100,7)
        reserve=query_for("begin_pending_attempt")
        self.assertIsNotNone(self.db.execute(reserve,(-200,7,mid)).fetchone())
        self.assertIsNone(self.db.execute(reserve,(-200,7,mid)).fetchone())
        cancel=query_for("cancel_pending")
        self.assertIsNotNone(self.db.execute(cancel,(99,"admin_command",200,-200,7)).fetchone())
        self.assertIsNone(self.db.execute(query_for("claim_due"),(-200,7,mid)).fetchone())
        self.assertEqual(self.state(-100,7)[1],"pending")
        self.assertIsNotNone(self.db.execute(reserve,(-100,7,other)).fetchone())
        self.assertIsNotNone(self.db.execute(query_for("claim_due"),(-100,7,other)).fetchone())
        self.assertIsNone(self.db.execute(query_for("claim_due"),(-100,7,other)).fetchone())
        self.db.execute("INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(-200,7,?,200,'spam',1,'warned')", (mid+1,))
        self.db.execute(query_for("create_pending_after_warning_check"),(-200,7,mid+1,200,200,None,None))
        self.assertEqual(self.state(-200,7),(mid+1,"pending",0,0))
        self.assertIsNone(self.db.execute(reserve,(-200,7,mid)).fetchone())
        self.assertIsNotNone(self.db.execute(reserve,(-200,7,mid+1)).fetchone())

    def test_timeout_cleanup_even_with_backoff(self):
        mid=self.insert(-200,7)
        self.tg.fail_lookup.add((-200,7))
        for clock,attempts,next_time in [(200,1,260),(260,2,380),(380,3,620),(620,4,860)]:
            self.time=clock
            self.run_tick()
            self.assertEqual(self.state(-200,7),(mid,"pending",attempts,next_time))
        self.time=780
        self.db.execute(query_for("expire_overdue_pending"))
        self.assertEqual(self.state(-200,7)[1],"pending")
        self.time=781
        self.db.execute(query_for("expire_overdue_pending"))
        self.assertEqual(self.state(-200,7)[1],"canceled")
        self.assertEqual(self.db.execute(
            "SELECT cancellation_reason FROM pending_bans WHERE chat_id=-200 AND user_id=7"
        ).fetchone()[0],"timer_too_late")
        self.assertEqual(self.run_tick(),[])

    def test_processing_reset_does_not_resurrect_failed_telegram_ban(self):
        chat, user = -200, 7
        mid = self.insert(chat, user)
        self.db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(?,?,?,100,'spam',0.99,'muted_pending')", (chat,user,mid)
        )
        self.assertIsNotNone(self.db.execute(query_for("claim_due"),(chat,user,mid)).fetchone())
        # Warning receipt commits while the simulated Telegram API call is in flight.
        self.db.execute(query_for("set_warning_count"),(801,chat,user,0,11,self.time))
        self.assertEqual(self.state(chat,user)[1],"processing")
        self.db.execute(query_for("retry_pending"),(chat,user,mid))
        self.assertEqual(self.state(chat,user)[1],"canceled")
        self.assertEqual(self.db.execute(
            "SELECT cancellation_reason FROM pending_bans WHERE chat_id=? AND user_id=?",
            (chat,user)).fetchone()[0],"warning_count_adjusted")
        self.assertIsNone(self.db.execute(query_for("claim_due"),(chat,user,mid)).fetchone())

    def test_retry_before_reset_then_receipt_cancels_pending(self):
        chat,user=-200,7
        mid=self.insert(chat,user)
        self.db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(?,?,?,100,'spam',0.99,'muted_pending')",(chat,user,mid))
        self.db.execute(query_for("claim_due"),(chat,user,mid))
        self.db.execute(query_for("retry_pending"),(chat,user,mid))
        self.assertEqual(self.state(chat,user)[1],"pending")
        self.db.execute(query_for("set_warning_count"),(802,chat,user,0,11,self.time))
        self.assertEqual(self.state(chat,user)[1],"canceled")
        self.assertIsNone(self.db.execute(query_for("claim_due"),(chat,user,mid)).fetchone())

    def test_retry_not_fenced_without_adjustment_and_group_isolation(self):
        mid=self.insert(-200,7)
        other=self.insert(-100,7)
        for chat,message in [(-200,mid),(-100,other)]:
            self.db.execute(
                "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
                "VALUES(?,?,?,100,'spam',0.99,'muted_pending')",(chat,7,message))
            self.db.execute(query_for("claim_due"),(chat,7,message))
        self.db.execute(query_for("set_warning_count"),(803,-200,7,0,11,self.time))
        self.db.execute(query_for("retry_pending"),(-200,7,mid))
        self.db.execute(query_for("retry_pending"),(-100,7,other))
        self.assertEqual(self.state(-200,7)[1],"canceled")
        self.assertEqual(self.state(-100,7)[1],"pending")

    def test_cancel_after_claim_is_not_reported_as_pending_cancel(self):
        mid=self.insert(-200,7)
        self.db.execute(query_for("claim_due"),(-200,7,mid))
        self.assertIsNone(self.db.execute(query_for("cancel_pending"),(11,"admin_command",200,-200,7)).fetchone())
        row=self.db.execute(query_for("ban_case_status"),(-200,7)).fetchone()
        self.assertEqual(row,("processing",))

    def test_cancel_does_not_call_telegram_permission_api(self):
        src=(ROOT/"src/moderation.rs").read_text()
        cancel=src.split('"/cancel" => {',1)[1].split('"/pending" => {',1)[0]
        self.assertIn("store::cancel_pending(",cancel)
        self.assertNotIn("tg::unmute(",cancel)
        self.assertNotIn("tg::get_member(",cancel)
        self.assertNotIn("tg::api(",cancel)
        tg=(ROOT/"src/tg.rs").read_text()
        self.assertNotIn("pub async fn unmute(",tg)

    def test_rust_order_and_boundaries(self):
        src=(ROOT/"src/moderation.rs").read_text()
        fn=src.split("pub async fn finalize_pending(",1)[1].split("pub async fn process_pending_tick",1)[0]
        self.assertLess(fn.index("store::begin_pending_attempt("),
                        fn.index("let member=tg::get_member"))
        self.assertLess(fn.index("let member=tg::get_member"),
                        fn.index("store::claim_due("))
        self.assertIn("saturating_sub(case.due_at)>600",fn)
        self.assertIn("store::expire_overdue_pending(env).await?",src)
        store=(ROOT/"src/store.rs").read_text()
        self.assertIn("ORDER BY next_attempt_at,due_at,chat_id,user_id LIMIT 8",store)


if __name__=="__main__":
    unittest.main()
