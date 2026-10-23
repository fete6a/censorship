#!/bin/sh
# Builds Rust/Wasm with Cargo profile.release fat LTO.
# Set Cloudflare credentials and configure D1+Queues before --deploy.
set -eu
command -v cargo >/dev/null 2>&1 || { echo 'cargo is required' >&2; exit 1; }
command -v worker-build >/dev/null 2>&1 || { echo 'worker-build is required' >&2; exit 1; }
command -v rustup >/dev/null 2>&1 || { echo 'rustup is required' >&2; exit 1; }
if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
  echo 'Install rustup target wasm32-unknown-unknown' >&2
  exit 1
fi
if [ "${1:-}" = '--deploy' ]; then
  command -v npx >/dev/null 2>&1 || { echo 'npx is required for deployment' >&2; exit 1; }
  if grep -q REPLACE_WITH_D1_UUID wrangler.jsonc; then
    echo 'Configure Cloudflare D1 database_id before deployment' >&2
    exit 1
  fi
  npx wrangler deploy
else
  worker-build --release
fi
