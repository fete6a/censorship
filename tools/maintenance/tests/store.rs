//! Execute actual production SQL against isolated SQLite with a controlled clock.
mod common;
use common::*;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::atomic::Ordering};

#[test]
fn digest_is_group_scoped_sorted_and_excludes_pending_unknown_and_duplicates() {
    let db = db();
    for (chat, mid, status) in [
        (-100, 5, "allow"),
        (-100, 6, "deleted"),
        (-100, 7, "pending"),
        (-100, 8, "needs_review"),
        (-200, 5, "deleted"),
    ] {
        sql(&db,"INSERT INTO archived_messages(chat_id,message_id,sent_at,updated_at,payload,last_status,last_category,last_probability) VALUES(?,?,100,100,'{}',?,'spam',0.95)",json!([chat,mid,status]));
    }
    assert_eq!(
        q(&db, "last_review_logs", json!([-100, "[6,5,5,7,99]"])),
        json!([[5, "allow", "spam", 0.95], [6, "deleted", "spam", 0.95]])
    );
    assert!(empty(&q(&db, "last_review_logs", json!([-100, "[]"]))));
}
#[test]
fn log_anchor_excludes_deleted_pending_and_other_groups() {
    let db = db();
    for (chat, mid, status) in [
        (-100, 10, "allow"),
        (-100, 11, "deleted"),
        (-100, 12, "pending"),
        (-200, 20, "allow"),
    ] {
        sql(&db,"INSERT INTO archived_messages(chat_id,message_id,sent_at,updated_at,payload,last_status) VALUES(?,?,100,100,'{}',?)",json!([chat,mid,status]));
    }
    assert_eq!(q(&db, "group_log_anchor", json!([-100])), json!([[10]]));
    assert!(empty(&q(&db, "group_log_anchor", json!([-300]))));
}
#[test]
fn archives_overwrite_without_duplicate_rows() {
    let db = db();
    for time in [100, 101] {
        q(
            &db,
            "archive",
            json!([-100, 15, 2, 100, time, "{\"text\":\"test\"}"]),
        );
    }
    assert_eq!(
        sql(
            &db,
            "SELECT COUNT(*),MAX(updated_at) FROM archived_messages",
            json!([])
        ),
        json!([[1, 101]])
    );
}
#[test]
fn archive_scan_lookups_are_bounded_sorted_and_group_scoped() {
    let db = db();
    for mid in [1, 10, 15, 19] {
        q(&db, "archive", json!([-100, mid, 22, 100, 100, "{}"]));
    }
    q(&db, "archive", json!([-200, 11, 22, 100, 100, "{}"]));
    assert_eq!(
        q(&db, "next_archived", json!([-100, 10, 19])),
        json!([[15, "{}"]])
    );
    assert!(empty(&q(&db, "next_archived", json!([-100, 15, 18]))));
    assert_eq!(q(&db, "archived", json!([-100, 15])), json!([["{}"]]));
    assert!(empty(&q(&db, "archived", json!([-200, 15]))));
}
#[test]
fn pending_creation_cannot_overwrite_an_existing_case() {
    let db = db();
    clock(&db, 200);
    let original = pending(&db, -100, 22, 160);
    violation(&db, -100, 22, 42, 100);
    assert!(empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 42, 200, 260, null, null])
    )));
    assert_eq!(state(&db, -100, 22), json!([original, "pending", 0, 0]));
}
#[test]
fn claims_are_atomic_at_due_and_expiry_boundaries() {
    for (time, allowed) in [(159, false), (160, true), (760, true), (761, false)] {
        let db = db();
        clock(&db, time);
        let mid = pending(&db, -100, 42, 160);
        let claimed = q(&db, "claim_due", json!([-100, 42, mid]));
        assert_eq!(!empty(&claimed), allowed, "time={time}");
        assert!(empty(&q(&db, "claim_due", json!([-100, 42, mid]))));
    }
}
#[test]
fn cancellation_wins_before_claim_but_cannot_recall_processing() {
    for claim_first in [false, true] {
        let db = db();
        clock(&db, 200);
        let mid = pending(&db, -100, 42, 160);
        if claim_first {
            assert!(!empty(&q(&db, "claim_due", json!([-100, 42, mid]))));
        }
        let canceled = q(
            &db,
            "cancel_pending",
            json!([9, "admin_command", 155, -100, 42]),
        );
        assert_eq!(empty(&canceled), claim_first);
        assert!(empty(&q(&db, "claim_due", json!([-100, 42, mid]))));
        if claim_first {
            assert_eq!(
                q(&db, "ban_case_status", json!([-100, 42])),
                json!([["processing"]])
            );
        }
    }
}
#[test]
fn due_batches_are_bounded_ordered_and_exclude_future_and_processing_cases() {
    let db = db();
    clock(&db, 160);
    for user in 1..=10 {
        pending(&db, -100, user, 150 + user);
    }
    pending(&db, -100, 20, 200);
    pending(&db, -100, 21, 140);
    batch(
        &db,
        "UPDATE pending_bans SET status='processing' WHERE user_id=21",
    );
    let rows = q(&db, "due_pending", json!([160]));
    let users: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[1].as_i64().unwrap())
        .collect();
    assert_eq!(users, (1..=8).collect::<Vec<_>>());
}
#[test]
fn threshold_updates_preserve_other_settings_and_enforce_ranges() {
    let db = db();
    batch(
        &db,
        "INSERT INTO group_settings(chat_id,dry_run,logs_enabled) VALUES(-1001,0,0)",
    );
    for (chat, threshold) in [(-1001, 0.75), (-1002, 0.9), (-1001, 0.8)] {
        q(&db, "set_group_threshold", json!([chat, threshold]));
    }
    assert_eq!(sql(&db,"SELECT chat_id,dry_run,logs_enabled,remove_threshold FROM group_settings ORDER BY chat_id DESC",json!([])),json!([[-1001,0,0,0.8],[-1002,null,1,0.9]]));
    for threshold in [-0.1, 1.01] {
        assert!(try_sql(
            &db,
            &queries("set_group_threshold")[0],
            json!([-1001, threshold])
        )
        .is_err());
    }
    assert!(try_sql(
        &db,
        "INSERT INTO group_settings(chat_id) VALUES(?)",
        json!([123])
    )
    .is_err());
}
#[test]
fn first_unrelated_setting_uses_warn_on_legacy_d1_without_changing_saved_full_mode() {
    let db=db();
    batch(&db,"INSERT INTO group_settings(chat_id,logs_enabled,logs_warn_only) VALUES(-100,1,0)");
    q(&db,"set_group_threshold",json!([-100,0.8]));
    q(&db,"set_group_dry_run",json!([-100,1]));
    assert_eq!(sql(&db,"SELECT logs_enabled,logs_warn_only FROM group_settings WHERE chat_id=-100",json!([])),json!([[1,0]]));

    q(&db,"set_group_threshold",json!([-200,0.9]));
    q(&db,"set_group_dry_run",json!([-300,0]));
    assert_eq!(sql(&db,"SELECT chat_id,logs_enabled,logs_warn_only FROM group_settings WHERE chat_id IN (-200,-300) ORDER BY chat_id",json!([])),json!([[-300,1,1],[-200,1,1]]));
}

