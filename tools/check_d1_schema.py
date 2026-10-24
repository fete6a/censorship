#!/usr/bin/env python3
"""Read-only, fail-closed release check for the existing production D1 schema.

No migration, data mutation, Worker deployment, or external HTTP call is made
by this script. Remote mode only runs a single SQLite metadata SELECT through
Wrangler against the configured DB binding.
"""
import argparse
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
CANONICAL_SCHEMA = ROOT / "schema/current.sql"

# SQLite's table-valued pragma lets the one read-only D1 query inspect every
# application table and index without querying message/user contents.
INSPECTION_SQL = (
    'SELECT m.type AS object_type, m.name AS object_name, '
    'm.tbl_name AS table_name, m.sql AS definition, '
    'p.name AS column_name, p.type AS data_type, '
    'p."notnull" AS not_null, p.pk AS primary_key, '
    'p.dflt_value AS default_value '
    'FROM sqlite_master AS m '
    'LEFT JOIN pragma_table_info(m.name) AS p ON m.type = \'table\' '
    'WHERE m.type IN (\'table\', \'index\', \'trigger\') '
    'AND m.name NOT LIKE \'sqlite_%\' '
    'ORDER BY m.type, m.name, p.cid'
)


def inspect_sqlite(db):
    """Test/local adapter for exactly the same metadata query used in D1."""
    previous = db.row_factory
    try:
        db.row_factory = sqlite3.Row
        return [dict(row) for row in db.execute(INSPECTION_SQL)]
    finally:
        db.row_factory = previous


def expected_metadata():
    """Construct the expected schema in an isolated, in-memory SQLite DB."""
    db = sqlite3.connect(":memory:")
    try:
        db.executescript(CANONICAL_SCHEMA.read_text(encoding="utf-8"))
        return inspect_sqlite(db)
    finally:
        db.close()


def parse_d1_json(text):
    """Reject failed, ambiguous or incomplete Wrangler D1 responses."""
    payload = json.loads(text)
    if isinstance(payload, dict):
        payload = [payload]
    if not isinstance(payload, list) or len(payload) != 1:
        raise ValueError("expected exactly one D1 statement result")
    statement = payload[0]
    if not isinstance(statement, dict) or statement.get("success") is not True:
        raise ValueError("D1 metadata query was not confirmed successful")
    rows = statement.get("results")
    if not isinstance(rows, list) or not rows:
        raise ValueError("missing D1 metadata results")
    for row in rows:
        if not isinstance(row, dict) or not all(
            key in row for key in
            ("object_type", "object_name", "table_name", "definition", "column_name",
             "data_type", "not_null", "primary_key", "default_value")
        ):
            raise ValueError("malformed D1 metadata result row")
    return rows


def normalized_ddl(value):
    # sqlite_master strips IF NOT EXISTS on CREATE. Whitespace, case, and
    # harmless identifier quote differences should not change comparisons.
    # String values are case/whitespace sensitive even though SQL keywords
    # are not. In particular, status='PENDING' must not match 'pending'.
    parts = re.split(r"('(?:''|[^'])*')", value or "")
    return "".join(part if i % 2 else re.sub(r"\s+", "", part.upper()).replace('"', "").replace(chr(96), "")
                   for i, part in enumerate(parts))


def objects(rows):
    by_name = {}
    for row in rows:
        typ, name = row.get("object_type"), row.get("object_name")
        if typ not in ("table", "index", "trigger") or not isinstance(name, str) or not isinstance(row.get("table_name"), str):
            raise ValueError("invalid schema metadata object")
        by_name.setdefault((typ, name), []).append(row)
    return by_name


# Only these objects may be absent in an explicitly checked old version.
MIGRATABLE_OBJECTS = {
    ("table", "warning_adjustments"),
    ("table", "warning_edit_receipts"),
    ("trigger", "apply_warning_edit_receipt"),
}
SCAN_COLUMNS = {"generation", "lease_id", "lease_until"}


def scan_migrations(rows):
    """Return only missing, trusted ALTERs; never execute production SQL."""
    columns = {row["column_name"] for row in rows
               if row["object_type"] == "table" and row["object_name"] == "scan_jobs"}
    statements = (ROOT / "schema/ensure_scan_fencing.sql").read_text(encoding="utf-8").splitlines()
    return [sql for sql in statements if any(
        sql.startswith(f"ALTER TABLE scan_jobs ADD COLUMN {column} ")
        for column in SCAN_COLUMNS - columns
    )]


