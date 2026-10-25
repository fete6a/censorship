//! Execute production cleanup-claim SQL on disposable SQLite with a fake UTC clock.
mod common;
use common::*;
use serde_json::json;
use std::sync::atomic::Ordering;

const DAY: i64 = 86_400;

#[test]
fn delayed_midnight_and_slow_earlier_work_catch_up_without_duplicates() {
    let midnight = 20_000 * DAY;
    let db = db();
    let time = clock(&db, midnight + 59);
    // The original one-minute check would pass here, but not after a slow tick.
    assert_eq!((midnight + 59).div_euclid(60).rem_euclid(1_440), 0);
    time.store(midnight + 61, Ordering::SeqCst);
    assert_ne!((midnight + 61).div_euclid(60).rem_euclid(1_440), 0);
    assert_eq!(q(&db, "claim_daily_cleanup", json!([20_000])), json!([[1]]));
    assert!(empty(&q(&db, "claim_daily_cleanup", json!([20_000]))));
    assert_eq!(q(&db, "complete_daily_cleanup", json!([20_000, 1])), json!([[1]]));
    assert!(empty(&q(&db, "claim_daily_cleanup", json!([20_000]))));
    time.store(midnight + DAY + 61, Ordering::SeqCst);
    assert_eq!(q(&db, "claim_daily_cleanup", json!([20_001])), json!([[2]]));
    assert_eq!(q(&db, "complete_daily_cleanup", json!([20_001, 2])), json!([[2]]));
}

#[test]
fn failure_retries_after_backoff_and_does_not_record_success() {
    let db = db();
    let start=20_000 * DAY + 61;
    let time=clock(&db,start);
    assert_eq!(q(&db,"claim_daily_cleanup",json!([20_000])),json!([[1]]));
    // Cleanup failed after a partial series of idempotent DELETEs.
    q(&db,"defer_daily_cleanup",json!([1]));
    assert_eq!(sql(&db,"SELECT last_success_day,lease_until FROM cleanup_state",json!([])),json!([[-1,start+300]]));
    time.store(start+299,Ordering::SeqCst);
    assert!(empty(&q(&db,"claim_daily_cleanup",json!([20_000]))));
    time.store(start+300,Ordering::SeqCst);
    assert_eq!(q(&db,"claim_daily_cleanup",json!([20_000])),json!([[2]]));
    assert_eq!(q(&db,"complete_daily_cleanup",json!([20_000,2])),json!([[2]]));
    assert_eq!(sql(&db,"SELECT last_success_day FROM cleanup_state",json!([])),json!([[20_000]]));
}

#[test]
fn crash_recovery_and_concurrent_stale_claims_are_fenced() {
    let db=db();
    let start=20_000 * DAY+61;
    let time=clock(&db,start);
    assert_eq!(q(&db,"claim_daily_cleanup",json!([20_000])),json!([[1]]));
    assert!(empty(&q(&db,"claim_daily_cleanup",json!([20_000]))));
    time.store(start+1199,Ordering::SeqCst);
    assert!(empty(&q(&db,"claim_daily_cleanup",json!([20_000]))));
    time.store(start+1200,Ordering::SeqCst);
    assert_eq!(q(&db,"claim_daily_cleanup",json!([20_000])),json!([[2]]));
    assert!(empty(&q(&db,"complete_daily_cleanup",json!([20_000,1]))));
    q(&db,"defer_daily_cleanup",json!([1]));
    assert_eq!(q(&db,"complete_daily_cleanup",json!([20_000,2])),json!([[2]]));
    assert!(empty(&q(&db,"claim_daily_cleanup",json!([20_000]))));
}

#[test]
fn finishing_after_utc_midnight_credits_new_day() {
    let db=db();
    let next_day=20_001*DAY;
    let time=clock(&db,next_day-1);
    assert_eq!(q(&db,"claim_daily_cleanup",json!([20_000])),json!([[1]]));
    time.store(next_day+1,Ordering::SeqCst);
    assert_eq!(q(&db,"complete_daily_cleanup",json!([20_001,1])),json!([[1]]));
    assert!(empty(&q(&db,"claim_daily_cleanup",json!([20_001]))));
}

#[test]
fn retention_queries_and_unlimited_archive_policy_remain_unchanged() {
    let sqls=queries("cleanup");
    assert_eq!(sqls.len(),6);
    assert!(sqls[0].contains("DELETE FROM processed_updates WHERE processed_at<?"));
    assert!(sqls[1].contains("DELETE FROM violations WHERE created_at<?"));
    assert!(sqls[2].contains("DELETE FROM warning_edit_receipts WHERE created_at<?"));
    assert!(sqls[3].contains("DELETE FROM warning_adjustments WHERE adjusted_at<?"));
    assert!(sqls[4].contains("DELETE FROM pending_bans WHERE status IN"));
    assert!(sqls[5].contains("DELETE FROM archived_messages WHERE sent_at<?"));
    let src=source("src/store.rs");
    assert!(src.contains("archive_cutoff(&crate::util::config(env,\"ARCHIVE_DAYS\"), now())"));
    let cfg:serde_json::Value=serde_json::from_str(&source("wrangler.jsonc")).unwrap();
    assert_eq!(cfg["vars"]["ARCHIVE_DAYS"],"0");
}