#[test]
fn managed_chat_states_are_constrained() {
    let db = db();
    for (chat, status) in [(-1001, "active"), (-1002, "delete_only"), (-1003, "paused")] {
        sql(&db,"INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES(?,'A','supergroup',?,100)",json!([chat,status]));
    }
    assert_eq!(
        sql(
            &db,
            "SELECT COUNT(*) FROM managed_chats WHERE status IN ('active','delete_only')",
            json!([])
        ),
        json!([[2]])
    );
    assert!(try_sql(&db,"INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at) VALUES(-999,'A','group','bad',100)",json!([])).is_err());
}
#[test]
fn warning_resets_preserve_strikes_and_count_new_strikes_in_the_same_second() {
    let db = db();
    let now = 1700000000;
    violation(&db, -100, 22, 401, now - 100);
    violation(&db, -100, 22, 402, now - 10);
    assert_eq!(count(&db, -100, 22, now - 7 * 86400), 2);
    receipt(&db, 1, -100, 22, 0, now);
    assert_eq!(count(&db, -100, 22, now - 7 * 86400), 0);
    violation(&db, -100, 22, 403, now);
    assert_eq!(count(&db, -100, 22, now - 7 * 86400), 1);
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM violations", json!([])),
        json!([[3]])
    );
    assert_eq!(q(&db, "has_strike", json!([-100, 401])), json!([[1]]));
}
#[test]
fn warning_counts_are_group_and_user_scoped() {
    let db = db();
    let now = 1700000000;
    for (chat, user, mid) in [(-100, 22, 1), (-200, 22, 2), (-100, 33, 3)] {
        violation(&db, chat, user, mid, now);
    }
    receipt(&db, 1, -100, 22, 3, now);
    assert_eq!(count(&db, -100, 22, now - 100), 3);
    assert_eq!(count(&db, -200, 22, now - 100), 1);
    assert_eq!(count(&db, -100, 33, now - 100), 1);
    q(&db, "set_warning_count", json!([2, -200, 22, 0, 21, now]));
    assert_eq!(count(&db, -200, 22, now - 100), 0);
    assert_eq!(count(&db, -100, 22, now - 100), 3);
    assert_eq!(
        sql(
            &db,
            "SELECT actor_id FROM warning_adjustments WHERE chat_id=-100",
            json!([])
        ),
        json!([[11]])
    );
}
#[test]
fn expired_adjustments_do_not_mask_new_genuine_strikes() {
    let db = db();
    let now = 1700000000;
    violation(&db, -100, 22, 1, now - 60);
    receipt(&db, 1, -100, 22, 2, now);
    violation(&db, -100, 22, 2, now + 600);
    assert_eq!(count(&db, -100, 22, now - 7 * 86400), 3);
    assert_eq!(count(&db, -100, 22, now + 1), 1);
    receipt(&db, 2, -100, 22, 1, now + 601);
    violation(&db, -100, 22, 3, now + 601);
    assert_eq!(count(&db, -100, 22, now + 1), 2);
}
#[test]
fn duplicate_warning_receipt_cannot_move_the_baseline() {
    let db = db();
    let now = 1700000000;
    violation(&db, -100, 22, 401, now);
    receipt(&db, 900, -100, 22, 0, now);
    violation(&db, -100, 22, 402, now + 1);
    let before = sql(&db, "SELECT * FROM warning_adjustments", json!([]));
    receipt(&db, 900, -100, 22, 0, now + 2);
    assert_eq!(count(&db, -100, 22, now - 7 * 86400), 1);
    assert_eq!(
        sql(&db, "SELECT * FROM warning_adjustments", json!([])),
        before
    );
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM warning_edit_receipts", json!([])),
        json!([[1]])
    );
}
#[test]
fn retention_cannot_reuse_rowids_below_a_live_warning_baseline() {
    let db = db();
    batch(&db,"INSERT INTO violations(rowid,chat_id,user_id,message_id,created_at,category,probability,action) VALUES(100,-100,22,1,1,'spam',1,'warned')");
    receipt(&db, 1, -100, 22, 0, 3000000);
    batch(&db, "DELETE FROM violations WHERE created_at<100");
    q(&db, "add_strike", json!([-100, 22, 2, 3000000, "spam", 1]));
    assert_eq!(
        sql(
            &db,
            "SELECT rowid,chat_id,user_id,message_id FROM violations",
            json!([])
        ),
        json!([[101, -100, 22, 2]])
    );
    q(&db, "add_strike", json!([-200, 33, 3, 3000000, "spam", 1]));
    q(&db, "add_strike", json!([-100, 22, 2, 3000001, "spam", 1]));
    assert_eq!(count(&db, -100, 22, 2900000), 1);
    assert_eq!(
        sql(&db, "SELECT COUNT(*) FROM violations", json!([])),
        json!([[2]])
    );
}
#[test]
fn reset_fences_late_pending_creation_but_allows_new_violations() {
    let db = db();
    for mid in [401, 402, 403] {
        violation(&db, -100, 22, mid, 100);
    }
    receipt(&db, 1, -100, 22, 0, 100);
    assert!(empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 403, 101, 161, null, null])
    )));
    violation(&db, -100, 22, 404, 101);
    assert!(!empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 404, 101, 161, null, null])
    )));
}
#[test]
fn reset_cancels_only_pending_cases_in_its_group() {
    let db = db();
    for (chat, mid) in [(-100, 500), (-200, 600)] {
        violation(&db, chat, 22, mid, 100);
        pending(&db, chat, 22, 160);
    }
    receipt(&db, 1, -100, 22, 0, 100);
    assert_eq!(
        sql(
            &db,
            "SELECT chat_id,status FROM pending_bans ORDER BY chat_id DESC",
            json!([])
        ),
        json!([[-100, "canceled"], [-200, "pending"]])
    );
    assert!(empty(&sql(
        &db,
        "SELECT 1 FROM warning_adjustments WHERE chat_id=-200",
        json!([])
    )));
}
#[test]
fn warning_constraints_and_idempotent_additive_setup() {
    let db = db();
    for (chat, user, count, actor) in [
        (1, 22, 2, 11),
        (-100, -22, 2, 11),
        (-100, 22, 4, 11),
        (-100, 22, -1, 11),
        (-100, 22, 2, -1),
    ] {
        assert!(try_sql(
            &db,
            &queries("set_warning_count")[0],
            json!([5000, chat, user, count, actor, 100])
        )
        .is_err());
    }
    batch(&db, censorship_maintenance::ENSURE_WARNINGS);
    batch(&db, censorship_maintenance::ENSURE_WARNINGS);
}

