mod body;
mod formatting;
mod inference;
mod media;
mod moderation;
mod queueing;
mod store;
mod tg;
mod util;

use std::collections::BTreeMap;
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
    let mut reviewed: BTreeMap<i64, BTreeMap<i64, formatting::ReviewLine>>=BTreeMap::new();
    for message in batch.messages()? {
        match moderation::process_queue_item(&env, message.body()).await {
            Ok(entry) => {
                message.ack();
                if let Some((chat_id,message_id))=entry {
                    if chat_id<0 {
                        match store::last_review_log(&env,chat_id,message_id).await {
                            Ok(Some(status)) => {
                                reviewed.entry(chat_id).or_default().insert(message_id, formatting::ReviewLine{
                                    message_id, status:status.last_status,
                                    category:status.last_category, probability:status.last_probability
                                });
                            },
                            Ok(None)=>{},
                            Err(error)=>console_error!("review_digest_lookup_failed chat={} message={} error={}",
                                chat_id,message_id,error)
                        }
                    }
                }
            },
            Err(error) => {
                console_error!("queue_message_failed: {}", error);
                message.retry();
            }
        }
    }
    // One admin lookup per group and digest chunk, not one private DM per
    // allowed message. Never mix group recipients or retry completed actions.
    for (chat_id,rows) in reviewed {
        let entries=rows.into_values().collect::<Vec<_>>();
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
    // Daily cleanup, keeping retention configurable.
    if crate::util::now().div_euclid(60).rem_euclid(1440) == 0 {
        match store::cleanup(&env).await {
            Ok(()) => console_log!("cleaned_up_expired_records"),
            Err(error) => console_error!("cleanup_failed: {}", error),
        }
    }
}
