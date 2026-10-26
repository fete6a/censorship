use censorship_maintenance::{canonical_db, inspect_sqlite, inspection_queries};
use serde_json::json;
use std::{
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "censorship-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_check-d1-schema"))
}
fn output(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

#[test]
fn cli_prints_read_only_inspection_without_external_tools() {
    let out = output(&["--print-inspection-sql"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("SELECT "));
    assert_eq!(text.matches("PRAGMA table_info(").count(), inspection_queries().unwrap().len()-1);
}
#[test]
fn cli_ready_missing_and_pre_migration_exit_codes() {
    let temp = Temp::new();
    let db = canonical_db().unwrap();
    let rows = inspect_sqlite(&db).unwrap();
    let file = temp.file(
        "response.json",
        &json!([{"success":true,"results":rows}]).to_string(),
    );
    let out = command().args(["--from-json"]).arg(&file).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("SCHEMA_READY"));
    db.execute_batch("DROP TABLE warning_edit_receipts; DROP TABLE warning_adjustments")
        .unwrap();
    let rows = inspect_sqlite(&db).unwrap();
    temp.file(
        "response.json",
        &json!([{"success":true,"results":rows}]).to_string(),
    );
    assert_eq!(
        command()
            .arg("--from-json")
            .arg(&file)
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    let out = command()
        .arg("--from-json")
        .arg(&file)
        .arg("--pre-migration")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("MIGRATION_REQUIRED"));
}
#[test]
fn cli_cleanup_only_migration_prints_actionable_create_and_reaches_ready() {
    let temp = Temp::new();
    let db = canonical_db().unwrap();
    db.execute_batch("DROP TABLE cleanup_state").unwrap();
    let file = temp.file(
        "cleanup-missing.json",
        &json!([{"success":true,"results":inspect_sqlite(&db).unwrap()}]).to_string(),
    );
    let out = command()
        .arg("--from-json")
        .arg(&file)
        .arg("--pre-migration")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("cleanup-state table"));
    assert!(text.contains("Apply only the missing additive SQL statements printed above"));
    assert!(!text.contains("only the missing ALTERs"));
    let statements: Vec<_> = text.lines()
        .filter(|line| line.starts_with("CREATE TABLE ") || line.starts_with("ALTER TABLE "))
        .collect();
    assert_eq!(statements.len(), 1);
    assert!(statements[0].starts_with("CREATE TABLE IF NOT EXISTS cleanup_state"));
    // Execute the CLI's actual migration advice on disposable SQLite, then
    // inspect afresh and require the normal post-migration CLI to accept it.
    for statement in statements {
        db.execute_batch(statement).unwrap();
        db.execute_batch(statement).unwrap();
    }
    temp.file(
        "cleanup-missing.json",
        &json!([{"success":true,"results":inspect_sqlite(&db).unwrap()}]).to_string(),
    );
    let out = command().arg("--from-json").arg(&file).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("SCHEMA_READY"));
}
#[test]
fn cli_invalid_arguments_missing_files_and_bad_json_fail_closed() {
    for args in [
        vec![],
        vec!["--wat"],
        vec!["--from-json"],
        vec!["--config"],
        vec!["--remote", "--print-inspection-sql"],
        vec!["--from-json", "/missing/censorship.json"],
    ] {
        assert_eq!(output(&args).status.code(), Some(2));
    }
    let temp = Temp::new();
    for text in [
        "not JSON",
        "[]",
        "[{\"success\":false,\"results\":[]}]",
        "[{\"success\":true,\"results\":[]}]",
        "{}",
    ] {
        let file = temp.file("bad.json", text);
        assert_eq!(
            command()
                .arg("--from-json")
                .arg(file)
                .output()
                .unwrap()
                .status
                .code(),
            Some(2)
        );
    }
}
#[test]
fn cli_help_succeeds() {
    assert!(output(&["--help"]).status.success());
}

#[cfg(unix)]
#[test]
fn remote_invokes_only_read_only_wrangler_with_literal_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let config = temp.file("config with spaces.jsonc", "{}");
    let response = temp.file(
        "response.json",
        &json!([{"success":true,"results":inspect_sqlite(&canonical_db().unwrap()).unwrap()}])
            .to_string(),
    );
    let args = temp.0.join("args.txt");
    let mock = temp.file(
        "npx",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec /bin/cat '{}'\n",
            args.display(),
            response.display()
        ),
    );
    std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = command()
        .env("PATH", &temp.0)
        .arg("--remote")
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(args).unwrap();
    assert!(text.starts_with("--no-install\nwrangler\nd1\nexecute\nDB\n--remote\n--config\n"));
    assert!(text.contains(config.to_str().unwrap()));
    assert!(text.contains("SELECT type AS object_type"));
    assert!(text.ends_with("--json\n"));
    assert!(!text.contains("INSERT "));
    temp.file("npx", "#!/bin/sh\nexit 9\n");
    assert_eq!(
        command()
            .env("PATH", &temp.0)
            .arg("--remote")
            .arg("--config")
            .arg(config)
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}
