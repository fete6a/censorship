use serde_json::{json, Value};
use wasm_bindgen::JsValue;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, Result};
use crate::util::{fail, field_str, field_int, secret};

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
    let mut response = Fetch::Request(request).send().await?;
    let status = response.status_code();
    let data: Value = response.json().await?;
    if status != 200 || data["ok"] != true {
        // Never log upstream URLs because Telegram's token appears in the path.
        return Err(fail(&format!("Telegram method {method} failed (HTTP {status})")));
    }
    Ok(data.get("result").cloned().unwrap_or(Value::Null))
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

/// Silent mode: Telegram supergroup and group IDs are negative. Never attempt
/// sendMessage to a group. For private commands the target is the human user's
/// positive Telegram ID (only works after they started the bot themselves).
pub async fn send(env: &Env, chat_id: i64, text: &str) -> Result<()> {
    if chat_id <= 0 { return Ok(()); }
    api(env, "sendMessage", json!({"chat_id": chat_id, "text": text})).await?;
    Ok(())
}

/// Optional audit alerts ONLY to an explicitly configured private user chat.
/// No fallback to the moderated group, even if a configuration is invalid.
pub async fn send_to_review_or_chat(env: &Env, _chat_id: i64, text: &str) -> Result<()> {
    let target = crate::util::config(env, "ADMIN_DM_CHAT_ID").parse::<i64>().unwrap_or(0);
    if target <= 0 { return Ok(()); }
    // Notifications are optional; Telegram forbids cold-start DMs to users.
    let _ = send(env, target, text).await;
    Ok(())
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
    let mut response = Fetch::Request(Request::new(&url, Method::Get)?).send().await?;
    if response.status_code() != 200 {
        return Err(fail("Telegram file download failed"));
    }
    if let Some(length) = response.headers().get("Content-Length")? {
        if length.parse::<usize>().map(|x| x > max_len).unwrap_or(true) {
            return Err(fail("Media content-length is over limit"));
        }
    }
    let bytes = response.bytes().await?;
    if bytes.len() > max_len {
        return Err(fail("Media downloaded beyond size limit"));
    }
    Ok(bytes)
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
