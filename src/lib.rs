mod body;
mod formatting;
mod inference;
mod links;
mod media;
mod moderation;
mod queueing;
mod store;
mod tg;
mod util;

use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;
use worker::{console_error, console_log, Context, Env, MessageBatch, MessageExt, Method, Request, Response, Result, ScheduleContext, ScheduledEvent};
use crate::util::{safe_equal, secret};

const MAX_UPDATE_BYTES: usize = 1024 * 1024;

#[worker::event(fetch)]
pub async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if req.path() == "/healthz" && req.method() == Method::Get {
        return Response::ok("Telegram moderation worker is running");
    }
    if req.path() != "/telegram" {
        return Response::error("Not Found", 404);
    }
    if req.method() != Method::Post {
        return Response::error("Method Not Allowed", 405);
    }

    // Do not accept unauthenticated updates. The secret must be registered with setWebhook.
    let expected = secret(&env, "WEBHOOK_SECRET")?;
    if expected.len() < 16 {
        return Response::error("Invalid webhook configuration", 500);
    }
    let actual = req.headers().get("X-Telegram-Bot-Api-Secret-Token")?.unwrap_or_default();
    if !safe_equal(&actual, &expected) {
        return Response::error("Forbidden", 403);
    }
    if let Some(content_length) = req.headers().get("Content-Length")? {
        if content_length.parse::<usize>().map(|n| n > MAX_UPDATE_BYTES).unwrap_or(true) {
            return Response::error("Payload Too Large", 413);
        }
    }
    if req.inner().body().is_none() {
        return Response::error("Bad JSON", 400);
    }
    let bytes = match body::read_limited(req.stream()?, MAX_UPDATE_BYTES).await {
        Ok(bytes) => bytes,
        Err(body::ReadError::TooLarge) => return Response::error("Payload Too Large", 413),
        Err(body::ReadError::Stream(error)) => return Err(error),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Response::error("Bad JSON", 400),
    };
    if !value["update_id"].is_i64() {
        return Response::error("Missing update_id", 400);
    }
    // The Queue decouples Telegram webhook delivery from network-bound AI inference.
    // Enqueue failure deliberately returns non-2xx, so Telegram can retry the update.
    queueing::send(&env, "MODERATION_QUEUE", &value).await?;
    Response::ok("Queued")
}

#[worker::event(queue)]
pub async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
    // Queue delivers up to 20 messages accumulated for up to 10 seconds.
    // Punitive actions remain per-message and are acknowledged independently.
    // Passive admin DMs are grouped only after the individual D1 reviews commit.
    let mut reviewed: BTreeMap<i64, BTreeSet<i64>>=BTreeMap::new();
    // A Queue delivery has a short lifetime. Reuse only successful group rule
    // lookups; ordered classification and punishment are still sequential.
    let mut policies: BTreeMap<i64, String> = BTreeMap::new();
    for message in batch.messages()? {
        match moderation::process_queue_item(&env, message.body(), &mut policies).await {
            Ok(entry) => {
                message.ack();
                if let Some((chat_id,message_id))=entry {
                    if chat_id<0 {
                        reviewed.entry(chat_id).or_default().insert(message_id);
                    }
                }
            },
            Err(error) => {
                console_error!("queue_message_failed: {}", error);
                message.retry();
            }
        }
    }
    // Read final statuses together after all edits in this batch are committed.
    // Logging-disabled groups skip these reads. Digest failures never retry actions.
    for (chat_id,ids) in reviewed {
        let entries=ids.into_iter().collect::<Vec<_>>();
        if let Err(error)=tg::send_review_digest(&env,chat_id,&entries).await {
            console_error!("review_digest_delivery_failed chat={} error={}",chat_id,error);
        }
    }
    Ok(())
}

#[worker::event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(error) = moderation::process_pending_tick(&env).await {
        console_error!("pending_ban_tick_failed: {}",error);
    }
    if let Err(error) = moderation::process_scan_tick(&env).await {
        console_error!("historical_scan_failed: {}", error);
    }
    // Claim the current UTC day even if midnight delivery or preceding work was delayed.
    // D1 records successful days and fences concurrent/overlapping Cron attempts.
    match store::claim_daily_cleanup(&env).await {
        Ok(Some(lease_id)) => match store::cleanup(&env).await {
            Ok(()) => match store::complete_daily_cleanup(&env, lease_id).await {
                Ok(true) => console_log!("cleaned_up_expired_records"),
                Ok(false) => console_error!("cleanup_completion_lost_lease"),
                Err(error) => console_error!("cleanup_completion_failed: {}", error),
            },
            Err(error) => {
                console_error!("cleanup_failed: {}", error);
                if let Err(retry_error) = store::defer_daily_cleanup(&env, lease_id).await {
                    console_error!("cleanup_retry_defer_failed: {}", retry_error);
                }
            }
        },
        Ok(None) => {},
        Err(error) => console_error!("cleanup_claim_failed: {}", error),
    }
}

