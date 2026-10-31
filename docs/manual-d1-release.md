# Manual production D1 release gate

GitHub Actions builds and tests only. Green CI never proves that production D1 was migrated.

1. Confirm production Cloudflare account and D1 DB binding against wrangler.jsonc. Record a D1 Time Travel recovery bookmark and the currently deployed Worker version; a full data export is a separate operation requiring appropriate authorization. Do not apply schema/current.sql on existing data.
2. On an authenticated operator machine, run: cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --remote --pre-migration
   Exit 0 = required warning objects, cleanup-state table, scan-fencing columns and log-filter constraints ready; exit 3 = known additive migration needed; exit 2 = incompatible or ambiguous schema, STOP.
3. Only after exit 3, run the additive idempotent schema/ensure_warning_adjustments.sql manually via authorized Cloudflare connector or: npx wrangler d1 execute DB --remote --file schema/ensure_warning_adjustments.sql
   Also apply `schema/ensure_cleanup_state.sql` if the `cleanup_state` table is missing (additive and idempotent; does not alter existing rows). Also apply exactly the missing ALTER statements printed by the preflight. Scan columns come from schema/ensure_scan_fencing.sql; the group log filter comes from schema/ensure_log_filter.sql. The full scan migration is one-time, not idempotent: do not repeat existing columns. A partially applied scan migration is resumable using only the remaining printed statements. Historical jobs and messages are preserved.
4. Verify: cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --remote ; exit 0 is MANDATORY. Recheck immediately before Worker upload. An existing wrong-shape table or trigger is not repaired by CREATE IF NOT EXISTS.
5. Verify the exact main commit, green Rust/SQLite/Wasm tests, and downloaded artifact SHA-256. Manually upload unchanged modules with Cloudflare MCP. Keep bindings, Secrets, Queues, Cron, and webhook. Do not enable production deployment in GitHub Actions.
6. Verify active version, /healthz, Queue errors and a harmless dry-run review. If rollout fails, return to previous Worker artifact, not a destructive database rollback.

The production batch reads sqlite_master and literal PRAGMA table_info only, never message contents or production rows. It verifies tables, column metadata, primary keys, indexes, the added log-filter column's exact CHECK declaration, warning constraints and the exact required receipt-trigger definition. Canonical trigger behavior is also tested on disposable in-memory SQLite dummy rows, not against production. Extra D1 migration-ledger tables remain intact. Unexpected triggers on privacy-sensitive `group_settings` (as well as warning, ban and cleanup tables) block release. Any missing core object or incompatible version blocks release; operator investigation is required.

Optional Cloudflare connector route: run the read-only batch printed by cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --print-inspection-sql against the same DB, save all successful statement responses in Wrangler-compatible JSON, and run: cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --from-json response.json

## Recovery from partial execution

Old Workers could delete a Telegram message and record a strike, then fail at warning count when warning_adjustments was missing. Queue retry can hit the duplicate-strike guard, skip outstanding penalties and notices, and mark previously_deleted. Failure before strike write can lead to a repeated Telegram deletion attempt. Inspect archive, violation and pending-ban records; never replay punishment blindly.

The new Worker checks the warning-count SQL before irreversible Telegram deletion for attributed senders. It counts again after recording the genuine strike to account for concurrent manual adjustments. A failure after deletion can still be partial and requires investigation. Never substitute an old-schema warning count: that silently changes escalation decisions. Cleanup ignores only the exact missing warning_adjustments or warning_edit_receipts table error and propagates other D1 failures.

Run cargo test --manifest-path tools/maintenance/Cargo.toml and the existing Rust/Wasm CI. tests/fixtures/schema_before_warning_adjustments.sql reproduces the actual pre-PR11 schema in ephemeral SQLite, not production D1. Tests exercise preservation, idempotence, wrong-shape tables, partial SQL failures and mismatched old versions.

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

The release gate also verifies the exact `cleanup_state` table definition. A missing table is an approved additive migration only in `--pre-migration` mode; an incompatible table or trigger is never auto-repaired. The singleton row is created by the first successful Worker claim; do not initialize or inspect production cleanup state in this PR. No Cloudflare deployment or production D1 migration is authorized here.

The combined release also requires scan_jobs.generation, lease_id and lease_until. In pre-migration mode only missing known scan columns are migratable; existing wrong column types, defaults or constraints still block release. Normal mode requires every column. A ready warning schema with missing scan columns is not deployment-ready.

## External-operation boundary

`/cancel` never changes Telegram permissions. Warning adjustments cancel pending database work and fence retries; neither command can recall a ban already sent to Telegram. Historical scans fence writes by generation and Cron lease. After a stop, a successful deletion already in flight is recorded, but new mute/ban scheduling is suppressed. A mute already in flight may still complete; an obsolete scan cannot persist a new pending ban afterwards. Telegram offers no conditional transaction spanning D1, member permissions and administrator role changes, so last-moment human edits still require operational review.

Violation insertion allocates an ID above both the remaining history and the user's warning baseline, so retention cannot hide a new strike by reusing an old SQLite rowid. Do not rebuild or VACUUM the violations table independently of its baselines: the implicit rowids are part of the application's warning ordering.

The warning-log mode requires `group_settings.logs_warn_only INTEGER NOT NULL DEFAULT 0 CHECK (logs_warn_only IN (0,1))`. A missing column is a known additive migration in `--pre-migration` mode (exit 3), and blocks normal release (exit 2). Execute only the printed ALTER from `schema/ensure_log_filter.sql`, then recheck. Existing saved on/off choices remain intact; a missing settings row resolves to `warn`, and first unrelated Worker setting writes explicitly initialize `warn`. An existing column with an incompatible type, default, missing/changed CHECK, or extra column-level or earlier cross-column restriction blocks **both** modes (exit 2); do not rerun the ALTER or auto-repair it. The check conservatively retains quote/comment syntax, rejects earlier references to the flag, and inspects the approved final-column declaration in `sqlite_master.sql`, because `PRAGMA table_info` alone does not reveal CHECK constraints.
