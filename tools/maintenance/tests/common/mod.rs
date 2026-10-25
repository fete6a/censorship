#![allow(dead_code)]
use censorship_maintenance::{canonical_db, inspect_sqlite, scan_migrations, ENSURE_WARNINGS};
use rusqlite::{
    functions::FunctionFlags,
    types::{Value as SqlValue, ValueRef},
    Connection,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};
use syn::visit::Visit;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn source(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}

/// Parse the real Rust string literals, including Rust's line continuations.
/// A renamed/missing function or nonliteral query fails the test immediately.
pub fn queries(name: &str) -> Vec<String> {
    struct Queries(Vec<String>);
    impl<'ast> Visit<'ast> for Queries {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "prepare" {
                if let Some(syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                })) = call.args.first()
                {
                    self.0.push(s.value());
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }
    let file = syn::parse_file(&source("src/store.rs")).unwrap();
    let function = file
        .items
        .iter()
        .find_map(|i| match i {
            syn::Item::Fn(f) if f.sig.ident == name => Some(f),
            _ => None,
        })
        .expect("store function missing");
    let mut visitor = Queries(Vec::new());
    visitor.visit_block(&function.block);
    assert!(!visitor.0.is_empty(), "no SQL in {name}");
    visitor.0
}

pub fn try_sql(db: &Connection, sql: &str, args: Value) -> rusqlite::Result<Value> {
    let args: Vec<SqlValue> = args
        .as_array()
        .unwrap()
        .iter()
        .map(|v| match v {
            Value::Null => SqlValue::Null,
            Value::String(s) => SqlValue::Text(s.clone()),
            Value::Number(n) if n.is_i64() => SqlValue::Integer(n.as_i64().unwrap()),
            Value::Number(n) => SqlValue::Real(n.as_f64().unwrap()),
            _ => panic!("invalid SQL parameter"),
        })
        .collect();
    let mut statement = db.prepare(sql)?;
    let count = statement.column_count();
    let mut cursor = statement.query(rusqlite::params_from_iter(args))?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut cells = Vec::new();
        for i in 0..count {
            cells.push(match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(x) => json!(x),
                ValueRef::Real(x) => json!(x),
                ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                ValueRef::Blob(_) => panic!("unexpected blob"),
            });
        }
        rows.push(Value::Array(cells));
    }
    Ok(Value::Array(rows))
}
pub fn sql(db: &Connection, query: &str, args: Value) -> Value {
    try_sql(db, query, args).unwrap()
}
pub fn q(db: &Connection, name: &str, args: Value) -> Value {
    sql(db, &queries(name)[0], args)
}
pub fn batch(db: &Connection, text: &str) {
    db.execute_batch(text).unwrap();
}
pub fn empty(rows: &Value) -> bool {
    rows.as_array().unwrap().is_empty()
}
pub fn db() -> Connection {
    canonical_db().unwrap()
}
pub fn clock(db: &Connection, now: i64) -> Arc<AtomicI64> {
    let time = Arc::new(AtomicI64::new(now));
    let copy = time.clone();
    db.create_scalar_function("unixepoch", 0, FunctionFlags::SQLITE_UTF8, move |_| {
        Ok(copy.load(Ordering::SeqCst))
    })
    .unwrap();
    time
}
pub fn migrate(db: &Connection) {
    for statement in scan_migrations(&inspect_sqlite(db).unwrap()) {
        batch(db, statement);
    }
    batch(db, ENSURE_WARNINGS);
}
pub fn violation(db: &Connection, chat: i64, user: i64, mid: i64, time: i64) {
    sql(db,"INSERT INTO violations(chat_id,user_id,message_id,created_at,category,probability,action) VALUES(?,?,?,?,'spam',0.99,'warned')", json!([chat,user,mid,time]));
}
pub fn receipt(db: &Connection, id: i64, chat: i64, user: i64, count: i64, time: i64) {
    q(
        db,
        "set_warning_count",
        json!([id, chat, user, count, 11, time]),
    );
}
pub fn count(db: &Connection, chat: i64, user: i64, cutoff: i64) -> i64 {
    q(db, "recent_strikes", json!([chat, user, cutoff]))[0][0]
        .as_i64()
        .unwrap()
}
pub fn pending(db: &Connection, chat: i64, user: i64, due: i64) -> i64 {
    let mid = chat.abs() * 100 + user;
    sql(db,"INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at) VALUES(?,?,?,100,?)",json!([chat,user,mid,due]));
    mid
}
pub fn state(db: &Connection, chat: i64, user: i64) -> Value {
    sql(db,"SELECT message_id,status,attempt_count,next_attempt_at FROM pending_bans WHERE chat_id=? AND user_id=?",json!([chat,user]))[0].clone()
}
