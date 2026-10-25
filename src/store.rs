use serde::Deserialize;
use serde_json::Value;
use wasm_bindgen::JsValue;
use worker::{Env, Result};
use crate::{formatting, tg};
use crate::util::{fail, field_int, js_i64, js_str, now};

#[derive(Debug, Deserialize)]
struct CountRow {
    count: i64,
}

#[derive(Debug, Deserialize)]
struct ArchivedRow {
    payload: String,
}

#[derive(Debug, Deserialize)]
pub struct Job {
    pub chat_id: i64,
    pub cursor_message_id: i64,
    pub max_message_id: i64,
    pub scanned: i64,
    pub flagged: i64,
    pub enforce: i64,
    pub status: String,
    pub generation: i64,
    pub lease_id: i64,
}

#[derive(Debug, Deserialize)]
struct MaxRow {
    max_id: i64,
}

pub async fn archived(env: &Env, chat_id: i64, message_id: i64) -> Result<Option<Value>> {
    let stmt = env.d1("DB")?.prepare("SELECT payload FROM archived_messages WHERE chat_id = ? AND message_id = ?")
        .bind(&[js_i64(chat_id), js_i64(message_id)])?;
    match stmt.first::<ArchivedRow>(None).await? {
        Some(row) => Ok(Some(serde_json::from_str(&row.payload)?)),
        None => Ok(None),
    }
}

pub async fn archive(env: &Env, msg: &Value) -> Result<()> {
    let (chat_id, message_id) = tg::ids(msg).ok_or_else(|| fail("Message lacks IDs"))?;
    let normalized = tg::normalize(msg);
    let payload = serde_json::to_string(&normalized)?;
    let user_id = tg::author(msg).map(js_i64).unwrap_or(JsValue::NULL);
    let sent_at = field_int(msg, "date").unwrap_or_else(now);
    let stmt = env.d1("DB")?.prepare(
        "INSERT INTO archived_messages (chat_id,message_id,user_id,sent_at,updated_at,payload,last_status) \
         VALUES (?,?,?,?,?,?,'pending') \
         ON CONFLICT(chat_id,message_id) DO UPDATE SET \
         payload=excluded.payload,updated_at=excluded.updated_at,last_status='pending'"
    ).bind(&[
        js_i64(chat_id), js_i64(message_id), user_id,
        js_i64(sent_at), js_i64(now()), js_str(&payload),
    ])?;
    stmt.run().await?;
    Ok(())
}

/// For historical scans, the archive write is atomically scoped to the
/// current run and Cron claim. Post-deletion bookkeeping is deliberately
/// unscoped because Telegram may already have performed the external action.
pub async fn finish_review_scoped(
    env: &Env, chat_id: i64, message_id: i64, status: &str,
    category: &str, probability: f64, scan: Option<&Job>
) -> Result<()> {
    let mut args = vec![
        js_str(status),js_str(category),JsValue::from_f64(probability),js_i64(now()),
        js_i64(chat_id),js_i64(message_id),
    ];
    let sql = if let Some(job)=scan {
        args.push(js_i64(job.generation));
        args.push(js_i64(job.lease_id));
        "UPDATE archived_messages SET last_status=?,last_category=?,last_probability=?,last_reviewed_at=? WHERE chat_id=? AND message_id=? AND EXISTS (SELECT 1 FROM scan_jobs WHERE scan_jobs.chat_id=archived_messages.chat_id AND generation=? AND lease_id=? AND status='running' AND lease_until>unixepoch())"
    } else {
        "UPDATE archived_messages SET last_status=?,last_category=?,last_probability=?,last_reviewed_at=? WHERE chat_id=? AND message_id=?"
    };
    env.d1("DB")?.prepare(sql).bind(&args)?.run().await?;
    Ok(())
}

/// One primary-key lookup set per group, restricted to successful Queue updates.
/// Pending/unknown rows are excluded and the latest committed edit wins.
pub async fn last_review_logs(env: &Env, chat_id: i64, message_ids: &[i64]) -> Result<Vec<formatting::ReviewLine>> {
    if chat_id>=0 || message_ids.is_empty() {return Ok(Vec::new());}
    let ids=serde_json::to_string(message_ids)?;
    env.d1("DB")?.prepare(
        "SELECT message_id,last_status AS status,last_category AS category,last_probability AS probability \
         FROM archived_messages WHERE chat_id=? AND message_id IN (SELECT value FROM json_each(?)) \
         AND last_status!='pending' ORDER BY message_id"
    ).bind(&[js_i64(chat_id),js_str(&ids)])?.all().await?.results::<formatting::ReviewLine>()
}

