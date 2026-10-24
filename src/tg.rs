use serde_json::{json, Value};
use wasm_bindgen::JsValue;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, Result};
use crate::util::{fail, field_str, field_int, secret};
use crate::body::response_bytes;
use crate::formatting;

pub async fn api(env: &Env, method: &str, args: Value) -> Result<Value> {
    let token = secret(env, "BOT_TOKEN")?;
    let url = format!("https://api.telegram.org/bot{token}/{method}");
    let payload = serde_json::to_string(&args)?;
    let mut init = RequestInit::new();
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    init.with_method(Method::Post);
    init.with_headers(headers);
    init.with_body(Some(JsValue::from_str(&payload)));
    let request = Request::new_with_init(&url, &init)?;
    let mut response = Fetch::Request(request).send().await
        .map_err(|_| fail(&format!("Telegram method {method} network request failed")))?;
    let status = response.status_code();
    let bytes = response_bytes(&mut response, 1024 * 1024).await
        .map_err(|_| fail(&format!("Telegram method {method} response could not be read")))?;
    let mut data: Value = serde_json::from_slice(&bytes)
        .map_err(|_| fail(&format!("Telegram method {method} returned invalid JSON")))?;
    if status != 200 || data["ok"] != true {
        // Never log upstream URLs because Telegram's token appears in the path.
        return Err(fail(&format!("Telegram method {method} failed (HTTP {status})")));
    }
    Ok(data["result"].take())
}

pub async fn group_rules(env: &Env, chat_id: i64) -> Result<String> {
    let info = api(env, "getChat", json!({"chat_id": chat_id})).await?;
    let description = field_str(&info, "description").trim();
    // Telegram's group description is limited to 255 characters.
    Ok(description.chars().take(1024).collect())
}

pub async fn user_is_admin(env: &Env, chat_id: i64, user_id: i64) -> Result<bool> {
    let member = api(env, "getChatMember", json!({"chat_id": chat_id, "user_id": user_id})).await?;
    Ok(matches!(field_str(&member, "status"), "creator" | "administrator"))
}

/// Only for command authorization: inaccessible/nonexistent target groups
/// cause terminal Telegram HTTP 400/403 errors that must not fill the DLQ.
/// Leave moderation lookups strict and keep 429/5xx/network errors retryable.
pub async fn user_is_admin_for_command(env: &Env, chat_id: i64, user_id: i64) -> Result<bool> {
    let member = match api(env, "getChatMember", json!({"chat_id": chat_id, "user_id": user_id})).await {
        Ok(member) => member,
        Err(error) => {
            let detail = error.to_string();
            if detail.contains("Telegram method getChatMember failed (HTTP 400)") ||
               detail.contains("Telegram method getChatMember failed (HTTP 403)") {
                return Ok(false);
            }
            return Err(error);
        }
    };
    Ok(matches!(field_str(&member, "status"), "creator" | "administrator"))
}

/// One safe Telegram MarkdownV2 sender for commands, digests and notices.
async fn post_markdown(env: &Env, chat_id: i64, markdown: &str) -> Result<()> {
    api(env, "sendMessage", json!({
        "chat_id":chat_id, "text":markdown, "parse_mode":"MarkdownV2",
        "link_preview_options":{"is_disabled":true}
    })).await?;
    Ok(())
}

/// Private command replies; dynamic text is escaped before adding formatting.
pub async fn send(env: &Env, chat_id: i64, text: &str) -> Result<()> {
    if chat_id <= 0 { return Ok(()); }
    post_markdown(env,chat_id,&formatting::plain_notice("机器人管理",text)).await
}

/// Public group messages are best effort and never reissue completed punishment.
pub async fn send_group_notice(env: &Env, chat_id: i64, text: &str) {
    if chat_id>=0 {return;}
    if post_markdown(env,chat_id,&formatting::plain_notice("群组通知",text)).await.is_err() {
        worker::console_error!("group_notice_delivery_failed");
    }
}

/// User mention links are generated solely from positive Telegram numeric IDs.
/// Deleted message IDs remain plain text: they cannot reliably be opened.
pub async fn send_group_action_notice(
    env: &Env, chat_id: i64, message_id: i64, user_id: Option<i64>,
    title: &str, text: &str
) {
    if chat_id>=0 {return;}
    let body=formatting::action_notice(title,message_id,user_id,text);
    if post_markdown(env,chat_id,&body).await.is_err() {
        worker::console_error!("group_action_notice_delivery_failed");
    }
}

