//! Native, read-only D1 release gate. Production supplies metadata only;
//! behavioral checks run exclusively on canonical SQL in disposable SQLite.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const SCHEMA: &str = include_str!("../../../schema/current.sql");
pub const ENSURE_WARNINGS: &str = include_str!("../../../schema/ensure_warning_adjustments.sql");
pub const ENSURE_SCAN: &str = include_str!("../../../schema/ensure_scan_fencing.sql");
pub const ENSURE_LOG_FILTER: &str = include_str!("../../../schema/ensure_log_filter.sql");
pub const ENSURE_CLEANUP: &str = include_str!("../../../schema/ensure_cleanup_state.sql");
pub const METADATA_SQL: &str = "SELECT type AS object_type,name AS object_name,tbl_name AS table_name,sql AS definition FROM sqlite_master WHERE type IN ('table','index','trigger') AND name NOT GLOB 'sqlite_*' ORDER BY type,name";
const INSPECTION_SQL: &str = "SELECT m.type,m.name,m.tbl_name,m.sql,p.name,p.type,p.\"notnull\",p.pk,p.dflt_value FROM sqlite_master AS m LEFT JOIN pragma_table_info(m.name) AS p ON m.type='table' WHERE m.type IN ('table','index','trigger') AND m.name NOT GLOB 'sqlite_*' ORDER BY m.type,m.name,p.cid";
const SCAN_COLUMNS: [&str; 3] = ["generation", "lease_id", "lease_until"];
// PRAGMA table_info omits CHECK constraints. The canonical schema and approved
// additive ALTER both place this exact column declaration last.
const LOG_FILTER_COLUMN_DDL_SUFFIXES: [&str; 2] = [
    ",LOGS_WARN_ONLYINTEGERNOTNULLDEFAULT1CHECK(LOGS_WARN_ONLYIN(0,1)))",
    ",LOGS_WARN_ONLYINTEGERNOTNULLDEFAULT0CHECK(LOGS_WARN_ONLYIN(0,1)))",
];

fn approved_log_filter_ddl(sql: &str) -> bool {
    // Deliberately conservative: retain every quote/comment character. In
    // particular, SQLite double-quoted strings must never become SQL tokens.
    // Unsupported equivalent spellings require operator investigation.
    let ddl: String = sql.chars().filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase).collect();
    let ddl=ddl.trim_end_matches(';');
    let Some(prefix) = LOG_FILTER_COLUMN_DDL_SUFFIXES.iter()
        .find_map(|suffix| ddl.strip_suffix(suffix)) else {
        return false;
    };
    // Earlier column/table constraints can also restrict the new flag.
    // Reject all earlier references, including quoted identifiers and comments.
    !prefix.contains("LOGS_WARN_ONLY")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub object_type: String,
    pub object_name: String,
    pub table_name: String,
    pub definition: Option<String>,
    pub column_name: Option<String>,
    pub data_type: Option<String>,
    pub not_null: Option<i64>,
    pub primary_key: Option<i64>,
    pub default_value: Option<String>,
}

