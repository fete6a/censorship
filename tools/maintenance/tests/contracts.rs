//! Source/documentation contracts complement behavioral SQLite and built-Wasm
//! tests. They do not claim to prove runtime behavior by matching source text.
mod common;
use common::*;
use serde_json::{json, Value};

fn contains(text: &str, parts: &[&str]) {
    for part in parts {
        assert!(text.contains(part), "missing contract: {part}");
    }
}
fn excludes(text: &str, parts: &[&str]) {
    for part in parts {
        assert!(!text.contains(part), "obsolete/unsafe contract: {part}");
    }
}
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    text.split_once(start)
        .expect(start)
        .1
        .split_once(end)
        .expect(end)
        .0
}

#[test]
fn ci_tests_artifacts_without_production_access_or_python() {
    let wf = source(".github/workflows/build.yml");
    contains(
        &wf,
        &[
            "actions/upload-artifact@v4",
            "sha256sum build/index.js build/index_bg.wasm",
            "node --experimental-vm-modules tests/worker_runtime.mjs",
            "cargo test --manifest-path tools/maintenance/Cargo.toml",
        ],
    );
    excludes(
        &wf,
        &[
            "  deploy:",
            "CLOUDFLARE_API_TOKEN",
            "CLOUDFLARE_ACCOUNT_ID",
            "wrangler deploy",
            "d1 execute",
            "actions/download-artifact",
            "python",
        ],
    );
    for path in [
        "scripts/deploy-ci.sh",
        ".github/workflows/deploy-once.yml",
        "migrations",
        "tools/check_d1_schema.py",
    ] {
        assert!(!root().join(path).exists());
    }
}
#[test]
fn wrangler_bindings_queue_delay_and_lto_defaults() {
    let config: Value = serde_json::from_str(&source("wrangler.jsonc")).unwrap();
    assert_eq!(config["ai"]["binding"], "AI");
    assert_eq!(config["vars"]["DRY_RUN"], "true");
    assert_eq!(config["vars"]["ARCHIVE_DAYS"], "0");
    assert_eq!(config["compatibility_date"], "2026-10-23");
    assert_eq!(
        config["queues"]["producers"][0]["binding"],
        "MODERATION_QUEUE"
    );
    assert_eq!(config["d1_databases"][0]["binding"], "DB");
    assert!(config["compatibility_flags"]
        .as_array()
        .unwrap()
        .contains(&json!("nodejs_compat")));
    let producers = config["queues"]["producers"].as_array().unwrap();
    let delayed = producers
        .iter()
        .find(|p| p["binding"] == "BAN_DELAY_QUEUE")
        .unwrap();
    assert_eq!(delayed["delivery_delay"], 60);
    assert!(config["queues"]["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["queue"] == delayed["queue"]));
    let moderation = config["queues"]["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["queue"] == "telegram-clef-moderation-events")
        .unwrap();
    assert_eq!(moderation["max_batch_size"], 20);
    assert_eq!(moderation["max_batch_timeout"], 10);
    contains(
        &source("Cargo.toml"),
        &["lto = \"fat\"", "codegen-units = 1"],
    );
}
#[test]
fn private_log_defaults_match_schema_and_both_readmes() {
    let store = source("src/store.rs");
    let group = between(
        &store,
        "pub async fn group_logs(",
        "pub async fn group_threshold(",
    );
    contains(group, &["unwrap_or(true)"]);
    contains(
        &source("schema/current.sql"),
        &["logs_enabled INTEGER NOT NULL DEFAULT 1"],
    );
    for (path, on, off) in [
        (
            "README.md",
            "**Private administrator logs are on by default.**",
            "**Private administrator logs are off by default.**",
        ),
        (
            "README_CN.md",
            "**管理员私人审核日志默认开启。**",
            "**管理员私人审核日志默认关闭。**",
        ),
    ] {
        let text = source(path);
        contains(&text, &[on]);
        excludes(&text, &[off]);
    }
}
#[test]
fn private_logs_target_current_human_admins_and_group_switch_is_documented() {
    let tg = source("src/tg.rs");
    let delivery = between(
        &tg,
        "async fn send_admin_markdown(",
        "pub async fn send_to_review_or_chat_titled(",
    );
    contains(
        delivery,
        &[
            "group_log_mode(env,chat_id)",
            "getChatAdministrators",
            "member[\"user\"][\"is_bot\"]==true",
            "post_markdown(env,user_id,",
        ],
    );
    contains(&tg, &["send_admin_markdown(env,chat_id,"]);
    excludes(
        &(source("src/store.rs") + &tg + &source("src/moderation.rs")),
        &["ADMIN_DM_CHAT_ID"],
    );
    for path in ["README.md", "README_CN.md"] {
        contains(
            &source(path),
            &[
                "`/logs off`",
                "`/logs on`",
                "`ADMIN_DM_CHAT_ID`",
                "`getChatAdministrators`",
                "`/start`",
            ],
        );
    }
    contains(&source("README.md"), &["current human administrators"]);
    contains(&source("README_CN.md"), &["当前人类管理员"]);
}
#[test]
fn group_action_notices_do_not_depend_on_private_logs() {
    let tg = source("src/tg.rs");
    let sender = between(
        &tg,
        "pub async fn reply_command_titled(",
        "async fn send_admin_markdown(",
    );
    contains(
        sender,
        &[
            "api(env,\"sendMessage\",args)",
            "post_markdown(env,chat_id,",
            "send_group_action_notice(",
        ],
    );
    excludes(sender, &["group_logs"]);
    contains(&tg, &["\"parse_mode\":\"MarkdownV2\""]);
    let moderation = source("src/moderation.rs");
    contains(
        &moderation,
        &[
            "tg::delete(env, chat_id, msg_id).await?;",
            "tg::send_group_action_notice(env,chat_id,msg_id,notice_user.as_ref(),",
            "tg::ban(env,chat_id,user_id).await",
            "tg::send_group_action_notice(env,chat_id,message_id,Some(&user),",
            "tg::reply_command_titled(env,msg,tone,title,text).await",
            "reply_msg(env,msg,",
        ],
    );
    excludes(&moderation, &["tg::send(env,origin_chat_id,"]);
    contains(
        &source("README.md"),
        &["does not disable group action notices", "`/threshold`"],
    );
    contains(
        &source("README_CN.md"),
        &["不关闭群内处罚通知", "`/threshold`"],
    );
}
#[test]
fn binary_questions_and_digest_scope_are_documented() {
    let moderation = source("src/moderation.rs");
    contains(&moderation, &["\"violation\": {", "\"type\": \"noul\""]);
    let questions = between(
        &moderation,
        "fn moderation_questions() -> Value",
        "async fn classify(",
    );
    excludes(questions, &["\"action\": {"]);
    let store = source("src/store.rs");
    contains(&store, &["last_review_log"]);
    excludes(&store, &["send_to_review_or_chat(env, chat_id, &format!("]);
    contains(&source("README.md"), &["10 seconds"]);
    contains(&source("README_CN.md"), &["10 秒"]);
}
#[test]
fn docs_describe_privacy_permissions_and_rust_release_gate() {
    for path in ["README.md", "README_CN.md"] {
        let doc = source(path);
        contains(
            &doc,
            &[
                "can_delete_messages",
                "can_restrict_members",
                "sendMessage",
                "getChatAdministrators",
                "DRY_RUN=true",
                "tools/maintenance/Cargo.toml",
                "--from-json",
                "--pre-migration",
            ],
        );
        excludes(&doc, &["python3 tools/check_d1_schema.py"]);
    }
    let en = source("README.md");
    contains(
        &en,
        &[
            "## Notifications",
            "## Administrator commands",
            "## Configuration and deployment",
            "## Security and privacy",
            "D1 archiving",
        ],
    );
    excludes(
        &en,
        &[
            "Silent by design",
            "No group notification privileges required",
            "No group warning messages",
            "the *only* unsolicited notification target",
        ],
    );
    contains(&source("README_CN.md"), &["D1 归档"]);
}
#[test]
fn warning_schema_guard_precedes_telegram_deletion_and_cleanup_propagates_errors() {
    let m = source("src/moderation.rs");
    let live = between(&m, "async fn review_with_policy_cache(", "fn command_parts");
    assert!(
        live.find("store::recent_strikes(env, chat_id, user_id).await?;")
            .unwrap()
            < live
                .find("tg::delete(env, chat_id, msg_id).await?;")
                .unwrap()
    );
    let store = source("src/store.rs");
    contains(
        &store,
        &[
            "no such table: warning_adjustments",
            "warning_adjustments_missing",
        ],
    );
    let cleanup = between(&store, "pub async fn cleanup(", "pub struct PendingBan");
    assert!(
        cleanup.find("DELETE FROM pending_bans").unwrap()
            < cleanup.find("archive_cutoff(").unwrap()
    );
    contains(cleanup, &["return Err(error)"]);
}
#[test]
fn cancel_never_calls_telegram_permission_apis() {
    let m = source("src/moderation.rs");
    let cancel = between(&m, "\"/cancel\" => {", "\"/pending\" => {");
    contains(cancel, &["store::cancel_pending("]);
    excludes(cancel, &["tg::unmute(", "tg::get_member(", "tg::api("]);
    excludes(&source("src/tg.rs"), &["pub async fn unmute("]);
}
#[test]
fn pending_attempt_is_reserved_before_lookup_and_claim_and_late_cases_expire() {
    let m = source("src/moderation.rs");
    let pending = between(
        &m,
        "pub async fn finalize_pending(",
        "pub async fn process_pending_tick",
    );
    assert!(
        pending.find("store::begin_pending_attempt(").unwrap()
            < pending.find("let member=tg::get_member").unwrap()
    );
    assert!(
        pending.find("let member=tg::get_member").unwrap()
            < pending.find("store::claim_due(").unwrap()
    );
    contains(pending, &["saturating_sub(case.due_at)>600"]);
    contains(
        &m,
        &[
            "store::expire_overdue_pending(env).await?",
            "\"timer_too_late\"",
            "\"admin_unmuted_in_telegram\"",
            "cancel_from_member_update",
            "\"administrator\" | \"creator\" | \"left\" | \"kicked\"",
            "tg::mute_one_minute(",
        ],
    );
    contains(
        &source("src/store.rs"),
        &["ORDER BY next_attempt_at,due_at,chat_id,user_id LIMIT 8"],
    );
}
#[test]
fn scans_and_command_replies_preserve_enforcement_and_lease_contracts() {
    let m = source("src/moderation.rs");
    contains(
        &m,
        &[
            "[] | [\"enforce\"] => true",
            "[\"audit\"] => false",
            "/scan_all [audit|enforce]",
            "if !store::advance_scan(env,job,item.message_id,flagged).await?",
            "if store::finish_scan(env,job).await?",
            "scan_is_current(env,job)",
            "review_with_policy_cache(env,&msg,mode,None,Some(job))",
        ],
    );
    contains(&source("README.md"), &["enforcement by default"]);
    contains(&source("README_CN.md"), &["默认请求执行处罚"]);
    for cmd in [
        "/scan",
        "/scan_id",
        "/scan_all",
        "/scan_status",
        "/scan_stop",
        "/rules",
    ] {
        contains(&m, &[cmd]);
    }
}
#[test]
fn onboarding_admin_exemption_and_error_reporting_guards() {
    let m = source("src/moderation.rs");
    contains(
        &m,
        &[
            "update.get(\"my_chat_member\")",
            "ensure_group_enrolled(env,&msg[\"chat\"])",
            "member_capabilities(&event[\"new_chat_member\"]",
            "!is_admin && store::can_auto_ban(env,chat_id).await?",
            "tg::user_is_admin(env, chat_id, user_id).await?",
            "IMMEDIATE_BAN\", false",
            "\"no_group_description\"",
            "\"review_error\"",
            "\"too_old_to_delete\"",
            "store::has_strike(env, chat_id, msg_id).await?",
        ],
    );
    contains(
        &source("src/tg.rs"),
        &[
            "pub async fn reply_command_titled(",
            "allow_sending_without_reply",
        ],
    );
    excludes(&source("src/util.rs"), &["GROUP_CHAT_ID"]);
    let config: Value = serde_json::from_str(&source("wrangler.jsonc")).unwrap();
    assert!(config["vars"].get("GROUP_CHAT_ID").is_none());
}
#[test]
fn media_transcoder_and_history_import_stay_native_rust() {
    contains(
        &source("src/media.rs"),
        &[
            "TRANSCODER_URL",
            "data:{mime};base64",
            "audio/mpeg",
            "video/mp4",
            "MAX_VIDEO",
        ],
    );
    contains(
        &source("transcoder/src/main.rs"),
        &["ffmpeg", "TRANSCODER_TOKEN"],
    );
    contains(
        &source("tools/history-import/src/main.rs"),
        &[
            "\"date_unixtime\"",
            "\"from_id\"",
            "INSERT OR IGNORE INTO archived_messages",
            "escaped_sql",
        ],
    );
}

#[test]
fn group_option_updates_are_partial_and_isolated() {
    let db = db();
    batch(&db,"INSERT INTO group_settings(chat_id,dry_run,logs_enabled,remove_threshold) VALUES(-1001,0,1,0.8); INSERT INTO group_settings(chat_id) VALUES(-1002)");
    q(&db, "set_group_log_mode", json!([-1001, 1, 1]));
    assert_eq!(sql(&db,"SELECT chat_id,dry_run,logs_enabled,logs_warn_only,remove_threshold FROM group_settings ORDER BY chat_id DESC",json!([])),json!([[-1001,0,1,1,0.8],[-1002,null,1,0,null]]));
    q(&db, "set_group_dry_run", json!([-1001, 1]));
    assert_eq!(
        sql(
            &db,
            "SELECT dry_run,logs_enabled,logs_warn_only FROM group_settings WHERE chat_id=-1001",
            json!([])
        ),
        json!([[1, 1, 1]])
    );
    q(&db, "set_group_log_mode", json!([-1001, 0, 0]));
    assert_eq!(
        sql(
            &db,
            "SELECT dry_run,logs_enabled,logs_warn_only FROM group_settings WHERE chat_id=-1001",
            json!([])
        ),
        json!([[1, 0, 0]])
    );
    q(&db, "set_group_log_mode", json!([-1001, 1, 0]));
    assert_eq!(
        sql(
            &db,
            "SELECT dry_run,logs_enabled,logs_warn_only FROM group_settings WHERE chat_id=-1001",
            json!([])
        ),
        json!([[1, 1, 0]])
    );
    assert!(try_sql(
        &db,
        "UPDATE group_settings SET logs_warn_only=2 WHERE chat_id=-1001",
        json!([])
    )
    .is_err());
}
