"""Deployment guards for a deliberately current-schema-only application."""
from pathlib import Path
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]


class DeploymentReviewTests(unittest.TestCase):
    def test_ci_builds_artifacts_without_production_access(self):
        workflow = (ROOT / '.github/workflows/build.yml').read_text()
        self.assertIn('actions/upload-artifact@v4', workflow)
        self.assertIn('sha256sum build/index.js build/index_bg.wasm', workflow)
        self.assertIn('node --experimental-vm-modules tests/worker_runtime.mjs', workflow)
        for removed in ('  deploy:', 'CLOUDFLARE_API_TOKEN', 'CLOUDFLARE_ACCOUNT_ID',
                        'wrangler deploy', 'd1 execute', 'actions/download-artifact'):
            self.assertNotIn(removed, workflow)
        self.assertFalse((ROOT / 'scripts/deploy-ci.sh').exists())
        self.assertFalse((ROOT / '.github/workflows/deploy-once.yml').exists())
        self.assertFalse((ROOT / 'migrations').exists())

    def test_current_schema_can_bootstrap_one_empty_database(self):
        script = (ROOT / 'schema/current.sql').read_text()
        self.assertIn('Use ONLY on a newly created, empty D1 database', script)
        self.assertNotIn('ALTER TABLE', script.upper())
        con = sqlite3.connect(':memory:')
        try:
            con.executescript(script)
            for table, expected in {
                'pending_bans': {'chat_id', 'user_id', 'message_id', 'status', 'next_attempt_at', 'attempt_count'},
                'group_settings': {'chat_id', 'logs_enabled', 'remove_threshold'},
                'managed_chats': {'chat_id', 'status', 'chat_type'},
                'archived_messages': {'chat_id', 'message_id', 'last_category'},
            }.items():
                columns = {x[1] for x in con.execute(f'PRAGMA table_info({table})')}
                self.assertTrue(expected <= columns, (table, columns))
            index = {x[0] for x in con.execute("SELECT name FROM sqlite_master WHERE type='index'")}
            self.assertIn('idx_pending_bans_ready', index)
            self.assertNotIn('d1_migrations', {x[0] for x in con.execute("SELECT name FROM sqlite_master WHERE type='table'")})
            with self.assertRaises(sqlite3.OperationalError):
                con.executescript(script)
        finally:
            con.close()


if __name__ == '__main__':
    unittest.main()