#[derive(Deserialize)]
struct MessageIdRow {message_id:i64}

/// Fallback navigation for private-group action logs without a public/invite URL.
pub async fn group_log_anchor(env: &Env, chat_id: i64) -> Result<Option<i64>> {
    let row=env.d1("DB")?.prepare(
        "SELECT message_id FROM archived_messages WHERE chat_id=? AND message_id>0 \
         AND last_status NOT IN ('pending','deleted','previously_deleted') ORDER BY message_id DESC LIMIT 1"
    ).bind(&[js_i64(chat_id)])?.first::<MessageIdRow>(None).await?;
    Ok(row.map(|r|r.message_id))
}

pub async fn is_update_done(env: &Env, update_id: i64) -> Result<bool> {
    let row = env.d1("DB")?.prepare("SELECT COUNT(*) AS count FROM processed_updates WHERE update_id=?")
        .bind(&[js_i64(update_id)])?.first::<CountRow>(None).await?;
    Ok(row.is_some_and(|x| x.count > 0))
}

pub async fn mark_update_done(env: &Env, update_id: i64) -> Result<()> {
    env.d1("DB")?.prepare("INSERT OR IGNORE INTO processed_updates(update_id,processed_at) VALUES (?,?)")
        .bind(&[js_i64(update_id), js_i64(now())])?.run().await?;
    Ok(())
}

pub async fn has_strike(env: &Env, chat_id: i64, message_id: i64) -> Result<bool> {
    let row = env.d1("DB")?.prepare(
        "SELECT COUNT(*) AS count FROM violations WHERE chat_id=? AND message_id=?"
    ).bind(&[js_i64(chat_id), js_i64(message_id)])?.first::<CountRow>(None).await?;
    Ok(row.is_some_and(|x| x.count > 0))
}

pub async fn add_strike(env: &Env, chat_id: i64, message_id: i64, user_id: i64, category: &str, probability: f64) -> Result<()> {
    // SQLite may reuse rowids after retention deletes the last violation.
    // Stay above this user's manual baseline even when that history is gone.
    // The allocation and insert are one statement, serialized by SQLite.
    env.d1("DB")?.prepare(
        "INSERT INTO violations(rowid,chat_id,user_id,message_id,created_at,category,probability,severity,action) \
         SELECT MAX(COALESCE((SELECT MAX(rowid) FROM violations),0), \
           COALESCE((SELECT violation_rowid FROM warning_adjustments WHERE chat_id=?1 AND user_id=?2),0))+1, \
           ?1,?2,?3,?4,?5,?6,0,'deleted' WHERE true \
         ON CONFLICT(chat_id,message_id) DO NOTHING"
    ).bind(&[
        js_i64(chat_id),js_i64(user_id),js_i64(message_id),js_i64(now()),
        js_str(category),JsValue::from_f64(probability),
    ])?.run().await?;
    Ok(())
}

pub async fn set_strike_action(env: &Env, chat_id: i64, message_id: i64, action: &str) -> Result<()> {
    env.d1("DB")?.prepare("UPDATE violations SET action=? WHERE chat_id=? AND message_id=?")
        .bind(&[js_str(action),js_i64(chat_id),js_i64(message_id)])?.run().await?;
    Ok(())
}

/// Seven-day effective warning count, including an administrator's most
/// recent explicit adjustment. The baseline is valid for seven days and
/// later violations use their SQLite rowid (rather than second-resolution
/// timestamps), so a strike in the same second is never counted twice.
pub async fn recent_strikes(env: &Env, chat_id: i64, user_id: i64) -> Result<i64> {
    if chat_id>=0 || user_id<=0 {return Err(fail("Invalid warning count scope"));}
    let cutoff = now() - (7 * 86400);
    let row = env.d1("DB")?.prepare(
        "SELECT COALESCE((SELECT base_count FROM warning_adjustments \
          WHERE chat_id=?1 AND user_id=?2 AND adjusted_at>=?3),0) + \
         (SELECT COUNT(*) FROM violations WHERE chat_id=?1 AND user_id=?2 AND created_at>=?3 \
          AND rowid>COALESCE((SELECT violation_rowid FROM warning_adjustments \
          WHERE chat_id=?1 AND user_id=?2 AND adjusted_at>=?3),0)) AS count"
    ).bind(&[js_i64(chat_id), js_i64(user_id), js_i64(cutoff)])?.first::<CountRow>(None).await?;
    Ok(row.map(|x| x.count).unwrap_or(0))
}