pub fn inspect_sqlite(db: &Connection) -> Result<Vec<Row>> {
    Ok(db
        .prepare(INSPECTION_SQL)?
        .query_map([], |r| {
            Ok(Row {
                object_type: r.get(0)?,
                object_name: r.get(1)?,
                table_name: r.get(2)?,
                definition: r.get(3)?,
                column_name: r.get(4)?,
                data_type: r.get(5)?,
                not_null: r.get(6)?,
                primary_key: r.get(7)?,
                default_value: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn canonical_db() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(SCHEMA)?;
    Ok(db)
}

pub fn inspection_queries() -> Result<Vec<String>> {
    let names: BTreeSet<_> = inspect_sqlite(&canonical_db()?)?
        .into_iter()
        .filter(|r| r.object_type == "table")
        .map(|r| r.object_name)
        .collect();
    let mut queries = vec![METADATA_SQL.to_owned()];
    queries.extend(
        names
            .into_iter()
            .map(|name| format!("PRAGMA table_info(\"{name}\")")),
    );
    Ok(queries)
}

fn required_keys(value: &Value, keys: &[&str]) -> Result<()> {
    let obj = value.as_object().ok_or("expected metadata object")?;
    if keys.iter().any(|k| !obj.contains_key(*k)) {
        return Err("incomplete metadata object".into());
    }
    Ok(())
}

fn results(value: &Value) -> Result<&Vec<Value>> {
    if value["success"] != true {
        return Err("D1 statement was not confirmed successful".into());
    }
    value["results"]
        .as_array()
        .ok_or_else(|| "missing D1 results".into())
}

/// Accept the original normalized format and the Wrangler-compatible literal
/// PRAGMA batch. Reject incomplete/failed batches, including absent null fields.
pub fn parse_inspection_json(text: &str) -> Result<Vec<Row>> {
    let payload: Value = serde_json::from_str(text)?;
    let statements = match payload {
        Value::Array(a) => a,
        Value::Object(_) => vec![payload],
        _ => return Err("expected D1 statement results".into()),
    };
    if statements.len() == 1 {
        let values = results(&statements[0])?;
        if values.is_empty() {
            return Err("missing D1 metadata".into());
        }
        return values
            .iter()
            .map(|v| {
                required_keys(
                    v,
                    &[
                        "object_type",
                        "object_name",
                        "table_name",
                        "definition",
                        "column_name",
                        "data_type",
                        "not_null",
                        "primary_key",
                        "default_value",
                    ],
                )?;
                Ok(serde_json::from_value(v.clone())?)
            })
            .collect();
    }
    let queries = inspection_queries()?;
    if statements.len() != queries.len() {
        return Err("incomplete D1 metadata/PRAGMA batch".into());
    }
    for statement in &statements {
        results(statement)?;
    }
    let mut columns = BTreeMap::new();
    for (query, statement) in queries.iter().skip(1).zip(statements.iter().skip(1)) {
        let name = query.split('"').nth(1).ok_or("invalid trusted PRAGMA")?;
        for c in results(statement)? {
            required_keys(c, &["name", "type", "notnull", "pk", "dflt_value"])?;
            if !c["name"].is_string()
                || !c["type"].is_string()
                || !c["notnull"].is_i64()
                || !c["pk"].is_i64()
            {
                return Err("malformed D1 PRAGMA column".into());
            }
        }
        columns.insert(name, results(statement)?);
    }
    let mut rows = Vec::new();
    for obj in results(&statements[0])? {
        required_keys(
            obj,
            &["object_type", "object_name", "table_name", "definition"],
        )?;
        // Deserialize even objects with no columns, so malformed names fail closed.
        let mut base = obj.clone();
        for key in [
            "column_name",
            "data_type",
            "not_null",
            "primary_key",
            "default_value",
        ] {
            base[key] = Value::Null;
        }
        let base: Row = serde_json::from_value(base)?;
        let cols = if base.object_type == "table" {
            columns.get(base.object_name.as_str()).copied()
        } else {
            None
        };
        if let Some(cols) = cols.filter(|c| !c.is_empty()) {
            for c in cols {
                rows.push(Row {
                    column_name: Some(c["name"].as_str().ok_or("invalid column name")?.into()),
                    data_type: Some(c["type"].as_str().ok_or("invalid column type")?.into()),
                    not_null: c["notnull"].as_i64(),
                    primary_key: c["pk"].as_i64(),
                    default_value: serde_json::from_value(c["dflt_value"].clone())?,
                    ..base.clone()
                });
            }
        } else {
            rows.push(base);
        }
    }
    if rows.is_empty() {
        return Err("missing D1 schema objects".into());
    }
    Ok(rows)
}

/// Normalize SQL keywords/identifier quoting but preserve single-quoted string
/// values exactly, including doubled quotes and case-sensitive status values.
pub fn normalized_ddl(sql: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            out.push(c);
            if quoted && chars.peek() == Some(&'\'') {
                out.push(chars.next().unwrap());
            } else {
                quoted = !quoted;
            }
        } else if quoted {
            out.push(c);
        } else if !c.is_whitespace() && c != '"' && c != '`' {
            out.extend(c.to_uppercase());
        }
    }
    out
}

type Objects<'a> = BTreeMap<(&'a str, &'a str), Vec<&'a Row>>;
fn objects(rows: &[Row]) -> Result<Objects<'_>> {
    let mut map: Objects<'_> = BTreeMap::new();
    for row in rows {
        if !matches!(row.object_type.as_str(), "table" | "index" | "trigger") {
            return Err("invalid schema object kind".into());
        }
        map.entry((&row.object_type, &row.object_name))
            .or_default()
            .push(row);
    }
    Ok(map)
}

pub fn scan_migrations(rows: &[Row]) -> Vec<&'static str> {
    let columns: BTreeSet<_> = rows
        .iter()
        .filter(|r| r.object_type == "table" && r.object_name == "scan_jobs")
        .filter_map(|r| r.column_name.as_deref())
        .collect();
    ENSURE_SCAN
        .lines()
        .filter(|sql| {
            SCAN_COLUMNS.iter().any(|c| {
                !columns.contains(c)
                    && sql.starts_with(&format!("ALTER TABLE scan_jobs ADD COLUMN {c} "))
            })
        })
        .collect()
}

pub fn additive_migrations(rows: &[Row]) -> Vec<&'static str> {
    let mut statements = scan_migrations(rows);
    if !rows.iter().any(|r| {
        r.object_type == "table"
            && r.object_name == "group_settings"
            && r.column_name.as_deref() == Some("logs_warn_only")
    }) {
        statements.extend(
            ENSURE_LOG_FILTER
                .lines()
                .filter(|line| line.starts_with("ALTER TABLE ")),
        );
    }
    if !rows.iter().any(|r| r.object_type == "table" && r.object_name == "cleanup_state") {
        statements.extend(ENSURE_CLEANUP.lines().filter(|line| line.starts_with("CREATE TABLE ")));
    }
    statements
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    MigrationRequired,
    Incompatible,
}
#[derive(Debug)]
pub struct Check {
    pub status: Status,
    pub errors: Vec<String>,
}

