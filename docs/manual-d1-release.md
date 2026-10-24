# Manual production D1 release gate

GitHub Actions builds and tests only. Green CI never proves that production D1 was migrated.

1. Confirm production Cloudflare account and D1 DB binding against wrangler.jsonc. Back up D1 and record currently deployed Worker version. Do not apply schema/current.sql on existing data.
2. On an authenticated operator machine, run: python3 tools/check_d1_schema.py --remote --pre-migration
   Exit 0 = ready; exit 3 = migration required (only warning_adjustments absent); exit 2 = unknown or incompatible schema, STOP.
3. Only after exit 3, run the additive idempotent schema/ensure_warning_adjustments.sql manually via authorized Cloudflare connector or: npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql
4. Verify: python3 tools/check_d1_schema.py --remote ; exit 0 is MANDATORY. Recheck immediately before Worker upload. An existing wrong-shape table is not repaired by CREATE TABLE IF NOT EXISTS.
5. Verify the exact main commit, green Rust/SQLite/Wasm tests, and downloaded artifact SHA-256. Manually upload unchanged modules with Cloudflare MCP. Keep bindings, Secrets, Queues, Cron, and webhook. Do not enable production deployment in GitHub Actions.
6. Verify active version, /healthz, Queue errors and a harmless dry-run review. If rollout fails, return to previous Worker artifact, not a destructive database rollback.

The check reads sqlite_master and pragma_table_info only, never message contents or production rows. It verifies required tables, column type, nullable flags, defaults, primary keys, index DDL and the new warning adjustment table CHECK constraints. Extra D1 migration-ledger tables remain intact. Any missing core object or incompatible version blocks release; operator investigation is required.

Optional Cloudflare connector route: run the exact INSPECTION_SQL constant from tools/check_d1_schema.py against the same DB, save the single successful statement response in Wrangler-compatible JSON, and run: python3 tools/check_d1_schema.py --from-json response.json

## Recovery from partial execution

Old Workers could delete a Telegram message and record a strike, then fail at warning count when warning_adjustments was missing. Queue retry can hit the duplicate-strike guard, skip outstanding penalties and notices, and mark previously_deleted. Failure before strike write can lead to a repeated Telegram deletion attempt. Inspect archive, violation and pending-ban records; never replay punishment blindly.

The new Worker checks the warning-count SQL before irreversible Telegram deletion for attributed senders. It counts again after recording the genuine strike to account for concurrent manual adjustments. A failure after deletion can still be partial and requires investigation. Never substitute an old-schema warning count: that silently changes escalation decisions. Cleanup ignores ONLY the exact missing warning_adjustments-table error and propagates other D1 failures.

Run python3 -m unittest discover -s tests -v and the existing Rust/Wasm CI. tests/fixtures/schema_before_warning_adjustments.sql reproduces the actual pre-PR11 schema in ephemeral SQLite, not production D1. Tests exercise preservation, idempotence, wrong-shape tables, partial SQL failures and mismatched old versions.
