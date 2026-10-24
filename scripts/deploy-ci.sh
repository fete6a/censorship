#!/usr/bin/env bash
# Upload only the current main commit's verified CI artifact. Never compile here.
set -euo pipefail

if [[ -z "${CLOUDFLARE_API_TOKEN:-}" || -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]]; then
  echo 'Configure CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID in Actions secrets' >&2
  exit 1
fi
if [[ "${GITHUB_REF:-}" != 'refs/heads/main' ]]; then
  echo 'Production deployment requires the main branch' >&2
  exit 1
fi
: "${GITHUB_SHA:?}" "${GITHUB_REPOSITORY:?}" "${GITHUB_STEP_SUMMARY:?}"

current_head=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '.object.sha')
if [[ "$current_head" != "$GITHUB_SHA" ]]; then
  printf 'Skipped superseded commit %s\n' "$GITHUB_SHA" >> "$GITHUB_STEP_SUMMARY"
  exit 0
fi
sha256sum --check build/SHA256SUMS

# The CLI does not rebuild Rust or transform the tested JavaScript. Keep local
# developer builds in wrangler.jsonc; use an ephemeral CI config in the same dir.
ci_config=$(mktemp ./wrangler.ci.XXXXXX.json)
trap 'rm -f "$ci_config"' EXIT
python3 - "$ci_config" <<'PY'
import json
from pathlib import Path
import sys
config = json.loads(Path('wrangler.jsonc').read_text())
config.pop('build', None)
Path(sys.argv[1]).write_text(json.dumps(config))
PY
npm install --package-lock=false --no-audit --no-fund
# Read-only checks; current-schema releases never migrate production D1.
npx wrangler d1 execute DB --config "$ci_config" --remote --command "SELECT next_attempt_at,attempt_count FROM pending_bans LIMIT 0"
npx wrangler d1 execute DB --config "$ci_config" --remote --command "SELECT logs_enabled,remove_threshold FROM group_settings LIMIT 0"
npx wrangler d1 execute DB --config "$ci_config" --remote --command "SELECT chat_type,status FROM managed_chats LIMIT 0"
npx wrangler d1 execute DB --config "$ci_config" --remote --command "SELECT last_status,last_category FROM archived_messages LIMIT 0"

# A newer push may have arrived while preparing the deployment.
current_head=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '.object.sha')
if [[ "$current_head" != "$GITHUB_SHA" ]]; then
  printf 'Skipped superseded commit %s\n' "$GITHUB_SHA" >> "$GITHUB_STEP_SUMMARY"
  exit 0
fi
npx wrangler deploy --config "$ci_config" --no-bundle --keep-vars --message "$GITHUB_SHA" --tag "${GITHUB_SHA:0:7}"
sha256sum --check build/SHA256SUMS
curl --fail --silent --show-error --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 --max-time 30 \
  https://telegram-clef-omni-moderator.blocker-sleeve-0w.workers.dev/healthz
printf '\nDeployed verified CI artifact for %s; health check passed.\n' "$GITHUB_SHA" >> "$GITHUB_STEP_SUMMARY"