pub fn validate(rows: &[Row], pre_migration: bool) -> Result<Check> {
    let expected_rows = inspect_sqlite(&canonical_db()?)?;
    let expected = objects(&expected_rows)?;
    let actual = objects(rows)?;
    let mut errors = Vec::new();
    let mut missing = false;
    for (&(kind, name), exp) in &expected {
        let Some(got) = actual.get(&(kind, name)) else {
            if pre_migration
                && matches!(
                    (kind, name),
                    ("table", "warning_adjustments" | "warning_edit_receipts" | "cleanup_state")
                        | ("trigger", "apply_warning_edit_receipt")
                )
            {
                missing = true;
            } else {
                errors.push(format!("missing required {kind}: {name}"));
            }
            continue;
        };
        if kind != "table" {
            if got.len() != 1
                || normalized_ddl(got[0].definition.as_deref().unwrap_or(""))
                    != normalized_ddl(exp[0].definition.as_deref().unwrap_or(""))
            {
                errors.push(format!("{kind} definition mismatch: {name}"));
            }
            if kind == "trigger" && got[0].table_name != exp[0].table_name {
                errors.push(format!("trigger target mismatch: {name}"));
            }
            continue;
        }
        let exp_cols: BTreeMap<_, _> = exp.iter().map(|r| (r.column_name.as_deref(), r)).collect();
        let got_cols: BTreeMap<_, _> = got.iter().map(|r| (r.column_name.as_deref(), r)).collect();
        if got_cols.contains_key(&None) || got_cols.len() != got.len() {
            errors.push(format!("invalid table column metadata: {name}"));
            continue;
        }
        for (&column, e) in &exp_cols {
            let Some(g) = got_cols.get(&column) else {
                if pre_migration
                    && ((name == "scan_jobs" && column.is_some_and(|c| SCAN_COLUMNS.contains(&c)))
                        || (name == "group_settings" && column == Some("logs_warn_only")))
                {
                    missing = true;
                } else {
                    errors.push(format!(
                        "missing table column: {name}.{}",
                        column.unwrap_or("")
                    ));
                }
                continue;
            };
            // The existing D1 additive migration intentionally has DEFAULT 0.
            // Accept that legacy column without changing any persisted group mode.
            let legacy_log_default = name=="group_settings"
                && column==Some("logs_warn_only")
                && e.default_value.as_deref()==Some("1")
                && g.default_value.as_deref()==Some("0");
            if e.data_type.as_deref().unwrap_or("").to_uppercase()
                != g.data_type.as_deref().unwrap_or("").to_uppercase()
                || e.not_null != g.not_null
                || e.primary_key != g.primary_key
                || (!legacy_log_default && e.default_value.as_deref().unwrap_or("").to_uppercase()
                    != g.default_value.as_deref().unwrap_or("").to_uppercase())
            {
                errors.push(format!(
                    "column type/constraint/default mismatch: {name}.{}",
                    column.unwrap_or("")
                ));
            }
        }
        if name == "group_settings" && got_cols.contains_key(&Some("logs_warn_only")) {
            // Compare only the new column's DDL; historical definitions of
            // unrelated settings need not be textually identical to SCHEMA.
            if !approved_log_filter_ddl(got[0].definition.as_deref().unwrap_or("")) {
                errors.push("group_settings.logs_warn_only CHECK/column DDL mismatch".into());
            }
        }
        if got_cols.keys().any(|c| !exp_cols.contains_key(c)) {
            errors.push(format!("unexpected table columns: {name}"));
        }
        if matches!(name, "warning_adjustments" | "warning_edit_receipts" | "cleanup_state")
            && normalized_ddl(got[0].definition.as_deref().unwrap_or(""))
                != normalized_ddl(exp[0].definition.as_deref().unwrap_or(""))
        {
            errors.push(format!("{name} CREATE TABLE/CHECK constraints mismatch"));
        }
    }
    // A trigger on an unrelated table can still change group privacy settings.
    // Without reviewing all transitive SQL effects, any extra trigger is unsafe.
    for (&(kind, name), _) in &actual {
        if kind == "trigger" && !expected.contains_key(&(kind, name)) {
            errors.push(format!("unexpected trigger (unreviewed effects): {name}"));
        }
    }
    let receipts = actual.contains_key(&("table", "warning_edit_receipts"));
    if receipts && !actual.contains_key(&("trigger", "apply_warning_edit_receipt")) {
        errors.push("receipt table exists without apply_warning_edit_receipt trigger; possible unapplied receipts need reconciliation".into());
    }
    if receipts && !actual.contains_key(&("table", "warning_adjustments")) {
        errors.push("receipt table exists without warning_adjustments; possible unapplied adjustments need reconciliation".into());
    }
    if !receipts && actual.contains_key(&("trigger", "apply_warning_edit_receipt")) {
        errors.push("receipt trigger exists without the receipt table".into());
    }
    let status = if !errors.is_empty() {
        Status::Incompatible
    } else if missing {
        Status::MigrationRequired
    } else {
        verify_warning_trigger_behavior()?;
        Status::Ready
    };
    Ok(Check { status, errors })
}

