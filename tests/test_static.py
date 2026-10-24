"""Offline structural and SQLite behavioral smoke checks; not a Rust compiler test."""
from pathlib import Path
import json
import sqlite3
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]

class SmokeTests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.executescript((ROOT / 'schema/current.sql').read_text())

    def tearDown(self):
        self.db.close()

    def test_configuration_schema(self):
        conf = json.loads((ROOT / 'wrangler.jsonc').read_text())
        self.assertEqual(conf['ai']['binding'], 'AI')
        self.assertEqual(conf['vars']['DRY_RUN'], 'true')
        self.assertEqual(conf['compatibility_date'], '2026-10-23')
        self.assertEqual(conf['queues']['producers'][0]['binding'], 'MODERATION_QUEUE')
        self.assertEqual(conf['d1_databases'][0]['binding'], 'DB')
        self.assertIn('nodejs_compat', conf['compatibility_flags'])

    def test_archive_insert_and_overwrite(self):
        normalized = json.dumps({'chat': {'id': -100}, 'message_id': 15, 'text':'test'})
        self.db.execute('''INSERT INTO archived_messages
        (chat_id,message_id,user_id,sent_at,updated_at,payload,last_status)
        VALUES(?,?,?,?,?,?,'pending')''',(-100,15,2,100,100,normalized))
        self.db.execute('''INSERT INTO archived_messages
        (chat_id,message_id,user_id,sent_at,updated_at,payload,last_status)
        VALUES(?,?,?,?,?,?,'pending')
        ON CONFLICT(chat_id,message_id) DO UPDATE SET
        payload=excluded.payload,updated_at=excluded.updated_at,last_status='pending' ''',(-100,15,2,100,101,normalized))
        row = self.db.execute('SELECT COUNT(*),MAX(updated_at) FROM archived_messages').fetchone()
        self.assertEqual(row,(1,101))

    def test_strikes_unique_and_7day_count(self):
        for user,msg,ts in [(8,1,100),(8,2,120),(9,3,130),(8,4,150)]:
            self.db.execute('''INSERT OR IGNORE INTO violations
            (chat_id,user_id,message_id,created_at,category,probability,severity,action)
            VALUES (-100,?,?,?,'spam',0.95,0,'deleted')''',(user,msg,ts))
        self.db.execute('''INSERT OR IGNORE INTO violations
        (chat_id,user_id,message_id,created_at,category,probability,severity,action)
        VALUES (-100,8,2,120,'spam',0.95,0,'deleted')''')
        count=self.db.execute('SELECT COUNT(*) FROM violations WHERE chat_id=? AND user_id=? AND created_at>=?',(-100,8,100)).fetchone()[0]
        self.assertEqual(count,3)

    def test_scan_progress_and_lookups(self):
        for i in [1,10,15,19]:
            self.db.execute('''INSERT INTO archived_messages(chat_id,message_id,sent_at,updated_at,payload,last_status)
            VALUES(-100,?,?,?,'{}','pending')''',(i,100,100))
        max_id=self.db.execute('SELECT MAX(message_id) FROM archived_messages WHERE chat_id=-100').fetchone()[0]
        self.db.execute('''INSERT INTO scan_jobs (chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,started_at,updated_at)
        VALUES(-100,0,?,0,0,0,'running',100,100)''',(max_id,))
        row=self.db.execute('''SELECT message_id FROM archived_messages WHERE chat_id=-100
        AND message_id>? AND message_id<=? ORDER BY message_id LIMIT 1''',(10,max_id)).fetchone()
        self.assertEqual(row,(15,))
        self.db.execute('''UPDATE scan_jobs SET cursor_message_id=?,scanned=scanned+1,
        flagged=flagged+1 WHERE chat_id=-100 AND cursor_message_id=0''',(15,))
        self.assertEqual(self.db.execute('SELECT scanned,flagged FROM scan_jobs').fetchone(),(1,1))

    def test_atomic_scan_start_and_cross_group_conflicts(self):
        src=(ROOT / 'src/store.rs').read_text()
        self.assertIn("WHERE scan_jobs.status!='running'",src)
        self.assertIn('RETURNING max_message_id AS max_id',src)
        sql="""INSERT INTO scan_jobs
        (chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,started_at,updated_at)
        VALUES (?,0,?,0,0,?,'running',?,?)
        ON CONFLICT(chat_id) DO UPDATE SET
        cursor_message_id=0,max_message_id=excluded.max_message_id,
        scanned=0,flagged=0,enforce=excluded.enforce,
        status='running',started_at=excluded.started_at,updated_at=excluded.updated_at
        WHERE scan_jobs.status!='running'
        RETURNING max_message_id AS max_id"""
        def start(chat,enforce):
            return self.db.execute(sql,(chat,10,enforce,100,100)).fetchone()
        self.assertEqual(start(-100,0),(10,))
        self.db.execute('UPDATE scan_jobs SET cursor_message_id=7,scanned=3 WHERE chat_id=-100')
        self.assertIsNone(start(-100,1))
        self.assertEqual(self.db.execute('SELECT cursor_message_id,scanned,enforce FROM scan_jobs WHERE chat_id=-100').fetchone(),(7,3,0))
        self.assertEqual(start(-200,1),(10,))
        self.assertEqual(self.db.execute('SELECT enforce FROM scan_jobs WHERE chat_id=-200').fetchone(),(1,))
        self.db.execute("UPDATE scan_jobs SET status='stopped' WHERE chat_id=-100")
        self.assertEqual(start(-100,1),(10,))
        self.assertEqual(self.db.execute('SELECT cursor_message_id,scanned,enforce FROM scan_jobs WHERE chat_id=-100').fetchone(),(0,0,1))

    def test_scan_defaults_match_help_and_bilingual_readmes(self):
        src=(ROOT/'src/moderation.rs').read_text()
        en=(ROOT/'README.md').read_text()
        cn=(ROOT/'README_CN.md').read_text()
        self.assertIn('[] | ["enforce"] => true',src)
        self.assertIn('["audit"] => false',src)
        self.assertIn('/scan_all [audit|enforce]',src)
        self.assertIn('enforcement by default',en)
        self.assertIn('默认请求执行处罚',cn)

    def test_auto_onboard_and_command_replies(self):
        src=(ROOT/'src/moderation.rs').read_text()
        tg=(ROOT/'src/tg.rs').read_text()
        util=(ROOT/'src/util.rs').read_text()
        self.assertIn('update.get("my_chat_member")',src)
        self.assertIn('ensure_group_enrolled(env,&msg["chat"])',src)
        self.assertIn('member_capabilities(&event["new_chat_member"]',src)
        self.assertIn('pub async fn reply_command(',tg)
        self.assertIn('allow_sending_without_reply',tg)
        self.assertNotIn('GROUP_CHAT_ID',util)
        conf=json.loads((ROOT/'wrangler.jsonc').read_text())
        self.assertNotIn('GROUP_CHAT_ID',conf['vars'])
        for chat_id,status in [(-1001,'active'),(-1002,'delete_only'),(-1003,'paused')]:
            self.db.execute('INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES(?,?,?,?,100)',(chat_id,'A','supergroup',status))
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM managed_chats WHERE status IN ('active','delete_only')").fetchone()[0],2)
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES (-999,'A','group','bad',100)")

    def test_admin_exempt_from_ban(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        self.assertIn('let can_ban = !is_admin && store::can_auto_ban(env,chat_id).await?',source)
        self.assertIn('tg::user_is_admin(env, chat_id, user_id).await?',source)
        self.assertIn('IMMEDIATE_BAN", false',source)

    def test_review_fail_open_and_no_public_error_spam(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        self.assertIn('"no_group_description"',source)
        self.assertIn('"review_error"',source)
        self.assertIn('"too_old_to_delete"',source)
        self.assertIn('store::has_strike(env, chat_id, msg_id).await?',source)

    def test_telegram_commands_present(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        for cmd in ['/scan','/scan_id','/scan_all','/scan_status','/scan_stop','/rules']:
            self.assertIn(cmd,source)

    def test_native_audio_transcode_fallback(self):
        source=(ROOT / 'src/media.rs').read_text()
        self.assertIn('TRANSCODER_URL',source)
        self.assertIn('data:{mime};base64',source)
        self.assertIn('audio/mpeg',source)
        self.assertIn('video/mp4',source)
        self.assertIn('MAX_VIDEO',source)
        service=(ROOT / 'transcoder/src/main.rs').read_text()
        self.assertIn('ffmpeg',service)
        self.assertIn('TRANSCODER_TOKEN',service)

    def test_offline_telegram_desktop_importer(self):
        source=(ROOT / 'tools/history-import/src/main.rs').read_text()
        self.assertIn('"date_unixtime"',source)
        self.assertIn('"from_id"',source)
        self.assertIn('INSERT OR IGNORE INTO archived_messages',source)
        self.assertIn('escaped_sql',source)

if __name__ == '__main__':
    unittest.main(verbosity=2)
