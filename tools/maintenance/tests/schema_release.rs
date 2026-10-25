mod common;
use censorship_maintenance::*;
use common::*;
use rusqlite::Connection;
use serde_json::{json, Value};

fn old() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    batch(
        &db,
        &source("tests/fixtures/schema_before_warning_adjustments.sql"),
    );
    batch(&db,"CREATE TABLE d1_migrations(id INTEGER PRIMARY KEY,name TEXT); INSERT INTO d1_migrations VALUES(6,'0006_pending_retry.sql'); INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(-100,22,42,1700000000,'spam',0.99,'warned'); INSERT INTO archived_messages(chat_id,message_id,user_id,sent_at,updated_at,payload) VALUES(-100,42,22,1700000000,1700000000,'{}'); INSERT INTO group_settings(chat_id,dry_run) VALUES(-100,0); INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) VALUES(-100,22,42,1700000000,1700000060);");
    db
}
fn check(db: &Connection, pre: bool) -> Check {
    validate(&inspect_sqlite(db).unwrap(), pre).unwrap()
}
fn snapshot(db: &Connection) -> Value {
    json!([
        "violations",
        "archived_messages",
        "group_settings",
        "pending_bans",
        "d1_migrations"
    ]
    .map(|name| sql(db, &format!("SELECT * FROM {name}"), json!([]))))
}
fn inspection_batch(db: &Connection) -> Vec<Value> {
    inspection_queries()
        .unwrap()
        .iter()
        .map(|query| {
            let stmt = db.prepare(query).unwrap();
            let names: Vec<_> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let rows = sql(db, query, json!([]));
            let objects: Vec<_> = rows
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    Value::Object(
                        names
                            .iter()
                            .cloned()
                            .zip(row.as_array().unwrap().iter().cloned())
                            .collect(),
                    )
                })
                .collect();
            json!({"success":true,"results":objects})
        })
        .collect()
}

