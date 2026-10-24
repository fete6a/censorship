use serde::Deserialize;
use serde_json::Value;
use wasm_bindgen::JsValue;
use worker::{Env, Result};
use crate::tg;
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

pub async fn finish_review(env: &Env, chat_id: i64, message_id: i64, status: &str, category: &str, probability: f64) -> Result<()> {
    let stmt = env.d1("DB")?.prepare(
        "UPDATE archived_messages SET last_status=?,last_category=?,last_probability=?,last_reviewed_at=? \
         WHERE chat_id=? AND message_id=?"
    ).bind(&[
        js_str(status), js_str(category), JsValue::from_f64(probability), js_i64(now()),
        js_i64(chat_id), js_i64(message_id),
    ])?;
    stmt.run().await?;
    let _ = tg::send_to_review_or_chat(env, chat_id, &format!(
        "message={message_id} status={status} category={category} probability={probability:.3}"
    )).await;
    Ok(())
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
    env.d1("DB")?.prepare(
        "INSERT OR IGNORE INTO violations(chat_id,user_id,message_id,created_at,category,probability,severity,action) \
         VALUES(?,?,?,?,?,?,0,'deleted')"
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

pub async fn recent_strikes(env: &Env, chat_id: i64, user_id: i64) -> Result<i64> {
    let cutoff = now() - (7 * 86400);
    let row = env.d1("DB")?.prepare(
        "SELECT COUNT(*) AS count FROM violations WHERE chat_id=? AND user_id=? AND created_at>=?"
    ).bind(&[js_i64(chat_id), js_i64(user_id), js_i64(cutoff)])?.first::<CountRow>(None).await?;
    Ok(row.map(|x| x.count).unwrap_or(0))
}

pub async fn start_scan(env: &Env, chat_id: i64, enforce: bool) -> Result<i64> {
    let row = env.d1("DB")?.prepare(
        "SELECT COALESCE(MAX(message_id),0) AS max_id FROM archived_messages WHERE chat_id=?"
    ).bind(&[js_i64(chat_id)])?.first::<MaxRow>(None).await?;
    let max_id = row.map(|r| r.max_id).unwrap_or(0);
    env.d1("DB")?.prepare(
        "INSERT INTO scan_jobs (chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status,started_at,updated_at) \
         VALUES (?,0,?,0,0,?,'running',?,?) \
         ON CONFLICT(chat_id) DO UPDATE SET cursor_message_id=0,max_message_id=excluded.max_message_id, \
         scanned=0,flagged=0,enforce=excluded.enforce,status='running',started_at=excluded.started_at,updated_at=excluded.updated_at"
    ).bind(&[js_i64(chat_id), js_i64(max_id), js_i64(i64::from(enforce)), js_i64(now()), js_i64(now())])?
        .run().await?;
    Ok(max_id)
}

pub async fn stop_scan(env: &Env, chat_id: i64) -> Result<()> {
    env.d1("DB")?.prepare("UPDATE scan_jobs SET status='stopped',updated_at=? WHERE chat_id=?")
        .bind(&[js_i64(now()),js_i64(chat_id)])?.run().await?;
    Ok(())
}

pub async fn job_for_group(env: &Env, chat_id: i64) -> Result<Option<Job>> {
    env.d1("DB")?.prepare("SELECT chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status FROM scan_jobs WHERE chat_id=?")
        .bind(&[js_i64(chat_id)])?.first::<Job>(None).await
}

pub async fn active_job(env: &Env) -> Result<Option<Job>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,cursor_message_id,max_message_id,scanned,flagged,enforce,status \
         FROM scan_jobs WHERE status='running' ORDER BY updated_at LIMIT 1"
    ).first::<Job>(None).await
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

pub async fn advance_scan(env: &Env, job: &Job, message_id: i64, flagged: bool) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE scan_jobs SET cursor_message_id=?,scanned=scanned+1,flagged=flagged+?,updated_at=? \
         WHERE chat_id=? AND status='running' AND cursor_message_id=?"
    ).bind(&[
        js_i64(message_id),js_i64(i64::from(flagged)),js_i64(now()),
        js_i64(job.chat_id),js_i64(job.cursor_message_id),
    ])?.run().await?;
    Ok(())
}

pub async fn finish_scan(env: &Env, chat_id: i64) -> Result<()> {
    env.d1("DB")?.prepare("UPDATE scan_jobs SET status='done',updated_at=? WHERE chat_id=? AND status='running'")
        .bind(&[js_i64(now()),js_i64(chat_id)])?.run().await?;
    Ok(())
}

pub async fn cleanup(env: &Env) -> Result<()> {
    env.d1("DB")?.prepare("DELETE FROM processed_updates WHERE processed_at<?")
        .bind(&[js_i64(now()-14*86400)])?.run().await?;
    env.d1("DB")?.prepare("DELETE FROM violations WHERE created_at<?")
        .bind(&[js_i64(now()-30*86400)])?.run().await?;
    let keep_days = crate::util::config(env,"ARCHIVE_DAYS").parse::<i64>().unwrap_or(0);
    if keep_days > 0 {
        env.d1("DB")?.prepare("DELETE FROM archived_messages WHERE sent_at<?")
            .bind(&[js_i64(now()-keep_days*86400)])?.run().await?;
    }
    env.d1("DB")?.prepare("DELETE FROM pending_bans WHERE status IN ('banned','canceled','failed') AND resolved_at<?")
        .bind(&[js_i64(now()-30*86400)])?.run().await?;
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

pub async fn current_pending(env: &Env, chat_id: i64, user_id: i64) -> Result<Option<PendingBan>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,user_id,message_id,created_at,due_at,status FROM pending_bans \
         WHERE chat_id=? AND user_id=? AND status='pending'"
    ).bind(&[js_i64(chat_id),js_i64(user_id)])?.first::<PendingBan>(None).await
}

pub async fn due_pending(env: &Env) -> Result<Vec<PendingBan>> {
    env.d1("DB")?.prepare(
        "SELECT chat_id,user_id,message_id,created_at,due_at,status FROM pending_bans \
         WHERE status='pending' AND due_at<=? ORDER BY due_at,chat_id,user_id LIMIT 8"
    ).bind(&[js_i64(now())])?.all().await?.results::<PendingBan>()
}

pub async fn create_pending(env: &Env, chat_id: i64, user_id: i64, message_id: i64, due_at: i64) -> Result<()> {
    env.d1("DB")?.prepare(
        "INSERT INTO pending_bans(chat_id,user_id,message_id,created_at,due_at,status) \
         VALUES(?,?,?,?,?,'pending') ON CONFLICT(chat_id,user_id) DO UPDATE SET \
         message_id=excluded.message_id,created_at=excluded.created_at,due_at=excluded.due_at, \
         status='pending',canceled_by=NULL,cancellation_reason=NULL,resolved_at=NULL \
         WHERE pending_bans.status IN ('banned','canceled','failed')"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id),js_i64(now()),js_i64(due_at)])?.run().await?;
    Ok(())
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

pub async fn retry_pending(env: &Env, chat_id: i64, user_id: i64, message_id: i64) -> Result<()> {
    env.d1("DB")?.prepare(
        "UPDATE pending_bans SET status='pending' WHERE chat_id=? AND user_id=? AND message_id=? AND status='processing'"
    ).bind(&[js_i64(chat_id),js_i64(user_id),js_i64(message_id)])?.run().await?;
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
