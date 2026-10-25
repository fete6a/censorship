use censorship_maintenance::{
    inspection_queries, parse_inspection_json, scan_migrations, validate, Result, Status,
};
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const HELP: &str = "Read-only production D1 schema check (never migrates or deploys).\n\
Usage: check-d1-schema (--remote | --from-json FILE | --print-inspection-sql) [--pre-migration] [--config PATH]\n\
Exit codes: 0 ready, 3 known additive migration required, 2 incompatible/error.\n\
Run via cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- ...";

fn remote(command: &mut Command) -> Result<String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain both pipes concurrently, so a large schema cannot deadlock wait().
    let mut stdout = child.stdout.take().ok_or("missing stdout")?;
    let mut stderr = child.stderr.take().ok_or("missing stderr")?;
    let out = thread::spawn(move || {
        let mut s = String::new();
        stdout.read_to_string(&mut s).map(|_| s)
    });
    let err = thread::spawn(move || {
        let mut s = Vec::new();
        stderr.read_to_end(&mut s)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("read-only Wrangler D1 query timed out after 60 seconds".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let text = out.join().map_err(|_| "stdout reader failed")??;
    let _ = err.join().map_err(|_| "stderr reader failed")??;
    if !status.success() {
        return Err(format!("read-only Wrangler D1 query failed ({status})").into());
    }
    Ok(text)
}

fn run() -> Result<i32> {
    let mut source = None;
    let mut pre = false;
    let mut config = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wrangler.jsonc");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(0);
            }
            "--pre-migration" => pre = true,
            "--config" => config = PathBuf::from(args.next().ok_or("--config requires a path")?),
            "--remote" | "--from-json" | "--print-inspection-sql" => {
                if source.is_some() {
                    return Err("choose exactly one metadata source".into());
                }
                let file = if arg == "--from-json" {
                    Some(args.next().ok_or("--from-json requires a file")?)
                } else {
                    None
                };
                source = Some((arg, file));
            }
            _ => return Err(format!("unknown argument: {arg}\n{HELP}").into()),
        }
    }
    let (source, file) = source.ok_or(HELP)?;
    let sql = inspection_queries()?.join(";\n") + ";";
    if source == "--print-inspection-sql" {
        println!("{sql}");
        return Ok(0);
    }
    let text = if let Some(file) = file {
        std::fs::read_to_string(file)?
    } else {
        let config = config.canonicalize()?;
        let mut command = Command::new(if cfg!(windows) { "npx.cmd" } else { "npx" });
        command
            .args([
                "--no-install",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--config",
            ])
            .arg(&config)
            .args(["--command", &sql, "--json"])
            .current_dir(config.parent().ok_or("missing config parent")?);
        remote(&mut command)?
    };
    let rows = parse_inspection_json(&text)?;
    let check = validate(&rows, pre)?;
    match check.status {
        Status::Ready => {
            println!("SCHEMA_READY: canonical tables, indexes, receipt trigger DDL and isolated trigger behavior verified.");
            Ok(0)
        }
        Status::MigrationRequired => {
            println!("MIGRATION_REQUIRED: known additive warning objects or scan-fencing columns are absent.\nApply schema/ensure_warning_adjustments.sql manually (idempotent).");
            for sql in scan_migrations(&rows) {
                println!("{sql}");
            }
            println!("Apply only the missing scan ALTERs printed above, then rerun without --pre-migration.");
            Ok(3)
        }
        Status::Incompatible => {
            for e in check.errors {
                eprintln!("SCHEMA_INCOMPATIBLE: {e}");
            }
            eprintln!("STOP: investigate D1 schema; do not run full bootstrap or deploy Worker.");
            Ok(2)
        }
    }
}

fn main() {
    let code = run().unwrap_or_else(|e| {
        eprintln!("SCHEMA_CHECK_FAILED: {e}");
        2
    });
    std::process::exit(code);
}