/// Persist an idempotent administrator adjustment. A duplicate Telegram update
/// never advances the baseline and therefore cannot erase later real strikes.
/// The SQLite trigger atomically cancels any *pending* automatic ban.
pub async fn set_warning_count(
    env: &Env, chat_id: i64, user_id: i64, count: i64, actor_id: i64, update_id: i64
) -> Result<bool> {
    if chat_id >= 0 || user_id <= 0 || actor_id <= 0 || update_id < 0 || !(0..=3).contains(&count) {
        return Err(fail("Invalid warning adjustment scope, user, count or update"));
    }
    let inserted = env.d1("DB")?.prepare(
        "INSERT INTO warning_edit_receipts(update_id,chat_id,user_id,base_count,actor_id,created_at) \
         VALUES(?,?,?,?,?,?) ON CONFLICT(update_id) DO NOTHING RETURNING update_id AS count"
    ).bind(&[js_i64(update_id),js_i64(chat_id),js_i64(user_id),
        js_i64(count),js_i64(actor_id),js_i64(now())])?.first::<CountRow>(None).await?;
    Ok(inserted.is_some())
}

/// A paused moderation request must not schedule an old ban after an
/// administrator has already reset its underlying violation. A fresh genuine
/// violation can still create a new pending task.
pub async fn create_pending_after_warning_check(
    env: &Env, chat_id: i64, user_id: i64, message_id: i64, due_at: i64, scan: Option<&Job>
) -> Result<bool> {
    let inserted = env.d1("DB")?.prepare(
        "INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at,status) \
         SELECT ?1,?2,?3,?4,?5,'pending' WHERE EXISTS ( \
           SELECT 1 FROM violations v WHERE v.chat_id=?1 AND v.user_id=?2 AND v.message_id=?3 \
           AND NOT EXISTS (SELECT 1 FROM warning_adjustments a WHERE a.chat_id=?1 \
             AND a.user_id=?2 AND a.violation_rowid>=v.rowid)) \
         AND (?6 IS NULL OR EXISTS (SELECT 1 FROM scan_jobs WHERE chat_id=?1 \
           AND generation=?6 AND lease_id=?7 AND status='running' AND lease_until>unixepoch())) \
         ON CONFLICT(chat_id,user_id) DO UPDATE SET \
           message_id=excluded.message_id,created_at=excluded.created_at,due_at=excluded.due_at, \
           status='pending',next_attempt_at=0,attempt_count=0, \
           canceled_by=NULL,cancellation_reason=NULL,resolved_at=NULL \
           WHERE pending_bans.status IN ('banned','canceled','failed') \
         RETURNING chat_id AS count"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id),
        js_i64(now()),js_i64(due_at),scan.map(|j|js_i64(j.generation)).unwrap_or(JsValue::NULL),
        scan.map(|j|js_i64(j.lease_id)).unwrap_or(JsValue::NULL)])?.first::<CountRow>(None).await?;
    Ok(inserted.is_some())
}

/// Atomically start or restart a completed/stopped scan. A running job must
/// never have its cursor or enforcement mode silently reset by another command.
/// Restart is atomic; the monotonic generation prevents stop/restart ABA.
pub async fn start_scan(env: &Env, chat_id: i64, enforce: bool) -> Result<Option<i64>> {
    let row=env.d1("DB")?.prepare(
        "SELECT COALESCE(MAX(message_id),0) AS max_id FROM archived_messages WHERE chat_id=?"
    ).bind(&[js_i64(chat_id)])?.first::<MaxRow>(None).await?;
    let max_id=row.map(|r|r.max_id).unwrap_or(0);
    let started=env.d1("DB")?.prepare(
        "INSERT INTO scan_jobs (chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,started_at,updated_at,generation,lease_id,lease_until) \
         VALUES (?,0,?,0,0,?,'running',?,?,1,0,0) \
         ON CONFLICT(chat_id) DO UPDATE SET cursor_message_id=0,max_message_id=excluded.max_message_id, \
         scanned=0,flagged=0,enforce=excluded.enforce,status='running',started_at=excluded.started_at,updated_at=excluded.updated_at, \
         generation=scan_jobs.generation+1,lease_until=0 \
         WHERE scan_jobs.status!='running' RETURNING max_message_id AS max_id"
    ).bind(&[js_i64(chat_id),js_i64(max_id),js_i64(i64::from(enforce)),js_i64(now()),js_i64(now())])?
        .first::<MaxRow>(None).await?;
    Ok(started.map(|r|r.max_id))
}

