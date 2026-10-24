# Manual production D1 release gate

GitHub Actions builds and tests only. Green CI never proves that production D1 was migrated.

1. Confirm production Cloudflare account and D1 DB binding against wrangler.jsonc. Back up D1 and record currently deployed Worker version. Do not apply schema/current.sql on existing data.
2. On an authenticated operator machine, run: python3 tools/check_d1_schema.py --remote --pre-migration
   Exit 0 = required warning objects and scan-fencing columns ready; exit 3 = known additive migration needed; exit 2 = incompatible or ambiguous schema, STOP.
3. Only after exit 3, run the additive idempotent schema/ensure_warning_adjustments.sql manually via authorized Cloudflare connector or: npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql
   Also apply exactly the missing scan_jobs ALTER statements printed by the preflight. These come from schema/ensure_scan_fencing.sql. The full scan migration is one-time, not idempotent: do not repeat existing columns. A partially applied scan migration is resumable using only the remaining printed statements. Historical jobs and messages are preserved.
4. Verify: python3 tools/check_d1_schema.py --remote ; exit 0 is MANDATORY. Recheck immediately before Worker upload. An existing wrong-shape table or trigger is not repaired by CREATE IF NOT EXISTS.
5. Verify the exact main commit, green Rust/SQLite/Wasm tests, and downloaded artifact SHA-256. Manually upload unchanged modules with Cloudflare MCP. Keep bindings, Secrets, Queues, Cron, and webhook. Do not enable production deployment in GitHub Actions.
6. Verify active version, /healthz, Queue errors and a harmless dry-run review. If rollout fails, return to previous Worker artifact, not a destructive database rollback.

The production query reads sqlite_master and pragma_table_info only, never message contents or production rows. It verifies tables, column metadata, primary keys, indexes, warning constraints and the exact required receipt-trigger definition. Canonical trigger behavior is also tested on disposable in-memory SQLite dummy rows, not against production. Extra D1 migration-ledger tables remain intact. Any missing core object or incompatible version blocks release; operator investigation is required.

Optional Cloudflare connector route: run the exact INSPECTION_SQL constant from tools/check_d1_schema.py against the same DB, save the single successful statement response in Wrangler-compatible JSON, and run: python3 tools/check_d1_schema.py --from-json response.json

## Recovery from partial execution

Old Workers could delete a Telegram message and record a strike, then fail at warning count when warning_adjustments was missing. Queue retry can hit the duplicate-strike guard, skip outstanding penalties and notices, and mark previously_deleted. Failure before strike write can lead to a repeated Telegram deletion attempt. Inspect archive, violation and pending-ban records; never replay punishment blindly.

The new Worker checks the warning-count SQL before irreversible Telegram deletion for attributed senders. It counts again after recording the genuine strike to account for concurrent manual adjustments. A failure after deletion can still be partial and requires investigation. Never substitute an old-schema warning count: that silently changes escalation decisions. Cleanup ignores ONLY the exact missing warning_adjustments-table error and propagates other D1 failures.

Run python3 -m unittest discover -s tests -v and the existing Rust/Wasm CI. tests/fixtures/schema_before_warning_adjustments.sql reproduces the actual pre-PR11 schema in ephemeral SQLite, not production D1. Tests exercise preservation, idempotence, wrong-shape tables, partial SQL failures and mismatched old versions.

## PR #11 / #13 migration compatibility

| Existing D1 structure | `--pre-migration` | Normal check | Operator action |
| --- | --- | --- | --- |
| Before PR #11: no warning tables/trigger | 3 | 2 | Apply additive migration, recheck |
| After PR #11: adjustment table only | 3 | 2 | Apply additive migration, recheck |
| Valid adjustment + receipt tables and valid trigger | 0 | 0 | Eligible after all other release checks |
| Adjustment table present, receipt table and trigger absent | 3 | 2 | Apply additive migration, recheck |
| Receipt table present, trigger missing | 2 | 2 | STOP: reconcile potentially unapplied receipts |
| Receipt table present, adjustments absent | 2 | 2 | STOP: reconcile manually |
| Wrong trigger definition or unexpected warning-related trigger | 2 | 2 | STOP: investigate trigger effects |
| Wrong required table shape or core schema | 2 | 2 | STOP: investigate structural mismatch |

If the receipt table exists without the trigger, adding the missing trigger does not apply historical receipt rows. Do not automatically classify this state as migratable or rerun Telegram commands to repair it. The read-only preflight intentionally cannot prove whether old receipt rows were processed.

The combined release also requires scan_jobs.generation, lease_id and lease_until. In pre-migration mode only missing known scan columns are migratable; existing wrong column types, defaults or constraints still block release. Normal mode requires every column. A ready warning schema with missing scan columns is not deployment-ready.

## External-operation boundary

`/cancel` never changes Telegram permissions. Warning adjustments cancel pending database work and fence retries; neither command can recall a ban already sent to Telegram. Historical scans fence writes by generation and Cron lease. After a stop, a successful deletion already in flight is recorded, but new mute/ban scheduling is suppressed. A mute already in flight may still complete; an obsolete scan cannot persist a new pending ban afterwards. Telegram offers no conditional transaction spanning D1, member permissions and administrator role changes, so last-moment human edits still require operational review.

Violation insertion allocates an ID above both the remaining history and the user's warning baseline, so retention cannot hide a new strike by reusing an old SQLite rowid. Do not rebuild or VACUUM the violations table independently of its baselines: the implicit rowids are part of the application's warning ordering.