pub fn verify_warning_trigger_behavior() -> Result<()> {
    let db = canonical_db()?;
    for (chat, msg) in [(-100, 41), (-200, 51)] {
        db.execute("INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(?,22,?,1700000000,'spam',0.9,'warned')", [chat,msg])?;
        db.execute("INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) VALUES(?,22,?,1700000000,1700000060)", [chat,msg])?;
    }
    let receipt = "INSERT INTO warning_edit_receipts(update_id,chat_id,user_id,base_count,actor_id,created_at) VALUES(900,-100,22,0,11,?) ON CONFLICT(update_id) DO NOTHING";
    db.execute(receipt, [1700000001])?;
    let baseline: (i64,i64,i64) = db.query_row("SELECT base_count,violation_rowid,actor_id FROM warning_adjustments WHERE chat_id=-100 AND user_id=22", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let status = |chat| {
        db.query_row(
            "SELECT status FROM pending_bans WHERE chat_id=?",
            [chat],
            |r| r.get::<_, String>(0),
        )
    };
    if baseline != (0, 1, 11) || status(-100)? != "canceled" || status(-200)? != "pending" {
        return Err("canonical trigger failed scoped adjustment/cancellation".into());
    }
    db.execute_batch("INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(-100,22,42,1700000002,'spam',0.9,'warned')")?;
    db.execute(receipt, [1700000003])?;
    let after: (i64,i64,i64) = db.query_row("SELECT base_count,violation_rowid,actor_id FROM warning_adjustments WHERE chat_id=-100 AND user_id=22", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let new: i64 = db.query_row("SELECT COUNT(*) FROM violations WHERE chat_id=-100 AND user_id=22 AND rowid>(SELECT violation_rowid FROM warning_adjustments WHERE chat_id=-100 AND user_id=22)", [], |r| r.get(0))?;
    let receipts: i64 = db.query_row("SELECT COUNT(*) FROM warning_edit_receipts", [], |r| {
        r.get(0)
    })?;
    if after != baseline || new != 1 || receipts != 1 {
        return Err("replayed warning receipt erased a genuine violation".into());
    }
    Ok(())
}