/// Admin stop invalidates any outstanding claim, even before a restart.
pub async fn stop_scan(env: &Env, chat_id: i64) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET status='stopped',generation=generation+1,lease_until=0,updated_at=? WHERE chat_id=?"
    ).bind(&[js_i64(now()),js_i64(chat_id)])?.run().await?;
    Ok(())
}

pub async fn job_for_group(env: &Env, chat_id: i64) -> Result<Option<Job>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,generation,lease_id FROM scan_jobs WHERE chat_id=?"
    ).bind(&[js_i64(chat_id)])?.first::<Job>(None).await
}

/// A single atomic claim serializes Cron runs per group. An abandoned lease
/// expires after 20 minutes; expired workers are barred from fenced writes.
pub async fn claim_active_scan(env: &Env) -> Result<Option<Job>> {
    env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET lease_id=lease_id+1,lease_until=unixepoch()+1200,updated_at=unixepoch() \
         WHERE chat_id=(SELECT chat_id FROM scan_jobs WHERE status='running' AND lease_until<=unixepoch() ORDER BY updated_at,chat_id LIMIT 1) \
         AND status='running' AND lease_until<=unixepoch() \
         RETURNING chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,generation,lease_id"
    ).first::<Job>(None).await
}

pub async fn scan_is_current(env: &Env, job: &Job) -> Result<bool> {
    let row=env.d1("DB")?.prepare(
        "SELECT COUNT(*) AS count FROM scan_jobs WHERE chat_id=? AND generation=? AND lease_id=? AND status='running' AND lease_until>unixepoch()"
    ).bind(&[js_i64(job.chat_id),js_i64(job.generation),js_i64(job.lease_id)])?
        .first::<CountRow>(None).await?;
    Ok(row.is_some_and(|r|r.count==1))
}

/// A delayed permission check cannot stop a replacement run.
pub async fn stop_scan_if_current(env: &Env, job: &Job) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET status='stopped',generation=generation+1,lease_until=0,updated_at=unixepoch() WHERE chat_id=? AND generation=? AND lease_id=? AND status='running' AND lease_until>unixepoch()"
    ).bind(&[js_i64(job.chat_id),js_i64(job.generation),js_i64(job.lease_id)])?.run().await?;
    Ok(())
}

/// On normal returns release immediately; a crashed worker relies on expiry.
pub async fn release_scan_claim(env: &Env, job: &Job) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET lease_until=0,updated_at=unixepoch() WHERE chat_id=? AND generation=? AND lease_id=? AND status='running'"
    ).bind(&[js_i64(job.chat_id),js_i64(job.generation),js_i64(job.lease_id)])?.run().await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct ArchiveItem {
    pub message_id: i64,
    pub payload: String,
}

pub async fn next_archived(env: &Env, job: &Job) -> Result<Option<ArchiveItem>> {
    env.d1("DB")?.prepare(
        "SELECT message_id,payload FROM archived_messages \
         WHERE chat_id=? AND message_id>? AND message_id<=? ORDER BY message_id ASC LIMIT 1"
    ).bind(&[
        js_i64(job.chat_id), js_i64(job.cursor_message_id), js_i64(job.max_message_id),
    ])?.first::<ArchiveItem>(None).await
}

/// CAS fences the scan generation, Cron claim and original cursor.
pub async fn advance_scan(env: &Env, job: &Job, message_id: i64, flagged: bool) -> Result<bool> {
    let updated=env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET cursor_message_id=?,scanned=scanned+1,flagged=flagged+?,updated_at=unixepoch() \
         WHERE chat_id=? AND generation=? AND lease_id=? AND status='running' AND lease_until>unixepoch() \
         AND cursor_message_id=? AND ?>cursor_message_id AND ?<=max_message_id RETURNING chat_id AS count"
    ).bind(&[
        js_i64(message_id),js_i64(i64::from(flagged)),js_i64(job.chat_id),
        js_i64(job.generation),js_i64(job.lease_id),js_i64(job.cursor_message_id),
        js_i64(message_id),js_i64(message_id),
    ])?.first::<CountRow>(None).await?;
    Ok(updated.is_some())
}

