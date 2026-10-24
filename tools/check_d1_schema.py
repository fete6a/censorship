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
    'm.sql AS definition, p.name AS column_name, p.type AS data_type, '
    'p."notnull" AS not_null, p.pk AS primary_key, '
    'p.dflt_value AS default_value '
    'FROM sqlite_master AS m '
    'LEFT JOIN pragma_table_info(m.name) AS p ON m.type = \'table\' '
    'WHERE m.type IN (\'table\', \'index\') AND m.name NOT LIKE \'sqlite_%\' '
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
            ("object_type", "object_name", "definition", "column_name",
             "data_type", "not_null", "primary_key", "default_value")
        ):
            raise ValueError("malformed D1 metadata result row")
    return rows


def normalized_ddl(value):
    # sqlite_master strips IF NOT EXISTS on CREATE. Whitespace, case, and
    # harmless identifier quote differences should not change comparisons.
    return re.sub(r"\s+", "", (value or "").upper()).replace('"', "").replace(chr(96), "")


def objects(rows):
    by_name = {}
    for row in rows:
        typ, name = row.get("object_type"), row.get("object_name")
        if typ not in ("table", "index") or not isinstance(name, str):
            raise ValueError("invalid schema metadata object")
        by_name.setdefault((typ, name), []).append(row)
    return by_name


def validate(actual_rows, allow_missing_warning=False):
    """Return (status, errors): ready, migration_required, or incompatible.

    Other production-only objects (e.g. d1_migrations) are preserved and not
    treated as version mismatches. Canonical application columns, primary keys,
    defaults and required index definitions must match.
    """
    expected = objects(expected_metadata())
    actual = objects(actual_rows)
    errors = []
    missing_warning = False
    for key, expected_rows in expected.items():
        kind, name = key
        observed = actual.get(key)
        if observed is None:
            if key == ("table", "warning_adjustments") and allow_missing_warning:
                missing_warning = True
                continue
            errors.append(f"missing required {kind}: {name}")
            continue
        if kind == "index":
            if len(observed) != 1 or normalized_ddl(observed[0]["definition"]) != normalized_ddl(expected_rows[0]["definition"]):
                errors.append(f"index definition mismatch: {name}")
            continue

        expected_columns = {row["column_name"]: row for row in expected_rows}
        observed_columns = {row["column_name"]: row for row in observed}
        if None in observed_columns or len(observed_columns) != len(observed):
            errors.append(f"invalid table column metadata: {name}")
            continue
        missing = sorted(set(expected_columns) - set(observed_columns))
        extra = sorted(set(observed_columns) - set(expected_columns))
        if missing or extra:
            errors.append(f"table columns mismatch: {name} missing={missing} extra={extra}")
        for column in sorted(set(expected_columns) & set(observed_columns)):
            exp, got = expected_columns[column], observed_columns[column]
            fields = ("data_type", "not_null", "primary_key", "default_value")
            if any(str(exp[field] or "").upper() != str(got[field] or "").upper() for field in fields):
                errors.append(f"column type/constraint/default mismatch: {name}.{column}")
        # CREATE TABLE IF NOT EXISTS does nothing on an existing incompatible
        # table. Require the exact reviewed constraints for this new table.
        if name == "warning_adjustments":
            if normalized_ddl(observed[0]["definition"]) != normalized_ddl(expected_rows[0]["definition"]):
                errors.append("warning_adjustments CREATE TABLE/CHECK constraints mismatch")
    if errors:
        return "incompatible", errors
    if missing_warning:
        return "migration_required", []
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
    parser.add_argument("--pre-migration", action="store_true", help="Allow ONLY the new warning table to be absent during migration planning")
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
        print("MIGRATION_REQUIRED: existing D1 schema is compatible except warning_adjustments.")
        print("Apply schema/ensure_warning_adjustments.sql manually, then rerun without --pre-migration.")
        return 3
    print("SCHEMA_READY: canonical application tables, columns and indexes verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
