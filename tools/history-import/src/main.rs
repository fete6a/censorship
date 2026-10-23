//! Converts Telegram Desktop `result.json` text export to D1 SQL imports.
//! Offline/local-only: does not contact Telegram or Cloudflare.
//! Use `wrangler d1 execute --remote --file path` to upload SQL batches.
use serde_json::{json, Value};
use std::{env, error::Error, fs, path::{Path, PathBuf}};

const BATCH_SIZE: usize = 250;

fn escaped_sql(s: &str) -> String {
    s.replace('\'', "''")
}

fn as_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    let Some(items) = v.as_array() else { return String::new() };
    items.iter().map(|part| {
        if let Some(s) = part.as_str() { s.to_owned() }
        else { part.get("text").and_then(Value::as_str).unwrap_or("").to_owned() }
    }).collect::<Vec<_>>().join("")
}

fn author_id(v: &Value) -> Option<i64> {
    if let Some(id) = v.as_i64() { return Some(id); }
    let s = v.as_str()?;
    s.strip_prefix("user")?.parse().ok()
}

fn normalized(chat_id: i64, msg: &Value) -> Option<(i64, i64, i64, Value)> {
    if msg["type"].as_str()? != "message" { return None; }
    let message_id = msg["id"].as_i64()?;
    let date = msg["date_unixtime"].as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| msg["date_unixtime"].as_i64())?;
    let user_id = author_id(&msg["from_id"])?;
    if message_id <= 0 || date <= 0 { return None; }
    let text = as_text(&msg["text"]);
    // Telegram Desktop media paths are local paths, NOT Telegram Bot API file_id.
    // We only import textual content to prevent falsely claiming that media was scanned.
    if text.trim().is_empty() { return None; }
    Some((message_id, date, user_id, json!({
        "message_id": message_id,
        "chat": {"id": chat_id,"type":"supergroup"},
        "date": date,
        "from": {"id": user_id,"is_bot": false},
        "sender_chat": null,
        "text": text,
        "source": "telegram_desktop_export_text_only"
    })))
}

fn write_sql_batches(json_file: &Path, chat_id: i64, output: &Path) -> Result<(usize, usize), Box<dyn Error>> {
    if chat_id >= 0 { return Err("chat ID must be the Telegram negative group ID".into()); }
    fs::create_dir_all(output)?;
    let obj: Value = serde_json::from_slice(&fs::read(json_file)?)?;
    let messages = obj["messages"].as_array().ok_or("Expected Telegram Desktop JSON `messages` array")?;
    let mut accepted: usize = 0;
    let mut part = 1;
    let mut buffer = String::from("-- Imported Telegram Desktop TEXT only. No media was imported.\nBEGIN TRANSACTION;\n");
    for msg in messages {
        let Some((id, date, user, payload)) = normalized(chat_id, msg) else { continue; };
        let text = escaped_sql(&serde_json::to_string(&payload)?);
        buffer.push_str(&format!(
            "INSERT OR IGNORE INTO archived_messages (chat_id,message_id,user_id,sent_at,updated_at,payload,last_status) VALUES ({chat_id},{id},{user},{date},{date},'{text}','pending');\n"
        ));
        accepted += 1;
        if accepted % BATCH_SIZE == 0 {
            buffer.push_str("COMMIT;\n");
            fs::write(output.join(format!("import-{part:04}.sql")), &buffer)?;
            part += 1;
            buffer = String::from("BEGIN TRANSACTION;\n");
        }
    }
    if accepted % BATCH_SIZE != 0 {
        buffer.push_str("COMMIT;\n");
        fs::write(output.join(format!("import-{part:04}.sql")), &buffer)?;
    }
    Ok((messages.len(), accepted))
}

fn main() -> Result<(), Box<dyn Error>> {
    let a = env::args().collect::<Vec<_>>();
    if a.len() != 4 {
        eprintln!("Usage: cargo run --release --manifest-path tools/history-import/Cargo.toml -- result.json -1001234567890 ./sql-batches");
        return Err("expected export JSON, numeric group chat ID, output directory".into());
    }
    let (total, imported) = write_sql_batches(&PathBuf::from(&a[1]),a[2].parse()?,&PathBuf::from(&a[3]))?;
    eprintln!("Source messages: {total}, text messages imported: {imported}; media and anonymous/service messages skipped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_text_fragments() {
        let msg = json!({"id":22,"type":"message","date_unixtime":"1720000000", "from_id":"user98",
            "text":["hello ", {"type":"bold","text":"World"}]});
        let (id, date, user, payload) = normalized(-1009,&msg).unwrap();
        assert_eq!((id,date,user),(22,1720000000,98));
        assert_eq!(payload["text"], "hello World");
        assert!(escaped_sql("a'b").contains("''"));
    }
}