/// Exactly one claimed execution may complete and announce a given run.
pub async fn finish_scan(env: &Env, job: &Job) -> Result<bool> {
    let finished=env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET status='done',lease_until=0,updated_at=unixepoch() \
         WHERE chat_id=? AND generation=? AND lease_id=? AND status='running' AND lease_until>unixepoch() \
         AND cursor_message_id=? RETURNING chat_id AS count"
    ).bind(&[
        js_i64(job.chat_id),js_i64(job.generation),js_i64(job.lease_id),js_i64(job.cursor_message_id),
    ])?.first::<CountRow>(None).await?;
    Ok(finished.is_some())
}

/// Only an explicit zero disables archive cleanup. Invalid values must not
/// silently become unlimited retention or overflow the time arithmetic.
fn archive_cutoff(raw: &str, current_time: i64) -> Result<Option<i64>> {
    const INVALID: &str = "Invalid ARCHIVE_DAYS: expected non-negative integer (0 disables archive cleanup)";
    let days = raw.parse::<i64>().map_err(|_| fail(INVALID))?;
    if days < 0 {
        return Err(fail(INVALID));
    }
    if days == 0 {
        return Ok(None);
    }
    let seconds = days.checked_mul(86_400).ok_or_else(|| fail(INVALID))?;
    let cutoff = current_time.checked_sub(seconds).ok_or_else(|| fail(INVALID))?;
    Ok(Some(cutoff))
}

pub async fn cleanup(env: &Env) -> Result<()> {
    env.d1("DB")?.prepare("DELETE FROM processed_updates WHERE processed_at<?")
        .bind(&[js_i64(now()-14*86400)])?.run().await?;
    env.d1("DB")?.prepare("DELETE FROM violations WHERE created_at<?")
        .bind(&[js_i64(now()-30*86400)])?.run().await?;
    // Retain replay receipts longer than processed_updates (14 days) so a
    // delayed duplicate cannot shift a manual warning baseline.
    // An accidental old-schema rollout may omit either additive table.
    // Ignore only its exact missing-table failure, never unrelated D1 errors.
    let receipt_cleanup = env.d1("DB")?.prepare("DELETE FROM warning_edit_receipts WHERE created_at<?")
        .bind(&[js_i64(now()-30*86400)])?.run().await;
    if let Err(error) = receipt_cleanup {
        if error.to_string().contains("no such table: warning_edit_receipts") {
            worker::console_error!("warning_edit_receipts_missing: migrate D1 before release");
        } else {
            return Err(error);
        }
    }
    let warning_cleanup = env.d1("DB")?.prepare("DELETE FROM warning_adjustments WHERE adjusted_at<?")
        .bind(&[js_i64(now()-7*86400)])?.run().await;
    if let Err(error) = warning_cleanup {
        if error.to_string().contains("no such table: warning_adjustments") {
            worker::console_error!("warning_adjustments_missing: migrate D1 before release");
        } else {
            return Err(error);
        }
    }
    // The fixed 30-day cleanup must still run if archive policy is misconfigured.
    env.d1("DB")?.prepare("DELETE FROM pending_bans WHERE status IN ('banned','canceled','failed') AND resolved_at<?")
        .bind(&[js_i64(now()-30*86400)])?.run().await?;
    if let Some(cutoff) = archive_cutoff(&crate::util::config(env,"ARCHIVE_DAYS"), now())? {
        env.d1("DB")?.prepare("DELETE FROM archived_messages WHERE sent_at<?")
            .bind(&[js_i64(cutoff)])?.run().await?;
    }
    Ok(())
}

/// A pending suspension case is uniquely identified by chat+user+message.
/// Rechecking message_id prevents an older timer from acting on a newer case.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingBan {
    pub chat_id: i64,
    pub user_id: i64,
    pub message_id: i64,
    pub due_at: i64,
}

#[derive(Deserialize)]
struct BanStatusRow { status: String }

