"""Deployment review regression checks."""
from pathlib import Path
import unittest
ROOT=Path(__file__).resolve().parents[1]
class DeploymentReviewTests(unittest.TestCase):
    def test_manual_deployment_migrates_before_worker(self):
        src=(ROOT/'.github/workflows/build.yml').read_text()
        self.assertIn("if: ${{ github.event_name == 'workflow_dispatch' && inputs.deploy == true }}",src)
        self.assertLess(src.index('npx wrangler d1 migrations apply DB --remote'),src.index('npx wrangler deploy'))
        self.assertIn('set -euo pipefail',src)
    def test_deployment_is_not_cancelled_by_push(self):
        src=(ROOT/'.github/workflows/build.yml').read_text()
        self.assertIn("&& 'deploy' || 'build'",src)
        self.assertIn("cancel-in-progress: ${{ github.event_name != 'workflow_dispatch' || inputs.deploy != true }}",src)
