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
        self.db.executescript((ROOT / 'migrations/0001_init.sql').read_text())

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

    def test_clef_is_real_binding_and_multimodal(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        self.assertIn('env.ai("AI")?.run("@cf/cloudflare/clef-omni"',source)
        self.assertIn('"images": input_media.images',source)
        self.assertIn('"audio": input_media.audio',source)
        self.assertIn('"videos": input_media.videos',source)
        self.assertIn('"group_description": rules',source)

    def test_auto_onboard_and_silent_mode(self):
        src=(ROOT/'src/moderation.rs').read_text()
        tg=(ROOT/'src/tg.rs').read_text()
        util=(ROOT/'src/util.rs').read_text()
        self.assertIn('update.get("my_chat_member")',src)
        self.assertIn('ensure_group_enrolled(env,&msg["chat"])',src)
        self.assertIn('member_capabilities(&event["new_chat_member"]',src)
        self.assertIn('if chat_id <= 0 { return Ok(()); }',tg)
        self.assertNotIn('GROUP_CHAT_ID',util)
        self.assertIn('ADMIN_DM_CHAT_ID',tg)
        conf=json.loads((ROOT/'wrangler.jsonc').read_text())
        self.assertNotIn('GROUP_CHAT_ID',conf['vars'])
        self.db.executescript((ROOT/'migrations/0002_pending_bans.sql').read_text())
        self.db.executescript((ROOT/'migrations/0003_managed_chats.sql').read_text())
        for chat_id,status in [(-1001,'active'),(-1002,'delete_only'),(-1003,'paused')]:
            self.db.execute('INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES(?,?,?,?,100)',(chat_id,'A','supergroup',status))
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM managed_chats WHERE status IN ('active','delete_only')").fetchone()[0],2)
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES (-999,'A','group','bad',100)")

    def test_admin_exempt_from_ban(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        self.assertIn('let ban_user = !is_admin && store::can_auto_ban(env,chat_id).await?',source)
        self.assertIn('tg::user_is_admin(env, chat_id, user_id).await?',source)
        self.assertIn('IMMEDIATE_BAN", false',source)

    def test_review_fail_open_and_no_public_error_spam(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        self.assertIn('"no_group_description"',source)
        self.assertIn('"review_error"',source)
        self.assertIn('mode == Mode::Live && !config(env, "ADMIN_DM_CHAT_ID").is_empty()',source)
        self.assertIn('"too_old_to_delete"',source)
        self.assertIn('store::has_strike(env, chat_id, msg_id).await?',source)

    def test_telegram_commands_present(self):
        source=(ROOT / 'src/moderation.rs').read_text()
        for cmd in ['/scan','/scan_id','/scan_all','/scan_status','/scan_stop','/rules']:
            self.assertIn(cmd,source)

    def test_auth_protected_webhook_and_queue(self):
        source=(ROOT / 'src/lib.rs').read_text()
        self.assertIn('X-Telegram-Bot-Api-Secret-Token',source)
        self.assertIn('safe_equal(&actual, &expected)',source)
        self.assertIn('env.queue("MODERATION_QUEUE")?.send(value).await?',source)
        self.assertIn('message.retry()',source)

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