/// A status snapshot for honest command feedback, including in-flight claims.
/// This does not authorize changing Telegram member permissions.
pub async fn ban_case_status(env: &Env, chat_id: i64, user_id: i64) -> Result<Option<String>> {
    let row=env.d1("DB")?.prepare(
        "SELECT status FROM pending_bans WHERE chat_id=? AND user_id=?"
    ).bind(&[js_i64(chat_id),js_i64(user_id)])?.first::<BanStatusRow>(None).await?;
    Ok(row.map(|r|r.status))
}

pub async fn current_pending(env: &Env, chat_id: i64, user_id: i64) -> Result<Option<PendingBan>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,user_id,message_id,created_at,due_at,status FROM pending_bans \
         WHERE chat_id=? AND user_id=? AND status='pending'"
    ).bind(&[js_i64(chat_id),js_i64(user_id)])?.first::<PendingBan>(None).await
}

/// Fetch at most eight per Cron tick; never-attempted timers run before retries.
pub async fn due_pending(env: &Env) -> Result<Vec<PendingBan>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,user_id,message_id,created_at,due_at,status FROM pending_bans \
         WHERE status='pending' AND due_at<=?1 AND next_attempt_at<=?1 \
         ORDER BY next_attempt_at,due_at,chat_id,user_id LIMIT 8"
    ).bind(&[js_i64(now())])?.all().await?.results::<PendingBan>()
}

/// Reserve a per-case retry slot *before* Telegram calls, including getChatMember.
/// Keep status pending so admin /cancel can still win before atomic claim_due.
/// Conditional D1 update also limits overlapping Cron/Queue attempts.
/// Retry intervals are 60 seconds, then 120, capped at 240 seconds.
pub async fn begin_pending_attempt(env: &Env, chat_id: i64, user_id: i64, message_id: i64) -> Result<bool> {
    let attempted = env.d1("DB")?.prepare(
        "UPDATE pending_bans SET attempt_count=attempt_count+1, \
         next_attempt_at=unixepoch() + CASE WHEN attempt_count=0 THEN 60 \
         WHEN attempt_count=1 THEN 120 ELSE 240 END \
         WHERE chat_id=? AND user_id=? AND message_id=? AND status='pending' \
         AND due_at BETWEEN unixepoch()-600 AND unixepoch() \
         AND next_attempt_at<=unixepoch() \
         RETURNING chat_id,user_id,message_id,created_at,due_at,status"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?
      .first::<PendingBan>(None).await?;
    Ok(attempted.is_some())
}

/// A transaction-like conditional transition.  Exactly one of cancel and
/// claim_due can win, irrespective of Queue/Cron webhook interleaving.
pub async fn cancel_pending(env: &Env, chat_id: i64, user_id: i64, actor: i64, reason: &str) -> Result<Option<PendingBan>> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='canceled', canceled_by=?, cancellation_reason=?, resolved_at=? \
         WHERE chat_id=? AND user_id=? AND status='pending' \
         RETURNING chat_id,user_id,message_id,created_at,due_at,status"
    ).bind(&[js_i64(actor),js_str(reason),js_i64(now()),js_i64(chat_id),js_i64(user_id)])?
      .first::<PendingBan>(None).await
}

pub async fn cancel_pending_automatically(env: &Env, chat_id: i64, user_id: i64, message_id: i64, reason: &str) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='canceled',cancellation_reason=?,resolved_at=? \
         WHERE chat_id=? AND user_id=? AND message_id=? AND status='pending'"
    ).bind(&[js_str(reason),js_i64(now()),js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?.run().await?;
    Ok(())
}

pub async fn claim_due(env: &Env, chat_id: i64, user_id: i64, message_id: i64) -> Result<Option<PendingBan>> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='processing' WHERE chat_id=? AND user_id=? AND message_id=? \
         AND status='pending' AND due_at BETWEEN unixepoch()-600 AND unixepoch() \
         RETURNING chat_id,user_id,message_id,created_at,due_at,status"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?
      .first::<PendingBan>(None).await
}