# A second trigger could sabotage atomicity even if the named trigger is correct.
SENSITIVE_TRIGGER_TABLES = {
    "warning_edit_receipts", "warning_adjustments", "pending_bans"
}


def verify_warning_trigger_behavior():
    """Use only trusted canonical SQL and dummy in-memory data.

    Do not execute a trigger definition extracted from production metadata.
    D1 remote preflight performs only SELECT; no real rows are accessed.
    """
    db = sqlite3.connect(":memory:")
    try:
        db.executescript(CANONICAL_SCHEMA.read_text(encoding="utf-8"))
        for chat, message in ((-100, 41), (-200, 51)):
            db.execute(
                "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
                "VALUES(?,?,?,1700000000,'spam',0.9,'warned')",
                (chat, 22, message),
            )
            db.execute(
                "INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) "
                "VALUES(?,?,?,1700000000,1700000060)",
                (chat, 22, message),
            )
        receipt = (
            "INSERT INTO warning_edit_receipts(update_id,chat_id,user_id,base_count,actor_id,created_at) "
            "VALUES(?,?,?,?,?,?) ON CONFLICT(update_id) DO NOTHING"
        )
        db.execute(receipt, (900, -100, 22, 0, 11, 1700000001))
        baseline = db.execute(
            "SELECT base_count,violation_rowid,actor_id FROM warning_adjustments "
            "WHERE chat_id=-100 AND user_id=22"
        ).fetchone()
        canceled = db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-100 AND user_id=22"
        ).fetchone()
        untouched = db.execute(
            "SELECT status FROM pending_bans WHERE chat_id=-200 AND user_id=22"
        ).fetchone()
        if baseline != (0, 1, 11) or canceled != ("canceled",) or untouched != ("pending",):
            raise ValueError("canonical warning trigger did not apply baseline and scoped cancellation")
        db.execute(
            "INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) "
            "VALUES(-100,22,42,1700000002,'spam',0.9,'warned')"
        )
        db.execute(receipt, (900, -100, 22, 0, 11, 1700000003))
        unchanged = db.execute(
            "SELECT base_count,violation_rowid,actor_id FROM warning_adjustments "
            "WHERE chat_id=-100 AND user_id=22"
        ).fetchone()
        new_strikes = db.execute(
            "SELECT COUNT(*) FROM violations WHERE chat_id=-100 AND user_id=22 "
            "AND rowid>(SELECT violation_rowid FROM warning_adjustments "
            "WHERE chat_id=-100 AND user_id=22)"
        ).fetchone()
        receipts = db.execute("SELECT COUNT(*) FROM warning_edit_receipts").fetchone()
        if unchanged != baseline or new_strikes != (1,) or receipts != (1,):
            raise ValueError("replayed warning receipt erased a newer genuine violation")
    except sqlite3.Error as error:
        raise ValueError(f"isolated SQLite trigger behavioral check failed: {error}") from error
    finally:
        db.close()