#[derive(Default)]
struct Telegram {
    fail_lookup: BTreeSet<(i64, i64)>,
    fail_ban: BTreeSet<(i64, i64)>,
    banned: Vec<(i64, i64)>,
    ban_calls: usize,
}
fn tick(db: &rusqlite::Connection, time: i64, tg: &mut Telegram) -> Vec<(i64, i64)> {
    let rows = q(db, "due_pending", json!([time]));
    assert!(rows.as_array().unwrap().len() <= 8);
    let mut keys = Vec::new();
    for row in rows.as_array().unwrap() {
        let (chat, user, mid) = (
            row[0].as_i64().unwrap(),
            row[1].as_i64().unwrap(),
            row[2].as_i64().unwrap(),
        );
        let key = (chat, user);
        keys.push(key);
        if empty(&q(db, "begin_pending_attempt", json!([chat, user, mid])))
            || tg.fail_lookup.contains(&key)
        {
            continue;
        }
        if empty(&q(db, "claim_due", json!([chat, user, mid]))) {
            continue;
        }
        tg.ban_calls += 1;
        if tg.fail_ban.contains(&key) {
            q(db, "retry_pending", json!([chat, user, mid]));
        } else {
            tg.banned.push(key);
            q(
                db,
                "conclude_pending",
                json!(["banned", time, chat, user, mid]),
            );
        }
    }
    keys
}
#[test]
fn first_eight_poison_cases_do_not_starve_later_four_then_recover_once() {
    let db = db();
    let time = clock(&db, 200);
    let mut tg = Telegram::default();
    for u in 1..=8 {
        pending(&db, -200, u, 180);
        tg.fail_lookup.insert((-200, u));
    }
    for u in 1..=4 {
        pending(&db, -100, u, 180);
    }
    assert_eq!(
        tick(&db, 200, &mut tg),
        (1..=8).map(|u| (-200, u)).collect::<Vec<_>>()
    );
    assert_eq!(
        tick(&db, 200, &mut tg),
        (1..=4).map(|u| (-100, u)).collect::<Vec<_>>()
    );
    assert!(tick(&db, 200, &mut tg).is_empty());
    time.store(259, Ordering::SeqCst);
    assert!(tick(&db, 259, &mut tg).is_empty());
    time.store(260, Ordering::SeqCst);
    tg.fail_lookup.clear();
    assert_eq!(tick(&db, 260, &mut tg).len(), 8);
    assert_eq!(tg.banned.len(), 12);
    assert_eq!(tg.banned.iter().collect::<BTreeSet<_>>().len(), 12);
    assert!(tick(&db, 260, &mut tg).is_empty());
}
#[test]
fn failed_ban_backs_off_then_succeeds_without_duplicate_ban() {
    let db = db();
    let time = clock(&db, 200);
    let mid = pending(&db, -200, 7, 180);
    let mut tg = Telegram::default();
    tg.fail_ban.insert((-200, 7));
    tick(&db, 200, &mut tg);
    assert_eq!(state(&db, -200, 7), json!([mid, "pending", 1, 260]));
    time.store(259, Ordering::SeqCst);
    assert!(tick(&db, 259, &mut tg).is_empty());
    time.store(260, Ordering::SeqCst);
    tick(&db, 260, &mut tg);
    assert_eq!(state(&db, -200, 7), json!([mid, "pending", 2, 380]));
    time.store(380, Ordering::SeqCst);
    tg.fail_ban.clear();
    tick(&db, 380, &mut tg);
    assert_eq!(state(&db, -200, 7)[1], "banned");
    assert_eq!(tg.banned, vec![(-200, 7)]);
    assert_eq!(tg.ban_calls, 3);
    time.store(620, Ordering::SeqCst);
    assert!(tick(&db, 620, &mut tg).is_empty());
}
#[test]
fn attempt_reservations_cancel_stale_timers_and_replacements_are_scoped() {
    let db = db();
    clock(&db, 200);
    let mid = pending(&db, -200, 7, 180);
    let other = pending(&db, -100, 7, 180);
    assert!(!empty(&q(
        &db,
        "begin_pending_attempt",
        json!([-200, 7, mid])
    )));
    assert!(empty(&q(
        &db,
        "begin_pending_attempt",
        json!([-200, 7, mid])
    )));
    assert!(!empty(&q(
        &db,
        "cancel_pending",
        json!([99, "admin_command", 200, -200, 7])
    )));
    assert!(empty(&q(&db, "claim_due", json!([-200, 7, mid]))));
    assert_eq!(state(&db, -100, 7)[1], "pending");
    assert!(!empty(&q(
        &db,
        "begin_pending_attempt",
        json!([-100, 7, other])
    )));
    assert!(!empty(&q(&db, "claim_due", json!([-100, 7, other]))));
    assert!(empty(&q(&db, "claim_due", json!([-100, 7, other]))));
    violation(&db, -200, 7, mid + 1, 200);
    q(
        &db,
        "create_pending_after_warning_check",
        json!([-200, 7, mid + 1, 200, 200, null, null]),
    );
    assert_eq!(state(&db, -200, 7), json!([mid + 1, "pending", 0, 0]));
    assert!(empty(&q(
        &db,
        "begin_pending_attempt",
        json!([-200, 7, mid])
    )));
    assert!(!empty(&q(
        &db,
        "begin_pending_attempt",
        json!([-200, 7, mid + 1])
    )));
}
#[test]
fn timeout_cleanup_ignores_retry_backoff_and_respects_strict_boundary() {
    let db = db();
    let time = clock(&db, 200);
    let mid = pending(&db, -200, 7, 180);
    let mut tg = Telegram::default();
    tg.fail_lookup.insert((-200, 7));
    for (now, attempt, next) in [(200, 1, 260), (260, 2, 380), (380, 3, 620), (620, 4, 860)] {
        time.store(now, Ordering::SeqCst);
        tick(&db, now, &mut tg);
        assert_eq!(state(&db, -200, 7), json!([mid, "pending", attempt, next]));
    }
    time.store(780, Ordering::SeqCst);
    q(&db, "expire_overdue_pending", json!([]));
    assert_eq!(state(&db, -200, 7)[1], "pending");
    time.store(781, Ordering::SeqCst);
    q(&db, "expire_overdue_pending", json!([]));
    assert_eq!(state(&db, -200, 7)[1], "canceled");
    assert_eq!(
        sql(
            &db,
            "SELECT cancellation_reason FROM pending_bans",
            json!([])
        ),
        json!([["timer_too_late"]])
    );
    assert!(tick(&db, 781, &mut tg).is_empty());
}
#[test]
fn receipts_fence_failed_ban_retry_in_either_interleaving_and_only_one_group() {
    for receipt_first in [false, true] {
        let db = db();
        clock(&db, 200);
        let mid = pending(&db, -200, 7, 180);
        let other = pending(&db, -100, 7, 180);
        for (chat, mid) in [(-200, mid), (-100, other)] {
            violation(&db, chat, 7, mid, 100);
            q(&db, "claim_due", json!([chat, 7, mid]));
        }
        if receipt_first {
            receipt(&db, 801, -200, 7, 0, 200);
            assert_eq!(state(&db, -200, 7)[1], "processing");
        }
        q(&db, "retry_pending", json!([-200, 7, mid]));
        q(&db, "retry_pending", json!([-100, 7, other]));
        if !receipt_first {
            assert_eq!(state(&db, -200, 7)[1], "pending");
            receipt(&db, 801, -200, 7, 0, 200);
        }
        assert_eq!(state(&db, -200, 7)[1], "canceled");
        assert_eq!(state(&db, -100, 7)[1], "pending");
        assert!(empty(&q(&db, "claim_due", json!([-200, 7, mid]))));
        assert_eq!(
            sql(
                &db,
                "SELECT cancellation_reason FROM pending_bans WHERE chat_id=-200",
                json!([])
            ),
            json!([["warning_count_adjusted"]])
        );
    }
}