/// Fetch current human administrators for the exact group; no global DM target.
async fn send_admin_markdown(env: &Env, chat_id: i64, markdowns: &[String]) -> Result<()> {
    if chat_id>=0 || !crate::store::group_logs(env,chat_id).await? {return Ok(());}
    let admins=match api(env,"getChatAdministrators",json!({"chat_id":chat_id})).await {
        Ok(value)=>value,
        Err(_)=>{worker::console_error!("group_admin_logs_unavailable");return Ok(());}
    };
    if let Some(admins)=admins.as_array() {
        for member in admins {
            if !matches!(field_str(member,"status"),"creator"|"administrator") || member["user"]["is_bot"]==true {continue;}
            if let Some(user_id)=field_int(&member["user"],"id").filter(|id|*id>0) {
                for markdown in markdowns {
                    if post_markdown(env,user_id,markdown).await.is_err() {
                        worker::console_error!("group_admin_log_dm_failed");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Enforcement, cancellation and scan notifications are independent of the
/// compact passive review digests.
pub async fn send_to_review_or_chat(env: &Env, chat_id: i64, text: &str) -> Result<()> {
    let display=format!("群组 {}\n{}",chat_id,text);
    send_admin_markdown(env,chat_id,&[formatting::plain_notice("管理员通知",&display)]).await
}

/// Queue batches group successful reviews by originating chat. A single admin
/// lookup is shared across all chunks, not repeated per allowed message.
pub async fn send_review_digest(env: &Env, chat_id: i64, message_ids: &[i64]) -> Result<()> {
    if chat_id>=0 || message_ids.is_empty() || !crate::store::group_logs(env,chat_id).await? {return Ok(());}
    let rows=crate::store::last_review_logs(env,chat_id,message_ids).await?;
    if rows.is_empty() {return Ok(());}
    // Recheck logs immediately before delivery; settings may change during D1 reads.
    send_admin_markdown(env,chat_id,&formatting::review_digest(chat_id,&rows)).await
}

pub async fn delete(env: &Env, chat_id: i64, message_id: i64) -> Result<()> {
    api(env, "deleteMessage", json!({"chat_id": chat_id, "message_id": message_id})).await?;
    Ok(())
}

/// Mute for exactly one minute; this restriction auto-expires even if all
/// Worker background jobs fail. Telegram requires a supergroup and ban rights.
pub async fn mute_one_minute(env: &Env, chat_id: i64, user_id: i64) -> Result<i64> {
    let due_at = crate::util::now() + 60;
    api(env, "restrictChatMember", json!({
        "chat_id": chat_id,
        "user_id": user_id,
        "until_date": due_at,
        "use_independent_chat_permissions": true,
        "permissions": {
            "can_send_messages": false,
            "can_send_audios": false,
            "can_send_documents": false,
            "can_send_photos": false,
            "can_send_videos": false,
            "can_send_video_notes": false,
            "can_send_voice_notes": false,
            "can_send_polls": false,
            "can_send_other_messages": false,
            "can_add_web_page_previews": false,
            "can_invite_users": false,
            "can_change_info": false,
            "can_pin_messages": false,
            "can_manage_topics": false
        }
    })).await?;
    Ok(due_at)
}

/// Remove the restriction after an explicit administrator cancellation.
/// Restore the group's default permissions instead of opening disallowed actions.
pub async fn unmute(env: &Env, chat_id: i64, user_id: i64) -> Result<()> {
    let chat = api(env, "getChat", json!({"chat_id":chat_id})).await?;
    let defaults = chat.get("permissions").filter(|v| v.is_object()).cloned().unwrap_or(json!({
        "can_send_messages": true,
        "can_send_audios": true,
        "can_send_documents": true,
        "can_send_photos": true,
        "can_send_videos": true,
        "can_send_video_notes": true,
        "can_send_voice_notes": true,
        "can_send_polls": true,
        "can_send_other_messages": true,
        "can_add_web_page_previews": true,
        "can_invite_users": true
    }));
    api(env,"restrictChatMember",json!({
        "chat_id":chat_id,"user_id":user_id,
        "permissions":defaults,"use_independent_chat_permissions":true
    })).await?;
    Ok(())
}

pub async fn get_member(env: &Env, chat_id: i64, user_id: i64) -> Result<Value> {
    api(env, "getChatMember", json!({"chat_id":chat_id,"user_id":user_id})).await
}

pub async fn ban(env: &Env, chat_id: i64, user_id: i64) -> Result<()> {
    api(env, "banChatMember", json!({
        "chat_id": chat_id,
        "user_id": user_id,
        "revoke_messages": false
    })).await?;
    Ok(())
}

pub async fn download(env: &Env, file_id: &str, max_len: usize) -> Result<Vec<u8>> {
    let file = api(env, "getFile", json!({"file_id": file_id})).await?;
    if let Some(size) = field_int(&file, "file_size") {
        if size < 0 || size as usize > max_len {
            return Err(fail("Telegram media exceeds moderation size limit"));
        }
    }
    let path = field_str(&file, "file_path");
    if path.is_empty() || path.starts_with('/') || path.contains("..") || path.contains('?') || path.contains('#') {
        return Err(fail("Telegram returned an invalid file path"));
    }
    let token = secret(env, "BOT_TOKEN")?;
    let url = format!("https://api.telegram.org/file/bot{token}/{path}");
    let mut response = Fetch::Request(Request::new(&url, Method::Get)?).send().await
        .map_err(|_| fail("Telegram file download network request failed"))?;
    if response.status_code() != 200 {
        return Err(fail("Telegram file download failed"));
    }
    response_bytes(&mut response, max_len).await
        .map_err(|_| fail("Telegram file download failed or exceeded its size limit"))
}

pub fn normalize(msg: &Value) -> Value {
    let keys = ["text", "caption", "photo", "video", "video_note", "voice", "audio", "document", "animation", "sticker"];
    let mut item = json!({
        "message_id": msg["message_id"],
        "chat": {"id": msg["chat"]["id"], "type": msg["chat"]["type"]},
        "date": msg["date"],
        "from": {"id": msg["from"]["id"], "is_bot": msg["from"]["is_bot"]},
        "sender_chat": msg["sender_chat"]
    });
    for key in keys {
        if let Some(value) = msg.get(key) {
            if !value.is_null() {
                // Telegram metadata only (file IDs, dimensions, mime, etc.), no raw media.
                item[key] = value.clone();
            }
        }
    }
    item
}

pub fn ids(msg: &Value) -> Option<(i64, i64)> {
    Some((field_int(&msg["chat"], "id")?, field_int(msg, "message_id")?))
}

pub fn author(msg: &Value) -> Option<i64> {
    // sender_chat denotes a chat/channel/anonymous administrator speaking as a chat.
    // Telegram may supply a fake `from` user in these cases. Never ban that identity.
    if !msg["sender_chat"].is_null() {
        return None;
    }
    field_int(&msg["from"], "id")
}

pub fn is_eligible_group_message(msg: &Value) -> bool {
    let Some((_, _)) = ids(msg) else { return false; };
    let chat_type = field_str(&msg["chat"], "type");
    (chat_type == "group" || chat_type == "supergroup")
        && msg["from"]["is_bot"] != true
        && (author(msg).is_some() || !msg["sender_chat"].is_null())
}

pub fn is_eligible(msg: &Value, group_id: i64) -> bool {
    is_eligible_group_message(msg) && ids(msg).is_some_and(|(id,_)| id==group_id)
}

pub fn message_has_payload(msg: &Value) -> bool {
    if !field_str(msg, "text").trim().is_empty() || !field_str(msg, "caption").trim().is_empty() {
        return true;
    }
    ["photo", "video", "video_note", "voice", "audio", "document", "animation", "sticker"]
        .iter().any(|k| msg.get(k).is_some_and(|v| !v.is_null()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_media_only_and_eligible() {
        let msg=json!({"message_id":123,"chat":{"id":-1009,"type":"supergroup"},"from":{"id":123,"is_bot":false},"photo":[{"file_id":"abc"}]});
        assert!(is_eligible(&msg, -1009));
        assert!(message_has_payload(&msg));
        assert!(!is_eligible(&msg, -1008));
        assert_eq!(normalize(&msg)["photo"][0]["file_id"], "abc");
    }
    #[test]
    fn anonymous_admin_fake_from_is_not_bannable() {
        let msg=json!({"message_id":5,"chat":{"id":-1009,"type":"supergroup"},
            "from":{"id":1087968824,"is_bot":false},"sender_chat":{"id":-1009},"text":"hello"});
        assert!(is_eligible(&msg,-1009));
        assert_eq!(author(&msg),None);
    }
}

