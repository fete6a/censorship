"""Deployment guards for a deliberately current-schema-only application."""
from pathlib import Path
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]


class DeploymentReviewTests(unittest.TestCase):
    def test_manual_deployment_checks_schema_before_publish_without_migration(self):
        workflow = (ROOT / '.github/workflows/build.yml').read_text()
        self.assertIn("if: ${{ github.event_name == 'workflow_dispatch' && inputs.deploy == true }}", workflow)
        self.assertIn('set -euo pipefail', workflow)
        queries = (
            'SELECT next_attempt_at,attempt_count FROM pending_bans LIMIT 0',
            'SELECT logs_enabled,remove_threshold FROM group_settings LIMIT 0',
            'SELECT chat_type,status FROM managed_chats LIMIT 0',
            'SELECT last_status,last_category FROM archived_messages LIMIT 0',
        )
        for q in queries:
            self.assertIn(q, workflow)
            self.assertLess(workflow.index(q), workflow.index('npx wrangler deploy'))
        self.assertNotIn('d1 migrations apply', workflow)
        self.assertFalse((ROOT / 'migrations').exists(),
                         'Historical migration chain should not ship')
        self.assertFalse((ROOT / '.github/workflows/deploy-once.yml').exists(),
                         'One-off dispatcher must be removed')

    def test_deployment_and_builds_use_independent_concurrency(self):
        workflow = (ROOT / '.github/workflows/build.yml').read_text()
        self.assertIn("&& 'deploy' || 'build'", workflow)
        self.assertIn(
            "cancel-in-progress: ${{ github.event_name != 'workflow_dispatch' || inputs.deploy != true }}",
            workflow
        )

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