fn start(db: &rusqlite::Connection, chat: i64, enforce: i64, time: i64) -> Value {
    sql(
        db,
        &queries("start_scan")[1],
        json!([chat, 42, enforce, time, time]),
    )
}
fn claim(db: &rusqlite::Connection) -> Value {
    q(db, "claim_active_scan", json!([]))
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .unwrap_or(Value::Null)
}
fn scope(job: &Value) -> Value {
    json!([job[0], job[7], job[8]])
}
fn advance(db: &rusqlite::Connection, job: &Value, mid: i64) -> Value {
    q(
        db,
        "advance_scan",
        json!([mid, 1, job[0], job[7], job[8], job[1], mid, mid]),
    )
}
fn finish(db: &rusqlite::Connection, job: &Value, cursor: i64) -> Value {
    q(db, "finish_scan", json!([job[0], job[7], job[8], cursor]))
}
#[test]
fn expired_claim_cannot_advance_finish_stop_or_release_its_successor() {
    let db = db();
    let time = clock(&db, 100);
    start(&db, -100, 1, 100);
    let old = claim(&db);
    assert!(claim(&db).is_null());
    time.store(1300, Ordering::SeqCst);
    let new = claim(&db);
    assert!(new[8].as_i64() > old[8].as_i64());
    assert_eq!(q(&db, "scan_is_current", scope(&old)), json!([[0]]));
    assert!(empty(&advance(&db, &old, 42)));
    assert!(empty(&finish(&db, &old, 0)));
    q(&db, "stop_scan_if_current", scope(&old));
    q(&db, "release_scan_claim", scope(&old));
    assert_eq!(q(&db, "scan_is_current", scope(&new)), json!([[1]]));
    assert!(!empty(&advance(&db, &new, 42)));
    assert!(!empty(&finish(&db, &new, 42)));
    assert!(empty(&finish(&db, &new, 42)));
}
#[test]
fn stop_restart_fences_old_completion_and_pending_bans_but_allows_fresh_and_live() {
    let db = db();
    clock(&db, 100);
    start(&db, -100, 1, 100);
    let old = claim(&db);
    violation(&db, -100, 22, 42, 100);
    q(&db, "stop_scan", json!([100, -100]));
    assert!(!empty(&start(&db, -100, 0, 100)));
    let new = claim(&db);
    assert_ne!(old[7], new[7]);
    assert!(empty(&advance(&db, &old, 42)));
    assert!(empty(&finish(&db, &old, 0)));
    assert!(empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 42, 100, 160, old[7], old[8]])
    )));
    assert!(!empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 42, 100, 160, new[7], new[8]])
    )));
    batch(&db, "DELETE FROM pending_bans");
    assert!(!empty(&q(
        &db,
        "create_pending_after_warning_check",
        json!([-100, 22, 42, 100, 160, null, null])
    )));
    assert_eq!(
        sql(
            &db,
            "SELECT cursor_message_id,scanned,flagged,enforce,status FROM scan_jobs",
            json!([])
        ),
        json!([[0, 0, 0, 0, "running"]])
    );
}
#[test]
fn two_chats_claim_independently_and_duplicate_start_preserves_progress_and_mode() {
    let db = db();
    clock(&db, 100);
    start(&db, -100, 1, 100);
    batch(
        &db,
        "UPDATE scan_jobs SET cursor_message_id=7,scanned=3 WHERE chat_id=-100",
    );
    assert!(empty(&start(&db, -100, 0, 100)));
    assert_eq!(
        sql(
            &db,
            "SELECT cursor_message_id,scanned,enforce FROM scan_jobs",
            json!([])
        ),
        json!([[7, 3, 1]])
    );
    start(&db, -200, 0, 100);
    let a = claim(&db);
    let b = claim(&db);
    assert_ne!(a[0], b[0]);
    assert!(claim(&db).is_null());
    q(&db, "release_scan_claim", scope(&a));
    assert!(!claim(&db).is_null());
}
#[test]
fn historical_review_status_is_fenced_after_stop_and_restart() {
    // This query is chosen dynamically in Rust; the JS/Wasm tests exercise its
    // binding path. Here execute the literal from that branch, parsed with syn.
    let db = db();
    clock(&db, 100);
    q(&db, "archive", json!([-100, 42, 22, 100, 100, "{}"]));
    start(&db, -100, 1, 100);
    let old = claim(&db);
    q(&db, "stop_scan", json!([100, -100]));
    start(&db, -100, 0, 100);
    let new = claim(&db);
    let file = syn::parse_file(&source("src/store.rs")).unwrap();
    struct Literals(Vec<String>);
    impl<'a> syn::visit::Visit<'a> for Literals {
        fn visit_lit_str(&mut self, s: &'a syn::LitStr) {
            if s.value()
                .starts_with("UPDATE archived_messages SET last_status=?")
            {
                self.0.push(s.value());
            }
        }
    }
    use syn::visit::Visit;
    let mut literals = Literals(Vec::new());
    literals.visit_file(&file);
    let query = literals.0.iter().find(|q| q.contains("EXISTS")).unwrap();
    sql(
        &db,
        query,
        json!(["deleted", "spam", 1, 100, -100, 42, old[7], old[8]]),
    );
    assert_eq!(
        sql(&db, "SELECT last_status FROM archived_messages", json!([])),
        json!([["pending"]])
    );
    sql(
        &db,
        query,
        json!(["audit_flag", "spam", 1, 100, -100, 42, new[7], new[8]]),
    );
    assert_eq!(
        sql(&db, "SELECT last_status FROM archived_messages", json!([])),
        json!([["audit_flag"]])
    );
}