pub async fn conclude_pending(env: &Env, chat_id: i64, user_id: i64, message_id: i64, status: &str) -> Result<()> {
    if !matches!(status, "banned" | "failed" | "canceled") { return Err(fail("Invalid pending case result")); }
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status=?,resolved_at=? WHERE chat_id=? AND user_id=? AND message_id=? AND status='processing'"
    ).bind(&[js_str(status),js_i64(now()),js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?.run().await?;
    Ok(())
}

/// Atomically recover an ambiguous Telegram ban failure only if no warning
/// adjustment covered the originating violation. Otherwise cancel the case.
/// This is one SQL update so a concurrent receipt and retry cannot resurrect it.
pub async fn retry_pending(env: &Env, chat_id: i64, user_id: i64, message_id: i64) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status=CASE WHEN EXISTS ( \
         SELECT 1 FROM warning_adjustments a JOIN violations v \
         ON v.chat_id=a.chat_id AND v.user_id=a.user_id \
         WHERE v.chat_id=pending_bans.chat_id AND v.user_id=pending_bans.user_id \
         AND v.message_id=pending_bans.message_id AND v.rowid<=a.violation_rowid) \
         THEN 'canceled' ELSE 'pending' END, \
         cancellation_reason=CASE WHEN EXISTS ( \
         SELECT 1 FROM warning_adjustments a JOIN violations v \
         ON v.chat_id=a.chat_id AND v.user_id=a.user_id \
         WHERE v.chat_id=pending_bans.chat_id AND v.user_id=pending_bans.user_id \
         AND v.message_id=pending_bans.message_id AND v.rowid<=a.violation_rowid) \
         THEN 'warning_count_adjusted' ELSE cancellation_reason END, \
         resolved_at=CASE WHEN EXISTS ( \
         SELECT 1 FROM warning_adjustments a JOIN violations v \
         ON v.chat_id=a.chat_id AND v.user_id=a.user_id \
         WHERE v.chat_id=pending_bans.chat_id AND v.user_id=pending_bans.user_id \
         AND v.message_id=pending_bans.message_id AND v.rowid<=a.violation_rowid) \
         THEN unixepoch() ELSE resolved_at END \
         WHERE chat_id=? AND user_id=? AND message_id=? AND status='processing'"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?.run().await?;
    Ok(())
}

/// Expire overdue pending rows even when backoff delays their next attempt.
/// This preserves the original strict ten-minute expiry without batch starvation.
pub async fn expire_overdue_pending(env: &Env) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='canceled', \
         cancellation_reason='timer_too_late',resolved_at=unixepoch() \
         WHERE status='pending' AND due_at<unixepoch()-600"
    ).run().await?;
    Ok(())
}

/// An interrupted ban operation may leave a processing row behind.
/// Do not re-ban after an ambiguous API failure; release these rows eventually.
pub async fn expire_stale_processing(env: &Env) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='failed',resolved_at=? \
         WHERE status='processing' AND due_at<?"
    ).bind(&[js_i64(now()),js_i64(now()-600)])?.run().await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ManagedChat {
    pub status: String,
    pub chat_type: String,
}

pub async fn managed_chat(env: &Env, chat_id: i64) -> Result<Option<ManagedChat>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,status,chat_type FROM managed_chats WHERE chat_id=?"
    ).bind(&[js_i64(chat_id)])?.first::<ManagedChat>(None).await
}

pub async fn upsert_chat(env: &Env, chat_id: i64, title: &str, chat_type: &str, status: &str, note: &str) -> Result<()> {
    if !matches!(chat_type, "group" | "supergroup") ||
       !matches!(status, "active" | "delete_only" | "paused" | "disabled") {
        return Err(fail("Invalid managed group state"));
    }
    env.d1("DB")?.prepare(
        "INSERT INTO managed_chats(chat_id,title,chat_type,status,updated_at,note) VALUES(?,?,?,?,?,?) \
         ON CONFLICT(chat_id) DO UPDATE SET title=excluded.title,chat_type=excluded.chat_type, \
         status=excluded.status,updated_at=excluded.updated_at,note=excluded.note"
    ).bind(&[
        js_i64(chat_id),js_str(title),js_str(chat_type),js_str(status),js_i64(now()),js_str(note),
    ])?.run().await?;
    // If removed or privileges are revoked, no pending automated ban should fire.
    if matches!(status,"paused"|"disabled"|"delete_only") {
        env.d1("DB")?.prepare(
            "UPDATE pending_bans SET status='canceled',cancellation_reason='bot_permissions_changed',resolved_at=? \
             WHERE chat_id=? AND status='pending'"
        ).bind(&[js_i64(now()),js_i64(chat_id)])?.run().await?;
    }
    if status == "disabled" {
        stop_scan(env,chat_id).await?;
    }
    Ok(())
}