def validate(actual_rows, allow_missing_warning=False):
    """Return ready, migration_required, incompatible.

    --pre-migration allows known additive old versions only. A receipt table
    with a missing trigger is not safe to auto-repair: receipt inserts may have
    succeeded without applying their side effects.
    """
    expected = objects(expected_metadata())
    actual = objects(actual_rows)
    errors = []
    missing = set()
    for key, expected_rows in expected.items():
        kind, name = key
        observed = actual.get(key)
        if observed is None:
            if allow_missing_warning and key in MIGRATABLE_OBJECTS:
                missing.add(key)
            else:
                errors.append(f"missing required {kind}: {name}")
            continue
        if kind in ("index", "trigger"):
            if len(observed) != 1 or normalized_ddl(observed[0]["definition"]) != normalized_ddl(expected_rows[0]["definition"]):
                errors.append(f"{kind} definition mismatch: {name}")
            if kind == "trigger" and observed[0]["table_name"] != expected_rows[0]["table_name"]:
                errors.append(f"trigger target mismatch: {name}")
            continue

        expected_columns = {row["column_name"]: row for row in expected_rows}
        observed_columns = {row["column_name"]: row for row in observed}
        if None in observed_columns or len(observed_columns) != len(observed):
            errors.append(f"invalid table column metadata: {name}")
            continue
        missing_columns = sorted(set(expected_columns) - set(observed_columns))
        extra_columns = sorted(set(observed_columns) - set(expected_columns))
        if allow_missing_warning and name == "scan_jobs":
            for column in set(missing_columns) & SCAN_COLUMNS:
                missing.add(("column", f"scan_jobs.{column}"))
                missing_columns.remove(column)
        if missing_columns or extra_columns:
            errors.append(f"table columns mismatch: {name} missing={missing_columns} extra={extra_columns}")
        for column in sorted(set(expected_columns) & set(observed_columns)):
            exp, got = expected_columns[column], observed_columns[column]
            fields = ("data_type", "not_null", "primary_key", "default_value")
            if any(str(exp[field] or "").upper() != str(got[field] or "").upper() for field in fields):
                errors.append(f"column type/constraint/default mismatch: {name}.{column}")
        if name in ("warning_adjustments", "warning_edit_receipts"):
            if normalized_ddl(observed[0]["definition"]) != normalized_ddl(expected_rows[0]["definition"]):
                errors.append(f"{name} CREATE TABLE/CHECK constraints mismatch")

    # Extra triggers on critical tables can silently change receipt semantics.
    for key, rows in actual.items():
        if key[0] == "trigger" and key not in expected and any(
            row["table_name"] in SENSITIVE_TRIGGER_TABLES for row in rows
        ):
            errors.append(f"unexpected trigger on warning/pending table: {key[1]}")

    receipts = ("table", "warning_edit_receipts") in actual
    adjustment = ("table", "warning_adjustments") in actual
    trigger = ("trigger", "apply_warning_edit_receipt") in actual
    if receipts and not trigger:
        errors.append("receipt table exists without apply_warning_edit_receipt trigger; possible unapplied receipts need reconciliation")
    if receipts and not adjustment:
        errors.append("receipt table exists without warning_adjustments; possible unapplied adjustments need reconciliation")
    if trigger and not receipts:
        errors.append("receipt trigger exists without the receipt table")

    if errors:
        return "incompatible", errors
    if missing:
        return "migration_required", []
    try:
        verify_warning_trigger_behavior()
    except ValueError as error:
        return "incompatible", [str(error)]
    return "ready", []


def remote_metadata():
    # Do not use a shell or a command supplied by external input. The config
    # path is absolute to avoid checking a different project accidentally.
    command = [
        "npx", "--no-install", "wrangler", "d1", "execute", "DB", "--remote",
        "--config", str(ROOT / "wrangler.jsonc"),
        "--command", INSPECTION_SQL, "--json",
    ]
    outcome = subprocess.run(
        command, cwd=ROOT, text=True, capture_output=True, check=False, timeout=60,
    )
    if outcome.returncode:
        raise RuntimeError(f"read-only Wrangler D1 query failed (exit {outcome.returncode})")
    return parse_d1_json(outcome.stdout)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--remote", action="store_true", help="Read production DB binding via Wrangler --remote")
    source.add_argument("--from-json", type=Path, metavar="FILE", help="Check saved --json response from an equivalent read-only D1 query")
    parser.add_argument("--pre-migration", action="store_true", help="Recognize known additive warning and scan-fencing migrations")
    args = parser.parse_args(argv)
    try:
        rows = remote_metadata() if args.remote else parse_d1_json(args.from_json.read_text(encoding="utf-8"))
        status, problems = validate(rows, allow_missing_warning=args.pre_migration)
    except (OSError, ValueError, RuntimeError, sqlite3.Error, subprocess.TimeoutExpired) as error:
        print(f"SCHEMA_CHECK_FAILED: {error}", file=sys.stderr)
        return 2
    if status == "incompatible":
        for issue in problems:
            print(f"SCHEMA_INCOMPATIBLE: {issue}", file=sys.stderr)
        print("STOP: investigate D1 schema; do not run full bootstrap or deploy Worker.", file=sys.stderr)
        return 2
    if status == "migration_required":
        print("MIGRATION_REQUIRED: known additive warning objects or scan-fencing columns are absent.")
        print("Apply schema/ensure_warning_adjustments.sql manually (idempotent).")
        for statement in scan_migrations(rows):
            print(statement)
        print("Apply only the missing scan ALTERs printed above, then rerun without --pre-migration.")
        return 3
    print("SCHEMA_READY: canonical tables, indexes, receipt trigger DDL and isolated trigger behavior verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
