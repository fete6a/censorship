"""Exercise the production deploy script with external CLIs replaced at the boundary."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SHA = 'a' * 40


class CIDeployTests(unittest.TestCase):
    def deploy(self, *, stale=False, stale_late=False, missing_secret=False,
               corrupt=False, schema_error=False, non_main=False, health_error=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'build').mkdir()
            (root/'bin').mkdir()
            files = {'build/index.js':b'export default {};', 'build/index_bg.wasm':b'wasm-fixture'}
            for name,data in files.items(): (root/name).write_bytes(data)
            (root/'build/SHA256SUMS').write_text(''.join(
                f'{hashlib.sha256(data).hexdigest()}  {name}\n' for name,data in files.items()))
            if corrupt: (root/'build/index_bg.wasm').write_bytes(b'changed')
            config={'name':'test-worker','main':'build/index.js','build':{'command':'worker-build --release'},
                    'vars':{'DRY_RUN':'true'},'d1_databases':[{'binding':'DB','database_id':'test-db'}]}
            (root/'wrangler.jsonc').write_text(json.dumps(config))
            stub = '''#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
p=pathlib.Path(os.environ['STUB_CALLS'])
calls=[json.loads(s) for s in p.read_text().splitlines()] if p.exists() else []
entry={'tool':name,'args':sys.argv[1:]}
if name=='npx' and 'deploy' in sys.argv:
    if '--config' in sys.argv: entry['config']=json.loads(pathlib.Path(sys.argv[sys.argv.index('--config')+1]).read_text())
with p.open('a') as f: f.write(json.dumps(entry)+'\\n')
if name=='gh':
    count=sum(c['tool']=='gh' for c in calls)
    print('b'*40 if os.environ.get('STALE')=='1' or (os.environ.get('STALE_LATE')=='1' and count>0) else os.environ['GITHUB_SHA'])
if name=='npx' and 'd1' in sys.argv and os.environ.get('SCHEMA_ERROR')=='1': sys.exit(9)
if name=='curl' and os.environ.get('HEALTH_ERROR')=='1': sys.exit(22)
'''
            for tool in ('npm','npx','gh','curl'):
                p=root/'bin'/tool;p.write_text(stub);p.chmod(0o755)
            env={**os.environ,'PATH':str(root/'bin')+os.pathsep+os.environ['PATH'],
                 'CLOUDFLARE_API_TOKEN':'' if missing_secret else 'test-token',
                 'CLOUDFLARE_ACCOUNT_ID':'test-account','GH_TOKEN':'test-github-token',
                 'GITHUB_SHA':SHA,'GITHUB_REF':'refs/heads/other' if non_main else 'refs/heads/main',
                 'GITHUB_REPOSITORY':'example/repo','GITHUB_STEP_SUMMARY':str(root/'summary'),
                 'STUB_CALLS':str(root/'calls'),'STALE':str(int(stale)),'STALE_LATE':str(int(stale_late)),
                 'SCHEMA_ERROR':str(int(schema_error)),'HEALTH_ERROR':str(int(health_error))}
            script=ROOT/'scripts/deploy-ci.sh'
            command=['bash',str(script)]
            result=subprocess.run(command,cwd=root,env=env,capture_output=True,text=True)
            calls=[json.loads(s) for s in (root/'calls').read_text().splitlines()] if (root/'calls').exists() else []
            summary=(root/'summary').read_text() if (root/'summary').exists() else ''
            return result,calls,summary

    def test_success_publishes_verified_artifact_without_custom_build_or_bundling(self):
        result,calls,summary=self.deploy()
        self.assertEqual(result.returncode,0,result.stderr)
        deploy=next(c for c in calls if c['tool']=='npx' and 'deploy' in c['args'])
        self.assertIn('--no-bundle',deploy['args'])
        self.assertIn('--keep-vars',deploy['args'])
        self.assertNotIn('build',deploy['config'])
        self.assertEqual(deploy['config']['main'],'build/index.js')
        self.assertEqual(deploy['config']['d1_databases'][0]['database_id'],'test-db')
        self.assertEqual(deploy['args'][deploy['args'].index('--message')+1],SHA)
        self.assertTrue(any(c['tool']=='curl' for c in calls))
        self.assertIn(SHA,summary)

    def test_superseded_build_is_skipped_without_deployment(self):
        result,calls,summary=self.deploy(stale=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertFalse(any(c['tool']=='npx' and 'deploy' in c['args'] for c in calls))
        self.assertIn('Skipped',summary)

    def test_main_moving_during_preparation_skips_upload(self):
        result,calls,summary=self.deploy(stale_late=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertFalse(any(c['tool']=='npx' and 'deploy' in c['args'] for c in calls))
        self.assertIn('Skipped',summary)

    def test_corrupt_artifact_cannot_be_deployed(self):
        result,calls,_=self.deploy(corrupt=True)
        self.assertNotEqual(result.returncode,0)
        self.assertFalse(any(c['tool']=='npx' and 'deploy' in c['args'] for c in calls))

    def test_schema_error_blocks_publish(self):
        result,calls,_=self.deploy(schema_error=True)
        self.assertNotEqual(result.returncode,0)
        self.assertFalse(any(c['tool']=='npx' and 'deploy' in c['args'] for c in calls))

    def test_missing_credentials_block_all_external_calls(self):
        result,calls,_=self.deploy(missing_secret=True)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(calls,[])

    def test_non_main_ref_cannot_deploy(self):
        result,calls,_=self.deploy(non_main=True)
        self.assertNotEqual(result.returncode,0)
        self.assertFalse(any(c['tool']=='npx' and 'deploy' in c['args'] for c in calls))

    def test_failed_health_check_reports_failed_release(self):
        result,_,_=self.deploy(health_error=True)
        self.assertNotEqual(result.returncode,0)


if __name__=='__main__': unittest.main()