pub async fn can_review(env: &Env, chat_id: i64) -> Result<bool> {
    Ok(managed_chat(env,chat_id).await?.is_some_and(|c|
        matches!(c.status.as_str(),"active" | "delete_only")))
}

pub async fn can_auto_ban(env: &Env, chat_id: i64) -> Result<bool> {
    Ok(managed_chat(env,chat_id).await?.is_some_and(|c|
        c.status == "active" && c.chat_type == "supergroup"))
}

#[derive(Debug, Deserialize)]
pub struct GroupSettings {
    pub dry_run: Option<i64>,
    pub logs_enabled: i64,
    pub remove_threshold: Option<f64>,
}
pub async fn group_settings(env: &Env, chat_id: i64) -> Result<Option<GroupSettings>> {
    env.d1("DB")?.prepare("SELECT dry_run,logs_enabled,remove_threshold FROM group_settings WHERE chat_id=?")
        .bind(&[js_i64(chat_id)])?.first::<GroupSettings>(None).await
}
pub async fn group_dry_run(env: &Env, chat_id: i64) -> Result<bool> {
    Ok(group_settings(env,chat_id).await?.and_then(|s|s.dry_run)
        .map(|value|value!=0).unwrap_or_else(||crate::util::flag(env,"DRY_RUN",true)))
}
pub async fn group_logs(env: &Env, chat_id: i64) -> Result<bool> {
    Ok(group_settings(env,chat_id).await?.map(|s|s.logs_enabled!=0).unwrap_or(true))
}
pub async fn group_threshold(env: &Env, chat_id: i64) -> Result<f64> {
    Ok(group_settings(env,chat_id).await?.and_then(|s|s.remove_threshold).unwrap_or(0.93))
}
pub async fn set_group_threshold(env: &Env, chat_id: i64, threshold: f64) -> Result<()> {
    if chat_id>=0 || !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(fail("Invalid group ID or threshold"));
    }
    env.d1("DB")?.prepare(
        "INSERT INTO group_settings(chat_id,remove_threshold) VALUES(?,?) ON CONFLICT(chat_id) DO UPDATE SET remove_threshold=excluded.remove_threshold"
    ).bind(&[js_i64(chat_id),JsValue::from_f64(threshold)])?.run().await?;
    Ok(())
}
pub async fn set_group_option(env: &Env, chat_id: i64, dry_run: bool, enabled: bool) -> Result<()> {
    if chat_id>=0 {return Err(fail("Invalid group ID"));}
    let sql=if dry_run {
        "INSERT INTO group_settings(chat_id,dry_run) VALUES(?,?) ON CONFLICT(chat_id) DO UPDATE SET dry_run=excluded.dry_run"
    } else {
        "INSERT INTO group_settings(chat_id,logs_enabled) VALUES(?,?) ON CONFLICT(chat_id) DO UPDATE SET logs_enabled=excluded.logs_enabled"
    };
    env.d1("DB")?.prepare(sql).bind(&[js_i64(chat_id),js_i64(i64::from(enabled))])?.run().await?;
    if dry_run && enabled {
        env.d1("DB")?.prepare("UPDATE pending_bans SET status='canceled',cancellation_reason='dry_run_enabled',resolved_at=? WHERE chat_id=? AND status='pending'")
            .bind(&[js_i64(now()),js_i64(chat_id)])?.run().await?;
    }
    Ok(())
}

#[cfg(test)]
mod archive_retention_tests {
    use super::archive_cutoff;

    #[test]
    fn explicit_zero_and_finite_days_preserve_the_configured_policy() {
        let now = 20_000_000_i64;
        assert_eq!(archive_cutoff("0", now).unwrap(), None);
        for days in [7_i64, 30, 90] {
            assert_eq!(
                archive_cutoff(&days.to_string(), now).unwrap(),
                Some(now - days * 86_400)
            );
        }
    }

    #[test]
    fn invalid_or_missing_values_cannot_silently_disable_cleanup() {
        for raw in ["", "-1", "7d", "7.5", "unlimited", "9223372036854775807"] {
            assert!(archive_cutoff(raw, 20_000_000).is_err(), "{raw}");
        }
        assert!(archive_cutoff("1", i64::MIN).is_err());
    }
}