#[test]
fn old_schema_requires_migration_without_mutating_history() {
    let db = old();
    let before = snapshot(&db);
    assert_eq!(check(&db, true).status, Status::MigrationRequired);
    assert_eq!(check(&db, false).status, Status::Incompatible);
    assert_eq!(snapshot(&db), before);
    assert_eq!(db.total_changes(), 5);
}
#[test]
fn migration_is_idempotent_preserves_data_and_applies_receipts() {
    let db = old();
    let before = snapshot(&db);
    assert!(try_sql(
        &db,
        &queries("recent_strikes")[0],
        json!([-100, 22, 1699000000])
    )
    .is_err());
    migrate(&db);
    assert_eq!(snapshot(&db), before);
    assert_eq!(check(&db, false).status, Status::Ready);
    assert_eq!(count(&db, -100, 22, 1699000000), 1);
    receipt(&db, 5000, -100, 22, 2, 1700000000);
    assert_eq!(count(&db, -100, 22, 1699000000), 2);
    assert_eq!(
        sql(&db, "SELECT status FROM pending_bans", json!([])),
        json!([["canceled"]])
    );
    let after = snapshot(&db);
    let adjustments = sql(&db, "SELECT * FROM warning_adjustments", json!([]));
    migrate(&db);
    migrate(&db);
    assert_eq!(snapshot(&db), after);
    assert_eq!(
        sql(&db, "SELECT * FROM warning_adjustments", json!([])),
        adjustments
    );
    assert_eq!(after[0], before[0]);
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM warning_edit_receipts", json!([])),
        json!([[1]])
    );
}
#[test]
fn wrong_warning_table_is_not_repaired_by_if_not_exists() {
    let db = old();
    batch(&db,"CREATE TABLE warning_adjustments(chat_id INTEGER,user_id INTEGER,base_count INTEGER,adjusted_at INTEGER,violation_rowid INTEGER,actor_id INTEGER,PRIMARY KEY(chat_id,user_id))");
    let before = snapshot(&db);
    migrate(&db);
    assert_eq!(check(&db, true).status, Status::Incompatible);
    assert_eq!(snapshot(&db), before);
}
#[test]
fn partial_migration_failure_is_retryable_without_data_loss() {
    let db = old();
    let before = snapshot(&db);
    for s in scan_migrations(&inspect_sqlite(&db).unwrap()) {
        batch(&db, s);
    }
    assert!(db.execute_batch(&format!("{ENSURE_WARNINGS} INSERT INTO warning_adjustments(chat_id,user_id,base_count,adjusted_at,actor_id) VALUES(-100,22,99,1700000000,11)")).is_err());
    assert_eq!(check(&db, false).status, Status::Ready);
    migrate(&db);
    assert_eq!(snapshot(&db), before);
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM warning_adjustments", json!([])),
        json!([[0]])
    );
}
#[test]
fn wrong_or_missing_core_tables_and_empty_databases_block_release() {
    for change in [
        "DROP TABLE scan_jobs; CREATE TABLE scan_jobs(chat_id INTEGER PRIMARY KEY)",
        "DROP TABLE processed_updates",
    ] {
        let db = old();
        batch(&db, change);
        assert_eq!(check(&db, true).status, Status::Incompatible);
    }
    let db = Connection::open_in_memory().unwrap();
    assert_eq!(check(&db, true).status, Status::Incompatible);
}
#[test]
fn normalized_remote_metadata_is_strict_about_failures_and_absent_fields() {
    let rows = inspect_sqlite(&db()).unwrap();
    let good = json!({"success":true,"results":rows});
    assert_eq!(parse_inspection_json(&good.to_string()).unwrap(), rows);
    assert_eq!(
        parse_inspection_json(&json!([good]).to_string()).unwrap(),
        rows
    );
    for bad in [
        json!([]),
        json!([{"success":false,"results":rows}]),
        json!([{"success":true,"results":[]}]),
        json!([good, good]),
        json!([{"success":true,"results":[{"object_type":"table"}]}]),
    ] {
        assert!(parse_inspection_json(&bad.to_string()).is_err());
    }
    let mut missing = serde_json::to_value(&rows).unwrap();
    missing[0].as_object_mut().unwrap().remove("definition");
    assert!(parse_inspection_json(&json!({"success":true,"results":missing}).to_string()).is_err());
    assert!(parse_inspection_json("not JSON").is_err());
}
#[test]
fn literal_pragma_batch_matches_local_metadata_and_rejects_partial_results() {
    let db = old();
    let values = inspection_batch(&db);
    assert_eq!(
        validate(
            &parse_inspection_json(&json!(values).to_string()).unwrap(),
            true
        )
        .unwrap()
        .status,
        Status::MigrationRequired
    );
    migrate(&db);
    let values = inspection_batch(&db);
    let rows = parse_inspection_json(&json!(values).to_string()).unwrap();
    // Extra production migration-ledger tables are intentionally not PRAGMA'd.
    // Compare required objects, not columns of unrelated extra tables.
    for expected in inspect_sqlite(&canonical_db().unwrap()).unwrap() {
        let local = inspect_sqlite(&db).unwrap();
        let observed: Vec<_> = rows
            .iter()
            .filter(|r| r.object_name == expected.object_name)
            .collect();
        let local: Vec<_> = local
            .iter()
            .filter(|r| r.object_name == expected.object_name)
            .collect();
        assert_eq!(observed, local);
    }
    assert_eq!(validate(&rows, false).unwrap().status, Status::Ready);
    let mut incomplete = values.clone();
    incomplete.pop();
    assert!(parse_inspection_json(&json!(incomplete).to_string()).is_err());
    let mut failed = values.clone();
    failed[1]["success"] = json!(false);
    assert!(parse_inspection_json(&json!(failed).to_string()).is_err());
    let mut malformed = values;
    malformed[1]["results"][0]["notnull"] = json!("unknown");
    assert!(parse_inspection_json(&json!(malformed).to_string()).is_err());
}
#[test]
fn production_inspection_is_only_metadata_select_and_literal_pragmas() {
    for query in inspection_queries().unwrap() {
        assert!(query == METADATA_SQL || query.starts_with("PRAGMA table_info(\""));
        for forbidden in ["INSERT ", "DELETE ", "UPDATE ", "ALTER ", "DROP "] {
            assert!(!query.to_uppercase().contains(forbidden));
        }
    }
}
#[test]
fn adjustment_only_or_absent_receipt_objects_are_additively_migratable() {
    for post_pr11 in [false, true] {
        let db = old();
        if post_pr11 {
            batch(
                &db,
                ENSURE_WARNINGS
                    .split("-- Durable per-Telegram-update receipt")
                    .next()
                    .unwrap(),
            );
        }
        let before = snapshot(&db);
        assert_eq!(check(&db, true).status, Status::MigrationRequired);
        migrate(&db);
        assert_eq!(check(&db, false).status, Status::Ready);
        assert_eq!(snapshot(&db), before);
        batch(&db, "DROP TABLE warning_edit_receipts");
        assert_eq!(check(&db, true).status, Status::MigrationRequired);
        migrate(&db);
        assert_eq!(check(&db, false).status, Status::Ready);
    }
}
#[test]
fn receipt_without_trigger_is_not_a_safe_automatic_migration() {
    let db = old();
    migrate(&db);
    batch(&db,"DROP TRIGGER apply_warning_edit_receipt; INSERT INTO warning_edit_receipts VALUES(700,-100,22,0,11,1700000001)");
    for pre in [false, true] {
        assert_eq!(check(&db, pre).status, Status::Incompatible);
    }
    migrate(&db);
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM warning_adjustments", json!([])),
        json!([[0]])
    );
    assert_eq!(
        sql(&db, "SELECT status FROM pending_bans", json!([])),
        json!([["pending"]])
    );
}
#[test]
fn wrong_trigger_missing_adjustments_and_extra_sensitive_trigger_block() {
    for change in ["DROP TRIGGER apply_warning_edit_receipt; CREATE TRIGGER apply_warning_edit_receipt AFTER INSERT ON warning_edit_receipts BEGIN SELECT 1; END",
        "DROP TABLE warning_adjustments", "CREATE TRIGGER unexpected_receipt_effect AFTER INSERT ON warning_edit_receipts BEGIN UPDATE pending_bans SET status='canceled'; END"] {
        let db=old();migrate(&db);batch(&db,change);
        for pre in [false,true] {assert_eq!(check(&db,pre).status,Status::Incompatible);}
    }
}
#[test]
fn trigger_normalization_preserves_string_semantics_and_escaped_quotes() {
    assert_eq!(
        normalized_ddl("SELECT 'a''B c', \"foo\""),
        "SELECT'a''B c',FOO"
    );
    let db = db();
    let mut rows = inspect_sqlite(&db).unwrap();
    for row in &mut rows {
        if row.object_type == "trigger" {
            row.definition = Some(
                row.definition
                    .as_ref()
                    .unwrap()
                    .replace("'pending'", "'PENDING'"),
            );
        }
    }
    assert_eq!(validate(&rows, true).unwrap().status, Status::Incompatible);
    verify_warning_trigger_behavior().unwrap();
}
#[test]
fn scan_migration_resumes_each_partial_state_preserving_jobs() {
    let db = old();
    batch(
        &db,
        "INSERT INTO scan_jobs(chat_id,max_message_id,started_at,updated_at) VALUES(-100,42,1,1)",
    );
    batch(&db, ENSURE_WARNINGS);
    let statements = scan_migrations(&inspect_sqlite(&db).unwrap());
    assert_eq!(statements.len(), 3);
    for (i, s) in statements.iter().enumerate() {
        assert_eq!(check(&db, true).status, Status::MigrationRequired);
        assert_eq!(check(&db, false).status, Status::Incompatible);
        batch(&db, s);
        assert_eq!(
            scan_migrations(&inspect_sqlite(&db).unwrap()),
            statements[i + 1..]
        );
    }
    assert_eq!(check(&db, false).status, Status::Ready);
    assert_eq!(
        sql(
            &db,
            "SELECT chat_id,max_message_id,generation,lease_id,lease_until FROM scan_jobs",
            json!([])
        ),
        json!([[-100, 42, 1, 0, 0]])
    );
}
#[test]
fn wrong_scan_column_is_not_migratable() {
    let db = old();
    batch(&db, "ALTER TABLE scan_jobs ADD COLUMN generation TEXT");
    assert_eq!(check(&db, true).status, Status::Incompatible);
}
#[test]
fn bootstrap_is_for_an_empty_database_only() {
    let db = db();
    assert!(db.execute_batch(SCHEMA).is_err());
    assert!(empty(&sql(
        &db,
        "SELECT name FROM sqlite_master WHERE name='d1_migrations'",
        json!([])
    )));
}
